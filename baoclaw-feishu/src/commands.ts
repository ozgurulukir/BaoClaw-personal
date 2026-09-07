/**
 * Command system for BaoClaw Feishu Gateway.
 * Adapts WhatsApp's commands.ts — same registry, same handlers,
 * but uses chatId/sendReply instead of jid/sock.
 */
import {
  IpcClient,
  formatToolHealth,
  type ControlChannel,
  type ToolHealthData,
} from "baoclaw-ipc";
import { logger } from "./log.js";
import * as fs from "fs";
import * as os from "os";

const MAX_OUTPUT = 4000;
/** Characters of log output shown by /logs. */
const LOG_TAIL_CHARS = 3000;
/** Characters of each tool call's content shown in history listings. */
const TOOL_CONTENT_PREVIEW_CHARS = 100;

// ── Adapted CommandContext for Feishu ──

export interface CommandContext {
  ipcClient: IpcClient;
  /** Dedicated connection for mid-turn RPCs (abort) — see attachControlChannel. */
  control: ControlChannel;
  args: string;
  sender: string;
  chatId: string;
  sendReply: (text: string) => Promise<void>;
}

interface ParsedCommand {
  name: string;
  args: string;
}

// ── RPC Response Types ──

interface ToolInfo {
  name: string;
  description: string;
  type: string;
}
interface SkillInfo {
  name: string;
  path: string;
  source: string;
  description?: string;
}
interface McpServerInfo {
  name: string;
  server_type: string;
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
  entry_type: string;
  snippet?: string;
  context?: string;
}
interface HistoryEntry {
  role: string;
  content: string;
  timestamp?: string;
}
interface ExportResult {
  path: string;
  size?: number;
}
interface TaskInfo {
  id: string;
  description: string;
  status: string;
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
interface SpecInfo {
  name: string;
  phase: string;
  total_tasks: number;
  completed_tasks: number;
}

// ── Formatting Helpers ──

function truncate(text: string, limit: number = MAX_OUTPUT): string {
  if (text.length <= limit) return text;
  return text.slice(0, limit) + "\n…(output truncated)";
}

function formatTools(tools: ToolInfo[]): string {
  const count = tools.length;
  if (count === 0) return "📋 Registered Tools (0)\nNo registered tools.";
  const groups: Record<string, ToolInfo[]> = {};
  for (const t of tools) {
    const type = t.type || "other";
    if (!groups[type]) groups[type] = [];
    groups[type].push(t);
  }
  let out = `📋 Registered Tools (${count})\n`;
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
  if (skills.length === 0) return "📋 Loaded Skills (0)";
  let out = `📋 Loaded Skills (${skills.length})\n`;
  for (const s of skills) {
    out += `• ${s.name} [${s.source}]\n`;
    if (s.description) out += `  ${s.description}\n`;
  }
  return truncate(out);
}

function formatMcpServers(servers: McpServerInfo[]): string {
  if (servers.length === 0) return "📋 MCP Servers (0)";
  let out = `📋 MCP Servers (${servers.length})\n`;
  for (const srv of servers) {
    const status = srv.disabled ? "🔴" : "🟢";
    out += `${status} ${srv.name} [${srv.server_type}] [${srv.source}]\n`;
  }
  return truncate(out);
}

function formatPlugins(plugins: PluginInfo[]): string {
  if (plugins.length === 0) return "📋 Installed Plugins (0)";
  let out = `📋 Installed Plugins (${plugins.length})\n`;
  for (const p of plugins) {
    const ver = p.version ? ` v${p.version}` : "";
    out += `• ${p.name}${ver} [${p.source}]\n`;
  }
  return truncate(out);
}

function formatCompact(result: CompactResult): string {
  const pct =
    result.tokens_before > 0
      ? ((result.tokens_saved / result.tokens_before) * 100).toFixed(0)
      : "0";
  return `✅ Context compacted\n\nBefore: ${result.tokens_before.toLocaleString()} tokens\nAfter: ${result.tokens_after.toLocaleString()} tokens\nSaved: ${result.tokens_saved.toLocaleString()} tokens (${pct}%)\nSummary: ${result.summary_tokens.toLocaleString()} tokens`;
}

function formatGitStatus(result: GitStatusResult): string {
  let out = `📂 Git Status\n\nBranch: ${result.branch ?? "(detached)"}\n`;
  if (result.staged_files.length) {
    out += `\nStaged (${result.staged_files.length}):\n`;
    for (const f of result.staged_files) out += `  ✅ ${f}\n`;
  }
  if (result.modified_files.length) {
    out += `\nModified (${result.modified_files.length}):\n`;
    for (const f of result.modified_files) out += `  ✏️ ${f}\n`;
  }
  if (result.untracked_files.length) {
    out += `\nUntracked (${result.untracked_files.length}):\n`;
    for (const f of result.untracked_files) out += `  ❓ ${f}\n`;
  }
  if (!result.has_changes) out += "\nWorking tree clean, no changes.";
  return out;
}

function formatGitDiff(result: GitDiffResult): string {
  return result.diff
    ? truncate(`📝 Git Diff\n\n${result.diff}`)
    : "No changes.";
}

function formatGitCommit(result: GitCommitResult): string {
  return `✅ Committed\n\nHash: ${result.hash}\nMessage: ${result.message}`;
}

function formatHistory(entries: HistoryEntry[]): string {
  if (!entries?.length) return "No conversation history.";
  let out = `📜 Recent Conversation (${entries.length})\n\n`;
  for (const e of entries) {
    const role = e.role === "user" ? "👤" : "🤖";
    const content =
      e.content.length > TOOL_CONTENT_PREVIEW_CHARS
        ? e.content.slice(0, TOOL_CONTENT_PREVIEW_CHARS) + "…"
        : e.content;
    out += `${role} ${content}\n\n`;
    if (out.length > MAX_OUTPUT) {
      out += "…";
      break;
    }
  }
  return out;
}

function formatSearchResults(results: SearchResult[], query: string): string {
  if (!results?.length) return `No matches found for "${query}"`;
  let out = `🔍 Search Results: "${query}" (${results.length})\n\n`;
  for (const r of results) {
    out += `${r.snippet || r.context || ""}\n\n`;
    if (out.length > MAX_OUTPUT) {
      out += "…";
      break;
    }
  }
  return out;
}

function formatExport(result: ExportResult): string {
  return `📤 Exported\n\nPath: ${result.path}${result.size ? `\nSize: ${(result.size / 1024).toFixed(1)} KB` : ""}`;
}

function formatProjects(projects: ProjectInfo[]): string {
  if (!projects?.length) return "📋 Projects (0)";
  let out = `📋 Projects (${projects.length})\n\n`;
  for (const p of projects) {
    out += `• ${p.name} [${p.id}]\n  ${p.path}\n\n`;
  }
  return truncate(out);
}

function formatTasks(tasks: TaskInfo[]): string {
  if (!tasks?.length) return "📋 Background Tasks (0)";
  let out = `📋 Background Tasks (${tasks.length})\n\n`;
  for (const t of tasks) {
    const emoji =
      t.status === "running"
        ? "🟢"
        : t.status === "completed"
          ? "✅"
          : t.status === "failed"
            ? "🔴"
            : "⚪";
    out += `${emoji} [${t.id}] ${t.description}\n  Status: ${t.status}\n\n`;
  }
  return truncate(out);
}

function formatCronList(crons: CronEntry[]): string {
  if (!crons?.length) return "📋 Cron Jobs (0)";
  let out = `📋 Cron Jobs (${crons.length})\n\n`;
  for (const c of crons) {
    const s = c.enabled ? "🟢" : "🔴";
    out += `${s} [${c.id}] ${c.schedule} ${c.command}\n`;
  }
  return truncate(out);
}

function formatSpecList(specs: SpecInfo[]): string {
  if (!specs?.length) return "📋 Specs (0)";
  let out = `📋 Specs (${specs.length})\n\n`;
  for (const s of specs) {
    out += `• ${s.name} [${s.phase}] (${s.completed_tasks}/${s.total_tasks})\n`;
  }
  return out;
}

function formatSpecShow(spec: {
  name: string;
  content: string;
  phase: string;
}): string {
  return truncate(
    `📋 Spec: ${spec.name}\nPhase: ${spec.phase}\n\n${spec.content}`,
  );
}

function formatSpecStatus(spec: {
  name: string;
  phase: string;
  tasks: { name: string; status: string }[];
}): string {
  let out = `📊 Spec Status: ${spec.name}\nPhase: ${spec.phase}\n\n`;
  for (const t of spec.tasks) {
    const e =
      t.status === "completed"
        ? "✅"
        : t.status === "in_progress"
          ? "🔄"
          : "⬜";
    out += `${e} ${t.name}\n`;
  }
  return truncate(out);
}

function formatError(title: string, detail: string): string {
  return `❌ ${title}\n${detail}`;
}

// ── Daemon info (set by gateway) ──

let _daemonInfo: { pid: number; session_id: string; cwd: string } | null = null;
let _daemonMetrics = { reconnectCount: 0, lastConnectAt: null as Date | null };

export function setDaemonInfo(info: typeof _daemonInfo): void {
  _daemonInfo = info;
}

export function setDaemonMetrics(metrics: typeof _daemonMetrics): void {
  _daemonMetrics = metrics;
}

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

// ── Command Handlers ──

async function handleCompact(ctx: CommandContext): Promise<string> {
  const result = await ctx.ipcClient.request<CompactResult>("compact");
  return formatCompact(result);
}

async function handleModel(ctx: CommandContext): Promise<string> {
  if (!ctx.args.trim())
    return "Ask the AI directly for current model info.\nUsage: /model <model-name>";
  const result = await ctx.ipcClient.request<{ model: string }>("switchModel", {
    model: ctx.args.trim(),
  });
  return `✅ Switched to model: ${result.model ?? ctx.args.trim()}`;
}

async function handleHistory(ctx: CommandContext): Promise<string> {
  const n = parseInt(ctx.args.trim(), 10) || 10;
  const result = await ctx.ipcClient.request<{ entries: HistoryEntry[] }>(
    "talkTail",
    { n },
  );
  return formatHistory(result.entries ?? (result as unknown as HistoryEntry[]));
}

async function handleSearch(ctx: CommandContext): Promise<string> {
  if (!ctx.args.trim())
    return formatError("Missing argument", "Usage: /search <query>");
  const result = await ctx.ipcClient.request<SearchResult[]>("searchHistory", {
    query: ctx.args.trim(),
  });
  return formatSearchResults(result, ctx.args.trim());
}

async function handleExport(ctx: CommandContext): Promise<string> {
  const result = await ctx.ipcClient.request<ExportResult>("export");
  return formatExport(result);
}

async function handleAbort(ctx: CommandContext): Promise<string> {
  await ctx.control.request("abort");
  return "⛔ Current task aborted.";
}

async function handleGit(ctx: CommandContext): Promise<string> {
  const result = await ctx.ipcClient.request<GitStatusResult>("gitStatus");
  return formatGitStatus(result);
}

async function handleDiff(ctx: CommandContext): Promise<string> {
  const result = await ctx.ipcClient.request<GitDiffResult>("gitDiff");
  return formatGitDiff(result);
}

async function handleCommit(ctx: CommandContext): Promise<string> {
  if (!ctx.args.trim())
    return formatError("Missing argument", "Usage: /commit <message>");
  const result = await ctx.ipcClient.request<GitCommitResult>("gitCommit", {
    message: ctx.args.trim(),
  });
  return formatGitCommit(result);
}

async function handleTools(ctx: CommandContext): Promise<string> {
  const result = await ctx.ipcClient.request<
    { tools: ToolInfo[] } | ToolInfo[]
  >("listTools");
  const tools = Array.isArray(result) ? result : ((result as any).tools ?? []);
  return formatTools(tools);
}

async function handleHealth(ctx: CommandContext): Promise<string> {
  const data = await ctx.ipcClient.request<ToolHealthData>("toolHealth", {});
  return formatToolHealth(data, { verbose: ctx.args.trim() === "all" });
}

async function handleMcp(ctx: CommandContext): Promise<string> {
  const result = await ctx.ipcClient.request<
    { servers: McpServerInfo[] } | McpServerInfo[]
  >("listMcpServers");
  const servers = Array.isArray(result)
    ? result
    : ((result as any).servers ?? []);
  return formatMcpServers(servers);
}

async function handleSkills(ctx: CommandContext): Promise<string> {
  const result = await ctx.ipcClient.request<
    { skills: SkillInfo[] } | SkillInfo[]
  >("listSkills");
  const skills = Array.isArray(result)
    ? result
    : ((result as any).skills ?? []);
  return formatSkills(skills);
}

async function handlePlugins(ctx: CommandContext): Promise<string> {
  const result = await ctx.ipcClient.request<
    { plugins: PluginInfo[] } | PluginInfo[]
  >("listPlugins");
  const plugins = Array.isArray(result)
    ? result
    : ((result as any).plugins ?? []);
  return formatPlugins(plugins);
}

async function handleProjects(ctx: CommandContext): Promise<string> {
  const result = await ctx.ipcClient.request<
    { projects: ProjectInfo[] } | ProjectInfo[]
  >("projectsList");
  const projects = Array.isArray(result)
    ? result
    : ((result as any).projects ?? []);
  return formatProjects(projects);
}

async function handleTask(ctx: CommandContext): Promise<string> {
  if (!ctx.args.trim())
    return formatError("Missing argument", "Usage: /task <description>");
  const result = await ctx.ipcClient.request<{ id: string; status: string }>(
    "taskCreate",
    { description: ctx.args.trim() },
  );
  return `🚀 Task created\n\nID: ${result.id}\nStatus: ${result.status}`;
}

async function handleTasks(ctx: CommandContext): Promise<string> {
  const result = await ctx.ipcClient.request<
    { tasks: TaskInfo[] } | TaskInfo[]
  >("taskList");
  const tasks = Array.isArray(result) ? result : ((result as any).tasks ?? []);
  return formatTasks(tasks);
}

async function handleTaskStop(ctx: CommandContext): Promise<string> {
  if (!ctx.args.trim())
    return formatError("Missing argument", "Usage: /task_stop <task-id>");
  await ctx.ipcClient.request("taskStop", { id: ctx.args.trim() });
  return `⏹️ Task stopped\n\nID: ${ctx.args.trim()}`;
}

async function handleCron(ctx: CommandContext): Promise<string> {
  const result = await ctx.ipcClient.request<
    { crons: CronEntry[] } | CronEntry[]
  >("cronList");
  const crons = Array.isArray(result) ? result : ((result as any).crons ?? []);
  return formatCronList(crons);
}

async function handleHelp(_ctx: CommandContext): Promise<string> {
  return formatHelp();
}

async function handleStatus(ctx: CommandContext): Promise<string> {
  const connected = ctx.ipcClient.connected
    ? "🟢 Connected"
    : "🔴 Disconnected";
  let out = `🐾 BaoClaw Feishu Gateway\n\nDaemon: ${connected}\n`;
  if (_daemonInfo) {
    out += `Daemon PID: ${_daemonInfo.pid}\nSession: ${_daemonInfo.session_id}\nCWD: ${_daemonInfo.cwd}\n`;
  }
  out += `Reconnects: ${_daemonMetrics.reconnectCount}\n`;
  out += `Last connect: ${_daemonMetrics.lastConnectAt?.toISOString() ?? "never"}\n`;
  return out;
}

async function handleStart(_ctx: CommandContext): Promise<string> {
  return "🐾 BaoClaw Feishu Gateway\n\nWelcome to BaoClaw!\n\nSend a message to chat with the AI, or use / commands.\nType /help to see all available commands.";
}

async function handleGateway(_ctx: CommandContext): Promise<string> {
  const args = _ctx.args.trim();
  const parts = args.split(/\s+/);
  const sub = parts[0] || "status";

  switch (sub) {
    case "status": {
      if (!_gatewayInfo) return "⚠️ Gateway info not initialized";
      const uptime = Math.floor((Date.now() - _gatewayInfo.startTime) / 1000);
      const mem = process.memoryUsage();
      let out = `🐾 ${_gatewayInfo.name} Gateway\n\n`;
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
        return `📄 Last ${recent.length} log lines\n\n${recent.join("\n").slice(0, LOG_TAIL_CHARS)}`;
      } catch (e: any) {
        return `⚠️ Cannot read log: ${e.message}`;
      }
    }
    default:
      return "📋 Gateway Commands\n\n• /gateway status — run status\n• /gateway ping — connectivity check\n• /gateway logs [n] — last n log lines";
  }
}

