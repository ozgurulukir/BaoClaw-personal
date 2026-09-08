//! Shared-mode client RPC handling.
//!
//! Contains [`handle_shared_client`] — the per-connection request loop — and
//! one named `scm_*` function per `ClientMethod` RPC. Each `scm_*` body is
//! the verbatim former match-arm body of the loop's giant
//! `match parse_client_method(&req)`; control flow is preserved via
//! [`ScmFlow`] (the loop's `continue`/`break`).

#![allow(clippy::too_many_arguments, clippy::ptr_arg)]

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::sync::Mutex as TokioMutex;

use baoclaw_core::engine::query_engine::{EngineEvent, ThinkingConfig};
use baoclaw_core::engine::shared_session::{ClientId, SharedSession};
use baoclaw_core::ipc::events::send_engine_event;
use baoclaw_core::ipc::protocol::{JsonRpcMessage, RequestId};
use baoclaw_core::ipc::router::{parse_client_method, ClientMethod};
use baoclaw_core::ipc::server::{IpcConnection, IpcError, IpcWriter};
use baoclaw_core::permissions::gate::PermissionDecision;
use baoclaw_core::{discovery, doc_upload, engine, ipc, permissions};

use crate::{
    cwd_hash, render_loop_header, spawn_shared_broadcast, switch_shared_client, update_loop_header,
    SharedState, SubmitterGuard,
};

/// Loop control returned by `scm_*` handlers that contain early exits.
pub(super) enum ScmFlow {
    Continue,
    Break,
}

