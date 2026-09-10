//! HTTP-based MCP transports.
//!
//! - `http` (Streamable HTTP): one fixed endpoint; every message POSTs a
//!   JSON-RPC document and the response is either `application/json` (the
//!   reply inline) or `text/event-stream` (a stream whose events carry the
//!   reply plus any server→client messages). `Mcp-Session-Id` from the
//!   initialize response is echoed on later requests. No standing GET
//!   stream: server→client messages are only observed on POST responses
//!   (documented scope cut).
//! - `sse` (legacy HTTP+SSE): a standing GET stream carries server→client
//!   messages; the `endpoint` event announces the POST URL.
//!
//! Header values from mcp.json (`headers`) are secrets: applied to every
//! request after the fixed defaults, never logged.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::{mpsc, Mutex};

use crate::discovery::mcp_config::McpServerInfo;
use crate::ipc::protocol::{JsonRpcMessage, RequestId};

use super::client::McpError;
use super::sse::SseParser;
use super::transport::McpSender;

/// Reserved header names a per-server `headers` entry may not override.
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "accept",
    "mcp-session-id",
];

/// Shared reqwest client construction, mirroring the API clients' builder
/// (including the third-party-gateway HTTP/1.1 escape hatch).
fn http_client() -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .user_agent(concat!("baoclaw-core/", env!("CARGO_PKG_VERSION")));
    if std::env::var("BAOCLAW_HTTP1_ONLY").ok().as_deref() == Some("1") {
        builder = builder.http1_only();
    }
    builder.build().expect("reqwest client builds")
}

/// Filter configured headers: reserved transport headers are ignored with a
/// one-line warning (no values printed — they are credentials).
fn configured_headers(
    headers: &std::collections::HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (k, v) in headers {
        let lower = k.to_ascii_lowercase();
        if RESERVED_HEADERS.contains(&lower.as_str()) {
            eprintln!("[mcp] WARNING: ignoring reserved header '{lower}' in mcp.json headers");
            continue;
        }
        out.push((k.clone(), v.clone()));
    }
    out
}

/// Map a non-2xx HTTP status onto the McpError semantics the supervisor
/// understands: auth failures are config problems, gone sessions are
/// lifecycle events, everything else is a protocol violation.
fn map_http_status(status: u16) -> McpError {
    match status {
        401 | 403 => McpError::Connect(format!("auth rejected (HTTP {status})")),
        404 | 410 => McpError::Closed,
        s => McpError::Protocol(format!("HTTP {s}")),
    }
}

fn map_reqwest_error(e: reqwest::Error, deadline: Duration) -> McpError {
    if e.is_timeout() {
        McpError::Timeout(deadline.as_millis() as u64)
    } else {
        // DNS, refused, TLS, reset: the server is unreachable — a lifecycle
        // event the supervisor reconnects on.
        McpError::Closed
    }
}

/// Resolve the legacy-SSE `endpoint` event data against the base URL.
fn resolve_endpoint(base: &str, data: &str) -> String {
    if data.starts_with("http://") || data.starts_with("https://") {
        data.to_string()
    } else if let (Some(scheme_end), true) = (base.find("://"), data.starts_with('/')) {
        let after_scheme = &base[scheme_end + 3..];
        let authority = after_scheme.split('/').next().unwrap_or("");
        format!("{}://{}{}", &base[..scheme_end], authority, data)
    } else {
        format!("{}/{}", base.trim_end_matches('/'), data)
    }
}

/// Drain an SSE response body into the inbound channel. Stops at stream end
/// or `deadline`; if `own_id` is set, stops right after the matching
/// response (servers close POST streams after answering; trailing events
/// are not waited for).
async fn pump_sse_response(
    response: reqwest::Response,
    tx: mpsc::Sender<JsonRpcMessage>,
    own_id: Option<RequestId>,
    deadline: Duration,
) {
    let mut stream = response.bytes_stream();
    let mut parser = SseParser::new();
    let _ = tokio::time::timeout(deadline, async move {
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    parser.feed(&bytes);
                    while let Some(event) = parser.next_event() {
                        let named_ok = matches!(event.event.as_deref(), None | Some("message"));
                        if !named_ok {
                            continue;
                        }
                        let Ok(msg) = serde_json::from_str::<JsonRpcMessage>(&event.data) else {
                            continue; // non-JSON event: skip (banner tolerance)
                        };
                        let done = match (&msg, &own_id) {
                            (JsonRpcMessage::Response(r), Some(id)) => &r.id == id,
                            (JsonRpcMessage::ErrorResponse(e), Some(id)) => {
                                e.id.as_ref() == Some(id)
                            }
                            _ => false,
                        };
                        if tx.send(msg).await.is_err() || done {
                            return;
                        }
                    }
                }
                Err(_) => return,
            }
        }
    })
    .await;
}

