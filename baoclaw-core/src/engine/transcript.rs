use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// A single transcript record.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TranscriptEntry {
    pub timestamp: String,
    pub entry_type: TranscriptEntryType,
    pub data: Value,
}

/// The type of a transcript entry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum TranscriptEntryType {
    UserMessage,
    AssistantMessage,
    ToolUse,
    ToolResult,
    SystemEvent,
}

/// Session transcript writer — appends entries to a JSONL file.
pub struct TranscriptWriter {
    file: std::fs::File,
    session_id: String,
}

impl TranscriptWriter {
    /// Create or open a transcript file for the given session.
    ///
    /// The file is stored at `~/.baoclaw/sessions/{session_id}.jsonl`.
    pub fn open(session_id: &str) -> Result<Self, std::io::Error> {
        let dir = Self::sessions_dir()?;
        Self::open_in_dir(session_id, &dir)
    }

    /// Create or open a transcript file in a specific directory.
    pub fn open_in_dir(session_id: &str, dir: &PathBuf) -> Result<Self, std::io::Error> {
        let path =
            crate::engine::session_persistence::session_artifact_path(dir, session_id, "jsonl")?;
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self {
            file,
            session_id: session_id.to_string(),
        })
    }

    /// Append a single entry as a JSON line + flush.
    pub fn append(&mut self, entry: &TranscriptEntry) -> Result<(), std::io::Error> {
        let line = serde_json::to_string(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        writeln!(self.file, "{}", line)?;
        self.file.flush()
    }

    /// Load all valid transcript entries for a session.
    ///
    /// Corrupted JSON lines are silently skipped.
    pub fn load(session_id: &str) -> Result<Vec<TranscriptEntry>, std::io::Error> {
        let dir = Self::sessions_dir()?;
        Self::load_from_dir(session_id, &dir)
    }

    /// Load all valid transcript entries from a specific directory.
    pub fn load_from_dir(
        session_id: &str,
        dir: &Path,
    ) -> Result<Vec<TranscriptEntry>, std::io::Error> {
        let path =
            crate::engine::session_persistence::session_artifact_path(dir, session_id, "jsonl")?;
        let file = std::fs::File::open(&path)?;
        let reader = std::io::BufReader::new(file);
        let entries = reader
            .lines()
            .filter_map(|line_result| {
                let line = line_result.ok()?;
                if line.trim().is_empty() {
                    return None;
                }
                serde_json::from_str::<TranscriptEntry>(&line).ok()
            })
            .collect();
        Ok(entries)
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Return the sessions directory path (`~/.baoclaw/sessions`).
    fn sessions_dir() -> Result<PathBuf, std::io::Error> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "HOME directory not found")
            })?;
        Ok(PathBuf::from(home).join(".baoclaw").join("sessions"))
    }
}

/// Rebuild a messages vector from transcript entries.
///
/// Only UserMessage and AssistantMessage entries are converted back to Messages.
/// ToolUse and ToolResult entries are skipped since they are embedded in the
/// assistant/user messages.
pub fn rebuild_messages_from_transcript(
    entries: &[TranscriptEntry],
) -> Vec<crate::models::message::Message> {
    use crate::models::message::{ApiUserMessage, Message, MessageContent};

    let mut messages: Vec<Message> = Vec::new();
    let mut pending_tool_results: Vec<serde_json::Value> = Vec::new();

    for entry in entries {
        match entry.entry_type {
            TranscriptEntryType::UserMessage | TranscriptEntryType::AssistantMessage => {
                // Flush any pending tool results as a user message first
                if !pending_tool_results.is_empty() {
                    messages.push(Message {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: entry.timestamp.clone(),
                        content: MessageContent::User {
                            message: ApiUserMessage {
                                role: "user".to_string(),
                                content: serde_json::Value::Array(std::mem::take(
                                    &mut pending_tool_results,
                                )),
                            },
                            is_meta: false,
                            tool_use_result: None,
                        },
                    });
                }
                // Add the actual message
                if let Ok(msg) = serde_json::from_value::<Message>(entry.data.clone()) {
                    messages.push(msg);
                }
            }
            TranscriptEntryType::ToolResult => {
                // Accumulate tool results to be flushed as a user message
                let tool_use_id = entry
                    .data
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let raw_output = entry
                    .data
                    .get("output")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                // API requires content to be a string or array of content blocks, not an object
                let output_str = match &raw_output {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => String::new(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };
                let is_error = entry
                    .data
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                pending_tool_results.push(serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": output_str,
                    "is_error": is_error,
                }));
            }
            _ => {} // Skip ToolUse, SystemEvent
        }
    }

    // Flush any remaining tool results
    if !pending_tool_results.is_empty() {
        messages.push(Message {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: MessageContent::User {
                message: ApiUserMessage {
                    role: "user".to_string(),
                    content: serde_json::Value::Array(pending_tool_results),
                },
                is_meta: false,
                tool_use_result: None,
            },
        });
    }

    messages
}

