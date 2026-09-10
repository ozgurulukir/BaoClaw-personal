//! Connection manager: one supervised slot per configured MCP server.
//!
//! Lifecycle contract: the tool catalog is frozen at boot. A slot that fails
//! its FIRST connection gives up immediately (there is nothing to register —
//! its tools were never fetched and boot is over). A slot that WAS ready may
//! crash later; a supervisor then reconnects with bounded exponential backoff
//! up to `max_restarts`, and while disconnected its registered tools return
//! error results instead of disappearing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{watch, RwLock};

use crate::config::BaoclawConfig;
use crate::discovery::mcp_config::McpServerInfo;
use crate::tools::Tool;

use super::bridge::McpToolBridge;
use super::client::{McpError, StdioMcpClient};
use super::types::{CallToolOutcome, McpToolDef};
use super::{composite_tool_name, MCP_BACKOFF_INITIAL_MS, MCP_BACKOFF_MAX_MS};

/// Runtime knobs, mirrored from config at boot
/// (same bridge pattern as `MicroCompactConfig`).
#[derive(Clone, Debug)]
pub struct McpLaunchConfig {
    /// Per-handshake-RPC budget: bounds each initialize / tools/list step.
    pub startup_timeout: Duration,
    /// Per tools/call budget.
    pub call_timeout: Duration,
    /// Reconnect attempts after which a previously-ready slot gives up.
    pub max_restarts: u32,
    /// First backoff delay (tests shrink this).
    pub backoff_initial: Duration,
}

impl Default for McpLaunchConfig {
    fn default() -> Self {
        // config.rs owns the numbers; this keeps the two surfaces equal.
        Self::from(&BaoclawConfig::default())
    }
}

impl From<&BaoclawConfig> for McpLaunchConfig {
    fn from(config: &BaoclawConfig) -> Self {
        Self {
            startup_timeout: Duration::from_secs(config.mcp_startup_timeout_secs),
            call_timeout: Duration::from_secs(config.mcp_call_timeout_secs),
            max_restarts: config.mcp_max_restarts,
            backoff_initial: Duration::from_millis(MCP_BACKOFF_INITIAL_MS),
        }
    }
}

/// Lifecycle state of one server slot (serialized into `listMcpServers`).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerRuntimeState {
    /// Boot attempt or reconnect in flight.
    Connecting,
    Ready,
    /// Was ready; process died; backoff/reconnect pending.
    Disconnected,
    /// Gave up: boot failure, or restart budget exhausted.
    Failed,
    /// Disabled / non-stdio / invalid entry (never spawned).
    Skipped,
}

/// Everything `listMcpServers` needs per server. `reason` is a skip reason or
/// the last error — never contains env values.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ServerStatus {
    pub state: ServerRuntimeState,
    pub tool_count: usize,
    pub restarts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

struct SlotInner {
    state: ServerRuntimeState,
    tool_defs: Vec<McpToolDef>,
    restarts: u32,
    reason: Option<String>,
    client: Option<Arc<StdioMcpClient>>,
}

struct ServerSlot {
    spec: McpServerInfo,
    cfg: McpLaunchConfig,
    /// std Mutex: tiny critical sections, never held across an await.
    inner: std::sync::Mutex<SlotInner>,
    /// Set once the FIRST connection attempt settles (Ready or Failed);
    /// `boot_and_build_tools` awaits it.
    boot_settled: watch::Receiver<bool>,
}

pub struct ConnectionManager {
    slots: RwLock<HashMap<String, Arc<ServerSlot>>>,
    /// Reset on every Ready transition so transport-failure records
    /// accumulated while a server was down never disable its registered
    /// tools after it comes back.
    tool_health: Option<crate::engine::tool_health::ToolHealthHandle>,
}

/// Why a tools/call through the manager failed.
#[derive(Debug)]
pub enum McpCallError {
    /// Slot not Ready right now (Disconnected/Connecting/Failed/Skipped).
    Disconnected {
        state: ServerRuntimeState,
        reason: Option<String>,
    },
    /// JSON-RPC error returned by the server.
    Remote {
        code: i32,
        message: String,
    },
    /// Connection died mid-call; the supervisor will reconnect.
    Lost,
    Timeout(u64),
}