// ─── Streamable HTTP (server_type "http") ─────────────────────────────────

struct StreamableSender {
    http: reqwest::Client,
    endpoint: String,
    headers: Vec<(String, String)>,
    /// `Mcp-Session-Id` captured from the initialize response.
    session: std::sync::Mutex<Option<String>>,
    /// The inbound channel. Held so the channel stays open between calls;
    /// taken on a fatal send error so a dead server surfaces as channel end
    /// (the client then reports the connection gone).
    tx: Mutex<Option<mpsc::Sender<JsonRpcMessage>>>,
}

#[async_trait]
impl McpSender for StreamableSender {
    async fn send(&self, msg: &JsonRpcMessage, deadline: Duration) -> Result<(), McpError> {
        let body = serde_json::to_vec(msg)?;
        let own_id = match msg {
            JsonRpcMessage::Request(r) => Some(r.id.clone()),
            _ => None,
        };
        let mut req = self
            .http
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .timeout(deadline)
            .body(body);
        if let Some(sid) = self.session.lock().unwrap().as_ref() {
            req = req.header("Mcp-Session-Id", sid);
        }
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let response = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                let mapped = map_reqwest_error(e, deadline);
                if matches!(mapped, McpError::Closed) {
                    // Unreachable server: end the channel so the connection
                    // is reported dead instead of silently staying Ready.
                    self.tx.lock().await.take();
                }
                return Err(mapped);
            }
        };
        if let Some(sid) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session.lock().unwrap() = Some(sid.to_string());
        }
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let mapped = map_http_status(status);
            if matches!(mapped, McpError::Closed) {
                self.tx.lock().await.take();
            }
            return Err(mapped);
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if content_type.starts_with("text/event-stream") {
            let tx = self.tx_channel().await?;
            pump_sse_response(response, tx, own_id, deadline).await;
            Ok(())
        } else {
            let body = response
                .bytes()
                .await
                .map_err(|e| map_reqwest_error(e, deadline))?;
            if body.is_empty() {
                return Ok(()); // 202-style accepted notification
            }
            let reply: JsonRpcMessage = serde_json::from_slice(&body)?;
            let tx = self.tx_channel().await?;
            let _ = tx.send(reply).await;
            Ok(())
        }
    }

    /// Best-effort session termination.
    async fn close(&self) {
        *self.tx.lock().await = None;
        let sid = self.session.lock().unwrap().clone();
        let mut req = self
            .http
            .delete(&self.endpoint)
            .timeout(Duration::from_secs(2));
        if let Some(sid) = sid {
            req = req.header("Mcp-Session-Id", sid);
        }
        let _ = req.send().await;
    }
}

impl StreamableSender {
    /// A live channel handle for pumps/inline replies. `None` after a fatal
    /// error or close — callers drop their message (the client is gone).
    async fn tx_channel(&self) -> Result<mpsc::Sender<JsonRpcMessage>, McpError> {
        self.tx.lock().await.clone().ok_or(McpError::Closed)
    }
}

/// Open a Streamable HTTP connection (server_type `http`).
pub(crate) fn streamable_http(
    spec: &McpServerInfo,
) -> Result<(Arc<dyn McpSender>, mpsc::Receiver<JsonRpcMessage>), McpError> {
    let url = spec
        .url
        .as_deref()
        .ok_or_else(|| McpError::Connect("http server missing url".to_string()))?;
    let (tx, rx) = mpsc::channel(64);
    let sender = Arc::new(StreamableSender {
        http: http_client(),
        endpoint: url.to_string(),
        headers: configured_headers(&spec.headers),
        session: std::sync::Mutex::new(None),
        tx: Mutex::new(Some(tx)),
    });
    Ok((sender, rx))
}

// ─── Legacy HTTP+SSE (server_type "sse") ──────────────────────────────────

