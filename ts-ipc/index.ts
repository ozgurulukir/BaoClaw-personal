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
export {
  formatSearchResults,
  type FormatSearchResultsOptions,
  type SearchResult,
} from "./search.js";
export {
  formatMcpServers,
  type McpRefreshResult,
  type McpServerInfo,
  type McpServerList,
  type McpServerRuntime,
} from "./mcp.js";
export {
  ALL_IPC_METHODS,
  isIpcMethod,
  type IpcMethodMap,
  type IpcMethodName,
  type JsonRpcRequest,
  type JsonRpcNotification,
  type JsonRpcResponse,
  type JsonRpcError,
  type ToolInfo,
  type ListToolsResult,
  type MemoryEntry,
  type MemoryAddParams,
  type MemoryAddResult,
  type MemoryListResult,
  type GitStatusResult,
  type GitDiffResult,
  type GitCommitResult,
  type CompactResult,
  type SessionTokensResult,
  type SessionCostResult,
  type SessionInfoResult,
} from "./protocol.js";

export * from "./gateway/index.js";