async function handleThink(_ctx: CommandContext): Promise<string> {
  return "🧠 Extended Thinking\n\nJust send a message describing what needs deep thought.";
}

async function handleSpec(ctx: CommandContext): Promise<string> {
  const parts = ctx.args.trim().split(/\s+/);
  const sub = parts[0] || "";
  const rest = parts.slice(1).join(" ");

  switch (sub) {
    case "list": {
      const result = await ctx.ipcClient.request<
        { specs: SpecInfo[] } | SpecInfo[]
      >("specList");
      const specs = Array.isArray(result)
        ? result
        : ((result as any).specs ?? []);
      return formatSpecList(specs);
    }
    case "new": {
      if (!rest)
        return formatError("Missing argument", "Usage: /spec new <name>");
      const result = await ctx.ipcClient.request<{
        name: string;
        phase: string;
      }>("specNew", { name: rest });
      return `✅ Spec created\n\nName: ${result.name}\nPhase: ${result.phase}`;
    }
    case "show": {
      if (!rest)
        return formatError("Missing argument", "Usage: /spec show <name>");
      const result = await ctx.ipcClient.request<{
        name: string;
        content: string;
        phase: string;
      }>("specShow", { name: rest });
      return formatSpecShow(result);
    }
    case "status": {
      if (!rest)
        return formatError("Missing argument", "Usage: /spec status <name>");
      const result = await ctx.ipcClient.request<{
        name: string;
        phase: string;
        tasks: { name: string; status: string }[];
      }>("specStatus", { name: rest });
      return formatSpecStatus(result);
    }
    case "run": {
      const name = parts[1];
      const taskId = parts[2];
      if (!name)
        return formatError(
          "Missing argument",
          "Usage: /spec run <name> [task_id]",
        );
      const params: Record<string, string> = { name };
      if (taskId) params.task_id = taskId;
      const result = await ctx.ipcClient.request<{
        task_id?: string;
        status: string;
        message?: string;
      }>("specRun", params);
      return result.message
        ? `🚀 Spec execution\n\n${result.message}`
        : `🚀 Spec started\n\nTask ID: ${result.task_id || "N/A"}\nStatus: ${result.status}`;
    }
    default:
      return "📋 Spec Commands\n\n• /spec list — list all\n• /spec new <name> — create\n• /spec show <name> — details\n• /spec status <name> — status\n• /spec run <name> — run";
  }
}

