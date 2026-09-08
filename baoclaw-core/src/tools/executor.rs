use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use super::trait_def::*;
use crate::engine::query_engine::EngineEvent;
use crate::permissions::gate::PermissionDecision;
use crate::permissions::manager::PermissionResult;

/// A ProgressSender that forwards progress events through an mpsc channel as EngineEvents.
pub struct ChannelProgressSender {
    tx: tokio::sync::mpsc::Sender<EngineEvent>,
}

impl ChannelProgressSender {
    pub fn new(tx: tokio::sync::mpsc::Sender<EngineEvent>) -> Self {
        Self { tx }
    }
}

#[async_trait::async_trait]
impl ProgressSender for ChannelProgressSender {
    async fn send_progress(&self, tool_use_id: &str, data: Value) {
        let _ = self
            .tx
            .send(EngineEvent::Progress {
                tool_use_id: tool_use_id.to_string(),
                data,
            })
            .await;
    }
}

/// Working handle for interactive permission checks inside the executor: the
/// engine's PermissionBridge joined with the event channel of the turn
/// currently being executed.
#[derive(Clone)]
pub struct PermissionChannels {
    pub bridge: crate::permissions::PermissionBridge,
    pub event_tx: tokio::sync::mpsc::Sender<EngineEvent>,
}

impl PermissionChannels {
    pub fn new(
        bridge: crate::permissions::PermissionBridge,
        event_tx: tokio::sync::mpsc::Sender<EngineEvent>,
    ) -> Self {
        Self { bridge, event_tx }
    }
}

/// Result of a single tool execution
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    pub tool_use_id: String,
    pub tool_name: String,
    pub output: Value,
    pub is_error: bool,
}

