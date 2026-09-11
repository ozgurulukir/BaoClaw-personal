use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::api::client::{ApiStreamEvent, CreateMessageRequest};
use crate::api::fallback::FallbackController;
use crate::api::unified::UnifiedClient;
use crate::config::BaoclawConfig;
use crate::engine::cost_tracker::CostTracker;
use crate::engine::git_info::get_git_info_async;
use crate::engine::session_memory::SessionMemory;
use crate::engine::token_counter::BudgetStatus;
use crate::engine::transcript::{TranscriptEntry, TranscriptEntryType, TranscriptWriter};
use crate::models::message::{
    ApiAssistantMessage, ApiUserMessage, ContentBlock, Message, MessageContent, Usage,
};
use crate::tools::executor::{execute_tools, ToolUseRequest};
use crate::tools::trait_def::ToolContext;

use crate::engine::query_engine::{
    estimate_tokens, format_messages_for_summary, EngineError, EngineEvent, NoopProgressSender,
    QueryLoopConfig, QueryStatus, EMPTY_USAGE,
};
use crate::engine::tool_loop::{build_tool_result_message, extract_text, extract_tool_uses};

/// How many times an empty stream (turn ended with zero content blocks) is
/// retried before the query fails with an `empty_stream` error instead of
/// reporting a silent, text-less success.
const MAX_EMPTY_STREAM_RETRIES: u32 = 2;

/// How many times a `model_context_window_exceeded` turn is retried after a
/// compaction before the query fails with a `context_overflow` error.
/// Guards against a compaction that made no progress (e.g. the whole
/// history fits inside the keep-recent window).
const MAX_OVERFLOW_COMPACT_RETRIES: u32 = 2;

/// Append a transcript entry; failures are logged, never fatal
/// (a broken transcript must not take down the query loop).
async fn append_transcript(writer: &mut Option<TranscriptWriter>, entry: &TranscriptEntry) {
    if let Some(w) = writer.as_mut() {
        if let Err(e) = w.append(entry).await {
            eprintln!(
                "[transcript] WARNING: append failed: {} (entry type: {:?})",
                e, entry.entry_type
            );
        }
    }
}

pub use crate::engine::stream_ingest::send_terminal_result;

