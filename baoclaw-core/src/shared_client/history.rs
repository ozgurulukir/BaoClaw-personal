use std::path::PathBuf;
use std::sync::Arc;

use baoclaw_core::engine;
use baoclaw_core::ipc::protocol::RequestId;

use super::WriterRef;
use crate::{SharedSession, SharedState};

pub(super) async fn scm_talk_tail(
    session: &Arc<SharedSession>,
    writer: WriterRef<'_>,
    id: RequestId,
    count: usize,
) {
    let engine = session.engine_read().await;
    let messages = engine.get_messages();
    let start = if messages.len() > count {
        messages.len() - count
    } else {
        0
    };
    // Collect tool results from user messages to attach to tool_use blocks
    let tool_results: std::collections::HashMap<String, serde_json::Value> = messages
        .iter()
        .filter_map(|m| match &m.content {
            crate::models::message::MessageContent::User {
                tool_use_result, ..
            } => tool_use_result
                .as_ref()
                .map(|r| (r.tool_use_id.clone(), r.output.clone())),
            _ => None,
        })
        .collect();
    let tail: Vec<serde_json::Value> = messages[start..]
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            crate::ipc::message_format::message_to_tail_entry(
                m,
                start + idx + 1,
                &tool_results,
                crate::ipc::message_format::TailEntryOptions {
                    include_tool_result_fields: true,
                    include_tool_results: true,
                    include_rich_tool_details: true,
                    include_assistant_metadata: true,
                },
            )
        })
        .collect();
    let total = messages.len();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "messages": tail,
                "count": tail.len(),
                "total": total,
            }),
        )
        .await;
}

pub(super) async fn scm_search_history(
    session: &Arc<SharedSession>,
    writer: WriterRef<'_>,
    id: RequestId,
    query: String,
    max_results: usize,
) {
    // Search across all sessions using CrossSessionDb (FTS5)
    let mut results: Vec<serde_json::Value> = Vec::new();
    match engine::cross_session_db::CrossSessionDb::new() {
        Ok(db) => {
            let hits = db.search_with_context(&query, max_results);
            for hit in hits {
                results.push(serde_json::json!({
                    "snippet": hit.snippet,
                    "timestamp": hit.timestamp,
                    "session_id": hit.session_id,
                    "cwd": hit.cwd,
                    "rank": hit.rank,
                }));
            }
        }
        Err(e) => {
            eprintln!(
                "CrossSessionDb error: {}, falling back to in-memory search",
                e
            );
            // Fallback: search current session only
            let engine = session.engine_read().await;
            let messages = engine.get_messages();
            let query_lower = query.to_lowercase();
            for m in messages.iter().rev() {
                if results.len() >= max_results {
                    break;
                }
                let (role, text) = match &m.content {
                    crate::models::message::MessageContent::User { message, .. } => {
                        let t = match &message.content {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Array(arr) => arr
                                .iter()
                                .filter_map(|b| {
                                    b.get("text").and_then(|t| t.as_str()).map(String::from)
                                })
                                .collect::<Vec<_>>()
                                .join(" "),
                            _ => String::new(),
                        };
                        ("user", t)
                    }
                    crate::models::message::MessageContent::Assistant { message, .. } => {
                        let t: String = message
                            .content
                            .iter()
                            .filter_map(|b| match b {
                                crate::models::message::ContentBlock::Text { text } => {
                                    Some(text.clone())
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        ("assistant", t)
                    }
                    _ => continue,
                };
                if text.to_lowercase().contains(&query_lower) {
                    let lower = text.to_lowercase();
                    let idx = lower.find(&query_lower).unwrap_or(0);
                    // Byte offsets from the lowercased copy can land
                    // mid-character in the original (multibyte text,
                    // case-expanding lowercase) — clamp to boundaries.
                    let mut start = idx.saturating_sub(50);
                    while start > 0 && !text.is_char_boundary(start) {
                        start -= 1;
                    }
                    let mut end = (idx + query.len() + 100).min(text.len());
                    while end < text.len() && !text.is_char_boundary(end) {
                        end += 1;
                    }
                    let snippet = &text[start..end];
                    results.push(serde_json::json!({
                        "role": role,
                        "text": text.chars().take(200).collect::<String>(),
                        "snippet": snippet,
                        "timestamp": m.timestamp,
                    }));
                }
            }
        }
    }

    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "results": results,
                "count": results.len(),
                "query": query,
            }),
        )
        .await;
}

