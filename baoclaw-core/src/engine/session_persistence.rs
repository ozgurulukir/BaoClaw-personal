//! Session persistence to disk (JSON files) for crash recovery.
//!
//! Each session's conversation state is persisted to `~/.baoclaw/sessions/<id>.json`
//! using atomic write (tmp + rename). A registry index file tracks all sessions.
//!
//! ## Storage Layout
//! ```text
//! ~/.baoclaw/sessions/
//! ├── registry.json          # session index (id/cwd/created_at/last_active)
//! ├── <session-id-1>.json    # single session full state (messages + metadata)
//! ├── <session-id-2>.json
//! └── archive/               # archived sessions (>7 days inactive)
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::models::message::Message;

/// Maximum age (in days) before a session is archived.
const DEFAULT_ARCHIVE_AGE_DAYS: u64 = 7;
pub const CURRENT_SCHEMA_VERSION: u32 = 1;
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);
static PERSISTENCE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Session IDs are used as filename components and must never contain path
/// separators or parent-directory components.
pub fn is_valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Build a path for a session artifact without accepting raw path components.
pub fn session_artifact_path(
    sessions_dir: &Path,
    session_id: &str,
    suffix: &str,
) -> io::Result<PathBuf> {
    validate_session_id(session_id)?;
    reject_symlinked_path_prefix(sessions_dir)?;
    if suffix.is_empty()
        || suffix.contains('/')
        || suffix.contains('\\')
        || suffix.chars().any(|character| character.is_control())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session artifact suffix contains invalid path characters",
        ));
    }
    let path = sessions_dir.join(format!("{}.{}", session_id, suffix));
    reject_existing_symlink(&path)?;
    Ok(path)
}

/// Build a directory path for session-scoped artifacts after validation.
pub fn session_directory_path(sessions_dir: &Path, session_id: &str) -> io::Result<PathBuf> {
    validate_session_id(session_id)?;
    reject_symlinked_path_prefix(sessions_dir)?;
    let path = sessions_dir.join(session_id);
    reject_existing_symlink(&path)?;
    Ok(path)
}

fn reject_symlinked_path_prefix(path: &Path) -> io::Result<()> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if let Ok(metadata) = fs::symlink_metadata(candidate) {
            if metadata.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "session storage path contains a symbolic link",
                ));
            }
        }
        let parent = candidate.parent();
        if parent == Some(candidate) {
            break;
        }
        current = parent;
    }
    Ok(())
}

fn reject_existing_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "session storage target is a symbolic link",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn persistence_lock() -> &'static Mutex<()> {
    PERSISTENCE_LOCK.get_or_init(|| Mutex::new(()))
}

fn default_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

/// One entry in the registry index.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub session_id: String,
    pub cwd: String,
    pub created_at: String,
    pub last_active: String,
}

/// The full registry index (serialized as registry.json).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionRegistryIndex {
    pub sessions: Vec<RegistryEntry>,
}

/// The persisted state of a single session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedSession {
    /// Snapshot schema version. Missing legacy values are treated as version 1.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub session_id: String,
    pub cwd: String,
    pub model: String,
    pub created_at: String,
    pub last_active: String,
    pub messages: Vec<Message>,
    /// Session memory summary text (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_summary: Option<String>,
}

/// Compute the default sessions directory: `~/.baoclaw/sessions/`.
pub fn default_sessions_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".baoclaw").join("sessions")
}

