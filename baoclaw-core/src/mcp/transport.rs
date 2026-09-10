//! Transport layer for MCP connections: how outbound JSON-RPC messages are
//! delivered and how inbound ones arrive. The protocol client
//! ([`super::client::McpClient`]) owns all policy — ids, pending demux,
//! deadlines, handshake — and drives transports through the [`McpSender`]
//! trait plus a plain mpsc inbound channel.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::ipc::protocol::{decode_ndjson_line, encode_ndjson, JsonRpcMessage};

use super::client::McpError;

/// Outbound half of an MCP connection, shared (Arc) between the client's
/// request path and its inbound-reply path. `send` returns once the
/// transport accepted the message; response routing happens through the
/// inbound channel and the client's pending demux.
#[async_trait]
pub(crate) trait McpSender: Send + Sync {
    /// Deliver one message. `deadline` is the caller's write budget; a
    /// transport that cannot accept the message within it returns
    /// [`McpError::Timeout`].
    async fn send(&self, msg: &JsonRpcMessage, deadline: Duration) -> Result<(), McpError>;

    /// Terminate the connection. Idempotent. stdio: close stdin → grace →
    /// force-kill. HTTP: best-effort session teardown.
    async fn close(&self);
}

/// A write failure with a closed/reset pipe means the peer is gone —
/// surface that as Closed so callers treat it as a lifecycle event, not an
/// incidental IO error.
pub(crate) fn map_write_error(e: std::io::Error) -> McpError {
    if matches!(
        e.kind(),
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
    ) {
        McpError::Closed
    } else {
        McpError::Io(e)
    }
}

/// Open a transport for a discovered server entry. Dispatches on
/// `server_type`; inbound messages arrive on the returned channel (ends when
/// the connection dies).
pub(crate) async fn open_connection(
    spec: &crate::discovery::mcp_config::McpServerInfo,
    startup_timeout: std::time::Duration,
) -> Result<(Arc<dyn McpSender>, mpsc::Receiver<JsonRpcMessage>), McpError> {
    match spec.server_type.as_str() {
        "stdio" => {
            let command = spec.command.clone().unwrap_or_default();
            spawn_stdio(&spec.name, &command, &spec.args, &spec.env).await
        }
        "http" => super::http::streamable_http(spec),
        "sse" => super::http::legacy_sse(spec, startup_timeout).await,
        other => Err(McpError::Protocol(format!(
            "transport '{other}' not supported"
        ))),
    }
}

/// Spawn `command args` as a stdio MCP server: env sanitization (sensitive
/// daemon keys removed, configured `env` wins — the removal targets
/// accidental credential leakage, not deliberate operator config), stderr
/// drained in the background (a full stderr pipe would freeze the protocol;
/// values never logged), `kill_on_drop` as the last-resort reaper.
pub(crate) async fn spawn_stdio(
    server_name: &str,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<(Arc<dyn McpSender>, mpsc::Receiver<JsonRpcMessage>), McpError> {
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

    let (tx, rx) = mpsc::channel::<JsonRpcMessage>(64);
    // Inbound pump: child stdout lines → decoded messages. Non-JSON lines
    // (banners, log output) are skipped; EOF ends the channel.
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(msg) = decode_ndjson_line(&line) {
                if tx.send(msg).await.is_err() {
                    break; // client gone
                }
            }
        }
    });

    let sender = Arc::new(StdioSender {
        stdin: tokio::sync::Mutex::new(stdin),
        child: tokio::sync::Mutex::new(Some(child)),
    });
    Ok((sender, rx))
}

struct StdioSender {
    /// `None` once close began: writers see Closed and the dropped pipe
    /// signals well-behaved servers to exit.
    stdin: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    child: tokio::sync::Mutex<Option<tokio::process::Child>>,
}

#[async_trait]
impl McpSender for StdioSender {
    async fn send(&self, msg: &JsonRpcMessage, deadline: Duration) -> Result<(), McpError> {
        let line = encode_ndjson(msg)?;
        let write = async {
            let mut g = self.stdin.lock().await;
            match g.as_mut() {
                Some(s) => s.write_all(&line).await.map_err(map_write_error),
                None => Err(McpError::Closed),
            }
        };
        // A write that outlives the budget means the server stopped reading
        // stdin. Fail as Closed, not Timeout: a cancelled write_all can
        // leave a truncated line on the pipe, which would corrupt every
        // later message — the connection is not safely usable.
        match tokio::time::timeout(deadline, write).await {
            Ok(res) => res,
            Err(_) => Err(McpError::Closed),
        }
    }

