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
 */

import { SenderTracker, type PermissionRequest } from "./senderTracker.js";
import { type IpcClient } from "baoclaw-ipc";
import {
  BasePermissionManager,
  PERMISSION_TIMEOUT_MS,
  LATE_REPLY_GRACE_MS,
  LATE_PERMISSION_ACK,
  type PermissionDecision,
} from "baoclaw-ipc/gateway";

export {
  PERMISSION_TIMEOUT_MS,
  LATE_REPLY_GRACE_MS,
  LATE_PERMISSION_ACK,
  type PermissionDecision,
};

/**
 * Manages permission request / response flow on behalf of the WhatsApp Gateway.
 */
export class PermissionManager extends BasePermissionManager<
  string,
  PermissionRequest
> {
  private senderTracker: SenderTracker;

  /**
   * @param senderTracker  The shared `SenderTracker` instance that stores
   *                       per-sender state including `pendingPermission`.
   */
  constructor(senderTracker: SenderTracker) {
    super();
    this.senderTracker = senderTracker;
  }

  // ── Formatting ────────────────────────────────────────────────────────────

  formatPermissionRequest(
    _toolUseId: string,
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
    const request: PermissionRequest = {
      tool_use_id: toolUseId,
      tool_name: toolName,
      description,
      expiresAt: Date.now() + timeoutMs,
    };

    this.senderTracker.setPendingPermission(phone, request);

    this.registerPending(
      phone,
      request,
      (p, id, reason) => {
        if (reason === "timeout") {
          this.senderTracker.clearPendingPermission(p);
        }
        onTimeout(p, id, reason);
      },
      timeoutMs,
    );
  }

  // ── Response handling ─────────────────────────────────────────────────────

  override async handleResponse(
    phone: string,
    text: string,
    client: Pick<IpcClient, "request">,
  ): Promise<
    | { decision: "allow" | "allow_always" | "deny"; delivered: boolean }
    | "late"
    | null
  > {
    const res = await super.handleResponse(phone, text, client);
    if (res && res !== "late") {
      this.senderTracker.clearPendingPermission(phone);
    }
    return res;
  }

  isRecentlyResolved(
    phone: string,
    graceMs: number = LATE_REPLY_GRACE_MS,
  ): boolean {
    const at = this.lastResolved.get(phone);
    return at !== undefined && Date.now() - at < graceMs;
  }

  override cleanup(): void {
    const activePhones = Array.from(this.pending.keys());
    super.cleanup();
    for (const phone of activePhones) {
      this.senderTracker.clearPendingPermission(phone);
    }
  }
}
