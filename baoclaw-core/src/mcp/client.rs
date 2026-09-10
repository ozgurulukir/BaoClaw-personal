//! Stdio MCP client: spawns a server process, performs the initialize
//! handshake, and multiplexes JSON-RPC requests over the child's stdin/stdout
//! (NDJSON framing, shared with the daemon's own IPC protocol layer).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{oneshot, watch, Mutex};

use crate::ipc::protocol::{
    decode_ndjson_line, encode_ndjson, JsonRpcErrorResponse, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse, RequestId,
};

use super::types::{map_call_result, parse_tool_defs, CallToolOutcome, McpToolDef};
use super::{MCP_CLIENT_NAME, MCP_MAX_LIST_PAGES, MCP_PROTOCOL_VERSION};

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("MCP spawn failed: {0}")]
    Spawn(String),
    #[error("MCP request timed out after {0}ms")]
    Timeout(u64),
    #[error("MCP connection closed")]
    Closed,
    #[error("MCP server error {code}: {message}")]
    Remote { code: i32, message: String },
    #[error("MCP io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("MCP serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("MCP protocol error: {0}")]
    Protocol(String),
}

/// A write failure with a closed/reset pipe means the server is gone —
/// surface that as Closed so callers treat it as a lifecycle event, not an
/// incidental IO error.
fn map_write_error(e: std::io::Error) -> McpError {
    if matches!(
        e.kind(),
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
    ) {
        McpError::Closed
    } else {
        McpError::Io(e)
    }
}

pub struct StdioMcpClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    server_name: String,
    /// `None` once shutdown began: writers see Closed and the dropped pipe
    /// signals well-behaved servers to exit.
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    /// In-flight request demux. std Mutex: never held across an await.
    pending: std::sync::Mutex<HashMap<RequestId, oneshot::Sender<JsonRpcMessage>>>,
    next_id: AtomicI64,
    child: Mutex<Option<tokio::process::Child>>,
    /// Set by the reader task when stdout hits EOF (child gone).
    done: watch::Receiver<bool>,
}

