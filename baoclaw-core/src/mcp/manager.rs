//! Connection manager: one supervised slot per configured MCP server.
//!
//! Lifecycle contract: the tool catalog is LIVE. Connect attempts count
//! toward `max_restarts`; the budget exhausted, a slot parks as Failed (no
//! automatic retries) but a manual `/mcp refresh` revives it immediately
//! with a fresh budget. Every Ready transition — and every
//! `notifications/tools/list_changed` — re-fetches the catalog and
//! republishes the slot's bridge bucket into the engine ToolRegistry, so
//! engines observe changes at their next snapshot. While disconnected, a
//! slot's registered tools return error results instead of disappearing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{watch, RwLock};

use crate::config::BaoclawConfig;
use crate::discovery::mcp_config::McpServerInfo;
use crate::tools::Tool;

use super::bridge::McpToolBridge;
use super::client::{McpClient, McpError};
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
    /// Register bridges as deferred (stub-schema) tools.
    pub deferred_tools: bool,
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
            deferred_tools: config.mcp_deferred_tools,
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
    client: Option<Arc<McpClient>>,
}

struct ServerSlot {
    spec: McpServerInfo,
    cfg: McpLaunchConfig,
    /// std Mutex: tiny critical sections, never held across an await.
    inner: std::sync::Mutex<SlotInner>,
    /// Set once the FIRST connection attempt settles (Ready or Failed);
    /// boot registration awaits it.
    boot_settled: watch::Receiver<bool>,
    /// Manual wake sources (`mcpRefresh`): demand a catalog refresh while
    /// Ready, or an immediate reconnect while Disconnected/Failed. Values
    /// are generation counters; consumers compare against the last one seen.
    refresh_kick_tx: watch::Sender<u64>,
    refresh_kick_rx: watch::Receiver<u64>,
    reconnect_kick_tx: watch::Sender<u64>,
    reconnect_kick_rx: watch::Receiver<u64>,
}

pub struct ConnectionManager {
    slots: RwLock<HashMap<String, Arc<ServerSlot>>>,
    /// Reset on every Ready transition so transport-failure records
    /// accumulated while a server was down never disable its registered
    /// tools after it comes back.
    tool_health: Option<crate::engine::tool_health::ToolHealthHandle>,
    /// Attached after construction (the registry is built over the engine's
    /// builtin head); once set, slot catalogs publish into it.
    registry: std::sync::Mutex<Option<crate::tools::registry::ToolRegistryHandle>>,
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
    /// Abort was requested; the server was told notifications/cancelled.
    Cancelled,
}

impl ConnectionManager {
    /// Empty manager: for tests of components that only need the handle
    /// (their lookups miss and surface as "unknown server").
    #[cfg(test)]
    pub(crate) fn empty() -> Arc<Self> {
        Arc::new(Self {
            slots: RwLock::new(HashMap::new()),
            tool_health: None,
            registry: std::sync::Mutex::new(None),
        })
    }

