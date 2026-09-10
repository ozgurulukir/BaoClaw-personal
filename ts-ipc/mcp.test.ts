import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { formatMcpServers, type McpServerInfo } from "./mcp.js";

/** Build a server entry with sensible defaults for tests. */
function server(overrides: Partial<McpServerInfo> = {}): McpServerInfo {
  return {
    name: "demo",
    server_type: "stdio",
    disabled: false,
    source: "project",
    config_path: "/tmp/mcp.json",
    ...overrides,
  };
}

describe("formatMcpServers", () => {
  test("empty list renders the configured-nothing message", () => {
    assert.equal(
      formatMcpServers({ servers: [], count: 0 }),
      "No MCP servers configured.",
    );
  });

  test("ready server shows state, tool count, and restarts", () => {
    const out = formatMcpServers({
      servers: [
        server({
          command: "demo-server",
          args: ["--port", "9"],
          runtime: { state: "ready", tool_count: 12, restarts: 1 },
        }),
      ],
      count: 1,
    });
    assert.match(out, /🌐 MCP Servers \(1\)/);
    assert.match(out, /🟢 demo  \[stdio\] \[project\]/);
    assert.match(out, /stdio: demo-server --port 9/);
    assert.match(out, /ready — 12 tools — 1 restarts/);
    assert.doesNotMatch(out, /undefined/);
  });

  test("zero counts and absent fields are omitted from the meta line", () => {
    const out = formatMcpServers({
      servers: [server({ runtime: { state: "ready", tool_count: 0 } })],
      count: 1,
    });
    assert.match(out, /  ready$/m);
    assert.doesNotMatch(out, /0 tools/);
    assert.doesNotMatch(out, /restarts/);
  });

  test("failed server carries its reason on the meta line", () => {
    const out = formatMcpServers({
      servers: [
        server({
          runtime: { state: "failed", reason: "spawn ENOENT" },
        }),
      ],
      count: 1,
    });
    assert.match(out, /🔴 demo/);
    assert.match(out, /failed — spawn ENOENT/);
  });

  test("connecting uses the in-progress glyph", () => {
    const out = formatMcpServers({
      servers: [server({ runtime: { state: "connecting" } })],
      count: 1,
    });
    assert.match(out, /🟡 demo/);
    assert.match(out, /  connecting$/m);
  });

  test("inactive states share the dim glyph", () => {
    for (const state of ["skipped", "requires_restart", "disabled_by_config"]) {
      const out = formatMcpServers({
        servers: [server({ runtime: { state } })],
        count: 1,
      });
      assert.match(out, /⚪ demo/, `state ${state} should render ⚪`);
    }
  });

  test("disconnected uses the failure glyph", () => {
    const out = formatMcpServers({
      servers: [server({ runtime: { state: "disconnected" } })],
      count: 1,
    });
    assert.match(out, /🔴 demo/);
  });

  test("url-based servers list their endpoint", () => {
    const out = formatMcpServers({
      servers: [
        server({
          command: undefined,
          server_type: "http",
          url: "http://127.0.0.1:3927/mcp",
          runtime: { state: "ready" },
        }),
      ],
      count: 1,
    });
    assert.match(out, /http: http:\/\/127\.0\.0\.1:3927\/mcp/);
    assert.doesNotMatch(out, /undefined/);
  });

  test("old-daemon payloads without runtime fall back to the disabled flag", () => {
    const enabled = formatMcpServers({
      servers: [server()],
      count: 1,
    });
    assert.match(enabled, /🟢 demo/);
    assert.doesNotMatch(enabled, /ready/);
    const disabled = formatMcpServers({
      servers: [server({ disabled: true })],
      count: 1,
    });
    assert.match(disabled, /⚪ demo/);
  });

  test("command previews longer than 60 chars are truncated", () => {
    const long = "demo-server --config " + "x".repeat(60);
    const out = formatMcpServers({
      servers: [
        server({ command: "demo-server", args: ["--config", "x".repeat(60)] }),
      ],
      count: 1,
    });
    assert.match(out, /…$/m);
    assert.ok(out.includes(long.slice(0, 60)));
  });
});
