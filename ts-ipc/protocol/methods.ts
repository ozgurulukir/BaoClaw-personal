/**
 * Strongly-typed IPC method mapping for BaoClaw JSON-RPC 2.0 daemon.
 * Maps every RPC method name to its exact request params and response result.
 */

import type {
  CompactResult,
  CronAddParams,
  CronParams,
  DocUploadParams,
  DocUploadResult,
  ExportParams,
  ExportResult,
  GitCommitParams,
  GitCommitResult,
  GitDiffResult,
  GitStatusResult,
  InitializeParams,
  InitializeResult,
  ListPluginsResult,
  ListSkillsResult,
  ListToolsResult,
  MemoryAddParams,
  MemoryAddResult,
  MemoryClearResult,
  MemoryDeleteResult,
  MemoryListResult,
  MemoryStatsResult,
  PermissionResponseParams,
  ProjectsNewParams,
  ProjectsSwitchParams,
  ProjectsUpdateDescParams,
  SearchHistoryParams,
  SearchHistoryResult,
  SessionCostResult,
  SessionInfoResult,
  SessionTokensResult,
  SpecEditParams,
  SpecNewParams,
  SpecParams,
  SpecRunParams,
  SubmitMessageParams,
  SubmitMessageResult,
  TalkTailParams,
  TaskCreateParams,
  TaskCreateResult,
  TaskStatusParams,
  TaskStopParams,
  TaskStopResult,
  TeamParams,
  TeamSpawnParams,
} from "./types.js";
import type { McpRefreshResult, McpServerList } from "../mcp.js";
import type { ToolHealthData } from "../toolHealth.js";

export interface IpcMethodMap {
  // ── Session & Core Lifecycle ──
  initialize: { params: InitializeParams; result: InitializeResult };
  submitMessage: { params: SubmitMessageParams; result: SubmitMessageResult };
  abort: { params: void; result: { aborted: boolean; [key: string]: unknown } };
  shutdown: { params: void; result: { ok: boolean; [key: string]: unknown } };
  updateSettings: {
    params: { settings: Record<string, unknown> };
    result: { ok: boolean; [key: string]: unknown };
  };
  permissionResponse: {
    params: PermissionResponseParams;
    result: { delivered: boolean; [key: string]: unknown };
  };
  compact: { params: void; result: CompactResult };
  clearSession: {
    params: void;
    result: { cleared: boolean; [key: string]: unknown };
  };
  switchModel: {
    params: { model: string };
    result: { switched: boolean; model: string };
  };

  // ── Read-Only Discoveries ──
  listTools: { params: void; result: ListToolsResult };
  listMcpServers: { params: void; result: McpServerList };
  mcpRefresh: { params: { server?: string }; result: McpRefreshResult };
  listSkills: { params: void; result: ListSkillsResult };
  listPlugins: { params: void; result: ListPluginsResult };
  toolHealth: { params: void; result: ToolHealthData };

  // ── Git Integration ──
  gitStatus: { params: void; result: GitStatusResult };
  gitDiff: { params: void; result: GitDiffResult };
  gitCommit: { params: GitCommitParams; result: GitCommitResult };
  gitPrCreate: {
    params: { title: string; body: string; base: string; head: string };
    result: unknown;
  };
  gitPrList: { params: void; result: unknown };
  gitBranchList: { params: void; result: unknown };
  gitConflictCheck: { params: void; result: unknown };

  // ── Task Management ──
  taskCreate: { params: TaskCreateParams; result: TaskCreateResult };
  taskList: { params: void; result: { tasks: unknown[]; count: number } };
  taskStatus: { params: TaskStatusParams; result: unknown };
  taskStop: { params: TaskStopParams; result: TaskStopResult };

  // ── Long-Term Memory ──
  memoryList: { params: void; result: MemoryListResult };
  memoryAdd: { params: MemoryAddParams; result: MemoryAddResult };
  memoryDelete: { params: { id: string }; result: MemoryDeleteResult };
  memoryClear: { params: void; result: MemoryClearResult };
  memoryStats: { params: void; result: MemoryStatsResult };
  memoryArchive: { params: { id: string }; result: unknown };
  memoryRestore: { params: { id: string }; result: unknown };
  memoryArchiveList: { params: void; result: unknown };
  memoryCleanup: { params: void; result: unknown };

