/**
 * Unit tests for the Telegram permission prompt state machine:
 * keyword parsing, supersede/expiry semantics, and cleanup.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  TelegramPermissionManager,
  buildPermissionKeyboard,
  formatPermissionRequest,
  parsePermissionReply,
} from "./permission.js";

test("parsePermissionReply keyword matrix", () => {
  const table: Array<[string, string | null]> = [
    ["y", "allow"],
    ["Yes", "allow"],
    ["  allow  ", "allow"],
    ["a", "allow_always"],
    ["ALWAYS", "allow_always"],
    ["n", "deny"],
    ["No", "deny"],
    ["deny", "deny"],
    ["maybe", null],
    ["", null],
    ["please allow", null],
  ];
  for (const [input, expected] of table) {
    assert.equal(parsePermissionReply(input), expected, `input: ${input}`);
  }
});

test("formatPermissionRequest includes tool name and escaped preview", () => {
  const text = formatPermissionRequest("Bash", '{"command":"echo <hi>"}');
  assert.match(text, /Bash/);
  assert.match(text, /&lt;hi&gt;/);
  assert.match(text, /60秒/);
});

test("buildPermissionKeyboard carries decision-only callback_data", () => {
  const kb = buildPermissionKeyboard() as {
    inline_keyboard: Array<Array<{ callback_data: string }>>;
  };
  const flat = kb.inline_keyboard.flat();
  assert.equal(flat.length, 3);
  for (const button of flat) {
    assert.ok(button.callback_data.length <= 64, "callback_data byte cap");
    assert.match(button.callback_data, /^perm:(allow|always|deny)$/);
  }
});

test("register stores pending; resolve returns and clears it", () => {
  const mgr = new TelegramPermissionManager();
  mgr.register(1, { tool_use_id: "tu_1", tool_name: "Bash" }, () => {});
  assert.equal(mgr.get(1)?.tool_use_id, "tu_1");
  const resolved = mgr.resolve(1);
  assert.equal(resolved?.tool_use_id, "tu_1");
  assert.equal(mgr.get(1), null);
  assert.equal(mgr.resolve(1), null);
});

test("supersede invokes onExpire with the OLD id and reason 'superseded'", () => {
  const mgr = new TelegramPermissionManager();
  const fired: Array<{ id: string; reason: string }> = [];
  // The gateway passes the same closure to every registration; the manager
  // invokes the NEW call's callback with the OLD request's id.
  const onExpire = (
    _chatId: number,
    toolUseId: string,
    reason: "timeout" | "superseded",
  ) => {
    fired.push({ id: toolUseId, reason });
  };

  mgr.register(1, { tool_use_id: "tu_old", tool_name: "Bash" }, onExpire, 50);
  mgr.register(1, { tool_use_id: "tu_new", tool_name: "Bash" }, onExpire, 50);

  assert.deepEqual(fired, [{ id: "tu_old", reason: "superseded" }]);
  assert.equal(mgr.get(1)?.tool_use_id, "tu_new");
  mgr.cleanup();
});

test("expiry invokes onExpire with reason 'timeout' and clears pending", async () => {
  const mgr = new TelegramPermissionManager();
  const fired: Array<{ id: string; reason: string }> = [];
  mgr.register(
    1,
    { tool_use_id: "tu_x", tool_name: "Bash" },
    (_c, toolUseId, reason) => {
      fired.push({ id: toolUseId, reason });
    },
    20,
  );
  await new Promise((r) => setTimeout(r, 60));
  assert.deepEqual(fired, [{ id: "tu_x", reason: "timeout" }]);
  assert.equal(mgr.get(1), null);
});

test("cleanup cancels pending timers so no expiry fires", async () => {
  const mgr = new TelegramPermissionManager();
  const fired: unknown[] = [];
  mgr.register(
    1,
    { tool_use_id: "tu_1", tool_name: "Bash" },
    (...args) => {
      fired.push(args);
    },
    20,
  );
  mgr.cleanup();
  await new Promise((r) => setTimeout(r, 60));
  assert.equal(fired.length, 0);
  assert.equal(mgr.get(1), null);
});