impl StdioMcpClient {
    /// Spawn `command args`, perform the initialize handshake, fetch the tool
    /// catalog, and start the reader task.
    ///
    /// `env` comes from the server's mcp.json entry (values are secrets —
    /// never logged). It is applied AFTER the sensitive-key removals so an
    /// explicitly configured value wins: the sanitization targets accidental
    /// daemon-credential leakage, not deliberate operator config.
    ///
    /// `startup_timeout` bounds EACH handshake RPC (initialize, tools/list
    /// page), so a hung server cannot stall boot beyond a bounded multiple of
    /// it. On any failure the child is killed before returning the error.
    pub async fn spawn(
        server_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        startup_timeout: Duration,
    ) -> Result<(Self, Vec<McpToolDef>), McpError> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args);
        for key in crate::tools::builtins::bash_tool::SENSITIVE_ENV_KEYS {
            cmd.env_remove(key);
        }
        cmd.envs(env);
        cmd.kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| McpError::Spawn(format!("failed to spawn '{command}': {e}")))?;

        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Spawn("child stdout not captured".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| McpError::Spawn("child stderr not captured".into()))?;

        // Drain stderr in the background: a server blocking on a full stderr
        // pipe would freeze the protocol. Values are never logged.
        let err_name = server_name.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let shown = if line.chars().count() > 500 {
                    let t: String = line.chars().take(500).collect();
                    format!("{t}…")
                } else {
                    line
                };
                eprintln!("[mcp:{err_name}] {shown}");
            }
        });

        let (done_tx, done_rx) = watch::channel(false);
        let client = Self {
            inner: Arc::new(ClientInner {
                server_name: server_name.to_string(),
                stdin: Mutex::new(stdin),
                pending: std::sync::Mutex::new(HashMap::new()),
                next_id: AtomicI64::new(1),
                child: Mutex::new(Some(child)),
                done: done_rx,
            }),
        };

        // Reader task: demux responses, auto-answer pings, reject
        // server→client requests, tolerate non-JSON stdout lines.
        let reader_inner = Arc::clone(&client.inner);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match decode_ndjson_line(&line) {
                    Ok(JsonRpcMessage::Response(resp)) => {
                        let tx = reader_inner.pending.lock().unwrap().remove(&resp.id);
                        if let Some(tx) = tx {
                            let _ = tx.send(JsonRpcMessage::Response(resp));
                        }
                    }
                    Ok(JsonRpcMessage::ErrorResponse(err)) => {
                        if let Some(id) = &err.id {
                            let tx = reader_inner.pending.lock().unwrap().remove(id);
                            if let Some(tx) = tx {
                                let _ = tx.send(JsonRpcMessage::ErrorResponse(err));
                            }
                        }
                    }
                    Ok(JsonRpcMessage::Request(req)) => {
                        // Server→client requests are out of scope: ping is
                        // answered, everything else (sampling, roots, ...)
                        // gets method-not-found so the server can degrade.
                        let reply = if req.method == "ping" {
                            encode_ndjson(&JsonRpcResponse::success(req.id.clone(), json!({})))
                        } else {
                            encode_ndjson(&JsonRpcErrorResponse::new(
                                Some(req.id.clone()),
                                -32601,
                                "server-to-client requests are not supported".into(),
                            ))
                        };
                        if let Ok(line) = reply {
                            let write = async {
                                let mut g = reader_inner.stdin.lock().await;
                                if let Some(s) = g.as_mut() {
                                    s.write_all(&line).await?;
                                }
                                Ok::<(), std::io::Error>(())
                            };
                            // The connection is dead if we cannot write a
                            // reply promptly; stop the reader so EOF cleanup
                            // fails every in-flight request.
                            if tokio::time::timeout(Duration::from_secs(5), write)
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                    Ok(JsonRpcMessage::Notification(n)) => {
                        if n.method == "notifications/tools/list_changed" {
                            eprintln!(
                                "[mcp:{}] tools list changed notification ignored (tool set is frozen at boot)",
                                reader_inner.server_name
                            );
                        }
                    }
                    // Non-JSON stdout (banners, log output): skip silently.
                    Err(_) => {}
                }
            }
            // EOF: the child is gone — fail every in-flight request.
            reader_inner.pending.lock().unwrap().clear();
            let _ = done_tx.send(true);
        });

        match Self::handshake_and_list(&client, startup_timeout).await {
            Ok(tools) => Ok((client, tools)),
            Err(e) => {
                client.shutdown().await;
                Err(e)
            }
        }
    }

    async fn handshake_and_list(
        client: &Self,
        startup_timeout: Duration,
    ) -> Result<Vec<McpToolDef>, McpError> {
        let init_result = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
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
        let id = RequestId::Number(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id.clone(), tx);

        let line = encode_ndjson(&JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
            id: id.clone(),
        })
        .map_err(McpError::from)?;

        let write = async {
            let mut g = self.inner.stdin.lock().await;
            match g.as_mut() {
                Some(s) => s.write_all(&line).await.map_err(map_write_error),
                None => Err(McpError::Closed),
            }
        };
        // A write that outlives the budget means the server stopped
        // reading stdin: fail the call instead of wedging the pipe.
        if tokio::time::timeout(timeout, write).await.is_err() {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err(McpError::Timeout(timeout.as_millis() as u64));
        }

        match tokio::time::timeout(timeout, rx).await {
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

    pub async fn notify(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<(), McpError> {
        let line =
            encode_ndjson(&JsonRpcNotification::new(method, params)).map_err(McpError::from)?;
        let write = async {
            let mut g = self.inner.stdin.lock().await;
            match g.as_mut() {
                Some(s) => s.write_all(&line).await.map_err(map_write_error),
                None => Err(McpError::Closed),
            }
        };
        match tokio::time::timeout(timeout, write).await {
            Ok(res) => res,
            Err(_) => Err(McpError::Timeout(timeout.as_millis() as u64)),
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

    /// Resolves when the reader task ends (process exited / stdout EOF).
    pub async fn wait_closed(&self) {
        let _ = self.inner.done.clone().wait_for(|v| *v).await;
    }

    /// Close stdin (well-behaved servers exit), then force-kill after a grace
    /// period so a stuck child cannot outlive the daemon.
    pub async fn shutdown(&self) {
        {
            let mut g = self.inner.stdin.lock().await;
            *g = None; // dropping the pipe closes it
        }
        let mut guard = self.inner.child.lock().await;
        if let Some(mut child) = guard.take() {
            match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(_) => {}
                Err(_) => {
                    child.start_kill().ok();
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::future::Future;
    use std::path::{Path, PathBuf};

    /// Write a POSIX-sh fake MCP server into a tempdir and return its path
    /// for use as the mcp.json `command`. Scripts echo the request id back
    /// from the raw line (no jq dependency) so the demux matches.
    fn write_fake_server(dir: &Path, name: &str, script: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn spawn_at(
        cmd: &Path,
        startup_timeout: Duration,
    ) -> impl Future<Output = (StdioMcpClient, Vec<McpToolDef>)> {
        let path = cmd.to_path_buf();
        async move {
            let (client, tools) = StdioMcpClient::spawn(
                "fake",
                path.to_str().unwrap(),
                &[],
                &HashMap::new(),
                startup_timeout,
            )
            .await
            .expect("spawn + handshake");
            (client, tools)
        }
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
    async fn spawn_handshake_list_and_verify_ping_sampling_handling() {
        let dir = tempfile::TempDir::new().unwrap();
        let cmd = write_fake_server(dir.path(), "happy.sh", HAPPY_SERVER);
        let (client, tools) = spawn_at(&cmd, Duration::from_secs(5)).await;
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
        let cmd = write_fake_server(dir.path(), "banner.sh", BANNER_SERVER);
        let (client, tools) = spawn_at(&cmd, Duration::from_secs(5)).await;
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
        let cmd = write_fake_server(dir.path(), "paged.sh", PAGED_SERVER);
        let (client, tools) = spawn_at(&cmd, Duration::from_secs(5)).await;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["t1", "t2"]);
        client.shutdown().await;
    }

    #[tokio::test]
    async fn boot_timeout_on_slow_server() {
        let dir = tempfile::TempDir::new().unwrap();
        let cmd = write_fake_server(dir.path(), "slow.sh", "#!/bin/sh\nsleep 30\n");
        let result = StdioMcpClient::spawn(
            "fake",
            cmd.to_str().unwrap(),
            &[],
            &HashMap::new(),
            Duration::from_millis(300),
        )
        .await;
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
        let cmd = write_fake_server(dir.path(), "crash.sh", CRASH_AFTER_INIT_SERVER);
        let result = StdioMcpClient::spawn(
            "fake",
            cmd.to_str().unwrap(),
            &[],
            &HashMap::new(),
            Duration::from_secs(5),
        )
        .await;
        match result {
            Err(McpError::Closed) | Err(McpError::Timeout(_)) => {}
            Err(other) => panic!("expected Closed/Timeout, got {other}"),
            Ok(_) => panic!("expected Closed/Timeout, got Ok"),
        }
    }
}
