/**
 * Tool health formatting shared by every /health surface (Telegram,
 * WhatsApp, Feishu, web, TUI, and the one-shot CLI). Pure formatting —
 * the payload comes from the daemon's `toolHealth` RPC.
 */

export interface ToolHealthRecordInfo {
  tool_name: string;
  total_calls: number;
  success_count: number;
  failure_count: number;
  timeout_count: number;
  recent_failures: string[];
  status: "Healthy" | "Degraded" | "Disabled";
  consecutive_failures: number;
  last_status_change: string;
}

export interface ToolHealthData {
  summary: {
    tracked: number;
    healthy: number;
    degraded: number;
    disabled: number;
    total_calls: number;
  };
  thresholds: {
    degrade: number;
    disable: number;
    recovery_minutes: number;
  };
  tools: ToolHealthRecordInfo[];
}

export interface FormatToolHealthOptions {
  /** Include every tracked tool, not just problem tools. */
  verbose?: boolean;
  /** Now (ms epoch) — injectable for deterministic tests. */
  nowMs?: number;
}

/** Characters of a recent failure reason shown per tool. */
const FAILURE_PREVIEW_CHARS = 120;

function failurePct(tool: ToolHealthRecordInfo): string {
  if (tool.total_calls === 0) return "0%";
  return `${Math.round((tool.failure_count / tool.total_calls) * 100)}%`;
}

function lastFailure(tool: ToolHealthRecordInfo): string | undefined {
  return tool.recent_failures[tool.recent_failures.length - 1];
}

function truncate(text: string): string {
  const oneLine = text.replace(/\s+/g, " ").trim();
  const chars = [...oneLine];
  if (chars.length <= FAILURE_PREVIEW_CHARS) return oneLine;
  return chars.slice(0, FAILURE_PREVIEW_CHARS).join("") + "…";
}

function minutesSince(iso: string, nowMs: number): number | null {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return null;
  return Math.max(0, Math.floor((nowMs - t) / 60_000));
}

function formatDuration(mins: number): string {
  if (mins < 60) return `${mins}m`;
  const h = Math.floor(mins / 60);
  const m = mins % 60;
  return m ? `${h}h ${m}m` : `${h}h`;
}

/**
 * Render the daemon's tool-health payload as plain text lines.
 * Default view: one-line summary when everything is healthy, otherwise
 * Disabled/Degraded details with failure rates, the last failure reason,
 * and an auto-recovery estimate. `verbose` appends the healthy-tool table
 * and the active thresholds.
 */
export function formatToolHealth(
  data: ToolHealthData,
  options: FormatToolHealthOptions = {},
): string {
  const nowMs = options.nowMs ?? Date.now();
  const { summary, thresholds, tools } = data;
  const lines: string[] = [];

  if (summary.tracked === 0) {
    return "✅ No tool calls recorded yet.";
  }

  const problem = tools.filter((t) => t.status !== "Healthy");
  const calls = summary.total_calls;
  const callsLabel = `${calls} call${calls === 1 ? "" : "s"}`;
  if (problem.length === 0) {
    lines.push(
      `✅ All ${summary.tracked} tracked tool${summary.tracked === 1 ? "" : "s"} healthy (${callsLabel}).`,
    );
  } else {
    for (const tool of tools) {
      if (tool.status === "Healthy") continue;
      const icon = tool.status === "Disabled" ? "🚫" : "⚠️";
      lines.push(
        `${icon} ${tool.tool_name} — ${tool.status} · ${tool.consecutive_failures} consecutive failures · ${failurePct(tool)} failure rate`,
      );
      const last = lastFailure(tool);
      if (last) lines.push(`   last failure: ${truncate(last)}`);
      if (tool.status === "Disabled") {
        const since = minutesSince(tool.last_status_change, nowMs);
        if (since !== null) {
          const remaining = thresholds.recovery_minutes - since;
          lines.push(
            remaining > 0
              ? `   auto-recovery in ~${formatDuration(remaining)}`
              : "   recovers on next call",
          );
        }
      }
    }
    lines.push(
      `✅ ${summary.healthy}/${summary.tracked} healthy · ${callsLabel} total`,
    );
  }

  if (options.verbose) {
    lines.push(
      `   thresholds: degrade ${thresholds.degrade} / disable ${thresholds.disable} / recover ${thresholds.recovery_minutes}m`,
    );
    for (const tool of tools) {
      if (tool.status !== "Healthy") continue;
      lines.push(
        `   • ${tool.tool_name} — ${tool.total_calls} calls · ${tool.failure_count} failures`,
      );
    }
  }

  return lines.join("\n");
}
