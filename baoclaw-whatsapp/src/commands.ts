/**
 * Command system for BaoClaw WhatsApp Gateway.
 * Provides command definitions, parsing, dispatch, formatting, and help.
 * Commands are dispatched via IPC JSON-RPC to baoclaw-core daemon.
 */
import { IpcClient } from "baoclaw-ipc/client";
import {
  formatToolHealth,
  type ControlChannel,
  type ToolHealthData,
} from "baoclaw-ipc";
import * as fs from "fs";
import * as os from "os";

// ═══════════════════════════════════════════════════════════════
// Constants
// ═══════════════════════════════════════════════════════════════

const MAX_OUTPUT = 4000;
/** Characters of log output shown by /logs. */
const LOG_TAIL_CHARS = 3000;
/** Characters of each tool call's content shown in history listings. */
const TOOL_CONTENT_PREVIEW_CHARS = 100;

// ═══════════════════════════════════════════════════════════════
// Interface Definitions
// ═══════════════════════════════════════════════════════════════

export interface Command {
  name: string;
  description: string;
  usage?: string;
  handler: (ctx: CommandContext) => Promise<string | void>;
}

export interface CommandContext {
  ipcClient: IpcClient;
  /** Dedicated connection for mid-turn RPCs (abort) — see attachControlChannel. */
  control: ControlChannel;
  args: string;
  sender: string;
  jid: string;
  sock: any;
}

// ═══════════════════════════════════════════════════════════════
// RPC Response Types
// ═══════════════════════════════════════════════════════════════

interface ToolInfo {
  name: string;
  description: string;
  type: string; // 'builtin' | 'mcp' | 'plugin'
}

interface SkillInfo {
  name: string;
  path: string;
  source: string; // 'project' | 'global'
  description?: string;
}

interface McpServerInfo {
  name: string;
  server_type: string; // 'stdio' | 'sse'
  disabled: boolean;
  source: string;
  command?: string;
  url?: string;
  config_path: string;
}

interface PluginInfo {
  name: string;
  version?: string;
  description?: string;
  path: string;
  source: string;
  has_tools: boolean;
  has_skills: boolean;
  has_mcp: boolean;
}

interface CompactResult {
  tokens_saved: number;
  summary_tokens: number;
  tokens_before: number;
  tokens_after: number;
}

interface GitStatusResult {
  branch: string | null;
  has_changes: boolean;
  staged_files: string[];
  modified_files: string[];
  untracked_files: string[];
}

interface GitCommitResult {
  hash: string;
  message: string;
}

interface GitDiffResult {
  diff: string;
}

interface SearchResult {
  timestamp?: string;
  snippet?: string;
  /** Present in the active-session fallback shape (DB unavailable). */
  role?: string;
  text?: string;
  session_id?: string;
  cwd?: string;
}

interface HistoryEntry {
  role: string;
  content: string;
  timestamp?: string;
}

interface ExportResult {
  file_path: string;
  message_count: number;
  size_bytes: number;
}

interface TaskInfo {
  id: string;
  description: string;
  /** Daemon serializes failures as { Failed: "..." }, the rest as plain strings. */
  status: string | { Failed: string };
  created_at?: string;
}

interface CronEntry {
  id: string;
  schedule: string;
  command: string;
  enabled: boolean;
}

interface ProjectInfo {
  id: string;
  name: string;
  path: string;
  description?: string;
}

// ═══════════════════════════════════════════════════════════════
// Private Formatting Helpers
// ═══════════════════════════════════════════════════════════════

/** Truncate text to MAX_OUTPUT characters with ellipsis indicator. */
function truncate(text: string, limit: number = MAX_OUTPUT): string {
  if (text.length <= limit) return text;
  return text.slice(0, limit) + "\n…(output truncated)";
}

/** Format a generic list (tools/skills/mcp/plugins). */
function formatItemList(
  emoji: string,
  title: string,
  items: string[],
  count: number,
): string {
  if (count === 0) return `${emoji} *${title}*\nNothing here yet`;
  let out = `📋 *${title}* (${count})\n`;
  for (const item of items) {
    out += `• ${item}\n`;
  }
  return truncate(out);
}

