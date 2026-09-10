//! Transport-agnostic MCP protocol client: performs the initialize
//! handshake, multiplexes JSON-RPC requests by id over whatever transport
//! [`super::transport`] provides (stdio, Streamable HTTP, legacy SSE),
//! answers server pings, rejects server→client requests, and surfaces
//! `tools/list_changed` as a signal for the live catalog.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{oneshot, watch, Mutex};

use crate::discovery::mcp_config::McpServerInfo;
use crate::ipc::protocol::{JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, RequestId};

use super::demux::{route_inbound, ListChangedSignal, PendingMap};
use super::transport::{open_connection, McpSender};
use super::types::{map_call_result, parse_tool_defs, CallToolOutcome, McpToolDef};
use super::{MCP_CLIENT_NAME, MCP_MAX_LIST_PAGES, MCP_PROTOCOL_VERSION, MCP_PROTOCOL_VERSION_HTTP};

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("MCP spawn failed: {0}")]
    Spawn(String),
    #[error("MCP connect failed: {0}")]
    Connect(String),
    #[error("MCP request timed out after {0}ms")]
    Timeout(u64),
    #[error("MCP connection closed")]
    Closed,
    #[error("MCP call cancelled")]
    Cancelled,
    #[error("MCP server error {code}: {message}")]
    Remote { code: i32, message: String },
    #[error("MCP io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("MCP serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("MCP protocol error: {0}")]
    Protocol(String),
}

/// Grace period for replying to server→client requests before treating the
/// connection as dead (a wedged reply write means nobody is reading).
const REPLY_WRITE_BUDGET: Duration = Duration::from_secs(5);

pub struct McpClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    server_name: String,
    /// `None` once shutdown began: writers see Closed.
    sender: Mutex<Option<Arc<dyn McpSender>>>,
    /// In-flight request demux. std Mutex: never held across an await.
    pending: PendingMap,
    next_id: AtomicI64,
    /// Bumped on `notifications/tools/list_changed`; the supervisor watches.
    list_changed_tx: watch::Sender<u64>,
    list_changed_signal: ListChangedSignal,
    /// Set when the inbound channel ends (connection dead).
    done_rx: watch::Receiver<bool>,
}

impl McpClient {
    /// Open a connection for a discovered server entry, perform the
    /// initialize handshake, and fetch the tool catalog.
    ///
    /// `startup_timeout` bounds the initialize request AND the whole catalog
    /// fetch, so a hung server cannot stall boot beyond a bounded multiple
    /// of it. On any failure the connection is closed before returning the
    /// error.
    pub async fn connect(
        spec: &McpServerInfo,
        startup_timeout: Duration,
    ) -> Result<(Self, Vec<McpToolDef>), McpError> {
        let (sender, inbound_rx) = open_connection(spec, startup_timeout).await?;
        // Streamable HTTP was introduced by the 2025-03-26 revision; stdio
        // and legacy SSE keep the original (every conforming server accepts
        // it, newer revisions can be rejected outright).
        let protocol_version = if spec.server_type == "http" {
            MCP_PROTOCOL_VERSION_HTTP
        } else {
            MCP_PROTOCOL_VERSION
        };
        Self::open(
            &spec.name,
            protocol_version,
            sender,
            inbound_rx,
            startup_timeout,
        )
        .await
    }

