//! One remote MCP tool as a first-class engine tool.

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use crate::tools::trait_def::{
    JsonSchema, ProgressSender, Tool, ToolContext, ToolError, ToolResult,
};

use super::manager::{ConnectionManager, McpCallError};
use super::types::McpToolDef;

/// A single remote tool exposed through the engine's `Tool` trait. Bridges
/// are rebuilt and republished per server bucket whenever a slot connects or
/// its catalog refreshes; engines observe the new set at their next
/// snapshot.
pub struct McpToolBridge {
    manager: Arc<ConnectionManager>,
    /// Original server name (slot key in the manager).
    server: String,
    /// Original remote tool name, sent verbatim in tools/call.
    tool: McpToolDef,
    /// Registry name: `mcp__<server>__<tool>` (sanitized segments).
    registry_name: String,
    deferred: bool,
}

impl McpToolBridge {
    pub(crate) fn new(
        manager: Arc<ConnectionManager>,
        server: String,
        tool: McpToolDef,
        registry_name: String,
        deferred: bool,
    ) -> Self {
        Self {
            manager,
            server,
            tool,
            registry_name,
            deferred,
        }
    }

    /// First line of the remote description, without the prompt() boilerplate
    /// — this is what a deferred stub advertises.
    fn short_remote_description(&self) -> String {
        self.tool
            .description
            .as_deref()
            .unwrap_or("remote tool")
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    }
}

#[async_trait]
impl Tool for McpToolBridge {
    fn name(&self) -> &str {
        &self.registry_name
    }

    /// Passthrough of the remote inputSchema. `JsonSchema.properties` is an
    /// opaque Value so remote properties survive verbatim; schema keywords
    /// the shared struct cannot express (`additionalProperties`, `$schema`,
    /// `$defs`/`$ref` tables, ...) are dropped — the struct is shared with
    /// every builtin tool. Servers with `$ref`-heavy schemas degrade to
    /// unresolvable refs; prefer servers with inline schemas.
    fn input_schema(&self) -> JsonSchema {
        let s = &self.tool.input_schema;
        JsonSchema {
            schema_type: s
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("object")
                .to_string(),
            properties: s.get("properties").cloned(),
            required: s
                .get("required")
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
            description: None,
        }
    }

    // is_read_only / is_concurrency_safe / is_destructive / check_permissions
    // are DELIBERATELY left at the trait defaults (false / false / false /
    // Ask). A remote tool's side effects are unknowable and its own
    // "read-only" hints are untrusted — the conservative defaults mean
    // mutating calls prompt like Bash (whole-tool allow rules work because
    // the permission manager is name-generic) and run sequentially.

    fn prompt(&self) -> String {
        format!(
            "{}: {} (remote tool provided by the MCP server '{}'; arguments are forwarded to that server process)",
            self.registry_name,
            self.short_remote_description(),
            self.server
        )
    }

    fn is_deferred(&self) -> bool {
        self.deferred
    }

    fn short_description(&self) -> String {
        self.short_remote_description()
    }

    fn aborts_internally(&self) -> bool {
        true
    }