pub(super) async fn handle_shared_client(
    conn: IpcConnection,
    shared: SharedState,
    mut session: Arc<SharedSession>,
    mut client_id: ClientId,
    broadcast_rx: tokio::sync::broadcast::Receiver<EngineEvent>,
    mut work_cwd: PathBuf,
    mut session_id: String,
) -> (Arc<SharedSession>, ClientId, String, PathBuf) {
    // Split reader/writer: the request loop owns the reader and parks on it
    // waiting for the next request, while spawned turn drains, broadcasts and
    // cron events write through the shared writer without contention.
    let (mut reader, writer) = conn.into_split();

    // Spawn background task to forward broadcast events to this client (Task 5.2)
    let mut broadcast_handle =
        spawn_shared_broadcast(writer.clone(), session.clone(), client_id, broadcast_rx);

    // Spawn background task to forward cron results to this client.
    // Cron jobs run independently (not tied to any session), so their
    // results are delivered via a separate broadcast channel.
    let conn_for_cron = Arc::clone(&writer);
    let mut cron_rx = shared.cron_manager.subscribe();
    let cron_broadcast_handle = tokio::spawn(async move {
        loop {
            match cron_rx.recv().await {
                Ok(cron_result) => {
                    let mut conn_guard = conn_for_cron.lock().await;
                    let params = serde_json::json!({
                        "job_id": cron_result.job_id,
                        "job_name": cron_result.job_name,
                        "text": cron_result.text,
                        "timestamp": cron_result.timestamp,
                    });
                    if conn_guard
                        .send_notification("cron_result", params)
                        .await
                        .is_err()
                    {
                        break; // Client disconnected
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("Cron result receiver lagged by {} events", n);
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    });

    // ── Shared-mode RPC loop ──
    loop {
        if shared.should_exit.load(Ordering::Relaxed) {
            break;
        }

        let msg = {
            match reader.recv_message().await {
                Ok(msg) => msg,
                Err(IpcError::ConnectionClosed) => {
                    eprintln!("Shared client {} disconnected", client_id);
                    break;
                }
                Err(e) => {
                    eprintln!("Shared client {} IPC error: {}", client_id, e);
                    break;
                }
            }
        };

        if let JsonRpcMessage::Request(req) = msg {
            let id = req.id.clone();
            match parse_client_method(&req) {
                Ok(method) => {
                    match method {
                        // ── Task 5.1: submitMessage in shared mode ──
                        ClientMethod::SubmitMessage {
                            prompt,
                            attachments,
                            ..
                        } => {
                            match scm_submit_message(
                                &session,
                                &writer,
                                &shared,
                                client_id,
                                &session_id,
                                id,
                                prompt,
                                attachments,
                            )
                            .await
                            {
                                ScmFlow::Break => break,
                                ScmFlow::Continue => continue,
                            }
                        }

                        // ── Task 5.3: abort — any client can call ──
                        // Known window: an abort landing while a submit's
                        // setup is running (which resets the abort watch) can
                        // be wiped; the window is sub-millisecond outside
                        // auto-compact and predates the concurrent loop.
                        ClientMethod::Abort => {
                            scm_abort(&session, &writer, id).await;
                        }

                        // ── Task 6.2: shutdown in shared mode ──
                        ClientMethod::Shutdown => match scm_shutdown(&shared, &writer, id).await {
                            ScmFlow::Break => break,
                            ScmFlow::Continue => continue,
                        },

                        ClientMethod::UpdateSettings { settings } => {
                            scm_update_settings(&session, &writer, id, settings).await;
                        }

                        ClientMethod::PermissionResponse {
                            tool_use_id,
                            decision,
                            rule,
                        } => {
                            scm_permission_response(
                                &shared,
                                &writer,
                                id,
                                tool_use_id,
                                decision,
                                rule,
                            )
                            .await;
                        }

                        ClientMethod::Initialize { .. } => {
                            scm_initialize(&writer, id).await;
                        }

                        // ── Task 5.3: Read-only operations — always allowed ──
                        ClientMethod::ListTools => {
                            scm_list_tools(&shared, &writer, id).await;
                        }
                        ClientMethod::ListMcpServers => {
                            scm_list_mcp_servers(&work_cwd, &writer, id).await;
                        }
                        ClientMethod::ListSkills => {
                            scm_list_skills(&work_cwd, &writer, id).await;
                        }
                        ClientMethod::ListPlugins => {
                            scm_list_plugins(&work_cwd, &writer, id).await;
                        }
                        ClientMethod::GitStatus => {
                            scm_git_status(&work_cwd, &writer, id).await;
                        }
                        ClientMethod::GitDiff => {
                            scm_git_diff(&work_cwd, &writer, id).await;
                        }

                        // ── Task 5.3: Write operations — blocked if ActiveSubmitter exists ──
                        ClientMethod::Compact => match scm_compact(&session, &writer, id).await {
                            ScmFlow::Break => break,
                            ScmFlow::Continue => continue,
                        },
                        ClientMethod::SwitchModel { model: new_model } => {
                            match scm_switch_model(&session, &shared, &writer, id, new_model).await
                            {
                                ScmFlow::Break => break,
                                ScmFlow::Continue => continue,
                            }
                        }

                        ClientMethod::GitCommit { message } => {
                            scm_git_commit(&work_cwd, &writer, id, message).await;
                        }

                        ClientMethod::TaskCreate {
                            description,
                            prompt,
                        } => {
                            scm_task_create(&shared, &work_cwd, &writer, id, description, prompt)
                                .await;
                        }
                        ClientMethod::TaskList => {
                            scm_task_list(&shared, &writer, id).await;
                        }
                        ClientMethod::TaskStatus { task_id } => {
                            scm_task_status(&shared, &writer, id, task_id).await;
                        }
                        ClientMethod::TaskStop { task_id } => {
                            scm_task_stop(&shared, &writer, id, task_id).await;
                        }
                        ClientMethod::MemoryList => {
                            scm_memory_list(&shared, &writer, id).await;
                        }
                        ClientMethod::MemoryAdd { content, category } => {
                            scm_memory_add(&shared, &writer, id, content, category).await;
                        }
                        ClientMethod::MemoryDelete { id: mem_id } => {
                            scm_memory_delete(&shared, &writer, id, mem_id).await;
                        }
                        ClientMethod::MemoryClear => {
                            scm_memory_clear(&shared, &writer, id).await;
                        }
                        ClientMethod::MemoryStats => {
                            scm_memory_stats(&shared, &writer, id).await;
                        }
                        ClientMethod::MemoryArchive { id: mem_id } => {
                            scm_memory_archive(&shared, &writer, id, mem_id).await;
                        }
                        ClientMethod::MemoryRestore { id: mem_id } => {
                            scm_memory_restore(&shared, &writer, id, mem_id).await;
                        }
                        ClientMethod::MemoryArchiveList => {
                            scm_memory_archive_list(&shared, &writer, id).await;
                        }
                        ClientMethod::MemoryCleanup => {
                            scm_memory_cleanup(&shared, &writer, id).await;
                        }
                        ClientMethod::CronAdd {
                            name,
                            prompt,
                            schedule,
                            cwd,
                        } => {
                            scm_cron_add(&shared, &writer, id, name, prompt, schedule, cwd).await;
                        }
                        ClientMethod::CronRemove { id: job_id } => {
                            scm_cron_remove(&shared, &writer, id, job_id).await;
                        }
                        ClientMethod::CronToggle { id: job_id } => {
                            scm_cron_toggle(&shared, &writer, id, job_id).await;
                        }
                        ClientMethod::CronList => {
                            scm_cron_list(&shared, &writer, id).await;
                        }
                        ClientMethod::ProjectsList => {
                            scm_projects_list(&shared, &writer, id).await;
                        }
                        ClientMethod::ProjectsSwitch { id_prefix } => {
                            scm_projects_switch(
                                &shared,
                                &writer,
                                &mut session,
                                &mut client_id,
                                &mut broadcast_handle,
                                &mut session_id,
                                &mut work_cwd,
                                id,
                                id_prefix,
                            )
                            .await;
                        }
                        ClientMethod::ProjectsNew { cwd, description } => {
                            scm_projects_new(
                                &shared,
                                &writer,
                                &mut session,
                                &mut client_id,
                                &mut broadcast_handle,
                                &mut session_id,
                                &mut work_cwd,
                                id,
                                cwd,
                                description,
                            )
                            .await;
                        }
                        ClientMethod::ProjectsUpdateDesc {
                            id_prefix,
                            description,
                        } => {
                            scm_projects_update_desc(&shared, &writer, id, id_prefix, description)
                                .await;
                        }
                        ClientMethod::TalkTail { count } => {
                            scm_talk_tail(&session, &writer, id, count).await;
                        }
                        ClientMethod::SearchHistory { query, max_results } => {
                            scm_search_history(&session, &writer, id, query, max_results).await;
                        }
                        ClientMethod::DocUpload { file_path } => {
                            scm_doc_upload(&writer, id, file_path).await;
                        }
                        ClientMethod::Export { output_path } => {
                            scm_export(&session, &work_cwd, &writer, id, output_path).await;
                        }

                        // ── Spec-Driven Development RPC ──
                        ClientMethod::SpecNew {
                            feature_name,
                            workflow,
                            spec_type,
                        } => {
                            scm_spec_new(&work_cwd, &writer, id, feature_name, workflow, spec_type)
                                .await;
                        }
                        ClientMethod::SpecList => {
                            scm_spec_list(&work_cwd, &writer, id).await;
                        }
                        ClientMethod::SpecShow { feature_name } => {
                            scm_spec_show(&work_cwd, &writer, id, feature_name).await;
                        }
                        ClientMethod::SpecStatus { feature_name } => {
                            scm_spec_status(&work_cwd, &writer, id, feature_name).await;
                        }
                        ClientMethod::SpecRun {
                            feature_name,
                            task_id,
                        } => {
                            match scm_spec_run(&work_cwd, &writer, id, feature_name, task_id).await
                            {
                                ScmFlow::Break => break,
                                ScmFlow::Continue => continue,
                            }
                        }
                        ClientMethod::SpecEdit {
                            feature_name,
                            phase,
                        } => {
                            scm_spec_edit(&work_cwd, &writer, id, feature_name, phase).await;
                        }

                        // ── Team Management RPC ──
                        ClientMethod::TeamSpawn {
                            count,
                            mode,
                            task,
                            policy,
                        } => {
                            match scm_team_spawn(
                                &shared, &work_cwd, &writer, id, count, mode, task, policy,
                            )
                            .await
                            {
                                ScmFlow::Break => break,
                                ScmFlow::Continue => continue,
                            }
                        }
                        ClientMethod::TeamList => {
                            scm_team_list(&shared, &writer, id).await;
                        }
                        ClientMethod::TeamStatus { team_id } => {
                            scm_team_status(&shared, &writer, id, team_id).await;
                        }
                        ClientMethod::TeamResults { team_id } => {
                            scm_team_results(&shared, &writer, id, team_id).await;
                        }
                        ClientMethod::TeamAbort { team_id } => {
                            scm_team_abort(&shared, &writer, id, team_id).await;
                        }
                        ClientMethod::TeamExecute { team_id } => {
                            scm_team_execute(&shared, &work_cwd, &writer, id, team_id).await;
                        }

                        // ── Template Engine handlers ──
                        ClientMethod::TemplateList => {
                            scm_template_list(&writer, id).await;
                        }
                        ClientMethod::TemplateCreate { json } => {
                            scm_template_create(&writer, id, json).await;
                        }
                        ClientMethod::TemplateDelete { name } => {
                            scm_template_delete(&writer, id, name).await;
                        }
                        ClientMethod::TemplateExport { name } => {
                            scm_template_export(&writer, id, name).await;
                        }
                        ClientMethod::TemplateImport { url } => {
                            scm_template_import(&writer, id, url).await;
                        }

                        // ── Git Integration handlers ──
                        ClientMethod::GitPrList => {
                            scm_git_pr_list(&writer, id).await;
                        }
                        ClientMethod::GitPrCreate {
                            title,
                            body,
                            base,
                            head: _,
                        } => {
                            scm_git_pr_create(&writer, id, title, body, base).await;
                        }
                        ClientMethod::GitBranchList => {
                            scm_git_branch_list(&writer, id).await;
                        }
                        ClientMethod::GitConflictCheck => {
                            scm_git_conflict_check(&writer, id).await;
                        }

                        // ── Model Router handlers ──
                        ClientMethod::ModelList => {
                            scm_model_list(&writer, id).await;
                        }
                        ClientMethod::ModelRoute { task } => {
                            scm_model_route(&writer, id, task).await;
                        }
                        ClientMethod::ModelBudget => {
                            scm_model_budget(&writer, id).await;
                        }

                        // ── Telemetry handlers ──
                        ClientMethod::TelemetryStats => {
                            match scm_telemetry_stats(&shared, &writer, id).await {
                                ScmFlow::Break => break,
                                ScmFlow::Continue => continue,
                            }
                        }
                        ClientMethod::TelemetryTrends { days } => {
                            match scm_telemetry_trends(&writer, id, days).await {
                                ScmFlow::Break => break,
                                ScmFlow::Continue => continue,
                            }
                        }
                        ClientMethod::TelemetryExport { format } => {
                            match scm_telemetry_export(&writer, id, format).await {
                                ScmFlow::Break => break,
                                ScmFlow::Continue => continue,
                            }
                        }
                        ClientMethod::TelemetrySetEnabled { enabled } => {
                            scm_telemetry_set_enabled(&shared, &writer, id, enabled).await;
                        }

                        // ── Permission Gate handlers ──
                        ClientMethod::PermissionStatus => {
                            scm_permission_status(&writer, id).await;
                        }
                        ClientMethod::PermissionGrant {
                            tool,
                            action,
                            target,
                            permanent,
                        } => {
                            scm_permission_grant(
                                &shared, &writer, id, tool, action, target, permanent,
                            )
                            .await;
                        }
                        ClientMethod::PermissionRevoke {
                            tool,
                            action: _action,
                            target,
                        } => {
                            scm_permission_revoke(&shared, &writer, id, tool, target).await;
                        }

                        // ── Permission Manager handlers (rule-based) ──
                        ClientMethod::PermissionsInfo => {
                            scm_permissions_info(&shared, &writer, id).await;
                        }
                        ClientMethod::PermissionsAddRule {
                            category,
                            tool_name,
                            rule_content,
                        } => {
                            scm_permissions_add_rule(
                                &shared,
                                &writer,
                                id,
                                category,
                                tool_name,
                                rule_content,
                            )
                            .await;
                        }
                        ClientMethod::PermissionsRemoveRule {
                            category,
                            tool_name,
                            rule_content,
                        } => {
                            scm_permissions_remove_rule(
                                &shared,
                                &writer,
                                id,
                                category,
                                tool_name,
                                rule_content,
                            )
                            .await;
                        }
                        ClientMethod::PermissionsSetMode { mode } => {
                            scm_permissions_set_mode(&shared, &writer, id, mode).await;
                        }

                        ClientMethod::PermissionsSetAutoAllow { channel, enabled } => {
                            scm_permissions_set_auto_allow(&shared, &writer, id, channel, enabled)
                                .await;
                        }

                        ClientMethod::PermissionsSetAskTimeout { seconds } => {
                            scm_permissions_set_ask_timeout(&shared, &writer, id, seconds).await;
                        }

                        ClientMethod::PermissionsSetPersistGrants { enabled } => {
                            scm_permissions_set_persist_grants(&shared, &writer, id, enabled).await;
                        }

                        ClientMethod::EvolutionRateTrajectory { rating } => {
                            scm_evolution_rate_trajectory(&shared, &writer, id, rating).await;
                        }

                        // ── Session Info / Token / Cost handlers (P2-2) ──
                        ClientMethod::SessionTokens => {
                            scm_session_tokens(&session, &shared, &session_id, &writer, id).await;
                        }
                        ClientMethod::SessionCost => {
                            scm_session_cost(&session, &shared, &writer, id).await;
                        }
                        ClientMethod::SessionInfo => {
                            scm_session_info(
                                &session,
                                &shared,
                                &session_id,
                                &work_cwd,
                                &writer,
                                id,
                            )
                            .await;
                        }
                        ClientMethod::ConfigModel => {
                            scm_config_model(&shared, &writer, id).await;
                        }
                        ClientMethod::ConfigShow => {
                            scm_config_show(&shared, &writer, id).await;
                        }
                        ClientMethod::ToolHealth => {
                            scm_tool_health(&shared, &writer, id).await;
                        }
                    }
                }
                Err(e) => {
                    let mut conn_guard = writer.lock().await;
                    let _ = conn_guard
                        .send_error(Some(id), -32601, format!("{}", e))
                        .await;
                }
            }
        }
    }

    // Cancel the broadcast receiver tasks
    broadcast_handle.abort();
    cron_broadcast_handle.abort();
    (session, client_id, session_id, work_cwd)
}

type WriterRef<'a> = &'a Arc<TokioMutex<IpcWriter>>;

async fn scm_submit_message(
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

async fn scm_abort(session: &Arc<SharedSession>, writer: WriterRef<'_>, id: RequestId) {
    let engine = session.engine_read().await;
    engine.abort();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, serde_json::json!("ok")).await;
}

async fn scm_shutdown(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) -> ScmFlow {
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, serde_json::json!("ok")).await;
    // Shutdown terminates the daemon for all clients
    eprintln!("Shutdown requested — setting should_exit flag");
    shared.should_exit.store(true, Ordering::Relaxed);
    ScmFlow::Break
}

async fn scm_update_settings(
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

async fn scm_permission_response(
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

async fn scm_initialize(writer: WriterRef<'_>, id: RequestId) {
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_error(Some(id), -32600, "Already initialized".into())
        .await;
}

async fn scm_list_tools(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let tl: Vec<serde_json::Value> = shared
        .engine_tools
        .iter()
        .map(
            |t| serde_json::json!({"name": t.name(), "description": t.prompt(), "type": "builtin"}),
        )
        .collect();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"tools": tl, "count": tl.len()}))
        .await;
}

async fn scm_list_mcp_servers(work_cwd: &PathBuf, writer: WriterRef<'_>, id: RequestId) {
    let s = discovery::mcp_config::discover_mcp_servers(work_cwd).await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"servers": s, "count": s.len()}))
        .await;
}

