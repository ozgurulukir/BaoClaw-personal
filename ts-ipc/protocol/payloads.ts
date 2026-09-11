/**
 * Strongly-typed payload and parameter schemas for BaoClaw daemon RPCs.
 */

// ── Common IPC Payloads ──

export interface ToolInfo {
  name: string;
  description: string;
  type: string;
  deferred?: boolean;
}

export interface ListToolsResult {
  tools: ToolInfo[];
  count: number;
}

export interface SkillInfo {
  name: string;
  path: string;
  source: string;
  description?: string;
}

export interface ListSkillsResult {
  skills: SkillInfo[];
  count: number;
}

export interface PluginInfo {
  name: string;
  version?: string;
  description?: string;
  path: string;
  source: string;
  has_tools: boolean;
  has_skills: boolean;
  has_mcp: boolean;
}

export interface ListPluginsResult {
  plugins: PluginInfo[];
  count: number;
}

export interface MemoryEntry {
  id: string;
  content: string;
  category: string;
  created_at: string;
  source?: string;
  pinned?: boolean;
  score?: number;
}

export interface MemoryAddParams {
  content: string;
  category?: string;
}

export interface MemoryAddResult {
  memory?: MemoryEntry;
  created: boolean;
}

export interface MemoryListResult {
  memories: MemoryEntry[];
  count: number;
}

export interface MemoryDeleteResult {
  deleted: boolean;
}

export interface MemoryClearResult {
  cleared: number;
}

export interface MemoryStatsResult {
  total: number;
  by_category: Record<string, number>;
  active: number;
  archived: number;
}

export interface GitStatusResult {
  branch: string;
  has_changes: boolean;
  staged_files: string[];
  modified_files: string[];
  untracked_files: string[];
}

export interface GitDiffResult {
  diff: string;
}

export interface GitCommitParams {
  message: string;
}

export interface GitCommitResult {
  hash: string;
  message: string;
}

export interface CompactResult {
  tokens_saved: number;
  summary_tokens: number;
  tokens_before: number;
  tokens_after: number;
}

export interface SessionTokensResult {
  session_id: string;
  current_tokens: number;
  context_window: number;
  usage_percent: number;
  compact_threshold: number;
  threshold_ratio: number;
  tokens_until_compact: number;
  total_input_tokens: number;
  total_output_tokens: number;
  cache_creation_tokens?: number;
  cache_read_tokens?: number;
  message_count: number;
  model: string;
}

export interface SessionCostResult {
  session_cost_usd: number;
  total_input_tokens: number;
  total_output_tokens: number;
  input_cost: number;
  output_cost: number;
  input_price_per_mtok: number;
  output_price_per_mtok: number;
  model: string;
  pricing_configured: boolean;
}

export interface SessionInfoResult {
  session_id: string;
  cwd: string;
  message_count: number;
  client_count: number;
  model: string;
  created_at: string;
  last_active: string;
}

export interface SubmitMessageParams {
  prompt: unknown;
  uuid?: string;
  attachments?: unknown[];
}

export interface SubmitMessageResult {
  turn_id?: string;
  status?: string;
  [key: string]: unknown;
}

export interface PermissionResponseParams {
  tool_use_id: string;
  decision: string;
  rule?: string;
}

export interface InitializeParams {
  cwd: string;
  model?: string;
  settings: Record<string, unknown>;
  protocol_version?: string;
  resume_session_id?: string;
  shared_session_id?: string;
}

export interface InitializeResult {
  session_id: string;
  model: string;
  [key: string]: unknown;
}

export interface TaskCreateParams {
  description: string;
  prompt: string;
}

export interface TaskCreateResult {
  task_id: string;
  status: string;
}

export interface TaskStatusParams {
  task_id: string;
}

export interface TaskStopParams {
  task_id: string;
}

export interface TaskStopResult {
  stopped: boolean;
}

export interface CronAddParams {
  name: string;
  prompt: string;
  schedule: string;
  cwd?: string;
}

export interface CronParams {
  id: string;
}

export interface ProjectsSwitchParams {
  id_prefix: string;
}

export interface ProjectsNewParams {
  cwd: string;
  description?: string;
}

export interface ProjectsUpdateDescParams {
  id_prefix: string;
  description: string;
}

export interface ExportParams {
  output_path?: string;
}

export interface ExportResult {
  file_path: string;
  message_count: number;
  size_bytes: number;
}

export interface DocUploadParams {
  file_path: string;
}

export interface DocUploadResult {
  success: boolean;
  [key: string]: unknown;
}

export interface SearchHistoryParams {
  query: string;
  max_results?: number;
}

export interface SearchHistoryResult {
  results: Array<{
    timestamp?: string;
    session_id?: string;
    snippet?: string;
    text?: string;
    role?: string;
    [key: string]: unknown;
  }>;
  count: number;
}

export interface TalkTailParams {
  count?: number;
}

export interface SpecParams {
  feature_name: string;
}

export interface SpecNewParams {
  feature_name: string;
  workflow?: string;
  spec_type?: string;
}

export interface SpecRunParams {
  feature_name: string;
  task_id?: string;
}

export interface SpecEditParams {
  feature_name: string;
  phase: string;
}

export interface TeamSpawnParams {
  count?: number;
  mode: string;
  task: string;
  policy?: unknown;
}

export interface TeamParams {
  team_id: string;
}
