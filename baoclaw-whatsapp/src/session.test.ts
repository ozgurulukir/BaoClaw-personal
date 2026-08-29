import { strict as assert } from "node:assert";
import test from "node:test";
import { retryDelayMs, SessionManager } from "./session.js";

test("retry delay grows exponentially and stays bounded", () => {
  assert.equal(retryDelayMs(1), 3_000);
  assert.equal(retryDelayMs(2), 6_000);
  assert.equal(retryDelayMs(5), 30_000);
  assert.equal(retryDelayMs(100), 30_000);
});

test("disconnect is idempotent before initialization", async () => {
  const session = new SessionManager();

  assert.equal(session.getLifecycleState(), "idle");
  await session.disconnect();
  await session.disconnect();

  assert.equal(session.isConnected(), false);
  assert.equal(session.getSocket(), null);
  assert.equal(session.getLifecycleState(), "stopping");
});