async fn scm_list_skills(work_cwd: &PathBuf, writer: WriterRef<'_>, id: RequestId) {
    let s = discovery::skills::discover_skills(work_cwd).await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"skills": s, "count": s.len()}))
        .await;
}

async fn scm_list_plugins(work_cwd: &PathBuf, writer: WriterRef<'_>, id: RequestId) {
    let p = discovery::plugins::discover_plugins(work_cwd).await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"plugins": p, "count": p.len()}))
        .await;
}

async fn scm_git_status(work_cwd: &PathBuf, writer: WriterRef<'_>, id: RequestId) {
    let mut conn_guard = writer.lock().await;
    match ipc::handlers::git::handle_git_status(std::path::Path::new(work_cwd)) {
        Ok(res) => {
            let _ = conn_guard.send_response(id, res).await;
        }
        Err(err) => {
            let _ = conn_guard.send_error(Some(id), -32000, err).await;
        }
    }
}

async fn scm_git_diff(work_cwd: &PathBuf, writer: WriterRef<'_>, id: RequestId) {
    let output = tokio::process::Command::new("git")
        .args(["diff", "--stat"])
        .current_dir(work_cwd)
        .output()
        .await;
    let mut conn_guard = writer.lock().await;
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            let result = if stdout.trim().is_empty() {
                "No uncommitted changes.".to_string()
            } else {
                stdout
            };
            let _ = conn_guard
                .send_response(id, serde_json::json!({"diff": result}))
                .await;
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("git diff failed: {}", stderr))
                .await;
        }
        Err(e) => {
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("Not a git repository or git not available: {}", e),
                )
                .await;
        }
    }
}

async fn scm_compact(
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

async fn scm_switch_model(
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

async fn scm_git_commit(work_cwd: &PathBuf, writer: WriterRef<'_>, id: RequestId, message: String) {
    let add_result = tokio::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(work_cwd)
        .output()
        .await;
    let mut conn_guard = writer.lock().await;
    match add_result {
        Ok(o) if o.status.success() => {
            let commit_result = tokio::process::Command::new("git")
                .args(["commit", "-m", &message])
                .current_dir(work_cwd)
                .output()
                .await;
            match commit_result {
                Ok(co) if co.status.success() => {
                    let hash = tokio::process::Command::new("git")
                        .args(["rev-parse", "--short", "HEAD"])
                        .current_dir(work_cwd)
                        .output()
                        .await
                        .ok()
                        .and_then(|h| String::from_utf8(h.stdout).ok())
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    let _ = conn_guard
                        .send_response(id, serde_json::json!({"hash": hash, "message": message}))
                        .await;
                }
                Ok(co) => {
                    let stderr = String::from_utf8_lossy(&co.stderr).to_string();
                    let stdout = String::from_utf8_lossy(&co.stdout).to_string();
                    let msg = if stderr.is_empty() { stdout } else { stderr };
                    let _ = conn_guard
                        .send_error(Some(id), -32000, format!("git commit failed: {}", msg))
                        .await;
                }
                Err(e) => {
                    let _ = conn_guard
                        .send_error(Some(id), -32000, format!("git commit error: {}", e))
                        .await;
                }
            }
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("git add failed: {}", stderr))
                .await;
        }
        Err(e) => {
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("Not a git repository or git not available: {}", e),
                )
                .await;
        }
    }
}

async fn scm_task_create(
    shared: &SharedState,
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    description: String,
    prompt: String,
) {
    let task_id = shared
        .task_manager
        .create_task(
            description,
            prompt,
            std::path::PathBuf::from(work_cwd),
            shared.state_manager.get().model,
        )
        .await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"task_id": task_id}))
        .await;
}

async fn scm_task_list(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let tasks = shared.task_manager.list_tasks().await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"tasks": tasks, "count": tasks.len()}),
        )
        .await;
}

async fn scm_task_status(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    task_id: String,
) {
    match shared.task_manager.get_task_status(&task_id).await {
        Some(task) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard.send_response(id, serde_json::json!(task)).await;
        }
        None => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Task not found: {}", task_id))
                .await;
        }
    }
}

async fn scm_task_stop(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    task_id: String,
) {
    let stopped = shared.task_manager.stop_task(&task_id).await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"stopped": stopped}))
        .await;
}

async fn scm_memory_list(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let entries = shared.memory_store.list().await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"memories": entries, "count": entries.len()}),
        )
        .await;
}

async fn scm_memory_add(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    content: String,
    category: String,
) {
    let cat = engine::memory::parse_category(&category);
    let result = shared
        .memory_store
        .add(content, cat, "user".to_string())
        .await;
    let mut conn_guard = writer.lock().await;
    match result {
        Ok(entry) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"memory": entry}))
                .await;
        }
        Err(e) => {
            eprintln!("ERROR: memory add failed: {}", e);
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Memory write failed: {}", e))
                .await;
        }
    }
}

async fn scm_memory_delete(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    mem_id: String,
) {
    let result = shared.memory_store.delete(&mem_id).await;
    let mut conn_guard = writer.lock().await;
    match result {
        Ok(deleted) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"deleted": deleted}))
                .await;
        }
        Err(e) => {
            eprintln!("ERROR: memory delete failed: {}", e);
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Memory delete failed: {}", e))
                .await;
        }
    }
}

async fn scm_memory_clear(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let result = shared.memory_store.clear().await;
    let mut conn_guard = writer.lock().await;
    match result {
        Ok(count) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"cleared": count}))
                .await;
        }
        Err(e) => {
            eprintln!("ERROR: memory clear failed: {}", e);
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Memory clear failed: {}", e))
                .await;
        }
    }
}