/// Rebuild messages from transcript entries, limited to the last `max_entries`.
///
/// If the entry count exceeds `max_entries`, only the tail is rebuilt.
/// When a `summary` is provided and truncation occurs, a `CompactBoundary`
/// system message is prepended so the LLM has context about earlier turns.
pub fn rebuild_messages_from_transcript_limited(
    entries: &[TranscriptEntry],
    max_entries: usize,
    summary: Option<&str>,
) -> Vec<crate::models::message::Message> {
    use crate::models::message::{Message, MessageContent, SystemSubtype};

    if entries.len() <= max_entries {
        return rebuild_messages_from_transcript(entries);
    }

    let limited = &entries[entries.len() - max_entries..];
    let mut messages = rebuild_messages_from_transcript(limited);

    if let Some(summary_text) = summary {
        if !summary_text.is_empty() {
            let boundary = Message {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                content: MessageContent::System {
                    subtype: SystemSubtype::CompactBoundary,
                    content: format!("[Session Memory — earlier context]\n{}", summary_text),
                },
            };
            messages.insert(0, boundary);
        }
    }

    messages
}

/// Find the transcript session a given cwd should resume.
///
/// Sessions are stored as `{cwd_hash}-{surface}.jsonl`.  Resolution order:
/// 1. With `preferred_session_id` (the caller's own `{cwd_hash}-{surface}`
///    id): that exact id if its transcript exists, else the newest
///    transcript carrying the **same surface** suffix.  A preferred id with
///    no surface part (e.g. `cron-{uuid}`) never adopts a transcript.
/// 2. With `None` (legacy discovery in `resolve_session_id`): the newest
///    matching transcript for the cwd, any surface.
pub fn find_latest_session_for_cwd(
    cwd: &str,
    preferred_session_id: Option<&str>,
) -> Option<String> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    let sessions_dir = PathBuf::from(home).join(".baoclaw").join("sessions");
    find_latest_session_for_cwd_in(&sessions_dir, cwd, preferred_session_id)
}

/// Directory-injectable variant of [`find_latest_session_for_cwd`] (test seam).
pub fn find_latest_session_for_cwd_in(
    sessions_dir: &std::path::Path,
    cwd: &str,
    preferred_session_id: Option<&str>,
) -> Option<String> {
    if !sessions_dir.is_dir() {
        return None;
    }

    // Match the current 16-character cwd identity and the legacy 8-character
    // FNV identity so existing transcripts remain discoverable during migration.
    let cwd_hash = cwd_identity_hash(cwd);
    let prefixes = [cwd_hash.as_str(), &cwd_hash[..8]];

    let mut candidates: Vec<(String, std::time::SystemTime)> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(sessions_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if prefixes.iter().any(|prefix| name.starts_with(prefix)) && name.ends_with(".jsonl") {
                let session_id = name.trim_end_matches(".jsonl").to_string();
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        candidates.push((session_id, modified));
                    }
                }
            }
        }
    }

    // Surface isolation: a session may only ever resume its own surface.
    // Without this, the newest transcript for the cwd wins regardless of
    // which surface (telegram/web/cli/...) wrote it, and the resuming
    // session inherits another surface's history and memory summary.
    if let Some(preferred) = preferred_session_id {
        if candidates.iter().any(|(id, _)| id == preferred) {
            return Some(preferred.to_string());
        }
        let surface = session_surface(preferred, &cwd_hash);
        return match surface {
            Some(surface) => candidates
                .into_iter()
                .filter(|(id, _)| session_surface(id, &cwd_hash) == Some(surface))
                .max_by_key(|(_, modified)| *modified)
                .map(|(id, _)| id),
            // The preferred id has no surface suffix (e.g. ephemeral job
            // sessions) — never adopt another session's transcript.
            None => None,
        };
    }

    candidates
        .into_iter()
        .max_by_key(|(_, modified)| *modified)
        .map(|(id, _)| id)
}