/// A pending tool use from the LLM
#[derive(Clone, Debug)]
pub struct ToolUseRequest {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// Execute a single tool following the pipeline: validate → permissions → call
pub async fn execute_tool(
    tool: &dyn Tool,
    request: &ToolUseRequest,
    context: &ToolContext,
    progress: &dyn ProgressSender,
) -> ToolExecutionResult {
    let tool_name = tool.name().to_string();
    let tool_use_id = request.id.clone();

    // Step 1: Validate input
    let validation = tool.validate_input(&request.input, context).await;
    if let ValidationResult::Invalid { message, .. } = validation {
        return ToolExecutionResult {
            tool_use_id,
            tool_name,
            output: Value::String(format!("Validation error: {}", message)),
            is_error: true,
        };
    }

    // Step 2: Check permissions
    let permission = tool.check_permissions(&request.input, context).await;
    match permission {
        ToolPermissionCheckResult::Allow { .. } => {}
        ToolPermissionCheckResult::Deny { message } => {
            return ToolExecutionResult {
                tool_use_id,
                tool_name,
                output: Value::String(format!("Permission denied: {}", message)),
                is_error: true,
            };
        }
        ToolPermissionCheckResult::Ask { message, .. } => {
            if tool.is_read_only(&request.input) {
                eprintln!(
                    "[permissions] WARN: Ask permission on read-only tool '{}'; proceeding without confirmation",
                    tool_name
                );
            } else {
                return ToolExecutionResult {
                    tool_use_id,
                    tool_name: tool_name.clone(),
                    output: Value::String(format!(
                        "Permission denied: confirmation required for non-read-only tool '{}' ({}); interactive permission channel not available in direct execution mode",
                        tool_name, message
                    )),
                    is_error: true,
                };
            }
        }
    }

    // Step 3: Call the tool with abort awareness
    let call_result = call_tool_with_abort(tool, request, context, progress).await;
    match call_result {
        Ok(result) => {
            let max_size = tool
                .max_result_size_chars()
                .min(crate::config::tool_output_threshold());
            let output = maybe_persist_or_truncate(result.data, max_size, context, &tool_use_id);
            ToolExecutionResult {
                tool_use_id,
                tool_name,
                output,
                is_error: result.is_error,
            }
        }
        Err(err) => ToolExecutionResult {
            tool_use_id,
            tool_name,
            output: Value::String(format!("Tool execution error: {}", err)),
            is_error: true,
        },
    }
}

/// Call a tool, cancelling with `ToolError::Aborted` as soon as the context's
/// abort signal fires. Shared by the direct and permission-gated paths so
/// abort behaves identically mid-tool regardless of how the call was approved.
async fn call_tool_with_abort(
    tool: &dyn Tool,
    request: &ToolUseRequest,
    context: &ToolContext,
    progress: &dyn ProgressSender,
) -> Result<ToolResult, ToolError> {
    let abort_signal = context.abort_signal.clone();
    tokio::select! {
        r = tool.call(request.input.clone(), context, progress) => r,
        _ = async {
            let mut rx = abort_signal.as_ref().clone();
            // Only abort if the value actually changed to true.
            // A dropped sender with value=false is not an abort.
            loop {
                if *rx.borrow() {
                    break; // abort signal received
                }
                if rx.changed().await.is_err() {
                    // Sender dropped without setting to true — don't abort
                    // Wait indefinitely (this branch should never resolve)
                    std::future::pending::<()>().await;
                }
            }
        } => {
            Err(ToolError::Aborted)
        }
    }
}

/// Execute a single tool with permission checking via PermissionManager and PermissionGate.
///
/// Flow: validate → check PermissionManager → Allow/Deny/Ask branch → call tool.
/// Ask branch: read-only tools proceed without prompting (same as the direct
/// path); mutating tools send a PermissionRequest event and wait on the gate
/// for the user's decision, auto-denying after the context's `ask_timeout_secs`.
pub async fn execute_tool_with_permission(
    tool: &dyn Tool,
    request: &ToolUseRequest,
    context: &ToolContext,
    permission: &PermissionChannels,
    progress: &dyn ProgressSender,
) -> ToolExecutionResult {
    let tool_name = tool.name().to_string();
    let tool_use_id = request.id.clone();

    // Step 1: Validate input
    let validation = tool.validate_input(&request.input, context).await;
    if let ValidationResult::Invalid { message, .. } = validation {
        return ToolExecutionResult {
            tool_use_id,
            tool_name,
            output: Value::String(format!("Validation error: {}", message)),
            is_error: true,
        };
    }

    // Step 2: Check permissions via PermissionManager.
    // The read guard is scoped so it is never held across the await below —
    // the permission/* IPC handlers need the manager's write lock meanwhile.
    let input_description = serde_json::to_string(&request.input).ok();
    let perm_result = {
        let manager = permission.bridge.manager.read().await;
        manager.check_permission(&tool_name, input_description.as_deref())
    };

    match perm_result {
        PermissionResult::Allow => {
            // Direct execution
            call_tool_and_wrap(tool, request, context, progress).await
        }
        PermissionResult::Deny { message } => ToolExecutionResult {
            tool_use_id,
            tool_name,
            output: Value::String(format!("Permission denied: {}", message)),
            is_error: true,
        },
        PermissionResult::Ask { .. } => {
            // Read-only tools are never worth a prompt: the manager defaults
            // to Ask for everything, and prompting for reads would regress
            // the daemon UX (the direct path lets read-only Ask through too).
            //
            // Exception: a Glob/Grep request whose `path` resolves outside
            // the project cwd (and outside the granted/search-allowed dirs)
            // MUST prompt — reads through those tools are how an agent
            // looks around the rest of the filesystem.
            let mut search_grant: Option<std::path::PathBuf> = None;
            let mut write_grant: Option<std::path::PathBuf> = None;
            if tool.is_read_only(&request.input) {
                match out_of_cwd_search_target(&tool_name, &request.input, &context.cwd) {
                    None => {
                        eprintln!(
                            "[permissions] WARN: Ask permission on read-only tool '{}'; proceeding without confirmation",
                            tool_name
                        );
                        return call_tool_and_wrap(tool, request, context, progress).await;
                    }
                    Some(target) => {
                        // Pre-approved (config knob or a prior Always grant)
                        // → the tool's own validator will accept it; skip
                        // the prompt.
                        if dir_granted(&permission.bridge.granted_dirs, &target) {
                            return call_tool_and_wrap(tool, request, context, progress).await;
                        }
                        search_grant = Some(target);
                        // Fall through to the interactive prompt below.
                    }
                }
            } else if let Some(target) =
                out_of_cwd_write_target(&tool_name, &request.input, &context.cwd)
            {
                // The write validator would reject this target. Pre-approved
                // (config knob or a prior Always grant) → the tool accepts
                // it; skip the prompt.
                if dir_granted(&permission.bridge.granted_write_dirs, &target) {
                    return call_tool_and_wrap(tool, request, context, progress).await;
                }
                write_grant = Some(target);
                // Fall through to the interactive prompt below — an
                // out-of-boundary write needs an explicit human decision,
                // which is also the only thing that can extend the write
                // boundary (whole-tool allow rules never do).
            }

            // Live knob read: every prompt uses the current config values
            // (config `extra["permissions"]`), so setAskTimeout /
            // setPersistGrants take effect without an engine restart.
            let (ask_timeout, persist_grants) = {
                let manager = permission.bridge.manager.read().await;
                let ctx = manager.get_context();
                (ctx.ask_timeout_duration(), ctx.persist_grants)
            };

            // Resolved out-of-boundary target (when the prompt exists because
            // of one) so gateways can show the real path.
            let target_path = search_grant
                .as_ref()
                .or(write_grant.as_ref())
                .map(|p| p.to_string_lossy().to_string());

            // Send PermissionRequest event to clients — carrying the exact
            // auto-deny window this ask is parked under, so gateway prompts
            // can mirror the daemon's schedule instead of guessing.
            let _ = permission
                .event_tx
                .send(EngineEvent::PermissionRequest {
                    tool_name: tool_name.clone(),
                    input: request.input.clone(),
                    tool_use_id: tool_use_id.clone(),
                    ask_timeout_secs: ask_timeout.as_secs().max(1),
                    target_path,
                })
                .await;

            // Wait for user response, auto-denying after the timeout
            let rx = permission.bridge.gate.request(&tool_use_id);
            let decision = match tokio::time::timeout(ask_timeout, rx).await {
                Ok(Ok(decision)) => decision,
                Ok(Err(_)) => PermissionDecision::Deny, // channel closed
                Err(_) => PermissionDecision::Deny,     // timeout → auto-deny
            };

            match decision {
                PermissionDecision::Allow => {
                    // One-shot grant: the approved dir is pushed for exactly
                    // this call. The guard removes it BY IDENTITY on drop, so
                    // concurrent one-shot grants (tools run in parallel) can
                    // never pop each other's entry, and a dropped future still
                    // cleans up after itself.
                    let _grant_guard = match (search_grant, write_grant) {
                        (Some(dir), _) => Some(OneShotGrantGuard::push(
                            &permission.bridge.granted_dirs,
                            dir,
                        )),
                        (None, Some(dir)) => Some(OneShotGrantGuard::push(
                            &permission.bridge.granted_write_dirs,
                            dir,
                        )),
                        (None, None) => None,
                    };
                    call_tool_and_wrap(tool, request, context, progress).await
                }
                PermissionDecision::AllowAlways { rule } => {
                    if let Some(dir) = search_grant.clone() {
                        // Directory-scoped "Always allow": record the granted
                        // dir (live + persisted) instead of the client's
                        // whole-tool rule, so only this directory opens up.
                        // For FileRead the target is a FILE, so grant its
                        // parent directory — sibling reads in the same
                        // location must not re-prompt.
                        let grant_dir = if tool_name == "FileRead" {
                            dir.parent()
                                .map(|p| p.to_path_buf())
                                .unwrap_or_else(|| dir.clone())
                        } else {
                            dir.clone()
                        };
                        push_granted_dir(&permission.bridge.granted_dirs, grant_dir.clone());
                        let dir_str = grant_dir.to_string_lossy().to_string();
                        {
                            let manager = permission.bridge.manager.write().await;
                            manager.update_context(|c| {
                                if !c.additional_search_dirs.contains(&dir_str) {
                                    c.additional_search_dirs.push(dir_str.clone());
                                }
                            });
                            // The write guard doubles as the serialization point
                            // against the permission.* RPC handlers that save
                            // the same file.
                            if persist_grants {
                                crate::permissions::persist_context_to_config(
                                    &manager.get_context(),
                                );
                            }
                        }
                    } else if let Some(dir) = write_grant.clone() {
                        // Directory-scoped "Always allow" for writes: open the
                        // target's PARENT directory (never the whole tool) in
                        // the write-grant list, so an approved location keeps
                        // working while the boundary stays intact elsewhere.
                        let grant_dir = dir
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_else(|| dir.clone());
                        push_granted_dir(&permission.bridge.granted_write_dirs, grant_dir.clone());
                        let dir_str = grant_dir.to_string_lossy().to_string();
                        {
                            let manager = permission.bridge.manager.write().await;
                            manager.update_context(|c| {
                                if !c.additional_write_dirs.contains(&dir_str) {
                                    c.additional_write_dirs.push(dir_str.clone());
                                }
                            });
                            if persist_grants {
                                crate::permissions::persist_context_to_config(
                                    &manager.get_context(),
                                );
                            }
                        }
                    } else {
                        let manager = permission.bridge.manager.write().await;
                        manager.add_allow_always_rule("user", &tool_name, rule);
                        // The write guard doubles as the serialization point
                        // against the permission.* RPC handlers that save
                        // the same file.
                        if persist_grants {
                            crate::permissions::persist_context_to_config(&manager.get_context());
                        }
                    }
                    call_tool_and_wrap(tool, request, context, progress).await
                }
                PermissionDecision::Deny => ToolExecutionResult {
                    tool_use_id,
                    tool_name,
                    output: Value::String("Permission denied by user".to_string()),
                    is_error: true,
                },
            }
        }
    }
}

/// If `input` asks the Glob/Grep tools to operate outside the project cwd
/// (or FileRead to read outside it), return the lexically-resolved target
/// (directory or file) that a grant would cover. `None` = not an out-of-cwd
/// search/read (no prompt needed for boundary reasons).
fn out_of_cwd_search_target(
    tool_name: &str,
    input: &Value,
    cwd: &std::path::Path,
) -> Option<std::path::PathBuf> {
    if tool_name != "GlobTool" && tool_name != "GrepTool" && tool_name != "FileRead" {
        return None;
    }
    // Glob/Grep take a search `path`; FileRead takes a `file_path`.
    let path = if tool_name == "FileRead" {
        input.get("file_path")?.as_str()?.trim()
    } else {
        input.get("path")?.as_str()?.trim()
    };
    if path.is_empty() {
        return None;
    }
    // The tools' validator rejects anything outside cwd + granted dirs, so
    // "the validator would fail" is exactly the prompt condition. (An `Ok`
    // here means the path is already inside the boundary.)
    match super::builtins::path_utils::resolve_and_validate_path(path, cwd, &[]) {
        Ok(_) => None,
        Err(_) => {
            // Lexical resolve for the grant record; the tool re-runs the full
            // validation (including symlink canonicalization) after approval.
            let joined = cwd.join(path);
            Some(crate::tools::builtins::path_utils::normalize_path(&joined))
        }
    }
}

/// If `input` asks the FileWrite/FileEdit/NotebookEdit tools to write
/// outside the project cwd, return the lexically-resolved target that a
/// grant would cover. `None` = not an out-of-cwd write (no boundary reason
/// to prompt). The write twin of [`out_of_cwd_search_target`]; pre-approved
/// dirs (config knob / prior Always grants) are filtered by the caller's
/// `dir_granted` check, exactly like the search flow.
fn out_of_cwd_write_target(
    tool_name: &str,
    input: &Value,
    cwd: &std::path::Path,
) -> Option<std::path::PathBuf> {
    if tool_name != "FileWrite" && tool_name != "FileEdit" && tool_name != "NotebookEditTool" {
        return None;
    }
    let path = if tool_name == "NotebookEditTool" {
        input.get("notebook_path")?.as_str()?.trim()
    } else {
        input.get("file_path")?.as_str()?.trim()
    };
    if path.is_empty() {
        return None;
    }
    // The tools' validator rejects anything outside cwd + granted write
    // dirs, so "the validator would fail" is exactly the prompt condition.
    match super::builtins::path_utils::resolve_and_validate_path(path, cwd, &[]) {
        Ok(_) => None,
        Err(_) => {
            // Lexical resolve for the grant record; the tool re-runs the full
            // validation (including symlink canonicalization) after approval.
            let joined = cwd.join(path);
            Some(crate::tools::builtins::path_utils::normalize_path(&joined))
        }
    }
}

fn dir_granted(
    granted_dirs: &crate::permissions::GrantedSearchDirs,
    target: &std::path::Path,
) -> bool {
    granted_dirs
        .read()
        .map(|dirs| dirs.iter().any(|d| target.starts_with(d)))
        .unwrap_or(false)
}

fn push_granted_dir(granted_dirs: &crate::permissions::GrantedSearchDirs, dir: std::path::PathBuf) {
    if let Ok(mut dirs) = granted_dirs.write() {
        dirs.push(dir);
    }
}

/// One-shot search-dir grant: pushes `dir` on creation and removes exactly
/// that entry (by identity, not position) when dropped — safe under the
/// parallel execution of concurrency-safe tools, and leak-free if the
/// executor future is dropped mid-call.
struct OneShotGrantGuard {
    granted_dirs: crate::permissions::GrantedSearchDirs,
    dir: std::path::PathBuf,
}

impl OneShotGrantGuard {
    fn push(granted_dirs: &crate::permissions::GrantedSearchDirs, dir: std::path::PathBuf) -> Self {
        push_granted_dir(granted_dirs, dir.clone());
        Self {
            granted_dirs: crate::permissions::GrantedSearchDirs::clone(granted_dirs),
            dir,
        }
    }
}

impl Drop for OneShotGrantGuard {
    fn drop(&mut self) {
        if let Ok(mut dirs) = self.granted_dirs.write() {
            if let Some(pos) = dirs.iter().position(|d| d == &self.dir) {
                dirs.remove(pos);
            }
        }
    }
}

/// Helper: call a tool and wrap the result into ToolExecutionResult.
/// Abort-aware mid-call, matching the direct path's behavior.
async fn call_tool_and_wrap(
    tool: &dyn Tool,
    request: &ToolUseRequest,
    context: &ToolContext,
    progress: &dyn ProgressSender,
) -> ToolExecutionResult {
    let tool_name = tool.name().to_string();
    let tool_use_id = request.id.clone();

    // Early-exit if already aborted before we even start the tool.
    if *context.abort_signal.borrow() {
        return ToolExecutionResult {
            tool_use_id,
            tool_name,
            output: serde_json::json!({"error": "Tool execution aborted by user."}),
            is_error: true,
        };
    }

    let call_result = call_tool_with_abort(tool, request, context, progress).await;
    match call_result {
        Ok(result) => {
            let max_size = tool
                .max_result_size_chars()
                .min(crate::config::tool_output_threshold());
            let output = maybe_persist_or_truncate(result.data, max_size, context, &tool_use_id);
            ToolExecutionResult {
                tool_use_id,
                tool_name,
                output,
                is_error: result.is_error,
            }
        }
        Err(err) => ToolExecutionResult {
            tool_use_id,
            tool_name,
            output: Value::String(format!("Tool execution error: {}", err)),
            is_error: true,
        },
    }
}

/// Try to persist large tool results to disk; fall back to truncation.
///
/// If a `ToolResultStore` is available in the context and the serialized
/// output exceeds the threshold, the full content is written to a file and
/// the in-context value is replaced with a `<persisted-output>` block.
/// Otherwise the old truncation logic is used.
fn maybe_persist_or_truncate(
    data: Value,
    max_size_chars: usize,
    context: &ToolContext,
    tool_use_id: &str,
) -> Value {
    let serialized = match serde_json::to_string(&data) {
        Ok(s) => s,
        Err(_) => return data,
    };

    // Under the size limit — no action needed
    if serialized.len() <= max_size_chars {
        return data;
    }

    // Try persisting via ToolResultStore
    if let Some(ref store) = context.tool_result_store {
        // Extract string content from the Value for persistence
        let content_str = match &data {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };

        if let Some(formatted) = store.persist_and_format(&content_str, tool_use_id) {
            return Value::String(formatted);
        }
    }

    // Fallback: truncate
    let truncated: String = serialized.chars().take(max_size_chars).collect();
    Value::String(format!(
        "{}\n\n[Result truncated: output exceeded {} characters]",
        truncated, max_size_chars
    ))
}

/// Execute a single tool via the gated path when permission channels are
/// available, otherwise the direct fail-closed path.
async fn dispatch_tool(
    tool: &dyn Tool,
    request: &ToolUseRequest,
    context: &ToolContext,
    progress: &dyn ProgressSender,
    permission: Option<&PermissionChannels>,
    tool_health: Option<&std::sync::Arc<crate::engine::tool_health::ToolHealthTracker>>,
) -> ToolExecutionResult {
    // Hard-block tools whose status is Disabled after repeated failures.
    // Degraded tools still run — the system-reminder warns the model.
    if let Some(th) = tool_health {
        if !th.is_available(tool.name()) {
            eprintln!("[tool-health] '{}' blocked (disabled)", tool.name());
            return ToolExecutionResult {
                tool_use_id: request.id.clone(),
                tool_name: tool.name().to_string(),
                output: Value::String(format!(
                    "🚫 Tool '{}' is temporarily disabled after repeated failures. Use an alternative tool or try again later.",
                    tool.name()
                )),
                is_error: true,
            };
        }
    }
    match permission {
        Some(p) => execute_tool_with_permission(tool, request, context, p, progress).await,
        None => execute_tool(tool, request, context, progress).await,
    }
}

/// Execute multiple tools, running concurrency-safe tools in parallel
/// and non-concurrency-safe tools sequentially.
pub async fn execute_tools(
    tools: &[Arc<dyn Tool>],
    requests: &[ToolUseRequest],
    context: &ToolContext,
    progress: &dyn ProgressSender,
    permission: Option<&PermissionChannels>,
    tool_health: Option<&std::sync::Arc<crate::engine::tool_health::ToolHealthTracker>>,
) -> Vec<ToolExecutionResult> {
    if requests.is_empty() {
        return vec![];
    }

    let total = requests.len();

    // Build (original_index, request, tool_ref) tuples
    let mut concurrent: Vec<(usize, &ToolUseRequest, &Arc<dyn Tool>)> = Vec::with_capacity(total);
    let mut sequential: Vec<(usize, &ToolUseRequest, &Arc<dyn Tool>)> = Vec::with_capacity(total);
    let mut not_found: Vec<(usize, &ToolUseRequest)> = Vec::with_capacity(total);

    for (idx, req) in requests.iter().enumerate() {
        match find_tool(tools, &req.name) {
            Some(tool) => {
                if tool.is_concurrency_safe(&req.input) {
                    concurrent.push((idx, req, tool));
                } else {
                    sequential.push((idx, req, tool));
                }
            }
            None => {
                not_found.push((idx, req));
            }
        }
    }

    let mut results: Vec<Option<ToolExecutionResult>> = vec![None; total];

    // Execute concurrent-safe tools in parallel, bounded by the configured
    // parallel-tool cap so a wide batch cannot stampede the system.
    if !concurrent.is_empty() {
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(
            crate::config::max_parallel_tools(),
        ));
        let futures: Vec<_> = concurrent
            .iter()
            .map(|(idx, req, tool)| {
                let permit = std::sync::Arc::clone(&semaphore).acquire_owned();
                async move {
                    // The semaphore is never closed, so acquiring always
                    // succeeds; on the impossible error path run unbounded.
                    let _permit = permit.await.ok();
                    (
                        idx,
                        dispatch_tool(
                            tool.as_ref(),
                            req,
                            context,
                            progress,
                            permission,
                            tool_health,
                        )
                        .await,
                    )
                }
            })
            .collect();
        let concurrent_results = futures::future::join_all(futures).await;
        for (idx, result) in concurrent_results {
            results[*idx] = Some(result);
        }
    }

