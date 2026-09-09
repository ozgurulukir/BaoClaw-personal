// `lc_*` handlers take `&PathBuf` to mirror the verbatim former inline code
// (same allowance as `shared_client.rs`).
#![allow(clippy::ptr_arg)]

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

const IPC_PROTOCOL_VERSION: &str = "1";
/// Poll interval while waiting for a turn's submit-drain task to finish.
const SUBMIT_DRAIN_POLL_MS: u64 = 100;
/// Poll interval for the service supervisor watching the daemon subprocess.
#[cfg(windows)]
const SERVICE_CHILD_POLL_MS: u64 = 500;
use std::path::PathBuf;
use tokio::sync::Mutex as TokioMutex;

use baoclaw_core::{api, config, engine, ipc, models, permissions, state, tools};

mod shared_client;
mod startup;

#[cfg(target_os = "windows")]
mod windows_service;

use api::unified::UnifiedClient;
use config::BaoclawConfig;
use engine::query_engine::{EngineEvent, QueryEngine, QueryEngineConfig, ThinkingConfig};
use engine::shared_session::{ClientId, SessionRegistry, SharedSession};
use engine::task_manager::TaskManager;
use ipc::events::engine_event_to_notification;
use ipc::protocol::JsonRpcMessage;
use ipc::router::{parse_client_method, ClientMethod};
use ipc::server::{IpcConnection, IpcError, IpcWriter};
use permissions::gate::PermissionGate;
use permissions::PermissionBridge;
use state::manager::StateManager;

