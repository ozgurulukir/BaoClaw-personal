//! Live engine tool catalog.
//!
//! Canonical order of every snapshot: builtin head (fixed at boot) → MCP
//! buckets (one per server, servers in sorted-name order) → pinned tail
//! (AgentTool, then ToolSearchTool). Consumers snapshot at their unit of
//! work — per query for the long-lived session engine, per construction for
//! sub-agents/tasks/teams/cron — so a mid-flight catalog refresh never
//! splits a query's tool array (prompt-cache prefix); it is observed at the
//! next snapshot. Each refresh costs one full-price prefix re-read per
//! session that adopts it.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::Tool;

pub type ToolRegistryHandle = Arc<ToolRegistry>;

/// Where a long-lived component gets its tool list: a boot-frozen Vec or the
/// live registry. `.resolve()` at the component's unit of work (per task, per
/// team, per job) makes registry refreshes visible without restarts.
#[derive(Clone)]
pub enum ToolSource {
    Static(Vec<Arc<dyn Tool>>),
    Registry(ToolRegistryHandle),
}

impl ToolSource {
    pub fn resolve(&self) -> Vec<Arc<dyn Tool>> {
        match self {
            ToolSource::Static(tools) => tools.clone(),
            ToolSource::Registry(registry) => registry.snapshot(),
        }
    }

    /// Resolve excluding named tools (the AgentTool/ToolSearchTool
    /// self-exclusion dance).
    pub fn resolve_without(&self, names: &[&str]) -> Vec<Arc<dyn Tool>> {
        match self {
            ToolSource::Static(tools) => tools
                .iter()
                .filter(|t| !names.contains(&t.name()))
                .cloned()
                .collect(),
            ToolSource::Registry(registry) => registry.snapshot_without(names),
        }
    }
}

pub struct ToolRegistry {
    /// Builtins, frozen at boot.
    head: Vec<Arc<dyn Tool>>,
    /// Pinned tail tools, installed once before the first snapshot.
    tail: std::sync::Mutex<Option<Vec<Arc<dyn Tool>>>>,
    /// One bucket per MCP server, keyed by server name (BTreeMap → sorted,
    /// deterministic snapshot order).
    buckets: std::sync::Mutex<BTreeMap<String, Vec<Arc<dyn Tool>>>>,
}

impl ToolRegistry {
    /// Create a registry over the builtin head.
    pub fn new(head: Vec<Arc<dyn Tool>>) -> ToolRegistryHandle {
        Arc::new(Self {
            head,
            tail: std::sync::Mutex::new(None),
            buckets: std::sync::Mutex::new(BTreeMap::new()),
        })
    }

    /// Install the pinned tail (AgentTool, ToolSearchTool). Called ONCE,
    /// before any snapshot is taken (boot is single-threaded).
    pub fn install_tail(&self, tail: Vec<Arc<dyn Tool>>) {
        let mut slot = self.tail.lock().unwrap();
        assert!(
            slot.is_none(),
            "ToolRegistry tail installed twice (boot-order contract)"
        );
        *slot = Some(tail);
    }

    /// Atomically replace one server's tool bucket and republish.
    pub fn replace_server_tools(&self, server: &str, tools: Vec<Arc<dyn Tool>>) {
        self.buckets
            .lock()
            .unwrap()
            .insert(server.to_string(), tools);
    }

    /// Full catalog: head + buckets + tail.
    pub fn snapshot(&self) -> Vec<Arc<dyn Tool>> {
        let mut out = self.head.clone();
        for bucket in self.buckets.lock().unwrap().values() {
            out.extend(bucket.iter().cloned());
        }
        if let Some(tail) = self.tail.lock().unwrap().as_ref() {
            out.extend(tail.iter().cloned());
        }
        out
    }

    /// Snapshot excluding the named tools. AgentTool's sub-agent catalog
    /// excludes both tail tools; ToolSearchTool excludes only itself (the
    /// clone-before-push dance this replaces).
    pub fn snapshot_without(&self, names: &[&str]) -> Vec<Arc<dyn Tool>> {
        self.snapshot()
            .into_iter()
            .filter(|t| !names.contains(&t.name()))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.snapshot().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::trait_def::{JsonSchema, ProgressSender, ToolContext, ToolError, ToolResult};
    use serde_json::{json, Value};

    struct Fake {
        name: &'static str,
    }

    #[async_trait::async_trait]
    impl Tool for Fake {
        fn name(&self) -> &str {
            self.name
        }
        fn input_schema(&self) -> JsonSchema {
            JsonSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
                description: None,
            }
        }
        fn prompt(&self) -> String {
            format!("tool {}", self.name)
        }
        async fn call(
            &self,
            _input: Value,
            _context: &ToolContext,
            _progress: &dyn ProgressSender,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult {
                data: json!({}),
                is_error: false,
            })
        }
    }

    fn tool(name: &'static str) -> Arc<dyn Tool> {
        Arc::new(Fake { name })
    }

    #[test]
    fn canonical_order_head_buckets_tail() {
        let registry = ToolRegistry::new(vec![tool("A"), tool("B")]);
        registry.install_tail(vec![tool("ZAgent"), tool("ZSearch")]);

        registry.replace_server_tools("srv-b", vec![tool("mcp__srv-b__x")]);
        registry.replace_server_tools("srv-a", vec![tool("mcp__srv-a__y")]);

        let snapshot = registry.snapshot();
        let names: Vec<&str> = snapshot.iter().map(|t| t.name()).collect();
        // Buckets sort by server name; tail comes last.
        assert_eq!(
            names,
            vec![
                "A",
                "B",
                "mcp__srv-a__y",
                "mcp__srv-b__x",
                "ZAgent",
                "ZSearch"
            ]
        );
    }

    #[test]
    fn snapshot_without_excludes_named_tools() {
        let registry = ToolRegistry::new(vec![tool("A")]);
        registry.install_tail(vec![tool("AgentTool"), tool("ToolSearchTool")]);
        let agent_snapshot = registry.snapshot_without(&["AgentTool", "ToolSearchTool"]);
        let agent_view: Vec<&str> = agent_snapshot.iter().map(|t| t.name()).collect();

        assert_eq!(agent_view, vec!["A"]);
        let search_snapshot = registry.snapshot_without(&["ToolSearchTool"]);
        let search_view: Vec<&str> = search_snapshot.iter().map(|t| t.name()).collect();
        assert_eq!(search_view, vec!["A", "AgentTool"]);
    }

    #[test]
    fn replace_and_remove_are_visible_on_next_snapshot() {
        let registry = ToolRegistry::new(vec![]);
        registry.replace_server_tools("srv", vec![tool("old")]);
        assert_eq!(registry.snapshot()[0].name(), "old");
        registry.replace_server_tools("srv", vec![tool("new")]);
        assert_eq!(registry.snapshot()[0].name(), "new");
    }

    #[test]
    fn empty_registry_is_empty() {
        let registry = ToolRegistry::new(vec![]);
        assert!(registry.is_empty());
    }
}