    /// Run the protocol client over an established transport.
    async fn open(
        server_name: &str,
        protocol_version: &'static str,
        sender: Arc<dyn McpSender>,
        mut inbound_rx: tokio::sync::mpsc::Receiver<JsonRpcMessage>,
        startup_timeout: Duration,
    ) -> Result<(Self, Vec<McpToolDef>), McpError> {
        let (done_tx, done_rx) = watch::channel(false);
        let (list_changed_tx, _) = watch::channel(0u64);
        let client = Self {
            inner: Arc::new(ClientInner {
                server_name: server_name.to_string(),
                sender: Mutex::new(Some(Arc::clone(&sender))),
                pending: std::sync::Mutex::new(HashMap::new()),
                next_id: AtomicI64::new(1),
                list_changed_tx,
                list_changed_signal: ListChangedSignal::default(),
                done_rx,
            }),
        };

        // Reader loop: inbound channel → routing → bounded reply writes.
        // Channel end (EOF / transport death) or a stuck reply fails every
        // in-flight request and marks the connection done.
        let reader_inner = Arc::clone(&client.inner);
        tokio::spawn(async move {
            while let Some(msg) = inbound_rx.recv().await {
                if let Some(reply) = route_inbound(
                    &reader_inner.pending,
                    &reader_inner.list_changed_tx,
                    &reader_inner.list_changed_signal,
                    msg,
                ) {
                    let sender = {
                        let g = reader_inner.sender.lock().await;
                        g.as_ref().map(Arc::clone)
                    };
                    let delivered = match sender {
                        Some(s) => s.send(&reply, REPLY_WRITE_BUDGET).await.is_ok(),
                        None => false,
                    };
                    if !delivered {
                        break;
                    }
                }
            }
            // Channel ended: the connection is gone — fail everything.
            reader_inner.pending.lock().unwrap().clear();
            let _ = done_tx.send(true);
        });

        match Self::handshake_and_list(&client, protocol_version, startup_timeout).await {
            Ok(tools) => Ok((client, tools)),
            Err(e) => {
                client.shutdown().await;
                Err(e)
            }
        }
    }

