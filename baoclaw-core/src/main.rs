#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const IPC_PROTOCOL_VERSION: &str = "1";
use std::path::PathBuf;
use tokio::sync::Mutex as TokioMutex;

use baoclaw_core::{api, config, discovery, engine, ipc, models, permissions, state, tools};

mod shared_client;

#[cfg(target_os = "windows")]
mod windows_service;

use api::client::ApiClientConfig;
use api::unified::UnifiedClient;
use config::BaoclawConfig;
use engine::query_engine::{
    EngineEvent, QueryEngine, QueryEngineConfig, ThinkingConfig, EMPTY_USAGE,
};
use engine::shared_session::{ClientId, SessionRegistry, SharedSession};
use engine::task_manager::TaskManager;
use ipc::events::engine_event_to_notification;
use ipc::protocol::JsonRpcMessage;
use ipc::router::{parse_client_method, ClientMethod};
use ipc::server::{IpcConnection, IpcError, IpcServer, IpcWriter};
use permissions::gate::PermissionGate;
use permissions::PermissionBridge;
use state::manager::{CoreState, StateManager};
use tools::builtins::{
    AgentTool, BashTool, FileEditTool, FileReadTool, FileWriteTool, ImageEditTool, ImageGenTool,
    MemoryTool, NotebookEditTool, ProjectNoteTool, TodoWriteTool, ToolSearchTool, WebFetchTool,
    WebSearchTool,
};

/// Shared state cloned into each spawned client task.
#[derive(Clone)]
struct SharedState {
    engine_tools: Vec<Arc<dyn tools::Tool>>,
    api_client: Arc<UnifiedClient>,
    permission_gate: PermissionGate,
    permission_manager: Arc<tokio::sync::RwLock<permissions::manager::PermissionManager>>,
    task_manager: Arc<TaskManager>,
    state_manager: Arc<StateManager>,
    baoclaw_config: BaoclawConfig,
    cli_thinking_config: ThinkingConfig,
    _cli_resume_session_id: Option<String>,
    session_id: String,
    should_exit: Arc<AtomicBool>,
    session_registry: Arc<SessionRegistry>,
    skill_prompt: Option<String>,
    memory_store: Arc<engine::memory::MemoryStore>,
    memory_archive: Arc<engine::memory::MemoryArchive>,
    memory_cleanup: Arc<engine::memory::MemoryCleanupScheduler>,
    evolution_engine: Arc<engine::evolution::EvolutionEngine>,
    cron_manager: Arc<engine::cron::CronManager>,
    project_registry: Arc<engine::projects::ProjectRegistry>,
    /// Shared file cache (LRU) for reducing redundant file reads.
    file_cache: Arc<tokio::sync::Mutex<engine::file_cache::FileCache>>,
    /// Tool result store for persisting large outputs to disk.
    tool_result_store: Option<Arc<engine::tool_result_store::ToolResultStore>>,
    /// Hook manager for event-driven automation.
    hook_manager: Arc<engine::hooks::HookManager>,
    /// Team executor for managing sub-agent teams.
    team_executor: Arc<engine::team::TeamManager>,
}

/// Socket directory for all BaoClaw daemon instances
fn socket_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
            if !xdg.is_empty() && std::path::Path::new(&xdg).exists() {
                let dir = PathBuf::from(xdg).join("baoclaw-sockets");
                let _ = std::fs::create_dir_all(&dir);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                }
                return dir;
            }
        }
    }
    let dir = std::env::temp_dir().join("baoclaw-sockets");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "[ipc] WARNING: could not create socket dir {}: {}",
            dir.display(),
            e
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

/// Compute a stable hash of the working directory path.
/// Uses the existing deterministic FNV-1a implementation with 16 hex chars
/// for a short, stable ID.
fn cwd_hash(cwd: &str) -> String {
    format!("{:016x}", md5_simple(cwd))
}

fn legacy_cwd_hash(cwd: &str) -> String {
    format!("{:016x}", md5_simple(cwd))[..8].to_string()
}

fn make_socket_path(cwd: &str) -> PathBuf {
    let hash = cwd_hash(cwd);
    socket_dir().join(format!("baoclaw-cwd-{}.sock", hash))
}

/// Preferred fixed socket path for the machine-level single daemon (P3-1c).
///
/// Linux: $XDG_RUNTIME_DIR/baoclaw.sock (typically /run/user/<UID>/baoclaw.sock)
/// macOS: /tmp/baoclaw-sockets/baoclaw.sock
/// Windows: %TEMP%/baoclaw-sockets/baoclaw.sock
///
/// Falls back to None if no suitable directory exists (then use cwd-hash path).
fn fixed_socket_path() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
            if !xdg.is_empty() && std::path::Path::new(&xdg).exists() {
                return Some(PathBuf::from(xdg).join("baoclaw.sock"));
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        let dir = std::env::temp_dir().join("baoclaw-sockets");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!(
                "[ipc] WARNING: could not create socket dir {}: {}",
                dir.display(),
                e
            );
        }
        Some(dir.join("baoclaw.sock"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let dir = std::env::temp_dir().join("baoclaw-sockets");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!(
                "[ipc] WARNING: could not create socket dir {}: {}",
                dir.display(),
                e
            );
        }
        Some(dir.join("baoclaw.sock"))
    }
}

/// Try fixed socket first, fall back to cwd-hash for backward compat (P3-1c).
fn resolve_daemon_socket(cwd: &str) -> PathBuf {
    if let Some(p) = fixed_socket_path() {
        p
    } else {
        make_socket_path(cwd)
    }
}

/// Write a metadata JSON file next to the socket for discovery
fn write_meta(socket_path: &std::path::Path, cwd: &str, session_id: &str) {
    let meta_path = socket_path.with_extension("json");
    let meta = serde_json::json!({
        "pid": std::process::id(),
        "cwd": cwd,
        "session_id": session_id,
        "socket": socket_path.to_string_lossy(),
        "started_at": chrono::Utc::now().to_rfc3339(),
    });
    if let Err(e) = std::fs::write(
        &meta_path,
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
    ) {
        eprintln!(
            "[daemon] WARNING: could not write daemon meta {}: {}",
            meta_path.display(),
            e
        );
    }
}

fn cleanup_meta(socket_path: &std::path::Path) {
    // Best-effort cleanup: a stale meta file is harmless (next daemon overwrites it),
    // but log so operators can spot permission problems.
    if let Err(e) = std::fs::remove_file(socket_path.with_extension("json")) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("[daemon] WARNING: could not remove daemon meta: {}", e);
        }
    }
}