function formatTools(tools: ToolInfo[]): string {
  const count = tools.length;
  if (count === 0) return "📋 *Registered Tools* (0)\nNo registered tools.";

  // Group by type
  const groups: Record<string, ToolInfo[]> = {};
  for (const t of tools) {
    const type = t.type || "other";
    if (!groups[type]) groups[type] = [];
    groups[type].push(t);
  }

  let out = `📋 *Registered Tools* (${count})\n`;
  for (const [type, items] of Object.entries(groups)) {
    out += `\n── ${type} (${items.length}) ──\n`;
    for (const t of items) {
      const desc = t.description
        ? t.description.length > 60
          ? t.description.slice(0, 60) + "…"
          : t.description
        : "";
      out += `• ${t.name}  ${desc}\n`;
    }
  }
  return truncate(out);
}

function formatSkills(skills: SkillInfo[]): string {
  const count = skills.length;
  if (count === 0) return "📋 *Loaded Skills* (0)\nNo loaded skills.";
  let out = `📋 *Loaded Skills* (${count})\n`;
  for (const s of skills) {
    out += `• ${s.name} [${s.source}]\n`;
    if (s.description) {
      out += `  ${s.description}\n`;
    }
  }
  return truncate(out);
}

function formatMcpServers(servers: McpServerInfo[]): string {
  const count = servers.length;
  if (count === 0) return "📋 *MCP Servers* (0)\nNo MCP servers configured.";
  let out = `📋 *MCP Servers* (${count})\n`;
  for (const srv of servers) {
    const status = srv.disabled ? "🔴" : "🟢";
    out += `${status} ${srv.name}  [${srv.server_type}] [${srv.source}]\n`;
  }
  return truncate(out);
}

function formatPlugins(plugins: PluginInfo[]): string {
  const count = plugins.length;
  if (count === 0) return "📋 *Installed Plugins* (0)\nNo installed plugins.";
  let out = `📋 *Installed Plugins* (${count})\n`;
  for (const p of plugins) {
    const ver = p.version ? ` v${p.version}` : "";
    const features: string[] = [];
    if (p.has_tools) features.push("tools");
    if (p.has_skills) features.push("skills");
    if (p.has_mcp) features.push("mcp");
    const featureStr = features.length > 0 ? ` (${features.join(", ")})` : "";
    out += `• ${p.name}${ver} [${p.source}]${featureStr}\n`;
    if (p.description) {
      out += `  ${p.description}\n`;
    }
  }
  return truncate(out);
}

function formatCompact(result: CompactResult): string {
  const pct =
    result.tokens_before > 0
      ? ((result.tokens_saved / result.tokens_before) * 100).toFixed(0)
      : "0";
  return (
    `✅ *Context Compacted*\n\n` +
    `Before  ${result.tokens_before.toLocaleString()} tokens\n` +
    `After   ${result.tokens_after.toLocaleString()} tokens\n` +
    `Saved   ${result.tokens_saved.toLocaleString()} tokens (${pct}%)\n` +
    `Summary ${result.summary_tokens.toLocaleString()} tokens`
  );
}

function formatGitStatus(result: GitStatusResult): string {
  const branch = result.branch ?? "(detached)";
  let out = `📂 *Git Status*\n\nBranch: *${branch}*\n`;
  if (result.staged_files.length > 0) {
    out += `\nStaged files (${result.staged_files.length}):\n`;
    for (const f of result.staged_files) out += `  ✅ ${f}\n`;
  }
  if (result.modified_files.length > 0) {
    out += `\nModified files (${result.modified_files.length}):\n`;
    for (const f of result.modified_files) out += `  ✏️ ${f}\n`;
  }
  if (result.untracked_files.length > 0) {
    out += `\nUntracked files (${result.untracked_files.length}):\n`;
    for (const f of result.untracked_files) out += `  ❓ ${f}\n`;
  }
  if (
    result.staged_files.length === 0 &&
    result.modified_files.length === 0 &&
    result.untracked_files.length === 0
  ) {
    out += "\nWorking tree clean, no changes.";
  }
  return out;
}

function formatGitDiff(result: GitDiffResult): string {
  if (!result.diff || result.diff.trim() === "") return "No changes.";
  return truncate(`📝 *Git Diff*\n\n\`\`\`\n${result.diff}\n\`\`\``);
}

function formatGitCommit(result: GitCommitResult): string {
  return `✅ *Committed*\n\nHash: \`${result.hash}\`\nMessage: ${result.message}`;
}

