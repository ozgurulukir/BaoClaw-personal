use std::path::PathBuf;

use baoclaw_core::ipc::protocol::RequestId;
use baoclaw_core::{discovery, doc_upload, engine};

use super::WriterRef;
use crate::SharedState;

pub(super) async fn scm_list_tools(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let tl: Vec<serde_json::Value> = shared
        .tool_registry
        .snapshot()
        .iter()
        .map(|t| {
            let ty = if t.name().starts_with(baoclaw_core::mcp::MCP_TOOL_PREFIX) {
                "mcp"
            } else {
                "builtin"
            };
            serde_json::json!({
                "name": t.name(),
                "description": t.prompt(),
                "type": ty,
                "deferred": t.is_deferred(),
            })
        })
        .collect();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"tools": tl, "count": tl.len()}))
        .await;
}

pub(super) async fn scm_list_skills(work_cwd: &PathBuf, writer: WriterRef<'_>, id: RequestId) {
    let s = discovery::skills::discover_skills(work_cwd).await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"skills": s, "count": s.len()}))
        .await;
}

pub(super) async fn scm_list_plugins(work_cwd: &PathBuf, writer: WriterRef<'_>, id: RequestId) {
    let p = discovery::plugins::discover_plugins(work_cwd).await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"plugins": p, "count": p.len()}))
        .await;
}

pub(super) async fn scm_doc_upload(writer: WriterRef<'_>, id: RequestId, file_path: String) {
    let path = std::path::Path::new(&file_path);
    let mut conn_guard = writer.lock().await;
    match doc_upload::build_attachment_from_file(path) {
        Ok(attachment) => {
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "attachment": attachment,
                        "file_path": file_path,
                    }),
                )
                .await;
        }
        Err(e) => {
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Document upload failed: {}", e))
                .await;
        }
    }
}

pub(super) async fn scm_tool_health(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    use engine::tool_health::ToolStatus;

    let snap = shared.tool_health.snapshot();
    let total_calls: u64 = snap.tools.iter().map(|r| r.total_calls).sum();
    let count_status =
        |status: ToolStatus| snap.tools.iter().filter(|r| r.status == status).count();
    let result = serde_json::json!({
        "summary": {
            "tracked": snap.tools.len(),
            "healthy": count_status(ToolStatus::Healthy),
            "degraded": count_status(ToolStatus::Degraded),
            "disabled": count_status(ToolStatus::Disabled),
            "total_calls": total_calls,
        },
        "thresholds": {
            "degrade": snap.degrade_threshold,
            "disable": snap.disable_threshold,
            "recovery_minutes": snap.recovery_minutes,
        },
        "tools": snap.tools,
    });
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}
