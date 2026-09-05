// Permission system - tool execution permission management

pub mod gate;
pub mod manager;

use std::sync::Arc;
use std::time::Duration;

use manager::PermissionManager;

/// How long a tool waits for a user decision before auto-denying.
pub const DEFAULT_ASK_TIMEOUT: Duration = Duration::from_secs(300);

/// Manager + gate pair handed to a query engine so its tools can prompt the
/// user interactively. Cloned per turn into the query loop; the gate shares
/// its pending-request map across clones, so a `permissionResponse` arriving
/// on any daemon connection resolves prompts from every session.
#[derive(Clone)]
pub struct PermissionBridge {
    pub manager: Arc<tokio::sync::RwLock<PermissionManager>>,
    pub gate: gate::PermissionGate,
    pub ask_timeout: Duration,
    /// When true, allow-always grants are written back to config.json so
    /// they survive a daemon restart. Interactive daemons set this; tests
    /// and headless engines keep it false to avoid touching the real file.
    pub persist_grants: bool,
}

/// Write the current permission rules to ~/.baoclaw/config.json
/// (`extra["permissions"]`), preserving every other config field. Mirrors
/// the idiom of the permission.* RPC handlers.
pub fn persist_context_to_config(ctx: &manager::ToolPermissionContext) {
    let ctx_json = match serde_json::to_value(ctx) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "[permissions] WARN: could not serialize permission context: {}",
                e
            );
            return;
        }
    };
    let mut cfg = crate::config::load_config();
    cfg.extra.insert("permissions".to_string(), ctx_json);
    if let Err(e) = cfg.save() {
        eprintln!(
            "[permissions] WARN: could not persist allow-always rule to config: {}",
            e
        );
    }
}