function formatHistory(entries: HistoryEntry[], count: number): string {
  if (!entries || entries.length === 0) return "No conversation history.";
  let out = `📜 *Recent Conversation* (${entries.length})\n\n`;
  for (const e of entries) {
    const role = e.role === "user" ? "👤" : "🤖";
    const content =
      e.content.length > TOOL_CONTENT_PREVIEW_CHARS
        ? e.content.slice(0, TOOL_CONTENT_PREVIEW_CHARS) + "…"
        : e.content;
    out += `${role} ${content}\n\n`;
    if (out.length > MAX_OUTPUT) {
      out += "…(more truncated)";
      break;
    }
  }
  return out;
}

function formatSearchResults(results: SearchResult[], query: string): string {
  if (!results || results.length === 0)
    return `No results found for "${query}"`;
  let out = `🔍 *Search Results*: "${query}" (${results.length})\n\n`;
  for (const r of results) {
    const ts = r.timestamp?.slice(0, 19).replace("T", " ") || "";
    const role = r.role === "user" ? "👤" : "🤖";
    const body = r.snippet || r.text || "";
    out += `[${ts}] ${role}\n${body}\n\n`;
    if (out.length > MAX_OUTPUT) {
      out += "…(more results truncated)";
      break;
    }
  }
  return out;
}

function formatExport(result: ExportResult): string {
  const size = result.size_bytes
    ? `\nSize: ${(result.size_bytes / 1024).toFixed(1)} KB`
    : "";
  const count = result.message_count
    ? `\nMessages: ${result.message_count}`
    : "";
  return `📤 *Export Complete*\n\nPath: ${result.file_path}${size}${count}`;
}

function formatProjects(projects: ProjectInfo[]): string {
  const count = projects.length;
  if (count === 0) return "📋 *Projects* (0)\nNo projects.";
  let out = `📋 *Projects* (${count})\n\n`;
  for (const p of projects) {
    out += `• *${p.name}* [${p.id}]\n  ${p.path}\n`;
    if (p.description) out += `  ${p.description}\n`;
    out += "\n";
  }
  return truncate(out);
}

/** Render a TaskInfo status regardless of its serialized shape. */
function taskStatusLabel(status: TaskInfo["status"]): string {
  if (typeof status === "string") return status;
  if (status && typeof status === "object" && "Failed" in status) {
    return `Failed: ${status.Failed}`;
  }
  return JSON.stringify(status);
}

function formatTasks(tasks: TaskInfo[]): string {
  const count = tasks.length;
  if (count === 0) return "📋 *Tasks* (0)\nNo background tasks.";
  let out = `📋 *Background Tasks* (${count})\n\n`;
  for (const t of tasks) {
    const label = taskStatusLabel(t.status);
    const lower = label.toLowerCase();
    const statusEmoji = lower.startsWith("running")
      ? "🟢"
      : lower.startsWith("completed")
        ? "✅"
        : lower.startsWith("failed")
          ? "🔴"
          : "⚪";
    out += `${statusEmoji} [${t.id}] ${t.description}\n  Status: ${label}\n\n`;
  }
  return truncate(out);
}

function formatCronList(crons: CronEntry[]): string {
  const count = crons.length;
  if (count === 0) return "📋 *Cron Jobs* (0)\nNo cron jobs.";
  let out = `📋 *Cron Jobs* (${count})\n\n`;
  for (const c of crons) {
    const status = c.enabled ? "🟢" : "🔴";
    out += `${status} [${c.id}] \`${c.schedule}\` ${c.command}\n`;
  }
  return truncate(out);
}

function formatSpecList(specs: SpecSummary[]): string {
  const count = specs.length;
  if (count === 0) return "📋 *Specs* (0)\nNo specs.";
  let out = `📋 *Specs* (${count})\n\n`;
  for (const s of specs) {
    const progress = s.task_progress
      ? ` (${s.task_progress.completed}/${s.task_progress.total} tasks)`
      : "";
    out += `• ${s.feature_name} [${s.phase}]${progress}\n`;
  }
  return out;
}

function formatSpecShow(spec: SpecSummary): string {
  let out = `📋 *Spec: ${spec.feature_name}*\n`;
  out += `Workflow: ${spec.workflow}\nPhase: ${spec.phase}\nType: ${spec.spec_type}\n`;
  if (spec.task_progress) {
    out += `Tasks: ${spec.task_progress.completed}/${spec.task_progress.total} (${spec.task_progress.in_progress} in progress)\n`;
  }
  return truncate(out);
}

function formatSpecStatus(
  name: string,
  progress: { total: number; completed: number; in_progress: number },
): string {
  return (
    `📊 *Spec Status: ${name}*\n\n` +
    `Total: ${progress.total}\n` +
    `✅ Completed: ${progress.completed}\n` +
    `🔄 In progress: ${progress.in_progress}`
  );
}

