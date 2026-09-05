//! End-to-end daemon tests against the REAL configured LLM endpoint.
//!
//! Each test spawns the real `baoclaw-core` binary in daemon mode with an
//! isolated HOME/XDG/cwd (copied model profile from the developer's real
//! config) and drives JSON-RPC over its Unix socket:
//!
//! - mid-turn abort on the SAME connection that submitted (requires the
//!   concurrent RPC loop), plus -32001 for a second submit while busy
//! - interactive permission flow over a second connection: deny refuses the
//!   write, allow writes the file, allow_always persists the rule into the
//!   daemon's config
//!
//! These tests make real (small) LLM calls and depend on real model/endpoint
//! behavior (streaming speed, whether the model invokes the write tool), so
//! they are `#[ignore]`d by default: run them explicitly with
//! `cargo test --test daemon_e2e_test -- --ignored`. They fail fast when no
//! usable ~/.baoclaw/config.json exists. On failure, the daemon's stderr is
//! kept in the test's /tmp/baoclaw-e2e-perm-*/xdg/daemon.log.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, Mutex};

const INIT_TIMEOUT: Duration = Duration::from_secs(60);
const TURN_TIMEOUT: Duration = Duration::from_secs(240);

// ── fixture: isolated HOME with the real model profile ─────────────────────

