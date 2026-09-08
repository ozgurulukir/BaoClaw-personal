/**
 * PermissionManager — state machine for tool-use permission requests.
 *
 * When the daemon needs user approval before executing a tool (e.g. file writes,
 * shell commands), this module:
 *   1. Formats a human-readable permission request message for WhatsApp.
 *   2. Registers the request in `SenderTracker` with an auto-expiry window
 *      taken from the daemon's `ask_timeout_secs` (carried by each
 *      permission_request event).
 *   3. Parses the user's WhatsApp reply (`yes`/`no`) and forwards the decision
 *      back to the daemon via `IpcClient.request('permissionResponse', …)`.
 *
 * Lifecycle of a single permission request:
 *   registerRequest()  →  [waiting for user]  →  handleResponse("yes"/"no")
 *                                                or timeout → onTimeout()
 */

import { SenderTracker, type PermissionRequest } from "./senderTracker.js";
import { IpcClient } from "baoclaw-ipc";
import { createLogger } from "baoclaw-ipc/logger";

const logger = createLogger("whatsapp");

/**
 * Last-resort auto-expiry window (ms), matching the daemon's default
 * `ask_timeout_secs`. Fresh prompts always carry the daemon's live value in
 * the `permission_request` event; this constant only covers a malformed or
 * pre-2.2 daemon event missing that field.
 */
const PERMISSION_TIMEOUT_MS = 300_000; // 300 seconds

/**
 * How long after a sender's permission request leaves the pending state
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
 * Manages permission request / response flow on behalf of the WhatsApp Gateway.
 *
 * Usage:
 * ```ts
 * const pm = new PermissionManager(senderTracker);
 * // When a tool_use event arrives:
 * const text = pm.formatPermissionRequest(toolUseId, toolName, desc);
 * await sock.sendMessage(jid, { text });
 * pm.registerRequest(phone, toolUseId, toolName, desc, (ph, id) => { … });
 *
 * // When an inbound WhatsApp message arrives:
 * const handled = await pm.handleResponse(phone, msgText, ipcClient);
 * if (handled) { /* was a permission reply, already forwarded to daemon *\/ }
 *
 * // On shutdown:
 * pm.cleanup();
 * ```
 */
export class PermissionManager {
  private senderTracker: SenderTracker;
  /** Per-phone timeout handles so we can cancel them on explicit replies. */
  private timers = new Map<string, ReturnType<typeof setTimeout>>();
  /** Per-phone timestamp of when the pending request last left pending state. */
  private lastResolved = new Map<string, number>();

  /**
   * @param senderTracker  The shared `SenderTracker` instance that stores
   *                       per-sender state including `pendingPermission`.
   */
  constructor(senderTracker: SenderTracker) {
    this.senderTracker = senderTracker;
  }

  // ── Formatting ────────────────────────────────────────────────────────────

  /**
   * Build a formatted permission request message suitable for WhatsApp.
   *
   * The returned string uses WhatsApp-friendly formatting (bold with `*…*`).
   *
   * @param toolUseId    Opaque ID from the daemon's `tool_use` event.
   * @param toolName     Human-readable tool name (e.g. `"bash"`).
   * @param description  Optional one-liner describing what the tool will do.
   * @param timeoutSecs  The daemon's auto-deny window for this ask, rendered
   *                     in the hint so the user sees the real schedule.
   * @returns A multi-line string ready to be sent via `sock.sendMessage`.
   *
   * @example
   * ```ts
   * const text = pm.formatPermissionRequest('tu_123', 'bash', 'rm -rf /tmp/old', 300);
   * // 🔐 *Permission Request*
   * // Tool: bash
   * // Description: rm -rf /tmp/old
   * //
   * // Reply *yes* to allow, *always* to always allow this tool, or *no* to deny
   * // (auto-denied after 300 seconds)
   * ```
   */
  formatPermissionRequest(
    toolUseId: string,
    toolName: string,
    description?: string,
    timeoutSecs: number = PERMISSION_TIMEOUT_MS / 1000,
    targetPath?: string,
  ): string {
    const desc = description?.trim() || "None";
    const lines = ["🔐 *Permission Request*", `Tool: ${toolName}`];
    if (targetPath) {
      lines.push(`Target: ${targetPath} (outside project dirs)`);
    }
    lines.push(
      `Description: ${desc}`,
      "",
      "Reply *yes* to allow, *always* to always allow this tool, or *no* to deny",
      `(auto-denied after ${timeoutSecs} seconds)`,
    );
    return lines.join("\n");
  }

  // ── Registration ──────────────────────────────────────────────────────────

