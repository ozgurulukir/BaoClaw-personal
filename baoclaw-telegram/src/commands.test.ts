import { test, describe } from "node:test";
import assert from "node:assert/strict";
import {
  COMMAND_REGISTRY,
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

describe("/search contract", () => {
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
    const { createCommandHandlers } = await import("./handlers.js");
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
    const handlers = createCommandHandlers({
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
    const out = await handlers["/search"]("probe", 1);
    assert.equal(ipc.calls[0].method, "searchHistory");
    assert.deepEqual((ipc.calls[0].params as { query: string }).query, "probe");
    assert.match(out!, /indexed probe content/);
    assert.doesNotMatch(out!, /undefined/);
  });
});
