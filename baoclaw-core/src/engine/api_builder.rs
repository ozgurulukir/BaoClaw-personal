use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

use crate::api::client::CreateMessageRequest;
use crate::engine::query_engine::{CachedRule, QueryLoopConfig, ThinkingConfig};
use crate::engine::query_loop::validate_and_fix_tool_messages;
use crate::models::message::{ContentBlock, Message, MessageContent};

/// Load project instructions from BAOCLAW.md files.
///
/// Scans `.baoclaw/BAOCLAW.md` first, then `BAOCLAW.md` in the given directory.
/// Returns the content of the first found non-empty file, or None.
pub fn load_project_instructions(cwd: &Path) -> Option<String> {
    let paths = [
        cwd.join(".baoclaw").join("BAOCLAW.md"),
        cwd.join("BAOCLAW.md"),
    ];
    for p in &paths {
        if let Ok(content) = std::fs::read_to_string(p) {
            if !content.trim().is_empty() {
                return Some(content);
            }
        }
    }
    None
}

/// A parsed rule file from `.baoclaw/rules/*.md`.
pub struct RuleFile {
    /// Rule content (with YAML frontmatter stripped).
    content: String,
    /// Optional glob pattern from frontmatter `paths` field.
    paths_pattern: Option<String>,
}

/// Load rules from `.baoclaw/rules/*.md`, optionally filtering by `recent_file_paths`.
///
/// Rules without a `paths` frontmatter field are loaded unconditionally.
/// Rules with `paths` are only included when at least one entry in
/// `recent_file_paths` matches the glob pattern.
pub fn load_rules_with_paths(cwd: &Path, recent_file_paths: &[String]) -> Vec<String> {
    let rules_dir = cwd.join(".baoclaw").join("rules");
    let entries = match std::fs::read_dir(&rules_dir) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };

    let mut matched_rules: Vec<String> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let rule = parse_rule_file(&content);
        let should_include = match &rule.paths_pattern {
            None => true,
            Some(pattern) => {
                // Use glob matching: include if any recent file path matches
                match glob::Pattern::new(pattern) {
                    Ok(glob_pattern) => {
                        recent_file_paths.iter().any(|fp| {
                            // Try matching against the full path or just the filename
                            glob_pattern.matches(fp)
                                || glob_pattern.matches(
                                    std::path::Path::new(fp)
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or(""),
                                )
                        })
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: invalid glob pattern '{}' in {}: {}",
                            pattern,
                            path.display(),
                            e
                        );
                        // If pattern is invalid, include the rule anyway
                        true
                    }
                }
            }
        };

        if should_include && !rule.content.trim().is_empty() {
            matched_rules.push(format!(
                "# Rule: {}\n\n{}",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown"),
                rule.content
            ));
        }
    }

    matched_rules
}

/// Parse a rule file, extracting YAML frontmatter if present.
///
/// Supports `---` delimited frontmatter with a `paths` field:
/// ```markdown
/// ---
/// paths: "src/**/*.rs"
/// ---
/// Rule content here.
/// ```
pub fn parse_rule_file(content: &str) -> RuleFile {
    let trimmed = content.trim();

    // Check for YAML frontmatter
    if trimmed.starts_with("---") {
        // Find closing ---
        if let Some(rest) = trimmed.get(3..) {
            if let Some(end_idx) = rest.find("---") {
                let frontmatter = &rest[..end_idx];
                let body = rest[end_idx + 3..].trim();

                // Parse paths from frontmatter (simple line-based parsing)
                let paths_pattern = frontmatter.lines().find_map(|line| {
                    let line = line.trim();
                    if line.starts_with("paths:") || line.starts_with("paths :") {
                        let value = line.split_once(':')?.1.trim();
                        // Strip quotes if present
                        let value = value.trim_matches('"').trim_matches('\'');
                        if value.is_empty() {
                            None
                        } else {
                            Some(value.to_string())
                        }
                    } else {
                        None
                    }
                });

                return RuleFile {
                    content: body.to_string(),
                    paths_pattern,
                };
            }
        }
    }

    // No frontmatter
    RuleFile {
        content: trimmed.to_string(),
        paths_pattern: None,
    }
}

