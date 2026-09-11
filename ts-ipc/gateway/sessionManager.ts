/**
 * Gateway Session Lifecycle & Reconnection Management.
 */

export type SessionLifecycleState =
  | "idle"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "logged_out"
  | "stopping";

export const RETRY_BASE_DELAY_MS = 3_000;
export const RETRY_MAX_DELAY_MS = 30_000;

export function retryDelayMs(
  retry: number,
  baseMs = RETRY_BASE_DELAY_MS,
  maxMs = RETRY_MAX_DELAY_MS,
): number {
  const exponent = Math.max(0, retry - 1);
  return Math.min(baseMs * 2 ** exponent, maxMs);
}