impl ConnectionManager {
    /// Empty manager: for tests of components that only need the handle
    /// (their lookups miss and surface as "unknown server").
    #[cfg(test)]
    pub(crate) fn empty() -> Arc<Self> {
        Arc::new(Self {
            slots: RwLock::new(HashMap::new()),
            tool_health: None,
        })
    }

    /// Validate + register slots and spawn one supervisor task per active
    /// server. Does NOT await readiness. Filtering happens HERE (not in
    /// discovery) because sse/http entries legitimately lack `command` and
    /// discovery must still list them.
    pub async fn start_all(
        servers: Vec<McpServerInfo>,
        cfg: McpLaunchConfig,
        tool_health: Option<crate::engine::tool_health::ToolHealthHandle>,
    ) -> Arc<Self> {
        let manager = Arc::new(Self {
            slots: RwLock::new(HashMap::new()),
            tool_health,
        });
        {
            let mut slots = manager.slots.write().await;
            for spec in servers {
                let skip_reason = if spec.disabled {
                    Some("disabled".to_string())
                } else if spec.server_type != "stdio" {
                    Some(format!(
                        "transport '{}' not supported (stdio only)",
                        spec.server_type
                    ))
                } else if spec.command.as_ref().map(String::is_empty).unwrap_or(true) {
                    Some("stdio server missing command".to_string())
                } else {
                    None
                };

                let (boot_tx, boot_rx) = watch::channel(false);
                let slot = Arc::new(ServerSlot {
                    spec,
                    cfg: cfg.clone(),
                    inner: std::sync::Mutex::new(SlotInner {
                        state: match skip_reason {
                            Some(_) => ServerRuntimeState::Skipped,
                            None => ServerRuntimeState::Connecting,
                        },
                        tool_defs: Vec::new(),
                        restarts: 0,
                        reason: skip_reason,
                        client: None,
                    }),
                    boot_settled: boot_rx,
                });
                if slot.inner.lock().unwrap().state == ServerRuntimeState::Connecting {
                    let tracker = manager.tool_health.clone();
                    tokio::spawn(supervise(Arc::clone(&slot), boot_tx, tracker));
                } else {
                    let _ = boot_tx.send(true);
                }
                slots.insert(slot.spec.name.clone(), slot);
            }
        }
        manager
    }

    /// Eager boot connect: await every slot's first attempt, then build one
    /// [`McpToolBridge`] per tool of every Ready slot. Failed slots get NO
    /// bridges — their schemas were never fetched and the catalog is frozen.
    pub async fn boot_and_build_tools(self: &Arc<Self>) -> Vec<Arc<dyn Tool>> {
        let mut slots: Vec<Arc<ServerSlot>> = self.slots.read().await.values().cloned().collect();
        // HashMap iteration order is nondeterministic; the registered tool
        // list must not shuffle between boots.
        slots.sort_by(|a, b| a.spec.name.cmp(&b.spec.name));
        for slot in &slots {
            let _ = slot.boot_settled.clone().wait_for(|v| *v).await;
        }

        let mut bridges: Vec<Arc<dyn Tool>> = Vec::new();
        // Dispatch is case-insensitive, so collision detection must be too.
        let mut seen = std::collections::HashSet::new();
        for slot in &slots {
            let g = slot.inner.lock().unwrap();
            // A slot that reached Ready and crashed in the microseconds
            // before this read still fetched its catalog — the frozen
            // catalog contract says its tools must be registered.
            if g.state != ServerRuntimeState::Ready && g.tool_defs.is_empty() {
                continue;
            }
            for def in &g.tool_defs {
                let registry_name = composite_tool_name(&slot.spec.name, &def.name);
                if !seen.insert(registry_name.to_lowercase()) {
                    eprintln!("[mcp] duplicate tool '{registry_name}' skipped");
                    continue;
                }
                bridges.push(Arc::new(McpToolBridge::new(
                    Arc::clone(self),
                    slot.spec.name.clone(),
                    def.clone(),
                    registry_name,
                )));
            }
        }
        bridges
    }