/// Ensure a directory exists, creating it (and parents) if needed.
fn ensure_dir(dir: &Path) -> io::Result<()> {
    reject_symlinked_path_prefix(dir)?;
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Ensure the session storage directory exists without following symlinks.
pub fn ensure_session_storage_dir(dir: &Path) -> io::Result<()> {
    ensure_dir(dir)
}

fn secure_file(path: &Path) -> io::Result<()> {
    reject_existing_symlink(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Atomically write content to a file.
///
/// Writes to a unique sibling temp file first, then renames to `<path>`.
/// This prevents corruption if the process is killed mid-write.
/// The rename is atomic on the same filesystem (POSIX guarantee).
pub fn atomic_write(path: &Path, content: &str) -> io::Result<()> {
    reject_symlinked_path_prefix(path)?;
    reject_existing_symlink(path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if parent == Path::new(".") {
        fs::create_dir_all(parent)?;
    } else {
        ensure_dir(parent)?;
    }
    let suffix = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), suffix));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        use std::io::Write;
        file.write_all(content.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Path for a session's JSON state file.
fn session_file_path(sessions_dir: &Path, session_id: &str) -> io::Result<PathBuf> {
    session_artifact_path(sessions_dir, session_id, "json")
}

/// Path for the registry index file.
fn registry_file_path(sessions_dir: &Path) -> io::Result<PathBuf> {
    reject_symlinked_path_prefix(sessions_dir)?;
    let path = sessions_dir.join("registry.json");
    reject_existing_symlink(&path)?;
    Ok(path)
}

/// Path for the archive subdirectory.
fn archive_dir(sessions_dir: &Path) -> PathBuf {
    sessions_dir.join("archive")
}

// ── Registry Index Operations ──

/// Load the registry index from disk. Returns empty if missing.
pub fn load_registry(sessions_dir: &Path) -> SessionRegistryIndex {
    let _guard = persistence_lock().lock().unwrap_or_else(|e| e.into_inner());
    load_registry_unlocked(sessions_dir)
}

fn load_registry_unlocked(sessions_dir: &Path) -> SessionRegistryIndex {
    let path = match registry_file_path(sessions_dir) {
        Ok(path) => path,
        Err(error) => {
            eprintln!(
                "[session-persist] WARNING: rejected registry storage path: {}",
                error
            );
            return SessionRegistryIndex::default();
        }
    };
    match fs::read_to_string(&path) {
        Ok(content) => {
            if let Err(error) = secure_file(&path) {
                eprintln!(
                    "[session-persist] WARNING: failed to secure {}: {}",
                    path.display(),
                    error
                );
            }
            match serde_json::from_str::<SessionRegistryIndex>(&content) {
                Ok(index) => SessionRegistryIndex {
                    sessions: index
                        .sessions
                        .into_iter()
                        .filter(|entry| is_valid_session_id(&entry.session_id))
                        .collect(),
                },
                Err(e) => {
                    let suffix = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
                    let backup = sessions_dir.join(format!("registry.json.corrupt.{}", suffix));
                    let preserved = match fs::rename(&path, &backup) {
                        Ok(()) => {
                            eprintln!(
                                concat!(
                                    "[session-persist] WARNING: registry.json corrupted: {}. ",
                                    "Moved it to {} and rebuilding from session snapshots."
                                ),
                                e,
                                backup.display()
                            );
                            true
                        }
                        Err(rename_error) => {
                            eprintln!(
                                concat!(
                                "[session-persist] WARNING: registry.json corrupted: {}. ",
                                "Could not preserve it ({}); rebuilding from session snapshots."
                            ),
                                e, rename_error
                            );
                            false
                        }
                    };
                    let rebuilt = rebuild_registry(sessions_dir);
                    if preserved {
                        if let Err(error) = save_registry_unlocked(sessions_dir, &rebuilt) {
                            eprintln!(
                                "[session-persist] WARNING: failed to save rebuilt registry: {}",
                                error
                            );
                        }
                    }
                    rebuilt
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => SessionRegistryIndex::default(),
        Err(error) => {
            eprintln!(
                "[session-persist] WARNING: failed to read {}: {}",
                path.display(),
                error
            );
            SessionRegistryIndex::default()
        }
    }
}

fn rebuild_registry(sessions_dir: &Path) -> SessionRegistryIndex {
    let mut sessions = fs::read_dir(sessions_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json")
                || path.file_name().and_then(|name| name.to_str()) == Some("registry.json")
            {
                return None;
            }
            let expected_id = path.file_stem()?.to_str()?.to_string();
            reject_existing_symlink(&path).ok()?;
            let content = fs::read_to_string(path).ok()?;
            let state = serde_json::from_str::<PersistedSession>(&content).ok()?;
            (state.session_id == expected_id
                && state.schema_version == CURRENT_SCHEMA_VERSION
                && is_valid_session_id(&state.session_id))
            .then_some(state)
        })
        .map(|state| RegistryEntry {
            session_id: state.session_id,
            cwd: state.cwd,
            created_at: state.created_at,
            last_active: state.last_active,
        })
        .collect::<Vec<_>>();
    sessions.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    SessionRegistryIndex { sessions }
}

/// Save the registry index to disk (atomic write).
pub fn save_registry(sessions_dir: &Path, index: &SessionRegistryIndex) -> io::Result<()> {
    let _guard = persistence_lock().lock().unwrap_or_else(|e| e.into_inner());
    save_registry_unlocked(sessions_dir, index)
}

fn save_registry_unlocked(sessions_dir: &Path, index: &SessionRegistryIndex) -> io::Result<()> {
    ensure_dir(sessions_dir)?;
    let path = registry_file_path(sessions_dir)?;
    let json = serde_json::to_string_pretty(index)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    atomic_write(&path, &json)
}

/// Update or insert a registry entry, then persist the index.
pub fn upsert_registry_entry(
    sessions_dir: &Path,
    session_id: &str,
    cwd: &str,
    created_at: &str,
    last_active: &str,
) -> io::Result<()> {
    validate_session_id(session_id)?;
    let _guard = persistence_lock().lock().unwrap_or_else(|e| e.into_inner());
    upsert_registry_entry_unlocked(sessions_dir, session_id, cwd, created_at, last_active)
}

fn upsert_registry_entry_unlocked(
    sessions_dir: &Path,
    session_id: &str,
    cwd: &str,
    created_at: &str,
    last_active: &str,
) -> io::Result<()> {
    ensure_dir(sessions_dir)?;
    let mut index = load_registry_unlocked(sessions_dir);
    let now = last_active.to_string();

    if let Some(entry) = index
        .sessions
        .iter_mut()
        .find(|e| e.session_id == session_id)
    {
        entry.cwd = cwd.to_string();
        entry.last_active = now;
    } else {
        index.sessions.push(RegistryEntry {
            session_id: session_id.to_string(),
            cwd: cwd.to_string(),
            created_at: created_at.to_string(),
            last_active: now,
        });
    }

    save_registry_unlocked(sessions_dir, &index)
}

// ── Session State Persistence ──

/// Persist a session's full state to disk (atomic write).
///
/// Serializes messages + metadata to `<session_id>.json`.
/// Also updates the registry index's `last_active`.
pub fn persist_session_state(sessions_dir: &Path, state: &PersistedSession) -> io::Result<()> {
    validate_session_id(&state.session_id)?;
    let _guard = persistence_lock().lock().unwrap_or_else(|e| e.into_inner());
    ensure_dir(sessions_dir)?;

    // 1. Write the session state file
    let path = session_file_path(sessions_dir, &state.session_id)?;
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    atomic_write(&path, &json)?;

    // 2. Update registry index
    if let Err(error) = upsert_registry_entry_unlocked(
        sessions_dir,
        &state.session_id,
        &state.cwd,
        &state.created_at,
        &state.last_active,
    ) {
        return Err(io::Error::new(
            error.kind(),
            format!(
                "snapshot for session {} persisted, but registry update failed: {}. To recover, retry persistence and inspect the sessions directory.",
                state.session_id, error
            ),
        ));
    }

    Ok(())
}

/// Load a session's persisted state from disk.
///
/// Returns `None` if the file doesn't exist.
/// Logs a warning and returns `None` if the file is corrupted.
pub fn load_session_state(sessions_dir: &Path, session_id: &str) -> Option<PersistedSession> {
    if !is_valid_session_id(session_id) {
        eprintln!("[session-persist] WARNING: rejected invalid session ID during load");
        return None;
    }
    let path = session_file_path(sessions_dir, session_id).ok()?;
    match fs::read_to_string(&path) {
        Ok(content) => {
            if let Err(error) = secure_file(&path) {
                eprintln!(
                    "[session-persist] WARNING: failed to secure {}: {}",
                    path.display(),
                    error
                );
            }
            match serde_json::from_str::<PersistedSession>(&content) {
                Ok(state) if state.session_id == session_id => Some(state),
                Ok(_) => {
                    eprintln!(
                        "[session-persist] WARNING: session {} snapshot ID does not match its filename",
                        session_id
                    );
                    None
                }
                Err(e) => {
                    eprintln!(
                        "[session-persist] WARNING: session {} state corrupted: {}. Skipping.",
                        session_id, e
                    );
                    None
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            eprintln!(
                "[session-persist] WARNING: failed to read session {} at {}: {}",
                session_id,
                path.display(),
                error
            );
            None
        }
    }
}

/// Copy a trusted legacy session snapshot to a new normalized session key.
/// The legacy file is retained until a later cleanup so migration cannot lose data.
pub fn migrate_legacy_session(
    sessions_dir: &Path,
    legacy_id: &str,
    new_id: &str,
    expected_cwd: &str,
) -> io::Result<bool> {
    validate_session_id(legacy_id)?;
    validate_session_id(new_id)?;
    if legacy_id == new_id || load_session_state(sessions_dir, new_id).is_some() {
        return Ok(false);
    }
    let Some(mut state) = load_session_state(sessions_dir, legacy_id) else {
        return Ok(false);
    };
    if state.cwd != expected_cwd {
        return Ok(false);
    }
    let source_dir = session_directory_path(sessions_dir, legacy_id)?;
    if source_dir.is_dir() {
        validate_directory_tree(&source_dir)?;
    }
    let target_dir = session_directory_path(sessions_dir, new_id)?;
    for suffix in ["jsonl", "memory.md", "baseline.json"] {
        session_artifact_path(sessions_dir, legacy_id, suffix)?;
        session_artifact_path(sessions_dir, new_id, suffix)?;
    }
    state.session_id = new_id.to_string();
    persist_session_state(sessions_dir, &state)?;

    for suffix in ["jsonl", "memory.md", "baseline.json"] {
        let source = session_artifact_path(sessions_dir, legacy_id, suffix)?;
        let target = session_artifact_path(sessions_dir, new_id, suffix)?;
        if source.exists() && !target.exists() {
            fs::copy(source, &target)?;
            secure_file(&target)?;
        }
    }
    if source_dir.is_dir() && !target_dir.exists() {
        copy_directory(&source_dir, &target_dir)?;
    }
    let mut registry = load_registry(sessions_dir);
    if registry
        .sessions
        .iter()
        .any(|entry| entry.session_id == legacy_id)
    {
        registry
            .sessions
            .retain(|entry| entry.session_id != legacy_id);
        save_registry(sessions_dir, &registry)?;
    }
    Ok(true)
}

fn validate_directory_tree(path: &Path) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "legacy session artifact contains a symbolic link",
            ));
        }
        if file_type.is_dir() {
            validate_directory_tree(&entry.path())?;
        }
    }
    Ok(())
}

fn copy_directory(source: &Path, target: &Path) -> io::Result<()> {
    ensure_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "legacy session artifact contains a symbolic link",
            ));
        }
        if file_type.is_dir() {
            copy_directory(&entry.path(), &destination)?;
        } else if !destination.exists() {
            fs::copy(entry.path(), &destination)?;
            secure_file(&destination)?;
        }
    }
    Ok(())
}