async fn scm_memory_stats(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let stats = shared.memory_store.stats().await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, serde_json::json!(stats)).await;
}

async fn scm_memory_archive(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    mem_id: String,
) {
    let archived = shared
        .memory_store
        .archive_by_id(&mem_id, &shared.memory_archive)
        .await;
    let mut conn_guard = writer.lock().await;
    match archived {
        Some(entry) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"archived": entry}))
                .await;
        }
        None => {
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Memory not found: {}", mem_id))
                .await;
        }
    }
}

async fn scm_memory_restore(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    mem_id: String,
) {
    let restored = shared
        .memory_store
        .restore_from_archive(&mem_id, &shared.memory_archive)
        .await;
    let mut conn_guard = writer.lock().await;
    match restored {
        Some(entry) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"restored": entry}))
                .await;
        }
        None => {
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("Archived memory not found: {}", mem_id),
                )
                .await;
        }
    }
}

async fn scm_memory_archive_list(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let archived = shared.memory_archive.list_archived().await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"archived": archived, "count": archived.len()}),
        )
        .await;
}

async fn scm_memory_cleanup(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let result = shared.memory_cleanup.run_now().await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "archived_count": result.archived_count,
                "deleted_count": result.deleted_count,
                "timestamp": result.timestamp,
                "duration_ms": result.duration_ms,
            }),
        )
        .await;
}

async fn scm_cron_add(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    name: String,
    prompt: String,
    schedule: String,
    cwd: Option<String>,
) {
    let mut conn_guard = writer.lock().await;
    match shared
        .cron_manager
        .add_job(name, prompt, schedule, cwd)
        .await
    {
        Ok(job) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"job": job}))
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32000, e).await;
        }
    }
}

async fn scm_cron_remove(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    job_id: String,
) {
    let removed = shared.cron_manager.remove_job(&job_id).await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"removed": removed}))
        .await;
}

async fn scm_cron_toggle(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    job_id: String,
) {
    let mut conn_guard = writer.lock().await;
    match shared.cron_manager.toggle_job(&job_id).await {
        Some(enabled) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"enabled": enabled}))
                .await;
        }
        None => {
            let _ = conn_guard
                .send_error(Some(id), -32000, "Job not found".to_string())
                .await;
        }
    }
}

async fn scm_cron_list(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let jobs = shared.cron_manager.list_jobs().await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"jobs": jobs, "count": jobs.len()}))
        .await;
}

async fn scm_projects_list(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let projects = shared.project_registry.list().await;
    // Enrich each project with its session_id (derived from cwd hash)
    let enriched: Vec<serde_json::Value> = projects
        .iter()
        .map(|p| {
            let session_key = cwd_hash(&p.cwd);
            let mut v = serde_json::to_value(p).unwrap_or_default();
            v["session_id"] = serde_json::json!(session_key);
            v
        })
        .collect();
    let count = enriched.len();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"projects": enriched, "count": count}),
        )
        .await;
}

async fn scm_projects_switch(
    shared: &SharedState,
    writer: WriterRef<'_>,
    session: &mut Arc<SharedSession>,
    client_id: &mut ClientId,
    broadcast_handle: &mut tokio::task::JoinHandle<()>,
    session_id: &mut String,
    work_cwd: &mut PathBuf,
    id: RequestId,
    id_prefix: String,
) {
    let mut conn_guard = writer.lock().await;
    match shared.project_registry.find_by_prefix(&id_prefix).await {
        Ok(project) => {
            let abs_cwd = std::path::PathBuf::from(&project.cwd);
            if !abs_cwd.is_dir() {
                let _ = conn_guard
                    .send_error(
                        Some(id),
                        -32000,
                        format!("Directory does not exist: {}", project.cwd),
                    )
                    .await;
            } else {
                drop(conn_guard);
                match switch_shared_client(
                    shared,
                    writer,
                    session,
                    client_id,
                    broadcast_handle,
                    session_id,
                    work_cwd,
                    abs_cwd.clone(),
                )
                .await
                {
                    Ok(message_count) => {
                        shared.project_registry.touch(&project.cwd).await;
                        let mut conn_guard = writer.lock().await;
                        let _ = conn_guard
                            .send_response(
                                id,
                                serde_json::json!({
                                    "project": project,
                                    "message_count": message_count,
                                    "session_id": session_id,
                                }),
                            )
                            .await;
                    }
                    Err(error) => {
                        let mut conn_guard = writer.lock().await;
                        let _ = conn_guard.send_error(Some(id), -32003, error).await;
                    }
                }
            }
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32000, e).await;
        }
    }
}

async fn scm_projects_new(
    shared: &SharedState,
    writer: WriterRef<'_>,
    session: &mut Arc<SharedSession>,
    client_id: &mut ClientId,
    broadcast_handle: &mut tokio::task::JoinHandle<()>,
    session_id: &mut String,
    work_cwd: &mut PathBuf,
    id: RequestId,
    cwd: String,
    description: Option<String>,
) {
    let expanded = if cwd.starts_with('~') {
        let home = std::env::var("HOME").unwrap_or_default();
        cwd.replacen('~', &home, 1)
    } else if std::path::Path::new(&cwd).is_relative() {
        work_cwd.join(&cwd).to_string_lossy().to_string()
    } else {
        cwd.clone()
    };
    let abs_path = std::path::PathBuf::from(&expanded);
    if !abs_path.is_dir() {
        let mut conn_guard = writer.lock().await;
        let _ = conn_guard
            .send_error(
                Some(id),
                -32000,
                format!("Directory does not exist: {}", expanded),
            )
            .await;
    } else {
        let desc = description.unwrap_or_else(|| {
            abs_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| expanded.clone())
        });
        let mut conn_guard = writer.lock().await;
        match shared
            .project_registry
            .register(expanded.clone(), desc)
            .await
        {
            Ok(project) => {
                // Auto-scaffold
                let baoclaw_dir = abs_path.join(".baoclaw");
                if !baoclaw_dir.exists() {
                    if let Err(e) = std::fs::create_dir_all(&baoclaw_dir) {
                        eprintln!(
                            "[projects] WARNING: could not create {}: {}",
                            baoclaw_dir.display(),
                            e
                        );
                    }
                    if let Err(e) =
                        std::fs::write(baoclaw_dir.join("BAOCLAW.md"), "# Project Instructions\n\n")
                    {
                        eprintln!("[projects] WARNING: could not write BAOCLAW.md: {}", e);
                    }
                    if let Err(e) =
                        std::fs::write(baoclaw_dir.join("mcp.json"), "{\"mcpServers\":{}}\n")
                    {
                        eprintln!("[projects] WARNING: could not write mcp.json: {}", e);
                    }
                    if let Err(e) = std::fs::create_dir_all(baoclaw_dir.join("skills")) {
                        eprintln!("[projects] WARNING: could not create skills dir: {}", e);
                    }
                }
                drop(conn_guard);
                match switch_shared_client(
                    shared,
                    writer,
                    session,
                    client_id,
                    broadcast_handle,
                    session_id,
                    work_cwd,
                    abs_path,
                )
                .await
                {
                    Ok(message_count) => {
                        let mut conn_guard = writer.lock().await;
                        let _ = conn_guard
                            .send_response(
                                id,
                                serde_json::json!({
                                    "project": project,
                                    "switched": true,
                                    "message_count": message_count,
                                    "session_id": session_id,
                                }),
                            )
                            .await;
                    }
                    Err(error) => {
                        let mut conn_guard = writer.lock().await;
                        let _ = conn_guard.send_error(Some(id), -32003, error).await;
                    }
                }
            }
            Err(e) => {
                let _ = conn_guard.send_error(Some(id), -32000, e).await;
            }
        }
    }
}

async fn scm_projects_update_desc(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    id_prefix: String,
    description: String,
) {
    let mut conn_guard = writer.lock().await;
    match shared
        .project_registry
        .update_description(&id_prefix, description)
        .await
    {
        Ok(()) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"updated": true}))
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32000, e).await;
        }
    }
}

async fn scm_talk_tail(
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

async fn scm_search_history(
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
                    let start = idx.saturating_sub(50);
                    let end = (idx + query.len() + 100).min(text.len());
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

async fn scm_doc_upload(writer: WriterRef<'_>, id: RequestId, file_path: String) {
    let path = std::path::Path::new(&file_path);
    let mut conn_guard = writer.lock().await;
    match doc_upload::build_attachment_from_file(path) {
        Ok(attachment) => {
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "attachment": attachment,
                        "file_path": file_path,
                    }),
                )
                .await;
        }
        Err(e) => {
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Document upload failed: {}", e))
                .await;
        }
    }
}