    /// Forward a tools/call to the named server. Called only by
    /// [`McpToolBridge`].
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<CallToolOutcome, McpCallError> {
        let slot = self
            .slots
            .read()
            .await
            .get(server)
            .cloned()
            .ok_or_else(|| McpCallError::Disconnected {
                state: ServerRuntimeState::Skipped,
                reason: Some(format!("unknown MCP server '{server}'")),
            })?;

        let (client, call_timeout) = {
            let g = slot.inner.lock().unwrap();
            if g.state != ServerRuntimeState::Ready {
                return Err(McpCallError::Disconnected {
                    state: g.state.clone(),
                    reason: g.reason.clone(),
                });
            }
            (
                g.client.clone().expect("Ready slot always holds a client"),
                slot.cfg.call_timeout,
            )
        };

        match client.call_tool(tool, arguments, call_timeout).await {
            Ok(outcome) => Ok(outcome),
            Err(McpError::Remote { code, message }) => Err(McpCallError::Remote { code, message }),
            Err(McpError::Timeout(ms)) => Err(McpCallError::Timeout(ms)),
            Err(_) => Err(McpCallError::Lost),
        }
    }

    /// Snapshot for `listMcpServers`. Keys are original server names.
    pub async fn status_map(&self) -> HashMap<String, ServerStatus> {
        self.slots
            .read()
            .await
            .iter()
            .map(|(name, slot)| {
                let g = slot.inner.lock().unwrap();
                (
                    name.clone(),
                    ServerStatus {
                        state: g.state.clone(),
                        tool_count: g.tool_defs.len(),
                        restarts: g.restarts,
                        reason: g.reason.clone(),
                    },
                )
            })
            .collect()
    }

    /// Kill every live child. Must run before `std::process::exit` (drops do
    /// not fire there, so `kill_on_drop` would not).
    pub async fn shutdown_all(&self) {
        let slots: Vec<Arc<ServerSlot>> = self.slots.read().await.values().cloned().collect();
        for slot in slots {
            let client = slot.inner.lock().unwrap().client.clone();
            if let Some(client) = client {
                client.shutdown().await;
            }
        }
    }
}