    // Execute sequential tools one by one
    for (idx, req, tool) in &sequential {
        let result = dispatch_tool(
            tool.as_ref(),
            req,
            context,
            progress,
            permission,
            tool_health,
        )
        .await;
        results[*idx] = Some(result);
    }

    // Handle not-found tools
    for (idx, req) in &not_found {
        results[*idx] = Some(ToolExecutionResult {
            tool_use_id: req.id.clone(),
            tool_name: req.name.clone(),
            output: Value::String(format!("Tool '{}' not found", req.name)),
            is_error: true,
        });
    }

    // Unwrap all results (every slot should be filled)
    results.into_iter().map(|r| r.unwrap()).collect()
}

/// Find a tool by name (case-insensitive, also checks aliases)
pub fn find_tool<'a>(tools: &'a [Arc<dyn Tool>], name: &str) -> Option<&'a Arc<dyn Tool>> {
    let name_lower = name.to_ascii_lowercase();
    tools.iter().find(|t| {
        if t.name().to_ascii_lowercase() == name_lower {
            return true;
        }
        t.aliases()
            .iter()
            .any(|alias| alias.to_ascii_lowercase() == name_lower)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::gate::PermissionGate;
    use crate::permissions::manager::{
        PermissionManager, PermissionMode, PermissionRule, ToolPermissionContext,
    };
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    // --- Mock implementations ---

    struct MockProgressSender;

    #[async_trait::async_trait]
    impl ProgressSender for MockProgressSender {
        async fn send_progress(&self, _tool_use_id: &str, _data: Value) {}
    }

    /// A simple mock tool for testing
    struct MockTool {
        tool_name: String,
        tool_aliases: Vec<String>,
        concurrency_safe: bool,
        read_only: bool,
        max_result_size: usize,
        validation_result: std::sync::Mutex<Option<ValidationResult>>,
        permission_result: std::sync::Mutex<Option<ToolPermissionCheckResult>>,
        call_result: std::sync::Mutex<Option<Result<ToolResult, ToolError>>>,
        call_count: AtomicUsize,
    }

    impl MockTool {
        fn new(name: &str) -> Self {
            Self {
                tool_name: name.to_string(),
                tool_aliases: vec![],
                concurrency_safe: false,
                read_only: false,
                max_result_size: 100_000,
                validation_result: std::sync::Mutex::new(None),
                permission_result: std::sync::Mutex::new(None),
                call_result: std::sync::Mutex::new(None),
                call_count: AtomicUsize::new(0),
            }
        }

        fn with_read_only(mut self, read_only: bool) -> Self {
            self.read_only = read_only;
            self
        }

        fn with_aliases(mut self, aliases: Vec<&str>) -> Self {
            self.tool_aliases = aliases.into_iter().map(String::from).collect();
            self
        }

        fn with_concurrency_safe(mut self, safe: bool) -> Self {
            self.concurrency_safe = safe;
            self
        }

        fn with_max_result_size(mut self, size: usize) -> Self {
            self.max_result_size = size;
            self
        }

        fn with_validation(self, result: ValidationResult) -> Self {
            *self.validation_result.lock().unwrap() = Some(result);
            self
        }

        fn with_permission(self, result: ToolPermissionCheckResult) -> Self {
            *self.permission_result.lock().unwrap() = Some(result);
            self
        }

        fn with_call_result(self, result: Result<ToolResult, ToolError>) -> Self {
            *self.call_result.lock().unwrap() = Some(result);
            self
        }
    }

    #[async_trait::async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.tool_name
        }

        fn aliases(&self) -> Vec<&str> {
            self.tool_aliases.iter().map(|s| s.as_str()).collect()
        }

        fn input_schema(&self) -> JsonSchema {
            JsonSchema {
                schema_type: "object".to_string(),
                properties: None,
                required: None,
                description: None,
            }
        }

        fn is_read_only(&self, _input: &Value) -> bool {
            self.read_only
        }

        fn is_concurrency_safe(&self, _input: &Value) -> bool {
            self.concurrency_safe
        }

        fn max_result_size_chars(&self) -> usize {
            self.max_result_size
        }

        async fn validate_input(&self, _input: &Value, _context: &ToolContext) -> ValidationResult {
            self.validation_result
                .lock()
                .unwrap()
                .take()
                .unwrap_or(ValidationResult::Ok)
        }

        async fn check_permissions(
            &self,
            _input: &Value,
            _context: &ToolContext,
        ) -> ToolPermissionCheckResult {
            self.permission_result.lock().unwrap().take().unwrap_or(
                ToolPermissionCheckResult::Allow {
                    updated_input: Value::Null,
                },
            )
        }

        async fn call(
            &self,
            _input: Value,
            _context: &ToolContext,
            _progress: &dyn ProgressSender,
        ) -> Result<ToolResult, ToolError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.call_result
                .lock()
                .unwrap()
                .take()
                .unwrap_or(Ok(ToolResult {
                    data: json!({"result": "ok"}),
                    is_error: false,
                }))
        }

        fn prompt(&self) -> String {
            String::new()
        }
    }

    fn make_context() -> ToolContext {
        let (tx, rx) = tokio::sync::watch::channel(false);
        // Keep tx alive — dropping it would cause the abort future to resolve immediately
        std::mem::forget(tx);
        ToolContext {
            cwd: PathBuf::from("/tmp"),
            model: "test-model".to_string(),
            abort_signal: Arc::new(rx),
            file_cache: None,
            tool_result_store: None,
            context_window: 200_000,
            auto_compact_threshold_ratio: 0.7,
        }
    }

    fn make_request(id: &str, name: &str) -> ToolUseRequest {
        ToolUseRequest {
            id: id.to_string(),
            name: name.to_string(),
            input: json!({}),
        }
    }

    // --- execute_tool tests ---

    #[tokio::test]
    async fn test_execute_tool_success() {
        let tool = MockTool::new("TestTool").with_call_result(Ok(ToolResult {
            data: json!({"hello": "world"}),
            is_error: false,
        }));
        let ctx = make_context();
        let progress = MockProgressSender;
        let request = make_request("req-1", "TestTool");

        let result = execute_tool(&tool, &request, &ctx, &progress).await;

        assert_eq!(result.tool_use_id, "req-1");
        assert_eq!(result.tool_name, "TestTool");
        assert_eq!(result.output, json!({"hello": "world"}));
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn test_execute_tool_validation_failure() {
        let tool = MockTool::new("TestTool").with_validation(ValidationResult::Invalid {
            message: "bad input".to_string(),
            code: None,
        });
        let ctx = make_context();
        let progress = MockProgressSender;
        let request = make_request("req-2", "TestTool");

        let result = execute_tool(&tool, &request, &ctx, &progress).await;

        assert!(result.is_error);
        assert!(result.output.as_str().unwrap().contains("Validation error"));
        assert!(result.output.as_str().unwrap().contains("bad input"));
        // call should not have been invoked
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_execute_tool_permission_denied() {
        let tool = MockTool::new("TestTool").with_permission(ToolPermissionCheckResult::Deny {
            message: "not allowed".to_string(),
        });
        let ctx = make_context();
        let progress = MockProgressSender;
        let request = make_request("req-3", "TestTool");

        let result = execute_tool(&tool, &request, &ctx, &progress).await;

        assert!(result.is_error);
        assert!(result
            .output
            .as_str()
            .unwrap()
            .contains("Permission denied"));
        assert!(result.output.as_str().unwrap().contains("not allowed"));
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_execute_tool_ask_fails_closed_for_mutating_tool() {
        let tool = MockTool::new("BashTool")
            .with_read_only(false)
            .with_permission(ToolPermissionCheckResult::Ask {
                message: "BashTool requires user confirmation".to_string(),
                updated_input: Value::Null,
            });
        let ctx = make_context();
        let progress = MockProgressSender;
        let request = make_request("req-ask-deny", "BashTool");

        let result = execute_tool(&tool, &request, &ctx, &progress).await;

        assert!(result.is_error);
        assert!(result
            .output
            .as_str()
            .unwrap()
            .contains("Permission denied: confirmation required"));
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_execute_tool_ask_proceeds_for_read_only_tool() {
        let tool = MockTool::new("FileReadTool")
            .with_read_only(true)
            .with_permission(ToolPermissionCheckResult::Ask {
                message: "FileReadTool requires confirmation".to_string(),
                updated_input: Value::Null,
            })
            .with_call_result(Ok(ToolResult {
                data: json!({"content": "file contents"}),
                is_error: false,
            }));
        let ctx = make_context();
        let progress = MockProgressSender;
        let request = make_request("req-ask-allow", "FileReadTool");

        let result = execute_tool(&tool, &request, &ctx, &progress).await;

        assert!(!result.is_error);
        assert_eq!(result.output, json!({"content": "file contents"}));
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_execute_tool_call_error() {
        let tool = MockTool::new("TestTool").with_call_result(Err(ToolError::ExecutionFailed(
            "something broke".to_string(),
        )));
        let ctx = make_context();
        let progress = MockProgressSender;
        let request = make_request("req-4", "TestTool");

        let result = execute_tool(&tool, &request, &ctx, &progress).await;

        assert!(result.is_error);
        assert!(result
            .output
            .as_str()
            .unwrap()
            .contains("Tool execution error"));
    }

    #[tokio::test]
    async fn test_execute_tool_truncates_large_result() {
        let large_data = "x".repeat(200);
        let tool = MockTool::new("TestTool")
            .with_max_result_size(50)
            .with_call_result(Ok(ToolResult {
                data: Value::String(large_data),
                is_error: false,
            }));
        let ctx = make_context();
        let progress = MockProgressSender;
        let request = make_request("req-5", "TestTool");

        let result = execute_tool(&tool, &request, &ctx, &progress).await;

        assert!(!result.is_error);
        let output_str = result.output.as_str().unwrap();
        assert!(output_str.contains("[Result truncated"));
    }

    // --- find_tool tests ---

    #[test]
    fn test_find_tool_by_name() {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(MockTool::new("FileRead")),
            Arc::new(MockTool::new("Bash")),
        ];

        assert!(find_tool(&tools, "Bash").is_some());
        assert_eq!(find_tool(&tools, "Bash").unwrap().name(), "Bash");
    }

    #[test]
    fn test_find_tool_case_insensitive() {
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(MockTool::new("FileRead"))];

        assert!(find_tool(&tools, "fileread").is_some());
        assert!(find_tool(&tools, "FILEREAD").is_some());
        assert!(find_tool(&tools, "FileRead").is_some());
    }

    #[test]
    fn test_find_tool_by_alias() {
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(
            MockTool::new("FileRead").with_aliases(vec!["Read", "FR"]),
        )];

        assert!(find_tool(&tools, "Read").is_some());
        assert!(find_tool(&tools, "fr").is_some());
        assert!(find_tool(&tools, "FR").is_some());
    }

    #[test]
    fn test_find_tool_not_found() {
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(MockTool::new("FileRead"))];

        assert!(find_tool(&tools, "NonExistent").is_none());
    }

    // --- execute_tools tests ---

    #[tokio::test]
    async fn test_execute_tools_empty() {
        let tools: Vec<Arc<dyn Tool>> = vec![];
        let requests: Vec<ToolUseRequest> = vec![];
        let ctx = make_context();
        let progress = MockProgressSender;

        let results = execute_tools(&tools, &requests, &ctx, &progress, None, None).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_execute_tools_not_found() {
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(MockTool::new("FileRead"))];
        let requests = vec![make_request("req-1", "NonExistent")];
        let ctx = make_context();
        let progress = MockProgressSender;

        let results = execute_tools(&tools, &requests, &ctx, &progress, None, None).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert!(results[0].output.as_str().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn test_execute_tools_preserves_order() {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(MockTool::new("ToolA").with_concurrency_safe(true)),
            Arc::new(MockTool::new("ToolB")),
            Arc::new(MockTool::new("ToolC").with_concurrency_safe(true)),
        ];
        let requests = vec![
            make_request("req-1", "ToolA"),
            make_request("req-2", "ToolB"),
            make_request("req-3", "ToolC"),
        ];
        let ctx = make_context();
        let progress = MockProgressSender;

        let results = execute_tools(&tools, &requests, &ctx, &progress, None, None).await;

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].tool_use_id, "req-1");
        assert_eq!(results[0].tool_name, "ToolA");
        assert_eq!(results[1].tool_use_id, "req-2");
        assert_eq!(results[1].tool_name, "ToolB");
        assert_eq!(results[2].tool_use_id, "req-3");
        assert_eq!(results[2].tool_name, "ToolC");
    }

    #[tokio::test]
    async fn test_execute_tools_mixed_concurrent_sequential() {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(MockTool::new("ConcurrentTool").with_concurrency_safe(true)),
            Arc::new(MockTool::new("SequentialTool")),
        ];
        let requests = vec![
            make_request("req-1", "ConcurrentTool"),
            make_request("req-2", "SequentialTool"),
        ];
        let ctx = make_context();
        let progress = MockProgressSender;

        let results = execute_tools(&tools, &requests, &ctx, &progress, None, None).await;

        assert_eq!(results.len(), 2);
        assert!(!results[0].is_error);
        assert!(!results[1].is_error);
    }

    // ── Interactive permission gate (execute_tool_with_permission) ──

    fn make_manager_ctx() -> ToolPermissionContext {
        ToolPermissionContext {
            mode: PermissionMode::Default,
            additional_search_dirs: Vec::new(),
            additional_write_dirs: Vec::new(),
            always_allow_rules: std::collections::HashMap::new(),
            always_deny_rules: std::collections::HashMap::new(),
            always_ask_rules: std::collections::HashMap::new(),
            auto_allow_channels: std::collections::HashMap::new(),
            ask_timeout_secs: 300,
            // Must stay false: a true value would let unit tests write mock
            // rules into the developer's real ~/.baoclaw/config.json.
            persist_grants: false,
        }
    }

    fn make_channels(
        manager: PermissionManager,
        gate: PermissionGate,
    ) -> (PermissionChannels, tokio::sync::mpsc::Receiver<EngineEvent>) {
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<EngineEvent>(16);
        let bridge = crate::permissions::PermissionBridge {
            manager: Arc::new(tokio::sync::RwLock::new(manager)),
            gate,
            granted_dirs: crate::permissions::GrantedSearchDirs::default(),
            granted_write_dirs: crate::permissions::GrantedWriteDirs::default(),
        };
        (PermissionChannels::new(bridge, event_tx), event_rx)
    }

    /// Context whose prompt timeout is `secs` (the executor reads the timeout
    /// from the manager's context, not from the bridge).
    fn manager_ctx_with_timeout(secs: u64) -> PermissionManager {
        let mut ctx = make_manager_ctx();
        ctx.ask_timeout_secs = secs;
        PermissionManager::new(ctx)
    }

    #[tokio::test]
    async fn test_gated_ask_allow_executes_tool() {
        let tool = MockTool::new("BashTool");
        let ctx = make_context();
        let progress = MockProgressSender;
        let request = make_request("req-gate-1", "BashTool");
        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let gate = channels.bridge.gate.clone();
        let responder = tokio::spawn(async move {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for PermissionRequest")
                .expect("event channel closed");
            match event {
                EngineEvent::PermissionRequest { tool_use_id, .. } => {
                    gate.respond(&tool_use_id, PermissionDecision::Allow);
                }
                other => panic!("unexpected event: {other:?}"),
            }
        });

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;
        responder.await.unwrap();

        assert!(!result.is_error);
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_gated_ask_deny_blocks_tool() {
        let tool = MockTool::new("BashTool");
        let ctx = make_context();
        let progress = MockProgressSender;
        let request = make_request("req-gate-2", "BashTool");
        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let gate = channels.bridge.gate.clone();
        let responder = tokio::spawn(async move {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for PermissionRequest")
                .expect("event channel closed");
            if let EngineEvent::PermissionRequest { tool_use_id, .. } = event {
                gate.respond(&tool_use_id, PermissionDecision::Deny);
            }
        });

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;
        responder.await.unwrap();

        assert!(result.is_error);
        assert_eq!(result.output.as_str().unwrap(), "Permission denied by user");
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_gated_ask_timeout_auto_denies() {
        let tool = MockTool::new("BashTool");
        let ctx = make_context();
        let progress = MockProgressSender;
        let request = make_request("req-gate-3", "BashTool");
        let (channels, _event_rx) =
            make_channels(manager_ctx_with_timeout(1), PermissionGate::new());

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;

        assert!(result.is_error);
        assert_eq!(result.output.as_str().unwrap(), "Permission denied by user");
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_gated_allow_always_persists_rule() {
        let tool = MockTool::new("Bash");
        let ctx = make_context();
        let progress = MockProgressSender;
        let request = make_request("req-gate-4", "Bash");
        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let gate = channels.bridge.gate.clone();
        let responder = tokio::spawn(async move {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for PermissionRequest")
                .expect("event channel closed");
            if let EngineEvent::PermissionRequest { tool_use_id, .. } = event {
                gate.respond(
                    &tool_use_id,
                    PermissionDecision::AllowAlways {
                        rule: Some("*".to_string()),
                    },
                );
            }
        });

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;
        responder.await.unwrap();
        assert!(!result.is_error);

        // The AllowAlways rule must make the next check skip the prompt.
        let manager = channels.bridge.manager.read().await;
        assert!(matches!(
            manager.check_permission("Bash", Some("anything")),
            PermissionResult::Allow
        ));
    }

    #[tokio::test]
    async fn test_gated_manager_deny_never_prompts() {
        let mut deny_rules = std::collections::HashMap::new();
        deny_rules.insert(
            "system".to_string(),
            vec![PermissionRule {
                tool_name: "BashTool".to_string(),
                rule_content: None,
            }],
        );
        let ctx = ToolPermissionContext {
            always_deny_rules: deny_rules,
            ask_timeout_secs: 5,
            ..make_manager_ctx()
        };

        let tool = MockTool::new("BashTool");
        let tool_ctx = make_context();
        let progress = MockProgressSender;
        let request = make_request("req-gate-5", "BashTool");
        let (channels, mut event_rx) =
            make_channels(PermissionManager::new(ctx), PermissionGate::new());

        let result =
            execute_tool_with_permission(&tool, &request, &tool_ctx, &channels, &progress).await;

        assert!(result.is_error);
        assert!(result
            .output
            .as_str()
            .unwrap()
            .contains("Permission denied"));
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 0);
        // No prompt may be emitted for a rule-based denial.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), event_rx.recv())
                .await
                .is_err(),
            "no PermissionRequest event should be sent for Deny"
        );
    }

    #[tokio::test]
    async fn test_gated_read_only_ask_does_not_prompt() {
        let tool = MockTool::new("FileReadTool").with_read_only(true);
        let ctx = make_context();
        let progress = MockProgressSender;
        let request = make_request("req-gate-6", "FileReadTool");
        let (channels, mut event_rx) = make_channels(
            PermissionManager::new(make_manager_ctx()),
            PermissionGate::new(),
        );

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;

        assert!(!result.is_error);
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), event_rx.recv())
                .await
                .is_err(),
            "read-only tools must not prompt"
        );
    }

    // ── Tool-health enforcement at dispatch ──

    #[tokio::test]
    async fn test_disabled_tool_is_blocked_at_dispatch() {
        let tool = Arc::new(MockTool::new("BashTool"));
        let ctx = make_context();
        let progress = MockProgressSender;
        let requests = vec![make_request("req-th-1", "BashTool")];
        let tools: Vec<Arc<dyn Tool>> = vec![tool.clone()];

        let tracker = Arc::new(crate::engine::tool_health::ToolHealthTracker::new());
        for _ in 0..6 {
            tracker.record_failure("BashTool", "boom");
        }

        let results = execute_tools(&tools, &requests, &ctx, &progress, None, Some(&tracker)).await;

        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert!(
            results[0]
                .output
                .as_str()
                .unwrap()
                .contains("temporarily disabled"),
            "unexpected output: {:?}",
            results[0].output
        );
        // The tool itself was never invoked.
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 0);
    }

    // ── Out-of-cwd Glob/Grep search grants ──

    #[test]
    fn test_one_shot_grant_guard_is_identity_scoped() {
        // Two overlapping one-shot grants (tools run in parallel): each guard
        // removes only its own entry, whatever the drop order.
        let granted: crate::permissions::GrantedSearchDirs =
            std::sync::Arc::new(std::sync::RwLock::new(Vec::new()));

        let g1 = OneShotGrantGuard::push(&granted, PathBuf::from("/a"));
        {
            let _g2 = OneShotGrantGuard::push(&granted, PathBuf::from("/b"));
            assert_eq!(
                granted.read().unwrap().as_slice(),
                [PathBuf::from("/a"), PathBuf::from("/b")]
            );
        } // g2 dropped: /b removed, /a untouched
        assert_eq!(granted.read().unwrap().as_slice(), [PathBuf::from("/a")]);
        drop(g1); // g1 dropped: /a removed
        assert!(granted.read().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_out_of_cwd_search_prompts_and_allow_once_executes() {
        let tool = MockTool::new("GlobTool").with_read_only(true);
        let ctx = make_context(); // cwd = /tmp
        let progress = MockProgressSender;
        let mut request = make_request("req-search-1", "GlobTool");
        request.input = json!({"pattern": "*.conf", "path": "/etc"});

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let gate = channels.bridge.gate.clone();
        let responder = tokio::spawn(async move {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for PermissionRequest")
                .expect("event channel closed");
            match event {
                EngineEvent::PermissionRequest { tool_use_id, .. } => {
                    gate.respond(&tool_use_id, PermissionDecision::Allow);
                }
                other => panic!("unexpected event: {other:?}"),
            }
        });

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;
        responder.await.unwrap();

        assert!(!result.is_error);
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 1);
        // The one-shot grant is popped after the call.
        assert!(channels.bridge.granted_dirs.read().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_out_of_cwd_search_granted_runs_without_prompt() {
        let tool = MockTool::new("GrepTool").with_read_only(true);
        let ctx = make_context();
        let progress = MockProgressSender;
        let mut request = make_request("req-search-2", "GrepTool");
        request.input = json!({"pattern": "root", "path": "/etc"});

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());
        channels
            .bridge
            .granted_dirs
            .write()
            .unwrap()
            .push(PathBuf::from("/etc"));

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;

        // Pre-approved directory: no prompt, tool just runs.
        assert!(event_rx.try_recv().is_err());
        assert!(!result.is_error);
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_out_of_cwd_search_allow_always_records_dir() {
        let tool = MockTool::new("GlobTool").with_read_only(true);
        let ctx = make_context();
        let progress = MockProgressSender;
        let mut request = make_request("req-search-3", "GlobTool");
        request.input = json!({"pattern": "*.conf", "path": "/etc"});

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let gate = channels.bridge.gate.clone();
        let responder = tokio::spawn(async move {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for PermissionRequest")
                .expect("event channel closed");
            if let EngineEvent::PermissionRequest { tool_use_id, .. } = event {
                gate.respond(&tool_use_id, PermissionDecision::AllowAlways { rule: None });
            }
        });

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;
        responder.await.unwrap();

        assert!(!result.is_error);
        // Directory grant is live for future calls...
        assert!(channels
            .bridge
            .granted_dirs
            .read()
            .unwrap()
            .contains(&PathBuf::from("/etc")));
        // ...and recorded in the context. (persist_grants=false in this
        // fixture, so nothing touches the real config.)
        let ctx_now = channels.bridge.manager.read().await.get_context();
        assert!(ctx_now.additional_search_dirs.iter().any(|d| d == "/etc"));
    }

    #[tokio::test]
    async fn test_out_of_cwd_search_deny_blocks_tool() {
        let tool = MockTool::new("GlobTool").with_read_only(true);
        let ctx = make_context();
        let progress = MockProgressSender;
        let mut request = make_request("req-search-4", "GlobTool");
        request.input = json!({"pattern": "*.conf", "path": "/etc"});

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let gate = channels.bridge.gate.clone();
        let responder = tokio::spawn(async move {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for PermissionRequest")
                .expect("event channel closed");
            if let EngineEvent::PermissionRequest { tool_use_id, .. } = event {
                gate.respond(&tool_use_id, PermissionDecision::Deny);
            }
        });

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;
        responder.await.unwrap();

        assert!(result.is_error);
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 0);
        // A denial grants nothing.
        assert!(channels.bridge.granted_dirs.read().unwrap().is_empty());
    }

    // ── Out-of-cwd write grants (FileWrite/FileEdit twin of the search flow) ──

    #[tokio::test]
    async fn test_out_of_cwd_write_prompts_and_allow_once_executes() {
        let tool = MockTool::new("FileWrite");
        let ctx = make_context(); // cwd = /tmp
        let progress = MockProgressSender;
        let mut request = make_request("req-write-1", "FileWrite");
        request.input = json!({"file_path": "/etc/baoclaw-write-smoke.txt", "content": "hi"});

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let gate = channels.bridge.gate.clone();
        let responder = tokio::spawn(async move {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for PermissionRequest")
                .expect("event channel closed");
            match event {
                EngineEvent::PermissionRequest {
                    tool_use_id,
                    target_path,
                    ..
                } => {
                    // The prompt carries the resolved out-of-boundary target.
                    assert_eq!(target_path.as_deref(), Some("/etc/baoclaw-write-smoke.txt"));
                    gate.respond(&tool_use_id, PermissionDecision::Allow);
                }
                other => panic!("unexpected event: {other:?}"),
            }
        });

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;
        responder.await.unwrap();

        assert!(!result.is_error);
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 1);
        // The one-shot write grant is popped after the call.
        assert!(channels
            .bridge
            .granted_write_dirs
            .read()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn test_out_of_cwd_write_granted_runs_without_prompt() {
        let tool = MockTool::new("FileEdit");
        let ctx = make_context();
        let progress = MockProgressSender;
        let mut request = make_request("req-write-2", "FileEdit");
        request.input = json!({"file_path": "/etc/app.conf", "old_string": "a", "new_string": "b"});

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());
        channels
            .bridge
            .granted_write_dirs
            .write()
            .unwrap()
            .push(PathBuf::from("/etc"));

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;

        // Pre-approved directory: no prompt, tool just runs.
        assert!(event_rx.try_recv().is_err());
        assert!(!result.is_error);
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 1);
    }

    // ── Out-of-cwd read grants (FileRead joins the search flow) ──

    #[tokio::test]
    async fn test_out_of_cwd_file_read_prompts_and_allow_once_executes() {
        let tool = MockTool::new("FileRead").with_read_only(true);
        let ctx = make_context(); // cwd = /tmp
        let progress = MockProgressSender;
        let mut request = make_request("req-read-1", "FileRead");
        request.input = json!({"file_path": "/etc/baoclaw-read-smoke.txt"});

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let gate = channels.bridge.gate.clone();
        let responder = tokio::spawn(async move {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for PermissionRequest")
                .expect("event channel closed");
            match event {
                EngineEvent::PermissionRequest {
                    tool_use_id,
                    target_path,
                    ..
                } => {
                    assert_eq!(target_path.as_deref(), Some("/etc/baoclaw-read-smoke.txt"));
                    gate.respond(&tool_use_id, PermissionDecision::Allow);
                }
                other => panic!("unexpected event: {other:?}"),
            }
        });

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;
        responder.await.unwrap();

        assert!(!result.is_error);
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 1);
        // The one-shot grant is popped after the call.
        assert!(channels.bridge.granted_dirs.read().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_out_of_cwd_file_read_granted_runs_without_prompt() {
        let tool = MockTool::new("FileRead").with_read_only(true);
        let ctx = make_context();
        let progress = MockProgressSender;
        let mut request = make_request("req-read-2", "FileRead");
        request.input = json!({"file_path": "/etc/notes.txt"});

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());
        channels
            .bridge
            .granted_dirs
            .write()
            .unwrap()
            .push(PathBuf::from("/etc"));

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;

        // Pre-approved directory: no prompt, tool just runs.
        assert!(event_rx.try_recv().is_err());
        assert!(!result.is_error);
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_file_read_in_cwd_auto_proceeds_without_prompt() {
        let tool = MockTool::new("FileRead").with_read_only(true);
        let ctx = make_context();
        let progress = MockProgressSender;
        let mut request = make_request("req-read-3", "FileRead");
        request.input = json!({"file_path": "/tmp/in-cwd.txt"});

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;

        // In-boundary read: no prompt (the read-only auto-proceed path).
        assert!(event_rx.try_recv().is_err());
        assert!(!result.is_error);
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_out_of_cwd_notebook_edit_prompts_and_allow_once_executes() {
        let tool = MockTool::new("NotebookEditTool");
        let ctx = make_context();
        let progress = MockProgressSender;
        let mut request = make_request("req-nb-1", "NotebookEditTool");
        request.input = json!({
            "notebook_path": "/etc/baoclaw-smoke.ipynb",
            "operation": "replace_cell",
            "cell_index": 0,
            "source": ["x"]
        });

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let gate = channels.bridge.gate.clone();
        let responder = tokio::spawn(async move {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for PermissionRequest")
                .expect("event channel closed");
            match event {
                EngineEvent::PermissionRequest {
                    tool_use_id,
                    target_path,
                    ..
                } => {
                    assert_eq!(target_path.as_deref(), Some("/etc/baoclaw-smoke.ipynb"));
                    gate.respond(&tool_use_id, PermissionDecision::Allow);
                }
                other => panic!("unexpected event: {other:?}"),
            }
        });

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;
        responder.await.unwrap();

        assert!(!result.is_error);
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 1);
        assert!(channels
            .bridge
            .granted_write_dirs
            .read()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn test_out_of_cwd_notebook_edit_allow_always_records_parent_dir() {
        let tool = MockTool::new("NotebookEditTool");
        let ctx = make_context();
        let progress = MockProgressSender;
        let mut request = make_request("req-nb-2", "NotebookEditTool");
        request.input = json!({
            "notebook_path": "/etc/conf/demo.ipynb",
            "operation": "insert_cell",
            "cell_index": 0,
            "cell_type": "code",
            "source": ["x"]
        });

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let gate = channels.bridge.gate.clone();
        let responder = tokio::spawn(async move {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for PermissionRequest")
                .expect("event channel closed");
            if let EngineEvent::PermissionRequest { tool_use_id, .. } = event {
                gate.respond(&tool_use_id, PermissionDecision::AllowAlways { rule: None });
            }
        });

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;
        responder.await.unwrap();

        assert!(!result.is_error);
        // The target's PARENT dir is granted (not the whole tool)...
        assert!(channels
            .bridge
            .granted_write_dirs
            .read()
            .unwrap()
            .contains(&PathBuf::from("/etc/conf")));
        let ctx_now = channels.bridge.manager.read().await.get_context();
        assert!(ctx_now
            .additional_write_dirs
            .iter()
            .any(|d| d == "/etc/conf"));
        // Directory scoping means NO whole-tool rule is recorded.
        assert!(ctx_now.always_allow_rules.is_empty());
    }

    #[tokio::test]
    async fn test_out_of_cwd_notebook_edit_deny_blocks_tool() {
        let tool = MockTool::new("NotebookEditTool");
        let ctx = make_context();
        let progress = MockProgressSender;
        let mut request = make_request("req-nb-3", "NotebookEditTool");
        request.input = json!({
            "notebook_path": "/etc/baoclaw-denied.ipynb",
            "operation": "insert_cell",
            "cell_index": 0,
            "cell_type": "code",
            "source": ["x"]
        });

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let gate = channels.bridge.gate.clone();
        let responder = tokio::spawn(async move {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for PermissionRequest")
                .expect("event channel closed");
            if let EngineEvent::PermissionRequest { tool_use_id, .. } = event {
                gate.respond(&tool_use_id, PermissionDecision::Deny);
            }
        });

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;
        responder.await.unwrap();

        assert!(result.is_error);
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 0);
        assert!(channels
            .bridge
            .granted_write_dirs
            .read()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn test_out_of_cwd_write_allow_always_records_parent_dir() {
        let tool = MockTool::new("FileWrite");
        let ctx = make_context();
        let progress = MockProgressSender;
        let mut request = make_request("req-write-3", "FileWrite");
        request.input = json!({"file_path": "/etc/conf/app.conf", "content": "hi"});

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let gate = channels.bridge.gate.clone();
        let responder = tokio::spawn(async move {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for PermissionRequest")
                .expect("event channel closed");
            if let EngineEvent::PermissionRequest { tool_use_id, .. } = event {
                gate.respond(&tool_use_id, PermissionDecision::AllowAlways { rule: None });
            }
        });

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;
        responder.await.unwrap();

        assert!(!result.is_error);
        // The target's PARENT dir is granted (not the whole tool)...
        assert!(channels
            .bridge
            .granted_write_dirs
            .read()
            .unwrap()
            .contains(&PathBuf::from("/etc/conf")));
        // ...and recorded in the context. (persist_grants=false in this
        // fixture, so nothing touches the real config.)
        let ctx_now = channels.bridge.manager.read().await.get_context();
        assert!(ctx_now
            .additional_write_dirs
            .iter()
            .any(|d| d == "/etc/conf"));
        // Directory scoping means NO whole-tool rule is recorded — an
        // "always allow" on one directory must not open every path.
        assert!(ctx_now.always_allow_rules.is_empty());
    }

    #[tokio::test]
    async fn test_out_of_cwd_write_deny_blocks_tool() {
        let tool = MockTool::new("FileWrite");
        let ctx = make_context();
        let progress = MockProgressSender;
        let mut request = make_request("req-write-4", "FileWrite");
        request.input = json!({"file_path": "/etc/baoclaw-denied.txt", "content": "hi"});

        let (channels, mut event_rx) =
            make_channels(manager_ctx_with_timeout(5), PermissionGate::new());

        let gate = channels.bridge.gate.clone();
        let responder = tokio::spawn(async move {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for PermissionRequest")
                .expect("event channel closed");
            if let EngineEvent::PermissionRequest { tool_use_id, .. } = event {
                gate.respond(&tool_use_id, PermissionDecision::Deny);
            }
        });

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;
        responder.await.unwrap();

        assert!(result.is_error);
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 0);
        // A denial grants nothing.
        assert!(channels
            .bridge
            .granted_write_dirs
            .read()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn test_write_allow_rule_never_expands_the_boundary() {
        // A whole-tool allow rule skips the PROMPT, but must not open the
        // write boundary: no interactive decision happened, so nothing may
        // be added to the granted write dirs. (In production the real
        // FileWriteTool then rejects the path at its own validator — the
        // mock here has no validator, so the call itself runs.)
        let tool = MockTool::new("FileWrite");
        let ctx = make_context();
        let progress = MockProgressSender;
        let mut request = make_request("req-write-5", "FileWrite");
        request.input = json!({"file_path": "/etc/baoclaw-rule-write.txt", "content": "hi"});

        let manager = manager_ctx_with_timeout(5);
        manager.add_allow_always_rule("user", "FileWrite", None);
        let (channels, mut event_rx) = make_channels(manager, PermissionGate::new());

        let result =
            execute_tool_with_permission(&tool, &request, &ctx, &channels, &progress).await;

        assert!(event_rx.try_recv().is_err());
        assert!(channels
            .bridge
            .granted_write_dirs
            .read()
            .unwrap()
            .is_empty());
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 1);
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn test_execute_tools_none_fails_closed_for_mutating_ask() {
        let tool = Arc::new(MockTool::new("BashTool").with_permission(
            ToolPermissionCheckResult::Ask {
                message: "BashTool requires user confirmation".to_string(),
                updated_input: Value::Null,
            },
        ));
        let tools: Vec<Arc<dyn Tool>> = vec![tool.clone()];
        let ctx = make_context();
        let progress = MockProgressSender;
        let requests = vec![make_request("req-gate-7", "BashTool")];

        let results = execute_tools(&tools, &requests, &ctx, &progress, None, None).await;

        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert!(results[0]
            .output
            .as_str()
            .unwrap()
            .contains("interactive permission channel not available"));
        assert_eq!(tool.call_count.load(Ordering::SeqCst), 0);
    }
}
