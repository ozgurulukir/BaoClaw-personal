//! Memory store implementation.
//!
//! Persistent memory store backed by a JSONL file.
//! Supports both global (~/.baoclaw/) and project-level (<cwd>/.baoclaw/) memory.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::Mutex;

use crate::engine::memory::apply_decay;
use crate::engine::memory::archive::{ArchiveResult, MemoryArchive};
use crate::engine::memory::decay::{boost_on_recall, days_since_anchor, decay_score, DecayConfig};
use crate::engine::security::validate_memory_content;

const MEMORY_FILE: &str = "memory.jsonl";

/// Errors that can occur during memory store operations.
#[derive(Debug)]
pub enum MemoryError {
    /// IO error (file read/write failure, permission denied, etc.)
    Io(std::io::Error),
    /// Serialization or deserialization error
    Serde(serde_json::Error),
    /// A corrupted entry was encountered during read.
    /// The entry is skipped but the error is surfaced for logging.
    Corrupted { line: usize, reason: String },
    /// Content rejected by the memory security scan (credential patterns,
    /// invisible unicode, prompt-injection phrasing).
    Rejected(String),
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::Serde(e) => write!(f, "Serialization error: {}", e),
            Self::Corrupted { line, reason } => {
                write!(f, "Corrupted entry at line {}: {}", line, reason)
            }
            Self::Rejected(reason) => write!(f, "{}", reason),
        }
    }
}

impl std::error::Error for MemoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Serde(e) => Some(e),
            Self::Corrupted { .. } | Self::Rejected(_) => None,
        }
    }
}

impl From<std::io::Error> for MemoryError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for MemoryError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serde(e)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryCategory {
    #[serde(rename = "fact")]
    Fact,
    #[serde(rename = "preference")]
    Preference,
    #[serde(rename = "decision")]
    Decision,
}

impl std::fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fact => write!(f, "fact"),
            Self::Preference => write!(f, "preference"),
            Self::Decision => write!(f, "decision"),
        }
    }
}

/// Default importance for new memories
const DEFAULT_IMPORTANCE: f64 = 0.5;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub category: MemoryCategory,
    pub created_at: String,
    pub source: String,
    /// Importance score (0.0-1.0), used for memory decay.
    /// Higher values indicate more important memories that should be retained longer.
    /// Default: 0.5
    #[serde(default = "default_importance")]
    pub importance: f64,
    /// Number of times this memory has been recalled.
    /// Increases each time the memory is accessed/referenced.
    #[serde(default)]
    pub recall_count: u32,
    /// ISO8601 timestamp of the last time this memory was recalled.
    /// Updated when the memory is referenced in a response.
    #[serde(default)]
    pub last_recalled_at: Option<String>,
    /// Whether this memory has been archived.
    /// Archived memories are moved to a separate storage file.
    #[serde(default)]
    pub archived: bool,
}

fn default_importance() -> f64 {
    DEFAULT_IMPORTANCE
}

/// Outcome of a store add: the stored entry plus whether this call created
/// it. An exact-content duplicate returns the existing entry unmodified
/// (`created: false`) instead of accumulating an identical line.
#[derive(Debug, Clone)]
pub struct AddOutcome {
    pub entry: MemoryEntry,
    pub created: bool,
}

/// Persistent memory store backed by a JSONL file.
/// Supports both global (~/.baoclaw/) and project-level (<cwd>/.baoclaw/) memory.
pub struct MemoryStore {
    entries: Mutex<Vec<MemoryEntry>>,
    file_path: Mutex<PathBuf>,
    /// Serializes file mutations (appends and whole-file rewrites) against
    /// each other. Without it, a rewrite's entries snapshot could clobber a
    /// concurrently appended entry on disk (in-memory state would heal it
    /// only at the next rewrite — or lose it at process exit).
    persist: Mutex<()>,
}

