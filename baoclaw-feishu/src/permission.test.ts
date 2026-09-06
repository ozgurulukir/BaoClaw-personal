/**
 * Unit tests for the Feishu permission request state machine:
 * keyword parsing, supersede/expiry semantics, decision forwarding.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  PermissionManager,
  buildPermissionCard,
  formatPermissionRequest,
  parseCardAction,
  parseCardActionValue,
  parsePermissionReply,
} from "./permission.js";

/** Tiny fake of the control channel's request surface. */
function fakeControl() {
  const calls: Array<{ method: string; params: any }> = [];
  return {
    calls,
    async request(method: string, params?: unknown) {
      calls.push({ method, params });
      return { delivered: true };
    },
  };
}

test("parsePermissionReply keyword matrix", () => {
  const table: Array<[string, string | null]> = [
    ["yes", "allow"],
    ["y", "allow"],
    [" allow ", "allow"],
    ["always", "allow_always"],
    ["a", "allow_always"],
    ["no", "deny"],
    ["deny", "deny"],
    ["n", "deny"],
    ["ok", null],
    ["", null],
  ];
  for (const [input, expected] of table) {
    assert.equal(parsePermissionReply(input), expected, `input: ${input}`);
  }
});

test("formatPermissionRequest lists all three reply keywords", () => {
  const text = formatPermissionRequest("Bash", '{"command":"ls"}');
  assert.match(text, /权限请求/);
  assert.match(text, /Bash/);
  assert.match(text, /yes/);
  assert.match(text, /always/);
  assert.match(text, /no/);
  assert.match(text, /60秒/);
});

test("handleResponse forwards allow with control channel and clears pending", async () => {
  const pm = new PermissionManager();
  const control = fakeControl();
  pm.registerRequest("chat1", "tu_1", "Bash", () => {});

  const reply = await pm.handleResponse("chat1", "YES", control);
  assert.deepEqual(reply, { decision: "allow", delivered: true });
  assert.equal(control.calls.length, 1);
  assert.equal(control.calls[0].method, "permissionResponse");
  assert.equal(control.calls[0].params.tool_use_id, "tu_1");
  assert.equal(control.calls[0].params.decision, "allow");
  assert.equal(pm.getPending("chat1"), null);
});

test("'always' forwards allow_always with rule set to the tool name", async () => {
  const pm = new PermissionManager();
  const control = fakeControl();
  pm.registerRequest("chat1", "tu_2", "Bash", () => {});

  const reply = await pm.handleResponse("chat1", "always", control);
  assert.equal(reply?.decision, "allow_always");
  assert.equal(control.calls[0].params.decision, "allow_always");
  assert.equal(control.calls[0].params.rule, "Bash");
});

test("non-keyword text with pending request returns null and keeps it open", async () => {
  const pm = new PermissionManager();
  const control = fakeControl();
  pm.registerRequest("chat1", "tu_3", "Bash", () => {});

  assert.equal(
    await pm.handleResponse("chat1", "please run it", control),
    null,
  );
  assert.equal(control.calls.length, 0);
  assert.equal(pm.getPending("chat1")?.tool_use_id, "tu_3");
  pm.cleanup();
});

test("supersede invokes onExpire with the OLD id and reason 'superseded'", () => {
  const pm = new PermissionManager();
  const fired: Array<{ id: string; reason: string }> = [];
  // The gateway passes the same closure to every registration; the manager
  // invokes the NEW call's callback with the OLD request's id.
  const onExpire = (
    _chatId: string,
    toolUseId: string,
    reason: "timeout" | "superseded",
  ) => {
    fired.push({ id: toolUseId, reason });
  };

  pm.registerRequest("chat1", "tu_old", "Bash", onExpire, 50);
  pm.registerRequest("chat1", "tu_new", "Bash", onExpire, 50);

  assert.deepEqual(fired, [{ id: "tu_old", reason: "superseded" }]);
  assert.equal(pm.getPending("chat1")?.tool_use_id, "tu_new");
  pm.cleanup();
});

test("expiry invokes onExpire with reason 'timeout' and clears pending", async () => {
  const pm = new PermissionManager();
  const fired: Array<{ id: string; reason: string }> = [];
  pm.registerRequest(
    "chat1",
    "tu_x",
    "Bash",
    (_c, id, reason) => {
      fired.push({ id, reason });
    },
    20,
  );

  await new Promise((r) => setTimeout(r, 60));
  assert.deepEqual(fired, [{ id: "tu_x", reason: "timeout" }]);
  assert.equal(pm.getPending("chat1"), null);
});

test("cleanup cancels pending timers so no expiry fires", async () => {
  const pm = new PermissionManager();
  const fired: unknown[] = [];
  pm.registerRequest(
    "chat1",
    "tu_1",
    "Bash",
    (...args: any[]) => {
      fired.push(args);
    },
    20,
  );
  pm.cleanup();
  await new Promise((r) => setTimeout(r, 60));
  assert.equal(fired.length, 0);
  assert.equal(pm.getPending("chat1"), null);
});

test("buildPermissionCard carries three decision-only buttons", () => {
  const card = buildPermissionCard("Bash", '{"command":"ls"}') as {
    header: { title: { content: string } };
    elements: Array<any>;
  };
  assert.match(card.header.title.content, /权限请求/);
  const action = card.elements.find((e) => e.tag === "action");
  const buttons = action.actions as Array<{ value: { perm_action: string } }>;
  assert.deepEqual(buttons.map((b) => b.value.perm_action).sort(), [
    "allow",
    "always",
    "deny",
  ]);
});

test("parseCardAction reads value objects and raw strings", () => {
  assert.equal(parseCardActionValue({ perm_action: "allow" }), "allow");
  assert.equal(parseCardActionValue({ perm_action: "always" }), "allow_always");
  assert.equal(parseCardActionValue({ perm_action: "deny" }), "deny");
  assert.equal(parseCardActionValue("deny"), "deny");
  assert.equal(parseCardActionValue({ other: 1 }), null);
  assert.equal(parseCardActionValue(undefined), null);
});

test("parseCardAction extracts chat id and decision from event wrappers", () => {
  const flat = parseCardAction({
    open_chat_id: "oc_a",
    action: { value: { perm_action: "allow" } },
  });
  assert.deepEqual(flat, { chatId: "oc_a", decision: "allow" });

  const wrapped = parseCardAction({
    event: {
      context: { open_chat_id: "oc_b" },
      action: { value: { perm_action: "deny" } },
    },
  });
  assert.deepEqual(wrapped, { chatId: "oc_b", decision: "deny" });

  assert.equal(parseCardAction({ unrelated: true }), null);
  assert.equal(parseCardAction("garbage"), null);
});

test("resolvePending resolves without keyword parsing and reports delivery", async () => {
  const pm = new PermissionManager();
  const control = fakeControl();
  pm.registerRequest("chat1", "tu_9", "Bash", () => {});

  const reply = await pm.resolvePending("chat1", "allow_always", control);
  assert.deepEqual(reply, { decision: "allow_always", delivered: true });
  assert.equal(control.calls[0].params.rule, "Bash");
  assert.equal(pm.getPending("chat1"), null);

  // Nothing pending → null.
  assert.equal(await pm.resolvePending("chat1", "allow", control), null);
});
