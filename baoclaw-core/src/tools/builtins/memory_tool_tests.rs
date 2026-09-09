#[cfg(test)]
mod tests {
    use super::super::memory_tool::*;
    use crate::engine::memory::MemoryStore;
    use crate::tools::trait_def::*;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::tempdir;

    struct NoopProgress;
    #[async_trait::async_trait]
    impl ProgressSender for NoopProgress {
        async fn send_progress(&self, _id: &str, _data: serde_json::Value) {}
    }

    /// A tool pointed at a tempdir-backed store: nothing here may touch the
    /// user's real `~/.baoclaw/memory.jsonl`.
    fn make_tool(dir: &std::path::Path) -> (Arc<MemoryStore>, MemoryTool) {
        let store = Arc::new(MemoryStore::load_with_path(dir.join("memory.jsonl")));
        let tool = MemoryTool::new(Arc::clone(&store));
        (store, tool)
    }

    fn make_ctx(path: &std::path::Path) -> ToolContext {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        ToolContext {
            cwd: path.to_path_buf(),
            model: "test".into(),
            abort_signal: std::sync::Arc::new(rx),
            file_cache: None,
            tool_result_store: None,
            context_window: 100000,
            auto_compact_threshold_ratio: 0.8,
        }
    }

    #[tokio::test]
    async fn test_memory_tool_schema_and_name() {
        let dir = tempdir().unwrap();
        let (_store, tool) = make_tool(dir.path());
        assert_eq!(tool.name(), "MemoryTool");
        assert!(tool.aliases().contains(&"Memory"));
        assert!(!tool.prompt().is_empty());
        let schema = tool.input_schema();
        assert_eq!(schema.schema_type, "object");
        let props = schema.properties.unwrap();
        assert!(props.get("importance").is_some());
    }

    #[tokio::test]
    async fn test_memory_tool_call_valid() {
        let dir = tempdir().unwrap();
        let memory_file = dir.path().join("memory.jsonl");
        let (_store, tool) = make_tool(dir.path());
        let ctx = make_ctx(dir.path());
        let progress = NoopProgress;

        let input = json!({
            "content": "User prefers dark mode",
            "category": "preference"
        });

        let res = tool.call(input, &ctx, &progress).await;
        assert!(res.is_ok());
        let result = res.unwrap();
        assert!(!result.is_error);
        assert_eq!(result.data["category"], "preference");

        // The entry landed in the redirected file via the store, not in the
        // user's real ~/.baoclaw/memory.jsonl.
        let written = std::fs::read_to_string(&memory_file).unwrap();
        assert!(written.contains("User prefers dark mode"));
        // Store-routed writes carry the full schema (importance present).
        assert!(written.contains("importance"));
    }

    #[tokio::test]
    async fn test_memory_tool_call_missing_fields() {
        let dir = tempdir().unwrap();
        let (_store, tool) = make_tool(dir.path());
        let ctx = make_ctx(dir.path());
        let progress = NoopProgress;

        let res = tool.call(json!({}), &ctx, &progress).await;
        assert!(res.is_err());
        assert!(!dir.path().join("memory.jsonl").exists());
    }

    #[tokio::test]
    async fn test_memory_tool_rejects_credential_content() {
        let dir = tempdir().unwrap();
        let (_store, tool) = make_tool(dir.path());
        let ctx = make_ctx(dir.path());
        let progress = NoopProgress;

        let res = tool
            .call(
                json!({
                    "content": "The API key is sk-test0123456789abcdefghij for the deploy account",
                    "category": "fact"
                }),
                &ctx,
                &progress,
            )
            .await
            .unwrap();
        assert!(res.is_error);
        assert_eq!(res.data["saved"], false);
        // Nothing was persisted.
        assert!(!dir.path().join("memory.jsonl").exists());
    }

    #[tokio::test]
    async fn test_memory_tool_duplicate_save_is_idempotent() {
        let dir = tempdir().unwrap();
        let (_store, tool) = make_tool(dir.path());
        let ctx = make_ctx(dir.path());
        let progress = NoopProgress;
        let input = json!({ "content": "User prefers dark mode", "category": "preference" });

        tool.call(input.clone(), &ctx, &progress).await.unwrap();
        let second = tool.call(input, &ctx, &progress).await.unwrap();
        assert!(!second.is_error);
        assert_eq!(second.data["duplicate"], true);

        let written = std::fs::read_to_string(dir.path().join("memory.jsonl")).unwrap();
        assert_eq!(written.lines().count(), 1);
    }

    #[tokio::test]
    async fn test_memory_tool_importance_reaches_store() {
        let dir = tempdir().unwrap();
        let (store, tool) = make_tool(dir.path());
        let ctx = make_ctx(dir.path());
        let progress = NoopProgress;

        tool.call(
            json!({ "content": "Production DB is postgres", "category": "fact", "importance": 0.9 }),
            &ctx,
            &progress,
        )
        .await
        .unwrap();
        let list = store.list().await;
        assert_eq!(list.len(), 1);
        assert!((list[0].importance - 0.9).abs() < f64::EPSILON);
    }
}
