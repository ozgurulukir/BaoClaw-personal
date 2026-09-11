use std::sync::Arc;
use tokio::sync::mpsc;

use crate::api::client::ApiError;
use crate::api::fallback::{FallbackAction, FallbackController};
use crate::api::unified::UnifiedStream;
use crate::engine::api_builder::build_api_request;
use crate::engine::compact::{
    adaptive_keep_recent, compact_messages, reactive_compact, record_compact_feedback,
};
use crate::engine::query_engine::{
    estimate_tokens, AdaptiveCompactTracker, EngineError, EngineEvent, QueryLoopConfig,
};
use crate::models::message::{Message, MessageContent};

/// Hard ceiling for a single streaming LLM API call.
pub const API_CALL_TIMEOUT_SECS: u64 = 300; // 5 min

/// Maximum consecutive compact failures before the circuit breaker trips.
pub const MAX_COMPACT_FAILURES: usize = 3;

/// Outcome of building and sending one API request through the fallback machinery.
pub enum ApiCallOutcome {
    /// A live SSE stream to ingest.
    Stream(UnifiedStream),
    /// Transient failure handled by retrying the loop (backoff/fallback applied).
    Retry,
    /// Fatal failure: an Error event was already sent to `tx`; the payload is
    /// carried back so the loop can record a trajectory before returning.
    Fatal(EngineError),
}

