use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use chrono::Utc;

use super::query_engine::{EngineEvent, QueryEngine};
use super::session_persistence::{self, PersistedSession};

/// Unique identifier for a client connection within a shared session.
pub type ClientId = u64;

/// A shared session wrapping a QueryEngine for multi-client access.
///
/// Provides concurrency control via an ActiveSubmitter lock and
/// event broadcasting to all connected clients.
pub struct SharedSession {
    /// The shared QueryEngine, protected by RwLock for multi-read / single-write.
    engine: Arc<RwLock<QueryEngine>>,
    /// The currently active message submitter (at most one at a time).
    active_submitter: Mutex<Option<ClientId>>,
    /// Broadcast sender for engine events.
    event_tx: broadcast::Sender<EngineEvent>,
    /// Set of currently connected client IDs.
    connected_clients: Mutex<HashSet<ClientId>>,
    /// Monotonic counter for generating unique client IDs.
    next_client_id: AtomicU64,
    /// Session creation timestamp (ISO 8601 / RFC 3339)
    created_at: String,
    /// Session last active timestamp (ISO 8601 / RFC 3339)
    last_active: Mutex<String>,
}

impl SharedSession {
    /// Create a new SharedSession wrapping the given QueryEngine.
    pub fn new(engine: QueryEngine, broadcast_capacity: usize) -> Self {
        let now = Utc::now().to_rfc3339();
        Self::with_created_at(engine, broadcast_capacity, now)
    }

    /// Create a SharedSession with an explicit creation timestamp.
    pub fn with_created_at(
        engine: QueryEngine,
        broadcast_capacity: usize,
        created_at: String,
    ) -> Self {
        let now = Utc::now().to_rfc3339();
        let (event_tx, _) = broadcast::channel(broadcast_capacity);
        Self {
            engine: Arc::new(RwLock::new(engine)),
            active_submitter: Mutex::new(None),
            event_tx,
            connected_clients: Mutex::new(HashSet::new()),
            next_client_id: AtomicU64::new(1),
            created_at,
            last_active: Mutex::new(now),
        }
    }

    /// Return creation timestamp.
    pub fn created_at(&self) -> &str {
        &self.created_at
    }

    /// Return last active timestamp.
    pub async fn last_active(&self) -> String {
        self.last_active.lock().await.clone()
    }

    /// Touch last active timestamp.
    pub async fn touch_active(&self) {
        let mut la = self.last_active.lock().await;
        *la = Utc::now().to_rfc3339();
    }

    /// Register a new client. Returns the assigned ClientId and a broadcast receiver.
    pub async fn add_client(&self) -> (ClientId, broadcast::Receiver<EngineEvent>) {
        let id = self.next_client_id.fetch_add(1, Ordering::Relaxed);
        self.connected_clients.lock().await.insert(id);
        let rx = self.event_tx.subscribe();
        (id, rx)
    }

    /// Remove a client from the session.
    ///
    /// If the removed client held the ActiveSubmitter lock, it is automatically released.
    /// Returns `true` if this was the last connected client (session should be cleaned up).
    pub async fn remove_client(&self, client_id: ClientId) -> bool {
        self.detach_client(client_id).await.unwrap_or(true)
    }

    /// Detach a client and report whether it was the last connected client.
    /// Returns `None` when the client was not attached.
    pub(crate) async fn detach_client(&self, client_id: ClientId) -> Option<bool> {
        if !self.connected_clients.lock().await.remove(&client_id) {
            return None;
        }

        // Auto-release ActiveSubmitter if held by this client
        let mut submitter = self.active_submitter.lock().await;
        if *submitter == Some(client_id) {
            *submitter = None;
        }

        Some(self.connected_clients.lock().await.is_empty())
    }

    /// Try to acquire the ActiveSubmitter lock for the given client.
    ///
    /// Returns `true` if the lock was acquired, `false` if another client already holds it.
    pub async fn try_acquire_submitter(&self, client_id: ClientId) -> bool {
        let mut submitter = self.active_submitter.lock().await;
        if submitter.is_none() {
            *submitter = Some(client_id);
            true
        } else {
            false
        }
    }