async fn scm_export(
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

async fn scm_spec_new(
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    feature_name: String,
    workflow: Option<String>,
    spec_type: Option<String>,
) {
    let spec_engine = engine::spec_engine::SpecEngine::new(work_cwd.clone());
    let wf = match workflow.as_deref() {
        Some("design") => engine::spec_engine::SpecWorkflow::DesignFirst,
        _ => engine::spec_engine::SpecWorkflow::RequirementsFirst,
    };
    let st = match spec_type.as_deref() {
        Some("bugfix") => engine::spec_engine::SpecType::Bugfix,
        _ => engine::spec_engine::SpecType::Feature,
    };
    let mut conn_guard = writer.lock().await;
    match spec_engine.create_spec(&feature_name, wf, st) {
        Ok(config) => {
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "status": "created",
                        "feature_name": feature_name,
                        "config": serde_json::to_value(&config).unwrap_or_default()
                    }),
                )
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32001, e.to_string()).await;
        }
    }
}

async fn scm_spec_list(work_cwd: &PathBuf, writer: WriterRef<'_>, id: RequestId) {
    let spec_engine = engine::spec_engine::SpecEngine::new(work_cwd.clone());
    let mut conn_guard = writer.lock().await;
    match spec_engine.list_specs() {
        Ok(specs) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"specs": specs}))
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32000, e.to_string()).await;
        }
    }
}

async fn scm_spec_show(
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    feature_name: String,
) {
    let spec_engine = engine::spec_engine::SpecEngine::new(work_cwd.clone());
    let mut conn_guard = writer.lock().await;
    match spec_engine.get_spec(&feature_name) {
        Ok(summary) => {
            let _ = conn_guard
                .send_response(id, serde_json::to_value(&summary).unwrap_or_default())
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32001, e.to_string()).await;
        }
    }
}

async fn scm_spec_status(
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    feature_name: String,
) {
    let spec_engine = engine::spec_engine::SpecEngine::new(work_cwd.clone());
    let mut conn_guard = writer.lock().await;
    match spec_engine.get_status(&feature_name) {
        Ok(progress) => {
            let _ = conn_guard
                .send_response(id, serde_json::to_value(&progress).unwrap_or_default())
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32001, e.to_string()).await;
        }
    }
}

async fn scm_spec_run(
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    feature_name: String,
    task_id: Option<String>,
) -> ScmFlow {
    let spec_engine = engine::spec_engine::SpecEngine::new(work_cwd.clone());
    let mut conn_guard = writer.lock().await;
    let task = if let Some(_tid) = &task_id {
        // Find specific task
        match spec_engine.next_task(&feature_name) {
            Ok(t) => t,
            Err(e) => {
                let _ = conn_guard.send_error(Some(id), -32001, e.to_string()).await;
                return ScmFlow::Continue;
            }
        }
    } else {
        match spec_engine.next_task(&feature_name) {
            Ok(t) => t,
            Err(e) => {
                let _ = conn_guard.send_error(Some(id), -32001, e.to_string()).await;
                return ScmFlow::Continue;
            }
        }
    };
    match task {
        Some(t) => {
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "status": "ready",
                        "task_id": t.id,
                        "task_description": t.description,
                    }),
                )
                .await;
        }
        None => {
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "status": "all_complete",
                        "message": "All tasks are completed"
                    }),
                )
                .await;
        }
    }
    ScmFlow::Continue
}

async fn scm_spec_edit(
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    feature_name: String,
    phase: String,
) {
    let spec_engine = engine::spec_engine::SpecEngine::new(work_cwd.clone());
    let mut conn_guard = writer.lock().await;
    match spec_engine.read_phase_doc(&feature_name, &phase) {
        Ok(content) => {
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "feature_name": feature_name,
                        "phase": phase,
                        "content": content,
                    }),
                )
                .await;
        }
        Err(e) => {
            let _ = conn_guard.send_error(Some(id), -32001, e.to_string()).await;
        }
    }
}

async fn scm_team_spawn(
    shared: &SharedState,
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    count: Option<usize>,
    mode: String,
    task: String,
    policy: Option<serde_json::Value>,
) -> ScmFlow {
    use engine::team::{TeamConfig, TeamExecutor, TeamMode as EngineTeamMode, TeamPolicy};
    use std::str::FromStr;

    // Parse mode
    let team_mode = match EngineTeamMode::from_str(&mode) {
        Ok(m) => m,
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard.send_error(Some(id), -32602, e).await;
            return ScmFlow::Continue;
        }
    };

    // Parse policy if provided
    let team_policy: Option<TeamPolicy> = match policy {
        Some(p) => match serde_json::from_value(p) {
            Ok(policy) => Some(policy),
            Err(e) => {
                let mut conn_guard = writer.lock().await;
                let _ = conn_guard
                    .send_error(Some(id), -32602, format!("Invalid policy: {}", e))
                    .await;
                return ScmFlow::Continue;
            }
        },
        None => None,
    };

    // Create team config
    let config = TeamConfig {
        mode: team_mode.clone(),
        policy: team_policy,
        cwd: Some(work_cwd.to_string_lossy().to_string()),
        model: Some(shared.state_manager.get().model.clone()),
        ..Default::default()
    };

    // Create the executor and team
    let executor = TeamExecutor::new(
        Arc::clone(&shared.api_client),
        shared.engine_tools.clone(),
        work_cwd.clone(),
        shared.state_manager.get().model.clone(),
        shared.headless_kit.clone(),
    );

    match executor.create_team(task.clone(), config).await {
        Ok(mut team) => {
            // For parallel mode, create the specified number of agents
            if team_mode == EngineTeamMode::Parallel {
                if let Err(e) = executor
                    .add_parallel_agents(&mut team, count.unwrap_or(1), &task)
                    .await
                {
                    let mut conn_guard = writer.lock().await;
                    let _ = conn_guard
                        .send_error(
                            Some(id),
                            -32000,
                            format!("Failed to add agents: {}", e.message),
                        )
                        .await;
                    return ScmFlow::Continue;
                }
            }

            let team_id = team.id.clone();
            let team_json = serde_json::to_value(&team).unwrap_or_default();

            // Store the team
            shared.team_executor.store_team(team).await;

            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "team_id": team_id,
                        "team": team_json,
                        "message": "Team created successfully"
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

async fn scm_team_list(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let teams = shared.team_executor.list_teams().await;
    let count = teams.len();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "teams": teams,
                "count": count
            }),
        )
        .await;
}

async fn scm_team_status(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    team_id: String,
) {
    match shared.team_executor.get_team(&team_id).await {
        Some(team) => {
            let summary = team.summary();
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "team": team,
                        "summary": summary
                    }),
                )
                .await;
        }
        None => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Team not found: {}", team_id))
                .await;
        }
    }
}

async fn scm_team_results(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    team_id: String,
) {
    match shared.team_executor.get_team(&team_id).await {
        Some(team) => {
            let results = team.collect_results();
            let agents: Vec<serde_json::Value> = team
                .agents
                .iter()
                .map(|a| {
                    serde_json::json!({
                        "id": a.id,
                        "status": a.status.to_string(),
                        "result": a.result,
                        "error": a.error,
                        "tokens_used": a.tokens_used,
                        "cost_usd": a.cost_usd,
                    })
                })
                .collect();
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "team_id": team_id,
                        "status": team.status.to_string(),
                        "results": results,
                        "agents": agents,
                        "total_tokens": team.total_tokens,
                        "total_cost_usd": team.total_cost_usd,
                    }),
                )
                .await;
        }
        None => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Team not found: {}", team_id))
                .await;
        }
    }
}

async fn scm_team_abort(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    team_id: String,
) {
    match shared.team_executor.abort_team(&team_id).await {
        Some(team) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "team_id": team_id,
                        "status": team.status.to_string(),
                        "message": "Team aborted"
                    }),
                )
                .await;
        }
        None => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Team not found: {}", team_id))
                .await;
        }
    }
}

