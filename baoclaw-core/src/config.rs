// Configuration file loading and management for BaoClaw
//
// Config file path: ~/.baoclaw/config.json

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

/// Default model name.
fn default_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}

/// Default max retries per model.
fn default_max_retries() -> u32 {
    2
}

/// Default model context window size in tokens (Claude default).
fn default_context_window() -> u64 {
    200_000
}

/// Default ratio of context window at which to auto-compact.
fn default_compact_threshold() -> f64 {
    0.7
}

/// Default per-request output token cap sent to the model API.
fn default_max_tokens() -> u32 {
    16_384
}

/// Default hard ceiling for a single Bash invocation, in milliseconds.
fn default_bash_max_timeout_ms() -> u64 {
    300_000
}

/// Default cap on how many concurrency-safe tools run in parallel.
fn default_max_parallel_tools() -> usize {
    8
}

// ─── ModelProfile ───────────────────────────────────────────────────────────

/// A model configuration profile with its own API credentials and window.
/// Each profile can independently specify api_type, api_key, base_url, etc.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelProfile {
    /// Model name (e.g., "glm-5.2", "deepseek-chat").
    pub model: String,
    /// API protocol: "anthropic" or "openai".
    pub api_type: String,
    /// API key (plaintext; will migrate to keychain later).
    #[serde(default)]
    pub api_key: Option<String>,
    /// Base URL (e.g., "https://open.bigmodel.cn/api/anthropic").
    #[serde(default)]
    pub base_url: Option<String>,
    /// Context window in tokens.
    #[serde(default = "default_context_window")]
    pub context_window: u64,
    /// Auto-compact threshold ratio (0.0-1.0).
    #[serde(default = "default_compact_threshold")]
    pub auto_compact_threshold_ratio: f64,
    /// Max retries before falling back to next profile.
    #[serde(default = "default_max_retries")]
    pub max_retries_per_model: u32,
}

/// BaoClaw configuration loaded from ~/.baoclaw/config.json.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BaoclawConfig {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default = "default_max_retries")]
    pub max_retries_per_model: u32,
    /// API type: "anthropic" (default) or "openai"
    #[serde(default = "default_api_type")]
    pub api_type: String,
    /// OpenAI-compatible API base URL (used when api_type is "openai")
    #[serde(default)]
    pub openai_base_url: Option<String>,
    /// Model context window size in tokens (e.g. 200_000 for Claude, 128_000 for GPT-4).
    #[serde(default = "default_context_window")]
    pub context_window: u64,
    /// Auto-compact threshold as fraction of context_window (e.g. 0.7 = 70%).
    #[serde(default = "default_compact_threshold")]
    pub auto_compact_threshold_ratio: f64,
    /// Maximum chars before tool output is persisted to disk (default 200_000).
    #[serde(default = "default_tool_output_threshold_chars")]
    pub tool_output_threshold_chars: usize,
    /// Hard cap on agent turns per query. None (default) = unbounded — the
    /// loop runs until the model stops issuing tool calls. Headless engines
    /// set their own tighter limits regardless of this knob.
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// Per-query cost ceiling in USD. When the accumulated cost of the
    /// current query reaches it, the model gets one grace call to produce
    /// its final answer and is then stopped with a `budget_exceeded` error.
    /// None (default) = no cost limit.
    #[serde(default)]
    pub max_budget_usd: Option<f64>,
    /// Output token cap sent with every model request (default 16_384).
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Hard ceiling for a single Bash invocation in milliseconds; a tool-call
    /// `timeout` above it is clamped down (default 300_000).
    #[serde(default = "default_bash_max_timeout_ms")]
    pub bash_max_timeout_ms: u64,
    /// Maximum number of concurrency-safe tools executed in parallel per
    /// turn; excess requests queue on a semaphore (default 8).
    #[serde(default = "default_max_parallel_tools")]
    pub max_parallel_tools: usize,
    /// Master switch for telemetry recording. When false, the daemon drops
    /// turn/session events instead of writing them to ~/.baoclaw/telemetry.db.
    /// Toggled at runtime via the `telemetry.setEnabled` RPC (/telemetry on|off)
    /// and persisted back to this field.
    #[serde(default = "default_telemetry_enabled")]
    pub telemetry_enabled: bool,

    // === New: Named model profiles (P1-1) ===
    /// Named model profiles (new format). Each profile has its own api_type,
    /// api_key, base_url, context_window, etc.
    #[serde(default)]
    pub model_profiles: HashMap<String, ModelProfile>,
    /// Primary profile name (if using profiles format).
    #[serde(default)]
    pub primary_profile: Option<String>,
    /// Fallback profile names (if using profiles format).
    #[serde(default)]
    pub fallback_profiles: Vec<String>,

    /// Preserve unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

