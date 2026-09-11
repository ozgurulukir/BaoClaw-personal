pub mod strategies;

use futures::StreamExt;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

pub use strategies::{
    adaptive_keep_recent, adjust_compact_split, micro_compact, reactive_compact,
    record_compact_feedback, session_memory_compact, tail_chars,
};

use crate::api::client::{ApiStreamEvent, CreateMessageRequest};
use crate::engine::api_builder::build_api_request;
use crate::engine::query_engine::{
    format_messages_for_summary, EngineError, EngineEvent, QueryLoopConfig,
};
use crate::models::message::{Message, MessageContent};

/// Compact messages in-place: summarize old messages via API and replace with a boundary.
pub async fn compact_messages(
    messages: &mut Vec<Message>,
    tx: mpsc::Sender<EngineEvent>,
    config: &QueryLoopConfig,
    keep_recent: usize,
) -> Result<(), EngineError> {
    if messages.len() <= keep_recent {
        return Ok(());
    }

    let old_count = adjust_compact_split(messages, messages.len() - keep_recent);
    let old_messages: Vec<Message> = messages[..old_count].to_vec();
    let recent_messages: Vec<Message> = messages[old_count..].to_vec();

    let raw_summary = format_messages_for_summary(&old_messages);
    let max_summary_chars: usize = 60_000;
    let truncated_summary = if raw_summary.len() > max_summary_chars {
        format!(
            "{}...\n\n[Conversation truncated, {} total chars]",
            raw_summary
                .chars()
                .take(max_summary_chars)
                .collect::<String>(),
            raw_summary.len()
        )
    } else {
        raw_summary
    };
    let summary_instruction = format!(
        "Summarize the following conversation history concisely, \
         preserving key context, decisions, and file changes:\n\n{}",
        truncated_summary
    );

    let main_request = build_api_request(messages, config);
    let old_api_messages: Vec<serde_json::Value> = old_messages
        .iter()
        .filter_map(|msg| match &msg.content {
            MessageContent::User { message, .. } => Some(serde_json::json!({
                "role": message.role,
                "content": message.content,
            })),
            MessageContent::Assistant { message, .. } => {
                let content_value =
                    serde_json::to_value(&message.content).unwrap_or(Value::Array(vec![]));
                Some(serde_json::json!({
                    "role": message.role,
                    "content": content_value,
                }))
            }
            _ => None,
        })
        .collect();
    let request = CreateMessageRequest::for_cache_safe_compaction(
        &main_request,
        &old_api_messages,
        &summary_instruction,
    );

    let stream_result = config.api_client.create_message_stream(request).await;
    let mut stream = match stream_result {
        Ok(s) => s,
        Err(e) => {
            return Err(EngineError {
                code: "compact_failed".to_string(),
                message: format!("Failed to call summary API: {}", e),
                details: None,
            });
        }
    };

    let mut summary_text = String::new();
    loop {
        let event_result = tokio::select! {
            r = stream.next() => r,
            _ = crate::engine::wait_for_abort(config.abort_rx.clone()) => {
                eprintln!("Compact aborted by user");
                return Err(EngineError {
                    code: "compact_aborted".to_string(),
                    message: "User aborted compaction".to_string(),
                    details: None,
                });
            }
        };
        let Some(event_result) = event_result else {
            break;
        };
        match event_result {
            Ok(event) => {
                if let ApiStreamEvent::ContentBlockDelta { delta, .. } = event {
                    if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                        summary_text.push_str(text);
                    }
                }
            }
            Err(e) => {
                eprintln!("Compact: stream error: {}", e);
                break;
            }
        }
    }

    if summary_text.trim().is_empty() {
        summary_text = format!(
            "[Previous conversation ({} messages) was compacted]",
            old_count
        );
    }

    let boundary = Message {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        content: MessageContent::System {
            subtype: crate::models::message::SystemSubtype::CompactBoundary,
            content: summary_text,
        },
    };

    *messages = vec![boundary];
    messages.extend(recent_messages);

    let _ = tx.send(EngineEvent::Progress {
        tool_use_id: String::new(),
        data: serde_json::json!({"message": format!("Context compacted: {} messages summarized", old_count)}),
    }).await;

    Ok(())
}