struct SseSender {
    http: reqwest::Client,
    /// POST URL announced by the `endpoint` event.
    post_url: String,
    headers: Vec<(String, String)>,
    /// Abort handle for the standing GET stream: closing it ends the
    /// inbound channel, which is how the client learns the connection died.
    get_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Channel handle for servers that answer POSTs inline instead of on
    /// the GET stream. The GET task holds its own clone, so taking this on
    /// a fatal error does not end the channel prematurely.
    tx: Mutex<Option<mpsc::Sender<JsonRpcMessage>>>,
}

#[async_trait]
impl McpSender for SseSender {
    async fn send(&self, msg: &JsonRpcMessage, deadline: Duration) -> Result<(), McpError> {
        let body = serde_json::to_vec(msg)?;
        let mut req = self
            .http
            .post(&self.post_url)
            .header("Content-Type", "application/json")
            .timeout(deadline)
            .body(body);
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let response = req
            .send()
            .await
            .map_err(|e| map_reqwest_error(e, deadline))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(map_http_status(status));
        }
        // Per the legacy wire format the POST answer is 202 with an empty
        // body; replies arrive on the GET stream. Some servers answer
        // inline — pump that too so the shared demux sees it either way.
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if content_type.is_empty()
            || content_type.starts_with("text/plain")
            || content_type.starts_with("application/json")
        {
            let body = response
                .bytes()
                .await
                .map_err(|e| map_reqwest_error(e, deadline))?;
            if body.is_empty() {
                return Ok(());
            }
            if let Ok(reply) = serde_json::from_slice::<JsonRpcMessage>(&body) {
                let tx = self.tx_channel().await?;
                let _ = tx.send(reply).await;
            }
        } else if content_type.starts_with("text/event-stream") {
            // Reply-bearing POST stream: same treatment as streamable.
            let tx = self.tx_channel().await?;
            pump_sse_response(response, tx, None, deadline).await;
        }
        Ok(())
    }

    async fn close(&self) {
        *self.tx.lock().await = None;
        if let Some(handle) = self.get_task.lock().await.take() {
            handle.abort();
        }
    }
}

impl SseSender {
    async fn tx_channel(&self) -> Result<mpsc::Sender<JsonRpcMessage>, McpError> {
        self.tx.lock().await.clone().ok_or(McpError::Closed)
    }
}

/// Open a legacy HTTP+SSE connection (server_type `sse`): open the GET
/// stream, wait for the `endpoint` event (bounded by `startup_timeout`),
/// then return the sender plus the inbound channel fed by the GET stream.
pub(crate) async fn legacy_sse(
    spec: &McpServerInfo,
    startup_timeout: Duration,
) -> Result<(Arc<dyn McpSender>, mpsc::Receiver<JsonRpcMessage>), McpError> {
    let url = spec
        .url
        .as_deref()
        .ok_or_else(|| McpError::Connect("sse server missing url".to_string()))?;
    let http = http_client();
    // No per-request timeout here: this response IS the standing stream
    // (reqwest request timeouts cover the body too, which would kill the
    // session after the handshake budget). Endpoint negotiation below is
    // bounded by an explicit outer timeout instead.
    let mut get = http.get(url).header("Accept", "text/event-stream");
    for (k, v) in configured_headers(&spec.headers) {
        get = get.header(k, v);
    }
    let response = get
        .send()
        .await
        .map_err(|e| map_reqwest_error(e, startup_timeout))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(map_http_status(status));
    }

    // Read events until `endpoint` announces the POST URL.
    let mut stream = response.bytes_stream();
    let mut parser = SseParser::new();
    let post_url = tokio::time::timeout(startup_timeout, async {
        loop {
            let Some(chunk) = stream.next().await else {
                return Err(McpError::Closed);
            };
            let bytes = chunk.map_err(|e| map_reqwest_error(e, startup_timeout))?;
            parser.feed(&bytes);
            while let Some(event) = parser.next_event() {
                if event.event.as_deref() == Some("endpoint") {
                    return Ok(resolve_endpoint(url, &event.data));
                }
            }
        }
    })
    .await
    .map_err(|_| McpError::Timeout(startup_timeout.as_millis() as u64))??;

    let (tx, rx) = mpsc::channel(64);
    let get_tx = tx.clone();
    let get_task = tokio::spawn(async move {
        let tx = get_tx;
        // Standing inbound pump: the session's reply channel.
        let _ = tokio::time::timeout(Duration::from_secs(u64::MAX / 2), async {
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        parser.feed(&bytes);
                        while let Some(event) = parser.next_event() {
                            if event.event.as_deref() == Some("endpoint") {
                                continue; // already handled
                            }
                            let Ok(msg) = serde_json::from_str::<JsonRpcMessage>(&event.data)
                            else {
                                continue;
                            };
                            if tx.send(msg).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(_) => return,
                }
            }
        })
        .await;
    });

    let sender = Arc::new(SseSender {
        http,
        post_url,
        headers: configured_headers(&spec.headers),
        get_task: Mutex::new(Some(get_task)),
        tx: Mutex::new(Some(tx)),
    });
    Ok((sender, rx))
}

