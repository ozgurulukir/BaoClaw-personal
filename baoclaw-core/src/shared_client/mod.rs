//! Shared-mode client RPC handling.
//!
//! Contains [`handle_shared_client`] — the per-connection request loop — and
//! dispatches to domain-specific submodules.

#![allow(clippy::too_many_arguments, clippy::ptr_arg)]

pub(crate) mod cron;
pub(crate) mod git;
pub(crate) mod history;
pub(crate) mod mcp;
pub(crate) mod memory;
pub(crate) mod permissions;
pub(crate) mod projects;
pub(crate) mod session;
pub(crate) mod spec;
pub(crate) mod system;
pub(crate) mod tasks;
pub(crate) mod team;
pub(crate) mod telemetry;
pub(crate) mod template;
pub(crate) mod tools;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::sync::Mutex as TokioMutex;

use baoclaw_core::engine::query_engine::EngineEvent;
use baoclaw_core::engine::shared_session::{ClientId, SharedSession};
use baoclaw_core::ipc::protocol::JsonRpcMessage;
use baoclaw_core::ipc::router::{parse_client_method, ClientMethod};
use baoclaw_core::ipc::server::{IpcConnection, IpcError, IpcWriter};

use crate::{spawn_shared_broadcast, SharedState};

use self::cron::*;
use self::git::*;
use self::history::*;
use self::mcp::*;
use self::memory::*;
use self::permissions::*;
use self::projects::*;
use self::session::*;
use self::spec::*;
use self::system::*;
use self::tasks::*;
use self::team::*;
use self::telemetry::*;
use self::template::*;
use self::tools::*;

/// Loop control returned by `scm_*` handlers that contain early exits.
pub(super) enum ScmFlow {
    Continue,
    Break,
}

pub(super) type WriterRef<'a> = &'a Arc<TokioMutex<IpcWriter>>;

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
                            scm_list_mcp_servers(&shared, &work_cwd, &writer, id).await;
                        }
                        ClientMethod::McpRefresh { server } => {
                            scm_mcp_refresh(&shared, server, &writer, id).await;
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
                        ClientMethod::ClearSession => {
                            scm_clear_session(&shared, &session, &session_id, &writer, id).await;
                        }
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