async fn scm_team_execute(
    shared: &SharedState,
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    team_id: String,
) {
    match shared.team_executor.get_team(&team_id).await {
        Some(team) => {
            // Spawn execution in background
            let executor = engine::team::TeamExecutor::new(
                Arc::clone(&shared.api_client),
                shared.engine_tools.clone(),
                work_cwd.clone(),
                shared.state_manager.get().model.clone(),
                shared.headless_kit.clone(),
            );

            // Execute the team
            let result = executor.execute(team).await;

            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "team_id": result.team.id,
                        "success": result.success,
                        "error": result.error,
                        "duration_ms": result.duration_ms,
                        "status": result.team.status.to_string(),
                        "total_tokens": result.team.total_tokens,
                        "total_cost_usd": result.team.total_cost_usd,
                    }),
                )
                .await;
        }
        None => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Team not found: {}", team_id))
                .await;
        }
    }
}

async fn scm_template_list(writer: WriterRef<'_>, id: RequestId) {
    let engine = engine::template::engine::TemplateEngine::new();
    let templates = engine.list_all();
    let result: Vec<serde_json::Value> = templates
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "trigger": t.trigger,
                "description": t.description,
                "version": t.version,
                "author": t.author,
                "builtin": t.builtin,
                "tags": t.tags,
                "variables_count": t.variables.len(),
                "steps_count": t.workflow.len(),
            })
        })
        .collect();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"templates": result, "count": result.len()}),
        )
        .await;
}

async fn scm_template_create(writer: WriterRef<'_>, id: RequestId, json: String) {
    let mut engine = engine::template::engine::TemplateEngine::new();
    match serde_json::from_str::<engine::template::types::Template>(&json) {
        Ok(template) => match engine.create_template(&template) {
            Ok(()) => {
                let mut conn_guard = writer.lock().await;
                let _ = conn_guard
                    .send_response(
                        id,
                        serde_json::json!({"success": true, "name": template.name}),
                    )
                    .await;
            }
            Err(e) => {
                let mut conn_guard = writer.lock().await;
                let _ = conn_guard
                    .send_error(
                        Some(id),
                        -32000,
                        format!("Failed to create template: {}", e),
                    )
                    .await;
            }
        },
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Invalid template JSON: {}", e))
                .await;
        }
    }
}

async fn scm_template_delete(writer: WriterRef<'_>, id: RequestId, name: String) {
    let mut engine = engine::template::engine::TemplateEngine::new();
    match engine.delete_template(&name) {
        Ok(()) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(id, serde_json::json!({"success": true, "name": name}))
                .await;
        }
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("Failed to delete template: {}", e),
                )
                .await;
        }
    }
}

async fn scm_template_export(writer: WriterRef<'_>, id: RequestId, name: String) {
    let engine = engine::template::engine::TemplateEngine::new();
    match engine.export_template(&name) {
        Ok(json_str) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard.send_response(id, serde_json::json!({"name": name, "template": serde_json::from_str::<serde_json::Value>(&json_str).unwrap_or_default()})).await;
        }
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("Failed to export template: {}", e),
                )
                .await;
        }
    }
}

async fn scm_template_import(writer: WriterRef<'_>, id: RequestId, url: String) {
    let mut engine = engine::template::engine::TemplateEngine::new();
    match engine.import_template_url(&url).await {
        Ok(template) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard.send_response(id, serde_json::json!({"success": true, "name": template.name, "trigger": template.trigger})).await;
        }
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("Failed to import template: {}", e),
                )
                .await;
        }
    }
}

async fn scm_git_pr_list(writer: WriterRef<'_>, id: RequestId) {
    let result = match engine::git_integration::pr::PrManager::list_prs(None).await {
        Ok(prs) => {
            let pr_list: Vec<serde_json::Value> = prs
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "number": p.number,
                        "title": p.title,
                        "state": p.state,
                        "author": p.author,
                        "base_branch": p.base_branch,
                        "head_branch": p.head_branch,
                        "created_at": p.created_at,
                        "url": p.url,
                    })
                })
                .collect();
            serde_json::json!({"pull_requests": pr_list, "count": pr_list.len()})
        }
        Err(e) => serde_json::json!({"error": format!("{}", e)}),
    };
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

async fn scm_git_pr_create(
    writer: WriterRef<'_>,
    id: RequestId,
    title: String,
    body: String,
    base: String,
) {
    let body_opt = if body.is_empty() {
        None
    } else {
        Some(body.as_str())
    };
    let base_opt = if base.is_empty() {
        None
    } else {
        Some(base.as_str())
    };
    let result =
        match engine::git_integration::pr::PrManager::create_pr(&title, body_opt, base_opt).await {
            Ok(pr) => serde_json::json!({
                "success": true,
                "number": pr.number,
                "title": pr.title,
                "url": pr.url,
            }),
            Err(e) => {
                serde_json::json!({"success": false, "error": format!("{}", e)})
            }
        };
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

async fn scm_git_branch_list(writer: WriterRef<'_>, id: RequestId) {
    let result = match engine::git_integration::branch::BranchManager::list_branches().await {
        Ok(branches) => {
            let branch_list: Vec<serde_json::Value> = branches
                .iter()
                .map(|b| {
                    serde_json::json!({
                        "name": b.name,
                        "is_current": b.is_current,
                        "ahead": b.ahead,
                        "behind": b.behind,
                        "last_commit": b.last_commit,
                        "last_commit_msg": b.last_commit_msg,
                    })
                })
                .collect();
            serde_json::json!({"branches": branch_list, "count": branch_list.len()})
        }
        Err(e) => serde_json::json!({"error": format!("{}", e)}),
    };
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

async fn scm_git_conflict_check(writer: WriterRef<'_>, id: RequestId) {
    let result = match engine::git_integration::conflict::ConflictResolver::detect_conflicts().await
    {
        Ok(conflicts) => {
            let conflict_list: Vec<serde_json::Value> = conflicts
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "file": c.file,
                        "resolved": c.resolved,
                    })
                })
                .collect();
            serde_json::json!({"conflicts": conflict_list, "count": conflict_list.len(), "has_conflicts": !conflicts.is_empty()})
        }
        Err(e) => serde_json::json!({"error": format!("{}", e)}),
    };
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

async fn scm_model_list(writer: WriterRef<'_>, id: RequestId) {
    let result = ipc::handlers::model::handle_model_list();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

async fn scm_model_route(writer: WriterRef<'_>, id: RequestId, task: String) {
    let result = ipc::handlers::model::handle_model_route(&task);
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

async fn scm_model_budget(writer: WriterRef<'_>, id: RequestId) {
    let result = ipc::handlers::model::handle_model_budget();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

async fn scm_telemetry_stats(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
) -> ScmFlow {
    let collector = match engine::telemetry::collector::TelemetryCollector::new() {
        Ok(c) => c,
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Telemetry unavailable: {}", e))
                .await;
            return ScmFlow::Continue;
        }
    };
    match collector.get_stats() {
        Ok(stats) => {
            // Recording switch lives on the shared collector (None = the DB
            // never opened, i.e. recording is off).
            let enabled = shared.telemetry.as_ref().is_some_and(|c| c.is_enabled());
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "total_turns": stats.total_turns,
                        "total_tokens": stats.total_tokens,
                        "total_cost_usd": stats.total_cost_usd,
                        "total_tools_called": stats.total_tools_called,
                        "sessions_count": stats.sessions_count,
                        "files_modified": stats.files_modified,
                        "avg_response_time_ms": stats.avg_response_time_ms,
                        "most_used_tool": stats.most_used_tool,
                        "enabled": enabled,
                    }),
                )
                .await;
        }
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Failed to get stats: {}", e))
                .await;
        }
    }
    ScmFlow::Continue
}

async fn scm_telemetry_trends(writer: WriterRef<'_>, id: RequestId, days: u32) -> ScmFlow {
    let collector = match engine::telemetry::collector::TelemetryCollector::new() {
        Ok(c) => c,
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Telemetry unavailable: {}", e))
                .await;
            return ScmFlow::Continue;
        }
    };
    let daily = match collector.get_daily_stats(days) {
        Ok(d) => d,
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Failed to get trends: {}", e))
                .await;
            return ScmFlow::Continue;
        }
    };
    let daily_list: Vec<serde_json::Value> = daily
        .iter()
        .map(|d| {
            serde_json::json!({
                "date": d.date,
                "turns": d.turns,
                "tokens": d.tokens,
                "cost": d.cost,
                "tools": d.tools,
                "sessions": d.sessions,
            })
        })
        .collect();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"days": days, "daily": daily_list, "count": daily_list.len()}),
        )
        .await;
    ScmFlow::Continue
}

