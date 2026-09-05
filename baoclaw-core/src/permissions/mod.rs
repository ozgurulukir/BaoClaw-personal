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
}
