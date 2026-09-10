//! MCP (Model Context Protocol) client support.
//!
//! The daemon spawns configured stdio MCP servers at boot, fetches their tool
//! catalogs, and registers each remote tool as a first-class engine tool.
//! The catalog is FROZEN after boot: reconnects revive a connection but never
//! add or remove registered tools, which keeps the engine's static tool-set
//! property (and with it the prompt-cache prefix) intact.

mod bridge;
pub mod client;
pub(crate) mod demux;
mod http;
pub mod manager;
mod sse;
mod transport;
mod types;

pub use client::McpError;
pub use manager::{
    ConnectionManager, McpCallError, McpLaunchConfig, ServerRuntimeState, ServerStatus,
};
pub use types::CallToolOutcome;

/// Protocol version for the Streamable HTTP transport (the revision that
/// introduced it). stdio and legacy SSE keep [`MCP_PROTOCOL_VERSION`].
pub const MCP_PROTOCOL_VERSION_HTTP: &str = "2025-03-26";

/// Protocol version advertised in the initialize handshake.
///
/// The original stable protocol revision: every conforming server must accept
/// it, while many reject unknown newer revisions, so we never send a newer
/// string. The server's response version is logged, never validated —
/// incompatibilities surface as failed requests, not boot failures.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Client identifier sent in the initialize handshake.
pub const MCP_CLIENT_NAME: &str = "baoclaw-core";

/// Reconnect backoff bounds (bounded exponential: 1s, 2s, 4s, ... capped).
pub const MCP_BACKOFF_INITIAL_MS: u64 = 1_000;
pub const MCP_BACKOFF_MAX_MS: u64 = 60_000;

/// Hard cap on tools/list pagination pages: a server that always returns
/// `nextCursor` must not loop forever.
pub const MCP_MAX_LIST_PAGES: usize = 100;

/// Registry prefix for MCP tools: `mcp__<server>__<tool>`.
pub const MCP_TOOL_PREFIX: &str = "mcp__";

/// Map a server/tool name to a registry-safe segment: only
/// `[A-Za-z0-9_-]` survive; anything else (including the `__` separator
/// characters) becomes `_`. Empty input yields the fallback so the composite
/// name is never malformed.
pub(crate) fn sanitize_segment(raw: &str, fallback: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

/// Registry name for a remote tool: `mcp__<server>__<tool>` with both
/// segments sanitized.
pub(crate) fn composite_tool_name(server: &str, tool: &str) -> String {
    format!(
        "{}{}__{}",
        MCP_TOOL_PREFIX,
        sanitize_segment(server, "server"),
        sanitize_segment(tool, "tool")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_safe_characters() {
        assert_eq!(sanitize_segment("fs", "x"), "fs");
        assert_eq!(sanitize_segment("my-server_1", "x"), "my-server_1");
    }

    #[test]
    fn sanitize_replaces_unsafe_characters() {
        assert_eq!(sanitize_segment("my server", "x"), "my_server");
        assert_eq!(sanitize_segment("do.thing/2", "x"), "do_thing_2");
        assert_eq!(sanitize_segment("工具", "x"), "__");
    }

    #[test]
    fn sanitize_empty_falls_back() {
        assert_eq!(sanitize_segment("", "server"), "server");
        // Non-empty input is always sanitized in place, never replaced.
        assert_eq!(sanitize_segment("///", "tool"), "___");
    }

    #[test]
    fn composite_name_assembles_and_sanitizes() {
        assert_eq!(composite_tool_name("fs", "read_file"), "mcp__fs__read_file");
        assert_eq!(
            composite_tool_name("my server", "do.thing"),
            "mcp__my_server__do_thing"
        );
    }
}