function formatSpecRun(result: {
  task_id?: string;
  task_description?: string;
  status: string;
  message?: string;
}): string {
  if (result.message) {
    return `🚀 *Spec Execution*\n\n${result.message}`;
  }
  const description = result.task_description
    ? `\n\n${result.task_description}`
    : "";
  return `🚀 *Next Task*\n\nTask ID: ${result.task_id || "N/A"}${description}`;
}

function formatStatus(
  daemonInfo: { pid: number; session_id: string; cwd: string } | null,
  ipcClient: IpcClient,
): string {
  const connected = ipcClient.connected ? "🟢 Connected" : "🔴 Disconnected";
  let out = `🐾 *BaoClaw WhatsApp Gateway*\n\n`;
  out += `Daemon connection: ${connected}\n`;
  if (daemonInfo) {
    out += `Daemon PID: ${daemonInfo.pid}\n`;
    out += `Session: ${daemonInfo.session_id}\n`;
    out += `CWD: ${daemonInfo.cwd}\n`;
  }
  out += `Reconnects: ${_daemonMetrics.reconnectCount}\n`;
  out += `Last connect: ${_daemonMetrics.lastConnectAt?.toISOString() ?? "never"}\n`;
  return out;
}

function formatError(title: string, detail: string): string {
  return `❌ *${title}*\n${detail}`;
}

// ═══════════════════════════════════════════════════════════════
// Command Definitions
// ═══════════════════════════════════════════════════════════════

// We need a mutable reference to daemonInfo for /status.
// This will be set by the gateway when it creates the commands module.
let _daemonInfo: { pid: number; session_id: string; cwd: string } | null = null;
let _daemonMetrics = { reconnectCount: 0, lastConnectAt: null as Date | null };

/** Set the daemon info reference (called by gateway after connection). */
export function setDaemonInfo(
  info: { pid: number; session_id: string; cwd: string } | null,
): void {
  _daemonInfo = info;
}

export function setDaemonMetrics(metrics: typeof _daemonMetrics): void {
  _daemonMetrics = metrics;
}

// ── Conversation Commands ──

const compactCommand: Command = {
  name: "/compact",
  description: "Compact conversation context",
  async handler(ctx) {
    const result = await ctx.ipcClient.request<CompactResult>("compact");
    return formatCompact(result);
  },
};

const thinkCommand: Command = {
  name: "/think",
  description: "Extended thinking mode tips",
  async handler(_ctx) {
    return (
      "🧠 *Extended Thinking*\n\n" +
      "Just send a message describing what needs deep thought.\n" +
      "The AI will analyze and reason in more detail.\n\n" +
      "For example:\n" +
      "• Ask a complex question directly\n" +
      "• Ask for an analysis of a piece of code\n" +
      "• Ask for a math problem to be solved step by step"
    );
  },
};

const modelCommand: Command = {
  name: "/model",
  description: "View or switch model",
  usage: "/model [name]",
  async handler(ctx) {
    if (!ctx.args.trim()) {
      return "🤖 *Model Info*\n\nAsk the AI directly for current model info.\n\nUsage: `/model <model-name>` to switch model";
    }
    const result = await ctx.ipcClient.request<{ model: string }>(
      "switchModel",
      { model: ctx.args.trim() },
    );
    return `✅ *Switched to model:* ${result.model ?? ctx.args.trim()}`;
  },
};

const historyCommand: Command = {
  name: "/history",
  description: "Show recent conversation",
  usage: "/history [n]",
  async handler(ctx) {
    const n = parseInt(ctx.args.trim(), 10) || 10;
    const result = await ctx.ipcClient.request<{ entries: HistoryEntry[] }>(
      "talkTail",
      { n },
    );
    return formatHistory(
      result.entries ?? (result as unknown as HistoryEntry[]),
      n,
    );
  },
};

const searchCommand: Command = {
  name: "/search",
  description: "Search conversation history",
  usage: "/search <query>",
  async handler(ctx) {
    if (!ctx.args.trim()) {
      return formatError("Missing argument", "Usage: /search <query>");
    }
    const result = await ctx.ipcClient.request<{
      results: SearchResult[];
      count: number;
    }>("searchHistory", { query: ctx.args.trim() });
    return formatSearchResults(result.results ?? [], ctx.args.trim());
  },
};