/// Build the API request for the current model and send it (with timeout),
/// handling rate limits / server errors via the FallbackController.
pub async fn call_api_with_fallback(
    messages: &mut Vec<Message>,
    tx: &mpsc::Sender<EngineEvent>,
    config: &mut QueryLoopConfig,
    fallback_controller: &mut FallbackController,
    current_tokens: u64,
) -> ApiCallOutcome {
    // Build API request using the current model from fallback controller
    let current_config = QueryLoopConfig {
        api_client: Arc::clone(&config.api_client),
        tools: config.tools.clone(),
        expanded_tools: Arc::clone(&config.expanded_tools),
        model: fallback_controller.current_model().to_string(),
        max_turns: config.max_turns,
        cwd: config.cwd.clone(),
        custom_system_prompt: config.custom_system_prompt.clone(),
        append_system_prompt: config.append_system_prompt.clone(),
        project_instructions: config.project_instructions.clone(),
        git_info: config.git_info.clone(),
        thinking_config: config.thinking_config.clone(),
        abort_rx: config.abort_rx.clone(),
        session_id: config.session_id.clone(),
        fallback_models: config.fallback_models.clone(),
        max_retries_per_model: config.max_retries_per_model,
        token_counter: Arc::clone(&config.token_counter),
        parent_turn_id: None,
        agent_label: None,
        session_memory: config.session_memory.as_ref().map(Arc::clone),
        compact_fail_count: config.compact_fail_count,
        recent_messages_for_rules: messages.clone(),
        file_cache: config.file_cache.as_ref().map(Arc::clone),
        tool_result_store: config.tool_result_store.as_ref().map(Arc::clone),
        initial_budget: None,
        cached_rules_raw: config.cached_rules_raw.clone(),
        adaptive_compact: AdaptiveCompactTracker::new(),
        tool_health: Arc::clone(&config.tool_health),
        permission: config.permission.clone(),
        telemetry: None,
        evolution: None,
        context_window: config.context_window,
        auto_compact_threshold_ratio: config.auto_compact_threshold_ratio,
        max_budget_usd: config.max_budget_usd,
        max_tokens_budget: config.max_tokens_budget,
        max_tokens: config.max_tokens,
        micro_compact: config.micro_compact,
    };
    let request = build_api_request(messages, &current_config);

    // Show what we're about to send
    let _ = tx
        .send(EngineEvent::Progress {
            tool_use_id: String::new(),
            data: serde_json::json!({
                "message": format!("Calling {} ({} messages, ~{} tokens)...",
                    current_config.model,
                    messages.len(),
                    current_tokens),
            }),
        })
        .await;

    // Call LLM API (streaming) with rate-limit fallback handling and timeout
    let stream_result = tokio::time::timeout(
        std::time::Duration::from_secs(API_CALL_TIMEOUT_SECS),
        config.api_client.create_message_stream(request),
    )
    .await;
    let stream_result = match stream_result {
        Ok(r) => r,
        Err(_) => {
            // Remove the user message that caused the timeout so it won't
            // appear as a duplicate on the next query attempt.
            if let Some(last) = messages.last() {
                if matches!(&last.content, MessageContent::User { .. }) {
                    eprintln!("API timeout, removing last user message to keep history clean");
                    messages.pop();
                }
            }
            let err = EngineError {
                code: "timeout".to_string(),
                message: "API call timed out after 5 minutes".to_string(),
                details: None,
            };
            let _ = tx.send(EngineEvent::Error(err.clone())).await;
            return ApiCallOutcome::Fatal(err);
        }
    };
    let stream = match stream_result {
        Ok(s) => s,
        Err(ApiError::RateLimited) => {
            // Handle rate limit with fallback controller
            match fallback_controller.on_rate_limit() {
                FallbackAction::Retry {
                    model,
                    attempt,
                    delay,
                } => {
                    eprintln!(
                        "Rate limited on {}, retrying (attempt {})...",
                        model, attempt
                    );
                    tokio::time::sleep(delay).await;
                    return ApiCallOutcome::Retry; // retry the loop
                }
                FallbackAction::Fallback { from, to } => {
                    eprintln!("Rate limited on {}, falling back to {}", from, to);
                    let _ = tx
                        .send(EngineEvent::ModelFallback {
                            from_model: from,
                            to_model: to,
                        })
                        .await;
                    return ApiCallOutcome::Retry; // retry with new model
                }
                FallbackAction::Exhausted {
                    models_tried,
                    total_retries,
                } => {
                    let error_msg = format!(
                        "All models exhausted after {} retries. Tried: {}",
                        total_retries,
                        models_tried.join(", ")
                    );
                    if let Some(last) = messages.last() {
                        if matches!(&last.content, MessageContent::User { .. }) {
                            eprintln!("All models exhausted, removing last user message to keep history clean");
                            messages.pop();
                        }
                    }
                    let err = EngineError {
                        code: "all_models_exhausted".to_string(),
                        message: error_msg,
                        details: Some(serde_json::json!({
                            "models_tried": models_tried,
                            "total_retries": total_retries,
                        })),
                    };
                    let _ = tx.send(EngineEvent::Error(err.clone())).await;
                    return ApiCallOutcome::Fatal(err);
                }
            }
        }
        Err(ApiError::ServerError { status }) => {
            // Retry server errors (500, 502, 503) with exponential backoff
            const MAX_SERVER_RETRIES: u32 = 3;
            let retry_count = fallback_controller.server_error_count();
            if retry_count < MAX_SERVER_RETRIES {
                let delay = std::time::Duration::from_millis(1000 * 2u64.pow(retry_count));
                eprintln!(
                    "Server error {} on {}, retrying in {:?} (attempt {}/{})...",
                    status,
                    fallback_controller.current_model(),
                    delay,
                    retry_count + 1,
                    MAX_SERVER_RETRIES
                );
                fallback_controller.on_server_error();
                tokio::time::sleep(delay).await;
                return ApiCallOutcome::Retry; // retry the loop
            }
            // Exhausted retries — fall back to next model if available
            eprintln!(
                "Server error {} on {} after {} retries, trying fallback...",
                status,
                fallback_controller.current_model(),
                MAX_SERVER_RETRIES
            );
            match fallback_controller.on_server_error_exhausted() {
                FallbackAction::Fallback { from, to } => {
                    let _ = tx
                        .send(EngineEvent::ModelFallback {
                            from_model: from,
                            to_model: to,
                        })
                        .await;
                    return ApiCallOutcome::Retry; // retry with new model
                }
                _ => {
                    let error_msg = format!(
                        "Server error {} after exhausting retries and fallbacks",
                        status
                    );
                    if let Some(last) = messages.last() {
                        if matches!(&last.content, MessageContent::User { .. }) {
                            eprintln!("Server error exhausted, removing last user message to keep history clean");
                            messages.pop();
                        }
                    }
                    let err = EngineError {
                        code: "api_server_error".to_string(),
                        message: error_msg,
                        details: None,
                    };
                    let _ = tx.send(EngineEvent::Error(err.clone())).await;
                    return ApiCallOutcome::Fatal(err);
                }
            }
        }
        Err(ApiError::BadRequest { message }) => {
            // 400 could be context overflow — try compaction before giving up
            let msg_lower = message.to_lowercase();
            if msg_lower.contains("context")
                || msg_lower.contains("token")
                || msg_lower.contains("too large")
                || msg_lower.contains("too long")
            {
                eprintln!("Bad request (likely context overflow), auto-compacting...");
                if config.compact_fail_count >= MAX_COMPACT_FAILURES {
                    eprintln!(
                        "Compact circuit breaker: {} consecutive failures, trying reactive compact",
                        config.compact_fail_count
                    );
                    reactive_compact(messages, None);
                    let _ = tx.send(EngineEvent::Progress {
                            tool_use_id: String::new(),
                            data: serde_json::json!({"message": "Reactive compact applied, retrying..."}),
                        }).await;
                    return ApiCallOutcome::Retry;
                }
                // Emergency path (context-overflow bad request): still adaptive,
                // but the tracker may steer toward larger keeps over time.
                let keep = adaptive_keep_recent(&config.adaptive_compact);
                let tokens_before = estimate_tokens(messages);
                match compact_messages(messages, tx.clone(), &*config, keep).await {
                    Ok(_) => {
                        record_compact_feedback(
                            &mut config.adaptive_compact,
                            messages,
                            tokens_before,
                        );
                        config.compact_fail_count = 0;
                        let _ = tx.send(EngineEvent::Progress {
                                tool_use_id: String::new(),
                                data: serde_json::json!({"message": "Auto-compacted context and retrying..."}),
                            }).await;
                        return ApiCallOutcome::Retry; // retry with compacted messages
                    }
                    Err(_) => {
                        config.compact_fail_count += 1;
                        // Compaction failed, try reactive compact as fallback
                        reactive_compact(messages, None);
                    }
                }
            }
            // Clean up: remove the user message that caused the bad request,
            // so the next query doesn't send duplicate/invalid messages.
            if let Some(last) = messages.last() {
                if matches!(&last.content, MessageContent::User { .. }) {
                    eprintln!("Bad request, removing last user message to keep history clean");
                    messages.pop();
                }
            }
            let err = EngineError {
                code: "api_bad_request".to_string(),
                message: message.to_string(),
                details: None,
            };
            let _ = tx.send(EngineEvent::Error(err.clone())).await;
            return ApiCallOutcome::Fatal(err);
        }
        Err(e) => {
            // Other API errors — remove the last user message to keep history clean
            if let Some(last) = messages.last() {
                if matches!(&last.content, MessageContent::User { .. }) {
                    eprintln!("API error, removing last user message to keep history clean");
                    messages.pop();
                }
            }
            let err = EngineError {
                code: "api_error".to_string(),
                message: format!("{}", e),
                details: None,
            };
            let _ = tx.send(EngineEvent::Error(err.clone())).await;
            return ApiCallOutcome::Fatal(err);
        }
    };

    ApiCallOutcome::Stream(stream)
}