pub(super) async fn scm_export(
    session: &Arc<SharedSession>,
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    output_path: Option<String>,
) {
    // Get conversation history (reuse TalkTail logic)
    let engine = session.engine_read().await;
    let messages = engine.get_messages();
    let tail: Vec<serde_json::Value> = messages
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            crate::ipc::message_format::message_to_tail_entry(
                m,
                idx + 1,
                &Default::default(),
                crate::ipc::message_format::TailEntryOptions {
                    include_tool_result_fields: false,
                    include_tool_results: false,
                    include_rich_tool_details: false,
                    include_assistant_metadata: false,
                },
            )
        })
        .collect();
    drop(engine);

    // Convert to ExportEntry and format
    let export_entries: Vec<engine::export::ExportEntry> = tail
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();

    if export_entries.is_empty() {
        let mut conn_guard = writer.lock().await;
        let _ = conn_guard
            .send_error(
                Some(id),
                -32000,
                "No conversation history in the current session".to_string(),
            )
            .await;
    } else {
        let markdown = engine::export::format_transcript_to_markdown(&export_entries);
        let file_path = output_path.unwrap_or_else(|| {
            work_cwd
                .join(engine::export::default_export_filename())
                .to_string_lossy()
                .to_string()
        });

        let mut conn_guard = writer.lock().await;
        match std::fs::write(&file_path, &markdown) {
            Ok(()) => {
                let _ = conn_guard
                    .send_response(
                        id,
                        serde_json::json!({
                            "file_path": file_path,
                            "message_count": export_entries.len(),
                            "size_bytes": markdown.len(),
                        }),
                    )
                    .await;
            }
            Err(e) => {
                let _ = conn_guard
                    .send_error(
                        Some(id),
                        -32000,
                        format!("Failed to write export file: {}", e),
                    )
                    .await;
            }
        }
    }
}

pub(super) async fn scm_session_tokens(
    session: &Arc<SharedSession>,
    shared: &SharedState,
    session_id: &str,
    writer: WriterRef<'_>,
    id: RequestId,
) {
    let engine = session.engine_read().await;
    let messages = engine.get_messages();
    let usage = engine.get_usage();
    let context_window = shared.baoclaw_config.context_window;
    let threshold_ratio = shared.baoclaw_config.auto_compact_threshold_ratio;
    let compact_threshold = (context_window as f64 * threshold_ratio) as u64;

    // Current estimated input tokens from TokenCounter
    let est_tokens = {
        let counter = engine.token_counter_arc();
        let counter_guard = counter.lock().await;
        counter_guard.estimate(messages)
    };

    let result = serde_json::json!({
        "session_id": session_id,
        "current_tokens": est_tokens,
        "context_window": context_window,
        "usage_percent": if context_window > 0 {
            est_tokens as f64 / context_window as f64 * 100.0
        } else { 0.0 },
        "compact_threshold": compact_threshold,
        "threshold_ratio": threshold_ratio,
        "tokens_until_compact": compact_threshold.saturating_sub(est_tokens),
        "total_input_tokens": usage.input_tokens,
        "total_output_tokens": usage.output_tokens,
        "cache_creation_tokens": usage.cache_creation_input_tokens.unwrap_or(0),
        "cache_read_tokens": usage.cache_read_input_tokens.unwrap_or(0),
        "message_count": messages.len(),
        "model": shared.baoclaw_config.model,
    });
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

pub(super) async fn scm_session_cost(
    session: &Arc<SharedSession>,
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
) {
    let engine = session.engine_read().await;
    let usage = engine.get_usage();
    let model = &shared.baoclaw_config.model;

    let cost_tracker = engine::cost_tracker::CostTracker::new();
    let session_cost = cost_tracker.calculate_cost(usage, model);

    // Per-million unit prices in USD (single pricing-map access).
    let (input_price, output_price) = cost_tracker.per_million_prices(model);

    let result = serde_json::json!({
        "session_cost_usd": session_cost,
        "total_input_tokens": usage.input_tokens,
        "total_output_tokens": usage.output_tokens,
        "input_cost": (usage.input_tokens as f64 / 1_000_000.0) * input_price,
        "output_cost": (usage.output_tokens as f64 / 1_000_000.0) * output_price,
        "input_price_per_mtok": input_price,
        "output_price_per_mtok": output_price,
        "model": model,
        "pricing_configured": true,
    });
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

pub(super) async fn scm_session_info(
    session: &Arc<SharedSession>,
    shared: &SharedState,
    session_id: &str,
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
) {
    let engine = session.engine_read().await;
    let msg_count = engine.get_messages().len();
    let client_count = session.client_count().await;
    let created_at = session.created_at().to_string();
    let last_active = session.last_active().await;

    let result = serde_json::json!({
        "session_id": session_id,
        "cwd": work_cwd.to_string_lossy(),
        "message_count": msg_count,
        "client_count": client_count,
        "model": shared.baoclaw_config.model,
        "created_at": created_at,
        "last_active": last_active,
    });
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}
