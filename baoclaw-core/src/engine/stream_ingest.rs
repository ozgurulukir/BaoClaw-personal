use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::api::client::ApiStreamEvent;
use crate::api::unified::UnifiedStream;
use crate::engine::cost_tracker::CostTracker;
use crate::engine::query_engine::{
    EngineError, EngineEvent, QueryLoopConfig, QueryResult, QueryStatus,
};
use crate::engine::tool_loop::accumulate_usage;
use crate::models::message::{ContentBlock, Message, MessageContent, Usage};

/// Content accumulated from one assistant turn's SSE stream.
pub struct IngestedTurn {
    pub content_blocks: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
}

/// Outcome of consuming one SSE stream. `Continue` yields the ingested
/// assistant turn; the other variants have already sent their terminal event
/// (`Result{Aborted}` or `Error`) and the loop must stop — the payload is
/// carried back so the loop can record a trajectory for the query.
pub enum StreamIngest {
    Continue(IngestedTurn),
    /// Mid-stream abort: a `Result{Aborted}` event was already sent.
    Aborted,
    /// Fatal failure: an Error event was already sent to `tx`.
    Fatal(EngineError),
}

/// Send the terminal `Result` event that ends a query.
#[allow(clippy::too_many_arguments)]
pub async fn send_terminal_result(
    tx: &mpsc::Sender<EngineEvent>,
    start_time: std::time::Instant,
    cost_tracker: &CostTracker,
    usage: Usage,
    num_turns: u32,
    status: QueryStatus,
    error: Option<EngineError>,
    text: Option<String>,
    stop_reason: Option<String>,
) {
    let _ = tx
        .send(EngineEvent::Result(QueryResult {
            status,
            error,
            text,
            stop_reason,
            total_cost_usd: cost_tracker.total_cost(),
            usage,
            num_turns,
            duration_ms: start_time.elapsed().as_millis() as u64,
        }))
        .await;
}

