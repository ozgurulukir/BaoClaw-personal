// Permission system - tool execution permission management

pub mod gate;
pub mod manager;

use std::sync::Arc;

use manager::PermissionManager;

/// Manager + gate pair handed to a query engine so its tools can prompt the
/// user interactively. Cloned per turn into the query loop; the gate shares
/// its pending-request map across clones, so a `permissionResponse` arriving
/// on any daemon connection resolves prompts from every session.
/// Live allow-list of extra directories the Glob/Grep search tools may read
/// beyond the project cwd. Seeded at startup from
/// `ToolPermissionContext::additional_search_dirs` (config
/// `extra["permissions"]`), grown by interactive "Always allow" grants in the
/// executor, and read by the tools themselves on every call — so grants apply
/// to later calls without a restart.
pub type GrantedSearchDirs = Arc<std::sync::RwLock<Vec<std::path::PathBuf>>>;

/// Live allow-list of extra directories the FileWrite/FileEdit tools may
/// write beyond the project cwd — the write twin of [`GrantedSearchDirs`],
/// kept as a DISTINCT list so a read grant can never become a write grant.
/// Seeded at startup from `ToolPermissionContext::additional_write_dirs`
/// (config `extra["permissions"]`) plus the default `~/.baoclaw` parity,
/// grown by interactive "Always allow" grants in the executor, and read by
/// the tools themselves on every call. Whole-tool allow rules never extend
/// this list — only interactive directory grants do.
pub type GrantedWriteDirs = Arc<std::sync::RwLock<Vec<std::path::PathBuf>>>;

/// The prompt timeout and grant-persistence settings are NOT carried here —
/// they live on `ToolPermissionContext` (config `extra["permissions"]`) and
/// are read live from the shared manager at each prompt, so config changes
/// apply without an engine restart.
#[derive(Clone)]
pub struct PermissionBridge {
    pub manager: Arc<tokio::sync::RwLock<PermissionManager>>,
    pub gate: gate::PermissionGate,
    /// Out-of-cwd directories Glob/Grep are allowed to search (see
    /// [`GrantedSearchDirs`]).
    pub granted_dirs: GrantedSearchDirs,
    /// Out-of-cwd directories FileWrite/FileEdit are allowed to write (see
    /// [`GrantedWriteDirs`]).
    pub granted_write_dirs: GrantedWriteDirs,
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
