import { test, describe, mock } from "node:test";
import assert from "node:assert/strict";
import {
  COMMAND_REGISTRY,
  formatHelp,
  formatSearchResults,
  isRegisteredCommand,
  parseCommand,
} from "./commands.js";
import { isAllowedChat } from "./authorization.js";

describe("Telegram command authorization boundary", () => {
  test("parses only registered slash commands", () => {
    assert.deepEqual(parseCommand("/status now"), {
      command: "/status",
      args: "now",
    });
    assert.equal(isRegisteredCommand("/status"), true);
    assert.equal(isRegisteredCommand("/status; rm -rf /"), false);
    assert.equal(isRegisteredCommand("status"), false);
  });

  test("allows configured chats and rejects unauthorized ids", () => {
    assert.equal(isAllowedChat(42, [42]), true);
    assert.equal(isAllowedChat(7, [42]), false);
    assert.equal(isAllowedChat(Number.NaN, [42]), false);
  });
});

describe("Telegram /health command", () => {
  test("is registered with a bot-menu description", () => {
    assert.equal(isRegisteredCommand("/health"), true);
    assert.match(COMMAND_REGISTRY["/health"].description, /\/health \[all\]/);
  });
});

/** Minimal IpcClient double: records calls, replies by method. */
function mockIpcClient(respond: (method: string) => unknown) {
  const calls: Array<{ method: string; params: unknown }> = [];
  return {
    calls,
    connected: true,
    async request<T>(method: string, params?: unknown): Promise<T> {
      calls.push({ method, params });
      return respond(method) as T;
    },
  };
}

async function buildHandlers(ipc: ReturnType<typeof mockIpcClient>) {
  const { createCommandHandlers } = await import("./handlers.js");
  return createCommandHandlers({
    ipcClient: ipc as never,
    control: {},
    daemonInfo: {
      pid: 1,
      session_id: "s",
      cwd: "/tmp",
      startTime: 0,
      logFile: "",
      name: "t",
    },
    botUsername: "testbot",
    sessionState: { resumed: false, messageCount: 0, sessionId: "s" },
    daemonConnector: {},
    sendDocument: async () => {},
    quitGateway: () => {},
  } as never);
}

describe("Telegram /mcp command", () => {
  test("is registered with the refresh usage", () => {
    assert.equal(isRegisteredCommand("/mcp"), true);
    assert.match(COMMAND_REGISTRY["/mcp"].usage!, /refresh \[server\]/);
  });

  test("menu descriptions fit the setMyCommands 32-char cap", () => {
    for (const [cmd, def] of Object.entries(COMMAND_REGISTRY)) {
      const len = [...def.description].length;
      assert.ok(
        len >= 1 && len <= 32,
        `${cmd} description is ${len} chars (Telegram allows 1-32): "${def.description}"`,
      );
    }
  });

  test("/help renders the richer usage line when present", () => {
    const help = formatHelp(COMMAND_REGISTRY);
    assert.match(help, /MCP servers: \/mcp \[refresh \[server\]\]/);
    assert.match(help, /Export conversation as Markdown or PDF/);
    assert.match(help, /Show help/); // description fallback when no usage
  });

  const readyList = {
    servers: [
      {
        name: "demo",
        server_type: "stdio",
        disabled: false,
        source: "project",
        config_path: "/tmp/mcp.json",
        runtime: { state: "ready", tool_count: 3 },
      },
    ],
    count: 1,
  };

  test("handler lists live runtime state via listMcpServers", async () => {
    const ipc = mockIpcClient(() => readyList);
    const handlers = await buildHandlers(ipc);
    const out = await handlers["/mcp"]("", 1);
    assert.equal(ipc.calls[0].method, "listMcpServers");
    assert.equal(ipc.calls[0].params, undefined);
    assert.match(out!, /🟢 demo/);
    assert.match(out!, /ready — 3 tools/);
    assert.doesNotMatch(out!, /undefined/);
  });

  test("refresh kicks mcpRefresh then re-lists the settled state", async () => {
    mock.timers.enable({ apis: ["setTimeout"] });
    try {
      const ipc = mockIpcClient((method) =>
        method === "mcpRefresh" ? { servers: [], count: 0 } : readyList,
      );
      const handlers = await buildHandlers(ipc);
      const pending = handlers["/mcp"]("refresh my server", 1);
      // Let the handler reach its settle timer before advancing mocked time.
      await new Promise((r) => setImmediate(r));
      mock.timers.tick(2000);
      const out = await pending;
      assert.equal(ipc.calls[0].method, "mcpRefresh");
      assert.deepEqual(ipc.calls[0].params, { server: "my server" });
      assert.equal(ipc.calls[1].method, "listMcpServers");
      assert.match(out!, /ready — 3 tools/);
    } finally {
      mock.timers.reset();
    }
  });

  test("refresh without a target refreshes every server", async () => {
    mock.timers.enable({ apis: ["setTimeout"] });
    try {
      const ipc = mockIpcClient((method) =>
        method === "mcpRefresh" ? { servers: [], count: 0 } : readyList,
      );
      const handlers = await buildHandlers(ipc);
      const pending = handlers["/mcp"]("refresh", 1);
      // Let the handler reach its settle timer before advancing mocked time.
      await new Promise((r) => setImmediate(r));
      mock.timers.tick(2000);
      await pending;
      assert.deepEqual(ipc.calls[0].params, { server: null });
    } finally {
      mock.timers.reset();
    }
  });

  test("unknown subcommands return usage without an RPC", async () => {
    const ipc = mockIpcClient(() => readyList);
    const handlers = await buildHandlers(ipc);
    const out = await handlers["/mcp"]("bogus", 1);
    assert.match(out!, /usage: \/mcp \[refresh \[server\]\]/);
    assert.equal(ipc.calls.length, 0);
  });
});

