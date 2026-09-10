//! Session-level rolling summary ("meeting notes").
//!
//! Maintained across the lifetime of a session and persisted to
//! `~/.baoclaw/sessions/{session_id}.memory.md`.
//!
//! Inspired by Claude Code's session memory mechanism:
//! - Updated every N turns via background API call during the session
//! - Loaded instantly at startup (no on-demand summarization)
//! - Used by session_memory_compact for zero-cost compaction

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::engine::security::validate_memory_content;

/// Minimum turns before first summary generation.
const FIRST_UPDATE_THRESHOLD: usize = 6;

/// Number of new messages between summary updates.
pub(crate) const UPDATE_INTERVAL: usize = 10;

/// Maximum summary length (chars).  Summaries exceeding this are truncated.
const MAX_SUMMARY_CHARS: usize = 8000;

/// Session-level rolling summary persisted to disk.
///
/// All mutable state is behind `Mutex` so that `&SessionMemory` is enough
/// for both reads and writes — safe to share behind `Arc<SessionMemory>`.
pub struct SessionMemory {
    file_path: PathBuf,
    content: Mutex<String>,
    last_update_count: Mutex<usize>,
}

impl SessionMemory {
    /// Load an existing session memory file. Returns empty string if missing.
    pub fn load(session_id: &str) -> Self {
        Self::load_in(
            &crate::engine::session_persistence::default_sessions_dir(),
            session_id,
        )
    }

    /// Directory-injectable variant of [`load`] (test seam) — tests must
    /// never touch the real `~/.baoclaw/sessions/` directory.
    pub fn load_in(sessions_dir: &std::path::Path, session_id: &str) -> Self {
        let file_path = crate::engine::session_persistence::session_artifact_path(
            sessions_dir,
            session_id,
            "memory.md",
        )
        .unwrap_or_default();
        let parent = match file_path.parent() {
            Some(parent) if !file_path.as_os_str().is_empty() => parent,
            _ => {
                eprintln!("[session-memory] WARNING: refusing persistence for invalid session ID");
                return Self {
                    file_path,
                    content: Mutex::new(String::new()),
                    last_update_count: Mutex::new(0),
                };
            }
        };
        if let Err(error) = crate::engine::session_persistence::ensure_session_storage_dir(parent) {
            eprintln!(
                "[session-memory] WARNING: failed to create {}: {}",
                parent.display(),
                error
            );
        }
        let content = match fs::read_to_string(&file_path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                eprintln!(
                    "[session-memory] WARNING: failed to read {}: {}",
                    file_path.display(),
                    error
                );
                String::new()
            }
        };
        #[cfg(unix)]
        if file_path.exists() {
            use std::os::unix::fs::PermissionsExt;
            if let Err(error) = fs::set_permissions(&file_path, fs::Permissions::from_mode(0o600)) {
                eprintln!(
                    "[session-memory] WARNING: failed to secure {}: {}",
                    file_path.display(),
                    error
                );
            }
        }

