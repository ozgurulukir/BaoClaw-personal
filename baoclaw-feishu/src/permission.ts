/**
 * PermissionManager — state machine for Feishu tool-use permission requests.
 *
 * Card-first with a plain-text keyword fallback for older lark-clis:
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

import {
  BasePermissionManager,
  parsePermissionReply,
  formatPermissionRequest,
  PERMISSION_TIMEOUT_MS,
  LATE_REPLY_GRACE_MS,
  LATE_PERMISSION_ACK,
  type PermissionDecision,
  type PendingPermissionRequest,
} from "baoclaw-ipc/gateway";

export {
  parsePermissionReply,
  formatPermissionRequest,
  PERMISSION_TIMEOUT_MS,
  LATE_REPLY_GRACE_MS,
  LATE_PERMISSION_ACK,
  type PermissionDecision,
};

export interface PermissionRequest extends PendingPermissionRequest {
  tool_use_id: string;
  tool_name: string;
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
            content: `Auto-denies within ${timeoutSecs}s if unhandled`,
          },
        ],
      },
    ],
  };
}

/**
 * Unpack the button value payload from an interactive card action event.
 * Feishu wraps the button's `value` in various envelopes depending on whether
 * the card was sent via webhook or bot API:
 *   { action: { value: { perm_action: "allow" } } }
 *   { action: { value: '{"perm_action":"allow"}' } }
 *   { perm_action: "allow" }
 */
export function parseCardActionValue(raw: unknown): PermissionDecision | null {
  if (!raw) return null;
  let val: unknown = raw;
  if (typeof raw === "string") {
    try {
      val = JSON.parse(raw);
    } catch {
      val = raw;
    }
  }
  const action =
    typeof val === "object" && val !== null
      ? (val as { perm_action?: unknown }).perm_action
      : val;
  if (action === "allow") return "allow";
  if (action === "always") return "allow_always";
  if (action === "deny") return "deny";
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
 * Extends BasePermissionManager with Feishu-specific convenience methods.
 */
export class PermissionManager extends BasePermissionManager<
  string,
  PermissionRequest
> {
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
    this.registerPending(
      chatId,
      { tool_use_id: toolUseId, tool_name: toolName },
      onExpire,
      timeoutMs,
    );
  }

  isRecentlyResolved(
    chatId: string,
    graceMs: number = LATE_REPLY_GRACE_MS,
  ): boolean {
    const at = this.lastResolved.get(chatId);
    return at !== undefined && Date.now() - at < graceMs;
  }
}