async fn scm_telemetry_export(writer: WriterRef<'_>, id: RequestId, format: String) -> ScmFlow {
    let collector = match engine::telemetry::collector::TelemetryCollector::new() {
        Ok(c) => c,
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Telemetry unavailable: {}", e))
                .await;
            return ScmFlow::Continue;
        }
    };
    let exporter = engine::telemetry::export::TelemetryExporter::new(collector);
    let result = match format.to_lowercase().as_str() {
        "json" => exporter.export_json(None),
        "csv" => exporter.export_csv(None),
        "summary" | "md" | "markdown" => exporter.export_summary(),
        _ => Err(format!(
            "Unknown export format: {}. Use json, csv, or summary.",
            format
        )),
    };
    match result {
        Ok(data) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(id, serde_json::json!({"format": format, "data": data}))
                .await;
        }
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Export failed: {}", e))
                .await;
        }
    }
    ScmFlow::Continue
}

async fn scm_permission_status(writer: WriterRef<'_>, id: RequestId) {
    let gate = engine::permission_gate::gate::RuleBasedPermissionGate::new();
    let rules = gate.list_rules();
    let rule_list: Vec<serde_json::Value> = rules
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "description": r.description,
                "tool": r.tool,
                "action": r.action,
                "target_pattern": r.target_pattern,
                "require_confirmation": r.require_confirmation,
                "auto_deny": r.auto_deny,
            })
        })
        .collect();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"rules": rule_list, "count": rule_list.len()}),
        )
        .await;
}

async fn scm_permission_grant(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    tool: String,
    action: String,
    target: String,
    permanent: bool,
) {
    let rule_content = if target.is_empty() || target == "*" {
        None
    } else {
        Some(target.clone())
    };
    let category = match action.to_ascii_lowercase().as_str() {
        "deny" => "deny",
        "ask" => "ask",
        _ => "allow",
    };
    {
        let mgr = shared.permission_manager.write().await;
        mgr.add_rule(category, "user", &tool, rule_content.clone());
        if permanent {
            crate::permissions::persist_context_to_config(&mgr.get_context());
        }
    }
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "tool": tool,
                "action": action,
                "target": target,
                "permanent": permanent
            }),
        )
        .await;
}

async fn scm_permission_revoke(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    tool: String,
    target: String,
) {
    let rule_content = if target.is_empty() || target == "*" {
        None
    } else {
        Some(target.as_str())
    };
    let removed = {
        let mgr = shared.permission_manager.write().await;
        let count = mgr.remove_rule(None, &tool, rule_content);
        if count > 0 {
            crate::permissions::persist_context_to_config(&mgr.get_context());
        }
        count
    };
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"success": removed > 0, "removed": removed}),
        )
        .await;
}

async fn scm_permissions_info(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let mgr = shared.permission_manager.read().await;
    let ctx = mgr.get_context();
    let perms = serde_json::to_value(&ctx).unwrap_or(serde_json::json!({
        "mode": "default",
        "always_allow_rules": {},
        "always_deny_rules": {},
        "always_ask_rules": {}
    }));
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, perms).await;
}

async fn scm_permissions_add_rule(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    category: String,
    tool_name: String,
    rule_content: Option<String>,
) {
    {
        let mgr = shared.permission_manager.write().await;
        mgr.add_rule(&category, "config", &tool_name, rule_content.clone());
        crate::permissions::persist_context_to_config(&mgr.get_context());
    }
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "message": "Rule added and persisted to config.",
                "category": category,
                "tool_name": tool_name,
                "rule_content": rule_content
            }),
        )
        .await;
}

async fn scm_permissions_remove_rule(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    category: String,
    tool_name: String,
    rule_content: Option<String>,
) {
    let removed = {
        let mgr = shared.permission_manager.write().await;
        let count = mgr.remove_rule(Some(&category), &tool_name, rule_content.as_deref());
        if count > 0 {
            crate::permissions::persist_context_to_config(&mgr.get_context());
        }
        count
    };
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": removed > 0,
                "message": format!("Removed {} rule(s)", removed),
                "category": category,
                "tool_name": tool_name,
                "rule_content": rule_content
            }),
        )
        .await;
}

async fn scm_permissions_set_mode(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    mode: String,
) {
    let parsed_mode = match mode.to_ascii_lowercase().as_str() {
        "plan" => permissions::manager::PermissionMode::Plan,
        "bypass" | "bypasspermissions" => permissions::manager::PermissionMode::BypassPermissions,
        "auto" => permissions::manager::PermissionMode::Auto,
        _ => permissions::manager::PermissionMode::Default,
    };
    {
        let mgr = shared.permission_manager.write().await;
        mgr.set_mode(parsed_mode);
        crate::permissions::persist_context_to_config(&mgr.get_context());
    }
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "mode": mode,
                "message": format!("Permission mode updated to {}", mode)
            }),
        )
        .await;
}

async fn scm_permissions_set_auto_allow(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    channel: String,
    enabled: bool,
) {
    // Persisted knob for clients that answer their own
    // permission prompts (the TUI toggle; see ToolPermissionContext::
    // auto_allow_channels). Enforcement is client-side.
    {
        let mgr = shared.permission_manager.write().await;
        mgr.update_context(|c| {
            c.auto_allow_channels.insert(channel.clone(), enabled);
        });
        crate::permissions::persist_context_to_config(&mgr.get_context());
    }
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "channel": channel,
                "enabled": enabled,
                "message": format!(
                    "Auto-allow for channel '{}' {}",
                    channel,
                    if enabled { "enabled" } else { "disabled" }
                )
            }),
        )
        .await;
}

async fn scm_telemetry_set_enabled(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    enabled: bool,
) {
    // Live switch on the shared collector (both record paths read it
    // per-event), then persisted to config.json so the choice survives
    // daemon restarts — same idiom as the permissions.* knobs.
    if let Some(ref telemetry) = shared.telemetry {
        telemetry.set_enabled(enabled);
    }
    let mut cfg = crate::config::load_config();
    cfg.telemetry_enabled = enabled;
    if let Err(e) = cfg.save() {
        eprintln!(
            "[telemetry] WARN: could not persist enabled={}: {}",
            enabled, e
        );
    }
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "enabled": enabled,
                "message": format!(
                    "Telemetry {}",
                    if enabled { "enabled" } else { "disabled" }
                )
            }),
        )
        .await;
}

async fn scm_permissions_set_ask_timeout(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    seconds: u64,
) {
    // Persisted prompt-timeout knob, read live by the
    // executor at every prompt. Reject out-of-range
    // values with an error instead of clamping.
    if (5..=3600).contains(&seconds) {
        {
            let mgr = shared.permission_manager.write().await;
            mgr.update_context(|c| c.ask_timeout_secs = seconds);
            crate::permissions::persist_context_to_config(&mgr.get_context());
        }
        let mut conn_guard = writer.lock().await;
        let _ = conn_guard
            .send_response(
                id,
                serde_json::json!({
                    "success": true,
                    "seconds": seconds,
                    "message": format!(
                        "Permission ask timeout set to {}s",
                        seconds
                    )
                }),
            )
            .await;
    } else {
        let mut conn_guard = writer.lock().await;
        let _ = conn_guard
            .send_error(
                Some(id),
                -32000,
                format!("ask timeout must be 5-3600 seconds, got {}", seconds),
            )
            .await;
    }
}

async fn scm_permissions_set_persist_grants(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    enabled: bool,
) {
    // Persisted knob: when false, allow-always grants
    // still allow the current tool but are not written
    // back to config.json.
    {
        let mgr = shared.permission_manager.write().await;
        mgr.update_context(|c| c.persist_grants = enabled);
        crate::permissions::persist_context_to_config(&mgr.get_context());
    }
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "enabled": enabled,
                "message": format!(
                    "Allow-always grant persistence {}",
                    if enabled { "enabled" } else { "disabled" }
                )
            }),
        )
        .await;
}

