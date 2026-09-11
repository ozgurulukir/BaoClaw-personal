use std::path::PathBuf;

use baoclaw_core::discovery;
use baoclaw_core::ipc::protocol::RequestId;

use super::WriterRef;
use crate::SharedState;

pub(super) async fn scm_list_mcp_servers(
    shared: &SharedState,
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
) {
    let s = discovery::mcp_config::discover_mcp_servers(work_cwd).await;
    let statuses = shared.mcp_manager.status_map().await;
    let servers: Vec<serde_json::Value> = s
        .iter()
        .map(|info| {
            // McpServerInfo serializes with `env` skipped, so secrets stay
            // out of the response; only the runtime status is added here.
            let mut v = serde_json::to_value(info).unwrap_or(serde_json::Value::Null);
            let runtime = match statuses.get(&info.name) {
                Some(st) => serde_json::to_value(st).unwrap_or(serde_json::Value::Null),
                None => serde_json::json!({
                    "state": if shared.baoclaw_config.mcp_enabled { "requires_restart" } else { "disabled_by_config" }
                }),
            };
            if let Some(obj) = v.as_object_mut() {
                obj.insert("runtime".to_string(), runtime);
            }
            v
        })
        .collect();
    let count = servers.len();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"servers": servers, "count": count}))
        .await;
}

pub(super) async fn scm_mcp_refresh(
    shared: &SharedState,
    server: Option<String>,
    writer: WriterRef<'_>,
    id: RequestId,
) {
    let statuses = shared.mcp_manager.refresh(server.as_deref()).await;
    let servers: Vec<serde_json::Value> = statuses
        .iter()
        .map(|(name, st)| {
            serde_json::json!({"name": name, "runtime": serde_json::to_value(st).unwrap_or(serde_json::Value::Null)})
        })
        .collect();
    let count = servers.len();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"servers": servers, "count": count}))
        .await;
}