impl MemoryStore {
    /// Load global memories from ~/.baoclaw/memory.jsonl.
    pub fn load() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let file_path = PathBuf::from(&home).join(".baoclaw").join(MEMORY_FILE);
        Self::load_with_path(file_path)
    }

    /// Load memories from an explicit file path (test seam) — tests must
    /// never touch the user's real `~/.baoclaw/memory.jsonl`.
    pub fn load_with_path(file_path: PathBuf) -> Self {
        let entries = Self::read_file(&file_path);
        eprintln!(
            "Loaded {} long-term memories from {}",
            entries.len(),
            file_path.display()
        );
        Self {
            entries: Mutex::new(entries),
            file_path: Mutex::new(file_path),
            persist: Mutex::new(()),
        }
    }

    /// Load project-level memories from <cwd>/.baoclaw/memory.jsonl.
    /// Falls back to global if project dir doesn't have .baoclaw/.
    pub fn load_for_project(cwd: &std::path::Path) -> Self {
        let project_path = cwd.join(".baoclaw").join(MEMORY_FILE);
        let file_path = if cwd.join(".baoclaw").exists() {
            project_path
        } else {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(&home).join(".baoclaw").join(MEMORY_FILE)
        };
        let entries = Self::read_file(&file_path);
        eprintln!(
            "Loaded {} project memories from {}",
            entries.len(),
            file_path.display()
        );
        Self {
            entries: Mutex::new(entries),
            file_path: Mutex::new(file_path),
            persist: Mutex::new(()),
        }
    }

    /// Switch to a different project's memory store.
    pub async fn switch_project(&self, cwd: &std::path::Path) {
        let new_path = if cwd.join(".baoclaw").exists() {
            cwd.join(".baoclaw").join(MEMORY_FILE)
        } else {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(&home).join(".baoclaw").join(MEMORY_FILE)
        };
        let new_entries = Self::read_file_async(&new_path).await;
        eprintln!(
            "Switched memory to {} ({} entries)",
            new_path.display(),
            new_entries.len()
        );
        *self.entries.lock().await = new_entries;
        *self.file_path.lock().await = new_path;
    }

    fn parse_entries(content: &str, path: &std::path::Path) -> Vec<MemoryEntry> {
        let mut entries = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (line_no, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<MemoryEntry>(line) {
                Ok(e) => {
                    // Order-preserving dedup: an exact-content duplicate (e.g.
                    // from the pre-store MemoryTool that appended blindly)
                    // renders twice in every prompt; keep the first occurrence.
                    if !seen.insert(e.content.clone()) {
                        eprintln!(
                            "WARNING: duplicate memory entry at line {} in {} skipped",
                            line_no,
                            path.display()
                        );
                        continue;
                    }
                    entries.push(e);
                }
                Err(e) => {
                    // Log corrupted line but don't abort reading (degraded mode: skip + warn)
                    eprintln!(
                        "WARNING: corrupted memory entry at line {} in {}: {}",
                        line_no,
                        path.display(),
                        e
                    );
                }
            }
        }
        entries
    }

    fn read_file(path: &std::path::Path) -> Vec<MemoryEntry> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Vec::new(), // File doesn't exist yet → empty Vec (not an error)
        };
        Self::parse_entries(&content, path)
    }

    async fn read_file_async(path: &std::path::Path) -> Vec<MemoryEntry> {
        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        Self::parse_entries(&content, path)
    }

    async fn write_all_async(
        path: &std::path::Path,
        entries: &[MemoryEntry],
    ) -> Result<(), MemoryError> {
        let lines: Vec<String> = entries
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?;
        let body = if lines.is_empty() {
            String::new()
        } else {
            lines.join("\n") + "\n"
        };
        // Write to a sibling temp file and rename: a crash mid-write must
        // never leave a truncated active file (the reader would degrade to
        // "empty memory" on the surviving partial line).
        let tmp = path.with_extension("jsonl.tmp");
        tokio::fs::write(&tmp, body.as_bytes()).await?;
        tokio::fs::rename(&tmp, path).await?;
        ensure_private_perms_async(path).await;
        Ok(())
    }

    /// Add a new memory entry.
    ///
    /// Returns the created entry on success, or `MemoryError` on write failure.
    /// The entry is always added to in-memory state; the error reflects a
    /// filesystem persistence failure.
    pub async fn add(
        &self,
        content: String,
        category: MemoryCategory,
        source: String,
    ) -> Result<AddOutcome, MemoryError> {
        self.add_with_importance(content, category, source, DEFAULT_IMPORTANCE)
            .await
    }

    /// Add a new memory entry with an explicit importance score.
    ///
    /// The write path enforces the store's invariants regardless of caller
    /// (model tool or user IPC): content must pass the memory security scan,
    /// and an exact-content duplicate is idempotent — the existing entry is
    /// returned with `created: false` instead of being re-appended.
    ///
    /// `importance` is clamped to `0.0..=1.0`; values above the default make a
    /// memory decay slower and rank higher in the budgeted prompt fragment.
    pub async fn add_with_importance(
        &self,
        content: String,
        category: MemoryCategory,
        source: String,
        importance: f64,
    ) -> Result<AddOutcome, MemoryError> {
        validate_memory_content(&content).map_err(MemoryError::Rejected)?;

        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
            content,
            category,
            created_at: chrono::Utc::now().to_rfc3339(),
            source,
            importance: importance.clamp(0.0, 1.0),
            recall_count: 0,
            last_recalled_at: None,
            archived: false,
        };

        // Lock order: persist → entries (same as every whole-file rewrite).
        let _persist = self.persist.lock().await;

        {
            let entries = self.entries.lock().await;
            if let Some(existing) = entries
                .iter()
                .filter(|e| !e.archived)
                .find(|e| e.content == entry.content)
            {
                return Ok(AddOutcome {
                    entry: existing.clone(),
                    created: false,
                });
            }
        }

        // Serialize before acquiring any locks
        let serialized_line = serde_json::to_string(&entry)?;

        // Phase 1: acquire entries lock, push, release
        {
            let mut entries = self.entries.lock().await;
            entries.push(entry.clone());
        }

        // Phase 2: acquire file_path lock, clone path, release (deadlock prevention:
        //          never hold entries lock while acquiring file_path lock)
        let fp = {
            let fp_guard = self.file_path.lock().await;
            fp_guard.clone()
        };

        // Phase 3: asynchronous append using tokio::fs
        let write_res = async {
            use tokio::io::AsyncWriteExt;
            let mut f = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&fp)
                .await?;
            f.write_all(format!("{}\n", serialized_line).as_bytes())
                .await?;
            f.flush().await?;
            ensure_private_perms_async(&fp).await;
            Ok::<(), MemoryError>(())
        }
        .await;

        match write_res {
            Ok(()) => Ok(AddOutcome {
                entry,
                created: true,
            }),
            Err(e) => {
                eprintln!("ERROR: memory write failed for entry {}: {}", entry.id, e);
                Err(e)
            }
        }
    }

    /// List all memories.
    pub async fn list(&self) -> Vec<MemoryEntry> {
        self.entries.lock().await.clone()
    }

    /// Delete a memory by ID prefix.
    ///
    /// Returns `Ok(true)` if a memory was deleted, `Ok(false)` if no match found.
    /// Returns `Err(MemoryError)` if the file rewrite fails.
    pub async fn delete(&self, id_prefix: &str) -> Result<bool, MemoryError> {
        // Lock order: persist → entries, so a delete's whole-file rewrite can
        // never interleave with an add's append (which would drop it).
        let _persist = self.persist.lock().await;
        let entries_snapshot;
        let mut entries = self.entries.lock().await;
        let before = entries.len();
        entries.retain(|e| !e.id.starts_with(id_prefix));
        if entries.len() < before {
            entries_snapshot = entries.clone();
            drop(entries);
            let fp = self.file_path.lock().await.clone();
            // Asynchronous whole-file atomic rewrite — entries lock already released
            if let Err(e) = Self::write_all_async(&fp, &entries_snapshot).await {
                eprintln!("ERROR: memory delete write failed: {}", e);
                return Err(e);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Clear all memories.
    ///
    /// Returns the number of cleared memories on success.
    /// Returns `Err(MemoryError)` if the file truncation fails.
    pub async fn clear(&self) -> Result<usize, MemoryError> {
        let _persist = self.persist.lock().await;
        let count = {
            let mut entries = self.entries.lock().await;
            let count = entries.len();
            entries.clear();
            count
        };
        // Drop entries lock, then do file I/O
        let fp = self.file_path.lock().await.clone();
        tokio::fs::write(&fp, b"").await?;
        ensure_private_perms_async(&fp).await;
        Ok(count)
    }

    /// Build the long-term memory system prompt fragment.
    ///
    /// Entries are ranked by decayed importance and rendered within a
    /// character budget (`DecayConfig::prompt_char_budget`) so the always-on
    /// prompt cost stays bounded as the store grows. Entries that don't fit
    /// stay reachable through the MemorySearch tool, which is pointed to in a
    /// trailing note. Returns None if no memories exist (or nothing fits).
    pub async fn build_prompt_fragment(&self) -> Option<String> {
        let config = DecayConfig::load();
        self.build_prompt_fragment_with(&config).await
    }

    /// Config-injectable variant of [`build_prompt_fragment`] (test seam).
    pub async fn build_prompt_fragment_with(&self, config: &DecayConfig) -> Option<String> {
        let entries = self.entries.lock().await;
        let candidates: Vec<&MemoryEntry> = entries
            .iter()
            .filter(|e| !e.archived && !e.content.trim().is_empty())
            .collect();
        if candidates.is_empty() {
            return None;
        }

        // Rank by decayed importance (highest first; stable on ties so file
        // order breaks them deterministically).
        let mut ranked: Vec<(&MemoryEntry, f64)> = candidates
            .iter()
            .map(|e| (*e, decayed_score(e, config)))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Greedy budget fill on the ranked order.
        let mut selected: Vec<&MemoryEntry> = Vec::new();
        let mut used = 0usize;
        for (entry, _) in &ranked {
            // Char (not byte) accounting so the budget means the same thing
            // for CJK content, which costs ~3 bytes per char.
            let line_len = entry.content.chars().count() + 2; // "- " prefix
            if used + line_len > config.prompt_char_budget {
                continue;
            }
            used += line_len;
            selected.push(entry);
        }
        if selected.is_empty() {
            return None;
        }
        let omitted = ranked.len() - selected.len();

        let mut parts = Vec::new();
        parts.push(format!(
            "# Long-term Memory [{}/{} memories · {}/{} chars]\n\n\
             The following are facts, preferences, and decisions remembered from previous conversations. \
             Use them to provide personalized responses.\n",
            selected.len(),
            ranked.len(),
            used,
            config.prompt_char_budget
        ));

        // Render selected entries grouped by category, in canonical order.
        for (category, title) in [
            (MemoryCategory::Fact, "## Facts"),
            (MemoryCategory::Preference, "## Preferences"),
            (MemoryCategory::Decision, "## Decisions"),
        ] {
            let group: Vec<&&MemoryEntry> =
                selected.iter().filter(|e| e.category == category).collect();
            if group.is_empty() {
                continue;
            }
            parts.push(title.to_string());
            for e in group {
                parts.push(format!("- {}", e.content));
            }
        }

        if omitted > 0 {
            parts.push(format!(
                "\n[{} lower-priority memories not shown — recall them with the MemorySearch tool.]",
                omitted
            ));
        }

        Some(parts.join("\n"))
    }

    /// Record a recall event for the given entry IDs.
    ///
    /// Applies the decay module's recall boost (importance bump, recall
    /// counter, last-recalled timestamp) and persists the updated entries.
    /// This is what keeps frequently-searched memories off the decay
    /// death-clock; without it the time-only decay archives everything
    /// purely by age.
    pub async fn record_recall(&self, ids: &[String], config: &DecayConfig) {
        // Persist lock held across mutation AND snapshot so the rewrite can
        // never clobber a concurrently appended entry.
        let snapshot = {
            let _persist = self.persist.lock().await;
            let mut entries = self.entries.lock().await;
            let mut changed = 0usize;
            for entry in entries.iter_mut() {
                if ids.iter().any(|id| entry.id.starts_with(id.as_str())) {
                    boost_on_recall(entry, config);
                    changed += 1;
                }
            }
            if changed == 0 {
                return;
            }
            entries.clone()
        };
        let fp = self.file_path.lock().await.clone();
        if let Err(e) = Self::write_all_async(&fp, &snapshot).await {
            eprintln!("ERROR: memory recall persist failed: {}", e);
        }
    }

    /// Archive low-importance memories.
    ///
    /// Uses the decay algorithm to identify memories below the archive threshold.
    /// Moves them to the archive and removes them from active memory.
    ///
    /// # Arguments
    /// * `archive` - The MemoryArchive instance to use
    /// * `config` - Decay configuration parameters
    ///
    /// # Returns
    /// ArchiveResult with IDs of archived memories and cleanup count.
    /// If the file rewrite fails, logs the error but still archives in-memory.
    pub async fn archive_low_importance(
        &self,
        archive: &MemoryArchive,
        config: &DecayConfig,
    ) -> ArchiveResult {
        let _persist = self.persist.lock().await;
        let (to_archive, entries_snapshot) = {
            let mut entries = self.entries.lock().await;

            // Apply decay and find memories to archive
            let to_archive_ids = apply_decay(&mut entries, config);

            if to_archive_ids.is_empty() {
                return ArchiveResult {
                    archived_ids: Vec::new(),
                    deleted_count: 0,
                };
            }

            // Collect memories to archive
            let to_archive: Vec<MemoryEntry> = entries
                .iter()
                .filter(|e| to_archive_ids.contains(&e.id))
                .cloned()
                .collect();

            // Remove from active memory
            entries.retain(|e| !to_archive_ids.contains(&e.id));
            (to_archive, entries.clone())
        };

        // Write updated memory file
        let fp = self.file_path.lock().await.clone();
        if let Err(e) = Self::write_all_async(&fp, &entries_snapshot).await {
            eprintln!(
                "ERROR: memory file rewrite during archive_low_importance failed: {}",
                e
            );
        }

        // Add to archive
        let result = archive.archive_memories(to_archive).await;

        eprintln!(
            "Archived {} low-importance memories",
            result.archived_ids.len()
        );

        result
    }

    /// Archive a specific memory by ID.
    ///
    /// Moves the memory to the archive regardless of its importance score.
    /// If the file rewrite fails, logs the error but still returns the archived entry.
    ///
    /// # Arguments
    /// * `id_prefix` - ID prefix of the memory to archive
    /// * `archive` - The MemoryArchive instance to use
    ///
    /// # Returns
    /// The archived memory entry, or None if not found
    pub async fn archive_by_id(
        &self,
        id_prefix: &str,
        archive: &MemoryArchive,
    ) -> Option<MemoryEntry> {
        let _persist = self.persist.lock().await;
        let (memory, entries_snapshot) = {
            let mut entries = self.entries.lock().await;

            // Find and remove the memory
            let pos = entries.iter().position(|e| e.id.starts_with(id_prefix))?;
            let memory = entries.remove(pos);
            (memory, entries.clone())
        };

        // Write updated memory file
        let fp = self.file_path.lock().await.clone();
        if let Err(e) = Self::write_all_async(&fp, &entries_snapshot).await {
            eprintln!(
                "ERROR: memory file rewrite during archive_by_id failed: {}",
                e
            );
        }

        // Add to archive
        let archived = archive.archive_memory(memory).await;

        eprintln!("Manually archived memory {}", id_prefix);
        Some(archived)
    }

    /// Restore a memory from the archive.
    ///
    /// Removes the memory from the archive and adds it back to active memory.
    /// Resets the importance to default (0.5) to prevent immediate re-archival.
    /// If the file write fails, logs the error but still returns the restored entry.
    ///
    /// # Arguments
    /// * `id_prefix` - ID prefix of the memory to restore
    /// * `archive` - The MemoryArchive instance to use
    ///
    /// # Returns
    /// The restored memory entry, or None if not found in archive
    pub async fn restore_from_archive(
        &self,
        id_prefix: &str,
        archive: &MemoryArchive,
    ) -> Option<MemoryEntry> {
        // Restore from archive
        let mut memory = archive.restore_memory(id_prefix).await?;

        // Reset importance to prevent immediate re-archival
        memory.importance = DEFAULT_IMPORTANCE;
        memory.archived = false;

        let line = match serde_json::to_string(&memory) {
            Ok(l) => l,
            Err(e) => {
                eprintln!(
                    "ERROR: failed to serialize restored memory {}: {}",
                    id_prefix, e
                );
                return Some(memory);
            }
        };

        let _persist = self.persist.lock().await;
        // Add back to active memory
        {
            let mut entries = self.entries.lock().await;
            entries.push(memory.clone());
        }

        let fp = self.file_path.lock().await.clone();
        use tokio::io::AsyncWriteExt;
        match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&fp)
            .await
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(format!("{}\n", line).as_bytes()).await {
                    eprintln!("ERROR: memory restore write failed: {}", e);
                } else {
                    let _ = f.flush().await;
                    ensure_private_perms_async(&fp).await;
                }
            }
            Err(e) => {
                eprintln!(
                    "ERROR: failed to open memory file for restore of {}: {}",
                    id_prefix, e
                );
            }
        }

        eprintln!("Restored memory {} from archive", id_prefix);
        Some(memory)
    }

    /// Run periodic memory maintenance.
    ///
    /// This should be called periodically (e.g., daily) to:
    /// 1. Apply decay to all memories
    /// 2. Archive low-importance memories
    /// 3. Clean up archive if needed
    ///
    /// # Arguments
    /// * `archive` - The MemoryArchive instance to use
    /// * `config` - Decay configuration parameters
    ///
    /// # Returns
    /// ArchiveResult with maintenance statistics
    pub async fn run_maintenance(
        &self,
        archive: &MemoryArchive,
        config: &DecayConfig,
    ) -> ArchiveResult {
        eprintln!("Running memory maintenance...");

        // Archive low-importance memories
        let result = self.archive_low_importance(archive, config).await;

        // Run archive cleanup
        let cleanup_count = archive.cleanup().await;

        eprintln!(
            "Maintenance complete: {} archived, {} cleaned up",
            result.archived_ids.len(),
            cleanup_count
        );

        ArchiveResult {
            archived_ids: result.archived_ids,
            deleted_count: result.deleted_count + cleanup_count,
        }
    }

    /// Get memory statistics.
    ///
    /// Returns counts of memories by category and archive status.
    pub async fn stats(&self) -> MemoryStats {
        let entries = self.entries.lock().await;

        let total = entries.len();
        let facts = entries
            .iter()
            .filter(|e| matches!(e.category, MemoryCategory::Fact))
            .count();
        let preferences = entries
            .iter()
            .filter(|e| matches!(e.category, MemoryCategory::Preference))
            .count();
        let decisions = entries
            .iter()
            .filter(|e| matches!(e.category, MemoryCategory::Decision))
            .count();
        let archived = entries.iter().filter(|e| e.archived).count();

        let avg_importance = if total > 0 {
            entries.iter().map(|e| e.importance).sum::<f64>() / total as f64
        } else {
            0.0
        };

        MemoryStats {
            total,
            facts,
            preferences,
            decisions,
            archived,
            avg_importance,
        }
    }
}

