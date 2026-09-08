/**
 * PermissionManager — state machine for Feishu tool-use permission requests.
 *
 * Mirrors the WhatsApp gateway's flow (baoclaw-whatsapp/src/permission.ts),
 * card-first with a plain-text keyword fallback for older lark-clis:
 *   1. Formats a human-readable permission prompt (tool + input preview).
 *   2. Registers the request per chat with an auto-expiry window taken from
 *      the daemon's `ask_timeout_secs` (carried by each permission_request
 *      event); on expiry or supersede the caller denies the request with the
 *      daemon.
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

/**
 * Build the plain-text permission prompt (sent via lark-cli --text).
 *
 * @param timeoutSecs The daemon's auto-deny window for this ask, rendered in
 *                    the hint so the user sees the real schedule.
 * @param targetPath  Resolved absolute path when the prompt exists because
 *                    the target falls outside the project dirs (the daemon's
 *                    `target_path` event field).
 */
export function formatPermissionRequest(
  toolName: string,
  inputPreview: string,
  timeoutSecs: number,
  targetPath?: string,
): string {
  const preview = inputPreview || "—";
  const lines = ["🔐 Permission Request", `Tool: ${toolName}`];
  if (targetPath) {
    lines.push(`Target: ${targetPath} (outside project dirs)`);
  }
  lines.push(
    `Input: ${preview}`,
    "",
    `Reply yes to allow / always to always allow this tool / no to deny (auto-denied after ${timeoutSecs}s)`,
  );
  return lines.join("\n");
}

/**
 * Build an interactive card version of the prompt (sent via
 * `lark-cli im +messages-send --msg-type interactive`). Button values carry
 * the DECISION ONLY — the pending request is looked up per chat, mirroring
 * the Telegram keyboard — so the payload stays small and stable.
 */
export function buildPermissionCard(
  toolName: string,
  inputPreview: string,
  timeoutSecs: number,
  targetPath?: string,
): Record<string, unknown> {
  const preview = inputPreview || "—";
  const targetLine = targetPath
    ? `**Target:** ${targetPath} (outside project dirs)\n`
    : "";
  return {
    config: { wide_screen_mode: false },
    header: {
      template: "orange",
      title: { tag: "plain_text", content: "🔐 Permission Request" },
    },
    elements: [
      {
        tag: "div",
        text: {
          tag: "lark_md",
          content: `**Tool:** ${toolName}\n${targetLine}**Input:** ${preview}`,
        },
      },
      { tag: "hr" },
      {
        tag: "action",
        actions: [
          {
            tag: "button",
            text: { tag: "plain_text", content: "✅ Allow" },
            type: "primary",
            value: { perm_action: "allow" },
          },
          {
            tag: "button",
            text: { tag: "plain_text", content: "🔁 Always allow" },
            value: { perm_action: "always" },
          },
          {
            tag: "button",
            text: { tag: "plain_text", content: "❌ Deny" },
            type: "danger",
            value: { perm_action: "deny" },
          },
        ],
      },
      {
        tag: "note",
        elements: [
          {
            tag: "plain_text",
            content: `Auto-denied if no decision within ${timeoutSecs}s`,
          },
        ],
      },
    ],
  };
}

/** Map a button `value` payload to a decision; null when unrecognized. */
export function parseCardActionValue(
  value: unknown,
): PermissionDecision | null {
  if (value === null || value === undefined) return null;
  if (typeof value === "string") return parsePermissionReply(value);
  if (typeof value === "object") {
    const action = (value as Record<string, unknown>).perm_action;
    if (typeof action === "string") return parsePermissionReply(action);
  }
  return null;
}

/**
 * Extract a permission decision + chat id from a card.action.trigger NDJSON
 * event. lark-cli's flattened envelope shape for card events is not
 * documented, so probe the common locations (raw, `event`, `body` wrappers)
 * for the chat id (`open_chat_id` / `chat_id`) and the button value.
 * Returns null when the event is not a recognizable permission click.
 */
export function parseCardAction(
  raw: unknown,
): { chatId: string; decision: PermissionDecision } | null {
  const candidates = [raw, (raw as any)?.event, (raw as any)?.body];
  for (const c of candidates) {
    if (!c || typeof c !== "object") continue;
    const chatId: unknown =
      c.open_chat_id ?? c.chat_id ?? c.context?.open_chat_id;
    const value = c.action?.value ?? c.value;
    const decision = parseCardActionValue(value);
    if (typeof chatId === "string" && chatId && decision) {
      return { chatId, decision };
    }
  }
  return null;
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
  /** chatId → when the pending request last left the pending state. */
  private lastResolved = new Map<string, number>();

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
      this.lastResolved.set(chatId, Date.now());
      onExpire(chatId, existing.tool_use_id, "superseded");
    }

    this.pending.set(chatId, {
      tool_use_id: toolUseId,
      tool_name: toolName,
    });
    const timer = setTimeout(() => {
      this.pending.delete(chatId);
      this.timers.delete(chatId);
      this.lastResolved.set(chatId, Date.now());
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
   * Returns, in order of precedence:
   * - `"late"` — the text is a decision keyword and this chat's request left
   *   the pending state within the grace window: the caller should ack the
   *   stale reply instead of treating it as chat.
   * - `{decision, delivered}` — a live pending request was resolved.
   * - `null` — not a permission reply; the caller treats the text as normal
   *   chat (an unrecognized keyword while a request is pending also returns
   *   null and keeps the request open).
   */
  async handleResponse(
    chatId: string,
    text: string,
    client: { request: (method: string, params?: unknown) => Promise<unknown> },
  ): Promise<
    { decision: PermissionDecision; delivered: boolean } | "late" | null
  > {
    const decision = parsePermissionReply(text);
    if (!decision) return null;
    if (!this.pending.has(chatId)) {
      return this.isRecentlyResolved(chatId) ? "late" : null;
    }
    return this.resolvePending(chatId, decision, client);
  }

  /**
   * Resolve the chat's pending request with an explicit decision — the
   * single resolution path shared by text replies and card button clicks.
   * Returns null when nothing is pending; forwards the decision to the
   * daemon (swallowing IPC errors so the user is never stuck) and clears
   * the pending entry + timer. `"always"` records a whole-tool allow rule.
   */
  async resolvePending(
    chatId: string,
    decision: PermissionDecision,
    client: { request: (method: string, params?: unknown) => Promise<unknown> },
  ): Promise<{ decision: PermissionDecision; delivered: boolean } | null> {
    const pending = this.pending.get(chatId);
    if (!pending) return null;

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
    this.lastResolved.set(chatId, Date.now());
    const timer = this.timers.get(chatId);
    if (timer !== undefined) {
      clearTimeout(timer);
      this.timers.delete(chatId);
    }
    return { decision, delivered };
  }

  /**
   * Whether the chat's last permission request left the pending state within
   * `graceMs` — i.e. a decision keyword arriving now is a late reply to that
   * request, not a normal chat message.
   */
  isRecentlyResolved(
    chatId: string,
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