// ── Archive Stale Sessions ──

/// Move sessions inactive for more than `max_age_days` to the archive/ subdirectory.
///
/// This only operates on files on disk — in-memory sessions are not affected
/// (the caller should handle evicting stale sessions from the registry).
///
/// Returns the list of archived session IDs.
pub fn archive_stale_sessions(sessions_dir: &Path, max_age_days: u64) -> io::Result<Vec<String>> {
    let _guard = persistence_lock().lock().unwrap_or_else(|e| e.into_inner());
    archive_stale_sessions_unlocked(sessions_dir, max_age_days)
}

fn archive_stale_sessions_unlocked(
    sessions_dir: &Path,
    max_age_days: u64,
) -> io::Result<Vec<String>> {
    let index = load_registry_unlocked(sessions_dir);
    let now = Utc::now();
    let archive = archive_dir(sessions_dir);
    let mut archived = Vec::new();
    let mut moved_artifacts = Vec::new();

    for entry in &index.sessions {
        if !is_valid_session_id(&entry.session_id) {
            eprintln!("[session-persist] WARNING: skipped invalid session ID during archive");
            continue;
        }
        // Parse last_active timestamp
        let last_active = entry
            .last_active
            .parse::<DateTime<Utc>>()
            .or_else(|_| {
                chrono::NaiveDateTime::parse_from_str(&entry.last_active, "%Y-%m-%dT%H:%M:%S%.fZ")
                    .map(|ndt| DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc))
            })
            .ok();

        let last_active = match last_active {
            Some(dt) => dt,
            None => continue, // Skip unparseable timestamps
        };

        let age = now.signed_duration_since(last_active);
        if age.num_days() > max_age_days as i64 {
            let src = match session_file_path(sessions_dir, &entry.session_id) {
                Ok(path) => path,
                Err(error) => {
                    eprintln!(
                        "[session-persist] WARNING: rejected session {} during archive: {}",
                        entry.session_id, error
                    );
                    continue;
                }
            };
            if src.exists() {
                match archive_session_artifacts(sessions_dir, &archive, &entry.session_id) {
                    Ok(mut moved) => {
                        moved_artifacts.append(&mut moved);
                        archived.push(entry.session_id.clone());
                    }
                    Err(error) => eprintln!(
                        "[session-persist] WARNING: failed to archive session {}: {}",
                        entry.session_id, error
                    ),
                }
            }
        }
    }

    // Remove archived sessions from the registry index
    if !archived.is_empty() {
        let mut new_index = index;
        new_index
            .sessions
            .retain(|e| !archived.contains(&e.session_id));
        if let Err(error) = save_registry_unlocked(sessions_dir, &new_index) {
            rollback_moved_artifacts(&mut moved_artifacts);
            return Err(error);
        }
    }

    Ok(archived)
}

