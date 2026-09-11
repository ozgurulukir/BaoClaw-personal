import { describe, it } from "node:test";
import * as assert from "node:assert/strict";
import {
  BasePermissionManager,
  parsePermissionReply,
  formatPermissionRequest,
  LATE_PERMISSION_ACK,
} from "./permissionBridge.js";
import { markdownFormatter } from "./formatters/markdown.js";

describe("Gateway PermissionBridge", () => {
  it("parsePermissionReply normalizes keywords correctly", () => {
    assert.equal(parsePermissionReply("yes"), "allow");
    assert.equal(parsePermissionReply("Y"), "allow");
    assert.equal(parsePermissionReply("allow"), "allow");
    assert.equal(parsePermissionReply("always"), "allow_always");
    assert.equal(parsePermissionReply("a"), "allow_always");
    assert.equal(parsePermissionReply("no"), "deny");
    assert.equal(parsePermissionReply("N"), "deny");
    assert.equal(parsePermissionReply("deny"), "deny");
    assert.equal(parsePermissionReply("something else"), null);
    assert.equal(parsePermissionReply(""), null);
  });

  it("formatPermissionRequest formats readable prompt with targetPath", () => {
    const text = formatPermissionRequest(
      "bash",
      "rm -rf /tmp/test",
      120,
      "/etc/hosts",
      markdownFormatter,
    );
    assert.match(text, /Permission Request/);
    assert.match(text, /Tool:/);
    assert.match(text, /bash/);
    assert.match(text, /\/etc\/hosts/);
    assert.match(text, /120s/);
  });

  it("handles standard allow / deny lifecycle with RPC target", async () => {
    const calls: Array<{ method: string; params: any }> = [];
    const target = {
      async request(method: string, params?: unknown) {
        calls.push({ method, params });
        return { delivered: true };
      },
    };

    const pm = new BasePermissionManager<string>();
    pm.registerPending(
      "chat-123",
      { tool_use_id: "tu-1", tool_name: "bash" },
      () => {},
      10,
    );

    assert.equal(pm.getPending("chat-123")?.tool_use_id, "tu-1");

    const res = await pm.handleResponse("chat-123", "yes", target);
    assert.deepEqual(res, { decision: "allow", delivered: true });
    assert.equal(pm.getPending("chat-123"), null);
    assert.equal(calls.length, 1);
    assert.equal(calls[0].method, "permissionResponse");
    assert.equal(calls[0].params.decision, "allow");

    // Late reply immediately after resolution triggers grace period
    const lateRes = await pm.handleResponse("chat-123", "yes", target);
    assert.equal(lateRes, "late");
    pm.cleanup();
  });

  it("supersedes previous request when new one arrives", () => {
    const events: string[] = [];
    const pm = new BasePermissionManager<string>();
    pm.registerPending(
      "chat-1",
      { tool_use_id: "tu-old", tool_name: "bash" },
      (_k, id, reason) => events.push(`${id}:${reason}`),
      10,
    );

    pm.registerPending(
      "chat-1",
      { tool_use_id: "tu-new", tool_name: "git" },
      (_k, id, reason) => events.push(`${id}:${reason}`),
      10,
    );

    assert.deepEqual(events, ["tu-old:superseded"]);
    assert.equal(pm.getPending("chat-1")?.tool_use_id, "tu-new");
    pm.cleanup();
  });
});
