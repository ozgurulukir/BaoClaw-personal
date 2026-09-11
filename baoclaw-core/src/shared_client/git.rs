use std::path::PathBuf;

use baoclaw_core::engine;
use baoclaw_core::ipc;
use baoclaw_core::ipc::protocol::RequestId;

use super::WriterRef;

pub(super) async fn scm_git_status(work_cwd: &PathBuf, writer: WriterRef<'_>, id: RequestId) {
    let mut conn_guard = writer.lock().await;
    match ipc::handlers::git::handle_git_status(std::path::Path::new(work_cwd)) {
        Ok(res) => {
            let _ = conn_guard.send_response(id, res).await;
        }
        Err(err) => {
            let _ = conn_guard.send_error(Some(id), -32000, err).await;
        }
    }
}

pub(super) async fn scm_git_diff(work_cwd: &PathBuf, writer: WriterRef<'_>, id: RequestId) {
    let output = tokio::process::Command::new("git")
        .args(["diff", "--stat"])
        .current_dir(work_cwd)
        .output()
        .await;
    let mut conn_guard = writer.lock().await;
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            let result = if stdout.trim().is_empty() {
                "No uncommitted changes.".to_string()
            } else {
                stdout
            };
            let _ = conn_guard
                .send_response(id, serde_json::json!({"diff": result}))
                .await;
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("git diff failed: {}", stderr))
                .await;
        }
        Err(e) => {
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("Not a git repository or git not available: {}", e),
                )
                .await;
        }
    }
}

pub(super) async fn scm_git_commit(
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    message: String,
) {
    let add_result = tokio::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(work_cwd)
        .output()
        .await;
    let mut conn_guard = writer.lock().await;
    match add_result {
        Ok(o) if o.status.success() => {
            let commit_result = tokio::process::Command::new("git")
                .args(["commit", "-m", &message])
                .current_dir(work_cwd)
                .output()
                .await;
            match commit_result {
                Ok(co) if co.status.success() => {
                    let hash = tokio::process::Command::new("git")
                        .args(["rev-parse", "--short", "HEAD"])
                        .current_dir(work_cwd)
                        .output()
                        .await
                        .ok()
                        .and_then(|h| String::from_utf8(h.stdout).ok())
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    let _ = conn_guard
                        .send_response(id, serde_json::json!({"hash": hash, "message": message}))
                        .await;
                }
                Ok(co) => {
                    let stderr = String::from_utf8_lossy(&co.stderr).to_string();
                    let stdout = String::from_utf8_lossy(&co.stdout).to_string();
                    let msg = if stderr.is_empty() { stdout } else { stderr };
                    let _ = conn_guard
                        .send_error(Some(id), -32000, format!("git commit failed: {}", msg))
                        .await;
                }
                Err(e) => {
                    let _ = conn_guard
                        .send_error(Some(id), -32000, format!("git commit error: {}", e))
                        .await;
                }
            }
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("git add failed: {}", stderr))
                .await;
        }
        Err(e) => {
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("Not a git repository or git not available: {}", e),
                )
                .await;
        }
    }
}

pub(super) async fn scm_git_pr_list(writer: WriterRef<'_>, id: RequestId) {
    let result = match engine::git_integration::pr::PrManager::list_prs(None).await {
        Ok(prs) => {
            let pr_list: Vec<serde_json::Value> = prs
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "number": p.number,
                        "title": p.title,
                        "state": p.state,
                        "author": p.author,
                        "base_branch": p.base_branch,
                        "head_branch": p.head_branch,
                        "created_at": p.created_at,
                        "url": p.url,
                    })
                })
                .collect();
            serde_json::json!({"pull_requests": pr_list, "count": pr_list.len()})
        }
        Err(e) => serde_json::json!({"error": format!("{}", e)}),
    };
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

pub(super) async fn scm_git_pr_create(
    writer: WriterRef<'_>,
    id: RequestId,
    title: String,
    body: String,
    base: String,
) {
    let body_opt = if body.is_empty() {
        None
    } else {
        Some(body.as_str())
    };
    let base_opt = if base.is_empty() {
        None
    } else {
        Some(base.as_str())
    };
    let result =
        match engine::git_integration::pr::PrManager::create_pr(&title, body_opt, base_opt).await {
            Ok(pr) => serde_json::json!({
                "success": true,
                "number": pr.number,
                "title": pr.title,
                "url": pr.url,
            }),
            Err(e) => {
                serde_json::json!({"success": false, "error": format!("{}", e)})
            }
        };
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

pub(super) async fn scm_git_branch_list(writer: WriterRef<'_>, id: RequestId) {
    let result = match engine::git_integration::branch::BranchManager::list_branches().await {
        Ok(branches) => {
            let branch_list: Vec<serde_json::Value> = branches
                .iter()
                .map(|b| {
                    serde_json::json!({
                        "name": b.name,
                        "is_current": b.is_current,
                        "ahead": b.ahead,
                        "behind": b.behind,
                        "last_commit": b.last_commit,
                        "last_commit_msg": b.last_commit_msg,
                    })
                })
                .collect();
            serde_json::json!({"branches": branch_list, "count": branch_list.len()})
        }
        Err(e) => serde_json::json!({"error": format!("{}", e)}),
    };
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

pub(super) async fn scm_git_conflict_check(writer: WriterRef<'_>, id: RequestId) {
    let result = match engine::git_integration::conflict::ConflictResolver::detect_conflicts().await
    {
        Ok(conflicts) => {
            let conflict_list: Vec<serde_json::Value> = conflicts
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "file": c.file,
                        "resolved": c.resolved,
                    })
                })
                .collect();
            serde_json::json!({"conflicts": conflict_list, "count": conflict_list.len(), "has_conflicts": !conflicts.is_empty()})
        }
        Err(e) => serde_json::json!({"error": format!("{}", e)}),
    };
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}
