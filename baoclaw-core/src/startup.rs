//! Daemon startup phases.
//!
//! Each free function here is the verbatim body of one coherent startup phase
//! formerly inlined in [`main`], in original order: CLI parsing, socket setup,
//! config/API-client construction, engine tool building, prompt/memory loading,
//! session-id resolution, shared-state assembly, background schedulers, and
//! the accept loop.

#![allow(clippy::too_many_arguments)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// How often the accept loop re-checks the shutdown flag while no client is
/// connecting.
const SHUTDOWN_CHECK_INTERVAL_MS: u64 = 500;

use baoclaw_core::{api, config, engine, ipc, permissions, state, tools};

use api::client::ApiClientConfig;
use api::unified::UnifiedClient;
use config::BaoclawConfig;
use engine::query_engine::{
    EngineEvent, QueryEngine, QueryEngineConfig, ThinkingConfig, EMPTY_USAGE,
};
use engine::task_manager::TaskManager;
use ipc::server::IpcServer;
use permissions::gate::PermissionGate;
use state::manager::{CoreState, StateManager};
use tools::builtins::{
    AgentTool, BashTool, FileEditTool, FileReadTool, FileWriteTool, ImageEditTool, ImageGenTool,
    MemoryTool, NotebookEditTool, ProjectNoteTool, TodoWriteTool, ToolSearchTool, WebFetchTool,
    WebSearchTool,
};

use crate::{resolve_daemon_socket, SharedState};

/// Parsed daemon launch options (CLI flags + resolved sandbox config).
pub(super) struct CliOptions {
    pub(super) is_daemon: bool,
    pub(super) cwd_str: String,
    pub(super) cli_resume_session_id: Option<String>,
    pub(super) cli_thinking_config: ThinkingConfig,
    pub(super) sandbox_config: Option<Arc<engine::sandbox::SandboxConfig>>,
}

/// Parse CLI flags: --daemon, --cwd, --resume, --think, --sandbox.
/// Invalid sandbox input aborts startup with an error.
pub(super) fn parse_cli_options(args: &[String]) -> Result<CliOptions, Box<dyn std::error::Error>> {
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

    Ok(CliOptions {
        is_daemon,
        cwd_str,
        cli_resume_session_id,
        cli_thinking_config,
        sandbox_config,
    })
}

/// Bind the daemon socket (fixed path first, cwd-hash fallback), announce the
/// socket path on stdout, and in daemon mode redirect stdout/stderr to a log.
pub(super) async fn bind_and_announce(
    cwd_str: &str,
    is_daemon: bool,
) -> Result<(PathBuf, IpcServer), Box<dyn std::error::Error>> {
    // Create socket: prefer fixed machine-level path (P3-1c), fall back to cwd-hash
    let socket_path = resolve_daemon_socket(cwd_str);

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

    Ok((socket_path, server))
}

/// Load BaoClaw config from ~/.baoclaw/config.json, resolve the primary model
/// profile, and construct (plus pre-warm) the unified API client.
pub(super) fn load_config_and_api_client() -> (BaoclawConfig, Arc<UnifiedClient>) {
    // Load BaoClaw config from ~/.baoclaw/config.json
    let mut baoclaw_config = config::load_config();
    config::apply_env_override(&mut baoclaw_config);
    // The executor reads the tool-output cap through this process-wide
    // accessor (it has no config handle at the truncation sites).
    config::init_tool_output_threshold(baoclaw_config.tool_output_threshold_chars);

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

    (baoclaw_config, api_client)
}