    /// Close stdin (well-behaved servers exit), then force-kill after a
    /// grace period so a stuck child cannot outlive the daemon.
    async fn close(&self) {
        {
            let mut g = self.stdin.lock().await;
            *g = None; // dropping the pipe closes it
        }
        let mut guard = self.child.lock().await;
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

/// Hermetic HTTP/1.1 fake server for transport tests: hand-rolled over a
/// tokio TcpListener, one request per connection (except SSE responses that
/// hold the stream open), no server framework dependency.
#[cfg(test)]
pub(crate) mod http_harness {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Clone)]
    pub(crate) struct Request {
        pub method: String,
        pub path: String,
        pub headers: HashMap<String, String>,
        pub body: Vec<u8>,
    }

    impl Request {
        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .get(&name.to_ascii_lowercase())
                .map(String::as_str)
        }
        pub fn body_str(&self) -> String {
            String::from_utf8_lossy(&self.body).to_string()
        }
    }

    pub(crate) enum Response {
        /// Complete response; the connection closes after it.
        Full {
            status: u16,
            content_type: &'static str,
            headers: Vec<(String, String)>,
            body: Vec<u8>,
        },
        /// Raw SSE stream: chunks are written as-is, then the connection
        /// closes (each chunk must carry its own blank-line separators).
        Sse { status: u16, chunks: Vec<Vec<u8>> },
    }

    impl Response {
        pub fn json(status: u16, body: String) -> Self {
            Response::Full {
                status,
                content_type: "application/json",
                headers: vec![],
                body: body.into_bytes(),
            }
        }
    }

    pub(crate) struct FakeServer {
        pub url: String,
        /// Requests observed so far.
        seen: Arc<tokio::sync::Mutex<Vec<Request>>>,
        shutdown_tx: tokio::sync::watch::Sender<bool>,
    }

    impl FakeServer {
        /// Serve `handler` for every request until the server is dropped.
        pub fn start(handler: Arc<dyn Fn(Request) -> Response + Send + Sync + 'static>) -> Self {
            let std_listener = {
                let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fake server");
                l.set_nonblocking(true).expect("nonblocking");
                l
            };
            let listener = TcpListener::from_std(std_listener).expect("async listener");
            let port = listener.local_addr().unwrap().port();
            let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
            let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
            let seen_for_task = Arc::clone(&seen);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = shutdown_rx.changed() => break,
                        accepted = listener.accept() => {
                            let Ok((stream, _)) = accepted else { break };
                            let handler = Arc::clone(&handler);
                            let seen = Arc::clone(&seen_for_task);
                            tokio::spawn(handle_connection(stream, handler, seen));
                        }
                    }
                }
            });
            Self {
                url: format!("http://127.0.0.1:{port}"),
                seen,
                shutdown_tx,
            }
        }

        pub async fn record(&self) -> Vec<Request> {
            self.seen.lock().await.clone()
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            let _ = self.shutdown_tx.send(true);
        }
    }

    async fn handle_connection(
        mut stream: tokio::net::TcpStream,
        handler: Arc<dyn Fn(Request) -> Response + Send + Sync>,
        seen: Arc<tokio::sync::Mutex<Vec<Request>>>,
    ) {
        let req = match read_request(&mut stream).await {
            Some(r) => r,
            None => return,
        };
        seen.lock().await.push(req.clone());
        match handler(req) {
            Response::Full {
                status,
                content_type,
                headers,
                body,
            } => {
                let head = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                    body.len(),
                    headers
                        .iter()
                        .map(|(k, v)| format!("{k}: {v}\r\n"))
                        .collect::<String>()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
                let _ = stream.shutdown().await;
            }
            Response::Sse { status, chunks } => {
                let head = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(head.as_bytes()).await;
                for chunk in chunks {
                    let _ = stream.write_all(&chunk).await;
                    let _ = stream.flush().await;
                    // Small pause so the client observes events separately.
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                let _ = stream.shutdown().await;
            }
        }
    }

    async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<Request> {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        // Read until the header block terminator.
        loop {
            let n = stream.read(&mut byte).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&buf).to_string();
        let mut lines = head.split("\r\n");
        let request_line = lines.next()?;
        let mut parts = request_line.split_whitespace();
        let method = parts.next()?.to_string();
        let path = parts.next()?.to_string();
        let mut headers = HashMap::new();
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        let len: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; len];
        if len > 0 {
            stream.read_exact(&mut body).await.ok()?;
        }
        Some(Request {
            method,
            path,
            headers,
            body,
        })
    }
}