/// The surface component of a session id: the part after the cwd hash
/// and its separator dash (e.g. `db7ff752…-telegram` → `telegram`).
pub fn session_surface<'a>(session_id: &'a str, cwd_hash: &str) -> Option<&'a str> {
    session_id
        .strip_prefix(cwd_hash)
        .or_else(|| session_id.strip_prefix(&cwd_hash[..8]))
        .and_then(|rest| rest.strip_prefix('-'))
        .filter(|surface| !surface.is_empty())
}

/// 16-character FNV-1a identity hash for a cwd.
///
/// Single source of truth: session ids are built from this (main.rs
/// `cwd_hash` delegates here), so resume's surface matching always agrees
/// with id construction.
pub fn cwd_identity_hash(cwd: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in cwd.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Create an empty transcript file with a deterministic mtime so
    /// "newest" comparisons are stable.
    fn write_session_file(
        dir: &std::path::Path,
        session_id: &str,
        modified: std::time::SystemTime,
    ) {
        let path = dir.join(format!("{}.jsonl", session_id));
        std::fs::write(&path, "").unwrap();
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }

    fn unix_time(secs: u64) -> std::time::SystemTime {
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    }

    #[test]
    fn test_resume_prefers_exact_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let hash = cwd_identity_hash("/proj");
        write_session_file(dir.path(), &format!("{}-telegram", hash), unix_time(2000));
        write_session_file(dir.path(), &format!("{}-default", hash), unix_time(1000));

        let found =
            find_latest_session_for_cwd_in(dir.path(), "/proj", Some(&format!("{}-default", hash)))
                .unwrap();
        assert_eq!(found, format!("{}-default", hash));
    }

    #[test]
    fn test_resume_never_adopts_other_surface() {
        let dir = tempfile::tempdir().unwrap();
        let hash = cwd_identity_hash("/proj");
        write_session_file(dir.path(), &format!("{}-telegram", hash), unix_time(2000));
        write_session_file(dir.path(), &format!("{}-default", hash), unix_time(1000));

        // The preferred surface (`-web`) has no transcript at all: the
        // newer cross-surface file must not be adopted.
        let found =
            find_latest_session_for_cwd_in(dir.path(), "/proj", Some(&format!("{}-web", hash)));
        assert_eq!(found, None);
    }

    #[test]
    fn test_resume_prefers_older_same_surface_over_newer_cross_surface() {
        let dir = tempfile::tempdir().unwrap();
        let hash = cwd_identity_hash("/proj");
        write_session_file(dir.path(), &format!("{}-telegram", hash), unix_time(2000));
        write_session_file(dir.path(), &format!("{}-default", hash), unix_time(1000));
        write_session_file(dir.path(), &format!("{}-web", hash), unix_time(500));

        // `-web` has no exact match, but its own older transcript exists —
        // the same-surface fallback must return it, not the newer telegram.
        let found =
            find_latest_session_for_cwd_in(dir.path(), "/proj", Some(&format!("{}-web", hash)))
                .unwrap();
        assert_eq!(found, format!("{}-web", hash));
    }

    #[test]
    fn test_resume_matches_legacy_hash_same_surface() {
        let dir = tempfile::tempdir().unwrap();
        let hash = cwd_identity_hash("/proj");
        write_session_file(
            dir.path(),
            &format!("{}-default", &hash[..8]),
            unix_time(1000),
        );

        let found =
            find_latest_session_for_cwd_in(dir.path(), "/proj", Some(&format!("{}-default", hash)))
                .unwrap();
        assert_eq!(found, format!("{}-default", &hash[..8]));
    }

    #[test]
    fn test_resume_surfaceless_preferred_never_adopts() {
        let dir = tempfile::tempdir().unwrap();
        let hash = cwd_identity_hash("/proj");
        write_session_file(dir.path(), &format!("{}-telegram", hash), unix_time(2000));

        // Ephemeral job sessions have no surface — never resume another's.
        let found = find_latest_session_for_cwd_in(dir.path(), "/proj", Some("cron-ab12cd34"));
        assert_eq!(found, None);
    }

    #[test]
    fn test_no_preferred_keeps_newest_any_surface() {
        let dir = tempfile::tempdir().unwrap();
        let hash = cwd_identity_hash("/proj");
        write_session_file(dir.path(), &format!("{}-telegram", hash), unix_time(2000));
        write_session_file(dir.path(), &format!("{}-default", hash), unix_time(1000));

        let found = find_latest_session_for_cwd_in(dir.path(), "/proj", None).unwrap();
        assert_eq!(found, format!("{}-telegram", hash));
    }

    #[test]
    fn test_session_surface_extraction() {
        let hash = cwd_identity_hash("/proj");
        assert_eq!(
            session_surface(&format!("{}-telegram", hash), &hash),
            Some("telegram")
        );
        assert_eq!(
            session_surface(&format!("{}-default", &hash[..8]), &hash),
            Some("default")
        );
        assert_eq!(session_surface(&hash, &hash), None);
        assert_eq!(session_surface("cron-ab12cd34", &hash), None);
        assert_eq!(session_surface(&format!("{}-", hash), &hash), None);
    }

    /// Helper to create a test TranscriptEntry.
    fn make_entry(entry_type: TranscriptEntryType, data: Value) -> TranscriptEntry {
        TranscriptEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            entry_type,
            data,
        }
    }

    #[test]
    fn test_transcript_entry_serialization_roundtrip() {
        let entry = make_entry(
            TranscriptEntryType::UserMessage,
            json!({"role": "user", "content": "hello"}),
        );
        let json_str = serde_json::to_string(&entry).unwrap();
        let deserialized: TranscriptEntry = serde_json::from_str(&json_str).unwrap();
        assert_eq!(entry, deserialized);
    }

    #[test]
    fn test_transcript_entry_type_variants() {
        let types = vec![
            TranscriptEntryType::UserMessage,
            TranscriptEntryType::AssistantMessage,
            TranscriptEntryType::ToolUse,
            TranscriptEntryType::ToolResult,
            TranscriptEntryType::SystemEvent,
        ];
        for t in types {
            let entry = make_entry(t.clone(), json!({}));
            let json_str = serde_json::to_string(&entry).unwrap();
            let deserialized: TranscriptEntry = serde_json::from_str(&json_str).unwrap();
            assert_eq!(entry.entry_type, deserialized.entry_type);
        }
    }

    #[test]
    fn test_write_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");

        let session_id = "test-roundtrip-session";
        let entries = vec![
            make_entry(
                TranscriptEntryType::UserMessage,
                json!({"content": "hello"}),
            ),
            make_entry(
                TranscriptEntryType::AssistantMessage,
                json!({"content": "hi there"}),
            ),
            make_entry(
                TranscriptEntryType::ToolUse,
                json!({"tool": "bash", "input": {"cmd": "ls"}}),
            ),
            make_entry(
                TranscriptEntryType::ToolResult,
                json!({"output": "file.txt"}),
            ),
        ];

        // Write entries
        {
            let mut writer = TranscriptWriter::open_in_dir(session_id, &sessions_dir).unwrap();
            assert_eq!(writer.session_id(), session_id);
            for entry in &entries {
                writer.append(entry).unwrap();
            }
        }

        // Load and verify
        let loaded = TranscriptWriter::load_from_dir(session_id, &sessions_dir).unwrap();
        assert_eq!(loaded.len(), entries.len());
        for (original, loaded_entry) in entries.iter().zip(loaded.iter()) {
            assert_eq!(original.entry_type, loaded_entry.entry_type);
            assert_eq!(original.data, loaded_entry.data);
            assert_eq!(original.timestamp, loaded_entry.timestamp);
        }
    }

    #[test]
    fn test_corrupted_lines_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");

        let session_id = "test-corrupted-session";

        // Write a valid entry
        {
            let mut writer = TranscriptWriter::open_in_dir(session_id, &sessions_dir).unwrap();
            let entry = make_entry(
                TranscriptEntryType::UserMessage,
                json!({"content": "valid"}),
            );
            writer.append(&entry).unwrap();
        }

        // Manually append a corrupted line
        let path = sessions_dir.join(format!("{}.jsonl", session_id));
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{{this is not valid json}}").unwrap();
        writeln!(file).unwrap(); // empty line

        // Write another valid entry
        {
            let mut writer = TranscriptWriter::open_in_dir(session_id, &sessions_dir).unwrap();
            let entry = make_entry(
                TranscriptEntryType::AssistantMessage,
                json!({"content": "also valid"}),
            );
            writer.append(&entry).unwrap();
        }

        // Load should skip corrupted and empty lines
        let loaded = TranscriptWriter::load_from_dir(session_id, &sessions_dir).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].entry_type, TranscriptEntryType::UserMessage);
        assert_eq!(loaded[1].entry_type, TranscriptEntryType::AssistantMessage);
    }

    #[test]
    fn test_load_nonexistent_session_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let result = TranscriptWriter::load_from_dir("nonexistent-session-id", &sessions_dir);
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_file_loads_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");

        let session_id = "test-empty-session";

        // Create an empty file by opening and immediately closing
        {
            let _writer = TranscriptWriter::open_in_dir(session_id, &sessions_dir).unwrap();
        }

        let loaded = TranscriptWriter::load_from_dir(session_id, &sessions_dir).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_rebuild_messages_from_transcript() {
        use crate::models::message::{
            ApiAssistantMessage, ApiUserMessage, ContentBlock, Message, MessageContent,
        };

        let user_msg = Message {
            uuid: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            timestamp: "2024-01-15T10:30:00Z".to_string(),
            content: MessageContent::User {
                message: ApiUserMessage {
                    role: "user".to_string(),
                    content: Value::String("hello".to_string()),
                },
                is_meta: false,
                tool_use_result: None,
            },
        };

        let assistant_msg = Message {
            uuid: "550e8400-e29b-41d4-a716-446655440001".to_string(),
            timestamp: "2024-01-15T10:30:01Z".to_string(),
            content: MessageContent::Assistant {
                message: ApiAssistantMessage {
                    role: "assistant".to_string(),
                    content: vec![ContentBlock::Text {
                        text: "hi".to_string(),
                    }],
                    stop_reason: Some("end_turn".to_string()),
                    usage: None,
                },
                cost_usd: 0.001,
                duration_ms: 100,
            },
        };

        let entries = vec![
            TranscriptEntry {
                timestamp: "2024-01-15T10:30:00Z".to_string(),
                entry_type: TranscriptEntryType::UserMessage,
                data: serde_json::to_value(&user_msg).unwrap(),
            },
            TranscriptEntry {
                timestamp: "2024-01-15T10:30:01Z".to_string(),
                entry_type: TranscriptEntryType::AssistantMessage,
                data: serde_json::to_value(&assistant_msg).unwrap(),
            },
            TranscriptEntry {
                timestamp: "2024-01-15T10:30:02Z".to_string(),
                entry_type: TranscriptEntryType::ToolUse,
                data: json!({"tool_name": "bash", "input": {}}),
            },
            TranscriptEntry {
                timestamp: "2024-01-15T10:30:03Z".to_string(),
                entry_type: TranscriptEntryType::ToolResult,
                data: json!({"output": "ok"}),
            },
        ];

        let messages = rebuild_messages_from_transcript(&entries);
        // UserMessage, AssistantMessage, and a ToolResult user message
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].uuid, user_msg.uuid);
        assert_eq!(messages[1].uuid, assistant_msg.uuid);
        // Third message should be a user message with tool results
        if let MessageContent::User { message: msg, .. } = &messages[2].content {
            assert_eq!(msg.role, "user");
        } else {
            panic!("Expected User message with tool results");
        }
    }
}