// ── Command Registry ──

interface CommandDef {
  name: string;
  description: string;
  usage?: string;
  handler: (ctx: CommandContext) => Promise<string>;
}

export const COMMAND_REGISTRY: Record<string, CommandDef> = {
  "/compact": {
    name: "/compact",
    description: "Compact conversation context",
    handler: handleCompact,
  },
  "/think": {
    name: "/think",
    description: "Extended thinking mode prompt",
    handler: handleThink,
  },
  "/model": {
    name: "/model",
    description: "View or switch model",
    usage: "/model [name]",
    handler: handleModel,
  },
  "/history": {
    name: "/history",
    description: "View recent conversation",
    usage: "/history [n]",
    handler: handleHistory,
  },
  "/search": {
    name: "/search",
    description: "Search conversation history",
    usage: "/search <query>",
    handler: handleSearch,
  },
  "/export": {
    name: "/export",
    description: "Export conversation history",
    handler: handleExport,
  },
  "/abort": {
    name: "/abort",
    description: "Abort current task",
    handler: handleAbort,
  },
  "/git": { name: "/git", description: "Show git status", handler: handleGit },
  "/diff": { name: "/diff", description: "Show git diff", handler: handleDiff },
  "/commit": {
    name: "/commit",
    description: "Commit git changes",
    usage: "/commit <message>",
    handler: handleCommit,
  },
  "/tools": {
    name: "/tools",
    description: "List registered tools",
    handler: handleTools,
  },
  "/health": {
    name: "/health",
    description: "Tool health overview",
    usage: "/health [all]",
    handler: handleHealth,
  },
  "/mcp": { name: "/mcp", description: "List MCP servers", handler: handleMcp },
  "/skills": {
    name: "/skills",
    description: "List loaded skills",
    handler: handleSkills,
  },
  "/plugins": {
    name: "/plugins",
    description: "List installed plugins",
    handler: handlePlugins,
  },
  "/projects": {
    name: "/projects",
    description: "List projects",
    handler: handleProjects,
  },
  "/task": {
    name: "/task",
    description: "Create background task",
    usage: "/task <description>",
    handler: handleTask,
  },
  "/tasks": {
    name: "/tasks",
    description: "List background tasks",
    handler: handleTasks,
  },
  "/task_stop": {
    name: "/task_stop",
    description: "Stop background task",
    usage: "/task_stop <id>",
    handler: handleTaskStop,
  },
  "/cron": {
    name: "/cron",
    description: "List cron jobs",
    handler: handleCron,
  },
  "/help": { name: "/help", description: "Show help", handler: handleHelp },
  "/status": {
    name: "/status",
    description: "Show gateway status",
    handler: handleStatus,
  },
  "/start": {
    name: "/start",
    description: "Show welcome message",
    handler: handleStart,
  },
  "/gateway": {
    name: "/gateway",
    description: "Gateway management (informational)",
    usage: "/gateway status|ping|logs",
    handler: handleGateway,
  },
  "/spec": {
    name: "/spec",
    description: "Spec management",
    usage: "/spec list|new|show|status|run",
    handler: handleSpec,
  },
};

