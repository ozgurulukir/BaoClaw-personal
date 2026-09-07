/**
 * Unit tests for the permission request state machine (PermissionManager):
 * supersede semantics, auto-expiry, keyword parsing, and cleanup.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { PermissionManager } from "./permission.js";
import { SenderTracker } from "./senderTracker.js";

/** Tiny fake of the IPC surface used for permissionResponse forwarding. */
function fakeClient() {
  const calls: Array<{ method: string; params: any }> = [];
  return {
    calls,
    async request(method: string, params: any) {
      calls.push({ method, params });
      return { delivered: true };
    },
  };
}

function makeManager() {
  const tracker = new SenderTracker();
  tracker.registerSender("+15550000001", "user@s.whatsapp.net", false);
  return { tracker, pm: new PermissionManager(tracker) };
}

test("registerRequest stores pending permission and formats prompt", () => {
  const { pm, tracker } = makeManager();
  const text = pm.formatPermissionRequest("tu_1", "bash", "ls -la");
  assert.match(text, /Permission Request/);
  assert.match(text, /bash/);

  pm.registerRequest("+15550000001", "tu_1", "bash", "ls -la", () => {});
  assert.ok(tracker.getPendingPermission("+15550000001"));
  pm.cleanup();
});

test("supersede fires onTimeout with reason 'superseded' for the old id", () => {
  const { pm, tracker } = makeManager();
  const fired: Array<{ id: string; reason: string }> = [];
  // The gateway passes the same closure to every registration; the manager
  // invokes the NEW call's callback with the OLD request's id.
  const onExpire = (
    _p: string,
    id: string,
    reason: "timeout" | "superseded",
  ) => {
    fired.push({ id, reason });
  };

  pm.registerRequest("+15550000001", "tu_old", "bash", "", onExpire, 50);
  pm.registerRequest("+15550000001", "tu_new", "bash", "", onExpire, 50);

  assert.deepEqual(fired, [{ id: "tu_old", reason: "superseded" }]);
  assert.equal(
    tracker.getPendingPermission("+15550000001")?.tool_use_id,
    "tu_new",
  );
  // Supersede also marks the sender as recently resolved, so a late keyword
  // meant for the OLD prompt is acked instead of reaching the new state.
  assert.equal(pm.isRecentlyResolved("+15550000001"), true);
  pm.cleanup();
});

test("expiry fires onTimeout with reason 'timeout' and clears pending", async () => {
  const { pm, tracker } = makeManager();
  const fired: Array<{ id: string; reason: string }> = [];

  pm.registerRequest(
    "+15550000001",
    "tu_x",
    "bash",
    "",
    (_p, id, reason) => {
      fired.push({ id, reason });
    },
    20,
  );

  await new Promise((r) => setTimeout(r, 60));
  assert.deepEqual(fired, [{ id: "tu_x", reason: "timeout" }]);
  assert.equal(tracker.getPendingPermission("+15550000001"), null);
});

test("handleResponse forwards allow/deny via permissionResponse and clears state", async () => {
  const { pm, tracker } = makeManager();
  const client = fakeClient();

  pm.registerRequest("+15550000001", "tu_1", "bash", "", () => {}, 60_000);

  const allowed = await pm.handleResponse(
    "+15550000001",
    "  YES ",
    client as any,
  );
  assert.deepEqual(allowed, { decision: "allow", delivered: true });
  assert.equal(client.calls.length, 1);
  assert.equal(client.calls[0].method, "permissionResponse");
  assert.equal(client.calls[0].params.tool_use_id, "tu_1");
  assert.equal(client.calls[0].params.decision, "allow");
  assert.equal(tracker.getPendingPermission("+15550000001"), null);

  // New request, denied by keyword.
  pm.registerRequest("+15550000001", "tu_2", "bash", "", () => {}, 60_000);
  const denied = await pm.handleResponse("+15550000001", "no", client as any);
  assert.deepEqual(denied, { decision: "deny", delivered: true });

  pm.cleanup();
});

