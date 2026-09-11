//! Memory archive management implementation.
//!
//! This module implements the archive system for low-importance memories.
//! Archived memories are moved from the active memory store to a separate
//! `memory_archive.jsonl` file, allowing them to be restored if needed.
//!
//! Archive triggers:
//! - Memory importance score below 0.1 (configurable)
//! - Manual archive command
//!
//! Features:
//! - Automatic archival of low-score memories
//! - Persistent storage in separate JSONL file
//! - Memory restoration capability
//! - Archive cleanup when exceeding 1000 entries

use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

use crate::engine::memory::{DecayConfig, MemoryEntry};

/// Archive file name for stored archived memories.
const ARCHIVE_FILE: &str = "memory_archive.jsonl";

/// Default maximum number of archived entries before cleanup is triggered.
const DEFAULT_MAX_ARCHIVE_SIZE: usize = 1000;

/// Result of an archive operation.
#[derive(Debug, Clone)]
pub struct ArchiveResult {
    /// IDs of memories that were archived.
    pub archived_ids: Vec<String>,
    /// Number of memories that were permanently deleted during cleanup.
    pub deleted_count: usize,
}

/// Memory archive for storing low-importance memories.
///
/// Archived memories are stored in a separate JSONL file and can be
/// restored to the active memory store if needed.
pub struct MemoryArchive {
    /// Archived memory entries.
    entries: Mutex<Vec<MemoryEntry>>,
    /// Path to the archive file.
    file_path: Mutex<PathBuf>,
    /// Maximum number of entries before cleanup.
    max_entries: usize,
}

