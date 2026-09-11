use std::path::PathBuf;

use baoclaw_core::ipc::protocol::RequestId;

use super::WriterRef;
use crate::SharedState;

pub(super) async fn scm_task_create(
    shared: &SharedState,
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    description: String,
    prompt: String,
) {
    let task_id = shared
        .task_manager
        .create_task(
            description,
            prompt,
            std::path::PathBuf::from(work_cwd),
            shared.state_manager.get().model,
        )
        .await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"task_id": task_id}))
        .await;
}

pub(super) async fn scm_task_list(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let tasks = shared.task_manager.list_tasks().await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"tasks": tasks, "count": tasks.len()}),
        )
        .await;
}

pub(super) async fn scm_task_status(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    task_id: String,
) {
    match shared.task_manager.get_task_status(&task_id).await {
        Some(task) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard.send_response(id, serde_json::json!(task)).await;
        }
        None => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Task not found: {}", task_id))
                .await;
        }
    }
}

pub(super) async fn scm_task_stop(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    task_id: String,
) {
    let stopped = shared.task_manager.stop_task(&task_id).await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"stopped": stopped}))
        .await;
}
