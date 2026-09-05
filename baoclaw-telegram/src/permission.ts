/**
 * PermissionManager — state machine for Telegram tool-use permission requests.
 *
 * Mirrors the WhatsApp gateway's flow (baoclaw-whatsapp/src/permission.ts),
 * adapted for Telegram's interactive surface:
 *   1. Formats an HTML prompt with inline buttons (Allow / Always / Deny).
 *   2. Registers the request per chat with a 60-second auto-expiry; on expiry
 *      or supersede the caller denies the request with the daemon.
 *   3. Parses a plain-text reply as a fallback decision path
 *      (y/yes/allow, a/always, n/no/deny).
 *
 * The decision is delivered through the CONTROL channel — the daemon's serial
 * main-connection loop is parked while a turn is in flight, exactly when a
 * permission gate is open.
 *
 * Telegram constraint: `callback_data` is capped at 64 bytes and daemon
 * `tool_use_id`s run 40–60 chars, so the keyboard carries the DECISION only
 * ("perm:allow" / "perm:always" / "perm:deny") and the pending request is
 * looked up per chat — at most one can be open at a time per chat.
 */

export type PermissionDecision = "allow" | "allow_always" | "deny";

export interface PendingPermission {
  tool_use_id: string;
  tool_name: string;
  /** Message id of the prompt, so the decision can replace it. */
  message_id?: number;
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

/** Escape a string for safe interpolation into Telegram HTML. */
function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

/**
 * Build the HTML permission prompt: tool name + truncated input preview +
 * keyword hints for the reply fallback.
 */
export function formatPermissionRequest(
  toolName: string,
  inputPreview: string,
): string {
  const preview = inputPreview ? escapeHtml(inputPreview) : "—";
  return [
    "🔐 <b>权限请求</b>",
    `工具: <code>${escapeHtml(toolName)}</code>`,
    `输入: <code>${preview}</code>`,
    "",
    "回复 <b>y</b> 允许 / <b>a</b> 总是允许 / <b>n</b> 拒绝（60秒后自动拒绝）",
  ].join("\n");
}

/** Build the inline keyboard markup for a fresh prompt. */
export function buildPermissionKeyboard(): Record<string, unknown> {
  return {
    inline_keyboard: [
      [
        { text: "✅ 允许", callback_data: "perm:allow" },
        { text: "❌ 拒绝", callback_data: "perm:deny" },
      ],
      [{ text: "🔁 总是允许此工具", callback_data: "perm:always" }],
    ],
  };
}

/**
 * Manages pending permission requests on behalf of the Telegram Gateway.
 * One pending request per chat — a new request supersedes the previous one.
 */
export class TelegramPermissionManager {
  /** chatId → pending request. */
  private pending = new Map<number, PendingPermission>();
  /** chatId → expiry timer handle. */
  private timers = new Map<number, ReturnType<typeof setTimeout>>();

  /**
   * Register a prompt for `chatId`, superseding any pending one.
   *
   * @param onExpire Invoked with `"timeout"` when the window lapses or
   *                 `"superseded"` when a newer request replaces this one.
   *                 The caller must deny the request with the daemon.
   */
  register(
    chatId: number,
    request: PendingPermission,
    onExpire: (
      chatId: number,
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

    this.pending.set(chatId, request);
    const timer = setTimeout(() => {
      this.pending.delete(chatId);
      this.timers.delete(chatId);
      onExpire(chatId, request.tool_use_id, "timeout");
    }, timeoutMs);
    // Never keep the Node.js event loop alive just for an expiry timer.
    timer.unref();
    this.timers.set(chatId, timer);
  }

  /** Pending request for the chat, if any. */
  get(chatId: number): PendingPermission | null {
    return this.pending.get(chatId) ?? null;
  }

  /** Resolve and clear the chat's pending request; null when none pending. */
  resolve(chatId: number): PendingPermission | null {
    const request = this.pending.get(chatId) ?? null;
    if (request) {
      this.pending.delete(chatId);
      const timer = this.timers.get(chatId);
      if (timer !== undefined) {
        clearTimeout(timer);
        this.timers.delete(chatId);
      }
    }
    return request;
  }

  /** Clear every pending request and timer (gateway shutdown). */
  cleanup(): void {
    for (const timer of this.timers.values()) clearTimeout(timer);
    this.timers.clear();
    this.pending.clear();
  }
}