        Self {
            file_path,
            content: Mutex::new(content),
            last_update_count: Mutex::new(0),
        }
    }

    /// Return the current summary text.
    pub fn get(&self) -> String {
        self.content
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Whether a non-trivial summary is available.
    pub fn is_available(&self) -> bool {
        let guard = self.content.lock().unwrap_or_else(|e| e.into_inner());
        let trimmed = guard.trim();
        !trimmed.is_empty() && trimmed.len() > 20
    }

    /// Whether enough new messages have arrived to warrant an update.
    ///
    /// Self-healing: compaction shrinks the message vector while this
    /// baseline stays high, which used to make the condition permanently
    /// unsatisfiable (the summary went stale for the rest of the session).
    /// When the history is smaller than the recorded baseline, the baseline
    /// re-anchors to the new size so the next interval fires normally.
    pub fn should_update(&self, message_count: usize) -> bool {
        let guard = self.content.lock().unwrap_or_else(|e| e.into_inner());
        let current = guard.trim();
        if current.is_empty() || current.len() <= 20 {
            // No real summary yet — update after first few messages.
            drop(guard);
            message_count >= FIRST_UPDATE_THRESHOLD
        } else {
            let mut last = self
                .last_update_count
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if message_count < *last {
                *last = message_count;
            }
            drop(guard);
            message_count >= *last + UPDATE_INTERVAL
        }
    }

    /// Write a new summary to memory (and persist to disk).
    pub fn update(&self, summary: String) {
        // Security scan before persisting memory
        if let Err(reason) = validate_memory_content(&summary) {
            eprintln!("Memory content blocked by security scan: {}", reason);
            return;
        }

        let truncated = if summary.len() > MAX_SUMMARY_CHARS {
            format!(
                "{}...\n\n[Summary truncated at {} chars]",
                summary.chars().take(MAX_SUMMARY_CHARS).collect::<String>(),
                summary.len()
            )
        } else {
            summary
        };
        let mut guard = self.content.lock().unwrap_or_else(|e| e.into_inner());
        *guard = truncated;
        if self.file_path.as_os_str().is_empty() {
            return;
        }
        if let Err(error) =
            crate::engine::session_persistence::atomic_write(&self.file_path, &guard)
        {
            eprintln!(
                "[session-memory] WARNING: failed to persist {}: {}",
                self.file_path.display(),
                error
            );
        }
    }

    /// Record the current message count so `should_update` can track deltas.
    pub fn set_message_count(&self, count: usize) {
        let mut guard = self
            .last_update_count
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = count;
    }

    /// Messages since the last summary update, for freshness hints.
    /// `None` when freshness is unknown — the baseline is in-memory only and
    /// resets to 0 on process load, so a restored session cannot say how old
    /// its carried-over summary is.
    pub fn messages_since_update(&self, current_count: usize) -> Option<usize> {
        let last = *self
            .last_update_count
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if last == 0 || current_count < last {
            None
        } else {
            Some(current_count - last)
        }
    }

    /// Clear the session memory.
    pub fn clear(&self) {
        let mut guard = self.content.lock().unwrap_or_else(|e| e.into_inner());
        guard.clear();
        if self.file_path.as_os_str().is_empty() {
            return;
        }
        if let Err(error) = crate::engine::session_persistence::atomic_write(&self.file_path, "") {
            eprintln!(
                "[session-memory] WARNING: failed to clear {}: {}",
                self.file_path.display(),
                error
            );
        }
    }

    /// Path to the backing file (for diagnostics).
    pub fn file_path(&self) -> &PathBuf {
        &self.file_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_update_empty() {
        let dir = tempfile::tempdir().unwrap();
        let sm = SessionMemory::load_in(dir.path(), "unit-empty");
        assert!(!sm.should_update(5));
        assert!(sm.should_update(6));
    }

    #[test]
    fn test_update_and_get() {
        let dir = tempfile::tempdir().unwrap();
        let sm = SessionMemory::load_in(dir.path(), "unit-update");
        let summary = "# Session Memory\n- Did stuff".to_string();
        sm.update(summary.clone());
        assert_eq!(sm.get(), summary);
        sm.clear();
        assert!(sm.get().is_empty());
    }

    #[test]
    fn test_should_update_rebases_after_history_shrinks() {
        let dir = tempfile::tempdir().unwrap();
        let sm = SessionMemory::load_in(dir.path(), "unit-rebase");
        sm.update("# Memory\nA real summary with plenty of content.".to_string());
        sm.set_message_count(50);
        // History shrank to 11 (compaction). Before the rebase fix this
        // condition was unsatisfiable for the rest of the session.
        assert!(!sm.should_update(11));
        // Next interval fires UPDATE_INTERVAL messages after the new size.
        assert!(sm.should_update(21));
    }

    #[test]
    fn test_messages_since_update() {
        let dir = tempfile::tempdir().unwrap();
        let sm = SessionMemory::load_in(dir.path(), "unit-since");
        sm.update("# Memory\nA real summary with plenty of content.".to_string());
        // Baseline resets to 0 on load → freshness unknown, not "0 old".
        assert_eq!(sm.messages_since_update(30), None);
        sm.set_message_count(20);
        assert_eq!(sm.messages_since_update(30), Some(10));
        // History shrank below the baseline → unknown again.
        assert_eq!(sm.messages_since_update(5), None);
    }

    #[test]
    fn test_is_available() {
        let dir = tempfile::tempdir().unwrap();
        let sm = SessionMemory::load_in(dir.path(), "unit-available");
        sm.update("# Memory\nThis is a real summary with enough content.".to_string());
        assert!(sm.is_available());
        sm.update("".to_string());
        assert!(!sm.is_available());
    }

    #[test]
    fn test_set_message_count() {
        let dir = tempfile::tempdir().unwrap();
        let sm = SessionMemory::load_in(dir.path(), "unit-count");
        sm.update(
            "# Memory\nThis is a real summary with enough content to pass the threshold."
                .to_string(),
        );
        sm.set_message_count(10);
        assert!(!sm.should_update(19));
        assert!(sm.should_update(20));
    }

    #[test]
    fn test_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let sm = SessionMemory::load_in(dir.path(), "unit-truncation");
        let long = "X".repeat(10_000);
        sm.update(long.clone());
        assert!(sm.get().len() < long.len());
        assert!(sm.get().contains("[Summary truncated"));
    }

    #[test]
    fn test_invalid_session_id_disables_persistence_without_fallback_path() {
        let dir = tempfile::tempdir().unwrap();
        let sm = SessionMemory::load_in(dir.path(), "../invalid-session");

        assert!(sm.file_path().as_os_str().is_empty());
        sm.update("summary that remains in memory only".to_string());
        assert_eq!(sm.get(), "summary that remains in memory only");
    }
}