pub async fn run_query_loop(
    messages: &mut Vec<Message>,
    mut config: QueryLoopConfig,
    tx: mpsc::Sender<EngineEvent>,
) {
    let start_time = std::time::Instant::now();
    // Trajectory recording: the prompt that started this query (last
    // non-tool-result user message) plus the tool actions across turns.
    let traj_prompt: String = messages
        .iter()
        .rev()
        .find_map(|m| match &m.content {
            MessageContent::User {
                message,
                tool_use_result: None,
                ..
            } => match &message.content {
                serde_json::Value::String(s) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        })
        .unwrap_or_default();
    let mut traj_actions: Vec<crate::engine::evolution::TrajectoryAction> = Vec::new();
    let mut turn_count = 0u32;
    let mut total_usage = EMPTY_USAGE;
    let mut cost_tracker = CostTracker::new();
    cost_tracker.reset_query();

    // Iteration budget pressure tracking (70/90/100 gradient)
    let mut budget_warned_70: bool = false;
    let mut budget_warned_90: bool = false;

    // Cost/token budget: the turn at which the grace call was injected
    // (None = not yet hit). Hard stop happens once a full turn has elapsed
    // past the grace point with the budget still exceeded.
    let mut budget_grace_turn: Option<u32> = None;

    // Consecutive empty-stream retries (see MAX_EMPTY_STREAM_RETRIES).
    let mut empty_stream_retries: u32 = 0;
    // Consecutive context-overflow compaction retries (see
    // MAX_OVERFLOW_COMPACT_RETRIES).
    let mut overflow_retries: u32 = 0;

    // Per-turn tracking for TurnStart/TurnEnd events
    let mut turn_id_counter: u32 = 0;
    let mut turn_start_time;
    let mut turn_tool_count: u32;
    let mut turn_input_tokens_at_start: u64;
    let mut turn_output_tokens_at_start: u64;

    // Open transcript writer if session_id is available
    let mut transcript_writer = match config.session_id.as_ref() {
        Some(sid) => match TranscriptWriter::open(sid).await {
            Ok(w) => Some(w),
            Err(e) => {
                eprintln!("[transcript] WARNING: could not open transcript for session {}: {} — transcript will be missing", sid, e);
                None
            }
        },
        None => None,
    };

    // Open cross-session DB for indexing (errors are non-fatal)
    let cross_db = crate::engine::cross_session_db::CrossSessionDb::new().ok();

    // Stub the session row before message indexing: index_message has a
    // foreign key on sessions(id), so without this row every insert fails.
    // The full summary is upserted again (ON CONFLICT, preserving the
    // original started_at) when the session closes.
    if let (Some(ref db), Some(ref sid)) = (&cross_db, &config.session_id) {
        let now = chrono::Utc::now().to_rfc3339();
        let summary = crate::engine::cross_session_db::SessionIndex {
            id: sid.clone(),
            cwd: config.cwd.to_string_lossy().to_string(),
            model: config.model.clone(),
            started_at: now.clone(),
            ended_at: now,
            turn_count: 0,
            cost_usd: 0.0,
        };
        if let Err(e) = db.index_session(summary) {
            eprintln!("[cross-session] WARNING: session not indexed: {}", e);
        }
    }

    // Write the user message that was just added (last message in the vec)
    if let Some(last_msg) = messages.last() {
        append_transcript(
            &mut transcript_writer,
            &TranscriptEntry {
                timestamp: last_msg.timestamp.clone(),
                entry_type: TranscriptEntryType::UserMessage,
                data: serde_json::to_value(last_msg).unwrap_or_default(),
            },
        )
        .await;
        // Index user message for cross-session search
        if let (Some(ref db), Some(ref sid)) = (&cross_db, &config.session_id) {
            if let MessageContent::User { message, .. } = &last_msg.content {
                let text = match &message.content {
                    serde_json::Value::String(s) => s.clone(),
                    _ => serde_json::to_string(&message.content).unwrap_or_default(),
                };
                if let Err(e) = db.index_message(sid, "user", &text, &last_msg.timestamp) {
                    eprintln!("[cross-session] WARNING: user message not indexed: {}", e);
                }
            }
        }
    }

    // Build FallbackController from config
    let fallback_config = BaoclawConfig {
        model: config.model.clone(),
        fallback_models: config.fallback_models.clone(),
        max_retries_per_model: config.max_retries_per_model,
        context_window: config.context_window,
        auto_compact_threshold_ratio: config.auto_compact_threshold_ratio,
        // The executor reads this cap through the process-wide accessor, so
        // the fallback controller must see the initialized value, not the
        // struct default.
        tool_output_threshold_chars: crate::config::tool_output_threshold(),
        ..Default::default()
    };
    let mut fallback_controller = FallbackController::new(&fallback_config);

    loop {
        // Emit TurnStart immediately — user sees "Turn N" without any delay
        turn_id_counter += 1;
        turn_start_time = std::time::Instant::now();
        turn_tool_count = 0;
        turn_input_tokens_at_start = total_usage.input_tokens;
        turn_output_tokens_at_start = total_usage.output_tokens;
        let _ = tx
            .send(EngineEvent::TurnStart {
                turn_id: turn_id_counter,
                parent_turn_id: config.parent_turn_id,
                agent_label: config.agent_label.clone(),
            })
            .await;

        // Check abort (after TurnStart so CLI can handle unmatched TurnStart)
        if config.is_aborted() {
            // Clean up any orphan tool_use blocks before returning, so the
            // message history stays API-legal for the next query.
            let fixed = crate::engine::cleanup_orphan_tool_uses(messages);
            if fixed > 0 {
                eprintln!("Cleaned up {} orphan tool_use block(s) after abort", fixed);
            }
            record_query_trajectory(
                &config,
                &traj_prompt,
                std::mem::take(&mut traj_actions),
                crate::engine::evolution::TrajectoryOutcome::Aborted,
                start_time.elapsed().as_millis() as u64,
            )
            .await;
            send_terminal_result(
                &tx,
                start_time,
                &cost_tracker,
                total_usage,
                turn_count,
                QueryStatus::Aborted,
                None,
                None,
                None,
            )
            .await;
            return;
        }

        // ── Cost/token budget enforcement ──
        // First hit injects a final-answer request. The grace call is
        // tracked by turn, not by a flag, so transient API retries
        // (rate limit, fallback) don't consume it; once a full turn has
        // elapsed past the grace point with the budget still exceeded,
        // the query stops with a budget_exceeded error.
        {
            let tokens_used = total_usage
                .input_tokens
                .saturating_add(total_usage.output_tokens);
            let cost_hit = config
                .max_budget_usd
                .is_some_and(|b| cost_tracker.current_query_cost() >= b);
            let token_hit = config.max_tokens_budget.is_some_and(|t| tokens_used >= t);
            if cost_hit || token_hit {
                if budget_grace_turn.is_some_and(|grace| turn_count > grace) {
                    let which = if cost_hit && token_hit {
                        "cost and token budgets"
                    } else if cost_hit {
                        "cost budget"
                    } else {
                        "token budget"
                    };
                    let err = EngineError {
                        code: "budget_exceeded".to_string(),
                        message: format!(
                            "Query {} reached: stopped after {} turn(s), ${:.4}, {} tokens",
                            which,
                            turn_count,
                            cost_tracker.current_query_cost(),
                            tokens_used
                        ),
                        details: None,
                    };
                    let _ = tx.send(EngineEvent::Error(err.clone())).await;
                    record_query_trajectory(
                        &config,
                        &traj_prompt,
                        std::mem::take(&mut traj_actions),
                        crate::engine::evolution::TrajectoryOutcome::Error {
                            code: err.code.clone(),
                            message: err.message.clone(),
                        },
                        start_time.elapsed().as_millis() as u64,
                    )
                    .await;
                    send_terminal_result(
                        &tx,
                        start_time,
                        &cost_tracker,
                        total_usage,
                        turn_count,
                        QueryStatus::Error,
                        Some(err),
                        None,
                        None,
                    )
                    .await;
                    return;
                }
                if budget_grace_turn.is_none() {
                    eprintln!(
                        "⚠ Budget limit reached (${:.4}, {} tokens) — forcing final response",
                        cost_tracker.current_query_cost(),
                        tokens_used
                    );
                    budget_grace_turn = Some(turn_count);
                    messages.push(Message {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        content: MessageContent::User {
                            message: ApiUserMessage {
                                role: "user".to_string(),
                                content: Value::String(
                                    "[System: Resource budget EXHAUSTED (cost/token limit). You MUST produce your final response NOW. Do NOT use any tools.]"
                                        .to_string(),
                                ),
                            },
                            is_meta: false,
                            tool_use_result: None,
                        },
                    });
                }
            }
        }

        // ── Git info refresh (non-blocking after first turn) ──
        // First turn or every 10th turn: refresh git info.
        // Other turns: use cached value to save ~30-50ms on TTFB.
        if turn_count == 0 || turn_count.is_multiple_of(10) {
            if let Some(fresh_git) = get_git_info_async(&config.cwd).await {
                config.git_info = Some(fresh_git);
            }
        }

        // ── Iteration budget pressure gradient (70% warn → 90% urgent → 100% grace call) ──
        inject_iteration_budget_warnings(
            messages,
            turn_count,
            config.max_turns,
            &mut budget_warned_70,
            &mut budget_warned_90,
        );

        // 100%: Grace call — allow exactly one more API call for final summary
        if let Some(max) = config.max_turns {
            if turn_count >= max {
                eprintln!(
                    "⚠ Iteration budget reached ({}/{}) — forcing final response",
                    turn_count, max
                );
                // Don't return immediately — let the loop continue for ONE final API call
                // The loop will exit after this because the model won't produce tool_use blocks
                // when told to produce a final answer.
                // If the model still tries tool_use, the next iteration will hit >= max again
                // and we return MaxTurns.
                if turn_count > max {
                    // Safety: second time hitting the limit, hard stop
                    record_query_trajectory(
                        &config,
                        &traj_prompt,
                        std::mem::take(&mut traj_actions),
                        crate::engine::evolution::TrajectoryOutcome::MaxTurns,
                        start_time.elapsed().as_millis() as u64,
                    )
                    .await;
                    send_terminal_result(
                        &tx,
                        start_time,
                        &cost_tracker,
                        total_usage,
                        turn_count,
                        QueryStatus::MaxTurns,
                        None,
                        None,
                        None,
                    )
                    .await;
                    return;
                }
                // First time hitting limit: inject final-answer instruction and let one more API call happen
                messages.push(Message {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    content: MessageContent::User {
                        message: ApiUserMessage {
                            role: "user".to_string(),
                            content: Value::String(
                                "[System: Iteration budget EXHAUSTED. You MUST produce your final response NOW. Do NOT use any tools.]".to_string()
                            ),
                        },
                        is_meta: false,
                        tool_use_result: None,
                    },
                });
            }
        }

        // ── Micro-compact: clear old, large tool results (config knobs) ──
        micro_compact(messages, config.micro_compact);

        // ── Multi-level budget check ──
        // Use pre-computed budget from submit_message_with_attachments on first turn
        // to avoid redundant lock + tiktoken estimation.
        let (budget_status, current_tokens) =
            compute_token_budget(messages, turn_count, &mut config).await;
        enforce_token_budget(messages, &tx, &mut config, budget_status, current_tokens).await;

        // Call LLM API (streaming) with rate-limit fallback handling and timeout
        let stream = match call_api_with_fallback(
            messages,
            &tx,
            &mut config,
            &mut fallback_controller,
            current_tokens,
        )
        .await
        {
            ApiCallOutcome::Stream(s) => s,
            ApiCallOutcome::Retry => continue, // retry the loop
            ApiCallOutcome::Fatal(err) => {
                // Record the failed query so evolution/rating sees it too.
                record_query_trajectory(
                    &config,
                    &traj_prompt,
                    std::mem::take(&mut traj_actions),
                    crate::engine::evolution::TrajectoryOutcome::Error {
                        code: err.code.clone(),
                        message: err.message.clone(),
                    },
                    start_time.elapsed().as_millis() as u64,
                )
                .await;
                // Terminal Result mirroring the Error event — downstream
                // layers key off Result to report failure (an Error event
                // alone used to end the turn as an empty success).
                send_terminal_result(
                    &tx,
                    start_time,
                    &cost_tracker,
                    total_usage,
                    turn_count,
                    QueryStatus::Error,
                    Some(err),
                    None,
                    None,
                )
                .await;
                return;
            }
        };

        // Process SSE stream events, accumulating content blocks
        let turn = match ingest_stream_events(
            stream,
            messages,
            &tx,
            &config,
            &mut total_usage,
            &mut cost_tracker,
            turn_count,
            start_time,
        )
        .await
        {
            StreamIngest::Continue(turn) => turn,
            StreamIngest::Aborted => {
                // Parity with the loop-top abort: record before returning.
                record_query_trajectory(
                    &config,
                    &traj_prompt,
                    std::mem::take(&mut traj_actions),
                    crate::engine::evolution::TrajectoryOutcome::Aborted,
                    start_time.elapsed().as_millis() as u64,
                )
                .await;
                return;
            }
            StreamIngest::Fatal(err) => {
                // Record the failed query so evolution/rating sees it too.
                record_query_trajectory(
                    &config,
                    &traj_prompt,
                    std::mem::take(&mut traj_actions),
                    crate::engine::evolution::TrajectoryOutcome::Error {
                        code: err.code.clone(),
                        message: err.message.clone(),
                    },
                    start_time.elapsed().as_millis() as u64,
                )
                .await;
                // Terminal Result mirroring the Error event (see the
                // ApiCallOutcome::Fatal arm above).
                send_terminal_result(
                    &tx,
                    start_time,
                    &cost_tracker,
                    total_usage,
                    turn_count,
                    QueryStatus::Error,
                    Some(err),
                    None,
                    None,
                )
                .await;
                return;
            }
        };
        let IngestedTurn {
            content_blocks: assistant_content_blocks,
            stop_reason,
        } = turn;
        if !assistant_content_blocks.is_empty() {
            empty_stream_retries = 0;
            overflow_retries = 0;
        }

        // Record the assistant turn in history, transcript, and cross-session index
        record_assistant_turn(
            messages,
            &assistant_content_blocks,
            &stop_reason,
            &config,
            &mut transcript_writer,
            &cross_db,
            &tx,
            &cost_tracker,
            &total_usage,
        )
        .await;

        // Check for tool_use blocks
        let tool_uses = extract_tool_uses(&assistant_content_blocks);

        if tool_uses.is_empty() {
            // Check for context window exceeded — auto-compact and retry
            if stop_reason.as_deref() == Some("model_context_window_exceeded") {
                if overflow_retries >= MAX_OVERFLOW_COMPACT_RETRIES {
                    let err = EngineError {
                        code: "context_overflow".to_string(),
                        message: String::from(
                            "Context window still exceeded after repeated compactions",
                        ),
                        details: None,
                    };
                    let _ = tx.send(EngineEvent::Error(err.clone())).await;
                    record_query_trajectory(
                        &config,
                        &traj_prompt,
                        std::mem::take(&mut traj_actions),
                        crate::engine::evolution::TrajectoryOutcome::Error {
                            code: err.code.clone(),
                            message: err.message.clone(),
                        },
                        start_time.elapsed().as_millis() as u64,
                    )
                    .await;
                    send_terminal_result(
                        &tx,
                        start_time,
                        &cost_tracker,
                        total_usage,
                        turn_count,
                        QueryStatus::Error,
                        Some(err),
                        None,
                        stop_reason,
                    )
                    .await;
                    return;
                }
                overflow_retries += 1;
                context_overflow_compact(messages, &tx, &config).await;
                continue; // retry the query loop
            }

            // Genuinely empty stream: the turn ended with no content blocks
            // at all. Retry a couple of times (transient provider glitch),
            // then fail the query instead of reporting a silent success.
            if assistant_content_blocks.is_empty() {
                if empty_stream_retries < MAX_EMPTY_STREAM_RETRIES {
                    empty_stream_retries += 1;
                    // Drop the empty assistant turn just recorded so the
                    // history stays clean for the retry.
                    if let Some(last) = messages.last() {
                        if matches!(&last.content, MessageContent::Assistant { .. }) {
                            messages.pop();
                        }
                    }
                    eprintln!(
                        "Empty stream from model (attempt {}/{}), retrying...",
                        empty_stream_retries, MAX_EMPTY_STREAM_RETRIES
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(
                        500 * u64::from(empty_stream_retries),
                    ))
                    .await;
                    continue;
                }
                let err = EngineError {
                    code: "empty_stream".to_string(),
                    message: format!(
                        "Model returned an empty stream (no content) after {} retries",
                        empty_stream_retries
                    ),
                    details: None,
                };
                let _ = tx.send(EngineEvent::Error(err.clone())).await;
                record_query_trajectory(
                    &config,
                    &traj_prompt,
                    std::mem::take(&mut traj_actions),
                    crate::engine::evolution::TrajectoryOutcome::Error {
                        code: err.code.clone(),
                        message: err.message.clone(),
                    },
                    start_time.elapsed().as_millis() as u64,
                )
                .await;
                send_terminal_result(
                    &tx,
                    start_time,
                    &cost_tracker,
                    total_usage,
                    turn_count,
                    QueryStatus::Error,
                    Some(err),
                    None,
                    stop_reason,
                )
                .await;
                return;
            }

            // No tools → query complete
            let text = extract_text(&assistant_content_blocks);
            // Emit TurnEnd for the final turn (no tools)
            let _ = tx
                .send(EngineEvent::TurnEnd {
                    turn_id: turn_id_counter,
                    duration_ms: turn_start_time.elapsed().as_millis() as u64,
                    tool_count: turn_tool_count,
                    input_tokens: total_usage
                        .input_tokens
                        .saturating_sub(turn_input_tokens_at_start),
                    output_tokens: total_usage
                        .output_tokens
                        .saturating_sub(turn_output_tokens_at_start),
                })
                .await;
            record_telemetry_turn(
                &config,
                turn_start_time.elapsed().as_millis() as u64,
                total_usage
                    .input_tokens
                    .saturating_sub(turn_input_tokens_at_start),
                total_usage
                    .output_tokens
                    .saturating_sub(turn_output_tokens_at_start),
                Vec::new(),
            );
            record_query_trajectory(
                &config,
                &traj_prompt,
                std::mem::take(&mut traj_actions),
                crate::engine::evolution::TrajectoryOutcome::Completed {
                    final_text_preview: text.clone().unwrap_or_default(),
                },
                start_time.elapsed().as_millis() as u64,
            )
            .await;
            maybe_spawn_session_memory_update(messages, &config);
            send_terminal_result(
                &tx,
                start_time,
                &cost_tracker,
                total_usage,
                turn_count,
                QueryStatus::Complete,
                None,
                text,
                stop_reason,
            )
            .await;
            return;
        }

        // Execute the tool-call turn (events, tools, transcript, TurnEnd)
        execute_tool_turn(
            messages,
            &tool_uses,
            &config,
            &tx,
            &mut transcript_writer,
            &total_usage,
            &mut traj_actions,
            turn_id_counter,
            turn_start_time,
            turn_input_tokens_at_start,
            turn_output_tokens_at_start,
        )
        .await;

        turn_count += 1;

        maybe_spawn_session_memory_update(messages, &config);
    }
}

/// Inject the 70% and 90% iteration-budget warning messages into the
/// conversation (each tier fires at most once per query loop).
fn inject_iteration_budget_warnings(
    messages: &mut Vec<Message>,
    turn_count: u32,
    max_turns: Option<u32>,
    budget_warned_70: &mut bool,
    budget_warned_90: &mut bool,
) {
    if let Some(max) = max_turns {
        let ratio = turn_count as f32 / max as f32;

        // 70%: Inject soft warning into conversation (hidden from user, model sees it)
        if ratio >= 0.7 && !*budget_warned_70 {
            *budget_warned_70 = true;
            messages.push(Message {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                content: MessageContent::User {
                    message: ApiUserMessage {
                        role: "user".to_string(),
                        content: Value::String(
                            "[System: Iteration budget at 70%. Prioritize wrapping up the current task.]".to_string()
                        ),
                    },
                    is_meta: false,
                    tool_use_result: None,
                },
            });
        }

        // 90%: Inject urgent warning
        if ratio >= 0.9 && !*budget_warned_90 {
            *budget_warned_90 = true;
            messages.push(Message {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                content: MessageContent::User {
                    message: ApiUserMessage {
                        role: "user".to_string(),
                        content: Value::String(
                            "[System: Iteration budget at 90% (CRITICAL). You must produce a final answer now. Do NOT start new sub-tasks.]".to_string()
                        ),
                    },
                    is_meta: false,
                    tool_use_result: None,
                },
            });
        }
    }
}

/// Compute the current token-budget status, preferring the pre-computed
/// budget on the first turn to avoid a redundant lock + tiktoken estimate.
async fn compute_token_budget(
    messages: &[Message],
    turn_count: u32,
    config: &mut QueryLoopConfig,
) -> (BudgetStatus, u64) {
    if turn_count == 0 {
        if let Some(precomputed) = config.initial_budget.take() {
            precomputed
        } else {
            let counter = config.token_counter.lock().await;
            let est = counter.current_estimate(messages);
            (counter.budget_status_given(est), est)
        }
    } else {
        let counter = config.token_counter.lock().await;
        let est = counter.current_estimate(messages);
        (counter.budget_status_given(est), est)
    }
}

/// React to the token-budget status: warn on `Warning`, auto-compact
/// (session-memory first, then API compaction) on `Blocking`/`Compact`.
async fn enforce_token_budget(
    messages: &mut Vec<Message>,
    tx: &mpsc::Sender<EngineEvent>,
    config: &mut QueryLoopConfig,
    budget_status: BudgetStatus,
    current_tokens: u64,
) {
    match budget_status {
        BudgetStatus::Warning => {
            eprintln!(
                "Token budget warning: {} tokens (approaching limit)",
                current_tokens
            );
        }
        BudgetStatus::Blocking | BudgetStatus::Compact if messages.len() > 5 => {
            eprintln!(
                "Token budget {} ({} tokens), auto-compacting mid-loop",
                if budget_status == BudgetStatus::Blocking {
                    "BLOCKING"
                } else {
                    "compact"
                },
                current_tokens
            );
            let _ = tx.send(EngineEvent::Progress {
                tool_use_id: String::new(),
                data: serde_json::json!({"message": format!("Context approaching limit ({} est. tokens), compacting...", current_tokens)}),
            }).await;

            // Circuit breaker: skip compact after too many consecutive failures.
            if config.compact_fail_count >= MAX_COMPACT_FAILURES {
                eprintln!(
                    "Compact circuit breaker: {} consecutive failures, skipping",
                    config.compact_fail_count
                );
            } else {
                // Try session_memory_compact first (no API call needed).
                let tokens_before = estimate_tokens(messages);
                let session_ok = config
                    .session_memory
                    .as_ref()
                    .is_some_and(|sm| session_memory_compact(messages, &sm.get()));

                if !session_ok {
                    let keep = adaptive_keep_recent(&config.adaptive_compact);
                    match compact_messages(messages, tx.clone(), &*config, keep).await {
                        Ok(_) => {
                            record_compact_feedback(
                                &mut config.adaptive_compact,
                                messages,
                                tokens_before,
                            );
                            eprintln!("Mid-loop auto-compact succeeded");
                            config.compact_fail_count = 0;
                        }
                        Err(e) => {
                            eprintln!(
                                "Mid-loop auto-compact failed: {}, continuing anyway",
                                e.message
                            );
                            config.compact_fail_count += 1;
                        }
                    }
                } else {
                    record_compact_feedback(&mut config.adaptive_compact, messages, tokens_before);
                    config.compact_fail_count = 0;
                }
            }
        }
        _ => {} // Normal
    }
}

pub use crate::engine::failover::{call_api_with_fallback, ApiCallOutcome};

pub use crate::engine::stream_ingest::{ingest_stream_events, IngestedTurn, StreamIngest};

/// Append the assistant message to history,
#[allow(clippy::too_many_arguments)]
/// write transcript + cross-session index entries, and emit StateUpdate.
async fn record_assistant_turn(
    messages: &mut Vec<Message>,
    assistant_content_blocks: &[ContentBlock],
    stop_reason: &Option<String>,
    config: &QueryLoopConfig,
    transcript_writer: &mut Option<TranscriptWriter>,
    cross_db: &Option<crate::engine::cross_session_db::CrossSessionDb>,
    tx: &mpsc::Sender<EngineEvent>,
    cost_tracker: &CostTracker,
    total_usage: &Usage,
) {
    // Build assistant message and append to history
    let assistant_msg = Message {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        content: MessageContent::Assistant {
            message: ApiAssistantMessage {
                role: "assistant".to_string(),
                content: assistant_content_blocks.to_vec(),
                stop_reason: stop_reason.clone(),
                usage: None,
            },
            cost_usd: cost_tracker.current_query_cost(),
            duration_ms: 0,
        },
    };
    messages.push(assistant_msg.clone());

    // Write assistant message to transcript
    append_transcript(
        &mut *transcript_writer,
        &TranscriptEntry {
            timestamp: assistant_msg.timestamp.clone(),
            entry_type: TranscriptEntryType::AssistantMessage,
            data: serde_json::to_value(&assistant_msg).unwrap_or_default(),
        },
    )
    .await;
    // Index assistant text for cross-session search
    if let (Some(ref db), Some(ref sid)) = (cross_db, &config.session_id) {
        let text: String = assistant_content_blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        if !text.is_empty() {
            if let Err(e) = db.index_message(sid, "assistant", &text, &assistant_msg.timestamp) {
                eprintln!(
                    "[cross-session] WARNING: assistant message not indexed: {}",
                    e
                );
            }
        }
    }

    // Push cost data to CLI via StateUpdate
    let _ = tx
        .send(EngineEvent::StateUpdate {
            patch: serde_json::json!({
                "total_cost_usd": cost_tracker.total_cost(),
                "current_query_cost_usd": cost_tracker.current_query_cost(),
                "usage": {
                    "input_tokens": total_usage.input_tokens,
                    "output_tokens": total_usage.output_tokens,
                    "cache_creation_input_tokens": total_usage.cache_creation_input_tokens,
                    "cache_read_input_tokens": total_usage.cache_read_input_tokens,
                }
            }),
        })
        .await;
}

#[allow(clippy::too_many_arguments)]
/// Execute one tool-call turn: emit ToolUse events, run the tools, emit
/// ToolResult events, append the tool-result message, and emit TurnEnd.
async fn execute_tool_turn(
    messages: &mut Vec<Message>,
    tool_uses: &[ToolUseRequest],
    config: &QueryLoopConfig,
    tx: &mpsc::Sender<EngineEvent>,
    transcript_writer: &mut Option<TranscriptWriter>,
    total_usage: &Usage,
    traj_actions: &mut Vec<crate::engine::evolution::TrajectoryAction>,
    turn_id_counter: u32,
    turn_start_time: std::time::Instant,
    turn_input_tokens_at_start: u64,
    turn_output_tokens_at_start: u64,
) {
    let mut turn_tool_count: u32 = 0;

    // Emit ToolUse events
    for tu in tool_uses {
        turn_tool_count += 1;
        let _ = tx
            .send(EngineEvent::ToolUse {
                tool_name: tu.name.clone(),
                input: tu.input.clone(),
                tool_use_id: tu.id.clone(),
            })
            .await;

        // Write tool use to transcript
        append_transcript(
            &mut *transcript_writer,
            &TranscriptEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                entry_type: TranscriptEntryType::ToolUse,
                data: serde_json::json!({
                    "tool_name": tu.name,
                    "input": tu.input,
                    "tool_use_id": tu.id,
                }),
            },
        )
        .await;
    }

    // Execute tools using the executor
    let tool_context = ToolContext {
        cwd: config.cwd.clone(),
        model: config.model.clone(),
        abort_signal: Arc::new(config.abort_rx.clone()),
        file_cache: config.file_cache.as_ref().map(Arc::clone),
        tool_result_store: config.tool_result_store.as_ref().map(Arc::clone),
        context_window: config.context_window,
        auto_compact_threshold_ratio: config.auto_compact_threshold_ratio,
    };
    let progress = NoopProgressSender;
    // With a permission bridge, mutating tools prompt the user instead of
    // failing closed; the PermissionRequest event rides the same `tx`.
    let permission_channels =
        config
            .permission
            .as_ref()
            .map(|p| crate::tools::executor::PermissionChannels {
                bridge: p.clone(),
                event_tx: tx.clone(),
            });
    let tool_results = execute_tools(
        &config.tools,
        tool_uses,
        &tool_context,
        &progress,
        permission_channels.as_ref(),
        Some(&config.tool_health),
    )
    .await;

    // Emit ToolResult events
    for result in &tool_results {
        let _ = tx
            .send(EngineEvent::ToolResult {
                tool_use_id: result.tool_use_id.clone(),
                output: result.output.clone(),
                is_error: result.is_error,
            })
            .await;

        // Write tool result to transcript
        append_transcript(
            &mut *transcript_writer,
            &TranscriptEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                entry_type: TranscriptEntryType::ToolResult,
                data: serde_json::json!({
                    "tool_use_id": result.tool_use_id,
                    "output": result.output,
                    "is_error": result.is_error,
                }),
            },
        )
        .await;
    }

    // Feed the shared tool-health tracker. Only real tools are recorded —
    // unknown-tool results would otherwise pile garbage records under the
    // raw (misspelled) names the model sent. The reason is truncated by
    // CHARS, not bytes, to avoid panicking inside a multi-byte character.
    for res in &tool_results {
        if res.is_error {
            if config.tools.iter().any(|t| t.name() == res.tool_name) {
                let reason: String = serde_json::to_string(&res.output)
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect();
                config.tool_health.record_failure(&res.tool_name, &reason);
            }
        } else {
            config.tool_health.record_success(&res.tool_name);
        }
    }

    // Sticky deferred activation: any invoked deferred tool, and any
    // deferred tool surfaced by tool search, carries its full input schema
    // on the NEXT request of this query (activation is the point where the
    // model showed interest — including error results, which are exactly
    // when the schema is needed to retry).
    for res in &tool_results {
        if res.tool_name == "ToolSearchTool" {
            if let Some(matches) = res.output.get("matches").and_then(Value::as_array) {
                for m in matches {
                    if let Some(n) = m.get("name").and_then(Value::as_str) {
                        if config
                            .tools
                            .iter()
                            .any(|t| t.name() == n && t.is_deferred())
                        {
                            config
                                .expanded_tools
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(n.to_string());
                        }
                    }
                }
            }
        } else if config
            .tools
            .iter()
            .any(|t| t.name() == res.tool_name && t.is_deferred())
        {
            config
                .expanded_tools
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(res.tool_name.clone());
        }
    }

    // Feed skill-outcome stats to the evolution engine: every named Skill
    // load is one data point for the improvement cycle (success = the skill
    // file was found and loaded). Best-effort; never blocks the turn.
    if let Some(evolution) = config.evolution.as_ref() {
        for res in &tool_results {
            if res.tool_name != "Skill" {
                continue;
            }
            let skill_name = tool_uses
                .iter()
                .find(|tu| tu.id == res.tool_use_id)
                .and_then(|tu| tu.input.get("skill").and_then(|v| v.as_str()))
                .unwrap_or("");
            if skill_name.is_empty() || skill_name == "__list__" {
                continue;
            }
            evolution
                .record_skill_outcome(skill_name, !res.is_error)
                .await;
        }
    }

    // Collect per-tool actions for the query trajectory.
    for res in &tool_results {
        traj_actions.push(crate::engine::evolution::TrajectoryAction {
            tool_name: res.tool_name.clone(),
            input_summary: tool_uses
                .iter()
                .find(|tu| tu.id == res.tool_use_id)
                .map(|tu| serde_json::to_string(&tu.input).unwrap_or_default())
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect::<String>(),
            output_summary: serde_json::to_string(&res.output)
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect::<String>(),
            is_error: res.is_error,
        });
    }

    // Build tool result user message and append to messages
    let tool_result_msg = build_tool_result_message(&tool_results);
    messages.push(tool_result_msg);

    // Emit TurnEnd after tool results are processed
    let _ = tx
        .send(EngineEvent::TurnEnd {
            turn_id: turn_id_counter,
            duration_ms: turn_start_time.elapsed().as_millis() as u64,
            tool_count: turn_tool_count,
            input_tokens: total_usage
                .input_tokens
                .saturating_sub(turn_input_tokens_at_start),
            output_tokens: total_usage
                .output_tokens
                .saturating_sub(turn_output_tokens_at_start),
        })
        .await;

    record_telemetry_turn(
        config,
        turn_start_time.elapsed().as_millis() as u64,
        total_usage
            .input_tokens
            .saturating_sub(turn_input_tokens_at_start),
        total_usage
            .output_tokens
            .saturating_sub(turn_output_tokens_at_start),
        tool_uses.iter().map(|tu| tu.name.clone()).collect(),
    );
}