    async fn handshake_and_list(
        client: &Self,
        protocol_version: &'static str,
        startup_timeout: Duration,
    ) -> Result<Vec<McpToolDef>, McpError> {
        let init_result = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": protocol_version,
                    "capabilities": {},
                    "clientInfo": {"name": MCP_CLIENT_NAME, "version": env!("CARGO_PKG_VERSION")}
                }),
                startup_timeout,
            )
            .await?;
        if let Some(version) = init_result.get("protocolVersion").and_then(Value::as_str) {
            eprintln!(
                "[mcp:{}] server negotiated protocol version {version}",
                client.inner.server_name
            );
        }
        client
            .notify("notifications/initialized", json!({}), startup_timeout)
            .await?;
        // One budget for the WHOLE catalog fetch: pagination is capped at
        // 100 pages, and a per-page budget would let a maliciously paging
        // server stretch boot to 100 × startup_timeout.
        match tokio::time::timeout(startup_timeout, client.list_tools(startup_timeout)).await {
            Ok(r) => r,
            Err(_) => Err(McpError::Timeout(startup_timeout.as_millis() as u64)),
        }
    }

    /// Send a request and await the matching response. The pending entry is
    /// removed on timeout/late-response so a straggler is dropped instead of
    /// misdelivered to a later request reusing the id slot.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        self.request_internal(method, params, timeout, None).await
    }

    async fn request_internal(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        abort: Option<watch::Receiver<bool>>,
    ) -> Result<Value, McpError> {
        let id = RequestId::Number(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id.clone(), tx);

        // Clone the sender under the lock and drop the guard BEFORE sending:
        // an HTTP send spans the whole POST round trip, and holding the mutex
        // across it would serialize every request and defer the abort select
        // until after delivery.
        let sender = {
            let g = self.inner.sender.lock().await;
            g.as_ref().map(Arc::clone)
        };
        let Some(sender) = sender else {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err(McpError::Closed);
        };
        {
            let msg = JsonRpcRequest {
                jsonrpc: "2.0".to_string(),
                method: method.to_string(),
                params,
                id: id.clone(),
            };
            if let Err(e) = sender.send(&JsonRpcMessage::Request(msg), timeout).await {
                self.inner.pending.lock().unwrap().remove(&id);
                return Err(e);
            }
        }

        let await_response = rx;
        tokio::select! {
            res = tokio::time::timeout(timeout, await_response) => {
                match res {
                    Ok(Ok(msg)) => match msg {
                        JsonRpcMessage::Response(resp) => Ok(resp.result),
                        JsonRpcMessage::ErrorResponse(err) => Err(McpError::Remote {
                            code: err.error.code,
                            message: err.error.message,
                        }),
                        _ => Err(McpError::Protocol("mismatched JSON-RPC message".into())),
                    },
                    // Sender dropped: the reader drained pending at EOF.
                    Ok(Err(_)) => Err(McpError::Closed),
                    Err(_) => {
                        self.inner.pending.lock().unwrap().remove(&id);
                        Err(McpError::Timeout(timeout.as_millis() as u64))
                    }
                }
            }
            _ = wait_for_abort(abort) => {
                // Tell the server to stop the work, then fail the call.
                // Best-effort: the user-visible abort never waits on a
                // wedged server. The pending entry is removed first so the
                // late tools/call response (or -32800) is dropped.
                self.inner.pending.lock().unwrap().remove(&id);
                let notify = self.notify(
                    "notifications/cancelled",
                    json!({"requestId": id_number(&id), "reason": "user requested cancellation"}),
                    Duration::from_secs(2),
                );
                if let Err(e) = notify.await {
                    eprintln!(
                        "[mcp:{}] cancel notification failed: {e}",
                        self.inner.server_name
                    );
                }
                Err(McpError::Cancelled)
            }
        }
    }

    pub async fn notify(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<(), McpError> {
        let sender = {
            let g = self.inner.sender.lock().await;
            g.as_ref().map(Arc::clone)
        };
        match sender {
            Some(s) => {
                let msg = JsonRpcNotification::new(method, params);
                s.send(&JsonRpcMessage::Notification(msg), timeout).await
            }
            None => Err(McpError::Closed),
        }
    }

    /// Fetch the tool catalog, following `nextCursor` pagination up to a hard
    /// page cap so a misbehaving server cannot loop forever.
    pub async fn list_tools(&self, timeout: Duration) -> Result<Vec<McpToolDef>, McpError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MCP_MAX_LIST_PAGES {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let result = self.request("tools/list", params, timeout).await?;
            out.extend(parse_tool_defs(&result));
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(String::from);
            if cursor.is_none() {
                return Ok(out);
            }
        }
        eprintln!(
            "[mcp:{}] tools/list pagination cap reached; catalog may be truncated",
            self.inner.server_name
        );
        Ok(out)
    }

    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<CallToolOutcome, McpError> {
        let result = self
            .request(
                "tools/call",
                json!({"name": tool_name, "arguments": arguments}),
                timeout,
            )
            .await?;
        Ok(map_call_result(&result))
    }

    /// [`Self::call_tool`] with abort support: on the abort signal the
    /// server is told `notifications/cancelled {requestId}` and the call
    /// fails deterministically with [`McpError::Cancelled`]. If the response
    /// races the abort and wins, the result is returned normally (the server
    /// completed anyway; cancellation would be redundant).
    pub async fn call_tool_cancellable(
        &self,
        tool_name: &str,
        arguments: Value,
        timeout: Duration,
        abort: Option<watch::Receiver<bool>>,
    ) -> Result<CallToolOutcome, McpError> {
        let result = self
            .request_internal(
                "tools/call",
                json!({"name": tool_name, "arguments": arguments}),
                timeout,
                abort,
            )
            .await?;
        Ok(map_call_result(&result))
    }

    /// Watch handle for `notifications/tools/list_changed`: the value
    /// increments on every notification; a supervisor compares against the
    /// value it last observed.
    pub fn list_changed(&self) -> watch::Receiver<u64> {
        self.inner.list_changed_tx.subscribe()
    }

    /// Resolves when the inbound channel ends (connection dead).
    pub async fn wait_closed(&self) {
        let _ = self.inner.done_rx.clone().wait_for(|v| *v).await;
    }

    /// Terminate the transport. Idempotent.
    pub async fn shutdown(&self) {
        let sender = {
            let mut g = self.inner.sender.lock().await;
            g.take()
        };
        if let Some(sender) = sender {
            sender.close().await;
        }
    }
}

