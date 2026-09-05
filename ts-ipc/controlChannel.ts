import { IpcClient } from "./client.js";
import { createLogger } from "./logger.js";

const logger = createLogger("ts-ipc");

export interface ControlChannelOptions {
  /** Daemon socket path — the same one the main connection uses. */
  socketPath: string;
  /**
   * Initialize params IDENTICAL to the main connection's: the daemon derives
   * the shared session key from cwd + shared_session_id, so the control
   * channel only reaches the main connection's session if these match.
   */
  initParams: Record<string, unknown>;
  /**
   * Main connection used when the control connection cannot be established.
   * Fallback requests disable timeouts, because the daemon's serial loop may
   * be parked inside an active turn and answer only after the turn drains.
   */
  fallbackClient: IpcClient;
  /** Invoked when an established control connection drops. */
  onDisconnect?: (error: Error) => void;
}

export interface ControlChannel {
  /**
   * Send an RPC that must not be deferred by an in-flight turn
   * (abort, permissionResponse).
   */
  request<T = unknown>(method: string, params?: unknown): Promise<T>;
  /** Close the control connection (no-op if it was never established). */
  close(): Promise<void>;
}

/**
 * Open a dedicated control connection to the daemon.
 *
 * The daemon serves each IPC connection with a serial loop: while a turn is
 * in flight, its loop cannot answer further requests on the submitting
 * connection. Abort and permissionResponse are designed to be callable from
 * any client of the same shared session, so a second connection delivers
 * them mid-turn; when setup fails, requests degrade to the fallback client
 * with timeouts disabled (pre-control-channel semantics).
 */
export async function attachControlChannel(
  opts: ControlChannelOptions,
): Promise<ControlChannel> {
  let control: IpcClient | null = null;
  try {
    control = new IpcClient();
    await control.connect(opts.socketPath);
    // Session restore can serialize behind the main connection's initialize
    // — allow a generous but bounded wait instead of the 30s default.
    await control.request("initialize", opts.initParams, 60_000);
    if (opts.onDisconnect) control.onDisconnect(opts.onDisconnect);
  } catch (err) {
    // Tear the socket down too: a connected-but-uninitialized connection
    // would stay registered daemon-side and block session cleanup.
    await control?.disconnect().catch(() => {});
    control = null;
    const message = err instanceof Error ? err.message : String(err);
    logger.warn(
      `control channel setup failed (${message}); control RPCs degrade to the main connection`,
    );
  }

  return {
    request: <T>(method: string, params?: unknown) =>
      control
        ? control.request<T>(method, params)
        : opts.fallbackClient.request<T>(method, params, 0),
    close: async () => {
      await control?.disconnect();
    },
  };
}