/// Compact on context window overflow and retry the query.
pub async fn context_overflow_compact(
    messages: &mut Vec<Message>,
    tx: &mpsc::Sender<EngineEvent>,
    config: &QueryLoopConfig,
) {
    eprintln!("Context window exceeded, auto-compacting...");
    let _ = tx
        .send(EngineEvent::AssistantChunk {
            content: "🗜️ Context window full — auto-compacting conversation history...\n"
                .to_string(),
            tool_use_id: None,
        })
        .await;

    if let Some(last) = messages.last() {
        if matches!(&last.content, MessageContent::Assistant { .. }) {
            messages.pop();
        }
    }
    let user_msg = messages.pop();

    let keep_recent: usize =
        adaptive_keep_recent(&config.adaptive_compact).min(messages.len().saturating_sub(2).max(1));
    if messages.len() > keep_recent {
        let split = adjust_compact_split(messages, messages.len() - keep_recent);

        let old_messages = &messages[..split];
        let summary_prompt = format!(
            "Summarize the following conversation history concisely, \
             preserving key context, decisions, and file changes:\n\n{}",
            format_messages_for_summary(old_messages)
        );
        let summary_request = CreateMessageRequest {
            model: config.model.clone(),
            messages: vec![serde_json::json!({
                "role": "user",
                "content": summary_prompt,
            })],
            system: Some(vec![serde_json::json!({
                "type": "text",
                "text": "You are a conversation summariser. Produce a concise summary.",
            })]),
            tools: None,
            max_tokens: 4096,
            stream: true,
            thinking: None,
            metadata: None,
        };
        let compact_abort_rx = config.abort_rx.clone();
        let compact_api_client = Arc::clone(&config.api_client);
        let summary_result = async move {
            let mut stream = compact_api_client
                .create_message_stream(summary_request)
                .await
                .map_err(|e| format!("{}", e))?;
            let mut text = String::new();
            let abort_rx = compact_abort_rx;
            loop {
                let event_result = tokio::select! {
                    r = stream.next() => r,
                    _ = crate::engine::wait_for_abort(abort_rx.clone()) => {
                        eprintln!("Aborted during compact summary streaming");
                        break;
                    }
                };
                let Some(event_result) = event_result else {
                    break;
                };
                match event_result {
                    Ok(ApiStreamEvent::ContentBlockDelta { delta, .. }) => {
                        if let Some(t) = delta.get("text").and_then(|v| v.as_str()) {
                            text.push_str(t);
                        }
                    }
                    Ok(ApiStreamEvent::MessageStop) => break,
                    Ok(ApiStreamEvent::Error { error }) => {
                        return Err(format!("{}: {}", error.error_type, error.message));
                    }
                    Err(e) => return Err(format!("{}", e)),
                    _ => {}
                }
            }
            Ok::<String, String>(text)
        }
        .await;

        match summary_result {
            Ok(summary_text) if !summary_text.is_empty() => {
                let recent = messages[split..].to_vec();
                messages.clear();
                messages.push(Message {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    content: MessageContent::System {
                        subtype: crate::models::message::SystemSubtype::CompactBoundary,
                        content: summary_text,
                    },
                });
                messages.extend(recent);
                eprintln!("Auto-compact done, {} messages remaining", messages.len());
            }
            Ok(_) | Err(_) => {
                eprintln!("Auto-compact summary failed, truncating instead");
                let recent = messages[split..].to_vec();
                messages.clear();
                messages.extend(recent);

                if messages.len() > 10 {
                    reactive_compact(messages, None);
                }
            }
        }
    }

    if let Some(msg) = user_msg {
        messages.push(msg);
    }
    let _ = tx
        .send(EngineEvent::AssistantChunk {
            content: "✅ Compaction complete — retrying...\n\n".to_string(),
            tool_use_id: None,
        })
        .await;
}