async fn scm_session_tokens(
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

async fn scm_session_cost(
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

async fn scm_session_info(
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

async fn scm_tool_health(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    use engine::tool_health::ToolStatus;

    let snap = shared.tool_health.snapshot();
    let total_calls: u64 = snap.tools.iter().map(|r| r.total_calls).sum();
    let count_status =
        |status: ToolStatus| snap.tools.iter().filter(|r| r.status == status).count();
    let result = serde_json::json!({
        "summary": {
            "tracked": snap.tools.len(),
            "healthy": count_status(ToolStatus::Healthy),
            "degraded": count_status(ToolStatus::Degraded),
            "disabled": count_status(ToolStatus::Disabled),
            "total_calls": total_calls,
        },
        "thresholds": {
            "degrade": snap.degrade_threshold,
            "disable": snap.disable_threshold,
            "recovery_minutes": snap.recovery_minutes,
        },
        "tools": snap.tools,
    });
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

async fn scm_config_model(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    // Mask API key: show first 4 + last 4
    let mask_key = |key: &Option<String>| -> String {
        match key {
            Some(k) if k.len() > 8 => {
                let prefix = &k[..4];
                let suffix = &k[k.len() - 4..];
                format!("{}****{}", prefix, suffix)
            }
            Some(_k) => "****".to_string(),
            None => "(not configured)".to_string(),
        }
    };

    // Check if using model_profiles format
    let cfg = &shared.baoclaw_config;
    let primary_model = if let Some(ref pname) = cfg.primary_profile {
        cfg.model_profiles
            .get(pname)
            .map(|p| p.model.clone())
            .unwrap_or_else(|| cfg.model.clone())
    } else {
        cfg.model.clone()
    };

    let primary_api_type = if let Some(ref pname) = cfg.primary_profile {
        cfg.model_profiles
            .get(pname)
            .map(|p| p.api_type.clone())
            .unwrap_or_else(|| cfg.api_type.clone())
    } else {
        cfg.api_type.clone()
    };

    let primary_key = if let Some(ref pname) = cfg.primary_profile {
        cfg.model_profiles
            .get(pname)
            .and_then(|p| p.api_key.clone())
    } else {
        // Check env for legacy key
        std::env::var("ANTHROPIC_API_KEY").ok()
    };

    let primary_base_url = if let Some(ref pname) = cfg.primary_profile {
        cfg.model_profiles
            .get(pname)
            .and_then(|p| p.base_url.clone())
            .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
    } else {
        cfg.openai_base_url
            .clone()
            .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
    };

    let primary_window = if let Some(ref pname) = cfg.primary_profile {
        cfg.model_profiles
            .get(pname)
            .map(|p| p.context_window)
            .unwrap_or(cfg.context_window)
    } else {
        cfg.context_window
    };

    let primary_threshold = if let Some(ref pname) = cfg.primary_profile {
        cfg.model_profiles
            .get(pname)
            .map(|p| p.auto_compact_threshold_ratio)
            .unwrap_or(cfg.auto_compact_threshold_ratio)
    } else {
        cfg.auto_compact_threshold_ratio
    };

    // Build fallback chain
    let fallback_chain: Vec<serde_json::Value> = if !cfg.fallback_profiles.is_empty() {
        cfg.fallback_profiles
            .iter()
            .filter_map(|name| {
                cfg.model_profiles.get(name).map(|p| {
                    serde_json::json!({
                        "name": name,
                        "model": p.model,
                        "api_type": p.api_type,
                        "context_window": p.context_window,
                        "api_key_masked": mask_key(&p.api_key),
                    })
                })
            })
            .collect()
    } else {
        cfg.fallback_models
            .iter()
            .map(|m| {
                serde_json::json!({
                    "name": m,
                    "model": m,
                })
            })
            .collect()
    };

    let result = serde_json::json!({
        "primary_model": primary_model,
        "primary_api_type": primary_api_type,
        "primary_api_key_masked": mask_key(&primary_key),
        "primary_base_url": primary_base_url,
        "primary_context_window": primary_window,
        "primary_threshold_ratio": primary_threshold,
        "fallback_chain": fallback_chain,
        "max_retries_per_model": cfg.max_retries_per_model,
    });
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, result).await;
}

async fn scm_config_show(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    // Serialize config with secret values masked
    let mut config_json =
        serde_json::to_value(&shared.baoclaw_config).unwrap_or(serde_json::json!({}));
    mask_secret_values(&mut config_json);

    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(id, serde_json::json!({"config": config_json}))
        .await;
}

/// Recursively mask string values under secret-bearing keys (api_key /
/// token / secret / password) anywhere in the config tree — including the
/// flattened `extra` map, where third-party sections like `telegram` keep
/// their credentials. Numbers and non-secret keys pass through untouched.
fn mask_secret_values(value: &mut serde_json::Value) {
    const SHORT_MASK: &str = "****";
    match value {
        serde_json::Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                let key = key.to_lowercase();
                let secret_key = key.contains("api_key")
                    || key.contains("apikey")
                    || key.contains("api-key")
                    || key.contains("token")
                    || key.contains("secret")
                    || key.contains("password");
                if secret_key {
                    if let serde_json::Value::String(s) = v {
                        if !s.is_empty() && !s.contains(SHORT_MASK) {
                            *s = if s.chars().count() > 8 {
                                let chars: Vec<char> = s.chars().collect();
                                let head: String = chars[..4].iter().collect();
                                let tail: String = chars[chars.len() - 4..].iter().collect();
                                format!("{}{}{}", head, SHORT_MASK, tail)
                            } else {
                                SHORT_MASK.to_string()
                            };
                        }
                        continue;
                    }
                }
                mask_secret_values(v);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                mask_secret_values(item);
            }
        }
        _ => {}
    }
}

async fn scm_evolution_rate_trajectory(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    rating: String,
) {
    use crate::engine::evolution::TrajectoryRating;
    let parsed = match rating.to_ascii_lowercase().as_str() {
        "good" => TrajectoryRating::Good,
        "bad" => TrajectoryRating::Bad,
        "neutral" => TrajectoryRating::Neutral,
        other => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("invalid rating '{}': expected good, bad, or neutral", other),
                )
                .await;
            return;
        }
    };
    shared.evolution_engine.rate_last_trajectory(parsed).await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "message": "rating recorded for the last trajectory",
            }),
        )
        .await;
}

#[cfg(test)]
mod mask_tests {
    use super::mask_secret_values;
    use serde_json::json;

    #[test]
    fn test_masks_nested_extra_secrets() {
        // Configs flatten unknown sections (e.g. telegram) into `extra`;
        // their credentials must not survive config.show.
        let mut v = json!({
            "model": "m",
            "extra": {
                "telegram": { "token": "123456:ABC-DEF-GHI", "allowedChatIds": [1] },
                "feishu": { "app_secret": "supersecret" }
            }
        });
        mask_secret_values(&mut v);
        assert_eq!(v["extra"]["telegram"]["token"], "1234****-GHI");
        assert_eq!(v["extra"]["feishu"]["app_secret"], "supe****cret");
        assert_eq!(v["extra"]["telegram"]["allowedChatIds"][0], 1);
    }

    #[test]
    fn test_masks_api_keys_everywhere() {
        let mut v = json!({
            "api_key": "sk-ant-verylongkeyvalue",
            "model_profiles": {
                "inferx": { "api_key": "ark-live-key-12345678", "model": "gpt" }
            }
        });
        mask_secret_values(&mut v);
        assert_eq!(v["api_key"], "sk-a****alue");
        assert_eq!(v["model_profiles"]["inferx"]["api_key"], "ark-****5678");
        assert_eq!(v["model_profiles"]["inferx"]["model"], "gpt");
    }

    #[test]
    fn test_leaves_non_secrets_untouched() {
        let mut v = json!({
            "max_tokens": 16384,
            "model": "claude",
            "openai_base_url": "https://api.example.com"
        });
        mask_secret_values(&mut v);
        // Numeric token counts are not secrets.
        assert_eq!(v["max_tokens"], 16384);
        assert_eq!(v["model"], "claude");
        assert_eq!(v["openai_base_url"], "https://api.example.com");
    }

    #[test]
    fn test_substring_key_match_is_conservative() {
        // Any key containing "token"/"secret"/... is treated as secret when
        // the value is a string — over-masking config.show is acceptable,
        // under-masking is not.
        let mut v = json!({ "token_count_hint": "aggregate only" });
        mask_secret_values(&mut v);
        assert_eq!(v["token_count_hint"], "aggr****only");
    }

    #[test]
    fn test_masks_inside_arrays_and_skips_already_masked() {
        let mut v = json!({
            "profiles": [ { "password": "hunter2" }, { "password": "correct-horse-battery" } ]
        });
        mask_secret_values(&mut v);
        assert_eq!(v["profiles"][0]["password"], "****");
        assert_eq!(v["profiles"][1]["password"], "corr****tery");

        let mut twice = json!({ "token": "abcd****wxyz" });
        mask_secret_values(&mut twice);
        assert_eq!(
            twice["token"], "abcd****wxyz",
            "double-masking must not mangle"
        );
    }
}
