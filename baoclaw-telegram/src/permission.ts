/**
 * PermissionManager — state machine for Telegram tool-use permission requests.
 *
 * Mirrors the WhatsApp gateway's flow (baoclaw-whatsapp/src/permission.ts),
 * adapted for Telegram's interactive surface:
 *   1. Formats an HTML prompt with inline buttons (Allow / Always / Deny).
 *   2. Registers the request per chat with an auto-expiry window taken from
 *      the daemon's `ask_timeout_secs` (carried by each permission_request
 *      event); on expiry or supersede the caller denies the request with the
 *      daemon.
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

/**
 * Last-resort auto-expiry window (ms), matching the daemon's default
 * `ask_timeout_secs`. Fresh prompts always carry the daemon's live value in
 * the `permission_request` event; this constant only covers a malformed or
 * pre-2.2 daemon event missing that field.
 */
const PERMISSION_TIMEOUT_MS = 300_000; // 300 seconds

/**
 * How long after a chat's permission request leaves the pending state
 * (timeout, supersede, or decision) a reply keyword is still treated as a
 * late answer to THAT request rather than as a normal chat message — so a
 * user replying "yes" to an already-resolved prompt doesn't accidentally
 * submit "yes" to the model as a chat prompt.
 */
const LATE_REPLY_GRACE_MS = 60_000; // 60 seconds

/** Acknowledgement sent for a decision keyword arriving after resolution. */
export const LATE_PERMISSION_ACK =
  "⏳ That permission request was already resolved — it timed out or was handled elsewhere. Nothing to approve.";

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
 * Build the HTML permission prompt: tool name + optional out-of-boundary
 * target + truncated input preview + keyword hints for the reply fallback.
 *
 * @param timeoutSecs The daemon's auto-deny window for this ask, rendered in
 *                    the hint so the user sees the real schedule.
 * @param targetPath  Resolved absolute path when the prompt exists because
 *                    the target falls outside the project dirs (the daemon's
 *                    `target_path` event field), shown prominently so the
 *                    user knows what the decision actually opens up.
 */
export function formatPermissionRequest(
  toolName: string,
  inputPreview: string,
  timeoutSecs: number,
  targetPath?: string,
): string {
  const preview = inputPreview ? escapeHtml(inputPreview) : "—";
  const lines = [
    "🔐 <b>Permission Request</b>",
    `Tool: <code>${escapeHtml(toolName)}</code>`,
  ];
  if (targetPath) {
    lines.push(
      `Target: <code>${escapeHtml(targetPath)}</code> (outside project dirs)`,
    );
  }
  lines.push(
    `Input: <code>${preview}</code>`,
    "",
    `Reply <b>y</b> to allow / <b>a</b> to always allow / <b>n</b> to deny (auto-denied after ${timeoutSecs}s)`,
  );
  return lines.join("\n");
}

/** Build the inline keyboard markup for a fresh prompt. */
export function buildPermissionKeyboard(): Record<string, unknown> {
  return {
    inline_keyboard: [
      [
        { text: "✅ Allow", callback_data: "perm:allow" },
        { text: "❌ Deny", callback_data: "perm:deny" },
      ],
      [{ text: "🔁 Always allow this tool", callback_data: "perm:always" }],
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
  /** chatId → when the pending request last left the pending state. */
  private lastResolved = new Map<number, number>();

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
      this.lastResolved.set(chatId, Date.now());
      onExpire(chatId, existing.tool_use_id, "superseded");
    }

    this.pending.set(chatId, request);
    const timer = setTimeout(() => {
      this.pending.delete(chatId);
      this.timers.delete(chatId);
      this.lastResolved.set(chatId, Date.now());
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
      this.lastResolved.set(chatId, Date.now());
      const timer = this.timers.get(chatId);
      if (timer !== undefined) {
        clearTimeout(timer);
        this.timers.delete(chatId);
      }
    }
    return request;
  }

  /**
   * Whether the chat's last permission request left the pending state within
   * `graceMs` — i.e. a decision keyword arriving now is a late reply to that
   * request, not a normal chat message.
   */
  isRecentlyResolved(
    chatId: number,
    graceMs: number = LATE_REPLY_GRACE_MS,
  ): boolean {
    const at = this.lastResolved.get(chatId);
    return at !== undefined && Date.now() - at < graceMs;
  }

  /** Clear every pending request and timer (gateway shutdown). */
  cleanup(): void {
    for (const timer of this.timers.values()) clearTimeout(timer);
    this.timers.clear();
    this.pending.clear();
    this.lastResolved.clear();
  }
}
