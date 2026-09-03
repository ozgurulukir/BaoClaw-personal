use crate::utils::command::run_command_async;
use serde_json::{json, Value};
use std::path::Path;

/// Execute `git status --porcelain` in the given directory asynchronously.
pub async fn execute_git_status(cwd: &Path) -> Value {
    match run_command_async("git", &["status", "--porcelain"], Some(cwd)).await {
        Ok(out) => {
            json!({
                "clean": out.stdout.trim().is_empty(),
                "status": out.stdout,
            })
        }
        Err(e) => json!({
            "clean": false,
            "error": format!("Failed to run git status: {}", e),
        }),
    }
}

/// Execute `git diff` in the given directory asynchronously.
pub async fn execute_git_diff(cwd: &Path) -> Value {
    match run_command_async("git", &["diff"], Some(cwd)).await {
        Ok(out) => {
            json!({
                "diff": out.stdout,
            })
        }
        Err(e) => json!({
            "error": format!("Failed to run git diff: {}", e),
        }),
    }
}

/// Handle GitStatus request using engine's git_info.
pub fn handle_git_status(cwd: &Path) -> Result<Value, String> {
    match crate::engine::git_info::get_git_info(cwd) {
        Some(info) => Ok(json!({
            "branch": info.branch,
            "has_changes": info.has_changes,
            "staged_files": info.staged_files,
            "modified_files": info.modified_files,
            "untracked_files": info.untracked_files,
        })),
        None => Err("Not a git repository".to_string()),
    }
}

/// Execute `git commit -m <message>` in the given directory asynchronously.
pub async fn execute_git_commit(cwd: &Path, message: &str) -> Value {
    match run_command_async("git", &["commit", "-m", message], Some(cwd)).await {
        Ok(out) => {
            json!({
                "success": out.status.success(),
                "output": format!("{}\n{}", out.stdout, out.stderr).trim().to_string(),
            })
        }
        Err(e) => json!({
            "success": false,
            "error": format!("Failed to run git commit: {}", e),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn setup_temp_repo() -> Option<tempfile::TempDir> {
        let tmp = tempdir().ok()?;
        let init = run_command_async("git", &["init"], Some(tmp.path())).await.ok()?;
        if !init.success() {
            return None;
        }
        let _ = run_command_async("git", &["config", "user.email", "test@test.com"], Some(tmp.path())).await;
        let _ = run_command_async("git", &["config", "user.name", "Test"], Some(tmp.path())).await;
        Some(tmp)
    }

    #[tokio::test]
    async fn test_git_status_runs() {
        if let Some(tmp) = setup_temp_repo().await {
            let res = execute_git_status(tmp.path()).await;
            assert!(res.get("clean").is_some());
        }
    }

    #[tokio::test]
    async fn test_git_diff_runs() {
        if let Some(tmp) = setup_temp_repo().await {
            let res = execute_git_diff(tmp.path()).await;
            assert!(res.get("diff").is_some());
        }
    }

    #[tokio::test]
    async fn test_git_commit_runs() {
        if let Some(tmp) = setup_temp_repo().await {
            let file_path = tmp.path().join("test.txt");
            std::fs::write(&file_path, "hello").unwrap();
            let _ = run_command_async("git", &["add", "."], Some(tmp.path())).await;
            let res = execute_git_commit(tmp.path(), "test message").await;
            assert_eq!(res.get("success").and_then(|v| v.as_bool()), Some(true));
        }
    }
}
