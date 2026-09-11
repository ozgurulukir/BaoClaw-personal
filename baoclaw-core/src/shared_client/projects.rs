use std::path::PathBuf;
use std::sync::Arc;

use baoclaw_core::engine::shared_session::{ClientId, SharedSession};
use baoclaw_core::ipc::protocol::RequestId;

use super::WriterRef;
use crate::{cwd_hash, switch_shared_client, SharedState};

pub(super) async fn scm_projects_list(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let projects = shared.project_registry.list().await;
    // Enrich each project with its session_id (derived from cwd hash)
    let enriched: Vec<serde_json::Value> = projects
        .iter()
        .map(|p| {
            let session_key = cwd_hash(&p.cwd);
            let mut v = serde_json::to_value(p).unwrap_or_default();
            v["session_id"] = serde_json::json!(session_key);
            v
        })
        .collect();
    let count = enriched.len();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"projects": enriched, "count": count}),
        )
        .await;
}

pub(super) async fn scm_projects_switch(
    shared: &SharedState,
    writer: WriterRef<'_>,
    session: &mut Arc<SharedSession>,
    client_id: &mut ClientId,
    broadcast_handle: &mut tokio::task::JoinHandle<()>,
    session_id: &mut String,
    work_cwd: &mut PathBuf,
    id: RequestId,
    id_prefix: String,
) {
    let mut conn_guard = writer.lock().await;
    match shared.project_registry.find_by_prefix(&id_prefix).await {
        Ok(project) => {
            let abs_cwd = std::path::PathBuf::from(&project.cwd);
            if !abs_cwd.is_dir() {
                let _ = conn_guard
                    .send_error(
                        Some(id),
                        -32000,
                        format!("Directory does not exist: {}", project.cwd),
                    )
                    .await;
            } else {
                drop(conn_guard);
                match switch_shared_client(
                    shared,
                    writer,
                    session,
                    client_id,
                    broadcast_handle,
                    session_id,
                    work_cwd,
                    abs_cwd.clone(),
                )
                .await
                {
                    Ok(message_count) => {
                        shared.project_registry.touch(&project.cwd).await;
                        let mut conn_guard = writer.lock().await;
                        let _ = conn_guard
                            .send_response(
                                id,
                                serde_json::json!({
                                    "project": project,
                                    "message_count": message_count,
                                    "session_id": session_id,
                                }),
                            )
                            .await;
                    }
                    Err(error) => {
                        let mut conn_guard = writer.lock().await;
                        let _ = conn_guard.send_error(Some(id), -32003, error).await;
                    }
                }
            }
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32000, e).await;
        }
    }
}

pub(super) async fn scm_projects_new(
    shared: &SharedState,
    writer: WriterRef<'_>,
    session: &mut Arc<SharedSession>,
    client_id: &mut ClientId,
    broadcast_handle: &mut tokio::task::JoinHandle<()>,
    session_id: &mut String,
    work_cwd: &mut PathBuf,
    id: RequestId,
    cwd: String,
    description: Option<String>,
) {
    let expanded = if cwd.starts_with('~') {
        let home = std::env::var("HOME").unwrap_or_default();
        cwd.replacen('~', &home, 1)
    } else if std::path::Path::new(&cwd).is_relative() {
        work_cwd.join(&cwd).to_string_lossy().to_string()
    } else {
        cwd.clone()
    };
    let abs_path = std::path::PathBuf::from(&expanded);
    if !abs_path.is_dir() {
        let mut conn_guard = writer.lock().await;
        let _ = conn_guard
            .send_error(
                Some(id),
                -32000,
                format!("Directory does not exist: {}", expanded),
            )
            .await;
    } else {
        let desc = description.unwrap_or_else(|| {
            abs_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| expanded.clone())
        });
        let mut conn_guard = writer.lock().await;
        match shared
            .project_registry
            .register(expanded.clone(), desc)
            .await
        {
            Ok(project) => {
                // Auto-scaffold
                let baoclaw_dir = abs_path.join(".baoclaw");
                if !baoclaw_dir.exists() {
                    if let Err(e) = std::fs::create_dir_all(&baoclaw_dir) {
                        eprintln!(
                            "[projects] WARNING: could not create {}: {}",
                            baoclaw_dir.display(),
                            e
                        );
                    }
                    if let Err(e) =
                        std::fs::write(baoclaw_dir.join("BAOCLAW.md"), "# Project Instructions\n\n")
                    {
                        eprintln!("[projects] WARNING: could not write BAOCLAW.md: {}", e);
                    }
                    if let Err(e) =
                        std::fs::write(baoclaw_dir.join("mcp.json"), "{\"mcpServers\":{}}\n")
                    {
                        eprintln!("[projects] WARNING: could not write mcp.json: {}", e);
                    }
                    if let Err(e) = std::fs::create_dir_all(baoclaw_dir.join("skills")) {
                        eprintln!("[projects] WARNING: could not create skills dir: {}", e);
                    }
                }
                drop(conn_guard);
                match switch_shared_client(
                    shared,
                    writer,
                    session,
                    client_id,
                    broadcast_handle,
                    session_id,
                    work_cwd,
                    abs_path,
                )
                .await
                {
                    Ok(message_count) => {
                        let mut conn_guard = writer.lock().await;
                        let _ = conn_guard
                            .send_response(
                                id,
                                serde_json::json!({
                                    "project": project,
                                    "switched": true,
                                    "message_count": message_count,
                                    "session_id": session_id,
                                }),
                            )
                            .await;
                    }
                    Err(error) => {
                        let mut conn_guard = writer.lock().await;
                        let _ = conn_guard.send_error(Some(id), -32003, error).await;
                    }
                }
            }
            Err(e) => {
                let _ = conn_guard.send_error(Some(id), -32000, e).await;
            }
        }
    }
}

pub(super) async fn scm_projects_update_desc(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    id_prefix: String,
    description: String,
) {
    let mut conn_guard = writer.lock().await;
    match shared
        .project_registry
        .update_description(&id_prefix, description)
        .await
    {
        Ok(()) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"updated": true}))
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32000, e).await;
        }
    }
}