  /**
   * Register a new permission request for `phone`.
   *
   * Steps:
   *  1. If the sender already has a **pending** request, cancel its timer and
   *     invoke `onTimeout(phone, oldToolUseId, "superseded")` so the caller
   *     can deny the stale request with the daemon.
   *  2. Create a `PermissionRequest` object with an expiry timestamp
   *     (`Date.now() + timeoutMs`).
   *  3. Store it via `SenderTracker.setPendingPermission`.
   *  4. Start a timer that, on expiry, clears the pending permission and
   *     invokes `onTimeout(phone, toolUseId, "timeout")`.
   *
   * @param phone       Sender phone (E.164).
   * @param toolUseId   Unique ID from the daemon.
   * @param toolName    Tool name for the request record.
   * @param description Human-readable description.
   * @param onTimeout   Callback invoked when the request expires without a
   *                    reply (`"timeout"`) **or** when superseded by a newer
   *                    request (`"superseded"`). Callers must deny the request
   *                    with the daemon in both cases.
   * @param timeoutMs   Auto-expiry window; overridable for tests.
   */
  registerRequest(
    phone: string,
    toolUseId: string,
    toolName: string,
    description: string,
    onTimeout: (
      phone: string,
      toolUseId: string,
      reason: "timeout" | "superseded",
    ) => void,
    timeoutMs: number = PERMISSION_TIMEOUT_MS,
  ): void {
    // 1. Evict any existing pending request for this sender.
    const existing = this.senderTracker.getPendingPermission(phone);
    if (existing) {
      const oldTimer = this.timers.get(phone);
      if (oldTimer !== undefined) {
        clearTimeout(oldTimer);
        this.timers.delete(phone);
      }
      // Notify caller about the superseded request so it can deny it.
      this.lastResolved.set(phone, Date.now());
      onTimeout(phone, existing.tool_use_id, "superseded");
    }

    // 2. Build the new request.
    const request: PermissionRequest = {
      tool_use_id: toolUseId,
      tool_name: toolName,
      description,
      expiresAt: Date.now() + timeoutMs,
    };

    // 3. Persist in the tracker.
    this.senderTracker.setPendingPermission(phone, request);

    // 4. Start the auto-expiry timer.
    const timer = setTimeout(() => {
      this.senderTracker.clearPendingPermission(phone);
      this.timers.delete(phone);
      this.lastResolved.set(phone, Date.now());
      onTimeout(phone, toolUseId, "timeout");
    }, timeoutMs);

    // Prevent the timer from keeping the Node.js event loop alive during
    // a clean shutdown (cleanup() will handle it explicitly).
    timer.unref();

    this.timers.set(phone, timer);
  }

  // ── Response handling ─────────────────────────────────────────────────────

  /**
   * Process an inbound WhatsApp text message as a potential permission reply.
   *
   * Returns, in order of precedence:
   * - `"late"` — the text is a decision keyword and this sender's request
   *   left the pending state within the grace window: the caller should ack
   *   the stale reply instead of treating it as chat.
   * - `{decision, delivered}` — a live pending request was resolved; the
   *   `delivered` flag is false when the daemon no longer knew the request
   *   (already timed out or answered elsewhere).
   * - `null` — not a permission reply; the caller can treat the message as a
   *   normal chat prompt (an unrecognized keyword while a permission is
   *   pending also returns null and keeps the request pending).
   *
   * @param phone      Sender phone (E.164).
   * @param text       Raw message text from WhatsApp.
   * @param client     Connected IPC client or control channel for the daemon.
   */
  async handleResponse(
    phone: string,
    text: string,
    client: Pick<IpcClient, "request">,
  ): Promise<
    | { decision: "allow" | "allow_always" | "deny"; delivered: boolean }
    | "late"
    | null
  > {
    // 1. Check for a pending request.
    const pending = this.senderTracker.getPendingPermission(phone);
    if (!pending) {
      const normalized = text.trim().toLowerCase();
      const isKeyword =
        normalized === "yes" ||
        normalized === "allow" ||
        normalized === "a" ||
        normalized === "always" ||
        normalized === "no" ||
        normalized === "deny";
      return isKeyword && this.isRecentlyResolved(phone) ? "late" : null;
    }

    // 2. Parse the reply.
    const normalized = text.trim().toLowerCase();
    let decision: "allow" | "allow_always" | "deny" | null = null;

    if (normalized === "yes" || normalized === "allow") {
      decision = "allow";
    } else if (normalized === "a" || normalized === "always") {
      decision = "allow_always";
    } else if (normalized === "no" || normalized === "deny") {
      decision = "deny";
    }

    // 3. Not a recognised keyword — leave the request pending.
    if (decision === null) {
      return null;
    }

    // 4. Forward the decision to the daemon. "always" records a whole-tool
    // allow rule (rule = tool name), mirroring the other gateways.
    let delivered = false;
    try {
      const res = await client.request("permissionResponse", {
        tool_use_id: pending.tool_use_id,
        decision,
        ...(decision === "allow_always" ? { rule: pending.tool_name } : {}),
      });
      delivered = (res as { delivered?: boolean })?.delivered === true;
    } catch (err) {
      // Swallow IPC errors — the daemon may have disconnected. We still
      // clean up the local state so the user is not stuck.
      logger.error(
        `Failed to send permissionResponse for ${pending.tool_use_id}: ${err}`,
      );
    }

    // 5. Clean up local state.
    this.senderTracker.clearPendingPermission(phone);
    this.lastResolved.set(phone, Date.now());
    const timer = this.timers.get(phone);
    if (timer !== undefined) {
      clearTimeout(timer);
      this.timers.delete(phone);
    }

    // 6. Signal that the message was handled.
    return { decision, delivered };
  }

  /**
   * Whether the sender's last permission request left the pending state
   * within `graceMs` — i.e. a decision keyword arriving now is a late reply
   * to that request, not a normal chat message.
   */
  isRecentlyResolved(
    phone: string,
    graceMs: number = LATE_REPLY_GRACE_MS,
  ): boolean {
    const at = this.lastResolved.get(phone);
    return at !== undefined && Date.now() - at < graceMs;
  }

  /**
   * Clear all pending timers.
   *
   * Must be called during gateway shutdown to prevent dangling `setTimeout`
   * handles. After calling this method no new timers should be created on
   * the same instance.
   */
  cleanup(): void {
    const allTimers = Array.from(this.timers.values());
    for (const timer of allTimers) {
      clearTimeout(timer);
    }
    this.timers.clear();
    this.lastResolved.clear();
  }
}
