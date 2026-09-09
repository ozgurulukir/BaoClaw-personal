use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::engine::memory::{parse_category, MemoryStore};
use crate::tools::trait_def::*;

/// Tool that allows the AI to automatically save important information
/// to long-term memory (user preferences, facts, decisions).
///
/// Shares the daemon's long-lived [`MemoryStore`] instance, so saves are
/// immediately visible to the prompt fragment, MemorySearch and the IPC
/// control plane; the store itself rejects security-scan failures and
/// collapses exact duplicates.
pub struct MemoryTool {
    store: Arc<MemoryStore>,
}

impl MemoryTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "MemoryTool"
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["Memory", "SaveMemory"]
    }

    fn input_schema(&self) -> JsonSchema {
        JsonSchema {
            schema_type: "object".to_string(),
            properties: Some(json!({
                "content": { "type": "string", "description": "The information to remember, as a single concise declarative fact" },
                "category": {
                    "type": "string",
                    "enum": ["fact", "preference", "decision"],
                    "description": "Category: fact (user told me X), preference (user prefers Y), decision (we decided Z)"
                },
                "importance": {
                    "type": "number",
                    "description": "Optional salience from 0.0 to 1.0 (default 0.5). Higher-importance memories stay in the always-on prompt longer; use ~0.9 only for durable facts (project constraints, standing preferences)."
                }
            })),
            required: Some(vec!["content".to_string(), "category".to_string()]),
            description: Some(
                "Save important information to long-term memory that persists across sessions"
                    .to_string(),
            ),
        }
    }

    fn prompt(&self) -> String {
        "Save important information to long-term memory. Use this when you discover user preferences, \
         important facts, or decisions that should be remembered across conversations. \
         Categories: 'fact' for things the user told you, 'preference' for user preferences and habits, \
         'decision' for decisions made during conversations. Be concise — store the key information only, \
         as one declarative sentence per save (duplicates are ignored). Never store credentials or secrets. \
         To look up what you already remember, use MemorySearch instead of asking the user."
            .to_string()
    }

    async fn call(
        &self,
        input: Value,
        _context: &ToolContext,
        _progress: &dyn ProgressSender,
    ) -> Result<ToolResult, ToolError> {
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::ExecutionFailed("Missing 'content'".into()))?;
        let category = parse_category(
            input
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("fact"),
        );
        let importance = input
            .get("importance")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5)
            .clamp(0.0, 1.0);

        match self
            .store
            .add_with_importance(
                content.to_string(),
                category,
                "auto".to_string(),
                importance,
            )
            .await
        {
            Ok(outcome) => {
                if outcome.created {
                    Ok(ToolResult {
                        data: json!({
                            "saved": true,
                            "id": outcome.entry.id,
                            "content": content,
                            "category": outcome.entry.category.to_string(),
                            "importance": outcome.entry.importance,
                        }),
                        is_error: false,
                    })
                } else {
                    Ok(ToolResult {
                        data: json!({
                            "saved": true,
                            "duplicate": true,
                            "id": outcome.entry.id,
                            "content": content,
                            "category": outcome.entry.category.to_string(),
                            "note": "Already stored — no duplicate added."
                        }),
                        is_error: false,
                    })
                }
            }
            Err(e) => {
                // Distinguish "rejected before touching the store" (nothing
                // was saved) from "stored in memory but the disk write
                // failed" (the entry is live for this daemon session; the
                // model must not retry it as if it were lost).
                let is_rejected =
                    matches!(e, crate::engine::memory::store::MemoryError::Rejected(_));
                let data = if is_rejected {
                    json!({ "saved": false, "error": e.to_string() })
                } else {
                    json!({
                        "saved": true,
                        "persisted": false,
                        "error": e.to_string(),
                        "note": "Stored for this session, but writing to disk failed; it may not survive a restart. Do not retry the same save."
                    })
                };
                Ok(ToolResult {
                    data,
                    is_error: true,
                })
            }
        }
    }
}
