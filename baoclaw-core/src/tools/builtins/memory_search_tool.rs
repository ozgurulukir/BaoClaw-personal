use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::engine::memory::decay::DecayConfig;
use crate::engine::memory::MemoryStore;
use crate::tools::trait_def::*;

/// Default number of results returned per search.
const DEFAULT_LIMIT: usize = 5;
/// Weight of entry importance relative to the query-match ratio when scoring.
const IMPORTANCE_WEIGHT: f64 = 0.25;

/// Model-facing recall over the long-term memory store.
///
/// The prompt fragment only carries the highest-scoring entries within a
/// character budget; this tool is how the model reaches everything else.
/// Returned entries get a decay-module recall boost, which is the signal
/// that keeps frequently-used memories off the age-only archival path.
///
/// Matching is plain keyword/substring (no embeddings): deterministic,
/// free, and good enough for a store capped at `max_entries`.
pub struct MemorySearchTool {
    /// The daemon's long-lived store instance (tests point it at a
    /// tempdir-backed store).
    store: Arc<MemoryStore>,
}

impl MemorySearchTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

/// Lowercased, deduplicated query terms of length >= 2.
fn query_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = query
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .filter(|w| w.chars().count() >= 2)
        .collect();
    terms.sort();
    terms.dedup();
    if terms.is_empty() {
        let q = query.trim().to_lowercase();
        if !q.is_empty() {
            terms.push(q);
        }
    }
    terms
}