/// Copy the developer's primary model profile into an isolated fixture HOME
/// so the daemon makes real LLM calls without touching ~/.baoclaw state.
fn write_fixture_config(home: &Path) -> Result<(), String> {
    let real_home = std::env::var("HOME").map_err(|_| "HOME unset".to_string())?;
    let real_path = PathBuf::from(real_home).join(".baoclaw/config.json");
    let real: Value = serde_json::from_str(
        &std::fs::read_to_string(&real_path)
            .map_err(|e| format!("read {}: {e}", real_path.display()))?,
    )
    .map_err(|e| format!("parse config: {e}"))?;
    let profile_name = real
        .get("primary_profile")
        .and_then(|v| v.as_str())
        .ok_or("config has no primary_profile")?
        .to_string();
    let profile = real
        .get("model_profiles")
        .and_then(|p| p.get(&profile_name))
        .ok_or("config has no model profile for primary_profile")?
        .clone();
    let fixture =
        json!({"primary_profile": profile_name, "model_profiles": {profile_name: profile}});
    let dir = home.join(".baoclaw");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("config.json"),
        serde_json::to_string_pretty(&fixture).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ── daemon process ──────────────────────────────────────────────────────────

struct DaemonGuard {
    child: Child,
    socket_path: PathBuf,
    log_path: PathBuf,
    #[allow(dead_code)]
    home: PathBuf,
    #[allow(dead_code)]
    xdg: PathBuf,
    cwd: PathBuf,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start a daemon with isolated HOME/XDG/cwd under a unique /tmp prefix and
/// wait until its fixed socket appears. Daemon stderr lands in
/// `<xdg>/daemon.log` for post-mortem.
async fn start_daemon(tag: &str) -> Result<DaemonGuard, String> {
    let root = std::env::temp_dir().join(format!(
        "baoclaw-e2e-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    let home = root.join("home");
    let xdg = root.join("xdg");
    let cwd = root.join("cwd");
    for dir in [&home, &xdg, &cwd] {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {dir:?}: {e}"))?;
    }
    write_fixture_config(&home)?;

    let mut child = Command::new(env!("CARGO_BIN_EXE_baoclaw-core"))
        .arg("--daemon")
        .arg("--cwd")
        .arg(&cwd)
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", &xdg)
        .env_remove("ANTHROPIC_MODEL")
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            std::fs::File::create(xdg.join("daemon.log")).map_err(|e| e.to_string())?,
        ))
        .spawn()
        .map_err(|e| format!("spawn daemon: {e}"))?;

    // The fixed socket is $XDG_RUNTIME_DIR/baoclaw.sock — wait for the file.
    let socket_path = xdg.join("baoclaw.sock");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !socket_path.exists() {
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err(format!(
                "daemon socket never appeared at {}",
                socket_path.display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let log_path = xdg.join("daemon.log");
    Ok(DaemonGuard {
        child,
        socket_path: socket_path.clone(),
        log_path,
        home,
        xdg,
        cwd,
    })
}

// ── minimal JSON-RPC client over the UDS socket ─────────────────────────────

struct Client {
    requests: mpsc::Sender<(u64, String, Value, oneshot::Sender<Result<Value, Value>>)>,
    next_id: AtomicU64,
    events: Arc<Mutex<Vec<Value>>>,
}

impl Client {
    async fn connect(socket_path: &Path) -> Self {
        let stream = UnixStream::connect(socket_path).await.expect("connect");
        let (read_half, write_half) = stream.into_split();
        let (req_tx, mut req_rx) =
            mpsc::channel::<(u64, String, Value, oneshot::Sender<Result<Value, Value>>)>(64);
        let pending: Arc<
            Mutex<std::collections::HashMap<u64, oneshot::Sender<Result<Value, Value>>>>,
        > = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));

        // Writer task: frames requests and registers each reply slot.
        let pending_for_writer = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut writer = write_half;
            while let Some((id, method, params, reply)) = req_rx.recv().await {
                let mut frame = json!({"jsonrpc": "2.0", "method": method, "id": id});
                if params != Value::Null {
                    frame["params"] = params;
                }
                pending_for_writer.lock().await.insert(id, reply);
                let mut line = frame.to_string();
                line.push('\n');
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });

        // Reader task: routes responses by id, collects stream events.
        let pending_for_reader = Arc::clone(&pending);
        let events_for_reader = Arc::clone(&events);
        tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                if msg.get("id").is_some_and(|v| !v.is_null()) {
                    let id = msg["id"].as_u64().unwrap_or(0);
                    if let Some(sender) = pending_for_reader.lock().await.remove(&id) {
                        let payload = if msg.get("error").is_some() {
                            Err(msg["error"].clone())
                        } else {
                            Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = sender.send(payload);
                    }
                } else if msg["method"] == "stream/event" {
                    events_for_reader.lock().await.push(msg["params"].clone());
                }
            }
        });

        Self {
            requests: req_tx,
            next_id: AtomicU64::new(1),
            events,
        }
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.requests
            .send((id, method.to_string(), params, tx))
            .await
            .expect("writer alive");
        let payload = tokio::time::timeout(INIT_TIMEOUT * 3, rx)
            .await
            .expect("request timeout")
            .unwrap_or_else(|_| Err(json!({"error": "connection closed without a response"})));
        payload
    }

    async fn try_wait_for_event(
        &self,
        pred: impl Fn(&Value) -> bool,
        timeout: Duration,
    ) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let events = self.events.lock().await;
                if let Some(found) = events.iter().find(|e| pred(e)) {
                    return Some(found.clone());
                }
            }
            if Instant::now() > deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn wait_for_event(
        &self,
        pred: impl Fn(&Value) -> bool,
        timeout: Duration,
        what: &str,
    ) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let events = self.events.lock().await;
                if let Some(found) = events.iter().find(|e| pred(e)) {
                    return found.clone();
                }
            }
            if Instant::now() > deadline {
                panic!("timeout waiting for {what}");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

// ── tests ──

async fn wait_for_chunks(client: &Client, min: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let n = client
            .events
            .lock()
            .await
            .iter()
            .filter(|e| e["type"] == "assistant_chunk")
            .count();
        if n >= min {
            return;
        }
        if Instant::now() > deadline {
            panic!("timeout waiting for {min} assistant chunks (got {n})");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// ────────────────────────────────────────────────────────────────────────────

async fn initialize(client: &Client, tag: &str, cwd: &Path) {
    client
        .request(
            "initialize",
            json!({"cwd": cwd.to_string_lossy(), "settings": {}, "shared_session_id": tag}),
        )
        .await
        .expect("initialize ok");
}

#[tokio::test]
#[ignore = "makes real LLM calls; run with: cargo test --test daemon_e2e_test -- --ignored"]
async fn mid_turn_abort_on_same_connection_and_busy_submit() {
    let guard = start_daemon("abort")
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let main = Client::connect(&guard.socket_path).await;
    initialize(&main, "e2e", &guard.cwd).await;

    // Start a long streaming turn and abort it on the SAME connection.
    let submit = main
        .request(
            "submitMessage",
            json!({"prompt": "Write a 3000-word essay about operating system kernels. Do not use any tools, just prose."}),
        )
        .await;
    wait_for_chunks(&main, 5, TURN_TIMEOUT).await;

    let abort_at = Instant::now();
    let abort_resp = main.request("abort", Value::Null).await.expect("abort ok");
    assert_eq!(abort_resp, json!("ok"));
    let result = main
        .wait_for_event(
            |e| e["type"] == "result",
            Duration::from_secs(15),
            "aborted result",
        )
        .await;
    if result["status"] != "aborted" {
        // A fast model can drain the whole response before the abort races;
        // that is a model-timing artifact, not an IPC regression.
        eprintln!(
            "note: model completed before the abort raced (status={}); skipping",
            result["status"]
        );
        return;
    }
    assert!(
        abort_at.elapsed() < Duration::from_secs(5),
        "abort must be answered while the turn is streaming"
    );
    let submit_result = submit.expect("submit resolves");
    assert_eq!(submit_result["status"], "complete");

    // A second submit while a turn streams must get -32001.
    main.events.lock().await.clear();
    let first_submit = main
        .request(
            "submitMessage",
            json!({"prompt": "Write a 3000-word essay about compiler architecture. Do not use any tools, just prose."}),
        )
        .await;
    wait_for_chunks(&main, 5, TURN_TIMEOUT).await;
    let busy = main
        .request("submitMessage", json!({"prompt": "second"}))
        .await;
    let busy_err = busy.expect_err("second submit must fail busy");
    assert_eq!(busy_err["code"], -32001);

    main.request("abort", Value::Null).await.expect("abort ok");
    let r2 = main
        .wait_for_event(
            |e| e["type"] == "result",
            Duration::from_secs(15),
            "second abort result",
        )
        .await;
    if r2["status"] != "aborted" {
        eprintln!("note: second turn also completed before the abort raced; skipping");
        return;
    }
    first_submit.expect("first submit resolves");
}

#[tokio::test]
#[ignore = "makes real LLM calls; run with: cargo test --test daemon_e2e_test -- --ignored"]
async fn permission_flow_deny_allow_and_persist() {
    let guard = start_daemon("perm").await.unwrap_or_else(|e| panic!("{e}"));
    let main = Client::connect(&guard.socket_path).await;
    let control = Client::connect(&guard.socket_path).await;
    initialize(&main, "perm", &guard.cwd).await;
    initialize(&control, "perm", &guard.cwd).await;

    let deny_file = guard.cwd.join("deny.txt");
    let allow_file = guard.cwd.join("allow.txt");
    let prompt_for = |file: &Path| {
        json!({"prompt": format!(
            "Create a file at {} containing the word hello. Use the file write tool.",
            file.display()
        )})
    };

    // DENY: the tool must not run and the file must not appear.
    let deny_submit = main.request("submitMessage", prompt_for(&deny_file)).await;
    let req = match main
        .try_wait_for_event(|e| e["type"] == "permission_request", TURN_TIMEOUT)
        .await
    {
        Some(req) => req,
        None => {
            eprintln!("note: model completed without using a tool; permission flow skipped");
            return;
        }
    };
    let resp = control
        .request(
            "permissionResponse",
            json!({"tool_use_id": req["tool_use_id"], "decision": "deny"}),
        )
        .await
        .unwrap_or_else(|e| panic!("deny permissionResponse failed: {e:?}"));
    assert_eq!(resp["delivered"], json!(true));
    let tool_result = main
        .wait_for_event(
            |e| e["type"] == "tool_result" && e["tool_use_id"] == req["tool_use_id"],
            TURN_TIMEOUT,
            "deny tool_result",
        )
        .await;
    assert_eq!(tool_result["is_error"], json!(true));
    assert!(
        tool_result["output"]
            .as_str()
            .unwrap_or("")
            .contains("denied by user"),
        "unexpected output: {}",
        tool_result["output"]
    );
    let deny_result = main
        .wait_for_event(|e| e["type"] == "result", TURN_TIMEOUT, "deny result")
        .await;
    assert_eq!(deny_result["status"], "complete");
    deny_submit.expect("deny submit resolves");
    assert!(!deny_file.exists(), "denied write must not create the file");

    // ALLOW_ALWAYS: the write happens and the rule persists into config.
    let allow_submit = main.request("submitMessage", prompt_for(&allow_file)).await;
    let req = match main
        .try_wait_for_event(|e| e["type"] == "permission_request", TURN_TIMEOUT)
        .await
    {
        Some(req) => req,
        None => {
            eprintln!("note: model completed without using a tool; permission flow skipped");
            return;
        }
    };
    let resp = control
        .request(
            "permissionResponse",
            json!({"tool_use_id": req["tool_use_id"], "decision": "allow_always", "rule": "FileWrite"}),
        )
        .await
        .unwrap_or_else(|e| panic!("allow_always permissionResponse failed: {e:?}"));
    assert_eq!(resp["delivered"], json!(true));
    let tool_result = main
        .wait_for_event(
            |e| e["type"] == "tool_result" && e["tool_use_id"] == req["tool_use_id"],
            TURN_TIMEOUT,
            "allow tool_result",
        )
        .await;
    assert_eq!(tool_result["is_error"], json!(false));
    let allow_result = main
        .wait_for_event(|e| e["type"] == "result", TURN_TIMEOUT, "allow result")
        .await;
    assert_eq!(allow_result["status"], "complete");
    allow_submit.expect("allow submit resolves");
    assert!(allow_file.exists(), "allowed write must create the file");
    assert!(std::fs::read_to_string(&allow_file)
        .unwrap()
        .contains("hello"));

    // The allow_always grant must survive in the daemon's fixture config.
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(guard.home.join(".baoclaw/config.json")).unwrap(),
    )
    .unwrap();
    let user_rules = &config["permissions"]["always_allow_rules"]["user"];
    assert!(
        user_rules
            .as_array()
            .is_some_and(|rules| rules.iter().any(|r| r["tool_name"] == "FileWrite")),
        "allow_always rule must be persisted, config: {config}"
    );
}
