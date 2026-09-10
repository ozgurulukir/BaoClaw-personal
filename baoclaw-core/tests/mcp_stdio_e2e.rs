//! End-to-end test of the MCP stdio client stack: discovery → connection
//! manager → tool bridges → dispatch through the real tool executor.
//! Uses a fake MCP server shell script (no network, no real MCP binaries).
#![cfg(unix)]

use std::sync::Arc;

use baoclaw_core::discovery::mcp_config::discover_mcp_servers_in;
use baoclaw_core::mcp::{ConnectionManager, McpLaunchConfig, MCP_TOOL_PREFIX};
use baoclaw_core::tools::executor::ToolUseRequest;
use baoclaw_core::tools::executor::{execute_tools, ToolExecutionResult};
use baoclaw_core::tools::trait_def::{ProgressSender, ToolContext};
use serde_json::{json, Value};

/// Minimal conforming fake server: handshake + one tool whose call echoes.
const ECHO_SERVER: &str = r#"#!/bin/sh
id_of() { printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(id_of "$line")
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=$(id_of "$line")
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo_tool","description":"Echoes text back","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      id=$(id_of "$line")
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echoed"}],"isError":false}}\n' "$id"
      ;;
  esac
done
exit 0
"#;

struct NoopProgress;
#[async_trait::async_trait]
impl ProgressSender for NoopProgress {
    async fn send_progress(&self, _tool_use_id: &str, _data: Value) {}
}

fn make_context() -> ToolContext {
    ToolContext {
        cwd: std::env::temp_dir(),
        model: "test".to_string(),
        abort_signal: Arc::new(tokio::sync::watch::channel(false).1),
        file_cache: None,
        tool_result_store: None,
        context_window: 200_000,
        auto_compact_threshold_ratio: 0.7,
    }
}

#[tokio::test]
async fn mcp_tool_survives_discovery_connect_and_dispatch() {
    let home = tempfile::TempDir::new().unwrap();
    let project = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join(".baoclaw")).unwrap();

    let server_script = home.path().join("echo-server.sh");
    std::fs::write(&server_script, ECHO_SERVER).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&server_script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let config = json!({
        "mcpServers": { "fake": { "command": server_script.to_str().unwrap() } }
    });
    std::fs::write(
        home.path().join(".baoclaw").join("mcp.json"),
        serde_json::to_string(&config).unwrap(),
    )
    .unwrap();

    // 1. Discovery over the injected (hermetic) roots.
    let servers = discover_mcp_servers_in(Some(home.path()), project.path()).await;
    assert_eq!(servers.len(), 1, "discovery must find the fake server");

    // 2. Connect + publish buckets into a registry.
    let manager = ConnectionManager::start_all(servers, McpLaunchConfig::default(), None).await;
    let registry = baoclaw_core::tools::registry::ToolRegistry::new(vec![]);
    manager.attach_registry(registry.clone());
    let published = manager.boot_and_register().await;
    assert_eq!(published, 1, "one server, one tool → one bridge");
    let bridges = registry.snapshot();
    assert_eq!(
        bridges[0].name(),
        format!("{MCP_TOOL_PREFIX}fake__echo_tool")
    );

    // 3. The bridge round-trips a call through the full MCP stack.
    let outcome = bridges[0]
        .call(json!({"text": "hello"}), &make_context(), &NoopProgress)
        .await
        .expect("bridge call succeeds");
    assert!(!outcome.is_error);
    assert_eq!(outcome.data, json!("echoed"));

    // 4. Dispatch finds the bridge by name; the fail-closed permission
    // default holds (Ask on a non-read-only tool denies in direct mode).
    let results: Vec<ToolExecutionResult> = execute_tools(
        &bridges,
        &[ToolUseRequest {
            id: "probe-1".to_string(),
            name: "mcp__fake__echo_tool".to_string(),
            input: json!({"text": "hello"}),
        }],
        &make_context(),
        &NoopProgress,
        None,
        None,
    )
    .await;
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    assert!(
        results[0]
            .output
            .as_str()
            .unwrap()
            .contains("Permission denied"),
        "MCP tools must fail closed without an interactive gate: {:?}",
        results[0].output
    );

    manager.shutdown_all().await;
}