test("handleResponse forwards allow_always with the tool-name rule", async () => {
  const { pm, tracker } = makeManager();
  const client = fakeClient();

  pm.registerRequest("+15550000001", "tu_3", "bash", "", () => {}, 60_000);
  const always = await pm.handleResponse(
    "+15550000001",
    "always",
    client as any,
  );
  assert.deepEqual(always, { decision: "allow_always", delivered: true });
  assert.equal(client.calls[0].params.decision, "allow_always");
  // Whole-tool rule, mirroring the other gateways.
  assert.equal(client.calls[0].params.rule, "bash");
  assert.equal(tracker.getPendingPermission("+15550000001"), null);
  pm.cleanup();
});

test("handleResponse reports delivered=false when the daemon moved on", async () => {
  const { pm, tracker } = makeManager();
  const client = {
    calls: [] as Array<{ method: string; params: any }>,
    async request(method: string, params: any) {
      this.calls.push({ method, params });
      return { delivered: false }; // gate already resolved elsewhere
    },
  };

  pm.registerRequest("+15550000001", "tu_1", "bash", "", () => {}, 60_000);
  const result = await pm.handleResponse("+15550000001", "yes", client as any);
  assert.deepEqual(result, { decision: "allow", delivered: false });
  assert.equal(tracker.getPendingPermission("+15550000001"), null);
});

test("handleResponse with non-keyword text keeps the request pending", async () => {
  const { pm, tracker } = makeManager();
  const client = fakeClient();

  pm.registerRequest("+15550000001", "tu_1", "bash", "", () => {}, 60_000);
  assert.equal(
    await pm.handleResponse("+15550000001", "run it please", client as any),
    null,
  );
  assert.equal(client.calls.length, 0);
  assert.equal(
    tracker.getPendingPermission("+15550000001")?.tool_use_id,
    "tu_1",
  );

  pm.cleanup();
});

test("handleResponse without a pending request is a no-op", async () => {
  const { pm } = makeManager();
  const client = fakeClient();
  assert.equal(
    await pm.handleResponse("+15550000001", "yes", client as any),
    null,
  );
  assert.equal(client.calls.length, 0);
});

test("cleanup cancels pending timers so no timeout fires", async () => {
  const { pm } = makeManager();
  const fired: unknown[] = [];
  pm.registerRequest(
    "+15550000001",
    "tu_1",
    "bash",
    "",
    (...args) => {
      fired.push(args);
    },
    20,
  );
  pm.cleanup();
  await new Promise((r) => setTimeout(r, 60));
  // Timers never fire after cleanup (tracker state is left as-is by design;
  // shutdown follows immediately anyway).
  assert.equal(fired.length, 0);
});

test("handleResponse returns 'late' for a keyword right after timeout", async () => {
  const { pm, tracker } = makeManager();
  const client = fakeClient();

  pm.registerRequest("+15550000001", "tu_1", "bash", "", () => {}, 20);
  await new Promise((r) => setTimeout(r, 60)); // expiry fires
  assert.equal(tracker.getPendingPermission("+15550000001"), null);

  // Keyword with nothing pending but recently resolved → late ack, and no
  // permissionResponse is forwarded to the daemon.
  assert.equal(
    await pm.handleResponse("+15550000001", "yes", client as any),
    "late",
  );
  assert.equal(client.calls.length, 0);

  // Non-keyword text is never late — falls through as normal chat.
  assert.equal(
    await pm.handleResponse("+15550000001", "please continue", client as any),
    null,
  );

  // isRecentlyResolved flips off once the grace window elapses.
  assert.equal(pm.isRecentlyResolved("+15550000001", 20), false);
  assert.equal(pm.isRecentlyResolved("+15550000001", 60_000), true);
  pm.cleanup();
});