const exportCommand: Command = {
  name: "/export",
  description: "Export conversation history to a file",
  async handler(ctx) {
    const result = await ctx.ipcClient.request<ExportResult>("export");
    const text = formatExport(result);
    // Return text + export path for gateway to handle file sending
    return text;
  },
};

const abortCommand: Command = {
  name: "/abort",
  description: "Abort the current task",
  async handler(ctx) {
    await ctx.control.request("abort");
    return "⛔ Current task aborted.";
  },
};

// ── Project & Git Commands ──

const projectsCommand: Command = {
  name: "/projects",
  description: "List projects",
  async handler(ctx) {
    const result = await ctx.ipcClient.request<
      { projects: ProjectInfo[] } | ProjectInfo[]
    >("projectsList");
    const projects = Array.isArray(result)
      ? result
      : ((result as any).projects ?? []);
    return formatProjects(projects);
  },
};

const gitCommand: Command = {
  name: "/git",
  description: "Show git status",
  async handler(ctx) {
    const result = await ctx.ipcClient.request<GitStatusResult>("gitStatus");
    return formatGitStatus(result);
  },
};

const diffCommand: Command = {
  name: "/diff",
  description: "Show git diff",
  async handler(ctx) {
    const result = await ctx.ipcClient.request<GitDiffResult>("gitDiff");
    return formatGitDiff(result);
  },
};

const commitCommand: Command = {
  name: "/commit",
  description: "Commit git changes",
  usage: "/commit <message>",
  async handler(ctx) {
    if (!ctx.args.trim()) {
      return formatError(
        "Missing argument",
        "Usage: /commit <message>\n\nPlease provide a commit message, e.g.:\n/commit Fix login page styling",
      );
    }
    const result = await ctx.ipcClient.request<GitCommitResult>("gitCommit", {
      message: ctx.args.trim(),
    });
    return formatGitCommit(result);
  },
};

// ── Tools & Extensions Commands ──

const toolsCommand: Command = {
  name: "/tools",
  description: "List registered tools",
  async handler(ctx) {
    const result = await ctx.ipcClient.request<
      { tools: ToolInfo[] } | ToolInfo[]
    >("listTools");
    const tools = Array.isArray(result)
      ? result
      : ((result as any).tools ?? []);
    return formatTools(tools);
  },
};

const healthCommand: Command = {
  name: "/health",
  description: "Tool health overview",
  usage: "/health [all]",
  async handler(ctx) {
    const data = await ctx.ipcClient.request<ToolHealthData>("toolHealth", {});
    return formatToolHealth(data, { verbose: ctx.args.trim() === "all" });
  },
};

const mcpCommand: Command = {
  name: "/mcp",
  description: "List MCP servers",
  async handler(ctx) {
    const result = await ctx.ipcClient.request<
      { servers: McpServerInfo[] } | McpServerInfo[]
    >("listMcpServers");
    const servers = Array.isArray(result)
      ? result
      : ((result as any).servers ?? []);
    return formatMcpServers(servers);
  },
};

const skillsCommand: Command = {
  name: "/skills",
  description: "List loaded skills",
  async handler(ctx) {
    const result = await ctx.ipcClient.request<
      { skills: SkillInfo[] } | SkillInfo[]
    >("listSkills");
    const skills = Array.isArray(result)
      ? result
      : ((result as any).skills ?? []);
    return formatSkills(skills);
  },
};

const pluginsCommand: Command = {
  name: "/plugins",
  description: "List installed plugins",
  async handler(ctx) {
    const result = await ctx.ipcClient.request<
      { plugins: PluginInfo[] } | PluginInfo[]
    >("listPlugins");
    const plugins = Array.isArray(result)
      ? result
      : ((result as any).plugins ?? []);
    return formatPlugins(plugins);
  },
};

// ── Automation Commands ──

/** Stop a background task via the daemon contract (task_id param). */
async function stopBackgroundTask(
  ipcClient: CommandContext["ipcClient"],
  taskId: string,
): Promise<string> {
  const result = await ipcClient.request<{ stopped: boolean }>("taskStop", {
    task_id: taskId,
  });
  return result?.stopped
    ? `⏹️ *Task Stopped*\n\nID: ${taskId}`
    : `⚠️ Task ${taskId} was not running or not found.`;
}

