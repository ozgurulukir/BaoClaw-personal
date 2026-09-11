/**
 * Unified Gateway Command Bridge.
 *
 * Implements slash command dispatch, RPC execution via IpcClient, and
 * presentation formatting parameterized by ChannelFormatter.
 */

import type { ChannelFormatter } from "./formatters/index.js";
import { plainFormatter } from "./formatters/plain.js";
import type {
  CommandDefinition,
  CompactResult,
  CronJob,
  DaemonInfo,
  DaemonMetrics,
  ExportResult,
  GitCommitResult,
  GitDiffResult,
  GitStatusResult,
  HistoryEntry,
  PluginInfo,
  ProjectInfo,
  SkillInfo,
  SpecDetail,
  SpecInfo,
  SpecProgress,
  TaskInfo,
  ToolInfo,
} from "./types.js";
import {
  formatMcpServers,
  formatSearchResults as formatSearchResultsShared,
  formatToolHealth,
  type McpServerList,
  type SearchResult,
  type ToolHealthData,
} from "../index.js";

export interface IpcTarget {
  connected?: boolean;
  request<T = any>(method: string, params?: unknown): Promise<T>;
}

export const CANONICAL_COMMAND_REGISTRY: Record<string, CommandDefinition> = {
  "/tools": { description: "List registered tools" },
  "/health": {
    description: "Tool health status",
    usage: "/health [all]",
  },
  "/skills": { description: "List loaded skills" },
  "/mcp": {
    description: "MCP servers (live state)",
    usage: "/mcp [refresh [server]]",
  },
  "/plugins": { description: "List installed plugins" },
  "/compact": { description: "Compact conversation context" },
  "/think": { description: "Toggle extended thinking mode tips" },
  "/model": {
    description: "View or switch model",
    usage: "/model [name]",
  },
  "/diff": { description: "Show git diff" },
  "/commit": {
    description: "Commit git changes",
    usage: "/commit <message>",
  },
  "/git": { description: "Show git status" },
  "/abort": { description: "Abort the current task" },
  "/help": { description: "Show available commands" },
  "/status": { description: "Show gateway and daemon status" },
  "/start": { description: "Show the welcome message" },
  "/clear": { description: "Clear the current session conversation" },
  "/shutdown": { description: "Shut down the daemon" },
  "/quit": {
    description: "Disconnect gateway; daemon stays running",
    usage: "Disconnect gateway",
  },
  "/memory": {
    description: "Manage long-term memory",
    usage: "/memory list|add <text>|delete <id>|clear|stats|archive",
  },
  "/cron": {
    description: "Scheduled cron jobs",
    usage: "/cron list|add|remove|toggle",
  },
  "/projects": {
    description: "Manage projects",
    usage: "/projects list|<id>|new <path> [desc]",
  },
  "/task": {
    description: "Create or stop a background task",
    usage: "/task <description> | /task stop <id>",
  },
  "/tasks": { description: "List background tasks" },
  "/task_stop": {
    description: "Stop a background task",
    usage: "/task_stop <id>",
  },
  "/history": {
    description: "Recent conversation",
    usage: "/history [n]",
  },
  "/export": {
    description: "Export conversation history",
    usage: "/export [path]",
  },
  "/search": {
    description: "Search conversation history",
    usage: "/search <query>",
  },
  "/spec": {
    description: "Spec-driven development workflow",
    usage: "/spec list|new|show|status|run",
  },
  "/rate": {
    description: "Rate the last assistant response",
    usage: "/rate good|bad|neutral",
  },
};

/**
 * Parse a message text into command name and arguments.
 * Returns null if the text is not a slash command.
 */
export function parseCommand(
  text: string,
): { command: string; args: string } | null {
  if (!text || !text.startsWith("/")) return null;
  const trimmed = text.trim();
  const spaceIdx = trimmed.indexOf(" ");
  if (spaceIdx === -1) {
    return { command: trimmed.toLowerCase(), args: "" };
  }
  return {
    command: trimmed.slice(0, spaceIdx).toLowerCase(),
    args: trimmed.slice(spaceIdx + 1).trim(),
  };
}

/**
 * Check whether a message text starts with a registered slash command.
 */
export function isRegisteredCommand(
  commandOrText: string,
  registry: Record<string, CommandDefinition> = CANONICAL_COMMAND_REGISTRY,
): boolean {
  if (commandOrText in registry) return true;
  const parsed = parseCommand(commandOrText);
  if (!parsed) return false;
  return parsed.command in registry;
}

