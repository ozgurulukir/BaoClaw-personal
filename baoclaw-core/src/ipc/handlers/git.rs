use serde_json::{json, Value};
use std::path::Path;

/// Execute `git status --porcelain` in the given directory.
pub fn execute_git_status(cwd: &Path) -> Value {
    match std::process::Command::new("git")
        .arg("status")
        .arg("--porcelain")
        .current_dir(cwd)
        .output()
    {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            json!({
                "clean": stdout.trim().is_empty(),
                "status": stdout,
            })
        }
        Err(e) => json!({
            "clean": false,
            "error": format!("Failed to run git status: {}", e),
        }),
    }
}

/// Execute `git diff` in the given directory.
pub fn execute_git_diff(cwd: &Path) -> Value {
    match std::process::Command::new("git")
        .arg("diff")
        .current_dir(cwd)
        .output()
    {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            json!({
                "diff": stdout,
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

/// Execute `git commit -m <message>` in the given directory.
pub fn execute_git_commit(cwd: &Path, message: &str) -> Value {
    match std::process::Command::new("git")
        .arg("commit")
        .arg("-m")
        .arg(message)
        .current_dir(cwd)
        .output()
    {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            json!({
                "success": out.status.success(),
                "output": format!("{}\n{}", stdout, stderr).trim().to_string(),
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

    #[test]
    fn test_git_status_runs() {
        let res = execute_git_status(Path::new("."));
        assert!(res.get("clean").is_some());
    }
}