const taskCommand: Command = {
  name: "/task",
  description: "Create a background task",
  usage: "/task <description> | /task stop <id>",
  async handler(ctx) {
    const args = ctx.args.trim();
    if (!args) {
      return formatError(
        "Missing argument",
        "Usage: /task <description>\n\nExample:\n/task Analyze code quality in the src directory",
      );
    }
    // "/task stop <id>" stops a running task. Only "stop" followed by
    // exactly one id counts as a stop — a bare "stop" gets usage, and a
    // plain-language description starting with "stop" still creates a task.
    const tokens = args.split(/\s+/);
    if (tokens[0].toLowerCase() === "stop" && tokens.length <= 2) {
      if (tokens.length === 1) {
        return formatError("Missing argument", "Usage: /task stop <task-id>");
      }
      return stopBackgroundTask(ctx.ipcClient, tokens[1]);
    }
    const result = await ctx.ipcClient.request<{ task_id: string }>(
      "taskCreate",
      // Both fields are required by the daemon; the prompt is the same text.
      { description: args, prompt: args },
    );
    return `🚀 *Task Created*\n\nID: ${result.task_id}`;
  },
};

const tasksCommand: Command = {
  name: "/tasks",
  description: "List background tasks",
  async handler(ctx) {
    const result = await ctx.ipcClient.request<
      { tasks: TaskInfo[] } | TaskInfo[]
    >("taskList");
    const tasks = Array.isArray(result)
      ? result
      : ((result as any).tasks ?? []);
    return formatTasks(tasks);
  },
};

const taskStopCommand: Command = {
  name: "/task_stop",
  description: "Stop a background task (alias of /task stop)",
  usage: "/task_stop <id>",
  async handler(ctx) {
    const taskId = ctx.args.trim();
    if (!taskId) {
      return formatError("Missing argument", "Usage: /task_stop <task-id>");
    }
    return stopBackgroundTask(ctx.ipcClient, taskId);
  },
};

const cronCommand: Command = {
  name: "/cron",
  description: "List cron jobs",
  async handler(ctx) {
    const result = await ctx.ipcClient.request<
      { crons: CronEntry[] } | CronEntry[]
    >("cronList");
    const crons = Array.isArray(result)
      ? result
      : ((result as any).crons ?? []);
    return formatCronList(crons);
  },
};

// ── Session Commands ──

const helpCommand: Command = {
  name: "/help",
  description: "Show help",
  async handler(_ctx) {
    return formatHelp();
  },
};

const statusCommand: Command = {
  name: "/status",
  description: "Show gateway status",
  async handler(ctx) {
    return formatStatus(_daemonInfo, ctx.ipcClient);
  },
};

const startCommand: Command = {
  name: "/start",
  description: "Show welcome message",
  async handler(_ctx) {
    return (
      "🐾 *BaoClaw WhatsApp Gateway*\n\n" +
      "Welcome to BaoClaw!\n\n" +
      "You can chat with the AI directly by sending messages, or use / commands.\n\n" +
      "Type `/help` to see all available commands."
    );
  },
};

const clearCommand: Command = {
  name: "/clear",
  description: "Clear local cache",
  async handler(_ctx) {
    return "🧹 Local cache cleared";
  },
};

// ── Gateway Info Store ──

export interface GatewayInfo {
  pid: number;
  startTime: number;
  logFile: string;
  name: string;
}

let _gatewayInfo: GatewayInfo | null = null;

export function setGatewayInfo(info: GatewayInfo): void {
  _gatewayInfo = info;
}

// ── Gateway Management Command ──

