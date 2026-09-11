use std::path::PathBuf;

use baoclaw_core::engine;
use baoclaw_core::ipc::protocol::RequestId;

use super::{ScmFlow, WriterRef};

pub(super) async fn scm_spec_new(
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    feature_name: String,
    workflow: Option<String>,
    spec_type: Option<String>,
) {
    let spec_engine = engine::spec_engine::SpecEngine::new(work_cwd.clone());
    let wf = match workflow.as_deref() {
        Some("design") => engine::spec_engine::SpecWorkflow::DesignFirst,
        _ => engine::spec_engine::SpecWorkflow::RequirementsFirst,
    };
    let st = match spec_type.as_deref() {
        Some("bugfix") => engine::spec_engine::SpecType::Bugfix,
        _ => engine::spec_engine::SpecType::Feature,
    };
    let mut conn_guard = writer.lock().await;
    match spec_engine.create_spec(&feature_name, wf, st) {
        Ok(config) => {
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "status": "created",
                        "feature_name": feature_name,
                        "config": serde_json::to_value(&config).unwrap_or_default()
                    }),
                )
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32001, e.to_string()).await;
        }
    }
}

pub(super) async fn scm_spec_list(work_cwd: &PathBuf, writer: WriterRef<'_>, id: RequestId) {
    let spec_engine = engine::spec_engine::SpecEngine::new(work_cwd.clone());
    let mut conn_guard = writer.lock().await;
    match spec_engine.list_specs() {
        Ok(specs) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"specs": specs}))
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32000, e.to_string()).await;
        }
    }
}

pub(super) async fn scm_spec_show(
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    feature_name: String,
) {
    let spec_engine = engine::spec_engine::SpecEngine::new(work_cwd.clone());
    let mut conn_guard = writer.lock().await;
    match spec_engine.get_spec(&feature_name) {
        Ok(summary) => {
            let _ = conn_guard
                .send_response(id, serde_json::to_value(&summary).unwrap_or_default())
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32001, e.to_string()).await;
        }
    }
}

pub(super) async fn scm_spec_status(
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    feature_name: String,
) {
    let spec_engine = engine::spec_engine::SpecEngine::new(work_cwd.clone());
    let mut conn_guard = writer.lock().await;
    match spec_engine.get_status(&feature_name) {
        Ok(progress) => {
            let _ = conn_guard
                .send_response(id, serde_json::to_value(&progress).unwrap_or_default())
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32001, e.to_string()).await;
        }
    }
}

pub(super) async fn scm_spec_run(
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    feature_name: String,
    task_id: Option<String>,
) -> ScmFlow {
    let spec_engine = engine::spec_engine::SpecEngine::new(work_cwd.clone());
    let mut conn_guard = writer.lock().await;
    let task = if let Some(_tid) = &task_id {
        // Find specific task
        match spec_engine.next_task(&feature_name) {
            Ok(t) => t,
            Err(e) => {
                let _ = conn_guard.send_error(Some(id), -32001, e.to_string()).await;
                return ScmFlow::Continue;
            }
        }
    } else {
        match spec_engine.next_task(&feature_name) {
            Ok(t) => t,
            Err(e) => {
                let _ = conn_guard.send_error(Some(id), -32001, e.to_string()).await;
                return ScmFlow::Continue;
            }
        }
    };
    match task {
        Some(t) => {
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "status": "ready",
                        "task_id": t.id,
                        "task_description": t.description,
                    }),
                )
                .await;
        }
        None => {
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "status": "all_complete",
                        "message": "All tasks are completed"
                    }),
                )
                .await;
        }
    }
    ScmFlow::Continue
}

pub(super) async fn scm_spec_edit(
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    feature_name: String,
    phase: String,
) {
    let spec_engine = engine::spec_engine::SpecEngine::new(work_cwd.clone());
    let mut conn_guard = writer.lock().await;
    match spec_engine.read_phase_doc(&feature_name, &phase) {
        Ok(content) => {
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "feature_name": feature_name,
                        "phase": phase,
                        "content": content,
                    }),
                )
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32001, e.to_string()).await;
        }
    }
}