/// Ingest the SSE stream for one assistant turn, accumulating content blocks,
/// usage, and cost. On `Aborted`/`Fatal` the terminal event (`Result{Aborted}`
/// or `Error`) has already been sent to `tx`; the payload lets the caller
/// record a trajectory before returning.
#[allow(clippy::too_many_arguments)]
pub async fn ingest_stream_events(
    mut stream: UnifiedStream,
    messages: &mut Vec<Message>,
    tx: &mpsc::Sender<EngineEvent>,
    config: &QueryLoopConfig,
    total_usage: &mut Usage,
    cost_tracker: &mut CostTracker,
    turn_count: u32,
    start_time: std::time::Instant,
) -> StreamIngest {
    // Process SSE stream events, accumulating content blocks
    let mut assistant_content_blocks: Vec<ContentBlock> = Vec::new();
    let mut current_text = String::new();
    let mut current_tool_id = String::new();
    let mut current_tool_name = String::new();
    let mut current_tool_input_json = String::new();
    let mut current_thinking_text = String::new();
    let mut stop_reason: Option<String> = None;
    // Track what kind of block we're in: "text", "tool_use", "thinking", or ""
    let mut current_block_type = String::new();

    while let Some(event_result) = tokio::select! {
        result = stream.next() => result,
        // Event-driven abort: resolves immediately when abort fires,
        // vs the old 500ms polling loop.
        _ = crate::engine::wait_for_abort(config.abort_rx.clone()) => {
            eprintln!("Query aborted during stream processing");
            let fixed = crate::engine::cleanup_orphan_tool_uses(messages);
            if fixed > 0 {
                eprintln!("Cleaned up {} orphan tool_use block(s) after stream abort", fixed);
            }
            send_terminal_result(
                tx,
                start_time,
                cost_tracker,
                total_usage.clone(),
                turn_count,
                QueryStatus::Aborted,
                None,
                None,
                None,
            )
            .await;
            return StreamIngest::Aborted;
        }
    } {
        match event_result {
            Ok(event) => match event {
                ApiStreamEvent::ContentBlockStart { content_block, .. } => {
                    let block_type = content_block
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    current_block_type = block_type.to_string();
                    match block_type {
                        "text" => {
                            current_text = String::new();
                        }
                        "tool_use" => {
                            current_tool_id = content_block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            current_tool_name = content_block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            // Some APIs send the full input in content_block_start
                            // instead of streaming via input_json_delta. Pre-seed
                            // current_tool_input_json if a non-empty input is present.
                            current_tool_input_json = match content_block.get("input") {
                                Some(v)
                                    if v.is_object()
                                        && v.as_object().is_some_and(|o| !o.is_empty()) =>
                                {
                                    serde_json::to_string(v).unwrap_or_default()
                                }
                                _ => String::new(),
                            };
                        }
                        "thinking" => {
                            current_thinking_text = String::new();
                        }
                        _ => {}
                    }
                }
                ApiStreamEvent::ContentBlockDelta { delta, .. } => {
                    let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match delta_type {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                current_text.push_str(text);
                                // Emit AssistantChunk
                                let _ = tx
                                    .send(EngineEvent::AssistantChunk {
                                        content: text.to_string(),
                                        tool_use_id: None,
                                    })
                                    .await;
                            }
                        }
                        "input_json_delta" => {
                            if let Some(partial) =
                                delta.get("partial_json").and_then(|v| v.as_str())
                            {
                                current_tool_input_json.push_str(partial);
                            }
                        }
                        "thinking_delta" => {
                            if let Some(text) = delta.get("thinking").and_then(|v| v.as_str()) {
                                current_thinking_text.push_str(text);
                                // Emit ThinkingChunk to CLI
                                let _ = tx
                                    .send(EngineEvent::ThinkingChunk {
                                        content: text.to_string(),
                                    })
                                    .await;
                            }
                        }
                        _ => {}
                    }
                }
                ApiStreamEvent::ContentBlockStop { .. } => {
                    match current_block_type.as_str() {
                        "text" => {
                            if !current_text.is_empty() {
                                assistant_content_blocks.push(ContentBlock::Text {
                                    text: current_text.clone(),
                                });
                            }
                        }
                        "tool_use" => {
                            if current_tool_input_json.trim().is_empty() {
                                eprintln!("[WARN] tool_use '{}' (id={}) has empty input_json — model returned no arguments",
                                        current_tool_name, current_tool_id);
                            }
                            let input: Value = serde_json::from_str(&current_tool_input_json)
                                .unwrap_or(Value::Object(serde_json::Map::new()));
                            assistant_content_blocks.push(ContentBlock::ToolUse {
                                id: current_tool_id.clone(),
                                name: current_tool_name.clone(),
                                input: input.clone(),
                            });
                        }
                        "thinking" if !current_thinking_text.is_empty() => {
                            assistant_content_blocks.push(ContentBlock::Thinking {
                                thinking: current_thinking_text.clone(),
                            });
                        }
                        _ => {}
                    }
                    current_block_type.clear();
                }
                ApiStreamEvent::MessageDelta { delta, usage, .. } => {
                    if let Some(sr) = delta.get("stop_reason").and_then(|v| v.as_str()) {
                        stop_reason = Some(sr.to_string());
                    }
                    accumulate_usage(total_usage, &usage);
                    // Accumulate cost from message_delta usage
                    let delta_usage = Usage {
                        input_tokens: usage
                            .get("input_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                        output_tokens: usage
                            .get("output_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                        cache_creation_input_tokens: usage
                            .get("cache_creation_input_tokens")
                            .and_then(|v| v.as_u64()),
                        cache_read_input_tokens: usage
                            .get("cache_read_input_tokens")
                            .and_then(|v| v.as_u64()),
                    };
                    cost_tracker.accumulate(&delta_usage, &config.model);
                }
                ApiStreamEvent::MessageStart { message } => {
                    // Extract usage from message_start if present
                    if let Some(usage_val) = message.get("usage") {
                        accumulate_usage(total_usage, usage_val);
                        // Accumulate cost from message_start usage
                        let start_usage = Usage {
                            input_tokens: usage_val
                                .get("input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0),
                            output_tokens: usage_val
                                .get("output_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0),
                            cache_creation_input_tokens: usage_val
                                .get("cache_creation_input_tokens")
                                .and_then(|v| v.as_u64()),
                            cache_read_input_tokens: usage_val
                                .get("cache_read_input_tokens")
                                .and_then(|v| v.as_u64()),
                        };
                        cost_tracker.accumulate(&start_usage, &config.model);

                        // Calibrate the token counter against the real API-reported input_tokens.
                        // This anchors future estimates to the truth, so subsequent
                        // tiktoken-based deltas only need to count newly-added messages.
                        if start_usage.input_tokens > 0 {
                            let mut counter = config.token_counter.lock().await;
                            counter.calibrate(start_usage.input_tokens, messages.len());
                            if let Some(ref sid) = config.session_id {
                                counter.save_baseline(sid);
                            }
                        }
                    }
                }
                ApiStreamEvent::MessageStop => {
                    break;
                }
                ApiStreamEvent::Error { error } => {
                    let err = EngineError {
                        code: error.error_type,
                        message: error.message,
                        details: None,
                    };
                    let _ = tx.send(EngineEvent::Error(err.clone())).await;
                    return StreamIngest::Fatal(err);
                }
                ApiStreamEvent::Ping => {}
            },
            Err(e) => {
                // Stream error — clean up: if no assistant content was accumulated,
                // remove the user message to keep history valid
                if assistant_content_blocks.is_empty() {
                    if let Some(last) = messages.last() {
                        if matches!(&last.content, MessageContent::User { .. }) {
                            messages.pop();
                        }
                    }
                }
                let err = EngineError {
                    code: "stream_error".to_string(),
                    message: format!("{}", e),
                    details: None,
                };
                let _ = tx.send(EngineEvent::Error(err.clone())).await;
                return StreamIngest::Fatal(err);
            }
        }
    }

    StreamIngest::Continue(IngestedTurn {
        content_blocks: assistant_content_blocks,
        stop_reason,
    })
}
