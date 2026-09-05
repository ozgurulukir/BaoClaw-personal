/**
 * PermissionManager — state machine for Feishu tool-use permission requests.
 *
 * Mirrors the WhatsApp gateway's flow (baoclaw-whatsapp/src/permission.ts),
 * reply-based because the lark-cli surface used by this gateway only sends
 * plain text/markdown (no interactive cards):
 *   1. Formats a human-readable permission prompt (tool + input preview).
 *   2. Registers the request per chat with a 60-second auto-expiry; on expiry
 *      or supersede the caller denies the request with the daemon.
 *   3. Parses the user's reply (yes / always / no) and forwards the decision
 *      back to the daemon via the CONTROL channel — the daemon's serial
 *      main-connection loop is parked while a turn is in flight, exactly when
 *      a permission gate is open.
 */

import { logger } from "./log.js";

export type PermissionDecision = "allow" | "allow_always" | "deny";

export interface PermissionRequest {
  tool_use_id: string;
  tool_name: string;
}

/** Time (ms) before an unanswered permission request is automatically denied. */
const PERMISSION_TIMEOUT_MS = 60_000; // 60 seconds

/**
 * Parse a plain-text reply as a permission decision.
 * Returns null when the text is not a decision keyword — the caller should
 * treat it as a normal chat message.
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

/** Build the plain-text permission prompt (sent via lark-cli --text). */
export function formatPermissionRequest(
  toolName: string,
  inputPreview: string,
): string {
  const preview = inputPreview || "—";
  return [
    "🔐 权限请求",
    `工具: ${toolName}`,
    `输入: ${preview}`,
    "",
    "回复 yes 允许 / always 总是允许此工具 / no 拒绝（60秒后自动拒绝）",
  ].join("\n");
}

/**
 * Manages pending permission requests on behalf of the Feishu Gateway.
 * One pending request per chat — a new request supersedes the previous one.
 */
export class PermissionManager {
  /** chatId → pending request. */
  private pending = new Map<string, PermissionRequest>();
  /** chatId → expiry timer handle. */
  private timers = new Map<string, ReturnType<typeof setTimeout>>();

  /**
   * Register a prompt for `chatId`, superseding any pending one.
   *
   * @param onExpire Invoked with `"timeout"` when the window lapses or
   *                 `"superseded"` when a newer request replaces this one.
   *                 The caller must deny the request with the daemon.
   */
  registerRequest(
    chatId: string,
    toolUseId: string,
    toolName: string,
    onExpire: (
      chatId: string,
      toolUseId: string,
      reason: "timeout" | "superseded",
    ) => void,
    timeoutMs: number = PERMISSION_TIMEOUT_MS,
  ): void {
    // Supersede: cancel the old timer, hand the OLD id to the caller.
    const existing = this.pending.get(chatId);
    if (existing) {
      const oldTimer = this.timers.get(chatId);
      if (oldTimer !== undefined) {
        clearTimeout(oldTimer);
        this.timers.delete(chatId);
      }
      onExpire(chatId, existing.tool_use_id, "superseded");
    }

    this.pending.set(chatId, {
      tool_use_id: toolUseId,
      tool_name: toolName,
    });
    const timer = setTimeout(() => {
      this.pending.delete(chatId);
      this.timers.delete(chatId);
      onExpire(chatId, toolUseId, "timeout");
    }, timeoutMs);
    // Never keep the Node.js event loop alive just for an expiry timer.
    timer.unref();
    this.timers.set(chatId, timer);
  }

  /** Pending request for the chat, if any. */
  getPending(chatId: string): PermissionRequest | null {
    return this.pending.get(chatId) ?? null;
  }

  /**
   * Process an inbound chat message as a potential permission reply.
   *
   * Idempotent: no pending request for the chat → returns null and the
   * caller treats the message as normal chat. An unrecognized keyword while
   * a request is pending also returns null and keeps the request open.
   *
   * On a keyword match the decision is forwarded via `client.request` (the
   * gateway passes its control channel) and the pending entry is cleared.
   * `"always"` records a whole-tool allow rule (rule = tool name).
   *
   * @returns The decision plus whether the daemon still knew the request
   *          (`delivered` false = already timed out or answered elsewhere),
   *          or null when the message was not a permission reply.
   */
  async handleResponse(
    chatId: string,
    text: string,
    client: { request: (method: string, params?: unknown) => Promise<unknown> },
  ): Promise<{ decision: PermissionDecision; delivered: boolean } | null> {
    const pending = this.pending.get(chatId);
    if (!pending) return null;

    const decision = parsePermissionReply(text);
    if (!decision) return null;

    let delivered = false;
    try {
      const res = await client.request("permissionResponse", {
        tool_use_id: pending.tool_use_id,
        decision,
        ...(decision === "allow_always" ? { rule: pending.tool_name } : {}),
      });
      delivered = (res as { delivered?: boolean })?.delivered === true;
    } catch (err) {
      // Swallow IPC errors — the daemon may be gone. Local state is still
      // cleaned up so the user is not stuck.
      logger.error(
        `Failed to send permissionResponse for ${pending.tool_use_id}: ${err}`,
      );
    }

    this.pending.delete(chatId);
    const timer = this.timers.get(chatId);
    if (timer !== undefined) {
      clearTimeout(timer);
      this.timers.delete(chatId);
    }
    return { decision, delivered };
  }

  /** Clear every pending request and timer (gateway shutdown). */
  cleanup(): void {
    for (const timer of this.timers.values()) clearTimeout(timer);
    this.timers.clear();
    this.pending.clear();
  }
}
