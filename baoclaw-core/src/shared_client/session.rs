use std::sync::atomic::Ordering;
use std::sync::Arc;

use baoclaw_core::engine::query_engine::{EngineEvent, ThinkingConfig};
use baoclaw_core::ipc::events::send_engine_event;
use baoclaw_core::ipc::protocol::RequestId;
use baoclaw_core::permissions::gate::PermissionDecision;

use super::{ScmFlow, WriterRef};
use crate::{
    render_loop_header, update_loop_header, ClientId, SharedSession, SharedState, SubmitterGuard,
};

pub(super) async fn scm_submit_message(
    session: &Arc<SharedSession>,
    writer: WriterRef<'_>,
    shared: &SharedState,
    client_id: ClientId,
    session_id: &str,
    id: RequestId,
    prompt: serde_json::Value,
    attachments: Option<Vec<serde_json::Value>>,
) -> ScmFlow {
    // The submitter lock is taken on the loop itself so
    // a second submit still gets -32001 immediately.
    if !session.try_acquire_submitter(client_id).await {
        let mut conn_guard = writer.lock().await;
        let _ = conn_guard
            .send_error(
                Some(id),
                -32001,
                "session busy: another client is currently submitting a message".into(),
            )
            .await;
        return ScmFlow::Continue;
    }
    session.touch_active().await;

    let prompt_str = match prompt.as_str() {
        Some(s) => s.to_string(),
        None => serde_json::to_string(&prompt).unwrap_or_default(),
    };

    // The drain runs in its own task so this
    // connection's serial loop keeps serving requests
    // while the turn streams — previously every request
    // on this connection (including abort and
    // permission responses) waited for the turn to end.
    let task_shared = shared.clone();
    let task_session = session.clone();
    let task_writer = writer.clone();
    let task_session_id = session_id.to_string();
    let task_client_id = client_id;
    tokio::spawn(async move {
        // Declared first so it drops last: releases
        // the submitter even if the drain panics.
        let mut submitter_guard = SubmitterGuard {
            session: task_session.clone(),
            client_id: task_client_id,
            armed: true,
        };
        let mut rx = {
            let mut engine = task_session.engine_write().await;
            engine
                .submit_message_with_attachments(prompt_str, attachments)
                .await
        };

        let mut disconnected = false;
        let mut turn_finished = false;
        // Structured failure of the terminal event, if any — the RPC reply
        // below reports the turn as an error instead of an unconditional
        // "complete" (an Error event alone used to be replied as success).
        let mut terminal_error: Option<(String, String)> = None;
        while let Some(event) = rx.recv().await {
            // Render Loop Headers to TUI on turn events
            match &event {
                EngineEvent::TurnStart {
                    turn_id,
                    agent_label,
                    ..
                } => {
                    render_loop_header(*turn_id, agent_label.as_deref());
                }
                EngineEvent::TurnEnd {
                    turn_id,
                    tool_count,
                    duration_ms,
                    ..
                } => {
                    update_loop_header(*turn_id, *tool_count, *duration_ms);
                }
                _ => {}
            }

            let terminal_event = matches!(&event, EngineEvent::Result(_) | EngineEvent::Error(_));
            if terminal_event {
                // Arm before broadcasting: this client's broadcast task then
                // skips the terminal event, which the direct write below
                // delivers. Deterministic — the flag is set before the event
                // can enter the broadcast channel.
                task_session.arm_terminal_handoff(task_client_id).await;
            }
            // Capture a structured failure so the RPC reply reports the
            // turn as an error instead of an unconditional "complete"
            // (an Error event alone used to be replied as success).
            terminal_error = match &event {
                EngineEvent::Error(err) => Some((err.code.clone(), err.message.clone())),
                EngineEvent::Result(result) => result
                    .error
                    .as_ref()
                    .map(|e| (e.code.clone(), e.message.clone())),
                _ => None,
            };
            // Broadcast to all clients
            task_session.broadcast(event.clone());

            // Also send directly to the submitting client
            {
                let mut conn_guard = task_writer.lock().await;
                if send_engine_event(&mut conn_guard, &event).await.is_err() {
                    disconnected = true;
                    turn_finished = terminal_event;
                    break;
                }
            }

            if terminal_event {
                turn_finished = true;
                break;
            }
        }

        if turn_finished {
            let mut engine = task_session.engine_write().await;
            engine.sync_messages().await;
            drop(engine);
            // Persist before releasing the submitter or handling disconnect.
            if let Err(e) = task_shared
                .session_registry
                .persist_session(&task_session_id)
                .await
            {
                eprintln!(
                    "[daemon] session {} persistence warning: {}",
                    task_session_id, e
                );
            }
        }

        // Release submitter AFTER the loop ends to prevent
        // the broadcast task from re-delivering the Result event.
        task_session.release_submitter(task_client_id).await;
        submitter_guard.armed = false;

        if disconnected {
            // The loop will notice the dead socket on its
            // next recv and run the disconnect cleanup.
            return;
        }

        let mut conn_guard = task_writer.lock().await;
        match terminal_error {
            Some((code, message)) => {
                let _ = conn_guard
                    .send_response(
                        id,
                        serde_json::json!({
                            "status": "error",
                            "error": {"code": code, "message": message}
                        }),
                    )
                    .await;
            }
            None => {
                let _ = conn_guard
                    .send_response(id, serde_json::json!({"status": "complete"}))
                    .await;
            }
        }
    });
    ScmFlow::Continue
}

