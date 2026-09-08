export { IpcClient, type IpcClientOptions } from "./client.js";
export {
  attachControlChannel,
  type ControlChannel,
  type ControlChannelOptions,
} from "./controlChannel.js";
export type { DaemonInfo, DaemonConnectorOptions } from "./daemon.js";
export {
  DaemonConnector,
  buildDaemonInitParams,
  discoverLegacyDaemons,
  selectNewestDaemon,
  getSocketDir,
  resolveFixedSocket,
} from "./daemon.js";
export { logger, createLogger, setLogLevel, setLogFile } from "./logger.js";
export { securePrivateFile } from "./security.js";
export {
  formatToolHealth,
  type FormatToolHealthOptions,
  type ToolHealthData,
  type ToolHealthRecordInfo,
} from "./toolHealth.js";
