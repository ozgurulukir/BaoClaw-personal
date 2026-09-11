use baoclaw_core::ipc::protocol::RequestId;

use super::WriterRef;
use crate::SharedState;

pub(super) async fn scm_cron_add(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    name: String,
    prompt: String,
    schedule: String,
    cwd: Option<String>,
) {
    let mut conn_guard = writer.lock().await;
    match shared
        .cron_manager
        .add_job(name, prompt, schedule, cwd)
        .await
    {
        Ok(job) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"job": job}))
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32000, e).await;
        }
    }
}

pub(super) async fn scm_cron_remove(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    job_id: String,
) {
    let removed = shared.cron_manager.remove_job(&job_id).await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"removed": removed}))
        .await;
}

pub(super) async fn scm_cron_toggle(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    job_id: String,
) {
    let mut conn_guard = writer.lock().await;
    match shared.cron_manager.toggle_job(&job_id).await {
        Some(enabled) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"enabled": enabled}))
                .await;
        }
        None => {
            let _ = conn_guard
                .send_error(Some(id), -32000, "Job not found".to_string())
                .await;
        }
    }
}

pub(super) async fn scm_cron_list(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let jobs = shared.cron_manager.list_jobs().await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"jobs": jobs, "count": jobs.len()}))
        .await;
}