/// Fire-and-forget: spawn a background task to update the session summary
/// every N messages.  The summary is persisted to .memory.md and loaded
/// instantly on next session startup.
fn maybe_spawn_session_memory_update(messages: &[Message], config: &QueryLoopConfig) {
    if let Some(ref sm) = config.session_memory {
        let current_count = messages.len();
        if sm.should_update(current_count) {
            let msgs_clone = messages.to_vec();
            let sm_arc = Arc::clone(sm);
            let api = Arc::clone(&config.api_client);
            let mdl = config.model.clone();
            let existing = sm.get();
            tokio::spawn(async move {
                update_session_memory_background(msgs_clone, sm_arc, api, mdl, existing).await;
            });
        }
    }
}

use crate::engine::compact::{adaptive_keep_recent, record_compact_feedback};
pub use crate::engine::compact::{
    adjust_compact_split, compact_messages, context_overflow_compact, micro_compact,
    reactive_compact, session_memory_compact, tail_chars,
};

/// Maximum consecutive compact failures before the circuit breaker trips.
const MAX_COMPACT_FAILURES: usize = 3;

/// Record the whole query as one trajectory (best-effort; no-op when no
/// evolution engine is attached). Actions are collected across tool turns.
async fn record_query_trajectory(
    config: &QueryLoopConfig,
    user_prompt: &str,
    actions: Vec<crate::engine::evolution::TrajectoryAction>,
    outcome: crate::engine::evolution::TrajectoryOutcome,
    duration_ms: u64,
) {
    use crate::engine::evolution::Trajectory;
    let Some(evolution) = config.evolution.as_ref() else {
        return;
    };
    let tool_count = actions.len();
    let prompt_preview: String = user_prompt.chars().take(500).collect();
    evolution
        .record_trajectory(Trajectory {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            cwd: config.cwd.to_string_lossy().to_string(),
            user_prompt: prompt_preview,
            assistant_actions: actions,
            outcome,
            tool_count,
            duration_ms,
            user_rating: None,
        })
        .await;
}

