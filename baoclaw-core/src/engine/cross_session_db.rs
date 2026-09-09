use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::models::message::{ContentBlock, MessageContent};

// ── Data Structures ──────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionIndex {
    pub id: String,
    pub cwd: String,
    pub model: String,
    pub started_at: String,
    pub ended_at: String,
    pub turn_count: i32,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub session_id: String,
    pub snippet: String,
    pub rank: f64,
    pub timestamp: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageRecord {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

// ── Database Path Helper ─────────────────────────────────────────────────────

fn db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(home).join(".baoclaw");
    std::fs::create_dir_all(&dir).ok();
    dir.join("cross_session.db")
}

/// Strip HTML tags from a string (simple version: remove `<...>`).
fn strip_html(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

// ── CrossSessionDb ───────────────────────────────────────────────────────────

pub struct CrossSessionDb {
    conn: Mutex<Connection>,
}

impl CrossSessionDb {
    /// Open (or create) the database at the default path and ensure all tables exist.
    pub fn new() -> Result<Self, String> {
        Self::with_path(&db_path())
    }

    /// Open (or create) the database at a custom path (tests use this for
    /// hermetic temp dirs). Initializes the schema if needed.
    pub fn with_path(path: &PathBuf) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create cross-session directory: {}", e))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| format!("Failed to open cross_session.db at {:?}: {}", path, e))?;

        // busy_timeout: the CLI and the daemon can hold this file open at the
        // same time; brief write contention should wait, not error.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=2000;",
        )
        .map_err(|e| format!("PRAGMA failed: {}", e))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                 id          TEXT PRIMARY KEY,
                 cwd         TEXT    NOT NULL DEFAULT '',
                 model       TEXT    NOT NULL DEFAULT '',
                 started_at  TEXT    NOT NULL DEFAULT '',
                 ended_at    TEXT    NOT NULL DEFAULT '',
                 turn_count  INTEGER NOT NULL DEFAULT 0,
                 cost_usd    REAL    NOT NULL DEFAULT 0.0
             );

             CREATE TABLE IF NOT EXISTS messages (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id  TEXT    NOT NULL REFERENCES sessions(id),
                 role        TEXT    NOT NULL,
                 content     TEXT    NOT NULL DEFAULT '',
                 timestamp   TEXT    NOT NULL DEFAULT ''
             );

             CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts
                 USING fts5(content, content=messages, content_rowid=id);",
        )
        .map_err(|e| format!("Create tables failed: {}", e))?;

        // Keep the FTS index in sync via triggers (if they do not exist yet).
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
                 INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
             END;

             CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
                 INSERT INTO messages_fts(messages_fts, rowid, content)
                     VALUES('delete', old.id, old.content);
             END;

             CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
                 INSERT INTO messages_fts(messages_fts, rowid, content)
                     VALUES('delete', old.id, old.content);
                 INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
             END;",
        )
        .map_err(|e| format!("Create triggers failed: {}", e))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Index a full session: insert or update the session row. A real upsert
    /// (not INSERT OR REPLACE): with foreign_keys=ON, REPLACE would delete
    /// the parent row and fail once any message references the session.
    /// MIN(started_at) preserves the original start across re-stubs.
    pub fn index_session(&self, summary: SessionIndex) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;

        conn.execute(
            "INSERT INTO sessions (id, cwd, model, started_at, ended_at, turn_count, cost_usd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                 cwd = excluded.cwd,
                 model = excluded.model,
                 started_at = MIN(sessions.started_at, excluded.started_at),
                 ended_at = excluded.ended_at,
                 turn_count = excluded.turn_count,
                 cost_usd = excluded.cost_usd",
            params![
                summary.id,
                summary.cwd,
                summary.model,
                summary.started_at,
                summary.ended_at,
                summary.turn_count,
                summary.cost_usd,
            ],
        )
        .map_err(|e| format!("Insert session failed: {}", e))?;

        Ok(())
    }

    /// Insert a single message for a session (FTS trigger fires automatically).
    pub fn index_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        timestamp: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let cleaned = strip_html(content);

        conn.execute(
            "INSERT INTO messages (session_id, role, content, timestamp)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id, role, cleaned, timestamp],
        )
        .map_err(|e| format!("Insert message failed: {}", e))?;

        Ok(())
    }

    /// Whether any message is indexed for this session. Backfill uses this
    /// to skip sessions that are already indexed — the live indexer writes
    /// the session row before its messages, so row-presence alone would
    /// double-count live sessions.
    pub fn has_messages(&self, session_id: &str) -> bool {
        let Ok(conn) = self.conn.lock() else {
            return false;
        };
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id = ?1)",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|v| v != 0)
        .unwrap_or(false)
    }

    /// Backfill historical session snapshots into the search index.
    ///
    /// Scans `<sessions_dir>/*.json` (skipping `registry.json`); for every
    /// session with no indexed messages yet, upserts the session row and
    /// indexes its real user prompts and assistant text (meta user messages
    /// — synthetic tool results and system injections — are skipped, and
    /// non-text assistant blocks carry no searchable prose). Idempotent:
    /// already-indexed sessions are skipped, so restarts never duplicate
    /// messages. Returns (sessions imported, messages indexed).
    pub fn backfill_from_snapshots(&self, sessions_dir: &Path) -> (usize, usize) {
        use crate::engine::session_persistence::PersistedSession;

        let entries = match std::fs::read_dir(sessions_dir) {
            Ok(e) => e,
            Err(_) => return (0, 0),
        };
        let mut sessions = 0usize;
        let mut messages = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) == Some("registry.json") {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            // Corrupt or foreign JSON is skipped, like rebuild_registry.
            let Ok(snapshot) = serde_json::from_str::<PersistedSession>(&raw) else {
                continue;
            };
            if self.has_messages(&snapshot.session_id) {
                continue;
            }
            // Never guess at a future snapshot shape — same rule as
            // rebuild_registry.
            if snapshot.schema_version > crate::engine::session_persistence::CURRENT_SCHEMA_VERSION
            {
                continue;
            }

            // One extraction pass first: the session row must exist before
            // its messages (foreign key).
            let mut turn_count = 0i32;
            let mut cost_usd = 0.0f64;
            let mut extracted: Vec<(&str, String, &str)> = Vec::new();
            for msg in &snapshot.messages {
                match &msg.content {
                    MessageContent::User {
                        message, is_meta, ..
                    } => {
                        if *is_meta {
                            continue;
                        }
                        turn_count += 1;
                        let text = match &message.content {
                            serde_json::Value::String(s) => s.clone(),
                            other => serde_json::to_string(other).unwrap_or_default(),
                        };
                        if !text.is_empty() {
                            extracted.push(("user", text, msg.timestamp.as_str()));
                        }
                    }
                    MessageContent::Assistant {
                        message,
                        cost_usd: c,
                        ..
                    } => {
                        cost_usd += c;
                        let text = message
                            .content
                            .iter()
                            .filter_map(|b| match b {
                                ContentBlock::Text { text } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !text.is_empty() {
                            extracted.push(("assistant", text, msg.timestamp.as_str()));
                        }
                    }
                    _ => {}
                }
            }

            let summary = SessionIndex {
                id: snapshot.session_id.clone(),
                cwd: snapshot.cwd.clone(),
                model: snapshot.model.clone(),
                started_at: snapshot.created_at.clone(),
                ended_at: snapshot.last_active.clone(),
                turn_count,
                cost_usd,
            };
            if self.index_session(summary).is_err() {
                continue;
            }
            let mut indexed_here = 0usize;
            for (role, text, ts) in &extracted {
                match self.index_message(&snapshot.session_id, role, text, ts) {
                    Ok(()) => indexed_here += 1,
                    // A dropped message must be visible, or the
                    // has_messages guard would hide the gap forever.
                    Err(e) => eprintln!(
                        "[cross-session] WARNING: backfill message not indexed for {}: {}",
                        snapshot.session_id, e
                    ),
                }
            }
            sessions += 1;
            messages += indexed_here;
        }
        (sessions, messages)
    }

    /// Full-text search using FTS5 with bm25 ranking.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };

        let sql = format!(
            "SELECT m.session_id,
                    snippet(messages_fts, 0, '[', ']', '...', 32) AS snippet,
                    bm25(messages_fts) AS rank,
                    m.timestamp,
                    s.cwd
             FROM messages_fts
             JOIN messages m ON m.id = messages_fts.rowid
             JOIN sessions  s ON s.id = m.session_id
             WHERE messages_fts MATCH ?1
             ORDER BY rank
             LIMIT {}",
            limit
        );

        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };

        let rows = stmt.query_map(params![query], |row| {
            Ok(SearchResult {
                session_id: row.get(0)?,
                snippet: row.get(1)?,
                rank: row.get(2)?,
                timestamp: row.get(3)?,
                cwd: row.get(4)?,
            })
        });

        match rows {
            Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
            Err(_) => vec![],
        }
    }

    /// Return the most recent N sessions ordered by started_at descending.
    pub fn get_recent_sessions(&self, limit: usize) -> Vec<SessionIndex> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };

        let mut stmt = match conn.prepare(
            "SELECT id, cwd, model, started_at, ended_at, turn_count, cost_usd
             FROM sessions
             ORDER BY started_at DESC
             LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };

        let rows = stmt.query_map(params![limit], |row| {
            Ok(SessionIndex {
                id: row.get(0)?,
                cwd: row.get(1)?,
                model: row.get(2)?,
                started_at: row.get(3)?,
                ended_at: row.get(4)?,
                turn_count: row.get(5)?,
                cost_usd: row.get(6)?,
            })
        });

        match rows {
            Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
            Err(_) => vec![],
        }
    }

    /// FTS5 search that also returns one message before and after each hit for context.
    pub fn search_with_context(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        // First, get the base search results.
        let base = self.search(query, limit);
        if base.is_empty() {
            return vec![];
        }

        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return base,
        };

        let mut enriched = Vec::with_capacity(base.len());
        for sr in &base {
            // Find the rowid of the matched message.
            let msg_rowid: Option<i64> = conn
                .query_row(
                    "SELECT id FROM messages
                     WHERE session_id = ?1 AND timestamp = ?2
                     ORDER BY id DESC LIMIT 1",
                    params![sr.session_id, sr.timestamp],
                    |row| row.get(0),
                )
                .ok();

            let mut context_parts: Vec<String> = Vec::new();

            if let Some(rid) = msg_rowid {
                // Previous message
                if let Ok(prev) = conn.query_row(
                    "SELECT role, content FROM messages WHERE id = ?1",
                    params![rid - 1],
                    |row| {
                        let role: String = row.get(0)?;
                        let content: String = row.get(1)?;
                        Ok(format!("[{}]: {}", role, content))
                    },
                ) {
                    context_parts.push(prev);
                }

                context_parts.push(format!(">>> {}", sr.snippet));

                // Next message
                if let Ok(next) = conn.query_row(
                    "SELECT role, content FROM messages WHERE id = ?1",
                    params![rid + 1],
                    |row| {
                        let role: String = row.get(0)?;
                        let content: String = row.get(1)?;
                        Ok(format!("[{}]: {}", role, content))
                    },
                ) {
                    context_parts.push(next);
                }
            } else {
                context_parts.push(sr.snippet.clone());
            }

            enriched.push(SearchResult {
                session_id: sr.session_id.clone(),
                snippet: context_parts.join("\n---\n"),
                rank: sr.rank,
                timestamp: sr.timestamp.clone(),
                cwd: sr.cwd.clone(),
            });
        }

        enriched
    }

    /// Return every message belonging to a given session.
    pub fn get_session_messages(&self, session_id: &str) -> Vec<MessageRecord> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };

        let mut stmt = match conn.prepare(
            "SELECT id, session_id, role, content, timestamp
             FROM messages
             WHERE session_id = ?1
             ORDER BY id ASC",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };

        let rows = stmt.query_map(params![session_id], |row| {
            Ok(MessageRecord {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                timestamp: row.get(4)?,
            })
        });

        match rows {
            Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
            Err(_) => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_db() -> (CrossSessionDb, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cross_session_test.db");
        (CrossSessionDb::with_path(&path).unwrap(), dir)
    }

    fn summary(id: &str) -> SessionIndex {
        SessionIndex {
            id: id.to_string(),
            cwd: "/tmp/proj".to_string(),
            model: "test-model".to_string(),
            started_at: "2026-09-09T10:00:00Z".to_string(),
            ended_at: "2026-09-09T10:05:00Z".to_string(),
            turn_count: 2,
            cost_usd: 0.01,
        }
    }

    #[test]
    fn test_message_without_session_row_is_rejected() {
        // Regression guard for the FK gap: with foreign_keys=ON, message
        // inserts MUST fail until the session row exists — silently
        // dropping them left the search index empty.
        let (db, _dir) = test_db();
        let err = db
            .index_message("ghost-session", "user", "hello", "2026-09-09T10:00:00Z")
            .unwrap_err();
        assert!(err.to_lowercase().contains("foreign key"), "got: {}", err);
        assert!(db.search("hello", 10).is_empty());
    }

    #[test]
    fn test_session_then_message_then_search_flow() {
        let (db, _dir) = test_db();
        db.index_session(summary("s-1")).unwrap();
        db.index_message(
            "s-1",
            "user",
            "how does the FTS indexer work",
            "2026-09-09T10:00:10Z",
        )
        .unwrap();
        db.index_message(
            "s-1",
            "assistant",
            "It uses SQLite FTS5 with bm25 ranking",
            "2026-09-09T10:00:12Z",
        )
        .unwrap();

        let hits = db.search_with_context("FTS indexer", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s-1");
        assert_eq!(hits[0].cwd, "/tmp/proj");
        // snippet() column/markers must render cleanly: match wrapped in [ ].
        assert!(
            hits[0].snippet.contains("[FTS"),
            "unexpected snippet: {}",
            hits[0].snippet
        );

        let recent = db.get_recent_sessions(10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, "s-1");
        assert_eq!(recent[0].turn_count, 2);
    }

    #[test]
    fn test_index_session_is_an_idempotent_upsert() {
        let (db, _dir) = test_db();
        db.index_session(summary("s-1")).unwrap();
        let mut closed = summary("s-1");
        closed.turn_count = 7;
        closed.cost_usd = 0.42;
        closed.ended_at = "2026-09-09T10:20:00Z".to_string();
        db.index_session(closed).unwrap();

        let recent = db.get_recent_sessions(10);
        assert_eq!(recent.len(), 1, "upsert must not duplicate session rows");
        assert_eq!(recent[0].turn_count, 7);
        assert_eq!(recent[0].cost_usd, 0.42);
    }

    #[test]
    fn test_reupsert_with_child_messages_succeeds() {
        // Production sequence: the query loop stubs the session row, then
        // indexes messages, then stubs again on the next query, and the
        // close handler upserts final metadata. INSERT OR REPLACE would
        // fail every re-upsert here (FK on the implicit delete).
        let (db, _dir) = test_db();
        db.index_session(summary("s-1")).unwrap();
        db.index_message(
            "s-1",
            "user",
            "how does the FTS indexer work",
            "2026-09-09T10:00:10Z",
        )
        .unwrap();

        let mut again = summary("s-1");
        again.started_at = "2026-09-09T10:03:00Z".to_string();
        db.index_session(again).unwrap();

        let mut closed = summary("s-1");
        closed.turn_count = 9;
        closed.ended_at = "2026-09-09T10:20:00Z".to_string();
        db.index_session(closed).unwrap();

        let recent = db.get_recent_sessions(10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].turn_count, 9);
        // MIN(started_at) must keep the ORIGINAL stub start.
        assert_eq!(recent[0].started_at, "2026-09-09T10:00:00Z");

        // The indexed message must survive every re-upsert.
        let hits = db.search("indexer", 10);
        assert_eq!(hits.len(), 1, "messages must survive session upserts");
    }

    #[test]
    fn test_backfill_from_snapshots_imports_and_is_idempotent() {
        use serde_json::json;
        let dir = tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        // One valid historical snapshot (user + assistant turn).
        let snap = json!({
            "schema_version": 1,
            "session_id": "hist-1",
            "cwd": "/tmp/proj",
            "model": "test-model",
            "created_at": "2026-08-01T10:00:00Z",
            "last_active": "2026-08-01T10:05:00Z",
            "messages": [
                { "uuid": "u1", "timestamp": "2026-08-01T10:00:10Z",
                  "type": "user",
                  "message": { "role": "user", "content": "what is the backfill budget" },
                  "is_meta": false, "tool_use_result": null },
                { "uuid": "u2", "timestamp": "2026-08-01T10:00:11Z",
                  "type": "user",
                  "message": { "role": "user", "content": [{"type":"tool_result"}] },
                  "is_meta": true, "tool_use_result": null },
                { "uuid": "a1", "timestamp": "2026-08-01T10:00:12Z",
                  "type": "assistant",
                  "message": { "role": "assistant",
                               "content": [ { "type": "text", "text": "the budget is small" } ],
                               "stop_reason": "end_turn", "usage": null },
                  "cost_usd": 0.02, "duration_ms": 0 }
            ]
        });
        std::fs::write(
            sessions_dir.join("hist-1.json"),
            serde_json::to_string(&snap).unwrap(),
        )
        .unwrap();
        // Corrupt JSON and the registry index must both be skipped.
        std::fs::write(sessions_dir.join("broken.json"), "{not json").unwrap();
        std::fs::write(sessions_dir.join("registry.json"), "{}").unwrap();

        let (db, _dir) = test_db();
        let (s, m) = db.backfill_from_snapshots(&sessions_dir);
        assert_eq!((s, m), (1, 2), "meta user messages are not indexed");

        let hits = db.search("backfill budget", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "hist-1");

        let recent = db.get_recent_sessions(10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].turn_count, 1, "meta messages are not turns");
        assert_eq!(recent[0].cost_usd, 0.02);
        assert_eq!(recent[0].started_at, "2026-08-01T10:00:00Z");

        // Second run: already indexed — must not duplicate anything.
        let (s2, m2) = db.backfill_from_snapshots(&sessions_dir);
        assert_eq!((s2, m2), (0, 0));
        assert_eq!(db.get_session_messages("hist-1").len(), 2);
    }

    #[test]
    fn test_backfill_skips_sessions_that_were_live_indexed() {
        use serde_json::json;
        let dir = tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let snap = json!({
            "schema_version": 1,
            "session_id": "live-1",
            "cwd": "/tmp/proj",
            "model": "test-model",
            "created_at": "2026-08-01T10:00:00Z",
            "last_active": "2026-08-01T10:05:00Z",
            "messages": [
                { "uuid": "u1", "timestamp": "2026-08-01T10:00:10Z",
                  "type": "user",
                  "message": { "role": "user", "content": "snapshot copy" },
                  "is_meta": false, "tool_use_result": null }
            ]
        });
        std::fs::write(
            sessions_dir.join("live-1.json"),
            serde_json::to_string(&snap).unwrap(),
        )
        .unwrap();

        let (db, _dir) = test_db();
        // The live indexer already wrote this session (row + messages).
        db.index_session(summary("live-1")).unwrap();
        db.index_message("live-1", "user", "live copy", "2026-08-01T11:00:00Z")
            .unwrap();

        let (s, m) = db.backfill_from_snapshots(&sessions_dir);
        assert_eq!((s, m), (0, 0), "live-indexed sessions must be skipped");
        assert_eq!(db.get_session_messages("live-1").len(), 1);
        assert!(db.search("snapshot copy", 10).is_empty());
    }

    #[test]
    fn test_html_is_stripped_from_indexed_content() {
        let (db, _dir) = test_db();
        db.index_session(summary("s-1")).unwrap();
        db.index_message(
            "s-1",
            "user",
            "check <b>bold</b> tags",
            "2026-09-09T10:00:10Z",
        )
        .unwrap();
        let hits = db.search("\"check bold tags\"", 10);
        assert_eq!(hits.len(), 1, "stripped text must be searchable");
    }
}