    /// Release the ActiveSubmitter lock if held by the given client.
    pub async fn release_submitter(&self, client_id: ClientId) {
        let mut submitter = self.active_submitter.lock().await;
        if *submitter == Some(client_id) {
            *submitter = None;
        }
    }

    /// Acquire a read lock on the shared QueryEngine.
    pub async fn engine_read(&self) -> RwLockReadGuard<'_, QueryEngine> {
        self.engine.read().await
    }

    /// Acquire a write lock on the shared QueryEngine.
    pub async fn engine_write(&self) -> RwLockWriteGuard<'_, QueryEngine> {
        self.engine.write().await
    }

    /// Broadcast an event to all connected clients.
    ///
    /// Errors from lagged receivers are silently ignored.
    pub fn broadcast(&self, event: EngineEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Get the number of currently connected clients.
    pub async fn client_count(&self) -> usize {
        self.connected_clients.lock().await.len()
    }

    /// Check whether the given client is the current ActiveSubmitter.
    pub async fn is_active_submitter(&self, client_id: ClientId) -> bool {
        *self.active_submitter.lock().await == Some(client_id)
    }

    /// Check whether there is any active submitter.
    pub async fn has_active_submitter(&self) -> bool {
        self.active_submitter.lock().await.is_some()
    }
}

fn restore_persisted_state(
    engine: &mut QueryEngine,
    session_id: &str,
    persisted: PersistedSession,
) -> bool {
    if persisted.schema_version != session_persistence::CURRENT_SCHEMA_VERSION
        || persisted.session_id != session_id
        || chrono::DateTime::parse_from_rfc3339(&persisted.created_at).is_err()
        || chrono::DateTime::parse_from_rfc3339(&persisted.last_active).is_err()
    {
        eprintln!(
            "[session-registry] WARNING: rejected invalid snapshot for session {}",
            session_id
        );
        return false;
    }

    if persisted.cwd != engine.get_cwd().to_string_lossy() {
        eprintln!(
            "[session-registry] WARNING: rejected snapshot for session {}: cwd mismatch",
            session_id
        );
        return false;
    }

    let has_messages = !persisted.messages.is_empty();
    let has_summary = persisted
        .memory_summary
        .as_deref()
        .is_some_and(|summary| !summary.trim().is_empty());
    if has_messages {
        engine.set_messages(persisted.messages);
    }
    if let Some(summary) = persisted.memory_summary {
        if !summary.trim().is_empty() {
            engine.seed_session_memory(&summary);
        }
    }
    has_messages || has_summary
}

/// Global registry of shared sessions, keyed by session ID.
pub struct SessionRegistry {
    sessions: Mutex<HashMap<String, Arc<SharedSession>>>,
    lifecycle: Arc<Mutex<()>>,
    /// Directory for session persistence files (~/.baoclaw/sessions/).
    persistence_dir: PathBuf,
}

impl SessionRegistry {
    /// Create an empty registry with default persistence directory.
    pub fn new() -> Self {
        Self::with_persistence_dir(session_persistence::default_sessions_dir())
    }

    /// Create an empty registry with a custom persistence directory.
    /// Ensures the directory exists.
    pub fn with_persistence_dir(persistence_dir: PathBuf) -> Self {
        if let Err(e) = session_persistence::ensure_session_storage_dir(&persistence_dir) {
            eprintln!(
                "[session-registry] WARNING: failed to create persistence dir {:?}: {}",
                persistence_dir, e
            );
        }
        Self {
            sessions: Mutex::new(HashMap::new()),
            lifecycle: Arc::new(Mutex::new(())),
            persistence_dir,
        }
    }

    /// Get the persistence directory.
    pub fn persistence_dir(&self) -> &PathBuf {
        &self.persistence_dir
    }

    /// Look up or create a shared session.
    ///
    /// `config_factory` is called only when a new session needs to be created.
    /// A persisted snapshot is restored before a new session is returned.
    /// Returns `(session, is_new)` where `is_new` is `true` if a new session was created.
    pub async fn get_or_create(
        &self,
        session_id: &str,
        config_factory: impl FnOnce() -> QueryEngine,
    ) -> (Arc<SharedSession>, bool) {
        let (session, is_new, _) = self
            .get_or_create_with_restore(session_id, config_factory)
            .await;
        (session, is_new)
    }

