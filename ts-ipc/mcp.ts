/**
 * MCP server status types and plain-text formatting shared by /mcp
 * surfaces (Telegram consumes the formatter; the CLI keeps its ANSI
 * renderer and imports only the types; web/public/app.js is a plain
 * script and mirrors the state grouping by hand). Pure formatting —
 * the payload comes from the daemon's `listMcpServers` / `mcpRefresh`
 * RPCs; `mcpRefresh` replies with a PRE-refresh snapshot.
 */

/** Live connection state from the daemon's connection manager. */
export interface McpServerRuntime {
  /** ready | connecting | disconnected | failed | skipped, plus the
   *  synthesized requires_restart | disabled_by_config (no live slot). */
  state: string;
  tool_count?: number;
  restarts?: number;
  reason?: string;
}

export interface McpServerInfo {
  name: string;
  command?: string;
  args?: string[];
  server_type: string;
  url?: string;
  disabled: boolean;
  source: string;
  config_path: string;
  runtime?: McpServerRuntime;
}

/** Envelope of `listMcpServers`. */
export interface McpServerList {
  servers: McpServerInfo[];
  count: number;
}

/** Envelope of `mcpRefresh` — a narrower PRE-refresh snapshot that
 *  carries only the live runtime per server. */
export interface McpRefreshResult {
  servers: Array<{ name: string; runtime?: McpServerRuntime }>;
  count: number;
}

/** Characters of the command-line preview shown per server. */
const COMMAND_PREVIEW_CHARS = 60;

function statusGlyph(server: McpServerInfo): string {
  const state = server.runtime?.state;
  if (!state) return server.disabled ? "⚪" : "🟢"; // old daemon: static flag only
  if (state === "ready") return "🟢";
  if (state === "connecting") return "🟡";
  if (state === "failed" || state === "disconnected") return "🔴";
  return "⚪"; // skipped | requires_restart | disabled_by_config
}

/**
 * Render `listMcpServers` as plain text. State/tool-count/restart
 * semantics mirror the CLI renderer; zero counts are omitted.
 */
export function formatMcpServers(data: McpServerList): string {
  if (data.count === 0 || data.servers.length === 0) {
    return "No MCP servers configured.";
  }
  const lines = [`🌐 MCP Servers (${data.count})`];
  for (const srv of data.servers) {
    lines.push(
      `${statusGlyph(srv)} ${srv.name}  [${srv.server_type}] [${srv.source}]`,
    );
    if (srv.command) {
      const cmd = `${srv.command} ${(srv.args ?? []).join(" ")}`.trim();
      lines.push(
        `  ${srv.server_type}: ${
          cmd.length > COMMAND_PREVIEW_CHARS
            ? cmd.slice(0, COMMAND_PREVIEW_CHARS) + "…"
            : cmd
        }`,
      );
    } else if (srv.url) {
      lines.push(`  ${srv.server_type}: ${srv.url}`);
    }
    const rt = srv.runtime;
    if (rt) {
      const parts = [rt.state];
      if (rt.tool_count) parts.push(`${rt.tool_count} tools`);
      if (rt.restarts) parts.push(`${rt.restarts} restarts`);
      if (rt.reason) parts.push(rt.reason);
      lines.push(`  ${parts.join(" — ")}`);
    }
  }
  return lines.join("\n");
}
