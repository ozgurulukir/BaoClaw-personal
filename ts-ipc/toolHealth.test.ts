import assert from "node:assert/strict";
import { describe, test } from "node:test";
import {
  formatToolHealth,
  type ToolHealthData,
  type ToolHealthRecordInfo,
} from "./toolHealth.js";

function record(
  overrides: Partial<ToolHealthRecordInfo>,
): ToolHealthRecordInfo {
  return {
    tool_name: "Bash",
    total_calls: 0,
    success_count: 0,
    failure_count: 0,
    timeout_count: 0,
    recent_failures: [],
    status: "Healthy",
    consecutive_failures: 0,
    last_status_change: "2026-09-08T12:00:00Z",
    ...overrides,
  };
}

function data(tools: ToolHealthRecordInfo[]): ToolHealthData {
  return {
    summary: {
      tracked: tools.length,
      healthy: tools.filter((t) => t.status === "Healthy").length,
      degraded: tools.filter((t) => t.status === "Degraded").length,
      disabled: tools.filter((t) => t.status === "Disabled").length,
      total_calls: tools.reduce((acc, t) => acc + t.total_calls, 0),
    },
    thresholds: { degrade: 3, disable: 6, recovery_minutes: 30 },
    tools,
  };
}

describe("formatToolHealth", () => {
  test("all healthy renders a one-line summary", () => {
    const out = formatToolHealth(
      data([
        record({ tool_name: "Read", total_calls: 10, success_count: 10 }),
        record({ tool_name: "Edit", total_calls: 5, success_count: 5 }),
      ]),
    );
    assert.equal(out, "✅ All 2 tracked tools healthy (15 calls).");
  });

  test("empty tracker explains there is nothing recorded", () => {
    assert.equal(formatToolHealth(data([])), "✅ No tool calls recorded yet.");
  });

  test("problem tools show status, rate, last failure and recovery ETA", () => {
    const now = Date.parse("2026-09-08T12:20:00Z");
    const out = formatToolHealth(
      data([
        record({
          tool_name: "Bash",
          status: "Disabled",
          total_calls: 6,
          failure_count: 6,
          consecutive_failures: 6,
          recent_failures: ["segfault"],
          last_status_change: "2026-09-08T12:10:00Z",
        }),
        record({
          tool_name: "WebSearch",
          status: "Degraded",
          total_calls: 12,
          failure_count: 5,
          consecutive_failures: 3,
          recent_failures: ["boom", "timeout fetching url"],
        }),
        record({ tool_name: "Read", total_calls: 4, success_count: 4 }),
      ]),
      { nowMs: now },
    );
    const lines = out.split("\n");
    assert.equal(lines.length, 6);
    // Disabled before Degraded (daemon sorts the payload).
    assert.match(
      lines[0],
      /^🚫 Bash — Disabled · 6 consecutive failures · 100% failure rate$/,
    );
    assert.equal(lines[1], "   last failure: segfault");
    assert.equal(lines[2], "   auto-recovery in ~20m"); // 30 - 10 minutes
    assert.match(
      lines[3],
      /^⚠️ WebSearch — Degraded · 3 consecutive failures · 42% failure rate$/,
    );
    assert.equal(lines[4], "   last failure: timeout fetching url");
    assert.equal(lines[5], "✅ 1/3 healthy · 22 calls total");
  });

  test("expired recovery window says the tool recovers on next call", () => {
    const now = Date.parse("2026-09-08T13:00:00Z");
    const out = formatToolHealth(
      data([
        record({
          tool_name: "Bash",
          status: "Disabled",
          total_calls: 6,
          failure_count: 6,
          consecutive_failures: 6,
          last_status_change: "2026-09-08T12:10:00Z",
        }),
      ]),
      { nowMs: now },
    );
    assert.ok(out.includes("recovers on next call"));
  });

  test("unparseable timestamps omit the recovery estimate", () => {
    const out = formatToolHealth(
      data([
        record({
          tool_name: "Bash",
          status: "Disabled",
          total_calls: 6,
          failure_count: 6,
          consecutive_failures: 6,
          last_status_change: "not-a-date",
        }),
      ]),
    );
    assert.ok(!out.includes("auto-recovery"));
    assert.ok(!out.includes("recovers on next call"));
  });

  test("verbose appends thresholds and the healthy-tool table", () => {
    const out = formatToolHealth(
      data([
        record({
          tool_name: "Read",
          total_calls: 10,
          success_count: 10,
          failure_count: 0,
        }),
        record({
          tool_name: "Edit",
          total_calls: 5,
          success_count: 4,
          failure_count: 1,
        }),
      ]),
      { verbose: true },
    );
    assert.ok(out.includes("thresholds: degrade 3 / disable 6 / recover 30m"));
    assert.ok(out.includes("• Read — 10 calls · 0 failures"));
    assert.ok(out.includes("• Edit — 5 calls · 1 failures"));
  });

  test("long single-line failure reasons are truncated", () => {
    const out = formatToolHealth(
      data([
        record({
          tool_name: "Bash",
          status: "Degraded",
          total_calls: 3,
          failure_count: 3,
          consecutive_failures: 3,
          recent_failures: ["x".repeat(500)],
        }),
      ]),
    );
    const lastLine = out.split("\n")[1];
    assert.ok(lastLine.length < 200);
    assert.ok(lastLine.endsWith("…"));
  });
});