    async fn call(
        &self,
        input: Value,
        context: &ToolContext,
        _progress: &dyn ProgressSender,
    ) -> Result<ToolResult, ToolError> {
        match self
            .manager
            .call_tool_cancellable(
                &self.server,
                &self.tool.name,
                input,
                Some(context.abort_signal.as_ref().clone()),
            )
            .await
        {
            Ok(outcome) => Ok(ToolResult {
                data: outcome.data,
                is_error: outcome.is_error,
            }),
            // Remote rejection (bad arguments etc.) is a tool-level error the
            // model can correct from — an error RESULT, not a ToolError.
            Err(McpCallError::Remote { code, message }) => Ok(ToolResult {
                data: serde_json::json!({ "error": message, "code": code }),
                is_error: true,
            }),
            // Server down: a meaningful error result. The registered tools
            // may come back (or change) via reconnect/refresh; the model
            // should retry rather than assume the tool vanished.
            Err(McpCallError::Disconnected { state, reason }) => Ok(ToolResult {
                data: serde_json::json!({
                    "error": format!(
                        "MCP server '{}' is not connected (state: {}). It may reconnect automatically — retry shortly.",
                        self.server,
                        serde_json::to_value(&state).unwrap_or(Value::Null)
                    ),
                    "detail": reason,
                }),
                is_error: true,
            }),
            Err(McpCallError::Lost) => Ok(ToolResult {
                data: serde_json::json!({
                    "error": format!(
                        "Connection to MCP server '{}' was lost during the call; it will reconnect in the background.",
                        self.server
                    )
                }),
                is_error: true,
            }),
            // Same semantics as BashTool's timeout path.
            Err(McpCallError::Timeout(ms)) => Err(ToolError::Timeout(ms)),
            // Cancellation was requested and forwarded; parity with the
            // executor's abort path.
            Err(McpCallError::Cancelled) => Err(ToolError::Aborted),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct NoopProgress;
    #[async_trait]
    impl ProgressSender for NoopProgress {
        async fn send_progress(&self, _tool_use_id: &str, _data: Value) {}
    }

    fn bridge_with(schema: Value, description: Option<&str>) -> McpToolBridge {
        // The manager is only touched in call(); the managerless parts
        // (naming, schema, prompt) are testable without a live server.
        McpToolBridge::new(
            ConnectionManager::empty(),
            "srv".to_string(),
            McpToolDef {
                name: "tool one".to_string(),
                description: description.map(String::from),
                input_schema: schema,
            },
            super::super::composite_tool_name("srv", "tool one"),
            false,
        )
    }

    #[tokio::test]
    async fn schema_passthrough_preserves_properties_and_required() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}, "n": {"type": "number"}},
            "required": ["path"],
            "additionalProperties": false
        });
        let b = bridge_with(schema, Some("Does a thing"));
        let js = b.input_schema();
        assert_eq!(js.schema_type, "object");
        assert_eq!(
            js.properties.as_ref().unwrap()["path"]["type"],
            serde_json::json!("string")
        );
        assert_eq!(js.required, Some(vec!["path".to_string()]));
        // Keywords the shared struct cannot express are dropped.
        let full = serde_json::to_value(&js).unwrap();
        assert!(full.get("additionalProperties").is_none());
    }

    #[tokio::test]
    async fn schema_defaults_for_empty_remote_schema() {
        let b = bridge_with(serde_json::json!({}), None);
        let js = b.input_schema();
        assert_eq!(js.schema_type, "object");
        assert!(js.properties.is_none());
        assert!(js.required.is_none());
    }

    #[test]
    fn prompt_names_server_and_first_description_line() {
        let b = bridge_with(
            serde_json::json!({}),
            Some("Does a thing.\nLonger details here."),
        );
        let p = b.prompt();
        assert!(p.starts_with("mcp__srv__tool_one: Does a thing."));
        assert!(p.contains("MCP server 'srv'"));
        assert!(!p.contains("Longer details"));
    }

    #[tokio::test]
    async fn call_on_unknown_server_is_an_error_result_not_a_tool_error() {
        let b = bridge_with(serde_json::json!({}), None);
        let context = ToolContext {
            cwd: std::path::PathBuf::from("/tmp"),
            model: "test".to_string(),
            abort_signal: Arc::new(tokio::sync::watch::channel(false).1),
            file_cache: None,
            tool_result_store: None,
            context_window: 200_000,
            auto_compact_threshold_ratio: 0.7,
        };
        let progress = NoopProgress;
        let result = Tool::call(&b, serde_json::json!({}), &context, &progress)
            .await
            .expect("call returns a result");
        assert!(result.is_error);
        assert!(result.data["error"]
            .as_str()
            .unwrap()
            .contains("not connected"));
    }
}