describe("Telegram /rate command", () => {
  test("handler sends evolution.rateTrajectory with the rating", async () => {
    const ipc = mockIpcClient(() => ({}));
    const handlers = await buildHandlers(ipc);
    const out = await handlers["/rate"]("Good", 1);
    assert.equal(ipc.calls[0].method, "evolution.rateTrajectory");
    assert.deepEqual(ipc.calls[0].params, { rating: "good" });
    assert.match(out!, /good/);
  });

  test("invalid ratings show usage without an RPC", async () => {
    const ipc = mockIpcClient(() => ({}));
    const handlers = await buildHandlers(ipc);
    const out = await handlers["/rate"]("meh", 1);
    assert.match(out!, /Usage: \/rate/);
    assert.equal(ipc.calls.length, 0);
  });
});

describe("/search contract", () => {
  test("formatters use the daemon shapes (no entry_type/context)", () => {
    const rows = [
      {
        snippet: "the indexer writes to FTS5",
        timestamp: "2026-09-09T10:00:12Z",
        session_id: "s-1",
        cwd: "/tmp/proj",
      },
      {
        role: "user",
        snippet: "fallback hello",
        timestamp: "2026-09-09T10:00:00Z",
      },
    ];
    const out = formatSearchResults(rows, "FTS");
    // DB-path hits carry no role → no label; fallback rows do.
    assert.match(out, /\[2026-09-09 10:00:12\]\nthe indexer writes to FTS5/);
    assert.match(out, /\[2026-09-09 10:00:00\] 👤 User/);
    assert.match(out, /fallback hello/);
    assert.doesNotMatch(out, /undefined/);
    assert.match(
      formatSearchResults([], "zzz"),
      /No matching results found for "zzz"/,
    );
  });

  test("handler unwraps the {results} envelope via the dispatch table", async () => {
    const ipc = mockIpcClient(() => ({
      results: [
        {
          snippet: "indexed probe content",
          timestamp: "2026-09-09T10:00:12Z",
          session_id: "s-9",
          cwd: "/tmp/proj",
        },
      ],
      count: 1,
    }));
    const handlers = await buildHandlers(ipc);
    const out = await handlers["/search"]("probe", 1);
    assert.equal(ipc.calls[0].method, "searchHistory");
    assert.deepEqual((ipc.calls[0].params as { query: string }).query, "probe");
    assert.match(out!, /indexed probe content/);
    assert.doesNotMatch(out!, /undefined/);
  });
});