/// The abort-watch wait used by [`McpClient::request_internal`]: resolves
/// only when the signal turns true; a dropped sender (engine gone without
/// aborting) is NOT an abort.
async fn wait_for_abort(abort: Option<watch::Receiver<bool>>) {
    match abort {
        Some(rx) => crate::engine::abort_helpers::wait_for_abort(rx).await,
        None => std::future::pending::<()>().await,
    }
}

/// The JSON wire value of a request id for notifications/cancelled.
fn id_number(id: &RequestId) -> Value {
    serde_json::to_value(id).unwrap_or(Value::Null)
}

/// Helper used by tests to build a stdio server spec.
#[cfg(test)]
pub(crate) fn stdio_spec(name: &str, command: &str, env: HashMap<String, String>) -> McpServerInfo {
    McpServerInfo {
        name: name.to_string(),
        command: Some(command.to_string()),
        args: vec![],
        server_type: "stdio".to_string(),
        url: None,
        disabled: false,
        source: "user".to_string(),
        config_path: "test".to_string(),
        env,
        headers: HashMap::new(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::Path;

    /// Write a POSIX-sh fake MCP server into a tempdir and return its spec
    /// for use with [`McpClient::connect`]. Scripts echo the request id back
    /// from the raw line (no jq dependency) so the demux matches.
    fn write_fake_server(dir: &Path, name: &str, script: &str) -> McpServerInfo {
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        stdio_spec("fake", path.to_str().unwrap(), HashMap::new())
    }

    async fn connect_at(
        spec: &McpServerInfo,
        startup_timeout: Duration,
    ) -> (McpClient, Vec<McpToolDef>) {
        McpClient::connect(spec, startup_timeout)
            .await
            .expect("connect + handshake")
    }

    /// Happy-path server. After the initialize response it fires two
    /// server→client requests (ping id 9001, sampling id 9002) and records
    /// how the client answered. It holds the tools/list reply until BOTH
    /// replies have been observed, then reports the outcome in the tool
    /// description — making the verification deterministic from the client
    /// side (the handshake cannot complete otherwise).
    const HAPPY_SERVER: &str = r#"#!/bin/sh
id_of() { printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }
classify_replies() {
  case "$1" in
    *'"id":9001'*)
      case "$1" in
        *'"result"'*) got_ping=ok ;;
        *) got_ping=bad ;;
      esac
      ;;
    *'"id":9002'*)
      case "$1" in
        *'"error"'*) got_sampling=ok ;;
        *) got_sampling=bad ;;
      esac
      ;;
  esac
}
got_ping=unknown
got_sampling=unknown
pending_id=""
emit_tools() {
  printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo_tool","description":"probe ping=%s sampling=%s","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]}}\n' "$1" "$got_ping" "$got_sampling"
}
while IFS= read -r line; do
  classify_replies "$line"
  case "$line" in
    *'"method":"initialize"'*)
      id=$(id_of "$line")
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1"}}}\n' "$id"
      printf '{"jsonrpc":"2.0","method":"ping","id":9001}\n'
      printf '{"jsonrpc":"2.0","method":"sampling/createMessage","id":9002}\n'
      ;;
    *'"method":"tools/list"'*)
      pending_id=$(id_of "$line")
      ;;
    *'"method":"tools/call"'*)
      id=$(id_of "$line")
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echoed"}],"isError":false}}\n' "$id"
      ;;
  esac
  if [ -n "$pending_id" ] && [ "$got_ping" != "unknown" ] && [ "$got_sampling" != "unknown" ]; then
    emit_tools "$pending_id"
    pending_id=""
  fi
