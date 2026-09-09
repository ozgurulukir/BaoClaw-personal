/** Contract tests for the feishu /search /export /spec /tasks commands
 * against the daemon's current RPC response shapes. */
import test from "node:test";
import assert from "node:assert/strict";
import {
  dispatchCommand,
  parseCommand,
  type CommandContext,
} from "./commands.js";

/** Minimal IpcClient double: records (method, params) calls, replies by method. */
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

function makeCtx(args: string, ipcClient: unknown): CommandContext {
  return {
    ipcClient,
    control: {} as CommandContext["control"],
    args,
    sender: "tester",
    chatId: "test-chat",
    sendReply: async () => {},
  } as unknown as CommandContext;
}

async function dispatch(
  text: string,
  ipcClient: unknown,
): Promise<string | null> {
  const parsed = parseCommand(text);
  assert.ok(parsed, `parseCommand failed for ${text}`);
  return dispatchCommand(parsed, makeCtx(text, ipcClient));
}

test("/search unwraps the {results} envelope and renders rows", async () => {
  const ipc = mockIpcClient(() => ({
    results: [
      {
        snippet: "the indexer writes to SQLite FTS5",
        timestamp: "2026-09-09T10:00:12Z",
        session_id: "s-1",
        cwd: "/tmp/proj",
      },
    ],
    count: 1,
    query: "FTS",
  }));
  const out = await dispatch("/search FTS", ipc);
  assert.equal(ipc.calls[0].method, "searchHistory");
  assert.deepEqual(ipc.calls[0].params, { query: "FTS" });
  assert.match(out!, /FTS5/);
  assert.match(out!, /s-1|2026-09-09/);
  assert.doesNotMatch(out!, /No matches found/);
});

test("/search renders the active-session fallback shape (role/text)", async () => {
  const ipc = mockIpcClient(() => ({
    results: [
      {
        role: "user",
        text: "hello there",
        snippet: "hello",
        timestamp: "2026-09-09T10:00:00Z",
      },
    ],
    count: 1,
  }));
  const out = await dispatch("/search hello", ipc);
  assert.match(out!, /hello/);
  assert.match(out!, /👤/);
});

test("/search with no hits says so", async () => {
  const ipc = mockIpcClient(() => ({ results: [], count: 0, query: "zzz" }));
  const out = await dispatch("/search zzz", ipc);
  assert.match(out!, /No matches found/);
});

test("/export reads file_path/size_bytes/message_count (not path/size)", async () => {
  const ipc = mockIpcClient(() => ({
    file_path: "/tmp/export.md",
    message_count: 42,
    size_bytes: 10240,
  }));
  const out = await dispatch("/export", ipc);
  assert.match(out!, /\/tmp\/export\.md/);
  assert.match(out!, /42/);
  assert.match(out!, /10\.0 KB/);
  assert.doesNotMatch(out!, /undefined/);
});

test("/spec new sends feature_name plus flag tokens", async () => {
  const ipc = mockIpcClient(() => ({
    status: "created",
    feature_name: "auth",
    config: { workflow: "design_first", phase: "design" },
  }));
  const out = await dispatch("/spec new auth design bugfix", ipc);
  assert.equal(ipc.calls[0].method, "specNew");
  assert.deepEqual(ipc.calls[0].params, {
    feature_name: "auth",
    workflow: "design",
    spec_type: "bugfix",
  });
  assert.match(out!, /auth/);
  assert.match(out!, /design_first/);
});

test("/spec show and status send feature_name (not name)", async () => {
  const ipc = mockIpcClient((m) =>
    m === "specShow"
      ? {
          feature_name: "auth",
          workflow: "requirements_first",
          phase: "requirements",
          spec_type: "feature",
          task_progress: { total: 3, completed: 1, in_progress: 1 },
        }
      : { total: 3, completed: 1, in_progress: 1 },
  );
  await dispatch("/spec show auth", ipc);
  assert.deepEqual(ipc.calls[0].params, { feature_name: "auth" });
  const out2 = await dispatch("/spec status auth", ipc);
  assert.deepEqual(ipc.calls[1].params, { feature_name: "auth" });
  assert.match(out2!, /Completed: 1/);
});

test("/spec run sends feature_name and renders the pending task description", async () => {
  const ipc = mockIpcClient(() => ({
    status: "ready",
    task_id: "t-2",
    task_description: "Implement the login handler",
  }));
  const out = await dispatch("/spec run auth", ipc);
  assert.deepEqual(ipc.calls[0].params, { feature_name: "auth" });
  assert.match(out!, /t-2/);
  assert.match(out!, /Implement the login handler/);
});

test("/tasks renders Failed enum objects from the daemon", async () => {
  const ipc = mockIpcClient(() => ({
    tasks: [
      { id: "a", description: "ok one", status: "Completed" },
      { id: "b", description: "bad one", status: { Failed: "boom" } },
    ],
  }));
  const out = await dispatch("/tasks", ipc);
  assert.match(out!, /Completed/);
  assert.match(out!, /Failed: boom/);
  assert.doesNotMatch(out!, /\[object Object\]/);
});

test("/history sends count and unwraps the {messages} envelope (text field)", async () => {
  const ipc = mockIpcClient(() => ({
    messages: [
      {
        role: "user",
        text: "fix the login flow",
        timestamp: "2026-09-09T10:00:00Z",
      },
    ],
    count: 1,
    total: 9,
  }));
  const out = await dispatch("/history", ipc);
  assert.equal(ipc.calls[0].method, "talkTail");
  assert.deepEqual(ipc.calls[0].params, { count: 10 });
  assert.match(out!, /fix the login flow/);
  assert.doesNotMatch(out!, /undefined/);
});

test("/task create sends description AND prompt, reports task_id", async () => {
  const ipc = mockIpcClient(() => ({ task_id: "t-77" }));
  const out = await dispatch("/task scan the logs", ipc);
  assert.equal(ipc.calls[0].method, "taskCreate");
  assert.deepEqual(ipc.calls[0].params, {
    description: "scan the logs",
    prompt: "scan the logs",
  });
  assert.match(out!, /t-77/);
  assert.doesNotMatch(out!, /undefined/);
});

test("/task_stop sends task_id and honors the stopped flag", async () => {
  const ipc = mockIpcClient(() => ({ stopped: false }));
  const out = await dispatch("/task_stop t-9", ipc);
  assert.equal(ipc.calls[0].method, "taskStop");
  assert.deepEqual(ipc.calls[0].params, { task_id: "t-9" });
  assert.match(out!, /not running or not found/);
});