fn archive_session_artifacts(
    sessions_dir: &Path,
    archive: &Path,
    session_id: &str,
) -> io::Result<Vec<(PathBuf, PathBuf)>> {
    ensure_dir(archive)?;
    let mut moved = Vec::new();
    let mut artifacts = vec![(
        session_file_path(sessions_dir, session_id)?,
        archive.join(format!("{}.json", session_id)),
    )];
    artifacts.extend(
        ["jsonl", "memory.md", "baseline.json"]
            .into_iter()
            .map(|suffix| -> io::Result<(PathBuf, PathBuf)> {
                Ok((
                    session_artifact_path(sessions_dir, session_id, suffix)?,
                    archive.join(format!("{}.{}", session_id, suffix)),
                ))
            })
            .collect::<io::Result<Vec<_>>>()?,
    );
    let tool_results = session_directory_path(sessions_dir, session_id)?;
    if tool_results.exists() {
        artifacts.push((tool_results, archive.join(session_id)));
    }

    for (source, destination) in artifacts {
        reject_existing_symlink(&destination)?;
        if !source.exists() {
            continue;
        }
        if destination.exists() {
            let error = io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "archive destination already exists: {}",
                    destination.display()
                ),
            );
            rollback_moved_artifacts(&mut moved);
            return Err(error);
        }
        if let Err(error) = fs::rename(&source, &destination) {
            rollback_moved_artifacts(&mut moved);
            return Err(error);
        }
        moved.push((source, destination));
    }

    Ok(moved)
}