/// Simple hash for cwd → short hex string
fn md5_simple(input: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in input.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

// ══════════════════════════════════════════════════════════
// Loop Header rendering for TUI
// ══════════════════════════════════════════════════════════

/// Render a Loop Header line when a new turn starts.
/// Outputs a formatted line like: `── Loop 1 ──`
/// If an agent_label is provided, includes it: `── Loop 1 (agent: sub) ──`
fn render_loop_header(turn_id: u32, agent_label: Option<&str>) {
    let label_part = match agent_label {
        Some(label) => format!(" ({})", label),
        None => String::new(),
    };
    eprintln!("── Loop {}{} ──", turn_id, label_part);
}

/// Update the Loop Header with statistics when a turn ends.
/// Outputs a formatted line like: `── Loop 1 ── tools: 3, 2.1s ──`
fn update_loop_header(turn_id: u32, tool_count: u32, duration_ms: u64) {
    let duration_secs = duration_ms as f64 / 1000.0;
    eprintln!(
        "── Loop {} ── tools: {}, {:.1}s ──",
        turn_id, tool_count, duration_secs
    );
}

/// Handle a client in shared mode. The client shares a QueryEngine with other clients
/// via the SharedSession. Uses ActiveSubmitter lock for concurrency control and
/// broadcast channel for event distribution.
fn build_shared_engine(
    shared: &SharedState,
    cwd: PathBuf,
    session_id: String,
    model: String,
) -> QueryEngine {
    QueryEngine::new(QueryEngineConfig {
        cwd,
        tools: shared.engine_tools.clone(),
        api_client: Arc::clone(&shared.api_client),
        model,
        thinking_config: shared.cli_thinking_config.clone(),
        max_turns: None,
        max_budget_usd: None,
        verbose: false,
        custom_system_prompt: None,
        append_system_prompt: shared.skill_prompt.clone(),
        session_id: Some(session_id.clone()),
        fallback_models: shared.baoclaw_config.fallback_models.clone(),
        max_retries_per_model: shared.baoclaw_config.max_retries_per_model,
        context_window: shared.baoclaw_config.context_window,
        auto_compact_threshold_ratio: shared.baoclaw_config.auto_compact_threshold_ratio,
        parent_turn_id: None,
        agent_label: None,
        session_memory: Some(Arc::new(
            crate::engine::session_memory::SessionMemory::load(&session_id),
        )),
        file_cache: Some(Arc::clone(&shared.file_cache)),
        tool_result_store: Some(Arc::new(
            engine::tool_result_store::ToolResultStore::for_session(&session_id),
        )),
        hook_manager: Some(Arc::clone(&shared.hook_manager)),
        permission: Some(PermissionBridge {
            manager: Arc::clone(&shared.permission_manager),
            gate: shared.permission_gate.clone(),
        }),
    })
}

fn spawn_shared_broadcast(
    writer: Arc<TokioMutex<IpcWriter>>,
    session: Arc<SharedSession>,
    client_id: ClientId,
    mut rx: tokio::sync::broadcast::Receiver<EngineEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if session.is_active_submitter(client_id).await {
                        continue;
                    }
                    let notif = engine_event_to_notification(&event);
                    let params =
                        serde_json::to_value(&notif.params).unwrap_or(serde_json::Value::Null);
                    let mut conn_guard = writer.lock().await;
                    if conn_guard
                        .send_notification(&notif.method, params)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("Shared client {} lagged by {} events", client_id, n);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

// Carries exactly the loop state that must move with a client switch.
#[allow(clippy::too_many_arguments)]
async fn switch_shared_client(
    shared: &SharedState,
    writer: &Arc<TokioMutex<IpcWriter>>,
    session: &mut Arc<SharedSession>,
    client_id: &mut ClientId,
    broadcast_handle: &mut tokio::task::JoinHandle<()>,
    session_id: &mut String,
    work_cwd: &mut PathBuf,
    target_cwd: PathBuf,
) -> Result<usize, String> {
    if session.has_active_submitter().await {
        return Err("session busy: cannot switch cwd while a message is being processed".into());
    }
    let session_tag = session_id
        .split_once('-')
        .map(|(_, tag)| tag.to_string())
        .ok_or_else(|| "session tag is unavailable".to_string())?;
    let target_id = format!(
        "{}-{}",
        cwd_hash(&target_cwd.to_string_lossy()),
        session_tag
    );
    let target_model = session.engine_read().await.get_model().to_string();
    let old_session = session.clone();
    let old_session_id = session_id.clone();
    let shared_clone = shared.clone();
    let target_id_for_engine = target_id.clone();
    let target_cwd_for_engine = target_cwd.clone();
    let result = shared
        .session_registry
        .switch_client(
            &old_session_id,
            &old_session,
            *client_id,
            &target_id,
            &target_cwd,
            || {
                build_shared_engine(
                    &shared_clone,
                    target_cwd_for_engine,
                    target_id_for_engine,
                    target_model,
                )
            },
        )
        .await?;

    broadcast_handle.abort();
    let (new_session, new_client_id, new_broadcast_rx) = result;
    *session = new_session;
    *client_id = new_client_id;
    *session_id = target_id;
    *work_cwd = target_cwd.clone();
    *broadcast_handle = spawn_shared_broadcast(
        writer.clone(),
        session.clone(),
        *client_id,
        new_broadcast_rx,
    );
    shared.memory_store.switch_project(&target_cwd).await;
    shared
        .project_registry
        .ensure_registered(&target_cwd.to_string_lossy(), None)
        .await;
    Ok(session.engine_read().await.get_messages().len())
}

/// Releases the session submitter when the spawned turn drain ends —
/// including on panic. `armed` is cleared once the normal release path has
/// run, so the guard only acts as a safety net.
struct SubmitterGuard {
    session: Arc<SharedSession>,
    client_id: ClientId,
    armed: bool,
}

impl Drop for SubmitterGuard {
    fn drop(&mut self) {
        if self.armed {
            let session = self.session.clone();
            let client_id = self.client_id;
            tokio::spawn(async move {
                session.release_submitter(client_id).await;
            });
        }
    }
}

/// Handle a single client connection. Each client gets its own QueryEngine
/// with independent conversation history.
async fn handle_client(mut conn: IpcConnection, shared: SharedState) {
    // Wait for initialize request
    let init_msg = match conn.recv_message().await {
        Ok(msg) => msg,
        Err(IpcError::ConnectionClosed) => {
            eprintln!("Client disconnected before initialize");
            return;
        }
        Err(e) => {
            eprintln!("Error reading init: {}", e);
            return;
        }
    };

    let (
        init_id,
        init_cwd,
        init_model,
        init_resume_session_id,
        init_shared_session_id,
        init_protocol_version,
    ) = match init_msg {
        JsonRpcMessage::Request(req) => {
            let id = req.id.clone();
            match parse_client_method(&req) {
                Ok(ClientMethod::Initialize {
                    cwd: c,
                    model: m,
                    protocol_version: p,
                    resume_session_id: r,
                    shared_session_id: s,
                    ..
                }) => (id, c, m, r, s, p),
                Ok(_) => {
                    let _ = conn
                        .send_error(
                            Some(req.id),
                            -32600,
                            "Expected 'initialize' as first request".into(),
                        )
                        .await;
                    return;
                }
                Err(e) => {
                    let _ = conn
                        .send_error(Some(req.id), -32600, format!("Invalid init: {}", e))
                        .await;
                    return;
                }
            }
        }
        _ => {
            return;
        }
    };

    if let Some(protocol_version) = init_protocol_version {
        if protocol_version != IPC_PROTOCOL_VERSION {
            let _ = conn
                .send_error(
                    Some(init_id),
                    -32001,
                    format!(
                        "Incompatible IPC protocol version '{}'; daemon supports '{}'. Upgrade the client or daemon.",
                        protocol_version, IPC_PROTOCOL_VERSION
                    ),
                )
                .await;
            return;
        }
    }

    if init_resume_session_id.is_some() {
        let _ = conn
            .send_error(
                Some(init_id),
                -32602,
                "resume_session_id is not supported; use shared_session_id instead".into(),
            )
            .await;
        return;
    }

    let model = init_model
        .or_else(|| std::env::var("ANTHROPIC_MODEL").ok())
        .unwrap_or_else(|| shared.baoclaw_config.model.clone());
    let work_cwd = init_cwd;

    // ── Shared mode: session key is derived from cwd, not client-provided ID ──
    // This allows one daemon to manage multiple project sessions.
    if let Some(ref shared_session_id) = init_shared_session_id {
        if !engine::session_persistence::is_valid_session_id(shared_session_id) {
            let _ = conn
                .send_error(
                    Some(init_id),
                    -32602,
                    "Invalid shared_session_id: use only letters, digits, '-' or '_' (max 128 bytes)"
                        .into(),
                )
                .await;
            return;
        }
        // Session key = cwd_hash + client_type, so different clients (web/telegram/cli)
        // on the same cwd get independent sessions and don't block each other.
        let cwd_key = cwd_hash(&work_cwd.to_string_lossy());
        let session_id_clone = format!("{}-{}", cwd_key, shared_session_id);
        if !engine::session_persistence::is_valid_session_id(&session_id_clone) {
            let _ = conn
                .send_error(
                    Some(init_id),
                    -32602,
                    "Invalid shared_session_id: use only letters, digits, '-' or '_' (max 128 bytes)"
                        .into(),
                )
                .await;
            return;
        }
        let legacy_session_id = format!(
            "{}-{}",
            legacy_cwd_hash(&work_cwd.to_string_lossy()),
            shared_session_id
        );
        if let Err(error) = engine::session_persistence::migrate_legacy_session(
            &shared.session_registry.persistence_dir().clone(),
            &legacy_session_id,
            &session_id_clone,
            &work_cwd.to_string_lossy(),
        ) {
            eprintln!(
                "[session-registry] WARNING: legacy migration skipped: {}",
                error
            );
        }
        eprintln!(
            "Client connecting to session '{}' (cwd: {})",
            session_id_clone,
            work_cwd.display()
        );
        let shared_clone = shared.clone();
        let model_clone = model.clone();
        let work_cwd_clone = work_cwd.clone();

        let (session, is_new, mut resumed) = shared
            .session_registry
            .get_or_create_with_restore(&session_id_clone, || {
                build_shared_engine(
                    &shared_clone,
                    work_cwd_clone,
                    session_id_clone.clone(),
                    model_clone,
                )
            })
            .await;

        // Auto-register this project in the registry
        shared
            .project_registry
            .ensure_registered(&work_cwd.to_string_lossy(), None)
            .await;

        // ── Resume session history: snapshot-first, legacy transcript fallback ──
        // Inspired by Claude Code: load pre-written summary + recent tail,
        // NEVER rebuild the full history or do on-demand API summarization.
        let current_msg_count = session.engine_read().await.get_messages().len();
        if (is_new || current_msg_count == 0) && !resumed {
            let cwd_str_for_resume = work_cwd.to_string_lossy().to_string();
            if let Some(rid) = engine::transcript::find_latest_session_for_cwd(&cwd_str_for_resume)
            {
                match engine::transcript::TranscriptWriter::load(&rid) {
                    Ok(entries) => {
                        let entry_count = entries.len();
                        let old_summary_obj =
                            crate::engine::session_memory::SessionMemory::load(&rid);
                        let old_summary = old_summary_obj.get();
                        let has_summary = old_summary_obj.is_available();

                        // ── Three-tier loading strategy ──
                        let messages = if has_summary {
                            // Tier 1 (best): pre-written summary exists
                            // Load summary + last 200 entries only — instant
                            let tail_size = 200.min(entry_count);
                            eprintln!("Session resume: loading pre-written summary + {} recent entries (of {} total)",
                                tail_size, entry_count);
                            engine::transcript::rebuild_messages_from_transcript_limited(
                                &entries,
                                tail_size,
                                Some(&old_summary),
                            )
                        } else if entry_count <= 400 {
                            // Tier 2: small session, no summary — safe to rebuild all
                            eprintln!(
                                "Session resume: small session ({} entries), rebuilding all",
                                entry_count
                            );
                            engine::transcript::rebuild_messages_from_transcript(&entries)
                        } else {
                            // Tier 3 (fallback): large session with NO summary
                            // Don't rebuild all (would cause 10-min auto-compact).
                            // Load last 200 entries with a warning header instead.
                            let tail_size = 200.min(entry_count);
                            eprintln!(
                                "WARNING: Large session ({} entries) with no pre-written summary. \
                                 Loading only last {} entries. Context from earlier turns may be lost. \
                                 (Summary will be generated during this session for next time.)",
                                entry_count, tail_size
                            );
                            let tail_entries = &entries[entry_count - tail_size..];
                            let mut msgs =
                                engine::transcript::rebuild_messages_from_transcript(tail_entries);

                            // Prepend a warning so the LLM knows context is incomplete
                            if !msgs.is_empty() {
                                let warning = crate::models::message::Message {
                                    uuid: uuid::Uuid::new_v4().to_string(),
                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                    content: crate::models::message::MessageContent::System {
                                        subtype: crate::models::message::SystemSubtype::CompactBoundary,
                                        content: format!(
                                            "[Session resumed — {} earlier conversation entries were omitted because no summary was available. \
                                             The current session will generate one for next time.]",
                                            entry_count - tail_size
                                        ),
                                    },
                                };
                                msgs.insert(0, warning);
                            }
                            msgs
                        };

                        if !messages.is_empty() {
                            let mut engine = session.engine_write().await;
                            engine.set_messages(messages);

                            // Load and apply persisted token baseline
                            engine.load_token_baseline(&rid).await;

                            // Seed the new session's memory with the old summary
                            engine.seed_session_memory(&old_summary);

                            eprintln!(
                                "Resumed session {} ({} entries → {} messages)",
                                rid,
                                entry_count,
                                engine.get_messages().len()
                            );
                            resumed = true;
                        }
                    }
                    Err(e) => eprintln!("Failed to resume session {}: {}", rid, e),
                }
            }
        }

        let (client_id, broadcast_rx) = session.add_client().await;
        let msg_count = session.engine_read().await.get_messages().len();

        // Send init response with shared: true
        let _ = conn
            .send_response(
                init_id,
                serde_json::json!({
                    "capabilities": { "tools": true, "streaming": true, "permissions": true },
                    "session_id": &session_id_clone,
                    "shared": true,
                    "reconnected": msg_count > 0,
                    "resumed": resumed,
                    "message_count": msg_count,
                    "model": session.engine_read().await.get_model(),
                }),
            )
            .await;

        // Enter shared-mode RPC loop
        let (session, client_id, session_id_clone, work_cwd) = shared_client::handle_shared_client(
            conn,
            shared,
            session.clone(),
            client_id,
            broadcast_rx,
            work_cwd,
            session_id_clone.clone(),
        )
        .await;
        let hook_cwd = work_cwd.to_string_lossy().to_string();

        // The submit drain runs in a spawned task and owns the submitter
        // until the turn's sync+persist completes. remove_client would
        // auto-release it mid-turn, letting a second turn start while the
        // first drain is still writing — wait for the drain instead. It
        // notices a dead socket on its next event, so this is short-lived.
        while session.is_active_submitter(client_id).await {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Client disconnect handling (Task 6.1)
        let is_last = session.remove_client(client_id).await;
        let cleanup_guard = if is_last {
            shared_clone
                .session_registry
                .acquire_last_client_cleanup(&session_id_clone, &session)
                .await
        } else {
            None
        };
        if is_last && cleanup_guard.is_some() {
            // ── Session-close evolution hook ──
            // Extract structured summary before removing the session.
            {
                let engine = session.engine_read().await;
                let messages = engine.get_messages();
                let usage = engine.get_usage().clone();
                let model = engine.get_model().to_string();
                let messages_clone = messages.to_vec();
                drop(engine);

                // Estimate session duration from first and last message timestamps
                let duration_secs = if let [first, .., last] = messages_clone.as_slice() {
                    let first_ts = &first.timestamp;
                    let last_ts = &last.timestamp;
                    (|| -> Option<u64> {
                        let t1 = chrono::DateTime::parse_from_rfc3339(first_ts).ok()?;
                        let t2 = chrono::DateTime::parse_from_rfc3339(last_ts).ok()?;
                        Some((t2 - t1).num_seconds().max(0) as u64)
                    })()
                    .unwrap_or(0)
                } else {
                    0
                };

                // Estimate total cost from token usage (Claude Sonnet pricing)
                let estimated_cost = (usage.input_tokens as f64 * 3.0e-6)
                    + (usage.output_tokens as f64 * 15.0e-6)
                    + (usage.cache_read_input_tokens.unwrap_or(0) as f64 * 0.3e-6);

                // ── Save session memory on close if not yet written ──
                // This ensures that even if the background updater never ran
                // (e.g., short session), the next startup will have a summary.
                {
                    let engine = session.engine_read().await;
                    if let Some(ref sm) = engine.get_session_memory() {
                        if !sm.is_available() && messages_clone.len() >= 4 {
                            eprintln!(
                                "Session close: generating heuristic session memory ({} messages)",
                                messages_clone.len()
                            );

                            let mut summary_parts = vec!["# Session Summary".to_string()];

                            // Extract user messages as task list
                            let mut task_descriptions = Vec::new();
                            for msg in &messages_clone {
                                if let crate::models::message::MessageContent::User {
                                    message,
                                    ..
                                } = &msg.content
                                {
                                    if let serde_json::Value::String(s) = &message.content {
                                        let first_line = s.lines().next().unwrap_or("");
                                        if !first_line.is_empty() && first_line.len() < 200 {
                                            task_descriptions.push(first_line.to_string());
                                        }
                                    }
                                }
                            }

                            if !task_descriptions.is_empty() {
                                summary_parts.push("## Tasks Discussed".to_string());
                                for (i, task) in task_descriptions.iter().take(20).enumerate() {
                                    summary_parts.push(format!("{}. {}", i + 1, task));
                                }
                            }

                            summary_parts.push(format!(
                                "\n## Stats\n- Messages: {}\n- Duration: {}s\n- Cost: ${:.4}",
                                messages_clone.len(),
                                duration_secs,
                                estimated_cost
                            ));

                            let summary = summary_parts.join("\n");
                            sm.update(summary);
                            eprintln!("Session memory saved on close ({} chars)", sm.get().len());
                        }
                    }
                }

                shared_clone
                    .evolution_engine
                    .on_session_close(
                        &session_id_clone,
                        &hook_cwd,
                        &model,
                        &messages_clone,
                        &usage,
                        estimated_cost,
                        duration_secs,
                    )
                    .await;
            }

            match shared_clone
                .session_registry
                .persist_session(&session_id_clone)
                .await
            {
                Ok(()) => {
                    shared_clone
                        .session_registry
                        .remove_after_last_client_cleanup(&session_id_clone)
                        .await;
                    eprintln!(
                        "Shared session '{}' removed (last client disconnected)",
                        session_id_clone
                    );
                }
                Err(error) => eprintln!(
                    "[daemon] WARNING: keeping session '{}' in memory because final persistence failed: {}",
                    session_id_clone, error
                ),
            }
        }

        eprintln!("Shared client {} session ended", client_id);
        return;
    }

    // All clients use shared mode.
    eprintln!("Client disconnected: no shared_session_id provided");
    let _ = conn
        .send_error(
            Some(init_id),
            -32600,
            "shared_session_id is required".into(),
        )
        .await;
}

/// Run the daemon under a Windows service context.
///
/// Called synchronously from `windows_service_main()`. Uses a subprocess
/// architecture for maximum safety:
/// 1. This process (service host) manages the SCM connection
/// 2. The actual daemon runs as a child process with `--daemon`
/// 3. When SCM says stop, we kill the child (its signal handler persists sessions)
///
/// This avoids the complexity of re-entering main()'s async logic.
#[cfg(target_os = "windows")]
pub fn run_daemon_main_with_shutdown_check() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[service] Cannot find current exe: {}", e);
            return;
        }
    };

    let cwd = std::env::current_dir().unwrap_or_default();

    eprintln!(
        "[service] Launching daemon subprocess: {} --daemon",
        exe.display()
    );

    let mut child = match std::process::Command::new(&exe)
        .arg("--daemon")
        .arg("--cwd")
        .arg(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[service] Failed to spawn daemon subprocess: {}", e);
            return;
        }
    };

    // Monitor loop: wait for SCM shutdown or child exit
    loop {
        // Check if SCM requested shutdown
        if windows_service::is_shutdown_requested() {
            eprintln!("[service] SCM requested shutdown, stopping daemon...");
            // Kill child — the daemon's own SIGTERM handler will persist sessions.
            // On Windows, kill() sends a TerminateProcess which is forceful;
            // for graceful shutdown we'd need CTRL_BREAK_EVENT, but the daemon's
            // session persistence is also handled by its periodic save logic.
            if let Err(e) = child.kill() {
                eprintln!(
                    "[daemon] WARNING: could not kill engine child: {} (may have already exited)",
                    e
                );
            }
            break;
        }

        // Check if child exited on its own
        match child.try_wait() {
            Ok(Some(_status)) => {
                // Child exited
                eprintln!("[service] Daemon subprocess exited");
                break;
            }
            Ok(None) => {
                // Still running, wait a bit
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Err(e) => {
                eprintln!("[service] Error waiting for daemon: {}", e);
                break;
            }
        }
    }

    // Ensure child is reaped; failure here would leak a zombie — surface it.
    if let Err(e) = child.wait() {
        eprintln!("[daemon] WARNING: child wait failed: {}", e);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    // ══════════════════════════════════════════════════════════
    // Windows service management commands (--install-service, etc.)
    // These are only valid on Windows and handled before anything else.
    // ══════════════════════════════════════════════════════════
    #[cfg(target_os = "windows")]
    {
        if args.iter().any(|a| a == "--install-service") {
            if let Err(e) = windows_service::install_service() {
                eprintln!("Error installing service: {}", e);
                std::process::exit(1);
            }
            return Ok(());
        }
        if args.iter().any(|a| a == "--uninstall-service") {
            if let Err(e) = windows_service::uninstall_service() {
                eprintln!("Error uninstalling service: {}", e);
                std::process::exit(1);
            }
            return Ok(());
        }
        if args.iter().any(|a| a == "--run-as-service") {
            // Dispatch to SCM — this blocks until the service stops
            if let Err(e) = windows_service::dispatch_as_service() {
                eprintln!("Service dispatch error: {}", e);
                std::process::exit(1);
            }
            return Ok(());
        }
    }

    let is_daemon = args.iter().any(|a| a == "--daemon");

    // CRITICAL: Ignore SIGPIPE so we don't die when CLI disconnects stdout/stderr
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    // Parse --cwd flag or use current directory
    let cwd_str = args
        .iter()
        .position(|a| a == "--cwd")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string())
        });
    let _cwd = PathBuf::from(&cwd_str);

    // Parse --resume flag for session resumption
    let cli_resume_session_id = args
        .iter()
        .position(|a| a == "--resume")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.to_string());

    // Parse --think flag for extended thinking
    let cli_thinking_config = if args.iter().any(|a| a == "--think") {
        let budget = args
            .iter()
            .position(|a| a == "--think")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(10240);
        ThinkingConfig::Enabled {
            budget_tokens: budget,
        }
    } else {
        ThinkingConfig::Disabled
    };

    // Parse --sandbox flag for sandboxed command execution
    // Usage: --sandbox bwrap | --sandbox docker | --sandbox none
    // If flag is omitted, no sandbox is used (direct execution).
    if let Some(mode) = args
        .iter()
        .position(|arg| arg == "--sandbox")
        .and_then(|index| args.get(index + 1))
    {
        if !matches!(
            mode.as_str(),
            "bwrap" | "bubblewrap" | "docker" | "none" | "off"
        ) {
            return Err(format!(
                "Unknown sandbox mode '{}'. Use bwrap, docker, or none.",
                mode
            )
            .into());
        }
    }
    let sandbox_config: Option<Arc<engine::sandbox::SandboxConfig>> = args
        .iter()
        .position(|a| a == "--sandbox")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .map(|mode| {
            use engine::sandbox::{SandboxBackend, SandboxConfig};
            let backend = match mode {
                "bwrap" | "bubblewrap" => {
                    eprintln!("[sandbox] Using Bubblewrap (bwrap) isolation");
                    SandboxBackend::Bubblewrap
                }
                "docker" => {
                    eprintln!("[sandbox] Using Docker container isolation");
                    SandboxBackend::Docker {
                        image: std::env::var("BAOCLAW_SANDBOX_IMAGE")
                            .unwrap_or_else(|_| "baoclaw-sandbox:latest".into()),
                    }
                }
                "none" | "off" => {
                    eprintln!("[sandbox] Sandbox disabled (direct execution)");
                    SandboxBackend::None
                }
                _ => unreachable!("sandbox mode validated before parsing"),
            };
            let mut cfg = SandboxConfig {
                backend,
                ..SandboxConfig::default()
            };
            // Auto-mount the working directory as read-write
            cfg.rw_mounts.push(cwd_str.clone());
            // Mount ~/.baoclaw for config/memory/session data access
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            let baoclaw_dir = format!("{}/.baoclaw", home);
            if std::path::Path::new(&baoclaw_dir).exists() {
                cfg.rw_mounts.push(baoclaw_dir);
            }
            // Mount /tmp for temp file exchange between host and sandbox
            cfg.rw_mounts.push("/tmp".to_string());
            // Set workdir to the project CWD
            cfg.workdir = Some(cwd_str.clone());
            Arc::new(cfg)
        });

    // If --sandbox flag was provided without a value, use auto-detect
    let sandbox_config = sandbox_config.or_else(|| {
        if args.iter().any(|a| a == "--sandbox") {
            eprintln!("[sandbox] No mode specified, auto-detecting...");
            let mut cfg = engine::sandbox::SandboxConfig::auto_detect();
            cfg.rw_mounts.push(cwd_str.clone());
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            let baoclaw_dir = format!("{}/.baoclaw", home);
            if std::path::Path::new(&baoclaw_dir).exists() {
                cfg.rw_mounts.push(baoclaw_dir);
            }
            cfg.rw_mounts.push("/tmp".to_string());
            cfg.workdir = Some(cwd_str.clone());
            eprintln!("[sandbox] Auto-detected: {}", cfg.description());
            Some(Arc::new(cfg))
        } else {
            None
        }
    });

    // Validate sandbox configuration at startup
    if let Some(ref cfg) = sandbox_config {
        if let Some(err) = cfg.validate() {
            return Err(format!("Sandbox configuration invalid: {}", err).into());
        } else {
            eprintln!("[sandbox] ✓ Sandbox ready: {}", cfg.description());
        }
    }

    // Create socket: prefer fixed machine-level path (P3-1c), fall back to cwd-hash
    let socket_path = resolve_daemon_socket(&cwd_str);

    // IpcServer::bind probes and removes stale sockets, avoiding a
    // check-then-delete race during daemon startup.
    let server = IpcServer::bind(&socket_path).await?;

    // Output socket path for clients to find
    println!("SOCKET:{}", socket_path.display());
    use std::io::Write;
    std::io::stdout().flush()?;

    // In daemon mode, close stdout/stderr after emitting socket path
    // so broken pipes from the launching CLI can't affect us
    if is_daemon {
        let log_path = socket_path.with_extension("log");
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok();

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            if let Some(ref f) = log_file {
                unsafe {
                    libc::dup2(f.as_raw_fd(), 2); // stderr → log
                }
            } else {
                // Init-time: /dev/null is guaranteed to exist on unix targets;
                // failure here means the fd table is exhausted — aborting startup
                // is the correct behavior, hence unwrap is intentional.
                let devnull = std::fs::File::open("/dev/null")
                    .expect("/dev/null must be openable at startup");
                unsafe {
                    libc::dup2(devnull.as_raw_fd(), 2);
                }
            }
            let devnull =
                std::fs::File::open("/dev/null").expect("/dev/null must be openable at startup");
            unsafe {
                libc::dup2(devnull.as_raw_fd(), 1); // stdout → /dev/null
            }
        }

        eprintln!(
            "baoclaw-core daemon started (pid={}, cwd={})",
            std::process::id(),
            cwd_str
        );
    }

    // Load BaoClaw config from ~/.baoclaw/config.json
    let mut baoclaw_config = config::load_config();
    config::apply_env_override(&mut baoclaw_config);

    // === P1-1: Model profiles support ===
    // Resolve the primary profile (auto-migrated from old format by normalize_profiles).
    // If model_profiles is populated, use the primary profile's api_type/key/base_url.
    // Otherwise, fall back to the old env-var-based logic for backward compatibility.
    //
    // resolve_api_key priority: profile.api_key → env var based on api_type
    // resolve_base_url priority: profile.base_url → env var based on api_type
    fn resolve_api_key(profile: &config::ModelProfile) -> String {
        // 1. Prefer profile.api_key (new format)
        if let Some(key) = &profile.api_key {
            if !key.is_empty() {
                return key.clone();
            }
        }
        // 2. Fall back to environment variable (backward compat)
        match profile.api_type.as_str() {
            "openai" => std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            _ => std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
        }
    }
    fn resolve_base_url(profile: &config::ModelProfile) -> Option<String> {
        if let Some(url) = &profile.base_url {
            if !url.is_empty() {
                return Some(url.clone());
            }
        }
        match profile.api_type.as_str() {
            "openai" => std::env::var("OPENAI_BASE_URL").ok(),
            _ => std::env::var("ANTHROPIC_BASE_URL").ok(),
        }
    }

    // Determine the effective primary profile for API client construction.
    // After normalize_profiles, primary_profile is always Some if model is set.
    let primary_profile: config::ModelProfile = {
        let name = baoclaw_config
            .primary_profile
            .as_deref()
            .unwrap_or("primary");
        baoclaw_config
            .model_profiles
            .get(name)
            .cloned()
            .unwrap_or_else(|| {
                // Fallback: construct from legacy fields
                config::ModelProfile {
                    model: baoclaw_config.model.clone(),
                    api_type: baoclaw_config.api_type.clone(),
                    api_key: None,
                    base_url: baoclaw_config.openai_base_url.clone(),
                    context_window: baoclaw_config.context_window,
                    auto_compact_threshold_ratio: baoclaw_config.auto_compact_threshold_ratio,
                    max_retries_per_model: baoclaw_config.max_retries_per_model,
                }
            })
    };

    // Get API key and config: use profile's api_type to pick env vars / credentials
    let api_client: Arc<UnifiedClient> = {
        let api_key = resolve_api_key(&primary_profile);
        let base_url = resolve_base_url(&primary_profile);
        match primary_profile.api_type.as_str() {
            "openai" => {
                eprintln!(
                    "Using OpenAI-compatible API (model: {}, base_url: {})",
                    primary_profile.model,
                    base_url.as_deref().unwrap_or("https://api.openai.com")
                );
                let config = ApiClientConfig {
                    api_key,
                    base_url,
                    max_retries: None,
                    api_path: None,
                };
                Arc::new(UnifiedClient::new_openai(config))
            }
            _ => {
                let api_path = std::env::var("ANTHROPIC_API_PATH").ok();
                eprintln!(
                    "Using Anthropic API (model: {}, base_url: {})",
                    primary_profile.model,
                    base_url.as_deref().unwrap_or("https://api.anthropic.com")
                );
                let config = ApiClientConfig {
                    api_key,
                    base_url,
                    max_retries: None,
                    api_path,
                };
                Arc::new(UnifiedClient::new_anthropic(config))
            }
        }
    };

    // Pre-warm the API connection pool in the background (TCP + TLS handshake
    // before the first real request, saving 100-300ms on first query).
    {
        let prewarm_client = Arc::clone(&api_client);
        tokio::spawn(async move {
            prewarm_client.prewarm().await;
        });
    }

    // Allow tools to access ~/.baoclaw/ in addition to project cwd
    let home_dir = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let baoclaw_home = std::path::PathBuf::from(&home_dir).join(".baoclaw");
    let additional_dirs = vec![baoclaw_home];

    // Create evolution engine for self-improvement
    let evolution_engine = Arc::new(engine::evolution::EvolutionEngine::new(
        std::path::Path::new(&cwd_str),
    ));

    // Build the core tool list (everything except AgentTool itself, which is added after)
    // BashTool is optionally sandboxed based on --sandbox CLI flag
    let bash_tool: BashTool = match &sandbox_config {
        Some(cfg) => BashTool::with_sandbox(Arc::clone(cfg)),
        None => BashTool::new(),
    };
    let core_tools: Vec<Arc<dyn tools::Tool>> = vec![
        Arc::new(bash_tool),
        Arc::new(FileReadTool::new(additional_dirs.clone())),
        Arc::new(FileWriteTool::new(additional_dirs.clone())),
        Arc::new(FileEditTool::new(additional_dirs.clone())),
        Arc::new(WebFetchTool::new()),
        Arc::new(WebSearchTool::new()),
        Arc::new(ImageGenTool::new()),
        Arc::new(ImageEditTool::new()),
        Arc::new(NotebookEditTool::new()),
        Arc::new(TodoWriteTool::new()),
        Arc::new(MemoryTool::new()),
        Arc::new(ProjectNoteTool::new()),
        Arc::new(tools::builtins::SkillTool::new(PathBuf::from(&cwd_str))),
        Arc::new(tools::builtins::EvolveTool::new(Arc::clone(
            &evolution_engine,
        ))),
    ];

    // AgentTool gets the full core tool set so sub-agents can write, edit, run bash, etc.
    let agent_tool = AgentTool::new_with_full_tools(Arc::clone(&api_client), core_tools.clone());

    let mut engine_tools: Vec<Arc<dyn tools::Tool>> = core_tools;
    engine_tools.push(Arc::new(agent_tool));

    // ToolSearchTool needs the full tool list, so register it last
    let engine_tools: Vec<Arc<dyn tools::Tool>> = {
        let mut all = engine_tools;

        //         // MCP integration: discover and connect to MCP servers (with timeout)
        //         // Singleton check: ensure MCP is only initialized once
        //         if MCP_INITIALIZED.load(Ordering::SeqCst) {
        //             eprintln!("MCP already initialized, skipping...");
        //         } else {
        //             MCP_INITIALIZED.store(true, Ordering::SeqCst);
        //         let mcp_servers = discovery::mcp_config::discover_mcp_servers(std::path::Path::new(&cwd_str)).await;
        //         for server_info in &mcp_servers {
        //             if server_info.disabled {
        //                 continue;
        //             }
        //             if let Some(ref command) = server_info.command {
        //                 let config = mcp::McpServerConfig {
        //                     name: server_info.name.clone(),
        //                     command: command.clone(),
        //                     args: server_info.args.clone(),
        //                     env: std::collections::HashMap::new(),
        //                     transport: mcp::McpTransportType::Stdio,
        //                 };
        //                 let mut client = mcp::McpClient::new(config);
        //                 let connect_result = tokio::time::timeout(
        //                     std::time::Duration::from_secs(30),
        //                     client.connect_stdio(),
        //                 ).await;
        //                 match connect_result {
        //                     Ok(Ok(())) => {
        //                         let client = Arc::new(client);
        //                         if let Ok(tools) = client.list_tools().await {
        //                             eprintln!("MCP server '{}': {} tools discovered", server_info.name, tools.len());
        //                             for tool_def in &tools {
        //                                 eprintln!("  MCP tool: {}", tool_def.name);
        //                             }
        //                             for tool_def in tools {
        //                                 let wrapper = McpToolWrapper::new(
        //                                     Arc::clone(&client),
        //                                     tool_def,
        //                                     server_info.name.clone(),
        //                                 );
        //                                 all.push(Arc::new(wrapper));
        //                             }
        //                         } else {
        //                             eprintln!("MCP server '{}': list_tools failed", server_info.name);
        //                         }
        //                         eprintln!("MCP server '{}' connected", server_info.name);
        //                     }
        //                     Ok(Err(e)) => {
        //                         eprintln!("Warning: MCP server '{}' failed to connect: {}", server_info.name, e);
        //                     }
        //                     Err(_) => {
        //                         eprintln!("Warning: MCP server '{}' connection timed out (30s)", server_info.name);
        //                     }
        //                 }
        //             }
        //         }
        //

        all.push(Arc::new(ToolSearchTool::new(all.clone())));
        eprintln!("Total tools registered: {} (including MCP)", all.len());
        all
    };

    // Load skill content for system prompt injection
    let skill_prompt =
        discovery::skills::load_skills_for_prompt(std::path::Path::new(&cwd_str)).await;
    if let Some(ref sp) = skill_prompt {
        eprintln!("Loaded skills into system prompt ({} chars)", sp.len());
    }

    // Load long-term memory
    let memory_store = Arc::new(engine::memory::MemoryStore::load());
    let memory_prompt = memory_store.build_prompt_fragment().await;
    if let Some(ref mp) = memory_prompt {
        eprintln!(
            "Loaded long-term memory into system prompt ({} chars)",
            mp.len()
        );
    }

    // Combine skill + memory into append_system_prompt
    let combined_append_prompt = {
        let mut parts = Vec::new();
        if let Some(sp) = skill_prompt {
            parts.push(sp);
        }
        if let Some(mp) = memory_prompt {
            parts.push(mp);
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    };

    // Reuse existing project session or create new one.
    // One project directory = one session file.
    let cwd_key = cwd_hash(&cwd_str);
    let session_id = match engine::transcript::find_latest_session_for_cwd(&cwd_str) {
        Some(legacy_id) => {
            let legacy_prefix = format!("{}-", legacy_cwd_hash(&cwd_str));
            if let Some(suffix) = legacy_id.strip_prefix(&legacy_prefix) {
                let normalized_id = format!("{}-{}", cwd_key, suffix);
                let sessions_dir = engine::session_persistence::default_sessions_dir();
                let migrated = engine::session_persistence::migrate_legacy_session(
                    &sessions_dir,
                    &legacy_id,
                    &normalized_id,
                    &cwd_str,
                )
                .unwrap_or(false);
                if migrated
                    || engine::session_persistence::load_session_state(
                        &sessions_dir,
                        &normalized_id,
                    )
                    .is_some()
                {
                    normalized_id
                } else {
                    legacy_id
                }
            } else {
                legacy_id
            }
        }
        None => format!("{}-{}", cwd_key, &uuid::Uuid::new_v4().to_string()[..8]),
    };
    eprintln!("Session ID: {} (cwd: {})", session_id, cwd_str);

    // Write metadata file for discovery by CLI
    write_meta(&socket_path, &cwd_str, &session_id);

    let state_manager = Arc::new(StateManager::new(CoreState {
        session_id: session_id.clone(),
        model: baoclaw_config.model.clone(),
        verbose: false,
        tasks: std::collections::HashMap::new(),
        usage: EMPTY_USAGE,
        total_cost_usd: 0.0,
    }));

    // If daemon mode, fully detach from controlling terminal:
    //   1. setsid() — new session + new process group, no controlling terminal
    //   2. Ignore SIGHUP — extra safety against accidental kills
    // This prevents zombie accumulation: without setsid(), the daemon's ppid
    // stays as the launching terminal shell. When the terminal exits, orphaned
    // children get reparented to init slowly, and any subprocess zombies in the
    // daemon won't be reaped promptly.
    if is_daemon {
        #[cfg(unix)]
        unsafe {
            libc::setsid();
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }
    }

    let should_exit = Arc::new(AtomicBool::new(false));

    // Create PermissionGate and PermissionManager for interactive permission flow
    let permission_gate = PermissionGate::new();
    let permission_manager = Arc::new(tokio::sync::RwLock::new(
        permissions::manager::PermissionManager::default(),
    ));
    if let Some(perms_val) = baoclaw_config.extra.get("permissions") {
        if let Ok(ctx) =
            serde_json::from_value::<permissions::manager::ToolPermissionContext>(perms_val.clone())
        {
            // main() runs inside the tokio runtime — blocking_write() here
            // panics ("Cannot block the current thread from within a
            // runtime"). Startup has no contenders, so a plain async write
            // is safe.
            let mgr = permission_manager.write().await;
            mgr.update_context(|c| *c = ctx);
        }
    }

    // Create TaskManager for background task execution
    let task_manager = Arc::new(TaskManager::new(
        Arc::clone(&api_client),
        engine_tools.clone(),
        baoclaw_config.context_window,
        baoclaw_config.auto_compact_threshold_ratio,
    ));

    let team_executor = Arc::new(engine::team::TeamManager::new(
        Arc::clone(&api_client),
        engine_tools.clone(),
        PathBuf::from(&cwd_str),
        baoclaw_config.model.clone(),
    ));

    // Create memory archive and cleanup scheduler for periodic memory maintenance
    let memory_archive = Arc::new(engine::memory::MemoryArchive::load());
    let memory_decay_config = engine::memory::DecayConfig::load();
    let memory_cleanup = Arc::new(engine::memory::MemoryCleanupScheduler::new(
        Arc::clone(&memory_store),
        Arc::clone(&memory_archive),
        memory_decay_config,
    ));

    let shared = SharedState {
        engine_tools,
        api_client,
        permission_gate,
        permission_manager,
        task_manager,
        state_manager,
        baoclaw_config,
        cli_thinking_config,
        _cli_resume_session_id: cli_resume_session_id,
        session_id: session_id.clone(),
        should_exit: Arc::clone(&should_exit),
        session_registry: Arc::new(SessionRegistry::new()),
        skill_prompt: combined_append_prompt,
        memory_store,
        memory_archive,
        memory_cleanup,
        evolution_engine,
        cron_manager: Arc::new(engine::cron::CronManager::new()),
        project_registry: Arc::new(engine::projects::ProjectRegistry::new()),
        file_cache: Arc::new(tokio::sync::Mutex::new(
            engine::file_cache::FileCache::default_capacity(),
        )),
        tool_result_store: Some(Arc::new(
            engine::tool_result_store::ToolResultStore::for_session(&session_id),
        )),
        hook_manager: Arc::new(engine::hooks::HookManager::new()),
        team_executor,
    };

    // ══════════════════════════════════════════════════════════
    // Start cron scheduler — runs periodic jobs in background.
    // Each job gets a fresh QueryEngine to execute its prompt,
    // and results are broadcast to all connected clients.
    // ══════════════════════════════════════════════════════════
    {
        let cron_manager = Arc::clone(&shared.cron_manager);
        let cron_tools = shared.engine_tools.clone();
        let cron_api_client = Arc::clone(&shared.api_client);
        let cron_baoclaw_config = shared.baoclaw_config.clone();
        let cron_thinking_config = shared.cli_thinking_config.clone();
        let cron_append_prompt = shared.skill_prompt.clone();
        let cron_session_id = shared.session_id.clone();
        let cron_file_cache = Arc::clone(&shared.file_cache);
        let cron_tool_result_store = shared.tool_result_store.as_ref().map(Arc::clone);
        let cron_hook_manager = Arc::clone(&shared.hook_manager);

        let run_fn: Arc<
            dyn Fn(String, Option<String>) -> tokio::task::JoinHandle<String> + Send + Sync,
        > = Arc::new(move |prompt: String, cwd: Option<String>| {
            let tools = cron_tools.clone();
            let api_client = Arc::clone(&cron_api_client);
            let baoclaw_config = cron_baoclaw_config.clone();
            let thinking_config = cron_thinking_config.clone();
            let append_prompt = cron_append_prompt.clone();
            let _session_id = cron_session_id.clone();
            let file_cache = Arc::clone(&cron_file_cache);
            let tool_result_store = cron_tool_result_store.as_ref().map(Arc::clone);
            let hook_manager = Arc::clone(&cron_hook_manager);

            let job_session_id = format!("cron-{}", &uuid::Uuid::new_v4().to_string()[..8]);

            tokio::spawn(async move {
                let cwd_path = cwd.map(PathBuf::from).unwrap_or_else(|| {
                    std::env::var("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(|_| PathBuf::from("/tmp"))
                });

                let mut engine = QueryEngine::new(QueryEngineConfig {
                    cwd: cwd_path,
                    tools,
                    api_client,
                    model: baoclaw_config.model.clone(),
                    thinking_config,
                    max_turns: Some(10),
                    max_budget_usd: Some(0.5),
                    verbose: false,
                    custom_system_prompt: None,
                    append_system_prompt: append_prompt,
                    session_id: Some(job_session_id),
                    fallback_models: baoclaw_config.fallback_models.clone(),
                    max_retries_per_model: baoclaw_config.max_retries_per_model,
                    context_window: baoclaw_config.context_window,
                    auto_compact_threshold_ratio: baoclaw_config.auto_compact_threshold_ratio,
                    parent_turn_id: None,
                    agent_label: Some("cron".to_string()),
                    session_memory: None,
                    file_cache: Some(file_cache),
                    tool_result_store,
                    hook_manager: Some(hook_manager),
                    // Headless cron jobs must never hang on an interactive
                    // permission prompt — mutating tools fail closed instead.
                    permission: None,
                });

                let mut rx = engine.submit_message(prompt).await;
                let mut result = String::new();
                while let Some(event) = rx.recv().await {
                    match event {
                        EngineEvent::AssistantChunk { content, .. } => result.push_str(&content),
                        EngineEvent::Result(qr) => {
                            if let Some(text) = qr.text {
                                if !text.is_empty() && result.is_empty() {
                                    result = text;
                                }
                            }
                            break;
                        }
                        EngineEvent::Error(_) => break,
                        _ => {}
                    }
                }
                if result.is_empty() {
                    result = "(no output)".to_string();
                }
                result
            })
        });

        tokio::spawn(async move {
            cron_manager.start_scheduler(run_fn).await;
        });
    }

    // ══════════════════════════════════════════════════════════
    // Start memory cleanup scheduler — runs periodic memory maintenance.
    // Applies decay to memories, archives low-importance ones,
    // and cleans up the archive when it exceeds max_entries.
    // ══════════════════════════════════════════════════════════
    {
        let memory_cleanup = Arc::clone(&shared.memory_cleanup);
        tokio::spawn(async move {
            let _ = memory_cleanup.start().await;
        });
    }

    // ══════════════════════════════════════════════════════════
    // P3-1c: Graceful shutdown handler (SIGTERM/SIGINT)
    // On shutdown signal, persist all sessions to disk before exiting.
    // ══════════════════════════════════════════════════════════
    {
        let registry = Arc::clone(&shared.session_registry);
        tokio::spawn(async move {
            use tokio::signal;

            let ctrl_c = async {
                let _ = signal::ctrl_c().await;
            };

            #[cfg(unix)]
            let terminate = async {
                let mut sig = signal::unix::signal(signal::unix::SignalKind::terminate())
                    .expect("failed to install SIGTERM handler (init-time; no recovery possible)");
                sig.recv().await;
            };

            #[cfg(not(unix))]
            let terminate = std::future::pending::<()>();

            tokio::select! {
                _ = ctrl_c => {}
                _ = terminate => {}
            }

            eprintln!("[daemon] received shutdown signal, persisting sessions...");
            registry.persist_all().await;
            eprintln!("[daemon] shutdown complete, exiting");
            std::process::exit(0);
        });
    }

    // ══════════════════════════════════════════════════════════
    // Windows service shutdown monitor (P3-1d)
    // When running as a child of the service host (--daemon mode),
    // periodically check if SCM has requested stop via the service host.
    // The service host sets the shutdown flag, and this monitor persists
    // sessions and exits gracefully.
    // ══════════════════════════════════════════════════════════
    #[cfg(target_os = "windows")]
    {
        let reg = Arc::clone(&shared.session_registry);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                if windows_service::is_shutdown_requested() {
                    tracing::info!("Windows SCM requested shutdown, persisting sessions...");
                    eprintln!("[daemon] Windows SCM requested shutdown, persisting sessions...");
                    let _ = reg.persist_all().await;
                    eprintln!("[daemon] shutdown complete, exiting");
                    std::process::exit(0);
                }
            }
        });
    }

    // ══════════════════════════════════════════════════════════
    // Main accept loop — spawns a task per client connection
    // Multiple clients can be connected simultaneously, each
    // with its own independent QueryEngine / conversation history.
    // Only `shutdown` RPC terminates the daemon.
    // ══════════════════════════════════════════════════════════
    let should_exit_clone = Arc::clone(&should_exit);
    loop {
        if should_exit.load(Ordering::Relaxed) {
            eprintln!("should_exit detected — breaking accept loop");
            break;
        }
        eprintln!("Waiting for client connection...");

        // Use select to race accept against a periodic should_exit check
        // so shutdown actually terminates the daemon promptly
        let accept_result = tokio::select! {
            result = server.accept() => Some(result),
            _ = async {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if should_exit_clone.load(Ordering::Relaxed) {
                        break;
                    }
                }
            } => None,
        };

        match accept_result {
            None => {
                eprintln!("should_exit watcher fired — breaking accept loop");
                break;
            }
            Some(Ok(conn)) => {
                eprintln!("Client connected");
                let client_shared = shared.clone();
                tokio::spawn(async move {
                    handle_client(conn, client_shared).await;
                });
            }
            Some(Err(e)) => {
                eprintln!("Accept error: {}", e);
                continue;
            }
        }
    }

    // Cleanup
    cleanup_meta(&socket_path);
    drop(server);
    eprintln!("baoclaw-core shutdown complete");
    Ok(())
}