    /// Attach the engine's live tool registry. Slot catalogs publish into it
    /// from this point (boot registration and every later refresh).
    pub fn attach_registry(&self, registry: crate::tools::registry::ToolRegistryHandle) {
        *self.registry.lock().unwrap() = Some(registry);
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
            registry: std::sync::Mutex::new(None),
        });
        {
            let mut slots = manager.slots.write().await;
            for spec in servers {
                let skip_reason = if spec.disabled {
                    Some("disabled".to_string())
                } else if spec.server_type == "stdio" {
                    if spec.command.as_ref().map(String::is_empty).unwrap_or(true) {
                        Some("stdio server missing command".to_string())
                    } else {
                        None
                    }
                } else if matches!(spec.server_type.as_str(), "http" | "sse") {
                    let ok_url = spec
                        .url
                        .as_deref()
                        .map(|u| u.starts_with("http://") || u.starts_with("https://"))
                        .unwrap_or(false);
                    if ok_url {
                        None
                    } else {
                        Some(format!("{} server missing http(s) url", spec.server_type))
                    }
                } else {
                    Some(format!("transport '{}' not supported", spec.server_type))
                };

                let (boot_tx, boot_rx) = watch::channel(false);
                let (refresh_kick_tx, refresh_kick_rx) = watch::channel(0u64);
                let (reconnect_kick_tx, reconnect_kick_rx) = watch::channel(0u64);
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
                    refresh_kick_tx,
                    refresh_kick_rx,
                    reconnect_kick_tx,
                    reconnect_kick_rx,
                });
                if slot.inner.lock().unwrap().state == ServerRuntimeState::Connecting {
                    let tracker = manager.tool_health.clone();
                    tokio::spawn(supervise(
                        Arc::clone(&manager),
                        Arc::clone(&slot),
                        boot_tx,
                        tracker,
                    ));
                } else {
                    let _ = boot_tx.send(true);
                }
                slots.insert(slot.spec.name.clone(), slot);
            }
        }
        manager
    }

    /// Eager boot registration: await every slot's first connection attempt,
    /// then publish one bridge bucket per slot that fetched a catalog. Slots
    /// that failed their first attempt publish nothing yet — with the live
    /// registry a later reconnect or `refresh()` CAN add their tools.
    /// Returns the number of tools published.
    pub async fn boot_and_register(self: &Arc<Self>) -> usize {
        let mut slots: Vec<Arc<ServerSlot>> = self.slots.read().await.values().cloned().collect();
        // Deterministic publish order.
        slots.sort_by(|a, b| a.spec.name.cmp(&b.spec.name));
        for slot in &slots {
            let _ = slot.boot_settled.clone().wait_for(|v| *v).await;
        }

        let registry = self.registry.lock().unwrap().clone();
        let Some(registry) = registry else {
            eprintln!("[mcp] WARNING: no registry attached at boot; MCP tools not registered");
            return 0;
        };

        let mut published = 0usize;
        // Dispatch is case-insensitive, so collision detection must be too.
        let mut seen = std::collections::HashSet::new();
        for slot in &slots {
            let g = slot.inner.lock().unwrap();
            // A slot that reached Ready and crashed in the microseconds
            // before this read still fetched its catalog — the live-catalog
            // contract says its tools must be registered.
            if g.state != ServerRuntimeState::Ready && g.tool_defs.is_empty() {
                continue;
            }
            let mut bridges: Vec<Arc<dyn Tool>> = Vec::new();
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
                    slot.cfg.deferred_tools,
                )));
            }
            published += bridges.len();
            registry.replace_server_tools(&slot.spec.name, bridges);
        }
        published
    }

    /// Republish one server's current catalog as bridges into the registry.
    /// Called on Ready transitions and `tools/list_changed` refreshes.
    fn republish_server(self: &Arc<Self>, slot: &ServerSlot) {
        let registry = self.registry.lock().unwrap().clone();
        let Some(registry) = registry else { return };
        let g = slot.inner.lock().unwrap();
        let mut bridges: Vec<Arc<dyn Tool>> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for def in &g.tool_defs {
            let registry_name = composite_tool_name(&slot.spec.name, &def.name);
            if !seen.insert(registry_name.to_lowercase()) {
                continue;
            }
            bridges.push(Arc::new(McpToolBridge::new(
                Arc::clone(self),
                slot.spec.name.clone(),
                def.clone(),
                registry_name,
                slot.cfg.deferred_tools,
            )));
        }
        registry.replace_server_tools(&slot.spec.name, bridges);
    }

    /// Forward a tools/call to the named server. Called only by
    /// [`McpToolBridge`].
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<CallToolOutcome, McpCallError> {
        self.call_tool_cancellable(server, tool, arguments, None)
            .await
    }

    /// [`Self::call_tool`] honoring an abort signal: on abort the server is
    /// told `notifications/cancelled {requestId}` and the call fails with
    /// [`McpCallError::Cancelled`]. If the response races the abort and
    /// wins, the result is returned normally.
    pub async fn call_tool_cancellable(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
        abort: Option<tokio::sync::watch::Receiver<bool>>,
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

        match client
            .call_tool_cancellable(tool, arguments, call_timeout, abort)
            .await
        {
            Ok(outcome) => Ok(outcome),
            Err(McpError::Remote { code, message }) => Err(McpCallError::Remote { code, message }),
            Err(McpError::Timeout(ms)) => Err(McpCallError::Timeout(ms)),
            Err(McpError::Cancelled) => Err(McpCallError::Cancelled),
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

    /// Manual refresh (`mcpRefresh` RPC): Ready slots re-fetch their
    /// catalog, Disconnected/Failed slots retry immediately (a Failed slot's
    /// restart budget resets). Connecting/Skipped/unknown are no-ops.
    /// Returns the status snapshot immediately — settling is asynchronous;
    /// clients re-list to observe the result.
    pub async fn refresh(&self, server: Option<&str>) -> HashMap<String, ServerStatus> {
        let slots: Vec<Arc<ServerSlot>> = self
            .slots
            .read()
            .await
            .iter()
            .filter(|(name, _)| server.is_none_or(|t| name == &t))
            .map(|(_, slot)| Arc::clone(slot))
            .collect();
        for slot in slots {
            let state = slot.inner.lock().unwrap().state.clone();
            match state {
                ServerRuntimeState::Ready => {
                    slot.refresh_kick_tx.send_modify(|v| *v += 1);
                }
                ServerRuntimeState::Disconnected | ServerRuntimeState::Failed => {
                    slot.reconnect_kick_tx.send_modify(|v| *v += 1);
                }
                _ => {}
            }
        }
        self.status_map().await
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

/// Per-server supervisor: connect → Ready (serving `tools/call`, watching
/// for process death, `notifications/tools/list_changed`, and manual
/// refresh kicks) → on death, bounded backoff reconnect → after the restart
/// budget, Failed parks awaiting a manual kick (a kick restarts the budget).
/// With the live registry, every reconnect or refresh that fetches a catalog
/// publishes it — tools can appear at any point in the state machine.
async fn supervise(
    manager: Arc<ConnectionManager>,
    slot: Arc<ServerSlot>,
    boot_tx: watch::Sender<bool>,
    tool_health: Option<crate::engine::tool_health::ToolHealthHandle>,
) {
    let cfg = slot.cfg.clone();
    let mut backoff = cfg.backoff_initial;
    let backoff_max = Duration::from_millis(MCP_BACKOFF_MAX_MS);
    let mut refresh_rx = slot.refresh_kick_rx.clone();
    let mut reconnect_rx = slot.reconnect_kick_rx.clone();
    let mut last_refresh_kick = *refresh_rx.borrow();
    let mut last_reconnect_kick = *reconnect_rx.borrow();
    // Set when a manual reconnect kick dropped the live connection: the next
    // connect happens immediately and does NOT burn restart budget.
    let mut manual_reconnect = false;

    loop {
        match McpClient::connect(&slot.spec, cfg.startup_timeout).await {
            Ok((client, tool_defs)) => {
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
                manager.republish_server(&slot);
                let _ = boot_tx.send(true);

                // Ready episode: serve until the process dies, the server
                // announces a catalog change, or a manual kick arrives.
                let mut list_rx = client.list_changed();
                loop {
                    tokio::select! {
                        _ = client.wait_closed() => {
                            let mut g = slot.inner.lock().unwrap();
                            g.client = None;
                            if !manual_reconnect {
                                g.restarts += 1;
                                g.reason = Some("server process exited".to_string());
                                g.state = ServerRuntimeState::Disconnected;
                            }
                            break;
                        }
                        _ = list_rx.changed() => {
                            refresh_catalog(&manager, &slot, &client, &tool_health).await;
                        }
                        _ = refresh_rx.changed() => {
                            let v = *refresh_rx.borrow();
                            if v != last_refresh_kick {
                                last_refresh_kick = v;
                                refresh_catalog(&manager, &slot, &client, &tool_health).await;
                            }
                        }
                        _ = reconnect_rx.changed() => {
                            let v = *reconnect_rx.borrow();
                            if v != last_reconnect_kick {
                                last_reconnect_kick = v;
                                // Operator-initiated: drop the live connection
                                // and reconnect at once, without charging the
                                // restart budget.
                                manual_reconnect = true;
                                client.shutdown().await;
                                break;
                            }
                        }
                    }
                }
                if manual_reconnect {
                    manual_reconnect = false;
                    backoff = cfg.backoff_initial;
                    continue;
                }
            }
            Err(e) => {
                let park = {
                    let mut g = slot.inner.lock().unwrap();
                    g.reason = Some(e.to_string());
                    g.client = None;
                    g.restarts += 1;
                    if g.restarts > cfg.max_restarts {
                        g.state = ServerRuntimeState::Failed;
                        g.reason = Some(format!(
                            "gave up after {} connect attempts; /mcp refresh retries",
                            cfg.max_restarts
                        ));
                        true
                    } else {
                        g.state = ServerRuntimeState::Connecting;
                        false
                    }
                };
                let _ = boot_tx.send(true);
                if park {
                    // Park: no automatic retries (fail-storm protection).
                    // A manual kick restarts the budget and retries now.
                    let _ = reconnect_rx.wait_for(|v| *v != last_reconnect_kick).await;
                    last_reconnect_kick = *reconnect_rx.borrow();
                    {
                        let mut g = slot.inner.lock().unwrap();
                        g.restarts = 0;
                        g.state = ServerRuntimeState::Connecting;
                        g.reason = Some("manual refresh".to_string());
                    }
                    backoff = cfg.backoff_initial;
                    continue;
                }
            }
        }

        // Between-attempt backoff; a manual reconnect kick short-circuits it.
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {
                backoff = (backoff * 2).min(backoff_max);
            }
            _ = reconnect_rx.changed() => {
                let v = *reconnect_rx.borrow();
                if v != last_reconnect_kick {
                    last_reconnect_kick = v;
                }
            }
        }
    }
}

/// Re-fetch the catalog (bounded) and publish it if changed. A failed fetch
/// keeps the previous catalog: the model keeps seeing the last-known tools,
/// and the state machine handles connection death separately.
async fn refresh_catalog(
    manager: &Arc<ConnectionManager>,
    slot: &ServerSlot,
    client: &Arc<McpClient>,
    tool_health: &Option<crate::engine::tool_health::ToolHealthHandle>,
) {
    // One overall budget for the whole fetch: per-page budgets would let a
    // maliciously paging server stretch a refresh indefinitely.
    let defs = match tokio::time::timeout(
        slot.cfg.call_timeout,
        client.list_tools(slot.cfg.call_timeout),
    )
    .await
    {
        Ok(Ok(defs)) => defs,
        Ok(Err(e)) => {
            eprintln!("[mcp:{}] catalog refresh failed: {e}", slot.spec.name);
            return;
        }
        Err(_) => {
            eprintln!(
                "[mcp:{}] catalog refresh timed out ({}ms)",
                slot.spec.name,
                slot.cfg.call_timeout.as_millis()
            );
            return;
        }
    };
    {
        let mut g = slot.inner.lock().unwrap();
        if g.tool_defs == defs {
            return; // no-op refresh: nothing to publish
        }
        g.tool_defs = defs;
        if let Some(th) = tool_health {
            for def in &g.tool_defs {
                th.reset_tool(&composite_tool_name(&slot.spec.name, &def.name));
            }
        }
    }
    manager.republish_server(slot);
    eprintln!(
        "[mcp:{}] catalog refreshed ({} tools)",
        slot.spec.name,
        slot.inner.lock().unwrap().tool_defs.len()
    );
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
            headers: HashMap::new(),
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
                    url: None,
                    command: None,
                    env: HashMap::new(),
                    headers: HashMap::new(),
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
            .contains("missing http(s) url"));
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
            McpLaunchConfig {
                max_restarts: 0,
                ..McpLaunchConfig::default()
            },
            None,
        )
        .await;
        let st = settle(&manager, "broken").await;
        assert_eq!(st.state, ServerRuntimeState::Failed);
        assert_eq!(manager.boot_and_register().await, 0);
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

        manager.attach_registry(crate::tools::registry::ToolRegistry::new(vec![]));
        let published = manager.boot_and_register().await;
        assert_eq!(published, 2);
        // Bridges are registered under sorted server buckets in the registry.
        let names: Vec<String> = manager
            .registry
            .lock()
            .unwrap()
            .clone()
            .expect("registry attached")
            .snapshot()
            .iter()
            .map(|t| t.name().to_string())
            .collect();
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
            McpLaunchConfig {
                max_restarts: 0,
                ..McpLaunchConfig::default()
            },
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
