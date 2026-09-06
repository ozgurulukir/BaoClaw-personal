/**
 * Daemon discovery and connection, backed by the shared ts-ipc connector.
 */
import { DaemonConnector } from "baoclaw-ipc";

export { type DaemonInfo, selectNewestDaemon } from "baoclaw-ipc";
export { DaemonConnector } from "baoclaw-ipc";

/** Preconfigured for the Feishu gateway's session tag. */
export function createDaemonConnector(): DaemonConnector {
  return new DaemonConnector({ sessionTag: "feishu" });
}