/// Shared state cloned into each spawned client task.
#[derive(Clone)]
struct SharedState {
    engine_tools: Vec<Arc<dyn tools::Tool>>,
    api_client: Arc<UnifiedClient>,
    permission_gate: PermissionGate,
    permission_manager: Arc<tokio::sync::RwLock<permissions::manager::PermissionManager>>,
    /// Out-of-cwd directories Glob/Grep may search (config seed + live grants).
    granted_dirs: permissions::GrantedSearchDirs,
    /// Out-of-cwd directories FileWrite/FileEdit may write (config seed +
    /// live grants). Distinct list: read grants never widen the write boundary.
    granted_write_dirs: permissions::GrantedWriteDirs,
    /// Daemon-wide tool-health tracker (failure accumulation + Disabled blocking).
    tool_health: std::sync::Arc<engine::tool_health::ToolHealthTracker>,
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
    user_profile: Arc<engine::user_profile::UserProfileManager>,
    /// Local telemetry recorder (None if the telemetry DB cannot be opened).
    telemetry: Option<Arc<engine::telemetry::collector::TelemetryCollector>>,
    memory_archive: Arc<engine::memory::MemoryArchive>,
    memory_cleanup: Arc<engine::memory::MemoryCleanupScheduler>,
    evolution_engine: Arc<engine::evolution::EvolutionEngine>,
    cron_manager: Arc<engine::cron::CronManager>,
    project_registry: Arc<engine::projects::ProjectRegistry>,
    /// Shared file cache (LRU) for reducing redundant file reads.
    file_cache: Arc<tokio::sync::Mutex<engine::file_cache::FileCache>>,
    /// Tool result store for persisting large outputs to disk.
    tool_result_store: Option<Arc<engine::tool_result_store::ToolResultStore>>,
    /// Shared resources threaded into every headless engine (cron, tasks,
    /// teams, sub-agents) — full interactive parity minus permissions.
    headless_kit: engine::kit::HeadlessEngineKit,
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

/// Compute a stable hash of the working directory path: 16 hex chars for a
/// short, stable ID. Delegates to the engine's canonical implementation so
/// session-id construction and resume's surface matching never diverge.
fn cwd_hash(cwd: &str) -> String {
    baoclaw_core::engine::transcript::cwd_identity_hash(cwd)
}

fn legacy_cwd_hash(cwd: &str) -> String {
    cwd_hash(cwd)[..8].to_string()
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
        tool_health: Some(std::sync::Arc::clone(&shared.tool_health)),
        max_turns: shared.baoclaw_config.max_turns,
        max_budget_usd: shared.baoclaw_config.max_budget_usd,
        verbose: false,
        custom_system_prompt: None,
        append_system_prompt: shared.skill_prompt.clone(),
        session_id: Some(session_id.clone()),
        fallback_models: shared.baoclaw_config.fallback_models.clone(),
        max_retries_per_model: shared.baoclaw_config.max_retries_per_model,
        context_window: shared.baoclaw_config.context_window,
        auto_compact_threshold_ratio: shared.baoclaw_config.auto_compact_threshold_ratio,
        max_tokens: shared.baoclaw_config.max_tokens,
        max_tokens_budget: None,
        telemetry: shared.telemetry.clone(),
        evolution: Some(Arc::clone(&shared.evolution_engine)),
        memory_store: Some(Arc::clone(&shared.memory_store)),
        parent_turn_id: None,
        agent_label: None,
        session_memory: Some(Arc::new(
            crate::engine::session_memory::SessionMemory::load(&session_id),
        )),
        file_cache: Some(Arc::clone(&shared.file_cache)),
        tool_result_store: shared.tool_result_store.clone(),
        permission: Some(PermissionBridge {
            manager: Arc::clone(&shared.permission_manager),
            gate: shared.permission_gate.clone(),
            granted_dirs: Arc::clone(&shared.granted_dirs),
            granted_write_dirs: Arc::clone(&shared.granted_write_dirs),
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
                    // Terminal events are hand-delivered to the submitting
                    // client by its turn drain. Consume the hand-off first:
                    // once `release_submitter` has run, the check below no
                    // longer skips for the submitting client, and without the
                    // hand-off it would receive the Result/Error a second
                    // time (duplicated assistant messages on chat gateways).
                    if matches!(event, EngineEvent::Result(_) | EngineEvent::Error(_))
                        && session.take_terminal_handoff(client_id).await
                    {
                        continue;
                    }
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
    // Wait for initialize request (handshake + protocol/resume validation)
    let (init_id, init_cwd, init_model, init_shared_session_id) =
        match lc_read_initialize(&mut conn).await {
            Some(t) => t,
            None => return,
        };

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

        let (session, is_new, resumed) = shared
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
        let resumed =
            lc_restore_session_history(&session, is_new, resumed, &work_cwd, &session_id_clone)
                .await;

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
            tokio::time::sleep(std::time::Duration::from_millis(SUBMIT_DRAIN_POLL_MS)).await;
        }

        // Client disconnect handling (Task 6.1)
        lc_session_close(
            &shared_clone,
            &session,
            client_id,
            &session_id_clone,
            &hook_cwd,
        )
        .await;

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

/// Read and validate the `initialize` handshake from a legacy connection.
/// Returns `None` (after replying with the appropriate error) when the
/// handshake fails and the connection must be dropped.
async fn lc_read_initialize(
    conn: &mut IpcConnection,
) -> Option<(
    ipc::protocol::RequestId,
    PathBuf,
    Option<String>,
    Option<String>,
)> {
    // Wait for initialize request
    let init_msg = match conn.recv_message().await {
        Ok(msg) => msg,
        Err(IpcError::ConnectionClosed) => {
            eprintln!("Client disconnected before initialize");
            return None;
        }
        Err(e) => {
            eprintln!("Error reading init: {}", e);
            return None;
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
                    return None;
                }
                Err(e) => {
                    let _ = conn
                        .send_error(Some(req.id), -32600, format!("Invalid init: {}", e))
                        .await;
                    return None;
                }
            }
        }
        _ => {
            return None;
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
            return None;
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
        return None;
    }

    Some((init_id, init_cwd, init_model, init_shared_session_id))
}

/// Restore session history for a shared session: snapshot-first, legacy
/// transcript fallback. Returns the final `resumed` flag.
///
/// Inspired by Claude Code: load pre-written summary + recent tail,
/// NEVER rebuild the full history or do on-demand API summarization.
async fn lc_restore_session_history(
    session: &Arc<SharedSession>,
    is_new: bool,
    mut resumed: bool,
    work_cwd: &PathBuf,
    session_id: &str,
) -> bool {
    let current_msg_count = session.engine_read().await.get_messages().len();
    if (is_new || current_msg_count == 0) && !resumed {
        let cwd_str_for_resume = work_cwd.to_string_lossy().to_string();
        // Surface-scoped resume: only this session's own transcript, never
        // another surface's (telegram must not inherit the web session).
        if let Some(rid) =
            engine::transcript::find_latest_session_for_cwd(&cwd_str_for_resume, session_id)
        {
            match engine::transcript::TranscriptWriter::load(&rid) {
                Ok(entries) => {
                    let entry_count = entries.len();
                    let old_summary_obj = crate::engine::session_memory::SessionMemory::load(&rid);
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
    resumed
}

/// Handle client disconnect for the last client of a shared session (Task 6.1):
/// session-close evolution hook, heuristic session memory, final persistence,
/// and session removal.
async fn lc_session_close(
    shared: &SharedState,
    session: &Arc<SharedSession>,
    client_id: ClientId,
    session_id: &str,
    hook_cwd: &str,
) {
    let is_last = session.remove_client(client_id).await;
    let cleanup_guard = if is_last {
        shared
            .session_registry
            .acquire_last_client_cleanup(session_id, session)
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
                                message, ..
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

            shared
                .evolution_engine
                .on_session_close(
                    session_id,
                    hook_cwd,
                    &model,
                    &messages_clone,
                    &usage,
                    estimated_cost,
                    duration_secs,
                )
                .await;

            // Merge this session's stats into the persistent user profile
            // (~/.baoclaw/USER.md). Turns = user messages; tools_used counts
            // ToolUse blocks across assistant messages.
            {
                use crate::models::message::MessageContent;
                use std::collections::BTreeMap;
                let mut turns = 0u64;
                let mut tool_counts: BTreeMap<String, u32> = BTreeMap::new();
                for msg in &messages_clone {
                    match &msg.content {
                        // Synthetic tool-result/system injections are not
                        // turns — same semantics as the search backfill.
                        MessageContent::User { is_meta, .. } if !*is_meta => turns += 1,
                        MessageContent::Assistant { message, .. } => {
                            for block in &message.content {
                                if let crate::models::message::ContentBlock::ToolUse {
                                    name, ..
                                } = block
                                {
                                    *tool_counts.entry(name.clone()).or_insert(0) += 1;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let tools_total: u32 = tool_counts.values().sum();
                shared
                    .user_profile
                    .merge_session_stats(&engine::user_profile::SessionStats {
                        turns,
                        cost_usd: estimated_cost,
                        tools_used: tool_counts.into_iter().collect(),
                        duration_secs: duration_secs as f64,
                        // Task-type classification is not implemented; left
                        // empty rather than guessed.
                        task_types: Vec::new(),
                    });
                shared.user_profile.save();

                // Session-level telemetry (same values as the profile merge).
                if let Some(ref telemetry) = shared.telemetry {
                    let now = chrono::Utc::now().timestamp();
                    let total_tokens = usage.input_tokens + usage.output_tokens;
                    if let Err(e) = telemetry.record_session(
                        session_id,
                        now - duration_secs as i64,
                        now,
                        turns,
                        total_tokens,
                        estimated_cost,
                        tools_total as u64,
                        // File-change counting is not wired; honest zero.
                        0,
                    ) {
                        eprintln!("Telemetry record_session failed: {}", e);
                    }
                }

                // Finalize the cross-session search index: the query loop
                // stubbed this row (foreign key for message indexing); the
                // upsert here fills in the final turn count, cost and end
                // time.
                let now_rfc3339 = chrono::Utc::now().to_rfc3339();
                let started_rfc3339 = (chrono::Utc::now()
                    - chrono::Duration::seconds(duration_secs as i64))
                .to_rfc3339();
                if let Ok(db) = engine::cross_session_db::CrossSessionDb::new() {
                    let summary = engine::cross_session_db::SessionIndex {
                        id: session_id.to_string(),
                        cwd: hook_cwd.to_string(),
                        model,
                        started_at: started_rfc3339,
                        ended_at: now_rfc3339,
                        turn_count: turns as i32,
                        cost_usd: estimated_cost,
                    };
                    if let Err(e) = db.index_session(summary) {
                        eprintln!(
                            "[cross-session] WARNING: session summary not indexed: {}",
                            e
                        );
                    }
                }
            }
        }

        match shared.session_registry.persist_session(session_id).await {
            Ok(()) => {
                shared
                    .session_registry
                    .remove_after_last_client_cleanup(session_id)
                    .await;
                eprintln!(
                    "Shared session '{}' removed (last client disconnected)",
                    session_id
                );
            }
            Err(error) => eprintln!(
                "[daemon] WARNING: keeping session '{}' in memory because final persistence failed: {}",
                session_id, error
            ),
        }
    }
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
                std::thread::sleep(std::time::Duration::from_millis(SERVICE_CHILD_POLL_MS));
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

    // Parse CLI flags (--daemon, --cwd, --resume, --think, --sandbox)
    let opts = startup::parse_cli_options(&args)?;

    // Bind daemon socket, announce it on stdout, redirect logs in daemon mode
    let (socket_path, server) = startup::bind_and_announce(&opts.cwd_str, opts.is_daemon).await?;

    // Load config, resolve model profile, build and pre-warm the API client
    let (baoclaw_config, api_client) = startup::load_config_and_api_client();

    // Load skill prompt + long-term memory, combine into append_system_prompt
    let (combined_append_prompt, memory_store, user_profile) =
        startup::load_prompts_and_memory(&opts.cwd_str).await;

    // Reuse existing project session or create a new one
    let session_id = startup::resolve_session_id(&opts.cwd_str);

    // Backfill historical session snapshots into the cross-session search
    // index. Idempotent (already-indexed sessions are skipped) and runs in
    // the background so startup never waits on it; daemon mode only —
    // one-shot CLI runs must not pay the scan.
    if opts.is_daemon {
        tokio::task::spawn_blocking(|| match engine::cross_session_db::CrossSessionDb::new() {
            Ok(db) => {
                let (sessions, messages) = db
                    .backfill_from_snapshots(&engine::session_persistence::default_sessions_dir());
                if sessions > 0 {
                    eprintln!(
                        "[cross-session] backfill: imported {} historical session(s) ({} messages)",
                        sessions, messages
                    );
                }
            }
            Err(e) => eprintln!("[cross-session] backfill skipped: {}", e),
        });
    }

    // Daemon-wide singletons shared by the kit, the tools and SharedState
    let tool_health = engine::tool_health::ToolHealthHandle::default();
    let evolution_engine = Arc::new(engine::evolution::EvolutionEngine::new(
        std::path::Path::new(&opts.cwd_str),
    ));
    let telemetry_collector = match engine::telemetry::collector::TelemetryCollector::new() {
        Ok(c) => {
            c.set_enabled(baoclaw_config.telemetry_enabled);
            Some(Arc::new(c))
        }
        Err(e) => {
            eprintln!("Telemetry disabled (DB open failed): {}", e);
            None
        }
    };
    let file_cache = Arc::new(tokio::sync::Mutex::new(
        engine::file_cache::FileCache::default_capacity(),
    ));
    let tool_result_store = Some(Arc::new(
        engine::tool_result_store::ToolResultStore::for_session(&session_id),
    ));

    // Headless engine kit: cron jobs, background tasks, team agents and
    // sub-agents run with full interactive parity minus the permission
    // bridge (they must never hang on a prompt).
    let headless_kit = engine::kit::HeadlessEngineKit {
        append_system_prompt: combined_append_prompt.clone(),
        fallback_models: baoclaw_config.fallback_models.clone(),
        max_retries_per_model: baoclaw_config.max_retries_per_model,
        context_window: baoclaw_config.context_window,
        auto_compact_threshold_ratio: baoclaw_config.auto_compact_threshold_ratio,
        max_tokens: baoclaw_config.max_tokens,
        max_budget_usd: baoclaw_config.max_budget_usd,
        telemetry: telemetry_collector.clone(),
        evolution: Some(Arc::clone(&evolution_engine)),
        memory_store: Some(Arc::clone(&memory_store)),
        file_cache: Some(Arc::clone(&file_cache)),
        tool_result_store: tool_result_store.clone(),
        tool_health: Arc::clone(&tool_health),
    };

    // Build engine tools (core tools + AgentTool + ToolSearchTool)
    let (engine_tools, granted_search_dirs, granted_write_dirs) = startup::build_engine_tools(
        &opts.cwd_str,
        &opts.sandbox_config,
        &api_client,
        &evolution_engine,
        &headless_kit,
        &memory_store,
    );

    // Write metadata file for discovery by CLI
    write_meta(&socket_path, &opts.cwd_str, &session_id);

    // Assemble the daemon SharedState
    let (shared, should_exit) = startup::assemble_shared_state(
        opts.is_daemon,
        baoclaw_config,
        api_client,
        engine_tools,
        granted_search_dirs,
        granted_write_dirs,
        tool_health,
        evolution_engine,
        memory_store,
        user_profile,
        combined_append_prompt,
        telemetry_collector,
        file_cache,
        tool_result_store,
        headless_kit,
        opts.cli_thinking_config,
        opts.cli_resume_session_id,
        session_id,
    )
    .await;

    // Start cron scheduler, memory cleanup, and shutdown signal handlers
    startup::start_cron_scheduler(&shared).await;
    startup::start_background_tasks(&shared);

    // Main accept loop — one task per client connection
    startup::run_accept_loop(&server, &shared, &should_exit).await;

    // Cleanup
    cleanup_meta(&socket_path);
    drop(server);
    eprintln!("baoclaw-core shutdown complete");
    Ok(())
}
