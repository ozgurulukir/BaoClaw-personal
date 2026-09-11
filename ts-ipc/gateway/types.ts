/**
 * Common RPC types and definitions shared across all BaoClaw gateways
 * (Telegram, WhatsApp, Feishu, Web, CLI).
 */

export interface ToolInfo {
  name: string;
  description: string;
  type: string; // 'builtin' | 'mcp' | 'plugin'
}

export interface SkillInfo {
  name: string;
  path: string;
  source: string; // 'project' | 'global'
  description?: string;
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

export interface CompactResult {
  tokens_saved: number;
  summary_tokens: number;
  tokens_before: number;
  tokens_after: number;
}

export interface GitStatusResult {
  branch: string | null;
  has_changes: boolean;
  staged_files: string[];
  modified_files: string[];
  untracked_files: string[];
}

export interface GitCommitResult {
  hash: string;
  message: string;
}

export interface GitDiffResult {
  diff: string;
}

export interface HistoryEntry {
  role: string;
  text: string;
  timestamp?: string;
}

export interface ExportResult {
  file_path: string;
  message_count: number;
  size_bytes: number;
}

export interface TaskInfo {
  id: string;
  description: string;
  status: string | { Failed: string } | Record<string, unknown>;
  prompt?: string;
}

export interface SpecInfo {
  name: string;
  workflow?: string;
  type?: string;
  task_progress?: {
    total: number;
    completed: number;
    in_progress: number;
  };
}

export interface SpecDetail {
  name: string;
  workflow?: string;
  type?: string;
  phases?: string[];
  current_phase?: string;
  content?: string;
  tasks?: Array<{
    id: string;
    description: string;
    status: string;
  }>;
  task_progress?: {
    total: number;
    completed: number;
    in_progress: number;
  };
}

export interface SpecProgress {
  total: number;
  completed: number;
  in_progress: number;
}

export interface MemoryEntry {
  id: string;
  content: string;
  category: string;
  timestamp?: string;
}

export interface CronJob {
  id: string;
  name: string;
  schedule: string;
  prompt: string;
  enabled: boolean;
  cwd?: string;
  last_run?: string;
}

export interface ProjectInfo {
  id: string;
  name?: string;
  path?: string;
  description?: string;
  cwd?: string;
}

export interface CommandDefinition {
  description: string;
  usage?: string;
}

export interface ParsedCommand {
  command: string;
  args: string;
}

export interface DaemonInfo {
  pid: number;
  session_id: string;
  cwd: string;
}

export interface DaemonMetrics {
  reconnectCount: number;
  lastConnectAt: Date | null;
}