fn default_api_type() -> String {
    "anthropic".to_string()
}
pub fn default_telemetry_enabled() -> bool {
    true
}
pub fn default_tool_output_threshold_chars() -> usize {
    200_000
}

static TOOL_OUTPUT_THRESHOLD: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// Initialize the process-wide tool-output threshold from the loaded config.
/// Called once at daemon startup; later calls are no-ops (first value wins).
pub fn init_tool_output_threshold(chars: usize) {
    let _ = TOOL_OUTPUT_THRESHOLD.set(chars);
}

/// The effective global cap on a single tool output, in characters. The
/// executor applies the smaller of this and the tool's own per-tool limit,
/// so a tighter tool limit always wins. Falls back to the default until
/// startup initializes it (tests, early call paths).
pub fn tool_output_threshold() -> usize {
    *TOOL_OUTPUT_THRESHOLD
        .get()
        .unwrap_or(&default_tool_output_threshold_chars())
}

static BASH_MAX_TIMEOUT_MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// Initialize the process-wide Bash timeout ceiling from the loaded config.
/// Called once at daemon startup; later calls are no-ops (first value wins).
pub fn init_bash_max_timeout_ms(ms: u64) {
    let _ = BASH_MAX_TIMEOUT_MS.set(ms);
}

/// The effective hard ceiling for a single Bash invocation, in milliseconds.
/// The Bash tool clamps the per-call `timeout` input to this value, so a
/// longer test suite or build can be allowed via config without code changes.
pub fn bash_max_timeout_ms() -> u64 {
    *BASH_MAX_TIMEOUT_MS
        .get()
        .unwrap_or(&default_bash_max_timeout_ms())
}

static MAX_PARALLEL_TOOLS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// Initialize the process-wide parallel-tool cap from the loaded config.
/// Called once at daemon startup; later calls are no-ops (first value wins).
pub fn init_max_parallel_tools(n: usize) {
    let _ = MAX_PARALLEL_TOOLS.set(n);
}

/// The effective cap on how many concurrency-safe tools run in parallel per
/// turn. Falls back to the default until startup initializes it. A configured
/// 0 is clamped to 1 — a zero-permit semaphore would deadlock every tool call.
pub fn max_parallel_tools() -> usize {
    (*MAX_PARALLEL_TOOLS
        .get()
        .unwrap_or(&default_max_parallel_tools()))
    .max(1)
}

impl Default for BaoclawConfig {
    fn default() -> Self {
        Self {
            model: default_model(),
            fallback_models: Vec::new(),
            max_retries_per_model: default_max_retries(),
            api_type: default_api_type(),
            openai_base_url: None,
            context_window: default_context_window(),
            auto_compact_threshold_ratio: default_compact_threshold(),
            tool_output_threshold_chars: default_tool_output_threshold_chars(),
            max_turns: None,
            max_budget_usd: None,
            max_tokens: default_max_tokens(),
            bash_max_timeout_ms: default_bash_max_timeout_ms(),
            max_parallel_tools: default_max_parallel_tools(),
            telemetry_enabled: default_telemetry_enabled(),
            model_profiles: HashMap::new(),
            primary_profile: None,
            fallback_profiles: Vec::new(),
            extra: HashMap::new(),
        }
    }
}

/// Returns the config file path: ~/.baoclaw/config.json
pub fn config_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".baoclaw").join("config.json")
}

/// Load configuration from ~/.baoclaw/config.json.
/// If the file does not exist, creates a default config file and returns defaults.
/// If the file contains invalid JSON, logs a warning and returns defaults.
/// Missing fields are filled with defaults; unknown fields are preserved.
pub fn load_config() -> BaoclawConfig {
    load_config_from(&config_path())
}