/// Memory statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    /// Total number of memories.
    pub total: usize,
    /// Number of fact memories.
    pub facts: usize,
    /// Number of preference memories.
    pub preferences: usize,
    /// Number of decision memories.
    pub decisions: usize,
    /// Number of archived memories.
    pub archived: usize,
    /// Average importance score.
    pub avg_importance: f64,
}

/// Parse a category string into MemoryCategory.
pub fn parse_category(s: &str) -> MemoryCategory {
    match s.to_lowercase().as_str() {
        "preference" | "pref" => MemoryCategory::Preference,
        "decision" | "dec" => MemoryCategory::Decision,
        _ => MemoryCategory::Fact,
    }
}

/// Decayed importance of an entry right now: the ranking signal for the
/// prompt fragment. Uses the same recency-anchor rule as maintenance decay
/// ([`days_since_anchor`]) so ranking and archival can't drift apart.
fn decayed_score(entry: &MemoryEntry, config: &DecayConfig) -> f64 {
    decay_score(entry, days_since_anchor(entry), config)
}

#[cfg(test)]
impl MemoryStore {
    /// Test-only injection for entries `add()` can't create (e.g. archived
    /// ones) — used by tests in other modules.
    pub async fn seed_for_tests(&self, entry: MemoryEntry) {
        self.entries.lock().await.push(entry);
    }
}