// ── Command Parsing & Dispatch ──

export function parseCommand(text: string): ParsedCommand | null {
  if (!text.startsWith("/")) return null;
  const trimmed = text.trim();
  const spaceIdx = trimmed.indexOf(" ");
  if (spaceIdx === -1) return { name: trimmed.toLowerCase(), args: "" };
  return {
    name: trimmed.slice(0, spaceIdx).toLowerCase(),
    args: trimmed.slice(spaceIdx + 1).trim(),
  };
}

export function isRegisteredCommand(name: string): boolean {
  return name in COMMAND_REGISTRY;
}

export async function dispatchCommand(
  cmd: ParsedCommand,
  ctx: CommandContext,
): Promise<string | null> {
  const command = COMMAND_REGISTRY[cmd.name];
  if (!command) return null;

  const fullCtx: CommandContext = { ...ctx, args: cmd.args };

  try {
    const result = await command.handler(fullCtx);
    return result;
  } catch (err: any) {
    const message = err instanceof Error ? err.message : String(err);
    logger.error(`Command ${cmd.name} failed: ${message}`);
    return formatError("Command failed", message);
  }
}

// ── Help Text ──

export function formatHelp(): string {
  const groups: [string, string[]][] = [
    [
      "💬 Chat",
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
    ["📂 Projects & Git", ["/projects", "/git", "/diff", "/commit"]],
    [
      "🔧 Tools & Extensions",
      ["/tools", "/health", "/mcp", "/skills", "/plugins"],
    ],
    ["⚙️ Automation", ["/task", "/tasks", "/task_stop", "/cron"]],
    ["📋 Spec", ["/spec"]],
    ["🚪 Gateway", ["/gateway"]],
    ["🔌 Session", ["/help", "/status", "/start"]],
  ];

  let out = "📖 BaoClaw Commands\n\n";
  for (const [group, cmds] of groups) {
    out += `${group}\n`;
    for (const c of cmds) {
      const def = COMMAND_REGISTRY[c];
      if (def)
        out += `  ${c} — ${def.description}${def.usage ? ` ${def.usage}` : ""}\n`;
    }
    out += "\n";
  }
  out += "Send any non-command message to chat with the AI";
  return out;
}