/// Record one turn into the local telemetry database (best-effort; no-op
/// when telemetry is disabled or the DB cannot be opened).
fn record_telemetry_turn(
    config: &QueryLoopConfig,
    duration_ms: u64,
    input_tokens: u64,
    output_tokens: u64,
    tool_names: Vec<String>,
) {
    let Some(telemetry) = config.telemetry.as_ref() else {
        return;
    };
    let mut usage = EMPTY_USAGE;
    usage.input_tokens = input_tokens;
    usage.output_tokens = output_tokens;
    let cost = CostTracker::new().calculate_cost(&usage, &config.model);
    let session = config.session_id.clone().unwrap_or_default();
    if let Err(e) = telemetry.record_turn(
        &session,
        input_tokens,
        output_tokens,
        cost,
        duration_ms,
        tool_names,
    ) {
        eprintln!("Telemetry record_turn failed: {}", e);
    }
}

pub use crate::engine::tool_repair::{
    collect_tool_ids, strip_orphan_tool_result_blocks, validate_and_fix_tool_messages,
};

/// Background task: generate a session summary and persist it.
///
/// Spawned (fire-and-forget) after each query loop iteration when
/// `session_memory.should_update()` returns true.  Uses a lightweight
/// API call (no tools, no conversation history) to generate a rolling
/// summary that is loaded instantly on next session startup.
pub async fn update_session_memory_background(
    messages: Vec<Message>,
    session_memory: Arc<SessionMemory>,
    api_client: Arc<UnifiedClient>,
    model: String,
    existing_summary: String,
) {
    let msg_count = messages.len();
    if msg_count < 4 {
        return;
    }

    let conversation_text = format_messages_for_summary(&messages);
    // Keep the most recent portion of the conversation: the tail carries the
    // current task state, while an early cut would freeze the summary on the
    // session's first minutes and leave it stale for the rest of the session.
    let truncated = tail_chars(&conversation_text, 40_000);

    let prompt = if existing_summary.is_empty() {
        format!(
            "You are summarizing a coding assistant session. Write a rolling meeting-notes \
             summary in markdown with exactly these sections:\n\n\
             ## Task Overview\n\
             ## Current State\n\
             ## Key Discoveries\n\
             ## Next Steps\n\
             ## Context to Preserve\n\n\
             - Task Overview: what the user asked for; quote key requests verbatim.\n\
             - Current State: what is done, what is in progress, the very last action taken.\n\
             - Key Discoveries: facts learned about the codebase/environment, including \
             approaches that failed and why.\n\
             - Next Steps: concrete pending actions, in order.\n\
             - Context to Preserve: promises made to the user, constraints, stated preferences.\n\n\
             Copy exact identifiers, paths, commands and error messages verbatim — never \
             paraphrase literals. Be concise; the summary is capped at 8000 characters.\n\n\
             Conversation:\n{}",
            truncated
        )
    } else {
        format!(
            "You are updating a rolling summary of a coding assistant session.\n\n\
             Existing summary:\n---\n{}\n---\n\n\
             Recent conversation:\n{}\n\n\
             Rewrite the summary so it reflects the latest work. Keep the same section layout \
             (Task Overview / Current State / Key Discoveries / Next Steps / Context to \
             Preserve): merge duplicates, drop resolved items, add new discoveries and failed \
             approaches, keep the current task state accurate. Copy exact identifiers, paths, \
             commands and error messages verbatim — never paraphrase literals. Be concise; the \
             summary is capped at 8000 characters.",
            existing_summary, truncated
        )
    };

    let request = CreateMessageRequest {
        model,
        messages: vec![serde_json::json!({
            "role": "user",
            "content": prompt,
        })],
        system: Some(vec![serde_json::json!({
            "type": "text",
            "text": "You are a session summarizer. Produce concise, structured markdown summaries.",
            "cache_control": {"type": "ephemeral"},
        })]),
        tools: None,
        max_tokens: 2048,
        stream: true,
        thinking: None,
        metadata: None,
    };

    match api_client.create_message_stream(request).await {
        Ok(mut stream) => {
            use futures::StreamExt;
            let mut summary_text = String::new();
            while let Some(event_result) = stream.next().await {
                match event_result {
                    Ok(ApiStreamEvent::ContentBlockDelta { delta, .. }) => {
                        if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                            summary_text.push_str(text);
                        }
                    }
                    Err(e) => {
                        eprintln!("Session memory background update stream error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
            if !summary_text.trim().is_empty() {
                session_memory.update(summary_text);
                session_memory.set_message_count(msg_count);
                eprintln!(
                    "Session memory updated ({} chars, at msg #{})",
                    session_memory.get().len(),
                    msg_count
                );
            }
        }
        Err(e) => {
            eprintln!("Session memory background update failed: {}", e);
        }
    }
}

#[cfg(test)]
mod session_summary_tests {
    use super::*;

    #[test]
    fn test_tail_chars_short_input_passthrough() {
        assert_eq!(tail_chars("hello", 100), "hello");
        assert_eq!(tail_chars("", 10), "");
    }

    #[test]
    fn test_tail_chars_keeps_tail_and_marks_omission() {
        let text = "a".repeat(300);
        let out = tail_chars(&text, 100);
        assert!(out.starts_with("[...earlier conversation omitted...]\n"));
        let body = out.lines().nth(1).unwrap();
        assert_eq!(body.len(), 100);
        assert!(text.ends_with(body));
    }

    #[test]
    fn test_tail_chars_cuts_at_char_boundary() {
        // 'é' is two bytes; a naive byte cut would split it.
        let text = "é".repeat(150);
        let out = tail_chars(&text, 100);
        let body = out.lines().nth(1).unwrap();
        assert_eq!(body.chars().count(), 50);
        assert!(text.ends_with(body));
    }
}

#[cfg(test)]
mod adaptive_compact_tests {
    use super::*;
    use crate::engine::query_engine::{AdaptiveCompactTracker, CompactResult};

    #[test]
    fn initial_recommendation_matches_historical_default() {
        // The tracker starts at 10 — identical to the pre-adaptive hardcoded
        // KEEP_RECENT — so the first compact of a session is unchanged.
        let tracker = AdaptiveCompactTracker::new();
        assert_eq!(adaptive_keep_recent(&tracker), 10);
    }

    #[test]
    fn keep_recent_is_clamped_to_safe_band() {
        let mut tracker = AdaptiveCompactTracker::new();
        tracker.keep_recent = 2;
        assert_eq!(adaptive_keep_recent(&tracker), 8);
        tracker.keep_recent = 500;
        assert_eq!(adaptive_keep_recent(&tracker), 30);
    }

    #[test]
    fn good_compression_lets_tracker_shrink_keep() {
        // Sustained good compression + no repeated topics should trend the
        // tracker's keep_recent down (floor 8 via the policy clamp).
        let mut tracker = AdaptiveCompactTracker::new();
        for _ in 0..5 {
            tracker.record_compact(
                &CompactResult {
                    tokens_saved: 900,
                    summary_tokens: 0,
                    tokens_before: 1000,
                    tokens_after: 100,
                },
                false,
            );
        }
        assert!(tracker.recommended_keep_recent() < 10);
        assert_eq!(adaptive_keep_recent(&tracker), 8);
    }

    #[test]
    fn feedback_records_estimated_tokens() {
        let mut tracker = AdaptiveCompactTracker::new();
        // Empty message list -> tokens_after 0, all "before" tokens saved.
        record_compact_feedback(&mut tracker, &[], 1234);
        assert_eq!(tracker.compact_count, 1);
        assert_eq!(tracker.history.len(), 1);
        assert_eq!(tracker.history[0].tokens_before, 1234);
        assert_eq!(tracker.history[0].tokens_after, 0);
        assert!(!tracker.history[0].user_repeated_topic);
    }
}

#[cfg(test)]
mod context_hygiene_tests {
    use super::*;
    use crate::engine::query_engine::MicroCompactConfig;
    use crate::models::message::{ApiAssistantMessage, ApiUserMessage, ContentBlock};

    fn old_ts() -> String {
        (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339()
    }

    fn assistant_with_tool_use(id: &str, name: &str) -> Message {
        Message {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: old_ts(),
            content: MessageContent::Assistant {
                message: ApiAssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![ContentBlock::ToolUse {
                        id: id.to_string(),
                        name: name.to_string(),
                        input: serde_json::json!({}),
                    }],
                    stop_reason: None,
                    usage: None,
                },
                cost_usd: 0.0,
                duration_ms: 0,
            },
        }
    }

    fn user_with_result(id: &str, content_len: usize) -> Message {
        Message {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: old_ts(),
            content: MessageContent::User {
                message: ApiUserMessage {
                    role: "user".to_string(),
                    content: Value::Array(vec![serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": "x".repeat(content_len),
                    })]),
                },
                is_meta: false,
                tool_use_result: None,
            },
        }
    }

    fn result_content(msg: &Message) -> String {
        match &msg.content {
            MessageContent::User { message, .. } => match &message.content {
                Value::Array(arr) => arr[0]
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                _ => panic!("expected array content"),
            },
            _ => panic!("expected user message"),
        }
    }

    fn aggressive() -> MicroCompactConfig {
        MicroCompactConfig {
            min_age_secs: 0,
            min_chars: 10,
        }
    }

    fn pad_to_five(messages: &mut Vec<Message>) {
        while messages.len() < 5 {
            messages.push(Message {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: old_ts(),
                content: MessageContent::User {
                    message: ApiUserMessage {
                        role: "user".to_string(),
                        content: Value::String("filler".to_string()),
                    },
                    is_meta: false,
                    tool_use_result: None,
                },
            });
        }
    }

    #[test]
    fn micro_compact_names_the_cleared_tool() {
        let mut messages = vec![
            user_with_result("tu_1", 100),
            assistant_with_tool_use("tu_1", "Bash"),
        ];
        pad_to_five(&mut messages);
        micro_compact(&mut messages, aggressive());
        let content = result_content(&messages[0]);
        assert!(
            content.starts_with("[Old tool result cleared — Bash output, "),
            "{}",
            content
        );
        assert!(content.ends_with("chars]"), "{}", content);
    }

    #[test]
    fn micro_compact_unknown_tool_uses_generic_placeholder() {
        let mut messages = vec![user_with_result("tu_missing", 100)];
        pad_to_five(&mut messages);
        micro_compact(&mut messages, aggressive());
        let content = result_content(&messages[0]);
        // The reported size is the JSON-serialized form (string + quotes).
        let serialized_len = serde_json::json!("x".repeat(100)).to_string().len();
        assert_eq!(
            content,
            format!(
                "[Old tool result cleared — originally {} chars]",
                serialized_len
            )
        );
    }

    #[test]
    fn micro_compact_respects_both_thresholds() {
        // Large but young → untouched.
        let mut messages = vec![user_with_result("tu_1", 100)];
        pad_to_five(&mut messages);
        micro_compact(
            &mut messages,
            MicroCompactConfig {
                min_age_secs: 86_400,
                min_chars: 10,
            },
        );
        assert_eq!(result_content(&messages[0]).len(), 100);

        // Old but small → untouched.
        let mut messages = vec![user_with_result("tu_1", 5)];
        pad_to_five(&mut messages);
        micro_compact(&mut messages, aggressive());
        assert_eq!(result_content(&messages[0]).len(), 5);
    }

    #[test]
    fn micro_compact_never_touches_recent_messages() {
        let mut messages = vec![user_with_result("tu_1", 100)];
        pad_to_five(&mut messages);
        micro_compact(&mut messages, MicroCompactConfig::disabled());
        assert_eq!(result_content(&messages[0]).len(), 100);
    }

    #[test]
    fn micro_compact_skips_last_four_messages() {
        let old = user_with_result("tu_1", 100);
        let mut messages = vec![
            assistant_with_tool_use("tu_1", "Bash"),
            old,
            Message {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: old_ts(),
                content: MessageContent::User {
                    message: ApiUserMessage {
                        role: "user".to_string(),
                        content: Value::Array(vec![serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": "tu_1",
                            "content": "x".repeat(100),
                        })]),
                    },
                    is_meta: false,
                    tool_use_result: None,
                },
            },
        ];
        // len == 3 → start == 0 → wait, skip_recent=4 > len → start == 0,
        // everything is "recent" relative to the window cap. Add padding to
        // push the duplicate result outside the last four.
        pad_to_five(&mut messages);
        // Now len == 5, start == 1 → index 0 processed, index 2 (in last 4) not.
        micro_compact(&mut messages, aggressive());
        let content = result_content(&messages[2]);
        assert_eq!(content.len(), 100, "result inside last 4 must stay intact");
    }

    #[test]
    fn strip_orphan_blocks_drops_emptied_user_messages() {
        let mut messages = vec![
            assistant_with_tool_use("tu_1", "Bash"),
            user_with_result("tu_orphan", 50),
        ];
        let (use_ids, result_ids) = collect_tool_ids(&messages);
        let orphans: std::collections::HashSet<String> =
            result_ids.difference(&use_ids).cloned().collect();
        assert_eq!(orphans.len(), 1);
        let removed = strip_orphan_tool_result_blocks(&mut messages, &orphans);
        assert_eq!(removed, 1);
        assert_eq!(messages.len(), 1, "emptied user message must be dropped");
    }

    #[test]
    fn validate_strips_orphan_blocks_but_keeps_valid_and_text() {
        use crate::models::message::ApiUserMessage;
        let mixed_user = Message {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: old_ts(),
            content: MessageContent::User {
                message: ApiUserMessage {
                    role: "user".to_string(),
                    content: Value::Array(vec![
                        serde_json::json!({"type": "text", "text": "the tool says:"}),
                        serde_json::json!({"type": "tool_result", "tool_use_id": "tu_1", "content": "valid"}),
                        serde_json::json!({"type": "tool_result", "tool_use_id": "tu_orphan", "content": "orphan"}),
                    ]),
                },
                is_meta: false,
                tool_use_result: None,
            },
        };
        let messages = vec![assistant_with_tool_use("tu_1", "Bash"), mixed_user];
        let fixed = validate_and_fix_tool_messages(&messages);
        assert_eq!(fixed.len(), 2);
        match &fixed[1].content {
            MessageContent::User { message, .. } => match &message.content {
                Value::Array(arr) => {
                    assert_eq!(arr.len(), 2, "orphan block must be stripped");
                    assert_eq!(arr[0]["type"], "text");
                    assert_eq!(arr[1]["tool_use_id"], "tu_1");
                }
                _ => panic!("expected array"),
            },
            _ => panic!("expected user message"),
        }
    }
}