/// Load all rule files from `.baoclaw/rules/*.md` into cached structures.
/// This is called once in `QueryEngine::new()` and the results are reused
/// across turns, avoiding repeated file I/O.
pub fn load_all_rule_files(cwd: &Path) -> Vec<CachedRule> {
    let rules_dir = cwd.join(".baoclaw").join("rules");
    let entries = match std::fs::read_dir(&rules_dir) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };

    let mut rules = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let rule = parse_rule_file(&content);
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        rules.push(CachedRule {
            filename,
            content: rule.content,
            paths_pattern: rule.paths_pattern,
        });
    }
    rules
}

/// Filter cached rules against recent file paths using glob matching.
/// Replaces `load_rules_with_paths` for in-memory cached rules.
pub fn filter_cached_rules(cached: &[CachedRule], recent_file_paths: &[String]) -> Vec<String> {
    cached
        .iter()
        .filter(|rule| {
            match &rule.paths_pattern {
                None => true,
                Some(pattern) => {
                    match glob::Pattern::new(pattern) {
                        Ok(glob_pattern) => recent_file_paths.iter().any(|fp| {
                            glob_pattern.matches(fp)
                                || glob_pattern.matches(
                                    std::path::Path::new(fp)
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or(""),
                                )
                        }),
                        Err(_) => true, // Invalid pattern → include anyway
                    }
                }
            }
        })
        .filter(|rule| !rule.content.trim().is_empty())
        .map(|rule| format!("# Rule: {}\n\n{}", rule.filename, rule.content))
        .collect()
}