/// Build the daemon engine tool list (core tools, AgentTool with the full set,
/// ToolSearchTool last).
pub(super) fn build_engine_tools(
    cwd_str: &str,
    sandbox_config: &Option<Arc<engine::sandbox::SandboxConfig>>,
    api_client: &Arc<UnifiedClient>,
    evolution_engine: &Arc<engine::evolution::EvolutionEngine>,
    kit: &engine::kit::HeadlessEngineKit,
) -> (
    Vec<Arc<dyn tools::Tool>>,
    permissions::GrantedSearchDirs,
    permissions::GrantedWriteDirs,
) {
    // Allow tools to access ~/.baoclaw/ in addition to project cwd
    let home_dir = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let baoclaw_home = std::path::PathBuf::from(&home_dir).join(".baoclaw");
    let additional_dirs = vec![baoclaw_home.clone()];

    // Shared allow-list of extra directories Glob/Grep may search (config
    // seed + interactive "Always allow" grants). Seeded after the permission
    // context loads; the Arc is shared with the executor via SharedState so
    // grants apply without a restart.
    let granted_search_dirs = permissions::GrantedSearchDirs::default();

    // Write twin: shared allow-list of extra directories FileWrite/FileEdit
    // may write. Kept DISTINCT from the search list so a read grant can
    // never widen the write boundary; seeded (with ~/.baoclaw parity) after
    // the permission context loads.
    let granted_write_dirs = permissions::GrantedWriteDirs::default();

    // Build the core tool list (everything except AgentTool itself, which is added after)
    // BashTool is optionally sandboxed based on --sandbox CLI flag
    let bash_tool: BashTool = match &sandbox_config {
        Some(cfg) => BashTool::with_sandbox(Arc::clone(cfg)),
        None => BashTool::new(),
    };
    let core_tools: Vec<Arc<dyn tools::Tool>> = vec![
        Arc::new(bash_tool),
        Arc::new(FileReadTool::new(additional_dirs.clone())),
        Arc::new(FileWriteTool::new(vec![]).with_granted_dirs(Arc::clone(&granted_write_dirs))),
        Arc::new(FileEditTool::new(vec![]).with_granted_dirs(Arc::clone(&granted_write_dirs))),
        Arc::new(tools::builtins::GlobTool::with_granted_dirs(Arc::clone(
            &granted_search_dirs,
        ))),
        Arc::new(tools::builtins::GrepTool::with_granted_dirs(Arc::clone(
            &granted_search_dirs,
        ))),
        Arc::new(WebFetchTool::new()),
        Arc::new(WebSearchTool::new()),
        Arc::new(ImageGenTool::new()),
        Arc::new(ImageEditTool::new()),
        Arc::new(NotebookEditTool::new()),
        Arc::new(TodoWriteTool::new()),
        Arc::new(MemoryTool::new()),
        Arc::new(ProjectNoteTool::new()),
        Arc::new(tools::builtins::SkillTool::new(PathBuf::from(cwd_str))),
        Arc::new(tools::builtins::EvolveTool::new(Arc::clone(
            evolution_engine,
        ))),
    ];

    // AgentTool gets the full core tool set so sub-agents can write, edit, run bash, etc.
    let agent_tool =
        AgentTool::new_with_full_tools(Arc::clone(api_client), core_tools.clone(), kit.clone());

    let mut engine_tools: Vec<Arc<dyn tools::Tool>> = core_tools;
    engine_tools.push(Arc::new(agent_tool));

    // ToolSearchTool needs the full tool list, so register it last
    let engine_tools: Vec<Arc<dyn tools::Tool>> = {
        let mut all = engine_tools;

        all.push(Arc::new(ToolSearchTool::new(all.clone())));
        eprintln!("Total tools registered: {}", all.len());
        all
    };

    (engine_tools, granted_search_dirs, granted_write_dirs)
}

