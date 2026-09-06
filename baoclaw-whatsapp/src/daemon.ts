/**
 * Daemon discovery and connection, backed by the shared ts-ipc connector.
 */
import { DaemonConnector } from "baoclaw-ipc/daemon";

export { type DaemonInfo, selectNewestDaemon } from "baoclaw-ipc/daemon";
export { DaemonConnector } from "baoclaw-ipc/daemon";

/** Preconfigured for the WhatsApp gateway's session tag. */
export function createDaemonConnector(): DaemonConnector {
  return new DaemonConnector({ sessionTag: "whatsapp" });
}