    /// Look up or create a shared session and restore its snapshot before exposing it.
    ///
    /// The lifecycle lock remains held through restore, preventing another client
    /// from observing a newly created session with only partially restored state.
    pub async fn get_or_create_with_restore(
        &self,
        session_id: &str,
        config_factory: impl FnOnce() -> QueryEngine,
    ) -> (Arc<SharedSession>, bool, bool) {
        let _lifecycle_guard = self.lifecycle.lock().await;
        let mut sessions = self.sessions.lock().await;
        self.get_or_create_unlocked(&mut sessions, session_id, config_factory)
    }

    fn get_or_create_unlocked(
        &self,
        sessions: &mut HashMap<String, Arc<SharedSession>>,
        session_id: &str,
        config_factory: impl FnOnce() -> QueryEngine,
    ) -> (Arc<SharedSession>, bool, bool) {
        if let Some(existing) = sessions.get(session_id) {
            (Arc::clone(existing), false, false)
        } else {
            let mut engine = config_factory();
            let registry = session_persistence::load_registry(&self.persistence_dir);
            let created_at = registry
                .sessions
                .iter()
                .find(|e| e.session_id == session_id)
                .map(|e| e.created_at.clone());
            let resumed =
                session_persistence::load_session_state(&self.persistence_dir, session_id)
                    .map(|persisted| restore_persisted_state(&mut engine, session_id, persisted))
                    .unwrap_or(false);
            let session = match created_at {
                Some(created) => Arc::new(SharedSession::with_created_at(engine, 256, created)),
                None => Arc::new(SharedSession::new(engine, 256)),
            };
            sessions.insert(session_id.to_string(), Arc::clone(&session));
            (session, true, resumed)
        }
    }

    /// Atomically persist the current session, detach one client, and attach it
    /// to another session. The registry lifecycle lock covers the whole move.
    pub async fn switch_client(
        &self,
        current_id: &str,
        current_session: &Arc<SharedSession>,
        client_id: ClientId,
        target_id: &str,
        target_cwd: &Path,
        config_factory: impl FnOnce() -> QueryEngine,
    ) -> Result<
        (
            Arc<SharedSession>,
            ClientId,
            tokio::sync::broadcast::Receiver<EngineEvent>,
        ),
        String,
    > {
        if current_id == target_id {
            return Err("target session is already active".to_string());
        }

        let _lifecycle_guard = self.lifecycle.lock().await;
        if current_session.has_active_submitter().await {
            return Err(
                "session busy: cannot switch cwd while a message is being processed".to_string(),
            );
        }

        {
            let sessions = self.sessions.lock().await;
            if !sessions
                .get(current_id)
                .is_some_and(|session| Arc::ptr_eq(session, current_session))
            {
                return Err("current session is no longer attached".to_string());
            }
        }

        let mut sessions = self.sessions.lock().await;
        let (target_session, target_is_new, _) =
            self.get_or_create_unlocked(&mut sessions, target_id, config_factory);
        drop(sessions);
        if target_session.engine_read().await.get_cwd() != target_cwd {
            if target_is_new {
                self.sessions.lock().await.remove(target_id);
            }
            return Err("target session identity does not match its working directory".to_string());
        }

        if let Err(error) = self.persist_session(current_id).await {
            if target_is_new {
                self.sessions.lock().await.remove(target_id);
            }
            return Err(error);
        }

        let Some(no_clients) = current_session.detach_client(client_id).await else {
            if target_is_new {
                self.sessions.lock().await.remove(target_id);
            }
            return Err("current client is no longer attached".to_string());
        };

        let mut sessions = self.sessions.lock().await;
        if no_clients {
            sessions.remove(current_id);
        }
        let (new_client_id, broadcast_rx) = target_session.add_client().await;
        Ok((target_session, new_client_id, broadcast_rx))
    }