/// Score of an entry for the query, or None when nothing matches.
/// Match coverage dominates; importance breaks near-ties so a 1.0-importance
/// memory outranks a 0.2 one at equal coverage.
fn entry_score(content_lower: &str, terms: &[String], importance: f64) -> Option<f64> {
    let matched = terms
        .iter()
        .filter(|t| content_lower.contains(t.as_str()))
        .count();
    if matched == 0 {
        return None;
    }
    Some(matched as f64 / terms.len() as f64 + IMPORTANCE_WEIGHT * importance)
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "MemorySearch"
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["SearchMemory", "RecallMemory"]
    }

    fn input_schema(&self) -> JsonSchema {
        JsonSchema {
            schema_type: "object".to_string(),
            properties: Some(json!({
                "query": { "type": "string", "description": "Keywords to look for (names, paths, tools, error codes — matching is keyword-based, not semantic)" },
                "limit": { "type": "integer", "description": "Maximum results to return (1-20, default 5)" }
            })),
            required: Some(vec!["query".to_string()]),
            description: Some(
                "Search long-term memory for previously saved facts, preferences and decisions"
                    .to_string(),
            ),
        }
    }

    fn prompt(&self) -> String {
        "Search long-term memory (facts, preferences and decisions saved across sessions). \
         Use it when the prompt's long-term memory section does not cover what you need, and \
         before asking the user for context you may already have been told. Matching is \
         keyword-based: query with distinctive words, identifiers or error codes rather than \
         full sentences."
            .to_string()
    }

    // A pure lookup over the user's own memory store — same permission class
    // as Grep/Glob: no prompt, and callable from headless engines. (Its only
    // write is internal recall bookkeeping on that same store, like the read
    // tools' cache side effects.)
    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    async fn call(
        &self,
        input: Value,
        _context: &ToolContext,
        _progress: &dyn ProgressSender,
    ) -> Result<ToolResult, ToolError> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::ExecutionFailed("Missing 'query'".into()))?;
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| (v as usize).clamp(1, 20))
            .unwrap_or(DEFAULT_LIMIT);

        let terms = query_terms(query);
        if terms.is_empty() {
            return Ok(ToolResult {
                data: json!({
                    "results": [],
                    "count": 0,
                    "query": query,
                    "note": "Empty query. Pass the keywords to look for."
                }),
                is_error: false,
            });
        }

        let store = &self.store;
        let entries = store.list().await;
        let mut scored: Vec<(f64, &crate::engine::memory::MemoryEntry)> = entries
            .iter()
            .filter(|e| !e.archived)
            .filter_map(|e| {
                entry_score(&e.content.to_lowercase(), &terms, e.importance).map(|s| (s, e))
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        // Every returned entry counts as a recall: boosted importance and a
        // fresh last_recalled_at keep it ranking (and un-archived) longer.
        if !scored.is_empty() {
            let ids: Vec<String> = scored.iter().map(|(_, e)| e.id.clone()).collect();
            store.record_recall(&ids, &DecayConfig::load()).await;
        }

        let results: Vec<Value> = scored
            .iter()
            .map(|(_, e)| {
                json!({
                    "id": e.id,
                    "content": e.content,
                    "category": e.category.to_string(),
                    "importance": e.importance,
                    "recall_count": e.recall_count,
                    "created_at": e.created_at,
                })
            })
            .collect();

        let mut data = json!({ "results": results, "count": results.len(), "query": query });
        if results.is_empty() {
            data["note"] = json!(
                "No memories matched. Retry with fewer or rarer keywords, or exact literals \
                 (paths, identifiers, error codes). Nothing in memory is not the same as the \
                 user never having told you — long-term memory only holds what was saved."
            );
        }
        Ok(ToolResult {
            data,
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_terms_dedup_and_filter_short_words() {
        assert_eq!(
            query_terms("The DB config of the db"),
            vec![
                "config".to_string(),
                "db".to_string(),
                "of".to_string(),
                "the".to_string()
            ]
        );
        // Nothing survives term filtering → the whole query is kept verbatim
        // so short/literal queries still work.
        assert_eq!(query_terms("a & b"), vec!["a & b".to_string()]);
    }

    #[test]
    fn query_terms_falls_back_to_whole_query() {
        assert_eq!(query_terms("x"), vec!["x".to_string()]);
        assert_eq!(query_terms("  "), Vec::<String>::new());
    }

    #[test]
    fn entry_score_requires_a_match() {
        let terms = query_terms("postgres port");
        assert!(entry_score("redis runs on 6379", &terms, 1.0).is_none());
        let s = entry_score("postgres runs on port 5433", &terms, 0.5).unwrap();
        // Full coverage (1.0) plus importance share (0.125).
        assert!((s - 1.125).abs() < 1e-9);
    }

    struct NoopProgress;
    #[async_trait::async_trait]
    impl ProgressSender for NoopProgress {
        async fn send_progress(&self, _id: &str, _data: serde_json::Value) {}
    }

    fn make_ctx(dir: &std::path::Path) -> ToolContext {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        ToolContext {
            cwd: dir.to_path_buf(),
            model: "test".into(),
            abort_signal: std::sync::Arc::new(rx),
            file_cache: None,
            tool_result_store: None,
            context_window: 100000,
            auto_compact_threshold_ratio: 0.8,
        }
    }

    fn make_tool(dir: &std::path::Path) -> (Arc<MemoryStore>, MemorySearchTool) {
        let store = Arc::new(MemoryStore::load_with_path(dir.join("memory.jsonl")));
        let tool = MemorySearchTool::new(Arc::clone(&store));
        (store, tool)
    }

    #[tokio::test]
    async fn search_returns_ranked_matches_and_boosts_recall() {
        let dir = tempfile::tempdir().unwrap();
        let (store, tool) = make_tool(dir.path());
        let ctx = make_ctx(dir.path());
        let progress = NoopProgress;

        store
            .add_with_importance(
                "Production postgres runs on port 5433".to_string(),
                crate::engine::memory::store::MemoryCategory::Fact,
                "auto".to_string(),
                0.9,
            )
            .await
            .unwrap();
        store
            .add(
                "User prefers concise answers".to_string(),
                crate::engine::memory::store::MemoryCategory::Preference,
                "auto".to_string(),
            )
            .await
            .unwrap();
        // An archived (decayed-out) entry must never come back via search.
        store
            .seed_for_tests(crate::engine::memory::MemoryEntry {
                id: "archived1".to_string(),
                content: "postgres legacy staging port 5433".to_string(),
                category: crate::engine::memory::store::MemoryCategory::Fact,
                created_at: chrono::Utc::now().to_rfc3339(),
                source: "auto".to_string(),
                importance: 0.05,
                recall_count: 0,
                last_recalled_at: None,
                archived: true,
            })
            .await;

        let res = tool
            .call(json!({ "query": "postgres port" }), &ctx, &progress)
            .await
            .unwrap();
        assert!(!res.is_error);
        assert_eq!(res.data["count"], 1);
        assert_eq!(
            res.data["results"][0]["content"],
            "Production postgres runs on port 5433"
        );

        // The returned entry was recorded as recalled (importance + count).
        let list = store.list().await;
        let hit = list
            .iter()
            .find(|e| e.content.contains("postgres"))
            .unwrap();
        assert!((hit.importance - 1.0).abs() < f64::EPSILON);
        assert_eq!(hit.recall_count, 1);
    }

    #[test]
    fn memory_search_is_read_only_and_concurrency_safe() {
        let dir = tempfile::tempdir().unwrap();
        let (_store, tool) = make_tool(dir.path());
        assert!(tool.is_read_only(&json!({ "query": "x" })));
        assert!(tool.is_concurrency_safe(&json!({ "query": "x" })));
    }

    #[tokio::test]
    async fn search_zero_results_carries_retry_guidance() {
        let dir = tempfile::tempdir().unwrap();
        let (_store, tool) = make_tool(dir.path());
        let ctx = make_ctx(dir.path());
        let progress = NoopProgress;

        let res = tool
            .call(json!({ "query": "kubernetes ingress" }), &ctx, &progress)
            .await
            .unwrap();
        assert!(!res.is_error);
        assert_eq!(res.data["count"], 0);
        assert!(res.data["note"]
            .as_str()
            .unwrap()
            .contains("No memories matched"));
    }
}