  // ── Cron & Automation ──
  cronAdd: { params: CronAddParams; result: unknown };
  cronRemove: { params: CronParams; result: { removed: boolean } };
  cronToggle: { params: CronParams; result: { enabled: boolean } };
  cronList: { params: void; result: { jobs: unknown[] } };

  // ── Projects ──
  projectsList: { params: void; result: { projects: unknown[] } };
  projectsSwitch: { params: ProjectsSwitchParams; result: unknown };
  projectsNew: { params: ProjectsNewParams; result: unknown };
  projectsUpdateDesc: { params: ProjectsUpdateDescParams; result: unknown };

  // ── Chat History & Search ──
  talkTail: { params: TalkTailParams; result: { messages: unknown[] } };
  searchHistory: { params: SearchHistoryParams; result: SearchHistoryResult };
  docUpload: { params: { file_path: string }; result: unknown };
  export: { params: ExportParams; result: ExportResult };

  // ── Spec Workflow ──
  specNew: { params: SpecNewParams; result: unknown };
  specList: { params: void; result: { specs: unknown[] } };
  specShow: { params: SpecParams; result: unknown };
  specStatus: { params: SpecParams; result: unknown };
  specRun: { params: SpecRunParams; result: unknown };
  specEdit: { params: SpecEditParams; result: unknown };

  // ── Team Collaboration ──
  teamSpawn: { params: TeamSpawnParams; result: unknown };
  teamList: { params: void; result: { teams: unknown[] } };
  teamStatus: { params: TeamParams; result: unknown };
  teamResults: { params: TeamParams; result: unknown };
  teamAbort: { params: TeamParams; result: unknown };
  teamExecute: { params: TeamParams; result: unknown };

  // ── Prompt & Flow Templates ──
  templateList: { params: void; result: { templates: unknown[] } };
  templateCreate: { params: { json: string }; result: unknown };
  templateDelete: { params: { name: string }; result: unknown };
  templateExport: { params: { name: string }; result: unknown };
  templateImport: { params: { url: string }; result: unknown };

  // ── Model & Routing ──
  modelList: { params: void; result: { models: unknown[] } };
  modelRoute: { params: { task: string }; result: unknown };
  modelBudget: { params: void; result: unknown };

  // ── Telemetry & Observability ──
  telemetryStats: { params: void; result: unknown };
  telemetryTrends: { params: { days: number }; result: unknown };
  telemetryExport: { params: { format: string }; result: unknown };
  "telemetry.setEnabled": { params: { enabled: boolean }; result: unknown };

  // ── Permission Gate & Rules ──
  permissionStatus: { params: void; result: unknown };
  permissionGrant: {
    params: {
      tool: string;
      action: string;
      target: string;
      permanent: boolean;
    };
    result: unknown;
  };
  permissionRevoke: {
    params: { tool: string; action: string; target: string };
    result: unknown;
  };
  "permissions.info": { params: void; result: unknown };
  "permissions.addRule": {
    params: { category: string; tool_name: string; rule_content?: string };
    result: unknown;
  };
  "permissions.removeRule": {
    params: { category: string; tool_name: string; rule_content?: string };
    result: unknown;
  };
  "permissions.setMode": { params: { mode: string }; result: unknown };
  "permissions.setAutoAllow": {
    params: { channel: string; enabled: boolean };
    result: unknown;
  };
  "permissions.setAskTimeout": { params: { seconds: number }; result: unknown };
  "permissions.setPersistGrants": {
    params: { enabled: boolean };
    result: unknown;
  };
  "evolution.rateTrajectory": {
    params: { rating: string };
    result: { success: boolean; [key: string]: unknown };
  };

  // ── Session Metrics & Config ──
  "session.tokens": { params: void; result: SessionTokensResult };
  "session.cost": { params: void; result: SessionCostResult };
  "session.info": { params: void; result: SessionInfoResult };
  "config.model": {
    params: void;
    result: { model: string; [key: string]: unknown };
  };
  "config.show": { params: void; result: Record<string, unknown> };
}

export type IpcMethodName = keyof IpcMethodMap;