fn rollback_moved_artifacts(moved: &mut Vec<(PathBuf, PathBuf)>) {
    while let Some((source, destination)) = moved.pop() {
        if let Err(error) = fs::rename(&destination, &source) {
            eprintln!(
                "[session-persist] ERROR: failed to roll back archive move {} -> {}: {}",
                destination.display(),
                source.display(),
                error
            );
        }
    }
}

/// Convenience: archive sessions with the default 7-day threshold.
pub fn archive_stale_default(sessions_dir: &Path) -> io::Result<Vec<String>> {
    archive_stale_sessions(sessions_dir, DEFAULT_ARCHIVE_AGE_DAYS)
}

// ── Remove a session from disk ──

/// Delete a session's state file and remove it from the registry index.
pub fn delete_session(sessions_dir: &Path, session_id: &str) -> io::Result<()> {
    validate_session_id(session_id)?;
    let _guard = persistence_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = session_file_path(sessions_dir, session_id)?;
    let mut deletion_error = None;
    if let Err(e) = fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "[session-persist] WARNING: could not delete {}: {}",
                path.display(),
                e
            );
            deletion_error = Some(e);
        }
    }

    for suffix in ["jsonl", "memory.md", "baseline.json"] {
        let related = session_artifact_path(sessions_dir, session_id, suffix)?;
        if let Err(e) = fs::remove_file(&related) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "[session-persist] WARNING: could not delete {}: {}",
                    related.display(),
                    e
                );
                if deletion_error.is_none() {
                    deletion_error = Some(e);
                }
            }
        }
    }
    let tool_results = session_directory_path(sessions_dir, session_id)?;
    if tool_results.exists() {
        let result = if tool_results.is_dir() {
            fs::remove_dir_all(&tool_results)
        } else {
            fs::remove_file(&tool_results)
        };
        if let Err(e) = result {
            eprintln!(
                "[session-persist] WARNING: could not delete {}: {}",
                tool_results.display(),
                e
            );
            if deletion_error.is_none() {
                deletion_error = Some(e);
            }
        }
    }
    if let Some(error) = deletion_error {
        return Err(error);
    }

    let mut index = load_registry_unlocked(sessions_dir);
    index.sessions.retain(|e| e.session_id != session_id);
    save_registry_unlocked(sessions_dir, &index)
}