const gatewayCommand: Command = {
  name: "/gateway",
  description: "Gateway management (info only, does not kill the process)",
  usage: "/gateway status|ping|logs [n]",
  async handler(_ctx) {
    const args = _ctx.args.trim();
    const parts = args.split(/\s+/);
    const sub = parts[0] || "status";

    switch (sub) {
      case "status": {
        if (!_gatewayInfo) return "⚠️ Gateway info not initialized";
        const uptime = Math.floor((Date.now() - _gatewayInfo.startTime) / 1000);
        const mem = process.memoryUsage();
        let out = `🐾 *${_gatewayInfo.name} Gateway*\n\n`;
        out += `PID: ${_gatewayInfo.pid}\n`;
        out += `Uptime: ${Math.floor(uptime / 3600)}h ${Math.floor((uptime % 3600) / 60)}m ${uptime % 60}s\n`;
        out += `Memory RSS: ${(mem.rss / 1024 / 1024).toFixed(1)} MB\n`;
        out += `Memory Heap: ${(mem.heapUsed / 1024 / 1024).toFixed(1)} / ${(mem.heapTotal / 1024 / 1024).toFixed(1)} MB\n`;
        out += `Node.js: ${process.version}\n`;
        out += `Platform: ${os.platform()} ${os.arch()}\n`;
        out += `System uptime: ${Math.floor(os.uptime() / 3600)}h\n`;
        out += `Log: ${_gatewayInfo.logFile}\n`;
        out += `Daemon: ${_daemonInfo ? `🟢 pid=${_daemonInfo.pid}` : "🔴 Not connected"}\n`;
        return out;
      }
      case "ping":
        return "🏓 pong! Gateway is alive.";
      case "logs": {
        if (!_gatewayInfo) return "⚠️ Gateway info not initialized";
        const n = parseInt(parts[1], 10) || 10;
        try {
          if (!fs.existsSync(_gatewayInfo.logFile))
            return "⚠️ Log file not found";
          const content = fs.readFileSync(_gatewayInfo.logFile, "utf-8");
          const lines = content.trim().split("\n");
          const recent = lines.slice(-Math.min(n, 50));
          if (recent.length === 0) return "📄 Log is empty";
          return `📄 *Last ${recent.length} log lines*\n\n\`\`\`\n${recent.join("\n").slice(0, LOG_TAIL_CHARS)}\n\`\`\``;
        } catch (e: any) {
          return `⚠️ Unable to read log: ${e.message}`;
        }
      }
      default:
        return `📋 *Gateway Commands*\n\n• /gateway status — gateway runtime status\n• /gateway ping — connectivity test\n• /gateway logs [n] — last n log lines`;
    }
  },
};

// ── Spec Commands (subcommand system) ──

interface SpecSummary {
  feature_name: string;
  workflow: string;
  phase: string;
  spec_type: string;
  task_progress: {
    total: number;
    completed: number;
    in_progress: number;
  } | null;
}

const specCommand: Command = {
  name: "/spec",
  description: "Spec management",
  usage: "/spec list|new|show|status|run",
  async handler(ctx) {
    const parts = ctx.args.trim().split(/\s+/);
    const sub = parts[0] || "";
    const rest = parts.slice(1).join(" ");

    switch (sub) {
      case "list": {
        const result = await ctx.ipcClient.request<
          { specs: SpecSummary[] } | SpecSummary[]
        >("specList");
        const specs = Array.isArray(result)
          ? result
          : ((result as any).specs ?? []);
        return formatSpecList(specs);
      }

      case "new": {
        const name = parts[1];
        if (!name) {
          return formatError("Missing argument", "Usage: /spec new <name>");
        }
        // Optional flag tokens, mirroring the CLI: [design] [bugfix].
        const params: Record<string, string> = { feature_name: name };
        if (parts.slice(1).includes("design")) params.workflow = "design";
        if (parts.slice(1).includes("bugfix")) params.spec_type = "bugfix";
        const result = await ctx.ipcClient.request<{
          feature_name: string;
          config: { workflow: string; phase: string };
        }>("specNew", params);
        return `✅ *Spec Created*\n\nName: ${result.feature_name}\nWorkflow: ${result.config?.workflow}\nPhase: ${result.config?.phase}`;
      }

      case "show": {
        const name = parts[1];
        if (!name) {
          return formatError("Missing argument", "Usage: /spec show <name>");
        }
        const result = await ctx.ipcClient.request<SpecSummary>("specShow", {
          feature_name: name,
        });
        return formatSpecShow(result);
      }

      case "status": {
        const name = parts[1];
        if (!name) {
          return formatError("Missing argument", "Usage: /spec status <name>");
        }
        const result = await ctx.ipcClient.request<{
          total: number;
          completed: number;
          in_progress: number;
        }>("specStatus", { feature_name: name });
        return formatSpecStatus(name, result);
      }

      case "run": {
        const name = parts[1];
        if (!name) {
          return formatError(
            "Missing argument",
            "Usage: /spec run <name> [task_id]",
          );
        }
        const taskId = parts[2];
        const params: Record<string, string> = { feature_name: name };
        if (taskId) params.task_id = taskId;
        const result = await ctx.ipcClient.request<{
          task_id?: string;
          task_description?: string;
          status: string;
          message?: string;
        }>("specRun", params);
        return formatSpecRun(result);
      }

      default:
        return (
          "📋 *Spec Commands*\n\n" +
          "• `/spec list` — list all specs\n" +
          "• `/spec new <name> [design] [bugfix]` — create a new spec\n" +
          "• `/spec show <name>` — view spec summary\n" +
          "• `/spec status <name>` — task progress counts\n" +
          "• `/spec run <name> [task_id]` — show next pending task"
        );
    }
  },
};