#[cfg(test)]
mod tests {
    use super::super::transport::http_harness::{FakeServer, Request, Response};
    use super::*;
    use crate::mcp::MCP_PROTOCOL_VERSION_HTTP;
    use serde_json::{json, Value};
    use std::collections::HashMap;

    const INIT_RESULT: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fake","version":"1"}}}"#;

    fn init_body() -> Value {
        json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {"protocolVersion": MCP_PROTOCOL_VERSION_HTTP, "capabilities": {}, "clientInfo": {}},
            "id": 1
        })
    }

    fn http_spec(url: &str) -> McpServerInfo {
        McpServerInfo {
            name: "httpdemo".to_string(),
            command: None,
            args: vec![],
            server_type: "http".to_string(),
            url: Some(url.to_string()),
            disabled: false,
            source: "user".to_string(),
            config_path: "test".to_string(),
            env: HashMap::new(),
            headers: {
                let mut h = HashMap::new();
                h.insert("Authorization".to_string(), "Bearer tok-123".to_string());
                h
            },
        }
    }

    /// Inline-JSON streamable server: initialize → result with session id,
    /// tools/list → one tool, recording every request it sees.
    async fn streamable_json_server() -> (FakeServer, Arc<tokio::sync::Mutex<Vec<Request>>>) {
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let seen_for_handler = Arc::clone(&seen);
        let server = FakeServer::start(Arc::new(move |req: Request| {
            let seen = Arc::clone(&seen_for_handler);
            let is_init = req.body_str().contains("\"method\":\"initialize\"");
            let session_headers = if is_init {
                vec![("Mcp-Session-Id".to_string(), "sid-42".to_string())]
            } else {
                vec![]
            };
            // The future that records the request must not block the
            // response, so recording happens through a try_lock-free clone
            // into the shared vec below (handler is sync; the vec mutex is
            // std-backed via blocking lock).
            let body = if is_init {
                INIT_RESULT.to_string()
            } else if req.body_str().contains("\"method\":\"tools/list\"") {
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"t\",\"inputSchema\":{}}]}}".to_string()
            } else {
                "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}".to_string()
            };
            let _ = seen; // recording handled by harness `seen`
            Response::Full {
                status: 200,
                content_type: "application/json",
                headers: session_headers,
                body: body.into_bytes(),
            }
        }));
        (server, seen)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn streamable_json_roundtrip_with_session_and_headers() {
        let (server, _seen) = streamable_json_server().await;
        let spec = http_spec(&format!("{}/mcp", server.url));
        let (sender, mut rx) = streamable_http(&spec).unwrap();

        // Handshake over the transport.
        sender
            .send(
                &serde_json::from_value::<JsonRpcMessage>(init_body()).unwrap(),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        let init_reply = rx.recv().await.expect("init reply");
        match init_reply {
            JsonRpcMessage::Response(r) => {
                assert_eq!(r.id, RequestId::Number(1));
            }
            other => panic!("expected response, got {other:?}"),
        }

        sender
            .send(
                &serde_json::from_value::<JsonRpcMessage>(json!({
                    "jsonrpc": "2.0", "method": "tools/list", "params": {}, "id": 2
                }))
                .unwrap(),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        let list_reply = rx.recv().await.expect("tools/list reply");
        match list_reply {
            JsonRpcMessage::Response(r) => {
                assert!(r.result["tools"].as_array().unwrap().len() == 1);
            }
            other => panic!("expected response, got {other:?}"),
        }

        // The server must have seen the configured auth header and, on the
        // second request, the session id.
        let recorded = server.record().await;
        assert!(recorded[0].header("authorization") == Some("Bearer tok-123"));
        assert_eq!(recorded[1].header("mcp-session-id"), Some("sid-42"));

        sender.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn streamable_sse_response_is_demuxed() {
        let server = FakeServer::start(Arc::new(|req: Request| {
            let body = req.body_str();
            if body.contains("\"method\":\"initialize\"") {
                return Response::json(200, INIT_RESULT.to_string());
            }
            // tools/call answered as an SSE stream.
            Response::Sse {
                status: 200,
                chunks: vec![
                    b"event: message\ndata: ".to_vec(),
                    b"{\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"streamed\"}]}}\n\n".to_vec(),
                ],
            }
        }));
        let spec = http_spec(&format!("{}/mcp", server.url));
        let (sender, mut rx) = streamable_http(&spec).unwrap();
        sender
            .send(
                &serde_json::from_value::<JsonRpcMessage>(json!({
                    "jsonrpc": "2.0", "method": "tools/call", "params": {}, "id": 9
                }))
                .unwrap(),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        let reply = rx.recv().await.expect("streamed reply");
        match reply {
            JsonRpcMessage::Response(r) => {
                assert_eq!(r.result["content"][0]["text"], json!("streamed"));
            }
            other => panic!("expected response, got {other:?}"),
        }
        sender.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_rejection_maps_to_connect() {
        let server = FakeServer::start(Arc::new(|_req: Request| {
            Response::json(401, "{\"error\":\"unauthorized\"}".to_string())
        }));
        let spec = http_spec(&format!("{}/mcp", server.url));
        let (sender, _rx) = streamable_http(&spec).unwrap();
        let err = sender
            .send(
                &serde_json::from_value::<JsonRpcMessage>(init_body()).unwrap(),
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, McpError::Connect(_)), "got {err}");
        sender.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unreachable_server_maps_to_closed_and_ends_channel() {
        // Port 1 on localhost is reliably closed in test environments.
        let spec = http_spec("http://127.0.0.1:1/mcp");
        let (sender, mut rx) = streamable_http(&spec).unwrap();
        let err = sender
            .send(
                &serde_json::from_value::<JsonRpcMessage>(init_body()).unwrap(),
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, McpError::Closed), "got {err}");
        // The channel must end so the supervisor learns the connection died.
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn legacy_sse_endpoint_negotiation_and_post() {
        let server = FakeServer::start(Arc::new(|req: Request| {
            if req.method == "GET" {
                // CRLF throughout to exercise the parser normalization.
                return Response::Sse {
                    status: 200,
                    chunks: vec![
                        b"event: endpoint\r\ndata: /msg?sid=1\r\n\r\n".to_vec(),
                        b"event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":5,\"result\":{\"tools\":[{\"name\":\"t\",\"inputSchema\":{}}]}}\r\n\r\n".to_vec(),
                    ],
                };
            }
            assert_eq!(
                req.path, "/msg?sid=1",
                "POST must land on the announced endpoint"
            );
            Response::Full {
                status: 202,
                content_type: "text/plain",
                headers: vec![],
                body: Vec::new(),
            }
        }));
        let mut spec = http_spec(&format!("{}/sse", server.url));
        spec.server_type = "sse".to_string();
        let (sender, mut rx) = {
            let (sender, rx) = super::legacy_sse(&spec, Duration::from_secs(5))
                .await
                .unwrap();
            (sender, rx)
        };
        sender
            .send(
                &serde_json::from_value::<JsonRpcMessage>(json!({
                    "jsonrpc": "2.0", "method": "tools/list", "params": {}, "id": 5
                }))
                .unwrap(),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        // The reply arrives on the GET stream.
        let reply = rx.recv().await.expect("GET-stream reply");
        match reply {
            JsonRpcMessage::Response(r) => assert_eq!(r.id, RequestId::Number(5)),
            other => panic!("expected response, got {other:?}"),
        }
        sender.close().await;
    }

    #[test]
    fn resolve_endpoint_variants() {
        assert_eq!(
            resolve_endpoint("http://h:80/base", "/msg?sid=1"),
            "http://h:80/msg?sid=1"
        );
        assert_eq!(
            resolve_endpoint("http://h:80/base", "http://other/x"),
            "http://other/x"
        );
        assert_eq!(
            resolve_endpoint("http://h:80/base", "msg"),
            "http://h:80/base/msg"
        );
    }

    #[test]
    fn reserved_headers_filtered() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "x".to_string());
        headers.insert("Content-Type".to_string(), "evil".to_string());
        let filtered = configured_headers(&headers);
        assert_eq!(
            filtered,
            vec![("Authorization".to_string(), "x".to_string())]
        );
    }
}
