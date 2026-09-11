/**
 * Unified Permission Bridge for multi-channel gateways.
 *
 * Implements the standard 3-keyword (allow / always / deny) state machine,
 * auto-expiry windows, supersede handling, and late-reply grace period.
 */

import type { ChannelFormatter } from "./formatters/index.js";
import { plainFormatter } from "./formatters/plain.js";

export type PermissionDecision = "allow" | "allow_always" | "deny";

export interface PendingPermissionRequest {
  tool_use_id: string;
  tool_name: string;
  input_preview?: string;
  target_path?: string;
  message_id?: number;
}

export interface PermissionRpcTarget {
  request(method: string, params?: unknown): Promise<any>;
}

export const PERMISSION_TIMEOUT_MS = 300_000; // 300 seconds
export const LATE_REPLY_GRACE_MS = 60_000; // 60 seconds
export const LATE_PERMISSION_ACK =
  "⏳ That permission request was already resolved — it timed out or was handled elsewhere. Nothing to approve.";

/**
 * Parse a plain-text reply as a permission decision.
 * Returns null when the text is not a decision keyword.
 */
export function parsePermissionReply(text: string): PermissionDecision | null {
  const normalized = text.trim().toLowerCase();
  switch (normalized) {
    case "y":
    case "yes":
    case "allow":
      return "allow";
    case "a":
    case "always":
      return "allow_always";
    case "n":
    case "no":
    case "deny":
      return "deny";
    default:
      return null;
  }
}

/**
 * Build a human-readable permission prompt formatted for the channel.
 */
export function formatPermissionRequest(
  toolName: string,
  inputPreview: string,
  timeoutSecs: number,
  targetPath?: string,
  formatter: ChannelFormatter = plainFormatter,
): string {
  const preview = inputPreview || "—";
  const title = formatter.bold("🔐 Permission Request");
  const toolLine = `${formatter.bold("Tool:")} ${toolName}`;
  const lines = [title, toolLine];
  if (targetPath) {
    lines.push(
      `${formatter.bold("Target:")} ${targetPath} (outside project dirs)`,
    );
  }
  lines.push(
    `${formatter.bold("Input:")} ${preview}`,
    "",
    `Reply ${formatter.code("yes")} to allow / ${formatter.code("always")} to always allow this tool / ${formatter.code("no")} to deny (auto-denied after ${timeoutSecs}s)`,
  );
  return lines.join("\n");
}

export type PermissionExpireCallback<TKey = string> = (
  key: TKey,
  toolUseId: string,
  reason: "timeout" | "superseded",
) => void;

/**
 * Core PermissionManager state machine parameterized by channel key (e.g. chatId or phone).
 */
export class BasePermissionManager<
  TKey = string,
  TReq extends PendingPermissionRequest = PendingPermissionRequest,
> {
  protected pending = new Map<TKey, TReq>();
  protected timers = new Map<TKey, ReturnType<typeof setTimeout>>();
  protected lastResolved = new Map<TKey, number>();

  /**
   * Register a new pending permission request for a channel key.
   * If a previous request was pending, it is superseded and its timer cancelled.
   */
  registerPending(
    key: TKey,
    request: TReq,
    onExpire: PermissionExpireCallback<TKey>,
    timeoutMs: number = PERMISSION_TIMEOUT_MS,
  ): void {
    const existing = this.pending.get(key);
    const oldTimer = this.timers.get(key);
    if (oldTimer) {
      clearTimeout(oldTimer);
      this.timers.delete(key);
    }

    if (existing) {
      this.lastResolved.set(key, Date.now());
      try {
        onExpire(key, existing.tool_use_id, "superseded");
      } catch (err) {
        console.error("[permission] error invoking supersede onExpire:", err);
      }
    }

    this.pending.set(key, request);

    const ms = timeoutMs > 0 ? timeoutMs : PERMISSION_TIMEOUT_MS;

    const timer = setTimeout(() => {
      this.timers.delete(key);
      const current = this.pending.get(key);
      if (current && current.tool_use_id === request.tool_use_id) {
        this.pending.delete(key);
        this.lastResolved.set(key, Date.now());
        try {
          onExpire(key, current.tool_use_id, "timeout");
        } catch (err) {
          console.error("[permission] error invoking timeout onExpire:", err);
        }
      }
    }, ms);
    timer.unref();

    this.timers.set(key, timer);
  }

  /**
   * Handle an inbound text reply.
   */
  async handleResponse(
    key: TKey,
    text: string,
    target: PermissionRpcTarget,
  ): Promise<
    { decision: PermissionDecision; delivered: boolean } | "late" | null
  > {
    const decision = parsePermissionReply(text);
    if (!decision) return null;

    const current = this.pending.get(key);
    if (!current) {
      const last = this.lastResolved.get(key) ?? 0;
      if (Date.now() - last <= LATE_REPLY_GRACE_MS) {
        return "late";
      }
      return null;
    }

    return this.resolvePending(key, decision, target);
  }

  /**
   * Explicitly resolve a pending request with a given decision.
   */
  async resolvePending(
    key: TKey,
    decision: PermissionDecision,
    target: PermissionRpcTarget,
    ruleOverride?: string,
  ): Promise<{ decision: PermissionDecision; delivered: boolean } | null> {
    const current = this.pending.get(key);
    if (!current) return null;

    const timer = this.timers.get(key);
    if (timer) {
      clearTimeout(timer);
      this.timers.delete(key);
    }
    this.pending.delete(key);
    this.lastResolved.set(key, Date.now());

    const rule =
      decision === "allow_always"
        ? (ruleOverride ?? current.tool_name)
        : undefined;

    let delivered = false;
    try {
      const res = await target.request("permissionResponse", {
        tool_use_id: current.tool_use_id,
        decision,
        ...(rule !== undefined ? { rule } : {}),
      });
      delivered = (res as { delivered?: boolean })?.delivered === true;
    } catch (err) {
      console.error("[permission] failed to deliver permissionResponse:", err);
      delivered = false;
    }

    return { decision, delivered };
  }

  getPending(key: TKey): TReq | null {
    return this.pending.get(key) ?? null;
  }

  clearPending(key: TKey): void {
    const timer = this.timers.get(key);
    if (timer) {
      clearTimeout(timer);
      this.timers.delete(key);
    }
    this.pending.delete(key);
  }

  cleanup(): void {
    for (const timer of this.timers.values()) {
      clearTimeout(timer);
    }
    this.timers.clear();
    this.pending.clear();
    this.lastResolved.clear();
  }
}