/// Restrict a memory file to owner-only permissions (best-effort, unix only).
/// The store holds everything the model has been told across sessions, so a
/// world-readable file would leak it to every local account.
async fn ensure_private_perms_async(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = tokio::fs::metadata(path).await {
            let mut perms = meta.permissions();
            if perms.mode() & 0o777 != 0o600 {
                perms.set_mode(0o600);
                let _ = tokio::fs::set_permissions(path, perms).await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> (tempfile::TempDir, MemoryStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        (dir, MemoryStore::load_with_path(path))
    }

    fn entry(id: &str, content: &str, category: MemoryCategory, importance: f64) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            content: content.to_string(),
            category,
            created_at: chrono::Utc::now().to_rfc3339(),
            source: "test".to_string(),
            importance,
            recall_count: 0,
            last_recalled_at: None,
            archived: false,
        }
    }

    async fn seed(store: &MemoryStore, entries: Vec<MemoryEntry>) {
        // Bypass add() so tests control ids/importance directly.
        *store.entries.lock().await = entries;
    }

    #[tokio::test]
    async fn fragment_includes_everything_within_budget() {
        let (_dir, store) = temp_store("memory.jsonl");
        seed(
            &store,
            vec![
                entry(
                    "a1",
                    "User prefers concise answers",
                    MemoryCategory::Preference,
                    0.8,
                ),
                entry(
                    "a2",
                    "Deploy target is the staging cluster",
                    MemoryCategory::Fact,
                    0.5,
                ),
            ],
        )
        .await;
        let frag = store
            .build_prompt_fragment_with(&DecayConfig {
                prompt_char_budget: 5000,
                ..DecayConfig::default()
            })
            .await
            .expect("fragment");
        assert!(frag.contains("[2/2 memories"));
        assert!(frag.contains("## Preferences"));
        assert!(frag.contains("- User prefers concise answers"));
        assert!(frag.contains("## Facts"));
        // Nothing omitted → no search pointer.
        assert!(!frag.contains("MemorySearch"));
    }

    #[tokio::test]
    async fn fragment_ranks_by_importance_within_budget() {
        let (_dir, store) = temp_store("memory.jsonl");
        let filler = "f".repeat(120);
        let mut entries = vec![entry(
            "hi",
            "Critical: production DB is postgres on port 5433",
            MemoryCategory::Fact,
            1.0,
        )];
        for i in 0..20 {
            entries.push(entry(
                &format!("low{i}"),
                &format!("memory {i}: {filler}"),
                MemoryCategory::Fact,
                0.2,
            ));
        }
        seed(&store, entries).await;
        let frag = store
            .build_prompt_fragment_with(&DecayConfig {
                prompt_char_budget: 400,
                ..DecayConfig::default()
            })
            .await
            .expect("fragment");
        // The high-importance entry must survive the budget cut.
        assert!(frag.contains("postgres on port 5433"));
        assert!(frag.contains("[3/21 memories"));
        // Omitted entries are pointed at the recall tool.
        assert!(frag.contains("lower-priority memories not shown"));
        assert!(frag.contains("MemorySearch"));
        assert!(frag.len() < 700);
    }

    #[tokio::test]
    async fn fragment_none_when_store_empty() {
        let (_dir, store) = temp_store("memory.jsonl");
        assert!(store
            .build_prompt_fragment_with(&DecayConfig::default())
            .await
            .is_none());
    }

    #[tokio::test]
    async fn read_file_dedups_exact_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory.jsonl");
        let dup = entry("d1", "same content", MemoryCategory::Fact, 0.5);
        let dup2 = {
            let mut e = entry("d2", "same content", MemoryCategory::Fact, 0.5);
            e.created_at = chrono::Utc::now().to_rfc3339();
            e
        };
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                serde_json::to_string(&dup).unwrap(),
                serde_json::to_string(&dup2).unwrap(),
                serde_json::to_string(&entry("u", "unique", MemoryCategory::Fact, 0.5)).unwrap()
            ),
        )
        .unwrap();
        let store = MemoryStore::load_with_path(path);
        let list = store.list().await;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "d1");
        assert_eq!(list[1].id, "u");
    }

    #[tokio::test]
    async fn add_with_importance_clamps_and_persists() {
        let (_dir, store) = temp_store("memory.jsonl");
        let outcome = store
            .add_with_importance(
                "User runs NixOS".to_string(),
                MemoryCategory::Fact,
                "auto".to_string(),
                7.5,
            )
            .await
            .expect("add");
        assert!(outcome.created);
        assert!((outcome.entry.importance - 1.0).abs() < f64::EPSILON);
        // Reload from disk: the entry (with clamped importance) round-trips.
        let reloaded = MemoryStore::load_with_path(_dir.path().join("memory.jsonl"));
        let list = reloaded.list().await;
        assert_eq!(list.len(), 1);
        assert!((list[0].importance - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn add_exact_duplicate_is_idempotent() {
        let (_dir, store) = temp_store("memory.jsonl");
        let first = store
            .add(
                "User prefers dark mode".to_string(),
                MemoryCategory::Preference,
                "auto".to_string(),
            )
            .await
            .unwrap();
        assert!(first.created);
        let second = store
            .add(
                "User prefers dark mode".to_string(),
                MemoryCategory::Preference,
                "user".to_string(),
            )
            .await
            .unwrap();
        assert!(!second.created);
        assert_eq!(second.entry.id, first.entry.id);
        // Case-differences are a different fact, not a duplicate.
        let third = store
            .add(
                "user prefers dark mode".to_string(),
                MemoryCategory::Preference,
                "auto".to_string(),
            )
            .await
            .unwrap();
        assert!(third.created);
        let list = store.list().await;
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn add_rejects_content_failing_security_scan() {
        let (_dir, store) = temp_store("memory.jsonl");
        let result = store
            .add(
                "The API key is sk-test0123456789abcdefghij".to_string(),
                MemoryCategory::Fact,
                "auto".to_string(),
            )
            .await;
        match result {
            Err(MemoryError::Rejected(reason)) => {
                assert!(reason.contains("credential"));
            }
            other => panic!("expected Rejected, got {:?}", other.map(|o| o.entry.id)),
        }
        assert!(store.list().await.is_empty());
        assert!(!_dir.path().join("memory.jsonl").exists());
    }

    #[tokio::test]
    async fn record_recall_boosts_importance_and_persists() {
        let (_dir, store) = temp_store("memory.jsonl");
        let e = store
            .add(
                "Deploy key lives in vault".to_string(),
                MemoryCategory::Fact,
                "auto".to_string(),
            )
            .await
            .unwrap();
        assert!((e.entry.importance - 0.5).abs() < f64::EPSILON);
        store
            .record_recall(std::slice::from_ref(&e.entry.id), &DecayConfig::default())
            .await;
        let list = store.list().await;
        assert!((list[0].importance - 0.6).abs() < f64::EPSILON);
        assert_eq!(list[0].recall_count, 1);
        assert!(list[0].last_recalled_at.is_some());
        // The boost survives a reload from disk.
        let reloaded = MemoryStore::load_with_path(_dir.path().join("memory.jsonl"));
        let persisted = reloaded.list().await;
        assert_eq!(persisted.len(), 1);
        assert!((persisted[0].importance - 0.6).abs() < f64::EPSILON);
        assert_eq!(persisted[0].recall_count, 1);
    }

    #[tokio::test]
    async fn record_recall_with_no_matches_skips_rewrite() {
        let (_dir, store) = temp_store("memory.jsonl");
        store
            .add(
                "something".to_string(),
                MemoryCategory::Fact,
                "auto".to_string(),
            )
            .await
            .unwrap();
        // Must not panic or touch the file.
        store
            .record_recall(&["zzzz".to_string()], &DecayConfig::default())
            .await;
        let list = store.list().await;
        assert_eq!(list[0].recall_count, 0);
    }

    #[tokio::test]
    async fn memory_file_gets_owner_only_permissions() {
        let (_dir, store) = temp_store("memory.jsonl");
        store
            .add(
                "secret-ish memory".to_string(),
                MemoryCategory::Fact,
                "auto".to_string(),
            )
            .await
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(_dir.path().join("memory.jsonl")).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn high_concurrency_stress_test_100_concurrent_operations() {
        let (_dir, store) = temp_store("memory.jsonl");
        let store = std::sync::Arc::new(store);

        let mut handles = Vec::new();
        // 50 concurrent writers and 50 concurrent readers
        for i in 0..50 {
            let s = std::sync::Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                s.add(
                    format!("concurrent memory entry {}", i),
                    MemoryCategory::Fact,
                    "stress_test".to_string(),
                )
                .await
                .unwrap();
            }));
        }
        for _ in 0..50 {
            let s = std::sync::Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                let _ = s.list().await;
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        let all = store.list().await;
        assert_eq!(all.len(), 50);

        // Verify reloaded store matches
        let path = _dir.path().join("memory.jsonl");
        let reloaded = MemoryStore::load_with_path(path);
        let reloaded_entries = reloaded.list().await;
        assert_eq!(reloaded_entries.len(), 50);
    }
}