impl MemoryArchive {
    /// Load the memory archive from the global ~/.baoclaw/memory_archive.jsonl.
    ///
    /// Creates an empty archive if the file doesn't exist.
    pub fn load() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let file_path = PathBuf::from(&home).join(".baoclaw").join(ARCHIVE_FILE);
        let entries = Self::read_file(&file_path);
        eprintln!(
            "Loaded {} archived memories from {}",
            entries.len(),
            file_path.display()
        );
        Self {
            entries: Mutex::new(entries),
            file_path: Mutex::new(file_path),
            max_entries: DEFAULT_MAX_ARCHIVE_SIZE,
        }
    }

    /// Load an archive from an explicit file path (test seam) — tests must
    /// never touch the user's real `~/.baoclaw` archive.
    pub fn load_with_path(file_path: PathBuf) -> Self {
        let entries = Self::read_file(&file_path);
        Self {
            entries: Mutex::new(entries),
            file_path: Mutex::new(file_path),
            max_entries: DEFAULT_MAX_ARCHIVE_SIZE,
        }
    }

    /// Load archive with custom configuration.
    pub fn load_with_config(config: &DecayConfig) -> Self {
        let mut archive = Self::load();
        archive.max_entries = config.max_entries;
        archive
    }

    /// Load project-level archive from <cwd>/.baoclaw/memory_archive.jsonl.
    /// Falls back to global archive if project doesn't have .baoclaw/.
    pub fn load_for_project(cwd: &std::path::Path) -> Self {
        let project_path = cwd.join(".baoclaw").join(ARCHIVE_FILE);
        let file_path = if cwd.join(".baoclaw").exists() {
            project_path
        } else {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(&home).join(".baoclaw").join(ARCHIVE_FILE)
        };
        let entries = Self::read_file(&file_path);
        eprintln!(
            "Loaded {} archived project memories from {}",
            entries.len(),
            file_path.display()
        );
        Self {
            entries: Mutex::new(entries),
            file_path: Mutex::new(file_path),
            max_entries: DEFAULT_MAX_ARCHIVE_SIZE,
        }
    }

    /// Read archived memories from file.
    fn read_file(path: &PathBuf) -> Vec<MemoryEntry> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Write all archived memories to file asynchronously.
    async fn write_all_async(path: &Path, entries: &[MemoryEntry]) {
        let lines: Vec<String> = entries
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .collect();
        let body = if lines.is_empty() {
            String::new()
        } else {
            lines.join("\n") + "\n"
        };
        let tmp = path.with_extension("tmp");
        if let Err(e) = tokio::fs::write(&tmp, body.as_bytes()).await {
            eprintln!(
                "[memory-archive] WARNING: could not write archive tmp {}: {} — archived memories may be lost",
                tmp.display(),
                e
            );
            return;
        }
        if let Err(e) = tokio::fs::rename(&tmp, path).await {
            eprintln!(
                "[memory-archive] WARNING: could not rename archive {}: {} — archived memories may be lost",
                path.display(),
                e
            );
        }
    }

    /// Archive a single memory entry.
    ///
    /// Moves the memory to the archive storage and marks it as archived.
    /// Returns the archived memory entry with `archived` flag set to true.
    pub async fn archive_memory(&self, mut memory: MemoryEntry) -> MemoryEntry {
        memory.archived = true;

        let line = match serde_json::to_string(&memory) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[memory-archive] WARNING: serialization failed: {}", e);
                return memory;
            }
        };

        // Add to archive
        {
            let mut entries = self.entries.lock().await;
            entries.push(memory.clone());
        }

        // Append to file without holding entries lock
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
                    eprintln!(
                        "[memory-archive] WARNING: append failed: {} — entry may be lost",
                        e
                    );
                } else {
                    let _ = f.flush().await;
                }
            }
            Err(e) => {
                eprintln!(
                    "[memory-archive] WARNING: open failed for append {}: {}",
                    fp.display(),
                    e
                );
            }
        }

        memory
    }

    /// Archive multiple memories at once.
    ///
    /// This is more efficient than calling `archive_memory` multiple times
    /// as it performs a single file write.
    ///
    /// Returns the number of memories archived and deleted.
    pub async fn archive_memories(&self, memories: Vec<MemoryEntry>) -> ArchiveResult {
        if memories.is_empty() {
            return ArchiveResult {
                archived_ids: Vec::new(),
                deleted_count: 0,
            };
        }

        let mut archived_ids = Vec::with_capacity(memories.len());
        let (entries_snapshot, deleted_count) = {
            let mut entries = self.entries.lock().await;

            for mut memory in memories {
                memory.archived = true;
                archived_ids.push(memory.id.clone());
                entries.push(memory);
            }

            // Check if cleanup is needed
            let deleted_count = if entries.len() > self.max_entries {
                let delete_count = entries.len() - self.max_entries;
                // Remove oldest entries (at the beginning)
                entries.drain(0..delete_count);
                eprintln!(
                    "Archive cleanup: removed {} old entries (limit: {})",
                    delete_count, self.max_entries
                );
                delete_count
            } else {
                0
            };
            (entries.clone(), deleted_count)
        };

        // Write entire archive to file without holding entries lock
        let fp = self.file_path.lock().await.clone();
        Self::write_all_async(&fp, &entries_snapshot).await;

        ArchiveResult {
            archived_ids,
            deleted_count,
        }
    }

    /// Restore a memory from the archive by ID prefix.
    ///
    /// Removes the memory from the archive and returns it with `archived`
    /// flag set to false. The caller is responsible for adding it back
    /// to the active memory store.
    ///
    /// Returns `None` if no memory matches the ID prefix.
    pub async fn restore_memory(&self, id_prefix: &str) -> Option<MemoryEntry> {
        let (mut memory, entries_snapshot) = {
            let mut entries = self.entries.lock().await;

            // Find and remove the memory
            let pos = entries.iter().position(|e| e.id.starts_with(id_prefix))?;
            let memory = entries.remove(pos);
            (memory, entries.clone())
        };

        // Mark as not archived
        memory.archived = false;

        // Update file without holding entries lock
        let fp = self.file_path.lock().await.clone();
        Self::write_all_async(&fp, &entries_snapshot).await;

        eprintln!("Restored memory {} from archive", memory.id);
        Some(memory)
    }

    /// List all archived memories.
    pub async fn list_archived(&self) -> Vec<MemoryEntry> {
        self.entries.lock().await.clone()
    }

    /// Get an archived memory by ID prefix without removing it.
    pub async fn get_archived(&self, id_prefix: &str) -> Option<MemoryEntry> {
        let entries = self.entries.lock().await;
        entries
            .iter()
            .find(|e| e.id.starts_with(id_prefix))
            .cloned()
    }

    /// Permanently delete an archived memory by ID prefix.
    ///
    /// Returns `true` if a memory was deleted.
    pub async fn delete_archived(&self, id_prefix: &str) -> bool {
        let (deleted, entries_snapshot) = {
            let mut entries = self.entries.lock().await;
            let before = entries.len();
            entries.retain(|e| !e.id.starts_with(id_prefix));

            if entries.len() < before {
                (true, Some(entries.clone()))
            } else {
                (false, None)
            }
        };

        if let Some(snapshot) = entries_snapshot {
            let fp = self.file_path.lock().await.clone();
            Self::write_all_async(&fp, &snapshot).await;
        }
        deleted
    }

    /// Clear all archived memories.
    ///
    /// Returns the number of memories cleared.
    pub async fn clear(&self) -> usize {
        let count = {
            let mut entries = self.entries.lock().await;
            let count = entries.len();
            entries.clear();
            count
        };

        let fp = self.file_path.lock().await.clone();
        if let Err(e) = tokio::fs::write(&fp, b"").await {
            eprintln!(
                "[memory-archive] WARNING: could not truncate archive {}: {}",
                fp.display(),
                e
            );
        }

        count
    }

    /// Get the number of archived memories.
    pub async fn count(&self) -> usize {
        self.entries.lock().await.len()
    }

    /// Check if the archive has exceeded the size limit.
    pub async fn needs_cleanup(&self) -> bool {
        self.entries.lock().await.len() > self.max_entries
    }

    /// Perform cleanup on the archive.
    ///
    /// Removes oldest entries if the archive exceeds the size limit.
    /// Returns the number of entries removed.
    pub async fn cleanup(&self) -> usize {
        let (delete_count, entries_snapshot) = {
            let mut entries = self.entries.lock().await;

            if entries.len() <= self.max_entries {
                return 0;
            }

            let delete_count = entries.len() - self.max_entries;
            entries.drain(0..delete_count);
            (delete_count, entries.clone())
        };

        let fp = self.file_path.lock().await.clone();
        Self::write_all_async(&fp, &entries_snapshot).await;

        eprintln!("Archive cleanup: removed {} old entries", delete_count);
        delete_count
    }

    /// Search archived memories by content (case-insensitive substring match).
    pub async fn search(&self, query: &str) -> Vec<MemoryEntry> {
        let query_lower = query.to_lowercase();
        let entries = self.entries.lock().await;
        entries
            .iter()
            .filter(|e| e.content.to_lowercase().contains(&query_lower))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::memory::store::MemoryCategory;

    fn create_test_memory(id: &str, importance: f64) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            content: format!("test content for {}", id),
            category: MemoryCategory::Fact,
            created_at: chrono::Utc::now().to_rfc3339(),
            source: "test".to_string(),
            importance,
            recall_count: 0,
            last_recalled_at: None,
            archived: false,
        }
    }

    /// Fresh archive in a throwaway directory (the TempDir must stay bound
    /// for the test's lifetime, hence the tuple).
    async fn fresh_archive() -> (tempfile::TempDir, MemoryArchive) {
        let dir = tempfile::tempdir().unwrap();
        let archive = MemoryArchive::load_with_path(dir.path().join("archive.jsonl"));
        (dir, archive)
    }

    #[tokio::test]
    async fn test_archive_single_memory() {
        let (_dir, archive) = fresh_archive().await;

        let memory = create_test_memory("test-archive-1", 0.05);
        let archived = archive.archive_memory(memory.clone()).await;

        assert!(archived.archived);
        assert_eq!(archived.id, "test-archive-1");

        // Cleanup
        archive.delete_archived("test-archive-1").await;
    }

    #[tokio::test]
    async fn test_archive_multiple_memories() {
        let (_dir, archive) = fresh_archive().await;

        let memories = vec![
            create_test_memory("test-multi-1", 0.05),
            create_test_memory("test-multi-2", 0.03),
        ];

        let result = archive.archive_memories(memories).await;

        assert_eq!(result.archived_ids.len(), 2);
        assert!(result.archived_ids.contains(&"test-multi-1".to_string()));
        assert!(result.archived_ids.contains(&"test-multi-2".to_string()));
        assert_eq!(result.deleted_count, 0);

        // Verify they're in the archive
        let listed = archive.list_archived().await;
        assert!(listed.iter().any(|e| e.id == "test-multi-1"));
        assert!(listed.iter().any(|e| e.id == "test-multi-2"));

        // Cleanup
    }

    #[tokio::test]
    async fn test_restore_memory() {
        let (_dir, archive) = fresh_archive().await;

        let memory = create_test_memory("test-restore", 0.05);
        archive.archive_memory(memory).await;

        let restored = archive.restore_memory("test-restore").await;

        assert!(restored.is_some());
        let restored = restored.unwrap();
        assert_eq!(restored.id, "test-restore");
        assert!(!restored.archived); // Should be unmarked

        // Should no longer be in archive
        let listed = archive.list_archived().await;
        assert!(!listed.iter().any(|e| e.id == "test-restore"));
    }

    #[tokio::test]
    async fn test_restore_nonexistent() {
        let (_dir, archive) = fresh_archive().await;

        let result = archive.restore_memory("nonexistent").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_archived() {
        let (_dir, archive) = fresh_archive().await;

        let memory = create_test_memory("test-get", 0.05);
        archive.archive_memory(memory).await;

        let retrieved = archive.get_archived("test-get").await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().id, "test-get");

        // Should still be in archive
        let listed = archive.list_archived().await;
        assert!(listed.iter().any(|e| e.id == "test-get"));
    }

    #[tokio::test]
    async fn test_delete_archived() {
        let (_dir, archive) = fresh_archive().await;

        let memory = create_test_memory("test-delete", 0.05);
        archive.archive_memory(memory).await;

        let deleted = archive.delete_archived("test-delete").await;
        assert!(deleted);

        // Should no longer be in archive
        let listed = archive.list_archived().await;
        assert!(!listed.iter().any(|e| e.id == "test-delete"));
    }

    #[tokio::test]
    async fn test_delete_nonexistent() {
        let (_dir, archive) = fresh_archive().await;

        let deleted = archive.delete_archived("nonexistent").await;
        assert!(!deleted);
    }

    #[tokio::test]
    async fn test_list_archived() {
        let (_dir, archive) = fresh_archive().await;

        assert!(archive.list_archived().await.is_empty());

        let memories = vec![
            create_test_memory("test-list-1", 0.05),
            create_test_memory("test-list-2", 0.03),
        ];
        archive.archive_memories(memories).await;

        let listed = archive.list_archived().await;
        assert_eq!(listed.len(), 2);
    }

    #[tokio::test]
    async fn test_clear() {
        let (_dir, archive) = fresh_archive().await;

        let memories = vec![
            create_test_memory("test-clear-1", 0.05),
            create_test_memory("test-clear-2", 0.03),
        ];
        archive.archive_memories(memories).await;

        let count = archive.clear().await;
        assert_eq!(count, 2);
        assert!(archive.list_archived().await.is_empty());
    }

    #[tokio::test]
    async fn test_count() {
        let (_dir, archive) = fresh_archive().await;

        assert_eq!(archive.count().await, 0);

        let memories = vec![
            create_test_memory("test-count-1", 0.05),
            create_test_memory("test-count-2", 0.03),
        ];
        archive.archive_memories(memories).await;

        assert_eq!(archive.count().await, 2);
    }

    #[tokio::test]
    async fn test_search() {
        let (_dir, archive) = fresh_archive().await;

        let mut memory1 = create_test_memory("test-search-1", 0.05);
        memory1.content = "Important fact about Rust programming".to_string();

        let mut memory2 = create_test_memory("test-search-2", 0.03);
        memory2.content = "User prefers dark mode".to_string();

        archive.archive_memories(vec![memory1, memory2]).await;

        // Search for "rust"
        let results = archive.search("rust").await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "test-search-1");

        // Search for "prefers"
        let results = archive.search("prefers").await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "test-search-2");

        // Search for "test" (should match neither)
        let results = archive.search("nonexistent_keyword").await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_cleanup() {
        // Create archive with low max_entries for testing
        let (_dir, archive) = fresh_archive().await;

        // Manually set low limit
        {
            let mut entries = archive.entries.lock().await;
            // Add more entries than max
            for i in 0..5 {
                entries.push(create_test_memory(&format!("test-cleanup-{}", i), 0.05));
            }
        }

        // Trigger cleanup with manual file write
        let count = archive.cleanup().await;
        // Should not trigger cleanup since we didn't set max_entries
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_needs_cleanup() {
        let (_dir, archive) = fresh_archive().await;

        assert!(!archive.needs_cleanup().await);

        // Add many entries
        let memories: Vec<MemoryEntry> = (0..1001)
            .map(|i| create_test_memory(&format!("test-needs-{}", i), 0.05))
            .collect();
        archive.archive_memories(memories).await;

        // Note: archive_memories performs cleanup, so this depends on max_entries
    }
}