/// Per-server supervisor: connect → Ready until the process dies → bounded
/// backoff reconnect → Failed after the restart budget. A failed FIRST
/// attempt goes straight to Failed (frozen catalog: a later reconnect would
/// have no registered tools to serve).
async fn supervise(
    slot: Arc<ServerSlot>,
    boot_tx: watch::Sender<bool>,
    tool_health: Option<crate::engine::tool_health::ToolHealthHandle>,
) {
    let cfg = slot.cfg.clone();
    let mut ever_ready = false;
    let mut backoff = cfg.backoff_initial;
    let backoff_max = Duration::from_millis(MCP_BACKOFF_MAX_MS);

    loop {
        match StdioMcpClient::spawn(
            &slot.spec.name,
            slot.spec.command.as_deref().unwrap_or_default(),
            &slot.spec.args,
            &slot.spec.env,
            cfg.startup_timeout,
        )
        .await
        {
            Ok((client, tool_defs)) => {
                ever_ready = true;
                let client = Arc::new(client);
                {
                    let mut g = slot.inner.lock().unwrap();
                    g.state = ServerRuntimeState::Ready;
                    g.reason = None;
                    g.restarts = 0;
                    g.tool_defs = tool_defs;
                    g.client = Some(Arc::clone(&client));
                }
                // Fresh episode: backoff restarts small and any failure
                // records accumulated while the server was down are wiped —
                // otherwise a brief outage would keep the registered tools
                // hard-blocked by tool-health long after recovery.
                backoff = cfg.backoff_initial;
                if let Some(th) = &tool_health {
                    let g = slot.inner.lock().unwrap();
                    for def in &g.tool_defs {
                        th.reset_tool(&composite_tool_name(&slot.spec.name, &def.name));
                    }
                }
                let _ = boot_tx.send(true);

                client.wait_closed().await;

                let mut g = slot.inner.lock().unwrap();
                g.client = None;
                g.restarts += 1;
                g.reason = Some("server process exited".to_string());
                g.state = ServerRuntimeState::Disconnected;
            }
            Err(e) => {
                let give_up = {
                    let mut g = slot.inner.lock().unwrap();
                    g.reason = Some(e.to_string());
                    if !ever_ready {
                        // Boot failure: frozen catalog means retrying could
                        // never produce registered tools.
                        g.state = ServerRuntimeState::Failed;
                        true
                    } else {
                        g.state = ServerRuntimeState::Disconnected;
                        g.restarts += 1;
                        if g.restarts > cfg.max_restarts {
                            g.state = ServerRuntimeState::Failed;
                            g.reason = Some(format!(
                                "gave up after {} restart attempts",
                                cfg.max_restarts
                            ));
                            true
                        } else {
                            false
                        }
                    }
                };
                let _ = boot_tx.send(true);
                if give_up {
                    return;
                }
            }
        }

        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(backoff_max);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::discovery::mcp_config::McpServerInfo;
    use std::path::{Path, PathBuf};

    fn server_entry(name: &str, command: &str, disabled: bool) -> McpServerInfo {
        McpServerInfo {
            name: name.to_string(),
            command: Some(command.to_string()),
            args: vec![],
            server_type: "stdio".to_string(),
            url: None,
            disabled,
            source: "user".to_string(),
            config_path: "test".to_string(),
            env: HashMap::new(),
        }
    }

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    async fn settle(manager: &Arc<ConnectionManager>, server: &str) -> ServerStatus {
        for _ in 0..200 {
            if let Some(st) = manager.status_map().await.get(server) {
                if matches!(
                    st.state,
                    ServerRuntimeState::Ready | ServerRuntimeState::Failed
                ) {
                    return st.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("slot '{server}' never settled");
    }

    #[tokio::test]
    async fn disabled_and_non_stdio_and_commandless_slots_are_skipped() {
        let manager = ConnectionManager::start_all(
            vec![
                server_entry("off", "true", true),
                McpServerInfo {
                    server_type: "sse".to_string(),
                    url: Some("http://localhost:1".into()),
                    command: None,
                    env: HashMap::new(),
                    ..server_entry("remote", "", false)
                },
                server_entry("nocommand", "", false),
            ],
            McpLaunchConfig::default(),
            None,
        )
        .await;
        let statuses = manager.status_map().await;
        assert_eq!(statuses["off"].state, ServerRuntimeState::Skipped);
        assert_eq!(statuses["off"].reason.as_deref(), Some("disabled"));
        assert_eq!(statuses["remote"].state, ServerRuntimeState::Skipped);
        assert!(statuses["remote"]
            .reason
            .clone()
            .unwrap()
            .contains("not supported"));
        assert_eq!(statuses["nocommand"].state, ServerRuntimeState::Skipped);
        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn boot_failure_yields_failed_and_no_bridges() {
        let dir = tempfile::TempDir::new().unwrap();
        let missing = dir
            .path()
            .join("does-not-exist")
            .to_str()
            .unwrap()
            .to_string();
        let manager = ConnectionManager::start_all(
            vec![server_entry("broken", &missing, false)],
            McpLaunchConfig::default(),
            None,
        )
        .await;
        let st = settle(&manager, "broken").await;
        assert_eq!(st.state, ServerRuntimeState::Failed);
        assert!(manager.boot_and_build_tools().await.is_empty());
        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn ready_slot_builds_bridges_with_composite_names() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = r#"
id_of() { printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(id_of "$line")
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fake","version":"1"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=$(id_of "$line")
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"alpha","inputSchema":{}},{"name":"beta","inputSchema":{}}]}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      id=$(id_of "$line")
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"done"}],"isError":false}}\n' "$id"
      ;;
  esac
done
"#;
        let cmd = write_script(dir.path(), "ok.sh", script);
        let cfg = McpLaunchConfig {
            call_timeout: Duration::from_secs(5),
            ..McpLaunchConfig::default()
        };
        let manager = ConnectionManager::start_all(
            vec![server_entry("toolsrv", cmd.to_str().unwrap(), false)],
            cfg,
            None,
        )
        .await;
        let st = settle(&manager, "toolsrv").await;
        assert_eq!(st.state, ServerRuntimeState::Ready);
        assert_eq!(st.tool_count, 2);

        let bridges = manager.boot_and_build_tools().await;
        let mut names: Vec<String> = bridges.iter().map(|b| b.name().to_string()).collect();
        names.sort();
        assert_eq!(names, vec!["mcp__toolsrv__alpha", "mcp__toolsrv__beta"]);

        // Tool calls round-trip through the bridge.
        let outcome = manager
            .call_tool("toolsrv", "alpha", serde_json::json!({}))
            .await
            .expect("call through manager");
        assert!(!outcome.is_error);
        assert_eq!(outcome.data, serde_json::json!("done"));
        manager.shutdown_all().await;
    }

    /// Server that stays alive after the handshake and exits when it
    /// receives a tools/call: the slot must cycle Ready → Disconnected →
    /// (reconnect with shrunk test backoff) → Ready, deterministically.
    const CRASHY_SERVER: &str = r#"
