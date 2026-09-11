use serde_json::Value;

use crate::engine::tool_loop::extract_tool_result_ids;
use crate::models::message::{ContentBlock, Message, MessageContent};

/// Global scan of a message history: all `tool_use` ids carried by assistant
/// messages and all `tool_result` ids carried by user messages. The single
/// source of "what pairs with what" for both the persisted-history cleanup
/// and the wire-shaping pass.
pub fn collect_tool_ids(
    messages: &[Message],
) -> (
    std::collections::HashSet<String>,
    std::collections::HashSet<String>,
) {
    let mut tool_use_ids = std::collections::HashSet::new();
    let mut tool_result_ids = std::collections::HashSet::new();
    for msg in messages.iter() {
        match &msg.content {
            MessageContent::Assistant { message, .. } => {
                for block in &message.content {
                    if let ContentBlock::ToolUse { id, .. } = block {
                        tool_use_ids.insert(id.clone());
                    }
                }
            }
            MessageContent::User { message, .. } => {
                for id in extract_tool_result_ids(message) {
                    tool_result_ids.insert(id);
                }
            }
            _ => {}
        }
    }
    (tool_use_ids, tool_result_ids)
}

/// The single "keep this block?" rule for orphan stripping: non-tool_result
/// blocks are always kept; a `tool_result` is kept only when its id is NOT
/// orphaned. Shared by the persisted-history cleanup (Pass 0) and the
/// wire-shaping pass so the two can never disagree on what an orphan is.
pub fn is_retained_block(block: &Value, orphans: &std::collections::HashSet<String>) -> bool {
    block.get("type").and_then(|v| v.as_str()) != Some("tool_result")
        || block
            .get("tool_use_id")
            .and_then(|v| v.as_str())
            .is_none_or(|id| !orphans.contains(id))
}

/// Remove `tool_result` blocks whose `tool_use_id` is in `orphans` from every
/// user message, and drop user messages whose content array becomes empty.
/// Returns the number of blocks removed. Mutates in place.
pub fn strip_orphan_tool_result_blocks(
    messages: &mut Vec<Message>,
    orphans: &std::collections::HashSet<String>,
) -> usize {
    let mut removed = 0usize;
    let mut i = 0;
    while i < messages.len() {
        let mut emptied = false;
        if let MessageContent::User { message, .. } = &mut messages[i].content {
            if let Value::Array(arr) = &mut message.content {
                let before = arr.len();
                arr.retain(|block| is_retained_block(block, orphans));
                removed += before - arr.len();
                emptied = arr.is_empty();
            }
        }
        if emptied {
            messages.remove(i);
        } else {
            i += 1;
        }
    }
    removed
}

/// Validate and fix tool_use/tool_result pairing in messages before API call.
/// This ensures we never send malformed messages to the API.
pub fn validate_and_fix_tool_messages(messages: &[Message]) -> Vec<Message> {
    eprintln!("=== validate_and_fix_tool_messages: START ===");
    eprintln!("  Input messages: {}", messages.len());

    // First pass: collect all tool_use IDs and their corresponding tool_result IDs
    let (tool_use_ids, tool_result_ids) = collect_tool_ids(messages);

    eprintln!(
        "  Found {} tool_use IDs: {:?}",
        tool_use_ids.len(),
        tool_use_ids
    );
    eprintln!(
        "  Found {} tool_result IDs: {:?}",
        tool_result_ids.len(),
        tool_result_ids
    );

    // Find orphaned tool_use IDs (without corresponding tool_result)
    let orphaned_tool_uses: std::collections::HashSet<String> =
        tool_use_ids.difference(&tool_result_ids).cloned().collect();

    // Find orphaned tool_result IDs (without corresponding tool_use)
    let orphaned_tool_results: std::collections::HashSet<String> =
        tool_result_ids.difference(&tool_use_ids).cloned().collect();

    eprintln!("  Orphaned tool_use blocks: {}", orphaned_tool_uses.len());
    eprintln!(
        "  Orphaned tool_result blocks: {}",
        orphaned_tool_results.len()
    );

    // Filter messages: remove those with orphaned tool_use/tool_result
    let mut result = Vec::new();
    for msg in messages {
        match &msg.content {
            MessageContent::System { .. } => {
                // Skip system messages (CompactBoundary etc.)
                continue;
            }
            MessageContent::Assistant { message, .. } => {
                // Check if this assistant message contains any orphaned tool_use
                let has_orphaned = message.content.iter().any(|block| {
                    if let ContentBlock::ToolUse { id, .. } = block {
                        orphaned_tool_uses.contains(id)
                    } else {
                        false
                    }
                });
                if !has_orphaned {
                    result.push(msg.clone());
                } else {
                    eprintln!(
                        "validate_and_fix: skipping assistant message with orphaned tool_use"
                    );
                }
            }
            MessageContent::User { message, .. } => {
                // Check if this user message contains only orphaned tool_result
                let result_ids = extract_tool_result_ids(message);
                if result_ids.is_empty() {
                    // Regular user message, keep it
                    result.push(msg.clone());
                } else {
                    // Tool result message - keep the valid blocks (text and
                    // paired results), strip the orphaned ones. An orphan
                    // reaching the wire is a 400.
                    let has_retained = match &message.content {
                        Value::Array(arr) => arr
                            .iter()
                            .any(|block| is_retained_block(block, &orphaned_tool_results)),
                        _ => true,
                    };
                    if has_retained {
                        let mut fixed = msg.clone();
                        if let MessageContent::User { message, .. } = &mut fixed.content {
                            if let Value::Array(arr) = &mut message.content {
                                arr.retain(|block| {
                                    is_retained_block(block, &orphaned_tool_results)
                                });
                            }
                        }
                        result.push(fixed);
                    } else {
                        eprintln!(
                            "validate_and_fix: skipping user message with only orphaned tool_result"
                        );
                    }
                }
            }
            _ => {
                result.push(msg.clone());
            }
        }
    }

    eprintln!("  Output messages: {}", result.len());
    eprintln!("=== validate_and_fix_tool_messages: END ===");

    result
}
