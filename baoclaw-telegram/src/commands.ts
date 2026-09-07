/**
 * Command module for BaoClaw Telegram Gateway.
 * Contains RPC response types, command registry, and parsing utilities.
 * Format functions and handlers are added in later tasks.
 */

// ═══════════════════════════════════════════════════════════════
// RPC Response Type Interfaces
// ═══════════════════════════════════════════════════════════════

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

export interface McpServerInfo {
  name: string;
  server_type: string; // 'stdio' | 'sse'
  disabled: boolean;
  source: string;
  command?: string;
  url?: string;
  config_path: string;
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

export interface SearchResult {
  timestamp?: string;
  entry_type: string;
  snippet?: string;
  context?: string;
}

export interface InitializeResult {
  capabilities: { tools: boolean; streaming: boolean; permissions: boolean };
  session_id: string;
  reconnected: boolean;
  resumed: boolean;
  message_count: number;
  shared?: boolean;
}

export interface SessionState {
  resumed: boolean;
  messageCount: number;
  sessionId: string;
  shared?: boolean;
}

// ═══════════════════════════════════════════════════════════════
// Command Definition & Registry
// ═══════════════════════════════════════════════════════════════

export interface CommandDefinition {
  description: string;
}

export const COMMAND_REGISTRY: Record<string, CommandDefinition> = {
  "/tools": { description: "List registered tools" },
  "/health": { description: "Tool health: /health [all]" },
  "/skills": { description: "List loaded skills" },
  "/mcp": { description: "List MCP servers" },
  "/plugins": { description: "List installed plugins" },
  "/compact": { description: "Compact the conversation context" },
  "/think": { description: "Toggle extended thinking mode" },
  "/model": { description: "Show or switch the model" },
  "/diff": { description: "Show git diff" },
  "/commit": { description: "Commit git changes" },
  "/git": { description: "Show git status" },
  "/abort": { description: "Abort the current task" },
  "/help": { description: "Show help" },
  "/status": { description: "Show gateway status" },
  "/start": { description: "Show the welcome message" },
  "/clear": { description: "Clear the session" },
  "/shutdown": { description: "Shut down the daemon" },
  "/quit": {
    description: "Disconnect the Telegram gateway (daemon keeps running)",
  },
  "/memory": { description: "Manage long-term memory" },
  "/cron": { description: "Cron jobs: /cron add|list|remove|toggle" },
  "/projects": {
    description: "Projects: /projects list|<id>|new <path> [desc]",
  },
  "/task": { description: "Background tasks: /task run|list|status|stop" },
  "/history": { description: "Recent conversation: /history [n]" },
  "/export": {
    description: "Export conversation as Markdown or PDF (/export pdf)",
  },
  "/search": { description: "Search conversation history: /search <query>" },
  "/spec": { description: "Specs: /spec list|new|show|status|run|edit" },
  "/rate": { description: "Rate the last interaction: /rate good|bad|neutral" },
};

// ═══════════════════════════════════════════════════════════════
// Command Parsing
// ═══════════════════════════════════════════════════════════════

/**
 * Parse a message text into a command name and arguments.
 * Returns null if the text is not a slash command.
 */
export function parseCommand(
  text: string,
): { command: string; args: string } | null {
  if (!text.startsWith("/")) return null;
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
 * Check whether a message text starts with a registered command.
 */
export function isRegisteredCommand(text: string): boolean {
  const parsed = parseCommand(text);
  if (!parsed) return false;
  return parsed.command in COMMAND_REGISTRY;
}

// ═══════════════════════════════════════════════════════════════
// List Format Functions (Task 2)
// ═══════════════════════════════════════════════════════════════

/**
 * Format a list of registered tools as plain text.
 */
export function formatTools(tools: ToolInfo[], count: number): string {
  if (count === 0) return "No tools registered.";

  // Group by type
  const groups: Record<string, ToolInfo[]> = {};
  for (const t of tools) {
    const type = t.type || "other";
    if (!groups[type]) groups[type] = [];
    groups[type].push(t);
  }

  let out = `🔧 Registered Tools (${count})\n\n`;
  for (const [type, items] of Object.entries(groups)) {
    out += `── ${type} (${items.length}) ──\n`;
    for (const t of items) {
      const desc = t.description
        ? t.description.length > 60
          ? t.description.slice(0, 60) + "…"
          : t.description
        : "";
      out += `• ${t.name}  ${desc}\n`;
    }
    out += "\n";
  }
  return out;
}

/**
 * Format a list of loaded skills as plain text.
 */
export function formatSkills(skills: SkillInfo[], count: number): string {
  if (count === 0) return "No skills loaded.";
  let out = `📚 Loaded Skills (${count})\n\n`;
  for (const s of skills) {
    out += `• ${s.name} [${s.source}]\n`;
    if (s.description) {
      out += `  ${s.description}\n`;
    }
  }
  return out;
}

/**
 * Format a list of MCP servers as plain text.
 */
export function formatMcpServers(
  servers: McpServerInfo[],
  count: number,
): string {
  if (count === 0) return "No MCP servers configured.";
  let out = `🌐 MCP Servers (${count})\n\n`;
  for (const srv of servers) {
    const status = srv.disabled ? "🔴" : "🟢";
    out += `${status} ${srv.name}  [${srv.server_type}] [${srv.source}]\n`;
  }
  return out;
}

/**
 * Format a list of installed plugins as plain text.
 */
export function formatPlugins(plugins: PluginInfo[], count: number): string {
  if (count === 0) return "No plugins installed.";
  let out = `🧩 Installed Plugins (${count})\n\n`;
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
  return out;
}

// ═══════════════════════════════════════════════════════════════
// Scalar Format Functions (Task 3)
// ═══════════════════════════════════════════════════════════════

/**
 * Format compact result showing tokens saved and summary tokens.
 */
export function formatCompact(result: CompactResult): string {
  const pct =
    result.tokens_before > 0
      ? ((result.tokens_saved / result.tokens_before) * 100).toFixed(0)
      : "0";
  return (
    `🗜️ Context Compacted\n\n` +
    `Before  ${result.tokens_before.toLocaleString()} tokens\n` +
    `After   ${result.tokens_after.toLocaleString()} tokens\n` +
    `Saved   ${result.tokens_saved.toLocaleString()} tokens (${pct}%)\n` +
    `Summary ${result.summary_tokens.toLocaleString()} tokens`
  );
}

/**
 * Format git status showing branch, staged, modified, and untracked files.
 */
export function formatGitStatus(result: GitStatusResult): string {
  const branch = result.branch ?? "(detached)";
  let out = `📂 Git Status\n\nBranch: ${branch}\n`;
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
  return out;
}

/**
 * Format git diff output. Returns friendly message when empty.
 */
export function formatGitDiff(result: GitDiffResult): string {
  if (!result.diff || result.diff.trim() === "") return "No changes.";
  return `📝 Git Diff\n\n${result.diff}`;
}

/**
 * Format git commit result showing hash and message.
 */
export function formatGitCommit(result: GitCommitResult): string {
  return `✅ Committed\n\nHash: ${result.hash}\nMessage: ${result.message}`;
}

/**
 * Format think toggle status with optional budget.
 */
export function formatThinkToggle(enabled: boolean, budget?: number): string {
  if (enabled) {
    const budgetStr = budget != null ? ` (budget: ${budget} tokens)` : "";
    return `🧠 Extended thinking enabled${budgetStr}`;
  }
  return "🧠 Extended thinking disabled";
}

/**
 * Format model info showing active model and fallback chain.
 */
export function formatModelInfo(
  activeModel: string,
  fallbackModels: string[],
): string {
  let out = `🤖 Model\n\nCurrent model: ${activeModel}\n`;
  if (fallbackModels.length > 0) {
    out += `\nFallback chain:\n`;
    out += `  0. ${activeModel} (primary)\n`;
    for (let i = 0; i < fallbackModels.length; i++) {
      out += `  ${i + 1}. ${fallbackModels[i]}\n`;
    }
  } else {
    out += "\nNo fallback models configured.";
  }
  return out;
}

/**
 * Format model switch confirmation.
 */
export function formatModelSwitch(model: string): string {
  return `✅ Switched to model: ${model}`;
}

/**
 * Format commit usage hint when no message is provided.
 */
export function formatCommitUsage(): string {
  return "Usage: /commit <message>";
}

/**
 * Format abort confirmation message.
 */
export function formatAbortConfirm(): string {
  return "⛔ Current task aborted.";
}

// ═══════════════════════════════════════════════════════════════
// Error and Help Functions (Task 4)
// ═══════════════════════════════════════════════════════════════

/**
 * Format an error message. Always starts with ❌ and includes error details.
 */
export function formatError(err: unknown): string {
  if (err instanceof Error) {
    return `❌ Command failed: ${err.message}`;
  }
  return `❌ Command failed: ${String(err)}`;
}

/**
 * Format a daemon disconnected warning.
 */
export function formatDisconnected(): string {
  return "⚠️ Daemon connection lost. Please restart the gateway.";
}

/**
 * Format help output listing all commands with descriptions.
 */
export function formatHelp(
  registry: Record<string, { description: string }>,
): string {
  // Group commands by category for cleaner display
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
    "🔧 Tools & Extensions": ["/tools", "/mcp", "/skills", "/plugins"],
    "⚙️ Automation": ["/task", "/cron", "/memory"],
    "🔌 Session": [
      "/help",
      "/status",
      "/start",
      "/clear",
      "/quit",
      "/shutdown",
    ],
  };

  let out = "📖 Available Commands\n\n";
  for (const [group, cmds] of Object.entries(groups)) {
    out += `${group}\n`;
    for (const cmd of cmds) {
      const def = registry[cmd];
      if (def) out += `  ${cmd} — ${def.description}\n`;
    }
    out += "\n";
  }

  // Any commands not in groups
  const grouped = new Set(Object.values(groups).flat());
  const ungrouped = Object.entries(registry).filter(
    ([cmd]) => !grouped.has(cmd),
  );
  if (ungrouped.length > 0) {
    for (const [cmd, def] of ungrouped) {
      out += `${cmd} — ${def.description}\n`;
    }
  }

  return out;
}

// ═══════════════════════════════════════════════════════════════
// Session Status & Start Functions (Task 6)
// ═══════════════════════════════════════════════════════════════

/**
 * Format the /status command output including session resume info.
 */
export function formatStatus(
  daemonInfo: { pid: number; session_id: string; cwd: string },
  botUsername: string,
  sessionState: SessionState,
  metrics?: { reconnectCount: number; lastConnectAt: Date | null },
): string {
  const sessionLine = sessionState.resumed
    ? `🔄 Resumed session (${sessionState.messageCount} messages)`
    : "🆕 New session";
  return (
    `🐾 BaoClaw Status\n\n` +
    `Daemon   pid=${daemonInfo.pid}\n` +
    `Session  ${daemonInfo.session_id}\n` +
    `CWD      ${daemonInfo.cwd}\n` +
    `Bot      @${botUsername}\n\n` +
    `Reconnects ${metrics?.reconnectCount ?? 0}\n` +
    `Last connect ${metrics?.lastConnectAt?.toISOString() ?? "never"}\n\n` +
    sessionLine
  );
}

/**
 * Format the /start welcome message including session resume info.
 */
export function formatStart(
  daemonInfo: { pid: number; session_id: string },
  chatId: number,
  sessionState: SessionState,
): string {
  let msg =
    `🐾 BaoClaw Telegram Gateway\n\n` +
    `Connected to daemon pid=${daemonInfo.pid}\n` +
    `Session: ${daemonInfo.session_id}\n` +
    `Your chat ID: ${chatId}\n\n` +
    `Send me any message to chat with BaoClaw.`;
  if (sessionState.resumed) {
    msg += `\n\n🔄 Resumed previous conversation (${sessionState.messageCount} messages)`;
  }
  return msg;
}

// ═══════════════════════════════════════════════════════════════
// Search Format Functions (Task 9)
// ═══════════════════════════════════════════════════════════════

/**
 * Format search results for Telegram display.
 * Truncates output to stay within Telegram's 4096 char limit.
 */
export function formatSearchResults(
  results: SearchResult[],
  query: string,
): string {
  if (!results.length) return "No matching results found.";
  let out = `🔍 Search results: "${query}" (${results.length})\n\n`;
  for (const r of results) {
    const ts = r.timestamp?.slice(0, 19).replace("T", " ") || "";
    const role = r.entry_type === "UserMessage" ? "User" : "Assistant";
    out += `[${ts}] ${role}\n${r.snippet || r.context || ""}\n\n`;
    if (out.length > 3800) {
      out += "…(more results truncated)";
      break;
    }
  }
  return out;
}