fn validate_session_id(session_id: &str) -> io::Result<()> {
    if is_valid_session_id(session_id) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session ID must contain only ASCII letters, digits, '-' or '_' and be at most 128 bytes",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_test_dir() -> TempDir {
        tempfile::tempdir().expect("failed to create temp dir")
    }

    #[test]
    fn test_atomic_write_basic() {
        let dir = make_test_dir();
        let path = dir.path().join("test.json");
        atomic_write(&path, r#"{"hello": "world"}"#).expect("write failed");
        let content = fs::read_to_string(&path).expect("read failed");
        assert!(content.contains("hello"));
    }

    #[test]
    fn test_atomic_write_no_tmp_left() {
        let dir = make_test_dir();
        let path = dir.path().join("test.json");
        atomic_write(&path, r#"{"v": 1}"#).expect("write failed");
        let leftovers = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().contains(".tmp."))
            .collect::<Vec<_>>();
        assert!(
            leftovers.is_empty(),
            "temp file should be gone after rename"
        );
    }

    #[test]
    fn test_session_artifact_path_rejects_invalid_ids_and_suffixes() {
        let dir = make_test_dir();
        assert!(session_artifact_path(dir.path(), "safe_session", "json").is_ok());
        assert!(session_artifact_path(dir.path(), "../outside", "json").is_err());
        assert!(session_artifact_path(dir.path(), "safe_session", "../escape").is_err());
        assert!(session_directory_path(dir.path(), "safe_session").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn test_session_artifact_path_rejects_symlinked_storage() {
        let dir = make_test_dir();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(session_artifact_path(&link, "safe_session", "json").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_session_artifact_path_rejects_symlinked_targets() {
        let dir = make_test_dir();
        let target_file = dir.path().join("target.json");
        let linked_file = dir.path().join("safe_session.json");
        let target_dir = dir.path().join("target-dir");
        let linked_dir = dir.path().join("safe_session");
        fs::write(&target_file, "outside").unwrap();
        fs::create_dir(&target_dir).unwrap();
        std::os::unix::fs::symlink(&target_file, &linked_file).unwrap();
        std::os::unix::fs::symlink(&target_dir, &linked_dir).unwrap();

        assert!(session_artifact_path(dir.path(), "safe_session", "json").is_err());
        assert!(session_directory_path(dir.path(), "safe_session").is_err());
    }

    #[test]
    fn test_session_id_rejects_path_components() {
        assert!(is_valid_session_id("project-1_client"));
        assert!(!is_valid_session_id("../outside"));
        assert!(!is_valid_session_id("session/child"));
        assert!(!is_valid_session_id(""));
    }

    #[test]
    fn test_archive_rolls_back_partial_move() {
        let dir = make_test_dir();
        let sessions_dir = dir.path().join("sessions");
        let archive = sessions_dir.join("archive");
        ensure_dir(&sessions_dir).unwrap();
        fs::write(sessions_dir.join("rollback-1.json"), "snapshot").unwrap();
        fs::write(sessions_dir.join("rollback-1.jsonl"), "transcript").unwrap();
        ensure_dir(&archive).unwrap();
        fs::write(archive.join("rollback-1.jsonl"), "existing").unwrap();

        assert!(archive_session_artifacts(&sessions_dir, &archive, "rollback-1").is_err());
        assert!(sessions_dir.join("rollback-1.json").exists());
        assert_eq!(
            fs::read_to_string(archive.join("rollback-1.jsonl")).unwrap(),
            "existing"
        );
        assert!(!archive.join("rollback-1.json").exists());
    }

    #[test]
    fn test_legacy_snapshot_defaults_schema_version() {
        let dir = make_test_dir();
        let sessions_dir = dir.path().join("sessions");
        ensure_dir(&sessions_dir).unwrap();
        let legacy = serde_json::json!({
            "session_id": "legacy-1",
            "cwd": "/tmp",
            "model": "m",
            "created_at": "2025-01-01T00:00:00Z",
            "last_active": "2025-01-01T00:00:00Z",
            "messages": [],
            "memory_summary": null
        });
        fs::write(
            sessions_dir.join("legacy-1.json"),
            serde_json::to_string(&legacy).unwrap(),
        )
        .unwrap();

        let loaded = load_session_state(&sessions_dir, "legacy-1").unwrap();
        assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn test_migrate_legacy_session_copies_artifacts_and_retains_source() {
        let dir = make_test_dir();
        let sessions_dir = dir.path().join("sessions");
        let state = PersistedSession {
            schema_version: CURRENT_SCHEMA_VERSION,
            session_id: "legacy-session".to_string(),
            cwd: "/tmp/project".to_string(),
            model: "m".to_string(),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            last_active: "2025-01-01T00:00:00Z".to_string(),
            messages: Vec::new(),
            memory_summary: None,
        };
        persist_session_state(&sessions_dir, &state).unwrap();
        for suffix in ["jsonl", "memory.md", "baseline.json"] {
            fs::write(
                sessions_dir.join(format!("legacy-session.{}", suffix)),
                suffix,
            )
            .unwrap();
        }
        ensure_dir(&sessions_dir.join("legacy-session")).unwrap();
        fs::write(
            sessions_dir.join("legacy-session").join("result.txt"),
            "tool result",
        )
        .unwrap();

        assert!(migrate_legacy_session(
            &sessions_dir,
            "legacy-session",
            "new-session",
            "/tmp/project"
        )
        .unwrap());
        assert_eq!(
            load_session_state(&sessions_dir, "new-session")
                .unwrap()
                .cwd,
            "/tmp/project"
        );
        assert_eq!(
            fs::read_to_string(sessions_dir.join("new-session.jsonl")).unwrap(),
            "jsonl"
        );
        assert_eq!(
            fs::read_to_string(sessions_dir.join("new-session").join("result.txt")).unwrap(),
            "tool result"
        );
        assert!(sessions_dir.join("legacy-session.json").exists());
        let registry = load_registry(&sessions_dir);
        assert!(registry
            .sessions
            .iter()
            .all(|entry| entry.session_id != "legacy-session"));
        assert!(registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "new-session"));
    }

    #[cfg(unix)]
    #[test]
    fn test_migrate_legacy_session_rejects_nested_symlink_before_writing_target() {
        let dir = make_test_dir();
        let sessions_dir = dir.path().join("sessions");
        let state = PersistedSession {
            schema_version: CURRENT_SCHEMA_VERSION,
            session_id: "legacy-session".to_string(),
            cwd: "/tmp/project".to_string(),
            model: "m".to_string(),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            last_active: "2025-01-01T00:00:00Z".to_string(),
            messages: Vec::new(),
            memory_summary: None,
        };
        persist_session_state(&sessions_dir, &state).unwrap();
        ensure_dir(&sessions_dir.join("legacy-session")).unwrap();
        let outside = dir.path().join("outside");
        fs::write(&outside, "outside").unwrap();
        std::os::unix::fs::symlink(
            &outside,
            sessions_dir.join("legacy-session").join("linked.txt"),
        )
        .unwrap();

        assert!(migrate_legacy_session(
            &sessions_dir,
            "legacy-session",
            "new-session",
            "/tmp/project"
        )
        .is_err());
        assert!(!sessions_dir.join("new-session.json").exists());
    }

    #[test]
    fn test_concurrent_persist_preserves_registry() {
        let dir = make_test_dir();
        let sessions_dir = std::sync::Arc::new(dir.path().join("sessions"));
        let mut workers = Vec::new();
        for index in 0..20 {
            let sessions_dir = std::sync::Arc::clone(&sessions_dir);
            workers.push(std::thread::spawn(move || {
                let state = PersistedSession {
                    schema_version: CURRENT_SCHEMA_VERSION,
                    session_id: format!("concurrent-{}", index),
                    cwd: "/tmp".to_string(),
                    model: "m".to_string(),
                    created_at: "2025-01-01T00:00:00Z".to_string(),
                    last_active: "2025-01-01T00:00:00Z".to_string(),
                    messages: Vec::new(),
                    memory_summary: None,
                };
                persist_session_state(&sessions_dir, &state).unwrap();
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }

        let index = load_registry(&sessions_dir);
        assert_eq!(index.sessions.len(), 20);
    }

    #[test]
    fn test_corrupt_registry_is_preserved_and_rebuilt() {
        let dir = make_test_dir();
        let sessions_dir = dir.path().join("sessions");
        let state = PersistedSession {
            schema_version: CURRENT_SCHEMA_VERSION,
            session_id: "recoverable-1".to_string(),
            cwd: "/tmp/project".to_string(),
            model: "m".to_string(),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            last_active: "2025-01-01T00:00:00Z".to_string(),
            messages: Vec::new(),
            memory_summary: None,
        };
        persist_session_state(&sessions_dir, &state).unwrap();
        fs::write(sessions_dir.join("registry.json"), "not-json").unwrap();

        let rebuilt = load_registry(&sessions_dir);

        assert_eq!(rebuilt.sessions.len(), 1);
        assert_eq!(rebuilt.sessions[0].session_id, "recoverable-1");
        assert!(fs::read_dir(&sessions_dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("registry.json.corrupt.")));
    }

    #[cfg(unix)]
    #[test]
    fn test_atomic_write_uses_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = make_test_dir();
        let sessions_dir = dir.path().join("sessions");
        ensure_dir(&sessions_dir).unwrap();
        let path = sessions_dir.join("private.json");
        atomic_write(&path, "private").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&sessions_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn test_persist_and_load_session() {
        let dir = make_test_dir();
        let sessions_dir = dir.path().join("sessions");

        let state = PersistedSession {
            schema_version: CURRENT_SCHEMA_VERSION,
            session_id: "test-123".to_string(),
            cwd: "/tmp/project".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            last_active: "2025-01-02T00:00:00Z".to_string(),
            messages: Vec::new(),
            memory_summary: Some("Test summary".to_string()),
        };

        persist_session_state(&sessions_dir, &state).expect("persist failed");

        // Verify file exists
        let file = sessions_dir.join("test-123.json");
        assert!(file.exists());

        // Verify registry exists
        let reg = sessions_dir.join("registry.json");
        assert!(reg.exists());

        // Load back
        let loaded = load_session_state(&sessions_dir, "test-123").expect("load failed");
        assert_eq!(loaded.session_id, "test-123");
        assert_eq!(loaded.model, "claude-sonnet-4-20250514");
        assert_eq!(loaded.memory_summary.as_deref(), Some("Test summary"));
    }

    #[test]
    fn test_load_missing_session() {
        let dir = make_test_dir();
        let sessions_dir = dir.path().join("sessions");
        ensure_dir(&sessions_dir).unwrap();
        let loaded = load_session_state(&sessions_dir, "nonexistent");
        assert!(loaded.is_none());
    }

    #[test]
    fn test_registry_upsert_and_update() {
        let dir = make_test_dir();
        let sessions_dir = dir.path().join("sessions");

        // First insert
        upsert_registry_entry(
            &sessions_dir,
            "s1",
            "/a",
            "2025-01-01T00:00:00Z",
            "2025-01-01T00:00:00Z",
        )
        .unwrap();
        let idx = load_registry(&sessions_dir);
        assert_eq!(idx.sessions.len(), 1);

        // Second insert
        upsert_registry_entry(
            &sessions_dir,
            "s2",
            "/b",
            "2025-01-02T00:00:00Z",
            "2025-01-02T00:00:00Z",
        )
        .unwrap();
        let idx = load_registry(&sessions_dir);
        assert_eq!(idx.sessions.len(), 2);

        // Update existing (should not add new)
        upsert_registry_entry(
            &sessions_dir,
            "s1",
            "/a",
            "2025-01-01T00:00:00Z",
            "2025-01-03T00:00:00Z",
        )
        .unwrap();
        let idx = load_registry(&sessions_dir);
        assert_eq!(idx.sessions.len(), 2);
        let s1 = idx.sessions.iter().find(|e| e.session_id == "s1").unwrap();
        assert_eq!(s1.last_active, "2025-01-03T00:00:00Z");
    }

    #[test]
    fn test_delete_session() {
        let dir = make_test_dir();
        let sessions_dir = dir.path().join("sessions");

        let state = PersistedSession {
            schema_version: CURRENT_SCHEMA_VERSION,
            session_id: "del-1".to_string(),
            cwd: "/tmp".to_string(),
            model: "m".to_string(),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            last_active: "2025-01-01T00:00:00Z".to_string(),
            messages: Vec::new(),
            memory_summary: None,
        };
        persist_session_state(&sessions_dir, &state).unwrap();

        // Verify exists
        assert!(load_session_state(&sessions_dir, "del-1").is_some());

        // Delete
        delete_session(&sessions_dir, "del-1").unwrap();
        assert!(load_session_state(&sessions_dir, "del-1").is_none());

        // Registry should be empty
        let idx = load_registry(&sessions_dir);
        assert!(idx.sessions.is_empty());
    }

    #[test]
    fn test_archive_stale() {
        let dir = make_test_dir();
        let sessions_dir = dir.path().join("sessions");

        // Create an old session (30 days ago)
        let old_state = PersistedSession {
            schema_version: CURRENT_SCHEMA_VERSION,
            session_id: "old-1".to_string(),
            cwd: "/tmp".to_string(),
            model: "m".to_string(),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            last_active: "2025-01-01T00:00:00Z".to_string(),
            messages: Vec::new(),
            memory_summary: None,
        };
        persist_session_state(&sessions_dir, &old_state).unwrap();
        for suffix in ["jsonl", "memory.md", "baseline.json"] {
            fs::write(
                sessions_dir.join(format!("old-1.{}", suffix)),
                "related session data",
            )
            .unwrap();
        }

        // Create a recent session (now)
        let now_iso = Utc::now().to_rfc3339();
        let recent_state = PersistedSession {
            schema_version: CURRENT_SCHEMA_VERSION,
            session_id: "recent-1".to_string(),
            cwd: "/tmp".to_string(),
            model: "m".to_string(),
            created_at: now_iso.clone(),
            last_active: now_iso,
            messages: Vec::new(),
            memory_summary: None,
        };
        persist_session_state(&sessions_dir, &recent_state).unwrap();

        // Archive with 7-day threshold
        let archived = archive_stale_sessions(&sessions_dir, 7).unwrap();
        assert_eq!(archived, vec!["old-1".to_string()]);

        // old-1 should be moved to archive/
        let archive_path = sessions_dir.join("archive").join("old-1.json");
        assert!(archive_path.exists());
        assert!(sessions_dir.join("archive").join("old-1.jsonl").exists());

        // old-1 should no longer be in main dir
        assert!(load_session_state(&sessions_dir, "old-1").is_none());

        // recent-1 should still be in main dir
        assert!(load_session_state(&sessions_dir, "recent-1").is_some());

        // Registry should only have recent-1
        let idx = load_registry(&sessions_dir);
        assert_eq!(idx.sessions.len(), 1);
        assert_eq!(idx.sessions[0].session_id, "recent-1");
    }

    #[test]
    fn test_overwrite_persist() {
        let dir = make_test_dir();
        let sessions_dir = dir.path().join("sessions");

        // Write v1
        let mut state = PersistedSession {
            schema_version: CURRENT_SCHEMA_VERSION,
            session_id: "s".to_string(),
            cwd: "/tmp".to_string(),
            model: "m1".to_string(),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            last_active: "2025-01-01T00:00:00Z".to_string(),
            messages: Vec::new(),
            memory_summary: None,
        };
        persist_session_state(&sessions_dir, &state).unwrap();

        // Write v2 (overwrite)
        state.model = "m2".to_string();
        state.last_active = "2025-01-02T00:00:00Z".to_string();
        persist_session_state(&sessions_dir, &state).unwrap();

        // Should have only 1 file, 1 registry entry
        let loaded = load_session_state(&sessions_dir, "s").unwrap();
        assert_eq!(loaded.model, "m2");

        let idx = load_registry(&sessions_dir);
        assert_eq!(idx.sessions.len(), 1);
    }
}