/**
 * Core command bridge implementing business logic for all slash commands.
 */
export class GatewayCommandBridge {
  constructor(
    protected client: IpcTarget,
    public formatter: ChannelFormatter = plainFormatter,
  ) {}

  /**
   * Dispatch a slash command to its corresponding handler.
   */
  async dispatch(
    name: string,
    args: string = "",
    control?: IpcTarget,
  ): Promise<string | null> {
    const cmd = name.toLowerCase();
    switch (cmd) {
      case "/compact":
        return this.handleCompact();
      case "/think":
        return this.handleThink();
      case "/model":
        return this.handleModel(args);
      case "/history":
        return this.handleHistory(args);
      case "/search":
        return this.handleSearch(args);
      case "/export":
        return this.handleExport();
      case "/abort":
        return this.handleAbort(control);
      case "/git":
        return this.handleGit();
      case "/diff":
        return this.handleDiff();
      case "/commit":
        return this.handleCommit(args);
      case "/tools":
        return this.handleTools();
      case "/health":
        return this.handleHealth(args);
      case "/mcp":
        return this.handleMcp();
      case "/skills":
        return this.handleSkills();
      case "/plugins":
        return this.handlePlugins();
      case "/projects":
        return this.handleProjects(args);
      case "/task":
        return this.handleTask(args);
      case "/tasks":
        return this.handleTasks();
      case "/task_stop":
        return this.handleTaskStop(args);
      case "/cron":
        return this.handleCron(args);
      case "/spec":
        return this.handleSpec(args);
      case "/memory":
        return this.handleMemory(args);
      case "/rate":
        return this.handleRate(args);
      case "/shutdown":
        return this.handleShutdown();
      case "/clear":
        return this.handleClear();
      case "/help":
        return this.formatHelp();
      case "/status":
        return this.formatStatus();
      case "/start":
        return "🐾 Welcome to BaoClaw!\n\nSend a message to chat with the AI, or use slash commands.\nType /help to see all available commands.";
      default:
        return null;
    }
  }

  // ── Format Helpers ──────────────────────────────────────────────────────────

  formatTools(tools: ToolInfo[], count?: number): string {
    const total = count ?? tools.length;
    if (total === 0) return "No tools registered.";

    const groups: Record<string, ToolInfo[]> = {};
    for (const t of tools) {
      const type = t.type || "other";
      if (!groups[type]) groups[type] = [];
      groups[type].push(t);
    }

    let out = `🔧 ${this.formatter.bold("Registered Tools")} (${total})\n\n`;
    for (const [type, items] of Object.entries(groups)) {
      out += `── ${type} (${items.length}) ──\n`;
      for (const t of items) {
        const desc = t.description
          ? t.description.length > 60
            ? t.description.slice(0, 60) + "…"
            : t.description
          : "";
        out += `• ${this.formatter.code(t.name)}  ${desc}\n`;
      }
      out += "\n";
    }
    return this.formatter.truncate(out.trimEnd());
  }

  formatSkills(skills: SkillInfo[], count?: number): string {
    const total = count ?? skills.length;
    if (total === 0) return "No skills loaded.";
    let out = `📚 ${this.formatter.bold("Loaded Skills")} (${total})\n\n`;
    for (const s of skills) {
      out += `• ${this.formatter.code(s.name)} [${s.source}]\n`;
      if (s.description) {
        out += `  ${s.description}\n`;
      }
    }
    return this.formatter.truncate(out.trimEnd());
  }

  formatPlugins(plugins: PluginInfo[], count?: number): string {
    const total = count ?? plugins.length;
    if (total === 0) return "No plugins installed.";
    let out = `🧩 ${this.formatter.bold("Installed Plugins")} (${total})\n\n`;
    for (const p of plugins) {
      const ver = p.version ? ` v${p.version}` : "";
      const features: string[] = [];
      if (p.has_tools) features.push("tools");
      if (p.has_skills) features.push("skills");
      if (p.has_mcp) features.push("mcp");
      const featureStr = features.length > 0 ? ` (${features.join(", ")})` : "";
      out += `• ${this.formatter.code(p.name + ver)} [${p.source}]${featureStr}\n`;
      if (p.description) {
        out += `  ${p.description}\n`;
      }
    }
    return this.formatter.truncate(out.trimEnd());
  }