id_of() { printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(id_of "$line")
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fake","version":"1"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=$(id_of "$line")
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"t","inputSchema":{}}]}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      exit 5
      ;;
  esac
done
"#;

    #[tokio::test]
    async fn crash_then_reconnect_recovers_ready_state() {
        let dir = tempfile::TempDir::new().unwrap();
        let cmd = write_script(dir.path(), "crashy.sh", CRASHY_SERVER);
        let cfg = McpLaunchConfig {
            backoff_initial: Duration::from_millis(10),
            ..McpLaunchConfig::default()
        };
        let manager = ConnectionManager::start_all(
            vec![server_entry("crashy", cmd.to_str().unwrap(), false)],
            cfg,
            None,
        )
        .await;
        let st = settle(&manager, "crashy").await;
        assert_eq!(st.state, ServerRuntimeState::Ready);

        // The tools/call makes the current server process exit; the
        // supervisor must observe the death and reconnect a fresh child.
        let _ = manager
            .call_tool("crashy", "t", serde_json::json!({}))
            .await;
        for _ in 0..200 {
            let statuses = manager.status_map().await;
            if statuses["crashy"].restarts >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let st = manager.status_map().await.remove("crashy").unwrap();
        assert!(
            matches!(
                st.state,
                ServerRuntimeState::Ready | ServerRuntimeState::Disconnected
            ),
            "expected cycling states after crash, got {:?}",
            st.state
        );
        manager.shutdown_all().await;
    }

    /// A previously-Ready slot that keeps failing must give up after the
    /// restart budget: Ready → crash → one failing reconnect → Failed with
    /// a "gave up" reason (the only Failed path after a successful boot).
    #[tokio::test]
    async fn restart_budget_exhaustion_marks_failed() {
        let dir = tempfile::TempDir::new().unwrap();
        // First run: full handshake, then die. Every later run: die
        // immediately (flag file survives the short-lived child).
        let flag = dir.path().join("ran.flag");
        let script = format!(
            r#"
if [ -f "{}" ]; then exit 9; fi
touch "{}"
id_of() {{ printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }}
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(id_of "$line")
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":"2024-11-05","capabilities":{{}},"serverInfo":{{"name":"fake","version":"1"}}}}}}
' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=$(id_of "$line")
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"tools":[{{"name":"t","inputSchema":{{}}}}]}}}}
' "$id"
      exit 5
      ;;
  esac
done
"#,
            flag.display(),
            flag.display()
        );
        let cmd = write_script(dir.path(), "flaky.sh", &script);
        let cfg = McpLaunchConfig {
            backoff_initial: Duration::from_millis(1),
            max_restarts: 1,
            ..McpLaunchConfig::default()
        };
        let manager = ConnectionManager::start_all(
            vec![server_entry("flaky", cmd.to_str().unwrap(), false)],
            cfg,
            None,
        )
        .await;
        for _ in 0..400 {
            let statuses = manager.status_map().await;
            if statuses["flaky"].state == ServerRuntimeState::Failed {
                assert!(statuses["flaky"]
                    .reason
                    .clone()
                    .unwrap()
                    .contains("gave up"));
                manager.shutdown_all().await;
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("slot 'flaky' never reached Failed");
    }

    #[tokio::test]
    async fn call_while_disconnected_reports_state() {
        let manager = ConnectionManager::start_all(
            vec![server_entry("missing", "/nonexistent/binary", false)],
            McpLaunchConfig::default(),
            None,
        )
        .await;
        let _ = settle(&manager, "missing").await;
        let err = manager
            .call_tool("missing", "t", serde_json::json!({}))
            .await
            .expect_err("must fail");
        match err {
            McpCallError::Disconnected { state, .. } => {
                assert_eq!(state, ServerRuntimeState::Failed)
            }
            other => panic!("expected Disconnected, got {other:?}"),
        }
        manager.shutdown_all().await;
    }
}