// ═══════════════════════════════════════════════════════════════
// Command Registry
// ═══════════════════════════════════════════════════════════════

export const COMMAND_REGISTRY: Record<string, Command> = {
  // Conversation
  "/compact": compactCommand,
  "/think": thinkCommand,
  "/model": modelCommand,
  "/history": historyCommand,
  "/search": searchCommand,
  "/export": exportCommand,
  "/abort": abortCommand,

  // Project & Git
  "/projects": projectsCommand,
  "/git": gitCommand,
  "/diff": diffCommand,
  "/commit": commitCommand,

  // Tools & Extensions
  "/tools": toolsCommand,
  "/health": healthCommand,
  "/mcp": mcpCommand,
  "/skills": skillsCommand,
  "/plugins": pluginsCommand,

  // Automation
  "/task": taskCommand,
  "/tasks": tasksCommand,
  "/task_stop": taskStopCommand,
  "/cron": cronCommand,

  // Session
  "/help": helpCommand,
  "/status": statusCommand,
  "/start": startCommand,
  "/clear": clearCommand,

  // Gateway
  "/gateway": gatewayCommand,

  // Spec
  "/spec": specCommand,
};

// ═══════════════════════════════════════════════════════════════
// Command Parsing & Dispatch
// ═══════════════════════════════════════════════════════════════

/**
 * Parse "/command args" format from message text.
 * Returns null if text is not a slash command.
 */
export function parseCommand(
  text: string,
): { name: string; args: string } | null {
  if (!text.startsWith("/")) return null;
  const trimmed = text.trim();
  const spaceIdx = trimmed.indexOf(" ");
  if (spaceIdx === -1) {
    return { name: trimmed.toLowerCase(), args: "" };
  }
  return {
    name: trimmed.slice(0, spaceIdx).toLowerCase(),
    args: trimmed.slice(spaceIdx + 1).trim(),
  };
}

/**
 * Check whether a command name is registered.
 */
export function isRegisteredCommand(name: string): boolean {
  return name in COMMAND_REGISTRY;
}

/**
 * Dispatch a parsed command to its handler.
 * Returns the formatted response string, or null if not a registered command.
 * If the handler returns void, returns null.
 */
export async function dispatchCommand(
  ctx: CommandContext,
): Promise<string | null> {
  const parsed = parseCommand(ctx.args);
  if (!parsed) return null;

  const command = COMMAND_REGISTRY[parsed.name];
  if (!command) return null;

  const commandCtx: CommandContext = {
    ...ctx,
    args: parsed.args,
  };

  try {
    const result = await command.handler(commandCtx);
    return typeof result === "string" ? result : null;
  } catch (err: any) {
    const message = err instanceof Error ? err.message : String(err);
    return formatError("Command failed", message);
  }
}

// ═══════════════════════════════════════════════════════════════
// Help Text
// ═══════════════════════════════════════════════════════════════

/**
 * Generate help text listing all registered commands.
 * Uses WhatsApp formatting: *bold*, _italic_, ```code```.
 */
export function formatHelp(): string {
  const groups: [string, string[]][] = [
    [
      "💬 *Conversation*",
      [
        "/compact",
        "/think",
        "/model",
        "/history",
        "/search",
        "/export",
        "/abort",
      ],
    ],
    ["📂 *Project & Git*", ["/projects", "/git", "/diff", "/commit"]],
    [
      "🔧 *Tools & Extensions*",
      ["/tools", "/health", "/mcp", "/skills", "/plugins"],
    ],
    ["⚙️ *Automation*", ["/task", "/tasks", "/task_stop", "/cron"]],
    ["📋 *Spec*", ["/spec"]],
    ["🚪 *Gateway*", ["/gateway"]],
    ["🔌 *Session*", ["/help", "/status", "/start", "/clear"]],
  ];

  let out = "📖 *BaoClaw Commands*\n\n";

  for (const [group, cmds] of groups) {
    out += `${group}\n`;
    for (const cmd of cmds) {
      const def = COMMAND_REGISTRY[cmd];
      if (def) {
        const usage = def.usage ? ` \`${def.usage}\`` : "";
        out += `  ${cmd} — ${def.description}${usage}\n`;
      }
    }
    out += "\n";
  }

  out += "_Send any non-command message to chat with the AI_";

  return out;
}