pub(super) async fn scm_abort(session: &Arc<SharedSession>, writer: WriterRef<'_>, id: RequestId) {
    let engine = session.engine_read().await;
    engine.abort();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, serde_json::json!("ok")).await;
}

pub(super) async fn scm_shutdown(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
) -> ScmFlow {
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, serde_json::json!("ok")).await;
    // Shutdown terminates the daemon for all clients
    eprintln!("Shutdown requested — setting should_exit flag");
    shared.should_exit.store(true, Ordering::Relaxed);
    ScmFlow::Break
}

pub(super) async fn scm_update_settings(
    session: &Arc<SharedSession>,
    writer: WriterRef<'_>,
    id: RequestId,
    settings: serde_json::Value,
) {
    if let Some(thinking) = settings.get("thinking") {
        if let Some(mode) = thinking.get("mode").and_then(|v| v.as_str()) {
            let mut engine = session.engine_write().await;
            match mode {
                "enabled" => {
                    let budget = thinking
                        .get("budget_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(10240) as u32;
                    engine.update_thinking_config(ThinkingConfig::Enabled {
                        budget_tokens: budget,
                    });
                }
                "adaptive" => {
                    engine.update_thinking_config(ThinkingConfig::Adaptive);
                }
                _ => {
                    engine.update_thinking_config(ThinkingConfig::Disabled);
                }
            }
        }
    }
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, serde_json::json!("ok")).await;
}

pub(super) async fn scm_permission_response(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    tool_use_id: String,
    decision: String,
    rule: Option<String>,
) {
    let perm_decision = match decision.as_str() {
        "allow" => PermissionDecision::Allow,
        "allow_always" => PermissionDecision::AllowAlways { rule },
        _ => PermissionDecision::Deny,
    };
    let delivered = shared.permission_gate.respond(&tool_use_id, perm_decision);
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"delivered": delivered}))
        .await;
}

pub(super) async fn scm_initialize(writer: WriterRef<'_>, id: RequestId) {
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_error(Some(id), -32600, "Already initialized".into())
        .await;
}

pub(super) async fn scm_compact(
    session: &Arc<SharedSession>,
    writer: WriterRef<'_>,
    id: RequestId,
) -> ScmFlow {
    if session.has_active_submitter().await {
        let mut conn_guard = writer.lock().await;
        let _ = conn_guard
            .send_error(
                Some(id),
                -32002,
                "session busy: cannot compact while a message is being processed".into(),
            )
            .await;
        return ScmFlow::Continue;
    }
    let mut engine = session.engine_write().await;
    match engine.compact().await {
        Ok(result) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "tokens_saved": result.tokens_saved,
                        "summary_tokens": result.summary_tokens,
                        "tokens_before": result.tokens_before,
                        "tokens_after": result.tokens_after,
                    }),
                )
                .await;
        }
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard.send_error(Some(id), -32000, e.message).await;
        }
    }
    ScmFlow::Continue
}

/// Empty the session's conversation in memory and on disk (fresh start).
/// Long-term memory is kept. Rejected while a turn is in flight.
pub(super) async fn scm_clear_session(
    shared: &SharedState,
    session: &Arc<SharedSession>,
    session_id: &str,
    writer: WriterRef<'_>,
    id: RequestId,
) {
    if session.has_active_submitter().await {
        let mut conn_guard = writer.lock().await;
        let _ = conn_guard
            .send_error(
                Some(id),
                -32002,
                "session busy: cannot clear while a message is being processed".into(),
            )
            .await;
        return;
    }
    match shared.session_registry.clear_conversation(session_id).await {
        Ok(removed) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "cleared": true,
                        "messages_removed": removed,
                    }),
                )
                .await;
        }
        Err(message) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard.send_error(Some(id), -32000, message).await;
        }
    }
}

pub(super) async fn scm_switch_model(
    session: &Arc<SharedSession>,
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    new_model: String,
) -> ScmFlow {
    if session.has_active_submitter().await {
        let mut conn_guard = writer.lock().await;
        let _ = conn_guard
            .send_error(
                Some(id),
                -32002,
                "session busy: cannot switch model while a message is being processed".into(),
            )
            .await;
        return ScmFlow::Continue;
    }
    let mut engine = session.engine_write().await;
    engine.update_model(new_model.clone());
    shared.state_manager.update(|s| {
        s.model = new_model.clone();
    });
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"model": new_model}))
        .await;
    ScmFlow::Continue
}
