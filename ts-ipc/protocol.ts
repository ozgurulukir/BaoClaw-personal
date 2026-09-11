/**
 * BaoClaw IPC Protocol: Strongly-typed JSON-RPC 2.0 Contract
 * Single Source of Truth (SSOT) between Rust baoclaw-core and TypeScript clients.
 */

import type { IpcMethodMap, IpcMethodName } from "./protocol/methods.js";

export * from "./protocol/types.js";
export * from "./protocol/methods.js";

/**
 * Complete runtime array of all 87 daemon RPC methods.
 * Used for parity verification against baoclaw-core ClientMethod enum.
 */
export const ALL_IPC_METHODS: readonly IpcMethodName[] = [
  // Session & Core Lifecycle
  "initialize",
  "submitMessage",
  "abort",
  "shutdown",
  "updateSettings",
  "permissionResponse",
  "compact",
  "clearSession",
  "switchModel",

  // Read-Only Discoveries
  "listTools",
  "listMcpServers",
  "mcpRefresh",
  "listSkills",
  "listPlugins",
  "toolHealth",

  // Git Integration
  "gitStatus",
  "gitDiff",
  "gitCommit",
  "gitPrCreate",
  "gitPrList",
  "gitBranchList",
  "gitConflictCheck",

  // Task Management
  "taskCreate",
  "taskList",
  "taskStatus",
  "taskStop",

  // Long-Term Memory
  "memoryList",
  "memoryAdd",
  "memoryDelete",
  "memoryClear",
  "memoryStats",
  "memoryArchive",
  "memoryRestore",
  "memoryArchiveList",
  "memoryCleanup",

  // Cron & Automation
  "cronAdd",
  "cronRemove",
  "cronToggle",
  "cronList",

  // Projects
  "projectsList",
  "projectsSwitch",
  "projectsNew",
  "projectsUpdateDesc",

  // Chat History & Search
  "talkTail",
  "searchHistory",
  "docUpload",
  "export",

  // Spec Workflow
  "specNew",
  "specList",
  "specShow",
  "specStatus",
  "specRun",
  "specEdit",

  // Team Collaboration
  "teamSpawn",
  "teamList",
  "teamStatus",
  "teamResults",
  "teamAbort",
  "teamExecute",

  // Prompt & Flow Templates
  "templateList",
  "templateCreate",
  "templateDelete",
  "templateExport",
  "templateImport",

  // Model & Routing
  "modelList",
  "modelRoute",
  "modelBudget",

  // Telemetry & Observability
  "telemetryStats",
  "telemetryTrends",
  "telemetryExport",
  "telemetry.setEnabled",

  // Permission Gate & Rules
  "permissionStatus",
  "permissionGrant",
  "permissionRevoke",
  "permissions.info",
  "permissions.addRule",
  "permissions.removeRule",
  "permissions.setMode",
  "permissions.setAutoAllow",
  "permissions.setAskTimeout",
  "permissions.setPersistGrants",
  "evolution.rateTrajectory",

  // Session Metrics & Config
  "session.tokens",
  "session.cost",
  "session.info",
  "config.model",
  "config.show",
] as const;

const METHOD_SET: ReadonlySet<string> = new Set(ALL_IPC_METHODS);

/**
 * Type guard to check if a given string is a valid BaoClaw IPC method name.
 */
export function isIpcMethod(name: string): name is IpcMethodName {
  return METHOD_SET.has(name);
}