/// Extract file paths mentioned in the most recent N messages.
///
/// Looks for file_path, file, path, pattern, cwd, and directory fields in
/// tool inputs and text content.
pub fn extract_recent_file_paths(messages: &[Message], max_messages: usize) -> Vec<String> {
    let start = messages.len().saturating_sub(max_messages);
    let mut paths = Vec::new();

    for msg in &messages[start..] {
        match &msg.content {
            MessageContent::User { message, .. } => {
                // Look for file_path in tool_result content
                if let Value::Array(blocks) = &message.content {
                    for block in blocks {
                        if let Some(content) = block.get("content").and_then(|c| c.as_str()) {
                            // Extract file paths that appear in tool results
                            for line in content.lines().take(50) {
                                if line.contains('/') || line.contains("\\.") {
                                    paths.push(line.trim().to_string());
                                }
                            }
                        }
                    }
                }
                if let Value::String(text) = &message.content {
                    // Simple heuristic: extract path-like strings from text
                    for word in text.split_whitespace() {
                        if (word.contains('/')
                            || word.contains(".rs")
                            || word.contains(".ts")
                            || word.contains(".js")
                            || word.contains(".py")
                            || word.contains(".md")
                            || word.contains(".toml"))
                            && word.len() > 5
                            && word.len() < 300
                        {
                            paths.push(word.to_string());
                        }
                    }
                }
            }
            MessageContent::Assistant { message, .. } => {
                for block in &message.content {
                    if let ContentBlock::ToolUse { input, .. } = block {
                        // Extract file_path from tool inputs
                        if let Some(fp) = input.get("file_path").and_then(|v| v.as_str()) {
                            paths.push(fp.to_string());
                        }
                        if let Some(p) = input.get("path").and_then(|v| v.as_str()) {
                            paths.push(p.to_string());
                        }
                        if let Some(p) = input.get("pattern").and_then(|v| v.as_str()) {
                            paths.push(p.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Deduplicate while preserving order
    let mut seen = std::collections::HashSet::new();
    paths.retain(|p| seen.insert(p.clone()));

    // Limit to reasonable count
    paths.truncate(50);
    paths
}

/// Build an API request from the current messages and config.
pub fn build_api_request(messages: &[Message], config: &QueryLoopConfig) -> CreateMessageRequest {
    // First validate and fix tool_use/tool_result pairing
    let validated_messages = validate_and_fix_tool_messages(messages);

    // Convert messages to API format
    let mut api_messages: Vec<Value> = validated_messages
        .iter()
        .filter_map(|msg| {
            match &msg.content {
                MessageContent::User { message, .. } => {
                    // Skip empty user messages
                    let is_empty = match &message.content {
                        Value::String(s) => s.trim().is_empty(),
                        Value::Array(arr) => arr.is_empty(),
                        _ => message.content.is_null(),
                    };
                    if is_empty {
                        eprintln!("Skipping empty user message");
                        return None;
                    }
                    Some(serde_json::json!({
                        "role": message.role,
                        "content": message.content,
                    }))
                }
                MessageContent::Assistant { message, .. } => {
                    // Skip empty assistant messages
                    if message.content.is_empty() {
                        eprintln!("Skipping empty assistant message");
                        return None;
                    }
                    // Also check if all content blocks are empty
                    let has_content = message.content.iter().any(|block| match block {
                        ContentBlock::Text { text } => !text.trim().is_empty(),
                        ContentBlock::Thinking { thinking } => !thinking.trim().is_empty(),
                        ContentBlock::ToolUse { .. } => true,
                        _ => false,
                    });
                    if !has_content {
                        eprintln!("Skipping assistant message with no valid content");
                        return None;
                    }
                    let content_value =
                        serde_json::to_value(&message.content).unwrap_or(Value::Array(vec![]));
                    Some(serde_json::json!({
                        "role": message.role,
                        "content": content_value,
                    }))
                }
                _ => None,
            }
        })
        .collect();

    // Inject dynamic <system-reminder> into the last user message to avoid
    // invalidating the cached system prompt prefix.  Git status, session
    // memory, and other per-turn information goes here.  Only genuine user
    // turns receive it: re-appending the reminder to every tool-result
    // continuation turn made the model re-acknowledge the same content
    // (e.g. the session memory summary) after each tool call.
    if let Some(reminder) = build_dynamic_reminder(config) {
        if api_messages.last().is_some_and(is_user_text_turn) {
            if let Some(last_msg) = api_messages.last_mut() {
                // Append the reminder to the existing user message content
                if let Some(content) = last_msg.get_mut("content") {
                    match content {
                        Value::String(s) => {
                            *s = format!("{}\n\n{}", s, reminder);
                        }
                        Value::Array(blocks) => {
                            blocks.push(serde_json::json!({
                                "type": "text",
                                "text": reminder,
                            }));
                        }
                        _ => {
                            // Fallback: replace content with a string containing the reminder
                            *content = Value::String(format!("{}\n\n{}", content, reminder));
                        }
                    }
                }
            }
        }
    }

    let system = build_system_prompt(config);
    let tools = build_tools_list(config);

    CreateMessageRequest {
        model: config.model.clone(),
        messages: api_messages,
        system,
        tools,
        max_tokens: config.max_tokens,
        stream: true,
        thinking: match &config.thinking_config {
            ThinkingConfig::Disabled => None,
            ThinkingConfig::Adaptive => Some(serde_json::json!({
                "type": "enabled",
                "budget_tokens": 10240
            })),
            ThinkingConfig::Enabled { budget_tokens } => Some(serde_json::json!({
                "type": "enabled",
                "budget_tokens": budget_tokens
            })),
        },
        metadata: None,
    }
}

/// Build the tools list for one API call over the query's tool snapshot.
///
/// Tool order is part of the cached prefix, so non-deterministic iteration
/// (e.g. HashMap-based) would break caching.
pub fn build_tools_list(config: &QueryLoopConfig) -> Option<Vec<Value>> {
    let expanded = config
        .expanded_tools
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    build_tools_list_with(&config.tools, &expanded)
}

/// Layout, for prompt-cache stability (the tools array is part of the cached
/// prefix): group 1 = non-deferred tools (alphabetical, frozen for the
/// query) carrying the cache breakpoint; group 2 = activated deferred tools
/// (alphabetical — their entries never move the breakpoint); group 3 = stubs
/// (alphabetical). A stub expanding to full schema mutates only its own
/// entry after the breakpoint, so activations never invalidate the prefix.
pub fn build_tools_list_with(
    tools: &[Arc<dyn crate::tools::Tool>],
    expanded: &std::collections::HashSet<String>,
) -> Option<Vec<Value>> {
    if tools.is_empty() {
        return None;
    }
    #[derive(PartialEq, PartialOrd, Ord, Eq)]
    enum Group {
        Frozen,
        Activated,
        Stub,
    }
    let mut entries: Vec<(Group, Value)> = tools
        .iter()
        .map(|t| {
            let is_deferred = t.is_deferred();
            let group = if !is_deferred {
                Group::Frozen
            } else if expanded.contains(t.name()) {
                Group::Activated
            } else {
                Group::Stub
            };
            let entry = if group == Group::Stub {
                // Minimal VALID schema: real gateways reject tools without
                // one, and no wire defer field is sent (deferral is purely
                // this client's serialization choice).
                serde_json::json!({
                    "name": t.name(),
                    "description": t.short_description(),
                    "input_schema": { "type": "object", "properties": {} },
                })
            } else {
                serde_json::json!({
                    "name": t.name(),
                    "description": t.prompt(),
                    "input_schema": t.input_schema(),
                })
            };
            (group, entry)
        })
        .collect();
    entries.sort_by(|a, b| {
        let name_a = a.1.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let name_b = b.1.get("name").and_then(|v| v.as_str()).unwrap_or("");
        a.0.cmp(&b.0).then_with(|| name_a.cmp(name_b))
    });
    // Cache breakpoint after the FROZEN group: activations mutate entries
    // that sit after it, so the cached prefix survives every activation.
    let breakpoint = entries
        .iter()
        .rposition(|(g, _)| *g == Group::Frozen)
        .map(|i| i + 1);
    let mut tool_list: Vec<Value> = entries.into_iter().map(|(_, e)| e).collect();
    if let Some(idx) = breakpoint {
        if let Some(obj) = tool_list.get_mut(idx - 1).and_then(|v| v.as_object_mut()) {
            obj.insert(
                "cache_control".to_string(),
                serde_json::json!({ "type": "ephemeral" }),
            );
        }
    }
    Some(tool_list)
}

/// Build the system prompt from config — **static parts only**.
///
/// Prompt Caching works via prefix matching: any change to the system prompt
/// invalidates the entire cached prefix.  Therefore, only content that is
/// stable across turns is placed here.  Dynamic information (git status,
/// session memory) is injected via `<system-reminder>` user messages instead.
///
/// Order (stable → volatile):
///   1. Core system prompt / custom prompt      ← never changes in-session
///   2. Working directory                        ← never changes in-session
///   3. Project instructions (BAOCLAW.md)        ← rarely changes
///   4. Project rules (.baoclaw/rules/)          ← rarely changes
///   5. Append system prompt                     ← rarely changes
pub fn build_system_prompt(config: &QueryLoopConfig) -> Option<Vec<Value>> {
    let mut parts: Vec<String> = Vec::new();

    // 1. Core system prompt
    if let Some(custom) = &config.custom_system_prompt {
        parts.push(custom.clone());
    } else {
        parts.push("You are a helpful AI coding assistant.".to_string());
    }

    // 2. Current working directory (stable within a session)
    parts.push(format!(
        "Current working directory: {}\n\nWhen the user asks to display or show a file's content, output the full content directly in your response. Do not summarize or describe the file — show the actual text.",
        config.cwd.display()
    ));

    // 3. Project instructions from BAOCLAW.md (rarely changes mid-session)
    if let Some(instructions) = &config.project_instructions {
        parts.push(format!(
            "# Project Instructions (from BAOCLAW.md)\n\n{}",
            instructions
        ));
    }

    // 4. Project rules from .baoclaw/rules/*.md (path-filtered from cache)
    {
        let recent_paths = extract_recent_file_paths(&config.recent_messages_for_rules, 10);
        let rules = filter_cached_rules(&config.cached_rules_raw, &recent_paths);
        if !rules.is_empty() {
            parts.push(format!(
                "# Project Rules (from .baoclaw/rules/)\n\n{}",
                rules.join("\n\n")
            ));
        }
    }

    // 5. Append system prompt
    if let Some(append) = &config.append_system_prompt {
        parts.push(append.clone());
    }

    if parts.is_empty() {
        None
    } else {
        let combined = parts.join("\n\n");
        // Mark the static system prompt with cache_control so the API caches it.
        Some(vec![serde_json::json!({
            "type": "text",
            "text": combined,
            "cache_control": { "type": "ephemeral" },
        })])
    }
}

/// The freshness note appended below the session-memory summary: present
/// when the summary is meaningfully behind the live history, absent when
/// freshness is unknown (`None` — e.g. right after a restore) or recent.
fn summary_freshness_note(age: Option<usize>) -> Option<String> {
    let age = age?;
    if age > crate::engine::session_memory::UPDATE_INTERVAL {
        Some(format!(
            "# Summary Freshness\n\nThe session summary above was last updated {} messages ago; later work may be missing from it.",
            age
        ))
    } else {
        None
    }
}

/// Build a `<system-reminder>` user message containing **dynamic** information
/// that changes between turns (git status, session memory, etc.).
///
/// This content is kept out of the system prompt so that the cached prefix
/// remains stable.  The reminder is appended as a user message — the model
/// still sees it, but it doesn't invalidate the prompt cache.
pub fn build_dynamic_reminder(config: &QueryLoopConfig) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    // Git status — changes every turn as files are edited
    if let Some(git_info) = &config.git_info {
        let mut git_parts: Vec<String> = Vec::new();
        if let Some(branch) = &git_info.branch {
            git_parts.push(format!("Current git branch: {}", branch));
        }
        if git_info.has_changes {
            let mut change_lines: Vec<String> = Vec::new();
            if !git_info.staged_files.is_empty() {
                change_lines.push(format!("Staged: {}", git_info.staged_files.join(", ")));
            }
            if !git_info.modified_files.is_empty() {
                change_lines.push(format!("Modified: {}", git_info.modified_files.join(", ")));
            }
            if !git_info.untracked_files.is_empty() {
                change_lines.push(format!(
                    "Untracked: {}",
                    git_info.untracked_files.join(", ")
                ));
            }
            git_parts.push(format!("Changed files:\n{}", change_lines.join("\n")));
        }
        if !git_parts.is_empty() {
            parts.push(format!("# Git Status\n\n{}", git_parts.join("\n")));
        }
    }

    // Session memory (rolling summary) — updated in the background as the
    // session progresses. A staleness note is appended when the summary is
    // meaningfully behind the current history, so the model can weigh it
    // accordingly instead of treating it as current fact.
    if let Some(sm) = &config.session_memory {
        let memory = sm.get();
        if !memory.is_empty() {
            parts.push(format!("# Session Memory\n\n{}", memory));
            let age = sm.messages_since_update(config.recent_messages_for_rules.len());
            if let Some(note) = summary_freshness_note(age) {
                parts.push(note);
            }
        }
    }

    // Tool health — changes as tools fail across the daemon's queries. Kept
    // in the dynamic reminder (not the cached system prompt) so the cached
    // prefix stays stable while statuses change.
    let health_warnings = config.tool_health.get_warnings();
    if !health_warnings.is_empty() {
        parts.push(format!(
            "# Tool Health Warnings\n\n{}\n\nThese tools have been failing; prefer alternatives \
             or use extra care with their inputs.",
            health_warnings.join("\n")
        ));
    }

    if parts.is_empty() {
        None
    } else {
        Some(format!(
            "<system-reminder>\n{}\n</system-reminder>",
            parts.join("\n\n")
        ))
    }
}

/// Whether the final API message opens a genuine user turn: a user message
/// whose content is plain text, with no tool results attached.  Tool-result
/// continuation turns share the `user` role but must not receive the dynamic
/// reminder again — it was already injected on the turn's first call.
fn is_user_text_turn(msg: &Value) -> bool {
    if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
        return false;
    }
    match msg.get("content") {
        Some(Value::String(_)) => true,
        Some(Value::Array(blocks)) => !blocks
            .iter()
            .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result")),
        _ => false,
    }
}

#[cfg(test)]
mod reminder_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_user_text_turn_detection() {
        assert!(is_user_text_turn(&json!({
            "role": "user",
            "content": "fix the bug"
        })));
        assert!(is_user_text_turn(&json!({
            "role": "user",
            "content": [{"type": "text", "text": "fix the bug"}]
        })));
    }

    #[test]
    fn test_summary_freshness_note_states() {
        // Unknown freshness (restored session) → no note.
        assert!(summary_freshness_note(None).is_none());
        // Recent summary → no note.
        assert!(summary_freshness_note(Some(5)).is_none());
        // Stale summary → note with the age spelled out.
        let note = summary_freshness_note(Some(25)).unwrap();
        assert!(note.contains("# Summary Freshness"));
        assert!(note.contains("25 messages ago"));
    }

    #[test]
    fn test_tool_result_turn_is_not_a_user_turn() {
        assert!(!is_user_text_turn(&json!({
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]
        })));
        assert!(!is_user_text_turn(&json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "ok"},
                {"type": "text", "text": "stray text"}
            ]
        })));
    }

    #[test]
    fn test_assistant_role_is_not_a_user_turn() {
        assert!(!is_user_text_turn(&json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "hi"}]
        })));
    }
}