/// Load skill prompt and the user profile; combine them into the
/// append_system_prompt fragment. Long-term memory is deliberately NOT part
/// of this frozen string — the engine rebuilds the memory fragment per query
/// (QueryEngineConfig.memory_store) so mid-session MemoryTool writes reach
/// the model without a restart. Returns (combined prompt, memory store,
/// profile manager).
pub(super) async fn load_prompts_and_memory(
    cwd_str: &str,
) -> (
    Option<String>,
    Arc<engine::memory::MemoryStore>,
    Arc<engine::user_profile::UserProfileManager>,
) {
    // Load skill content for system prompt injection
    let skill_prompt =
        baoclaw_core::discovery::skills::load_skills_for_prompt(std::path::Path::new(cwd_str))
            .await;
    if let Some(ref sp) = skill_prompt {
        eprintln!("Loaded skills into system prompt ({} chars)", sp.len());
    }

    // Load long-term memory (injected live per query — see QueryEngineConfig)
    let memory_store = Arc::new(engine::memory::MemoryStore::load());

    // Load the persistent user profile (~/.baoclaw/USER.md)
    let profile_manager = Arc::new(engine::user_profile::UserProfileManager::new());
    let profile_prompt = profile_manager.build_prompt_fragment();
    if let Some(ref pp) = profile_prompt {
        eprintln!(
            "Loaded user profile into system prompt ({} chars)",
            pp.len()
        );
    }

    // Combine skill + profile into append_system_prompt (memory rides in
    // live per query)
    let combined_append_prompt = {
        let mut parts = Vec::new();
        if let Some(sp) = skill_prompt {
            parts.push(sp);
        }
        if let Some(pp) = profile_prompt {
            parts.push(pp);
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    };

    (combined_append_prompt, memory_store, profile_manager)
}

/// Reuse existing project session or create new one.
/// One project directory = one session file.
pub(super) fn resolve_session_id(cwd_str: &str) -> String {
    let cwd_key = crate::cwd_hash(cwd_str);
    let session_id = match engine::transcript::find_latest_session_for_cwd(cwd_str) {
        Some(legacy_id) => {
            let legacy_prefix = format!("{}-", crate::legacy_cwd_hash(cwd_str));
            if let Some(suffix) = legacy_id.strip_prefix(&legacy_prefix) {
                let normalized_id = format!("{}-{}", cwd_key, suffix);
                let sessions_dir = engine::session_persistence::default_sessions_dir();
                let migrated = engine::session_persistence::migrate_legacy_session(
                    &sessions_dir,
                    &legacy_id,
                    &normalized_id,
                    cwd_str,
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
    session_id
}

/// Assemble the daemon [`SharedState`]: state manager, permission
/// gate/manager, task manager, team executor, memory archive/cleanup, and all
/// shared engine resources.
pub(super) async fn assemble_shared_state(
    is_daemon: bool,
    baoclaw_config: BaoclawConfig,
    api_client: Arc<UnifiedClient>,
    engine_tools: Vec<Arc<dyn tools::Tool>>,
    granted_search_dirs: permissions::GrantedSearchDirs,
    granted_write_dirs: permissions::GrantedWriteDirs,
    tool_health: engine::tool_health::ToolHealthHandle,
    evolution_engine: Arc<engine::evolution::EvolutionEngine>,
    memory_store: Arc<engine::memory::MemoryStore>,
    user_profile: Arc<engine::user_profile::UserProfileManager>,
    combined_append_prompt: Option<String>,
    telemetry: Option<Arc<engine::telemetry::collector::TelemetryCollector>>,
    file_cache: Arc<tokio::sync::Mutex<engine::file_cache::FileCache>>,
    tool_result_store: Option<Arc<engine::tool_result_store::ToolResultStore>>,
    headless_kit: engine::kit::HeadlessEngineKit,
    cli_thinking_config: ThinkingConfig,
    cli_resume_session_id: Option<String>,
    session_id: String,
) -> (SharedState, Arc<AtomicBool>) {
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
    // Seed the search-dir allow-list: config (`permissions.additional_search_dirs`)
    // plus the default ~/.baoclaw parity the file tools already get. Grown
    // live by interactive "Always allow" grants in the executor.
    let granted_dirs = Arc::clone(&granted_search_dirs);
    {
        let home_dir = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let mut dirs = granted_dirs.write().unwrap();
        dirs.push(std::path::PathBuf::from(home_dir).join(".baoclaw"));
        if let Ok(ctx) = permission_manager.try_read() {
            for d in &ctx.get_context().additional_search_dirs {
                dirs.push(std::path::PathBuf::from(d));
            }
        }
    }
    // Seed the write-dir allow-list the same way: ~/.baoclaw parity (the
    // static roots FileWrite/FileEdit used to get) plus config
    // (`permissions.additional_write_dirs`). Grown live by interactive
    // "Always allow" grants in the executor.
    {
        let home_dir = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let mut dirs = granted_write_dirs.write().unwrap();
        dirs.push(std::path::PathBuf::from(home_dir).join(".baoclaw"));
        if let Ok(ctx) = permission_manager.try_read() {
            for d in &ctx.get_context().additional_write_dirs {
                dirs.push(std::path::PathBuf::from(d));
            }
        }
    }

    // Kit assembly: the headless bundle is built by the caller (main) so
    // AgentTool can already carry it; here it just flows into SharedState.

    // Create TaskManager for background task execution
    let task_manager = Arc::new(TaskManager::new(
        Arc::clone(&api_client),
        engine_tools.clone(),
        headless_kit.clone(),
    ));

    // Team state store — execution builds a fresh TeamExecutor per RPC from
    // the daemon's shared resources.
    let team_executor = Arc::new(engine::team::TeamManager::new());

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
        granted_dirs,
        granted_write_dirs,
        tool_health,
        task_manager,
        state_manager,
        baoclaw_config,
        cli_thinking_config,
        _cli_resume_session_id: cli_resume_session_id,
        session_id: session_id.clone(),
        should_exit: Arc::clone(&should_exit),
        session_registry: Arc::new(engine::shared_session::SessionRegistry::new()),
        skill_prompt: combined_append_prompt,
        memory_store,
        user_profile,
        telemetry,
        memory_archive,
        memory_cleanup,
        evolution_engine,
        cron_manager: Arc::new(engine::cron::CronManager::new()),
        project_registry: Arc::new(engine::projects::ProjectRegistry::new()),
        file_cache,
        tool_result_store,
        headless_kit,
        team_executor,
    };

    (shared, should_exit)
}

/// Start cron scheduler — runs periodic jobs in background.
/// Each job gets a fresh QueryEngine to execute its prompt,
/// and results are broadcast to all connected clients.
pub(super) async fn start_cron_scheduler(shared: &SharedState) {
    {
        let cron_manager = Arc::clone(&shared.cron_manager);
        let cron_tools = shared.engine_tools.clone();
        let cron_api_client = Arc::clone(&shared.api_client);
        let cron_model = shared.baoclaw_config.model.clone();
        let cron_thinking_config = shared.cli_thinking_config.clone();
        let cron_headless_kit = shared.headless_kit.clone();
        let cron_session_id = shared.session_id.clone();

        let run_fn: Arc<
            dyn Fn(String, Option<String>) -> tokio::task::JoinHandle<String> + Send + Sync,
        > = Arc::new(move |prompt: String, cwd: Option<String>| {
            let tools = cron_tools.clone();
            let api_client = Arc::clone(&cron_api_client);
            let model = cron_model.clone();
            let thinking_config = cron_thinking_config.clone();
            let append_prompt = cron_headless_kit.append_system_prompt.clone();
            let _session_id = cron_session_id.clone();
            let headless_kit = cron_headless_kit.clone();

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
                    model,
                    tool_health: Some(Arc::clone(&headless_kit.tool_health)),
                    thinking_config,
                    max_turns: Some(10),
                    max_budget_usd: Some(0.5),
                    verbose: false,
                    custom_system_prompt: None,
                    append_system_prompt: append_prompt,
                    session_id: Some(job_session_id),
                    fallback_models: headless_kit.fallback_models.clone(),
                    max_retries_per_model: headless_kit.max_retries_per_model,
                    context_window: headless_kit.context_window,
                    auto_compact_threshold_ratio: headless_kit.auto_compact_threshold_ratio,
                    parent_turn_id: None,
                    agent_label: Some("cron".to_string()),
                    session_memory: None,
                    file_cache: headless_kit.file_cache.clone(),
                    tool_result_store: headless_kit.tool_result_store.clone(),
                    // Headless cron jobs must never hang on an interactive
                    // permission prompt — mutating tools fail closed instead.
                    permission: None,
                    telemetry: headless_kit.telemetry.clone(),
                    evolution: headless_kit.evolution.clone(),
                    memory_store: headless_kit.memory_store.clone(),
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
}

/// Start memory cleanup scheduler and install the graceful-shutdown signal
/// handlers (SIGTERM/SIGINT; Windows SCM monitor on Windows).
pub(super) fn start_background_tasks(shared: &SharedState) {
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
                if crate::windows_service::is_shutdown_requested() {
                    tracing::info!("Windows SCM requested shutdown, persisting sessions...");
                    eprintln!("[daemon] Windows SCM requested shutdown, persisting sessions...");
                    let _ = reg.persist_all().await;
                    eprintln!("[daemon] shutdown complete, exiting");
                    std::process::exit(0);
                }
            }
        });
    }
}

/// Main accept loop — spawns a task per client connection.
/// Multiple clients can be connected simultaneously, each
/// with its own independent QueryEngine / conversation history.
/// Only `shutdown` RPC terminates the daemon.
pub(super) async fn run_accept_loop(
    server: &IpcServer,
    shared: &SharedState,
    should_exit: &Arc<AtomicBool>,
) {
    let should_exit_clone = Arc::clone(should_exit);
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
                    tokio::time::sleep(std::time::Duration::from_millis(SHUTDOWN_CHECK_INTERVAL_MS))
                        .await;
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
                    crate::handle_client(conn, client_shared).await;
                });
            }
            Some(Err(e)) => {
                eprintln!("Accept error: {}", e);
                continue;
            }
        }
    }
}