done
exit 0
"#;

    #[tokio::test]
    async fn connect_handshake_list_and_verify_ping_sampling_handling() {
        let dir = tempfile::TempDir::new().unwrap();
        let spec = write_fake_server(dir.path(), "happy.sh", HAPPY_SERVER);
        let (client, tools) = connect_at(&spec, Duration::from_secs(5)).await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo_tool");
        assert_eq!(
            tools[0].input_schema["properties"]["text"]["type"],
            json!("string")
        );
        // The server only releases tools/list after seeing both replies.
        assert_eq!(
            tools[0].description.as_deref(),
            Some("probe ping=ok sampling=ok")
        );
        let outcome = client
            .call_tool("echo_tool", json!({"text": "hi"}), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(outcome.data, json!("echoed"));
        assert!(!outcome.is_error);
        client.shutdown().await;
    }

    /// Server that prints garbage stdout lines around protocol messages:
    /// the reader must tolerate them (banner/log tolerance).
    const BANNER_SERVER: &str = r#"#!/bin/sh
id_of() { printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }
echo "starting up, please wait..."
while IFS= read -r line; do
  echo "[debug] got a message"
  case "$line" in
    *'"method":"initialize"'*)
      id=$(id_of "$line")
      printf 'not json at all\n'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fake","version":"1"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=$(id_of "$line")
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"t1","inputSchema":{}}]}}\n' "$id"
      ;;
  esac
done
"#;

    #[tokio::test]
    async fn banner_tolerance() {
        let dir = tempfile::TempDir::new().unwrap();
        let spec = write_fake_server(dir.path(), "banner.sh", BANNER_SERVER);
        let (client, tools) = connect_at(&spec, Duration::from_secs(5)).await;
        assert_eq!(tools.len(), 1);
        client.shutdown().await;
    }

    /// First tools/list page returns nextCursor; the full catalog only
    /// completes when the cursor comes back.
    const PAGED_SERVER: &str = r#"#!/bin/sh
id_of() { printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(id_of "$line")
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fake","version":"1"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=$(id_of "$line")
      case "$line" in
        *'"cursor"'*)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"t2","inputSchema":{}}]}}\n' "$id"
          ;;
        *)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"t1","inputSchema":{}}],"nextCursor":"page2"}}\n' "$id"
          ;;
      esac
      ;;
  esac
done
"#;

    #[tokio::test]
    async fn cursor_pagination_fetches_all_pages() {
        let dir = tempfile::TempDir::new().unwrap();
        let spec = write_fake_server(dir.path(), "paged.sh", PAGED_SERVER);
        let (client, tools) = connect_at(&spec, Duration::from_secs(5)).await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["t1", "t2"]);
        client.shutdown().await;
    }

    #[tokio::test]
    async fn boot_timeout_on_slow_server() {
        let dir = tempfile::TempDir::new().unwrap();
        let spec = write_fake_server(dir.path(), "slow.sh", "#!/bin/sh\nsleep 30\n");
        let result = McpClient::connect(&spec, Duration::from_millis(300)).await;
        match result {
            Err(McpError::Timeout(_)) => {}
            Err(other) => panic!("expected Timeout, got {other}"),
            Ok(_) => panic!("expected Timeout, got Ok"),
        }
    }

    /// Server that answers initialize then exits: the handshake's tools/list
    /// must fail (Closed via EOF or per-request Timeout), not hang.
    const CRASH_AFTER_INIT_SERVER: &str = r#"#!/bin/sh
id_of() { printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(id_of "$line")
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fake","version":"1"}}}\n' "$id"
      exit 7
      ;;
  esac
done
"#;

    #[tokio::test]
    async fn crash_during_handshake_maps_to_closed() {
        let dir = tempfile::TempDir::new().unwrap();
        let spec = write_fake_server(dir.path(), "crash.sh", CRASH_AFTER_INIT_SERVER);
        let result = McpClient::connect(&spec, Duration::from_secs(5)).await;
        match result {
            Err(McpError::Closed) | Err(McpError::Timeout(_)) => {}
            Err(other) => panic!("expected Closed/Timeout, got {other}"),
            Ok(_) => panic!("expected Closed/Timeout, got Ok"),
        }
    }
}