    /// Remove a session from the registry.
    pub async fn remove(&self, session_id: &str) {
        let _lifecycle_guard = self.lifecycle.lock().await;
        self.sessions.lock().await.remove(session_id);
    }

    /// Remove a session while the caller holds the last-client cleanup guard.
    pub async fn remove_after_last_client_cleanup(&self, session_id: &str) {
        self.sessions.lock().await.remove(session_id);
    }

    /// Serialize last-client cleanup with new session acquisition.
    pub async fn acquire_last_client_cleanup(
        &self,
        session_id: &str,
        session: &Arc<SharedSession>,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let guard = Arc::clone(&self.lifecycle).lock_owned().await;
        if session.client_count().await != 0 {
            return None;
        }
        let sessions = self.sessions.lock().await;
        if sessions
            .get(session_id)
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            Some(guard)
        } else {
            None
        }
    }

    /// Check whether a session exists in the registry.
    pub async fn contains(&self, session_id: &str) -> bool {
        self.sessions.lock().await.contains_key(session_id)
    }

    // ── Persistence Methods ──

    /// Persist a session's conversation state to disk (called after each turn).
    ///
    /// Serializes messages + metadata to `<session_id>.json` using atomic write.
    /// Also updates the registry index's `last_active` timestamp.
    ///
    /// Errors are logged but do not crash the daemon. Returns `Ok(())` on success.
    pub async fn persist_session(&self, session_id: &str) -> Result<(), String> {
        let sessions = self.sessions.lock().await;
        let session = match sessions.get(session_id) {
            Some(s) => Arc::clone(s),
            None => return Err(format!("session {} not found in registry", session_id)),
        };
        drop(sessions); // release lock before acquiring engine read lock

        let engine = session.engine_read().await;
        let messages = engine.get_messages().to_vec();
        let cwd = engine.get_cwd().to_string_lossy().to_string();
        let model = engine.get_model().to_string();
        let memory_summary = engine.get_session_memory().as_ref().map(|sm| sm.get());

        let now = Utc::now().to_rfc3339();
        // Use created_at from existing registry entry, or now for new sessions
        let registry = session_persistence::load_registry(&self.persistence_dir);
        let created_at = registry
            .sessions
            .iter()
            .find(|e| e.session_id == session_id)
            .map(|e| e.created_at.clone())
            .unwrap_or_else(|| now.clone());

        let state = PersistedSession {
            schema_version: session_persistence::CURRENT_SCHEMA_VERSION,
            session_id: session_id.to_string(),
            cwd,
            model,
            created_at: created_at.clone(),
            last_active: now,
            messages,
            memory_summary,
        };

        // Keep the read lock through the disk write so a concurrent turn cannot
        // publish a newer in-memory state before this snapshot is persisted.
        session_persistence::persist_session_state(&self.persistence_dir, &state).map_err(|e| {
            let msg = format!(
                "Cannot persist session {} because {}. To fix, retry persistence and inspect the sessions directory permissions and available space.",
                session_id, e
            );
            eprintln!("[session-registry] WARNING: {}", msg);
            msg
        })
    }

    /// Load persisted session data for a given session ID from disk.
    ///
    /// Returns `None` if the session has no persisted state or if the file is corrupted.
    /// This is used by the daemon to restore conversation history when a session is
    /// re-created after a crash or restart.
    pub fn load_persisted_session(&self, session_id: &str) -> Option<PersistedSession> {
        session_persistence::load_session_state(&self.persistence_dir, session_id)
    }

    /// Restore messages and memory into a session from persisted state (if available).
    ///
    /// This remains available for callers that explicitly manage restore timing;
    /// `get_or_create` restores new sessions before exposing them by default.
    /// Returns `true` if any persisted state was restored, `false` otherwise.
    pub async fn restore_session_messages(&self, session_id: &str) -> bool {
        let persisted = match self.load_persisted_session(session_id) {
            Some(p) => p,
            None => return false,
        };

        let session = {
            let sessions = self.sessions.lock().await;
            sessions.get(session_id).map(Arc::clone)
        };
        if let Some(session) = session {
            let mut engine = session.engine_write().await;
            restore_persisted_state(&mut engine, session_id, persisted)
        } else {
            false
        }
    }

    /// Archive sessions that have been inactive for more than `max_age_days`.
    ///
    /// Moves their JSON files to `archive/` subdirectory and removes them
    /// from the registry index. In-memory sessions are not evicted (use `remove()`
    /// for that).
    ///
    /// Returns the list of archived session IDs.
    pub fn archive_stale(&self, max_age_days: u64) -> Vec<String> {
        match session_persistence::archive_stale_sessions(&self.persistence_dir, max_age_days) {
            Ok(archived) => {
                if !archived.is_empty() {
                    eprintln!(
                        "[session-registry] Archived {} stale sessions (>{} days inactive)",
                        archived.len(),
                        max_age_days
                    );
                }
                archived
            }
            Err(e) => {
                eprintln!("[session-registry] WARNING: archive_stale failed: {}", e);
                Vec::new()
            }
        }
    }

    /// Archive sessions with the default 7-day threshold.
    pub fn archive_stale_default(&self) -> Vec<String> {
        self.archive_stale(7)
    }

    /// Get all session IDs from the registry index (on disk).
    pub fn list_persisted_sessions(&self) -> Vec<(String, String)> {
        let index = session_persistence::load_registry(&self.persistence_dir);
        index
            .sessions
            .iter()
            .map(|e| (e.session_id.clone(), e.cwd.clone()))
            .collect()
    }

    /// Delete a session's persisted state from disk.
    pub fn delete_persisted_session(&self, session_id: &str) {
        if let Err(e) = session_persistence::delete_session(&self.persistence_dir, session_id) {
            eprintln!(
                "[session-registry] WARNING: failed to delete persisted session {}: {}",
                session_id, e
            );
        }
    }

    /// Persist all currently in-memory sessions to disk.
    /// Useful for graceful shutdown (SIGTERM/SIGINT handlers).
    pub async fn persist_all(&self) {
        let session_ids: Vec<String> = {
            let sessions = self.sessions.lock().await;
            sessions.keys().cloned().collect()
        };

        let results = futures::future::join_all(
            session_ids
                .iter()
                .map(|sid| async move { (sid, self.persist_session(sid).await) }),
        )
        .await;

        for (sid, res) in results {
            if let Err(e) = res {
                eprintln!(
                    "[session-registry] WARNING: persist_all failed for {}: {}",
                    sid, e
                );
            }
        }
        eprintln!(
            "[session-registry] Persisted {} sessions to disk",
            session_ids.len()
        );
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::client::ApiClientConfig;
    use crate::api::unified::UnifiedClient;
    use crate::engine::query_engine::ThinkingConfig;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_engine(cwd: PathBuf, session_id: &str) -> QueryEngine {
        QueryEngine::new(crate::engine::query_engine::QueryEngineConfig {
            cwd,
            tools: vec![],
            api_client: Arc::new(UnifiedClient::new_anthropic(ApiClientConfig {
                api_key: "test-key".to_string(),
                base_url: None,
                max_retries: None,
                api_path: None,
            })),
            model: "test-model".to_string(),
            thinking_config: ThinkingConfig::Disabled,
            max_turns: None,
            max_budget_usd: None,
            verbose: false,
            custom_system_prompt: None,
            append_system_prompt: None,
            session_id: Some(session_id.to_string()),
            fallback_models: vec![],
            max_retries_per_model: 1,
            context_window: 200_000,
            auto_compact_threshold_ratio: 0.7,
            parent_turn_id: None,
            agent_label: None,
            session_memory: None,
            file_cache: None,
            tool_result_store: None,
            hook_manager: None,
            permission: None,
        })
    }

    fn make_registry() -> (TempDir, SessionRegistry, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let registry = SessionRegistry::with_persistence_dir(dir.path().join("sessions"));
        let current_cwd = dir.path().join("current");
        let target_cwd = dir.path().join("target");
        std::fs::create_dir_all(&current_cwd).unwrap();
        std::fs::create_dir_all(&target_cwd).unwrap();
        (dir, registry, current_cwd, target_cwd)
    }

    #[tokio::test]
    async fn switch_client_rejects_busy_session_without_detaching_client() {
        let (_dir, registry, current_cwd, target_cwd) = make_registry();
        let current = registry
            .get_or_create("current", || make_engine(current_cwd.clone(), "current"))
            .await
            .0;
        let (client_id, _rx) = current.add_client().await;
        assert!(current.try_acquire_submitter(client_id).await);

        let result = registry
            .switch_client(
                "current",
                &current,
                client_id,
                "target",
                &target_cwd,
                || make_engine(target_cwd.clone(), "target"),
            )
            .await;

        assert!(result.is_err());
        assert_eq!(current.client_count().await, 1);
        assert!(registry.contains("current").await);
        assert!(!registry.contains("target").await);
    }

    #[tokio::test]
    async fn switch_client_rejects_target_identity_mismatch_without_detaching_client() {
        let (_dir, registry, current_cwd, target_cwd) = make_registry();
        let current = registry
            .get_or_create("current", || make_engine(current_cwd.clone(), "current"))
            .await
            .0;
        let (client_id, _rx) = current.add_client().await;
        let (_other_client_id, _other_rx) = current.add_client().await;

        let result = registry
            .switch_client(
                "current",
                &current,
                client_id,
                "target",
                &target_cwd,
                || make_engine(current_cwd.clone(), "target"),
            )
            .await;

        assert!(result.is_err());
        assert_eq!(current.client_count().await, 2);
        assert!(registry.contains("current").await);
        assert!(!registry.contains("target").await);
    }

    #[tokio::test]
    async fn switch_client_persists_old_session_and_attaches_target_client() {
        let (_dir, registry, current_cwd, target_cwd) = make_registry();
        let current = registry
            .get_or_create("current", || make_engine(current_cwd.clone(), "current"))
            .await
            .0;
        let (client_id, _rx) = current.add_client().await;
        let (_other_client_id, _other_rx) = current.add_client().await;

        let (target, new_client_id, _rx) = registry
            .switch_client(
                "current",
                &current,
                client_id,
                "target",
                &target_cwd,
                || make_engine(target_cwd.clone(), "target"),
            )
            .await
            .unwrap();

        assert_eq!(current.client_count().await, 1);
        assert_eq!(target.client_count().await, 1);
        assert_eq!(new_client_id, 1);
        assert!(registry.contains("current").await);
        assert!(registry.contains("target").await);
        assert_eq!(
            registry.load_persisted_session("current").unwrap().cwd,
            current_cwd.to_string_lossy()
        );
    }

    #[tokio::test]
    async fn concurrent_switches_allow_only_one_client_transition() {
        let (_dir, registry, current_cwd, target_cwd) = make_registry();
        let current = registry
            .get_or_create("current", || make_engine(current_cwd.clone(), "current"))
            .await
            .0;
        let (client_id, _rx) = current.add_client().await;

        let first = registry.switch_client(
            "current",
            &current,
            client_id,
            "target",
            &target_cwd,
            || make_engine(target_cwd.clone(), "target"),
        );
        let second = registry.switch_client(
            "current",
            &current,
            client_id,
            "target",
            &target_cwd,
            || make_engine(target_cwd.clone(), "target"),
        );
        let (first_result, second_result) = tokio::join!(first, second);

        assert_eq!(first_result.is_ok() as u8 + second_result.is_ok() as u8, 1);
        assert!(!registry.contains("current").await);
        assert!(registry.contains("target").await);
        let target = registry
            .get_or_create("target", || make_engine(target_cwd, "target"))
            .await
            .0;
        assert_eq!(target.client_count().await, 1);
    }
    #[tokio::test]
    async fn persist_all_persists_multiple_sessions_in_parallel() {
        let (_dir, registry, current_cwd, target_cwd) = make_registry();
        let _s1 = registry
            .get_or_create("s1", || make_engine(current_cwd.clone(), "s1"))
            .await;
        let _s2 = registry
            .get_or_create("s2", || make_engine(target_cwd.clone(), "s2"))
            .await;

        registry.persist_all().await;

        assert!(registry.load_persisted_session("s1").is_some());
        assert!(registry.load_persisted_session("s2").is_some());
    }
}
