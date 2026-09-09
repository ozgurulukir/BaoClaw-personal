/** Contract tests for the automation command family (/task, /tasks,
 * /task_stop, /history) against the daemon's taskCreate/taskStop/taskList/
 * talkTail RPC shapes. */
import test from "node:test";
import assert from "node:assert/strict";
import { dispatchCommand, type CommandContext } from "./commands.js";

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
    jid: "test@s.whatsapp.net",
    sock: {},
  } as unknown as CommandContext;
}

test("/task create sends description AND prompt (daemon requires both) and reports task_id", async () => {
  const ipc = mockIpcClient((m) =>
    m === "taskCreate" ? { task_id: "t-123" } : {},
  );
  const out = await dispatchCommand(makeCtx("/task analyze the logs", ipc));
  assert.equal(ipc.calls[0].method, "taskCreate");
  assert.deepEqual(ipc.calls[0].params, {
    description: "analyze the logs",
    prompt: "analyze the logs",
  });
  assert.match(out!, /t-123/);
  assert.doesNotMatch(out!, /undefined/);
});

test("/task stop <id> maps to taskStop with task_id param", async () => {
  const ipc = mockIpcClient(() => ({ stopped: true }));
  const out = await dispatchCommand(makeCtx("/task stop t-9", ipc));
  assert.equal(ipc.calls[0].method, "taskStop");
  assert.deepEqual(ipc.calls[0].params, { task_id: "t-9" });
  assert.match(out!, /Stopped/);
});

test("/task stop <id> reports when daemon says not stopped", async () => {
  const ipc = mockIpcClient(() => ({ stopped: false }));
  const out = await dispatchCommand(makeCtx("/task stop gone", ipc));
  assert.match(out!, /not running or not found/);
});

test("/task with multi-word text starting with 'stop' still creates (not a stop)", async () => {
  const ipc = mockIpcClient((m) =>
    m === "taskCreate" ? { task_id: "t-2" } : {},
  );
  const out = await dispatchCommand(
    makeCtx("/task stop watching error logs", ipc),
  );
  assert.equal(ipc.calls[0].method, "taskCreate");
  assert.match(out!, /t-2/);
});

test("bare '/task stop' shows usage instead of creating a junk task", async () => {
  const ipc = mockIpcClient(() => ({}));
  const out = await dispatchCommand(makeCtx("/task stop", ipc));
  assert.match(out!, /Usage/);
  assert.equal(ipc.calls.length, 0);
});

test("/task_stop alias uses the same taskStop contract", async () => {
  const ipc = mockIpcClient(() => ({ stopped: true }));
  const out = await dispatchCommand(makeCtx("/task_stop t-7", ipc));
  assert.equal(ipc.calls[0].method, "taskStop");
  assert.deepEqual(ipc.calls[0].params, { task_id: "t-7" });
  assert.match(out!, /Stopped/);
});

test("/tasks renders Failed enum objects from the daemon", async () => {
  const ipc = mockIpcClient(() => ({
    tasks: [
      { id: "a", description: "ok one", status: "Completed" },
      { id: "b", description: "bad one", status: { Failed: "boom" } },
    ],
  }));
  const out = await dispatchCommand(makeCtx("/tasks", ipc));
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
  const out = await dispatchCommand(makeCtx("/history", ipc));
  assert.equal(ipc.calls[0].method, "talkTail");
  assert.deepEqual(ipc.calls[0].params, { count: 10 });
  assert.match(out!, /fix the login flow/);
  assert.doesNotMatch(out!, /undefined/);
});

test("/task with no args shows usage, no RPC", async () => {
  const ipc = mockIpcClient(() => ({}));
  const out = await dispatchCommand(makeCtx("/task", ipc));
  assert.match(out!, /Usage/);
  assert.equal(ipc.calls.length, 0);
});
