use serde_json::Value;

use crate::engine::query_engine::{
    estimate_tokens, AdaptiveCompactTracker, CompactResult, MicroCompactConfig,
};
use crate::engine::tool_loop::extract_tool_result_ids;
use crate::models::message::{ContentBlock, Message, MessageContent};

/// Adjust the compaction split point to avoid splitting between tool calls and
/// their results. If an assistant message contains tool_use blocks, all
/// corresponding tool_result messages must either be before or after the split.
pub fn adjust_compact_split(messages: &[Message], split: usize) -> usize {
    let mut split = split;
    if split > 0 && split < messages.len() {
        if let MessageContent::Assistant { message, .. } = &messages[split - 1].content {
            // Extract all tool_use IDs from the assistant message
            let tool_use_ids: Vec<&str> = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                    _ => None,
                })
                .collect();

            if !tool_use_ids.is_empty() {
                // Scan forward to find all corresponding tool_result messages
                let mut found_results: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                let mut next_idx = split;

                while next_idx < messages.len() {
                    if let MessageContent::User { message, .. } = &messages[next_idx].content {
                        let result_ids = extract_tool_result_ids(message);
                        for id in result_ids {
                            if tool_use_ids.contains(&id.as_str()) {
                                found_results.insert(id);
                            }
                        }
                        // Stop if we've found all tool_use results
                        if found_results.len() == tool_use_ids.len() {
                            break;
                        }
                    }
                    next_idx += 1;
                }

                // Adjust split to include all tool_result messages in old_messages
                // Otherwise, move the assistant message to recent_messages
                if found_results.len() == tool_use_ids.len() {
                    split = next_idx + 1;
                } else if split > 1 {
                    split -= 1;
                }
            }
        }
    }
    split
}

/// Adaptive compact policy: clamp the tracker's recommendation to a safe
/// band. The tracker starts at 10, matching the historical hardcoded default,
/// so the first compact behaves exactly as before adaptation kicks in.
pub const ADAPTIVE_KEEP_RECENT_MIN: usize = 8;
pub const ADAPTIVE_KEEP_RECENT_MAX: usize = 30;

pub fn adaptive_keep_recent(adaptive: &AdaptiveCompactTracker) -> usize {
    adaptive
        .recommended_keep_recent()
        .clamp(ADAPTIVE_KEEP_RECENT_MIN, ADAPTIVE_KEEP_RECENT_MAX)
}

/// Feed a completed compact back into the tracker so the next keep_recent
/// adapts to observed compression.
///
/// `user_repeated` (did the user re-ask about pre-compact content within 3
/// turns) is recorded as false: that detection signal is not wired yet, so
/// today only the compression-ratio branches of the tracker can fire.
pub fn record_compact_feedback(
    adaptive: &mut AdaptiveCompactTracker,
    messages: &[Message],
    tokens_before: u64,
) {
    let tokens_after = estimate_tokens(messages);
    adaptive.record_compact(
        &CompactResult {
            tokens_saved: tokens_before.saturating_sub(tokens_after),
            summary_tokens: 0,
            tokens_before,
            tokens_after,
        },
        false,
    );
}