  formatCompact(result: CompactResult): string {
    const pct =
      result.tokens_before > 0
        ? ((result.tokens_saved / result.tokens_before) * 100).toFixed(0)
        : "0";
    return (
      `🗜️ ${this.formatter.bold("Context Compacted")}\n\n` +
      `Before  ${result.tokens_before.toLocaleString()} tokens\n` +
      `After   ${result.tokens_after.toLocaleString()} tokens\n` +
      `Saved   ${result.tokens_saved.toLocaleString()} tokens (${pct}%)\n` +
      `Summary ${result.summary_tokens.toLocaleString()} tokens`
    );
  }

  formatGitStatus(result: GitStatusResult): string {
    const branch = result.branch ?? "(detached)";
    let out = `📂 ${this.formatter.bold("Git Status")}\n\nBranch: ${this.formatter.code(branch)}\n`;
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
      out += "\nWorking tree clean — no changes.";
    }
    return out.trimEnd();
  }

  formatGitDiff(result: GitDiffResult): string {
    if (!result.diff || result.diff.trim() === "") return "No changes.";
    return this.formatter.truncate(
      `📝 ${this.formatter.bold("Git Diff")}\n\n${this.formatter.codeBlock(result.diff, "diff")}`,
    );
  }

  formatGitCommit(result: GitCommitResult): string {
    return (
      `✅ ${this.formatter.bold("Committed")}\n\n` +
      `Hash: ${this.formatter.code(result.hash)}\n` +
      `Message: ${result.message}`
    );
  }

  formatHistory(entries: HistoryEntry[]): string {
    if (!entries || entries.length === 0) return "No conversation history.";
    let out = `📜 ${this.formatter.bold("Recent Conversation")} (${entries.length})\n\n`;
    for (const e of entries) {
      const role = e.role === "user" ? "👤" : "🤖";
      const text = e.text.length > 100 ? e.text.slice(0, 100) + "…" : e.text;
      out += `${role} ${text}\n\n`;
    }
    return this.formatter.truncate(out.trimEnd());
  }

  formatTasks(tasks: TaskInfo[]): string {
    if (tasks.length === 0) return "No background tasks.";
    let out = `📋 ${this.formatter.bold("Background Tasks")} (${tasks.length})\n\n`;
    for (const t of tasks) {
      let statusStr = "unknown";
      if (typeof t.status === "string") {
        statusStr = t.status;
      } else if (t.status && typeof t.status === "object") {
        statusStr = (t.status as any).Failed
          ? `Failed: ${(t.status as any).Failed}`
          : (Object.keys(t.status)[0] ?? "unknown");
      }
      out += `• ${this.formatter.code(t.id)} [${statusStr}]: ${t.description}\n`;
    }
    return this.formatter.truncate(out.trimEnd());
  }

  formatCronList(crons: CronJob[]): string {
    if (crons.length === 0) return "No cron jobs configured.";
    let out = `⏰ ${this.formatter.bold("Scheduled Cron Jobs")} (${crons.length})\n\n`;
    for (const c of crons) {
      const state = c.enabled ? "🟢 active" : "⏸️ paused";
      out += `• ${this.formatter.bold(c.name)} (${this.formatter.code(c.id)})\n`;
      out += `  Schedule: ${this.formatter.code(c.schedule)} [${state}]\n`;
      out += `  Prompt: ${c.prompt}\n`;
      if (c.last_run) out += `  Last run: ${c.last_run}\n`;
      out += "\n";
    }
    return this.formatter.truncate(out.trimEnd());
  }

  formatProjects(projects: ProjectInfo[], current?: string): string {
    if (projects.length === 0) return "No projects registered.";
    let out = `📁 ${this.formatter.bold("Projects")} (${projects.length})\n\n`;
    for (const p of projects) {
      const isCurrent =
        current &&
        (p.id === current || p.cwd === current || p.name === current);
      const marker = isCurrent ? " ⭐ (active)" : "";
      out += `• ${this.formatter.bold(p.name ?? p.id)}${marker}\n`;
      out += `  ID: ${this.formatter.code(p.id)}\n`;
      if (p.cwd || p.path)
        out += `  Path: ${this.formatter.code(p.cwd ?? p.path ?? "")}\n`;
      if (p.description) out += `  Desc: ${p.description}\n`;
      out += "\n";
    }
    return this.formatter.truncate(out.trimEnd());
  }

  formatSpecList(specs: SpecInfo[]): string {
    if (specs.length === 0) return "No specs found.";
    let out = `📋 ${this.formatter.bold("Specs")} (${specs.length})\n\n`;
    for (const s of specs) {
      out += `• ${this.formatter.bold(s.name)}\n`;
      if (s.workflow) out += `  Workflow: ${s.workflow}\n`;
      if (s.type) out += `  Type: ${s.type}\n`;
      if (s.task_progress) {
        out += `  Progress: ${s.task_progress.completed}/${s.task_progress.total} done (${s.task_progress.in_progress} in progress)\n`;
      }
      out += "\n";
    }
    return this.formatter.truncate(out.trimEnd());
  }

  formatSpecShow(spec: SpecDetail): string {
    let out = `📋 ${this.formatter.bold(`Spec: ${spec.name}`)}\n\n`;
    if (spec.workflow) out += `Workflow: ${spec.workflow}\n`;
    if (spec.current_phase) out += `Phase: ${spec.current_phase}\n`;
    if (spec.task_progress) {
      out += `Tasks: ${spec.task_progress.completed}/${spec.task_progress.total} completed\n`;
    }
    if (spec.content) {
      out += `\n${this.formatter.codeBlock(spec.content, "markdown")}\n`;
    }
    return this.formatter.truncate(out.trimEnd());
  }

  formatSpecStatus(name: string, progress: SpecProgress): string {
    return (
      `📊 ${this.formatter.bold(`Spec Status: ${name}`)}\n\n` +
      `Total: ${progress.total}\n` +
      `✅ Completed: ${progress.completed}\n` +
      `🔄 In progress: ${progress.in_progress}`
    );
  }

  formatSpecRun(result: {
    task_id?: string;
    task_description?: string;
    status: string;
    message?: string;
  }): string {
    if (result.message) {
      return `🚀 ${this.formatter.bold("Spec Execution")}\n\n${result.message}`;
    }
    const description = result.task_description
      ? `\n\n${result.task_description}`
      : "";
    return `🚀 ${this.formatter.bold("Next Task")}\n\nTask ID: ${this.formatter.code(result.task_id || "N/A")}${description}`;
  }

  formatError(title: string, detail?: string): string {
    return `❌ ${this.formatter.bold(title)}${detail ? `\n${detail}` : ""}`;
  }

  formatHelp(
    registry: Record<string, CommandDefinition> = CANONICAL_COMMAND_REGISTRY,
  ): string {
    const groups: Record<string, string[]> = {
      "💬 Conversation": [
        "/compact",
        "/think",
        "/model",
        "/history",
        "/search",
        "/export",
        "/abort",
      ],
      "📂 Projects & Git": ["/projects", "/git", "/diff", "/commit"],
      "🔧 Tools & Extensions": [
        "/tools",
        "/health",
        "/mcp",
        "/skills",
        "/plugins",
      ],
      "⚙️ Automation": ["/task", "/tasks", "/cron", "/memory", "/spec"],
      "🔌 Session": [
        "/help",
        "/status",
        "/start",
        "/clear",
        "/quit",
        "/shutdown",
        "/rate",
      ],
    };

    let out = `📖 ${this.formatter.bold("Available Commands")}\n\n`;
    for (const [group, cmds] of Object.entries(groups)) {
      out += `${this.formatter.bold(group)}\n`;
      for (const cmd of cmds) {
        const def = registry[cmd];
        if (def) {
          out += `  ${this.formatter.code(cmd)} — ${def.usage ?? def.description}\n`;
        }
      }
      out += "\n";
    }

    const grouped = new Set(Object.values(groups).flat());
    const ungrouped = Object.entries(registry).filter(
      ([cmd]) => !grouped.has(cmd),
    );
    if (ungrouped.length > 0) {
      for (const [cmd, def] of ungrouped) {
        out += `  ${this.formatter.code(cmd)} — ${def.usage ?? def.description}\n`;
      }
    }
    return out.trimEnd();
  }

  formatStatus(
    daemonInfo?: DaemonInfo | null,
    metrics?: DaemonMetrics,
    extra?: Record<string, string>,
  ): string {
    const isConn = this.client.connected ?? true;
    const connected = isConn ? "🟢 Connected" : "🔴 Disconnected";
    let out = `🐾 ${this.formatter.bold("BaoClaw Gateway Status")}\n\n`;
    out += `Daemon connection: ${connected}\n`;
    if (daemonInfo) {
      out += `Daemon PID: ${daemonInfo.pid}\n`;
      out += `Session: ${daemonInfo.session_id}\n`;
      out += `CWD: ${daemonInfo.cwd}\n`;
    }
    if (metrics) {
      out += `Reconnects: ${metrics.reconnectCount}\n`;
      out += `Last connect: ${metrics.lastConnectAt?.toISOString() ?? "never"}\n`;
    }
    if (extra) {
      for (const [k, v] of Object.entries(extra)) {
        out += `${k}: ${v}\n`;
      }
    }
    return out.trimEnd();
  }

  // ── Command Handlers ────────────────────────────────────────────────────────

  async handleCompact(): Promise<string> {
    const result = await this.client.request<CompactResult>("compact");
    return this.formatCompact(result);
  }

  async handleThink(): Promise<string> {
    return (
      `🧠 ${this.formatter.bold("Extended Thinking")}\n\n` +
      "Just send a message describing what needs deep thought.\n" +
      "The AI will analyze and reason in more detail.\n\n" +
      "For example:\n" +
      "• Ask a complex question directly\n" +
      "• Ask for an analysis of a piece of code\n" +
      "• Ask for a math problem to be solved step by step"
    );
  }

  async handleModel(args = ""): Promise<string> {
    const trimmed = args.trim();
    if (!trimmed) {
      return (
        `🤖 ${this.formatter.bold("Model Info")}\n\n` +
        "Ask the AI directly for current model info.\n\n" +
        `Usage: ${this.formatter.code("/model <model-name>")} to switch model`
      );
    }
    const result = await this.client.request<{ model: string }>("switchModel", {
      model: trimmed,
    });
    return `✅ ${this.formatter.bold("Switched to model:")} ${result.model ?? trimmed}`;
  }

  async handleHistory(args = ""): Promise<string> {
    const count = parseInt(args.trim(), 10) || 10;
    const result = await this.client.request<{ messages: HistoryEntry[] }>(
      "talkTail",
      { count },
    );
    const messages = result.messages ?? [];
    return this.formatHistory(messages);
  }

  async handleClear(): Promise<string> {
    const res = await this.client.request<{
      cleared: boolean;
      messages_removed: number;
    }>("clearSession");
    return `🗑️ ${this.formatter.bold("Session Cleared")}\n\nRemoved ${res.messages_removed ?? 0} messages.`;
  }

  async handleExport(outputPath?: string): Promise<string> {
    const params = outputPath ? { output_path: outputPath } : {};
    const result = await this.client.request<ExportResult>("export", params);
    const sizeKb = (result.size_bytes / 1024).toFixed(1);
    return (
      `📄 ${this.formatter.bold("Conversation Exported")}\n\n` +
      `File: ${this.formatter.code(result.file_path)}\n` +
      `Messages: ${result.message_count}\n` +
      `Size: ${sizeKb} KB`
    );
  }

  async handleSearch(query: string): Promise<string> {
    const q = query.trim();
    if (!q) {
      return this.formatError(
        "Missing query",
        `Usage: ${this.formatter.code("/search <query>")}`,
      );
    }
    const result = await this.client.request<{
      results: SearchResult[];
      count: number;
    }>("searchHistory", { query: q });
    return formatSearchResultsShared(result.results ?? [], q, {
      maxChars: 3800,
      emptyMessage: () => "No matches found in conversation history.",
      header: (queryStr, n) => `🔍 Search results: "${queryStr}" (${n})\n\n`,
      userLabel: "👤",
      assistantLabel: "🤖",
      truncatedMarker: "\n\n… [results truncated]",
    });
  }

  async handleTools(): Promise<string> {
    const res = await this.client.request<{ tools: ToolInfo[] } | ToolInfo[]>(
      "listTools",
    );
    const tools = Array.isArray(res) ? res : (res?.tools ?? []);
    return this.formatTools(tools);
  }

  async handleHealth(args = ""): Promise<string> {
    const data = await this.client.request<ToolHealthData>("toolHealth", {});
    return formatToolHealth(data, { verbose: args.trim() === "all" });
  }

  async handleSkills(): Promise<string> {
    const res = await this.client.request<
      { skills: SkillInfo[] } | SkillInfo[]
    >("listSkills");
    const skills = Array.isArray(res) ? res : (res?.skills ?? []);
    return this.formatSkills(skills);
  }

  async handleMcp(args = ""): Promise<string> {
    const parts = args.trim().split(/\s+/).filter(Boolean);
    if (parts[0]?.toLowerCase() === "refresh") {
      const server = parts[1];
      const res = await this.client.request<{
        refreshed: string[];
        error_count: number;
      }>("mcpRefresh", server ? { server } : {});
      return `🔄 ${this.formatter.bold("MCP Refreshed")}\n\nServers: ${res.refreshed?.join(", ") || "none"}\nErrors: ${res.error_count ?? 0}`;
    }
    const result = await this.client.request<McpServerList>("listMcpServers");
    return formatMcpServers(result);
  }

  async handlePlugins(): Promise<string> {
    const res = await this.client.request<
      { plugins: PluginInfo[] } | PluginInfo[]
    >("listPlugins");
    const plugins = Array.isArray(res) ? res : (res?.plugins ?? []);
    return this.formatPlugins(plugins);
  }

  async handleGit(): Promise<string> {
    const result = await this.client.request<GitStatusResult>("gitStatus");
    return this.formatGitStatus(result);
  }

  async handleDiff(): Promise<string> {
    const result = await this.client.request<GitDiffResult>("gitDiff");
    return this.formatGitDiff(result);
  }

  async handleCommit(message: string): Promise<string> {
    const msg = message.trim();
    if (!msg) {
      return this.formatError(
        "Missing commit message",
        `Usage: ${this.formatter.code("/commit <message>")}`,
      );
    }
    const result = await this.client.request<GitCommitResult>("gitCommit", {
      message: msg,
    });
    return this.formatGitCommit(result);
  }

  async handleAbort(control?: {
    request: (method: string, params?: unknown) => Promise<any>;
  }): Promise<string> {
    const target = control ?? this.client;
    await target.request("abort");
    return "⛔ Current task aborted.";
  }

  async handleProjects(args = ""): Promise<string> {
    const parts = args.trim().split(/\s+/).filter(Boolean);
    const sub = parts[0]?.toLowerCase() || "list";

    if (sub === "list" || parts.length === 0) {
      const res = await this.client.request<
        { projects: ProjectInfo[] } | ProjectInfo[]
      >("projectsList");
      const projects = Array.isArray(res) ? res : (res?.projects ?? []);
      return this.formatProjects(projects);
    }

    if (sub === "new") {
      const cwd = parts[1];
      if (!cwd) {
        return this.formatError(
          "Missing path",
          `Usage: ${this.formatter.code("/projects new <path> [description]")}`,
        );
      }
      const desc = parts.slice(2).join(" ") || undefined;
      const res = await this.client.request<{
        project_id: string;
        session_id: string;
      }>("projectsNew", { cwd, description: desc });
      return `📁 ${this.formatter.bold("Project Created & Switched")}\n\nProject ID: ${this.formatter.code(res.project_id)}\nSession: ${this.formatter.code(res.session_id)}`;
    }

    // Otherwise treat as switch prefix
    const res = await this.client.request<{
      project_id: string;
      session_id: string;
    }>("projectsSwitch", { id_prefix: parts[0] });
    return `📁 ${this.formatter.bold("Switched Project")}\n\nProject ID: ${this.formatter.code(res.project_id)}\nSession: ${this.formatter.code(res.session_id)}`;
  }

  async handleTask(args: string): Promise<string> {
    const trimmed = args.trim();
    if (!trimmed) {
      return this.formatError(
        "Missing argument",
        `Usage: ${this.formatter.code("/task <description> | /task stop <id>")}`,
      );
    }
    const tokens = trimmed.split(/\s+/);
    if (tokens[0].toLowerCase() === "stop" && tokens.length <= 2) {
      if (tokens.length === 1) {
        return this.formatError(
          "Missing argument",
          `Usage: ${this.formatter.code("/task stop <task-id>")}`,
        );
      }
      return this.handleTaskStop(tokens[1]);
    }
    const result = await this.client.request<{ task_id: string }>(
      "taskCreate",
      {
        description: trimmed,
        prompt: trimmed,
      },
    );
    return `🚀 ${this.formatter.bold("Task Created")}\n\nID: ${this.formatter.code(result.task_id)}`;
  }

  async handleTasks(): Promise<string> {
    const res = await this.client.request<{ tasks: TaskInfo[] } | TaskInfo[]>(
      "taskList",
    );
    const tasks = Array.isArray(res) ? res : (res?.tasks ?? []);
    return this.formatTasks(tasks);
  }

  async handleTaskStop(taskId: string): Promise<string> {
    const id = taskId.trim();
    if (!id) {
      return this.formatError(
        "Missing argument",
        `Usage: ${this.formatter.code("/task_stop <id>")}`,
      );
    }
    const result = await this.client.request<{ stopped: boolean }>("taskStop", {
      task_id: id,
    });
    return result?.stopped
      ? `⏹️ ${this.formatter.bold("Task Stopped")}\n\nID: ${this.formatter.code(id)}`
      : `⚠️ Task ${id} was not running or not found.`;
  }

  async handleCron(args = ""): Promise<string> {
    const parts = args.trim().split(/\s+/).filter(Boolean);
    const sub = parts[0]?.toLowerCase();

    if (!sub || sub === "list") {
      const res = await this.client.request<{ crons: CronJob[] } | CronJob[]>(
        "cronList",
      );
      const crons = Array.isArray(res) ? res : (res?.crons ?? []);
      return this.formatCronList(crons);
    }

    if (sub === "remove" || sub === "rm") {
      const id = parts[1];
      if (!id) {
        return this.formatError(
          "Missing job ID",
          `Usage: ${this.formatter.code("/cron remove <id>")}`,
        );
      }
      const res = await this.client.request<{ removed: boolean }>(
        "cronRemove",
        {
          id,
        },
      );
      return res?.removed
        ? `🗑️ ${this.formatter.bold("Cron Job Removed")}\n\nID: ${id}`
        : `⚠️ Cron job ${id} not found.`;
    }

    if (sub === "toggle") {
      const id = parts[1];
      if (!id) {
        return this.formatError(
          "Missing job ID",
          `Usage: ${this.formatter.code("/cron toggle <id>")}`,
        );
      }
      const res = await this.client.request<{
        id: string;
        enabled: boolean;
      }>("cronToggle", { id });
      const state = res.enabled ? "enabled" : "paused";
      return `⏰ ${this.formatter.bold("Cron Job Toggled")}\n\nID: ${id} is now ${state}.`;
    }

    return (
      `⏰ ${this.formatter.bold("Cron Commands")}\n\n` +
      `• ${this.formatter.code("/cron list")} — list scheduled jobs\n` +
      `• ${this.formatter.code("/cron toggle <id>")} — enable/pause a job\n` +
      `• ${this.formatter.code("/cron remove <id>")} — delete a job`
    );
  }

  async handleSpec(args = ""): Promise<string> {
    const parts = args.trim().split(/\s+/).filter(Boolean);
    const sub = parts[0]?.toLowerCase();

    switch (sub) {
      case "list": {
        const result = await this.client.request<
          { specs: SpecInfo[] } | SpecInfo[]
        >("specList");
        const specs = Array.isArray(result) ? result : (result?.specs ?? []);
        return this.formatSpecList(specs);
      }
      case "new": {
        const name = parts[1];
        if (!name) {
          return this.formatError(
            "Missing argument",
            `Usage: ${this.formatter.code("/spec new <name> [design] [bugfix]")}`,
          );
        }
        const params: Record<string, string> = { feature_name: name };
        if (parts.slice(1).includes("design")) params.workflow = "design";
        if (parts.slice(1).includes("bugfix")) params.spec_type = "bugfix";
        const result = await this.client.request<{
          feature_name: string;
          config: { workflow: string; phase: string };
        }>("specNew", params);
        return `✅ ${this.formatter.bold("Spec Created")}\n\nName: ${result.feature_name}\nWorkflow: ${result.config?.workflow}\nPhase: ${result.config?.phase}`;
      }
      case "show": {
        const name = parts[1];
        if (!name) {
          return this.formatError(
            "Missing argument",
            `Usage: ${this.formatter.code("/spec show <name>")}`,
          );
        }
        const result = await this.client.request<SpecDetail>("specShow", {
          feature_name: name,
        });
        return this.formatSpecShow(result);
      }
      case "status": {
        const name = parts[1];
        if (!name) {
          return this.formatError(
            "Missing argument",
            `Usage: ${this.formatter.code("/spec status <name>")}`,
          );
        }
        const result = await this.client.request<SpecProgress>("specStatus", {
          feature_name: name,
        });
        return this.formatSpecStatus(name, result);
      }
      case "run": {
        const name = parts[1];
        if (!name) {
          return this.formatError(
            "Missing argument",
            `Usage: ${this.formatter.code("/spec run <name> [task_id]")}`,
          );
        }
        const taskId = parts[2];
        const params: Record<string, string> = { feature_name: name };
        if (taskId) params.task_id = taskId;
        const result = await this.client.request<{
          task_id?: string;
          task_description?: string;
          status: string;
          message?: string;
        }>("specRun", params);
        return this.formatSpecRun(result);
      }
      default:
        return (
          `📋 ${this.formatter.bold("Spec Commands")}\n\n` +
          `• ${this.formatter.code("/spec list")} — list all specs\n` +
          `• ${this.formatter.code("/spec new <name> [design] [bugfix]")} — create a new spec\n` +
          `• ${this.formatter.code("/spec show <name>")} — view spec summary\n` +
          `• ${this.formatter.code("/spec status <name>")} — task progress counts\n` +
          `• ${this.formatter.code("/spec run <name> [task_id]")} — show next pending task`
        );
    }
  }

  async handleMemory(args = ""): Promise<string> {
    const parts = args.trim().split(/\s+/).filter(Boolean);
    const sub = parts[0]?.toLowerCase();

    if (!sub || sub === "list") {
      const res = await this.client.request<{
        entries: Array<{ id: string; content: string; category: string }>;
      }>("memoryList");
      const entries = res.entries ?? [];
      if (entries.length === 0) return "No long-term memories stored.";
      let out = `🧠 ${this.formatter.bold("Long-Term Memory")} (${entries.length})\n\n`;
      for (const m of entries) {
        out += `• [${m.category}] ${this.formatter.code(m.id)}: ${m.content}\n`;
      }
      return this.formatter.truncate(out.trimEnd());
    }

    if (sub === "add") {
      const content = parts.slice(1).join(" ").trim();
      if (!content) {
        return this.formatError(
          "Missing content",
          `Usage: ${this.formatter.code("/memory add <text>")}`,
        );
      }
      const res = await this.client.request<{
        id: string;
        created: boolean;
      }>("memoryAdd", { content, category: "general" });
      return `🧠 ${this.formatter.bold("Memory Stored")}\n\nID: ${this.formatter.code(res.id)}`;
    }

    if (sub === "delete" || sub === "rm") {
      const id = parts[1];
      if (!id) {
        return this.formatError(
          "Missing ID",
          `Usage: ${this.formatter.code("/memory delete <id>")}`,
        );
      }
      const res = await this.client.request<{ deleted: boolean }>(
        "memoryDelete",
        { id },
      );
      return res?.deleted
        ? `🗑️ ${this.formatter.bold("Memory Deleted")}\n\nID: ${id}`
        : `⚠️ Memory ${id} not found.`;
    }

    if (sub === "clear") {
      const res = await this.client.request<{ cleared: boolean }>(
        "memoryClear",
      );
      return res?.cleared
        ? `🗑️ ${this.formatter.bold("All Memories Cleared")}`
        : "⚠️ Failed to clear memory.";
    }

    if (sub === "stats") {
      const res =
        await this.client.request<Record<string, unknown>>("memoryStats");
      return (
        `🧠 ${this.formatter.bold("Memory Stats")}\n\n` +
        this.formatter.codeBlock(JSON.stringify(res, null, 2), "json")
      );
    }

    return (
      `🧠 ${this.formatter.bold("Memory Commands")}\n\n` +
      `• ${this.formatter.code("/memory list")} — list memories\n` +
      `• ${this.formatter.code("/memory add <text>")} — add a memory\n` +
      `• ${this.formatter.code("/memory delete <id>")} — delete by ID\n` +
      `• ${this.formatter.code("/memory stats")} — memory statistics\n` +
      `• ${this.formatter.code("/memory clear")} — clear all memories`
    );
  }

  async handleRate(args = ""): Promise<string> {
    const rating = args.trim().toLowerCase();
    if (!["good", "bad", "neutral"].includes(rating)) {
      return this.formatError(
        "Invalid rating",
        `Expected ${this.formatter.code("good")}, ${this.formatter.code("bad")}, or ${this.formatter.code("neutral")}.`,
      );
    }
    await this.client.request("evolution.rateTrajectory", { rating });
    return `⭐ Rating ${this.formatter.bold(rating)} recorded. Thank you!`;
  }

  async handleShutdown(): Promise<string> {
    await this.client.request("shutdown");
    return "🛑 Daemon shutdown requested.";
  }
}