/// Load configuration from a specific path (for testing).
pub fn load_config_from(path: &std::path::Path) -> BaoclawConfig {
    let mut config = match std::fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str::<BaoclawConfig>(&content) {
            Ok(config) => config,
            Err(e) => {
                eprintln!(
                    "Warning: invalid config JSON at {}: {}, using defaults",
                    path.display(),
                    e
                );
                BaoclawConfig::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Create default config file
            if let Err(write_err) = save_default_config_to(path) {
                eprintln!(
                    "Warning: could not create default config at {}: {}",
                    path.display(),
                    write_err
                );
            }
            BaoclawConfig::default()
        }
        Err(e) => {
            eprintln!(
                "Warning: could not read config at {}: {}, using defaults",
                path.display(),
                e
            );
            BaoclawConfig::default()
        }
    };

    // Normalize: auto-migrate old format to profiles if needed
    normalize_profiles(&mut config);
    // Sync new format back to old format for backward compatibility
    sync_profiles_to_legacy(&mut config);
    config
}

/// Normalize config: if old format (model + fallback_models strings),
/// auto-convert to new format (model_profiles + primary_profile).
/// This is called automatically by `load_config_from`.
pub fn normalize_profiles(config: &mut BaoclawConfig) {
    if !config.model_profiles.is_empty() {
        return; // Already using new format
    }
    if config.model.is_empty() {
        return; // Nothing to migrate
    }

    // Auto-migrate old format → profiles
    let primary = ModelProfile {
        model: config.model.clone(),
        api_type: config.api_type.clone(),
        api_key: None, // API keys come from env vars or profile.api_key
        base_url: config.openai_base_url.clone(),
        context_window: config.context_window,
        auto_compact_threshold_ratio: config.auto_compact_threshold_ratio,
        max_retries_per_model: config.max_retries_per_model,
    };
    config.model_profiles.insert("primary".to_string(), primary);
    config.primary_profile = Some("primary".to_string());

    for (i, m) in config.fallback_models.iter().enumerate() {
        let name = format!("fallback_{}", i);
        let p = ModelProfile {
            model: m.clone(),
            api_type: config.api_type.clone(), // inherit primary api_type
            api_key: None,
            base_url: config.openai_base_url.clone(),
            context_window: config.context_window,
            auto_compact_threshold_ratio: config.auto_compact_threshold_ratio,
            max_retries_per_model: config.max_retries_per_model,
        };
        config.model_profiles.insert(name.clone(), p);
        config.fallback_profiles.push(name);
    }
}

/// Sync new format back to old format fields for backward compatibility.
/// This ensures that code using config.model and config.fallback_models
/// still works when the config uses model_profiles.
pub fn sync_profiles_to_legacy(config: &mut BaoclawConfig) {
    if config.model_profiles.is_empty() {
        return; // Nothing to sync
    }

    // Sync primary model
    if let Some(ref primary_name) = config.primary_profile {
        if let Some(primary_profile) = config.model_profiles.get(primary_name.as_str()) {
            config.model = primary_profile.model.clone();
            config.api_type = primary_profile.api_type.clone();
            config.context_window = primary_profile.context_window;
            config.auto_compact_threshold_ratio = primary_profile.auto_compact_threshold_ratio;
            // The retry chain reads only the top-level field — sync it too,
            // otherwise the per-profile value is parsed but never consumed.
            config.max_retries_per_model = primary_profile.max_retries_per_model;
        }
    }

    // Sync fallback_models from fallback_profiles
    config.fallback_models.clear();
    for fallback_name in config.fallback_profiles.iter() {
        if let Some(fallback_profile) = config.model_profiles.get(fallback_name.as_str()) {
            config.fallback_models.push(fallback_profile.model.clone());
        }
    }
}

/// Save the default configuration to ~/.baoclaw/config.json.
pub fn save_default_config() -> Result<(), std::io::Error> {
    save_default_config_to(&config_path())
}

/// Save the default configuration to a specific path (for testing).
pub fn save_default_config_to(path: &std::path::Path) -> Result<(), std::io::Error> {
    save_config_to(&BaoclawConfig::default(), path)
}

/// Save a configuration to a specific path.
pub fn save_config_to(
    config: &BaoclawConfig,
    path: &std::path::Path,
) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    let json = serde_json::to_string_pretty(config).map_err(std::io::Error::other)?;
    // Write via temp file + rename: a crash mid-write must not truncate the
    // real config (it carries the plaintext api_key and permission rules).
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

impl BaoclawConfig {
    /// Save this configuration to ~/.baoclaw/config.json.
    pub fn save(&self) -> Result<(), std::io::Error> {
        save_config_to(self, &config_path())
    }

    /// Save this configuration to a specific path.
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), std::io::Error> {
        save_config_to(self, path)
    }
}

