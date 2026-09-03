/**
 * Daemon discovery and connection, backed by the shared ts-ipc connector.
 */
import { DaemonConnector } from "../../ts-ipc/daemon.js";

export { type DaemonInfo, selectNewestDaemon } from "../../ts-ipc/daemon.js";
export { DaemonConnector } from "../../ts-ipc/daemon.js";

/** Preconfigured for the WhatsApp gateway's session tag. */
export function createDaemonConnector(): DaemonConnector {
  return new DaemonConnector({ sessionTag: "whatsapp" });
}
