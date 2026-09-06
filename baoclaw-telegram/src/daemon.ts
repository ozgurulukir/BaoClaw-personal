/**
 * Daemon discovery and connection for the Telegram gateway.
 * Shared implementation lives in ts-ipc (IpcClient, DaemonInfo, discovery
 * helpers imported below).
 */
import * as fs from "fs";
import {
  DaemonConnector,
  IpcClient,
  resolveFixedSocket,
  selectNewestDaemon,
  type DaemonInfo,
} from "baoclaw-ipc";
import { createLogger } from "baoclaw-ipc/logger";
import { SessionState, InitializeResult } from "./commands.js";

const logger = createLogger("telegram");

/**
 * Connect to daemon with retry. Waits up to maxWaitMs for a daemon to appear.
 * Kept local because Telegram overrides the initialize cwd via
 * BAOCLAW_TELEGRAM_CWD and derives a richer SessionState from the response.
 */
export async function connectToDaemon(
  maxWaitMs = 60_000,
  retryIntervalMs = 5_000,
): Promise<{
  client: IpcClient;
  info: DaemonInfo;
  sessionState: SessionState;
  connector: DaemonConnector;
  initParams: Record<string, unknown>;
}> {
  const connector = new DaemonConnector({ sessionTag: "telegram" });
  const deadline = Date.now() + maxWaitMs;
  let lastError: Error | null = null;
  while (Date.now() < deadline) {
    const fixedSocket = resolveFixedSocket();
    if (fixedSocket && fs.existsSync(fixedSocket)) {
      const fixedInfo: DaemonInfo = {
        pid: 0,
        cwd: process.cwd(),
        session_id: "telegram",
        socket: fixedSocket,
        started_at: new Date().toISOString(),
      };
      try {
        const client = new IpcClient({ requestTimeoutMs: 0 });
        await client.connect(fixedSocket);
        const telegramCwd = process.env.BAOCLAW_TELEGRAM_CWD || process.cwd();
        const initParams = {
          cwd: telegramCwd,
          settings: {},
          shared_session_id: "telegram",
        };
        const result = await client.request<InitializeResult>(
          "initialize",
          initParams,
        );
        const sessionState: SessionState = {
          resumed: Boolean(result?.resumed),
          messageCount: result?.message_count ?? 0,
          sessionId: result?.session_id ?? "telegram",
          shared: Boolean(result?.shared),
        };
        return { client, info: fixedInfo, sessionState, connector, initParams };
      } catch (err) {
        lastError = err instanceof Error ? err : new Error(String(err));
        logger.info(
          `Fixed socket connection attempt failed: ${lastError.message}`,
        );
      }
    }
    const best = selectNewestDaemon(connector.discover());
    if (best) {
      try {
        const client = new IpcClient({ requestTimeoutMs: 0 });
        await client.connect(best.socket);
        // Use CLI's cwd if available (from /telegram start), else daemon's cwd
        const telegramCwd = process.env.BAOCLAW_TELEGRAM_CWD || best.cwd;
        const initParams = {
          cwd: telegramCwd,
          settings: {},
          shared_session_id: "telegram",
        };
        const result = await client.request<InitializeResult>(
          "initialize",
          initParams,
        );
        let sessionState: SessionState = {
          resumed: false,
          messageCount: 0,
          sessionId: result?.session_id ?? best.session_id,
          shared: result?.shared ?? false,
        };
        try {
          if (result && result.resumed) {
            sessionState = {
              resumed: true,
              messageCount: result.message_count ?? 0,
              sessionId: result.session_id ?? best.session_id,
              shared: result?.shared ?? false,
            };
            logger.info(
              `Resumed session ${sessionState.sessionId} (${sessionState.messageCount} messages)`,
            );
          }
          if (sessionState.shared) {
            logger.info(
              `Joined shared session ${sessionState.sessionId} (${sessionState.messageCount} messages)`,
            );
          }
        } catch {
          // Resume extraction failed — silently degrade to new session
        }
        return { client, info: best, sessionState, connector, initParams };
      } catch (err) {
        lastError = err instanceof Error ? err : new Error(String(err));
        logger.info(`Connection attempt failed: ${err}. Retrying...`);
      }
    } else {
      logger.info("No daemon found. Waiting...");
    }
    await new Promise((r) => setTimeout(r, retryIntervalMs));
  }
  const detail = lastError ? ` Last error: ${lastError.message}` : "";
  throw new Error(
    `No BaoClaw daemon found after ${maxWaitMs / 1000}s. Start one with: baoclaw.${detail}`,
  );
}