/// Micro-compaction — strip large tool result payloads from old messages.
/// Replaces large tool results with a lightweight placeholder if they are
/// older than `cfg.min_age_secs` AND serialized larger than `cfg.min_chars`.
pub fn micro_compact(messages: &mut [Message], cfg: MicroCompactConfig) {
    let now = std::time::SystemTime::now();
    let threshold = std::time::Duration::from_secs(cfg.min_age_secs);

    // Skip the last few messages (they are the current turn — keep intact).
    let skip_recent = 4usize;
    let start = messages.len().saturating_sub(skip_recent);

    // tool_use_id → tool name, so the placeholder says WHAT was cleared.
    let mut tool_names: std::collections::HashMap<String, String> = Default::default();
    for msg in messages.iter() {
        if let MessageContent::Assistant { message, .. } = &msg.content {
            for block in &message.content {
                if let ContentBlock::ToolUse { id, name, .. } = block {
                    tool_names.insert(id.clone(), name.clone());
                }
            }
        }
    }

    for msg in messages[..start].iter_mut() {
        let age = match chrono::DateTime::parse_from_rfc3339(&msg.timestamp) {
            Ok(ts) => {
                let msg_time = std::time::SystemTime::from(ts.with_timezone(&chrono::Utc));
                now.duration_since(msg_time)
                    .unwrap_or(std::time::Duration::ZERO)
            }
            Err(_) => continue,
        };

        if age < threshold {
            continue;
        }

        // Replace large tool-result payloads with a placeholder.
        if let MessageContent::User { message, .. } = &mut msg.content {
            if let Value::Array(blocks) = &mut message.content {
                for block in blocks.iter_mut() {
                    if block.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                        let tool_label = block
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .and_then(|id| tool_names.get(id))
                            .map(|name| format!("{name} output, "))
                            .unwrap_or_default();
                        if let Some(content) = block.get_mut("content") {
                            let output_str = content.to_string();
                            let output_chars = output_str.chars().count();
                            if output_chars > cfg.min_chars {
                                *content = serde_json::json!(format!(
                                    "[Old tool result cleared — {}originally {} chars]",
                                    tool_label, output_chars
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Lightweight compact path using the session memory summary.
pub fn session_memory_compact(messages: &mut Vec<Message>, summary_text: &str) -> bool {
    if summary_text.is_empty() {
        return false;
    }

    let keep_recent: usize = 10;
    if messages.len() <= keep_recent {
        return false;
    }

    let boundary = Message {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        content: MessageContent::System {
            subtype: crate::models::message::SystemSubtype::CompactBoundary,
            content: format!("[Session Memory]\n{}", summary_text),
        },
    };

    let recent = messages[messages.len() - keep_recent..].to_vec();
    *messages = vec![boundary];
    messages.extend(recent);

    eprintln!(
        "Session-memory compact: replaced {} old messages, kept {} recent",
        messages.len() - keep_recent - 1,
        keep_recent
    );
    true
}

/// Reactive compact — drop the oldest turns when all other compaction has
/// failed and we still can't fit the context window.
pub fn reactive_compact(messages: &mut Vec<Message>, target_reduction: Option<usize>) {
    if messages.len() <= 4 {
        return;
    }

    let mut turn_starts: Vec<usize> = Vec::new();
    for (i, msg) in messages.iter().enumerate() {
        if let MessageContent::User {
            message,
            tool_use_result,
            ..
        } = &msg.content
        {
            if tool_use_result.is_none() {
                let is_tool_result_array = match &message.content {
                    Value::Array(arr) => arr
                        .iter()
                        .all(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_result")),
                    _ => false,
                };
                if !is_tool_result_array {
                    turn_starts.push(i);
                }
            }
        }
    }

    if turn_starts.len() <= 2 {
        return;
    }

    let drop_count = match target_reduction {
        Some(target) => {
            let total_tokens = estimate_tokens(messages) as usize;
            let tokens_per_turn = total_tokens / turn_starts.len().max(1);
            (target / tokens_per_turn.max(1))
                .max(1)
                .min(turn_starts.len() / 2)
        }
        None => (turn_starts.len() / 5).max(1),
    };

    if drop_count >= turn_starts.len() {
        return;
    }

    let drop_to = turn_starts[drop_count];
    eprintln!(
        "Reactive compact: dropping {} oldest turns ({} messages)",
        drop_count, drop_to
    );
    *messages = messages[drop_to..].to_vec();
}

/// Keep at most the last `max_chars` bytes worth of `text`, cutting at a
/// UTF-8 character boundary and marking what was omitted.
pub fn tail_chars(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let mut start = text.len() - max_chars;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[...earlier conversation omitted...]\n{}", &text[start..])
}