/// Apply environment variable overrides to the config.
/// If ANTHROPIC_MODEL is set, it overrides the primary model.
pub fn apply_env_override(config: &mut BaoclawConfig) {
    if let Ok(model) = std::env::var("ANTHROPIC_MODEL") {
        if !model.is_empty() {
            config.model = model;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Serializes tests that mutate process-global env vars: `std::env` is
    /// process-wide, so parallel test threads would otherwise race and fail
    /// intermittently.
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn config_in(dir: &std::path::Path) -> PathBuf {
        dir.join("config.json")
    }

    #[test]
    fn test_default_values() {
        let config = BaoclawConfig::default();
        assert_eq!(config.model, "claude-sonnet-4-20250514");
        assert!(config.fallback_models.is_empty());
        assert_eq!(config.max_retries_per_model, 2);
        assert!(config.extra.is_empty());
    }

    #[test]
    fn test_file_not_exist_creates_default() {
        let dir = TempDir::new().unwrap();
        let path = config_in(dir.path());
        assert!(!path.exists());

        let config = load_config_from(&path);
        assert_eq!(config.model, "claude-sonnet-4-20250514");
        assert!(path.exists(), "config file should be created");

        // Verify the created file is valid JSON with correct defaults
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: BaoclawConfig = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.model, "claude-sonnet-4-20250514");
    }

    #[test]
    fn test_invalid_json_fallback() {
        let dir = TempDir::new().unwrap();
        let path = config_in(dir.path());
        std::fs::write(&path, "not valid json {{{").unwrap();

        let config = load_config_from(&path);
        assert_eq!(config.model, "claude-sonnet-4-20250514");
        assert_eq!(config.max_retries_per_model, 2);
    }

    #[test]
    fn test_missing_fields_filled() {
        let dir = TempDir::new().unwrap();
        let path = config_in(dir.path());
        // Only specify model, missing fallback_models and max_retries_per_model
        std::fs::write(&path, r#"{"model": "claude-opus-4-20250514"}"#).unwrap();

        let config = load_config_from(&path);
        assert_eq!(config.model, "claude-opus-4-20250514");
        assert!(config.fallback_models.is_empty());
        assert_eq!(config.max_retries_per_model, 2);
    }

    #[test]
    fn test_unknown_fields_preserved() {
        let dir = TempDir::new().unwrap();
        let path = config_in(dir.path());
        std::fs::write(
            &path,
            r#"{
            "model": "claude-sonnet-4-20250514",
            "fallback_models": [],
            "max_retries_per_model": 2,
            "future_feature": true,
            "theme": "dark"
        }"#,
        )
        .unwrap();

        let config = load_config_from(&path);
        assert_eq!(config.extra.get("future_feature"), Some(&Value::Bool(true)));
        assert_eq!(
            config.extra.get("theme"),
            Some(&Value::String("dark".to_string()))
        );
    }

    #[test]
    fn test_env_override_model() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Save and restore so we don't leak global state to other tests.
        let original = std::env::var("ANTHROPIC_MODEL").ok();
        std::env::set_var("ANTHROPIC_MODEL", "claude-opus-4-20250514");
        let mut config = BaoclawConfig::default();
        apply_env_override(&mut config);
        assert_eq!(config.model, "claude-opus-4-20250514");
        match original {
            Some(v) => std::env::set_var("ANTHROPIC_MODEL", v),
            None => std::env::remove_var("ANTHROPIC_MODEL"),
        }
    }

    #[test]
    fn test_env_override_not_set() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Save and restore so we don't leak global state to other tests.
        let original = std::env::var("ANTHROPIC_MODEL").ok();
        std::env::remove_var("ANTHROPIC_MODEL");
        let mut config = BaoclawConfig::default();
        apply_env_override(&mut config);
        assert_eq!(config.model, "claude-sonnet-4-20250514");
        match original {
            Some(v) => std::env::set_var("ANTHROPIC_MODEL", v),
            None => std::env::remove_var("ANTHROPIC_MODEL"),
        }
    }

    #[test]
    fn test_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = config_in(dir.path());

        let mut original = BaoclawConfig {
            model: "claude-opus-4-20250514".to_string(),
            fallback_models: vec![
                "claude-sonnet-4-20250514".to_string(),
                "claude-3-5-haiku-20241022".to_string(),
            ],
            max_retries_per_model: 3,
            api_type: "anthropic".to_string(),
            max_turns: Some(25),
            max_budget_usd: Some(2.5),
            max_tokens: 8192,
            bash_max_timeout_ms: 450_000,
            max_parallel_tools: 4,
            telemetry_enabled: false,
            openai_base_url: None,
            context_window: 200_000,
            auto_compact_threshold_ratio: 0.7,
            tool_output_threshold_chars: 200_000,
            model_profiles: HashMap::new(),
            primary_profile: None,
            fallback_profiles: Vec::new(),
            extra: {
                let mut m = HashMap::new();
                m.insert(
                    "custom_key".to_string(),
                    Value::String("custom_value".to_string()),
                );
                m
            },
        };

        save_config_to(&original, &path).unwrap();
        let loaded = load_config_from(&path);
        // normalize_profiles runs during load_config_from, so normalize original too
        normalize_profiles(&mut original);
        assert_eq!(original, loaded);
    }

    #[test]
    fn test_auto_migrate_old_format() {
        let dir = TempDir::new().unwrap();
        let path = config_in(dir.path());

        // Old-format config: only model + fallback_models (no model_profiles)
        std::fs::write(
            &path,
            r#"{
            "model": "claude-sonnet-4-20250514",
            "fallback_models": ["claude-3-5-haiku-20241022", "claude-opus-4-20250514"],
            "api_type": "anthropic",
            "context_window": 200000
        }"#,
        )
        .unwrap();

        let config = load_config_from(&path);

        // primary_profile should be set
        assert_eq!(config.primary_profile.as_deref(), Some("primary"));

        // model_profiles should contain "primary" + 2 fallbacks
        assert_eq!(config.model_profiles.len(), 3);
        assert!(config.model_profiles.contains_key("primary"));
        assert!(config.model_profiles.contains_key("fallback_0"));
        assert!(config.model_profiles.contains_key("fallback_1"));

        // fallback_profiles should list the fallback names
        assert_eq!(config.fallback_profiles, vec!["fallback_0", "fallback_1"]);

        // Primary profile should carry the model name
        let primary = config.model_profiles.get("primary").unwrap();
        assert_eq!(primary.model, "claude-sonnet-4-20250514");
        assert_eq!(primary.api_type, "anthropic");
        assert_eq!(primary.context_window, 200_000);

        // Fallback profiles should inherit api_type from primary
        let fb0 = config.model_profiles.get("fallback_0").unwrap();
        assert_eq!(fb0.model, "claude-3-5-haiku-20241022");
        assert_eq!(fb0.api_type, "anthropic");

        let fb1 = config.model_profiles.get("fallback_1").unwrap();
        assert_eq!(fb1.model, "claude-opus-4-20250514");
        assert_eq!(fb1.api_type, "anthropic");
    }

    #[test]
    fn test_invalid_path_is_directory() {
        let dir = TempDir::new().unwrap();
        // Path points to a directory, not a file. std::fs::read_to_string will fail with EISDIR/permission error.
        let path = dir.path();

        let config = load_config_from(path);
        let mut expected = BaoclawConfig::default();
        normalize_profiles(&mut expected);
        sync_profiles_to_legacy(&mut expected);
        assert_eq!(config, expected);
    }

    #[test]
    fn test_invalid_path_unwritable_parent() {
        let dir = TempDir::new().unwrap();
        let dummy_file = dir.path().join("dummy_file");
        std::fs::write(&dummy_file, "content").unwrap();

        // Path is inside dummy_file (which is a file, not a directory)
        let invalid_path = dummy_file.join("config.json");

        // std::fs::read_to_string will return NotFound or NotADirectory depending on OS,
        // and attempting to create/save default config will fail.
        let config = load_config_from(&invalid_path);
        let mut expected = BaoclawConfig::default();
        normalize_profiles(&mut expected);
        sync_profiles_to_legacy(&mut expected);
        assert_eq!(config, expected);
    }

    #[test]
    fn save_config_is_atomic_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join("baoclaw-config-atomic-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");

        let mut cfg = BaoclawConfig::default();
        cfg.extra.insert(
            "permissions".to_string(),
            serde_json::json!({"mode": "Default", "always_allow_rules": {"user": []}}),
        );
        cfg.save_to(&path).expect("save succeeds");
        cfg.save_to(&path).expect("re-save succeeds");

        // The temp file must be renamed away, never left behind.
        assert!(!dir.join("config.json.tmp").exists());
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk["permissions"]["mode"], "Default");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
