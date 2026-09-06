/**
 * Command handlers for the Telegram gateway.
 * Each handler checks connection, calls IPC, formats result, wraps in try/catch.
 * Factory receives the gateway's connections and runtime state; the returned
 * table maps registered slash commands to their handler functions.
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import {
  DaemonConnector,
  IpcClient,
  type ControlChannel,
  type DaemonInfo,
} from "baoclaw-ipc";
import { createLogger } from "baoclaw-ipc/logger";
import {
  SessionState,
  SearchResult,
  COMMAND_REGISTRY,
  formatTools,
  formatSkills,
  formatMcpServers,
  formatPlugins,
  formatCompact,
  formatGitStatus,
  formatGitDiff,
  formatGitCommit,
  formatThinkToggle,
  formatModelInfo,
  formatModelSwitch,
  formatCommitUsage,
  formatAbortConfirm,
  formatError,
  formatDisconnected,
  formatHelp,
  formatStatus,
  formatStart,
  formatSearchResults,
} from "./commands.js";
import {
  formatTranscriptToMarkdown,
  defaultExportFilename,
  markdownToPdf,
} from "./export.js";
import { CONFIG_PATH } from "./config.js";

const logger = createLogger("telegram");

export interface CommandHandlerDeps {
  ipcClient: IpcClient;
  /** Dedicated connection for mid-turn RPCs (abort) — see attachControlChannel. */
  control: ControlChannel;
  daemonInfo: DaemonInfo;
  botUsername: string;
  sessionState: SessionState;
  daemonConnector: DaemonConnector;
  sendDocument: (
    chatId: number,
    document: string,
    options?: Record<string, unknown>,
  ) => Promise<unknown>;
  /** Stops the bot, closes connections, removes the PID file, and exits. */
  quitGateway: () => void;
}

export type CommandHandlerTable = Record<
  string,
  (args: string, chatId: number) => Promise<string> | string
>;

export function createCommandHandlers(
  deps: CommandHandlerDeps,
): CommandHandlerTable {
  const {
    ipcClient,
    control,
    daemonInfo,
    botUsername,
    sessionState,
    daemonConnector,
    sendDocument,
    quitGateway,
  } = deps;

  // ── Command state ──
  let thinkingEnabled = false;
  let thinkingBudget: number | undefined;
  // Read model config from ~/.baoclaw/config.json
  let currentModel = "unknown";
  let fallbackModels: string[] = [];
  try {
    const raw = JSON.parse(fs.readFileSync(CONFIG_PATH, "utf-8"));
    currentModel = raw?.model || process.env.ANTHROPIC_MODEL || "unknown";
    fallbackModels = Array.isArray(raw?.fallback_models)
      ? raw.fallback_models
      : [];
  } catch {
    /* use defaults */
  }

  async function handleTools(): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    try {
      const result = await ipcClient.request<{ tools: any[]; count: number }>(
        "listTools",
      );
      return formatTools(result.tools, result.count);
    } catch (err) {
      return formatError(err);
    }
  }

  async function handleSkills(): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    try {
      const result = await ipcClient.request<{ skills: any[]; count: number }>(
        "listSkills",
      );
      return formatSkills(result.skills, result.count);
    } catch (err) {
      return formatError(err);
    }
  }

  async function handleMcp(): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    try {
      const result = await ipcClient.request<{ servers: any[]; count: number }>(
        "listMcpServers",
      );
      return formatMcpServers(result.servers, result.count);
    } catch (err) {
      return formatError(err);
    }
  }

  async function handlePlugins(): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    try {
      const result = await ipcClient.request<{ plugins: any[]; count: number }>(
        "listPlugins",
      );
      return formatPlugins(result.plugins, result.count);
    } catch (err) {
      return formatError(err);
    }
  }

  async function handleCompact(): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    try {
      const result = await ipcClient.request<{
        tokens_saved: number;
        summary_tokens: number;
        tokens_before: number;
        tokens_after: number;
      }>("compact");
      return formatCompact(result);
    } catch (err: any) {
      const msg = err?.message || "";
      if (msg.includes("session busy") || msg.includes("mutate busy")) {
        return "⏳ Session busy — unable to perform this operation.";
      }
      return formatError(err);
    }
  }

  async function handleThink(): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    try {
      thinkingEnabled = !thinkingEnabled;
      const settings = thinkingEnabled
        ? {
            thinking: {
              type: "enabled",
              budget_tokens: thinkingBudget ?? 10000,
            },
          }
        : { thinking: { type: "disabled" } };
      await ipcClient.request("updateSettings", { settings });
      return formatThinkToggle(
        thinkingEnabled,
        thinkingEnabled ? (thinkingBudget ?? 10000) : undefined,
      );
    } catch (err) {
      thinkingEnabled = !thinkingEnabled; // revert on failure
      return formatError(err);
    }
  }

  async function handleModel(args: string): Promise<string> {
    if (!args) {
      return formatModelInfo(currentModel, fallbackModels);
    }
    if (!ipcClient.connected) return formatDisconnected();
    try {
      await ipcClient.request("switchModel", { model: args });
      return formatModelSwitch(args);
    } catch (err: any) {
      const msg = err?.message || "";
      if (msg.includes("session busy") || msg.includes("mutate busy")) {
        return "⏳ Session busy — unable to perform this operation.";
      }
      return formatError(err);
    }
  }

  async function handleDiff(): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    try {
      const result = await ipcClient.request<{ diff: string }>("gitDiff");
      return formatGitDiff(result);
    } catch (err) {
      return formatError(err);
    }
  }

  async function handleCommit(args: string): Promise<string> {
    if (!args) return formatCommitUsage();
    if (!ipcClient.connected) return formatDisconnected();
    try {
      const result = await ipcClient.request<{ hash: string; message: string }>(
        "gitCommit",
        { message: args },
      );
      return formatGitCommit(result);
    } catch (err) {
      return formatError(err);
    }
  }

  async function handleGit(): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    try {
      const result = await ipcClient.request<any>("gitStatus");
      return formatGitStatus(result);
    } catch (err) {
      return formatError(err);
    }
  }

  async function handleAbort(): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    try {
      await control.request("abort");
      return formatAbortConfirm();
    } catch (err) {
      return formatError(err);
    }
  }

  function handleHelp(): string {
    return formatHelp(COMMAND_REGISTRY);
  }

  function handleStatus(): string {
    return formatStatus(daemonInfo, botUsername!, sessionState, {
      reconnectCount: daemonConnector.reconnectCount,
      lastConnectAt: daemonConnector.lastConnectAt,
    });
  }

  function handleStart(chatId: number): string {
    return formatStart(daemonInfo, chatId, sessionState);
  }

  function handleClear(): string {
    return (
      `ℹ️ Each Telegram connection has its own conversation history managed by the daemon. ` +
      `Reconnect the gateway for a fresh session.`
    );
  }

  async function handleShutdown(): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    try {
      await ipcClient.request("shutdown");
      // Daemon will exit, which triggers our onDisconnect handler
      return "🛑 Daemon is shutting down...";
    } catch (err) {
      return formatError(err);
    }
  }

  async function handleQuit(chatId: number): Promise<string> {
    // Send goodbye, then shut down the gateway process
    setTimeout(() => {
      logger.info("Quit requested via Telegram");
      quitGateway();
    }, 500);
    return "👋 Telegram Gateway is disconnecting... (daemon keeps running)";
  }

  async function handleMemory(args: string): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    const parts = args.split(/\s+/);
    const subCmd = parts[0] || "";
    const rest = parts.slice(1).join(" ");

    try {
      if (subCmd === "list" || subCmd === "ls") {
        const result = await ipcClient.request<{
          memories: any[];
          count: number;
        }>("memoryList");
        if (result.count === 0) return "No long-term memories.";
        let out = `🧠 Long-term Memory (${result.count})\n\n`;
        for (const m of result.memories) {
          out += `• [${m.id}] [${m.category}] ${m.content}\n`;
        }
        return out;
      } else if (subCmd === "add") {
        let category = "fact";
        let content = rest;
        if (
          parts[1] &&
          ["fact", "preference", "pref", "decision", "dec"].includes(parts[1])
        ) {
          category = parts[1];
          content = parts.slice(2).join(" ");
        }
        if (!content)
          return "Usage: /memory add [fact|preference|decision] <content>";
        const result = await ipcClient.request<{ memory: any }>("memoryAdd", {
          content,
          category,
        });
        return `✅ Memory added [${result.memory.id}] ${result.memory.content}`;
      } else if (subCmd === "delete" || subCmd === "del" || subCmd === "rm") {
        if (!rest) return "Usage: /memory delete <id>";
        const result = await ipcClient.request<{ deleted: boolean }>(
          "memoryDelete",
          { id: rest },
        );
        return result.deleted
          ? "✅ Memory deleted"
          : `❌ Memory not found: ${rest}`;
      } else if (subCmd === "clear") {
        const result = await ipcClient.request<{ cleared: number }>(
          "memoryClear",
        );
        return `✅ Cleared ${result.cleared} memories`;
      } else {
        return "🧠 Memory commands\n\n/memory list — list all memories\n/memory add [category] <content> — add a memory\n/memory delete <id> — delete a memory\n/memory clear — clear all memories";
      }
    } catch (err) {
      return formatError(err);
    }
  }

  async function handleHistory(args: string): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    const count = parseInt(args, 10) || 10;
    try {
      const result = await ipcClient.request<{
        messages: any[];
        count: number;
        total: number;
      }>("talkTail", { count });
      if (result.count === 0) return "No conversation history.";
      let out = `📜 Recent conversation (${result.count}/${result.total})\n\n`;
      for (const m of result.messages) {
        const ts = m.timestamp ? m.timestamp.slice(11, 19) : "";
        if (m.role === "user") {
          const text = (m.text || "").slice(0, 80);
          out += `${ts}  👤 ${text}${text.length >= 80 ? "…" : ""}\n`;
        } else if (m.role === "assistant") {
          const text = (m.text || "").slice(0, 80);
          const tools =
            m.tools && m.tools.length > 0 ? ` [${m.tools.length}🔧]` : "";
          out += `${ts}  🤖${tools} ${text}${text.length >= 80 ? "…" : ""}\n`;
        }
      }
      return out;
    } catch (err) {
      return formatError(err);
    }
  }

  async function handleExport(chatId: number, args?: string): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    try {
      const result = await ipcClient.request<{
        messages: any[];
        count: number;
        total: number;
      }>("talkTail", { count: 9999 });
      if (result.count === 0) return "No conversation history in this session";

      const entries = result.messages.map((m: any) => ({
        role: m.role as "user" | "assistant",
        text: m.text || "",
        timestamp: m.timestamp,
        tools: m.tools,
      }));

      const markdown = formatTranscriptToMarkdown(entries, {
        sessionId: sessionState.sessionId,
      });

      const isPdf = args?.trim().toLowerCase() === "pdf";
      const format = isPdf ? "pdf" : "markdown";
      const filename = defaultExportFilename(format);
      const filepath = path.join(os.tmpdir(), filename);

      if (isPdf) {
        const pdfBuf = await markdownToPdf(markdown);
        fs.writeFileSync(filepath, pdfBuf);
      } else {
        fs.writeFileSync(filepath, markdown, "utf-8");
      }

      try {
        await sendDocument(chatId, filepath, {
          caption: isPdf
            ? "📄 Conversation Export (PDF)"
            : "📄 Conversation Export",
        });
      } finally {
        try {
          fs.unlinkSync(filepath);
        } catch {}
      }

      return "";
    } catch (err) {
      return formatError(err);
    }
  }

  async function handleSearch(args: string): Promise<string> {
    if (!args.trim()) return "Usage: /search <query>";
    if (!ipcClient.connected) return formatDisconnected();
    try {
      const result = await ipcClient.request<{ results: SearchResult[] }>(
        "searchHistory",
        { query: args.trim(), max_results: 10 },
      );
      return formatSearchResults(result.results || [], args.trim());
    } catch (err) {
      return formatError(err);
    }
  }

  async function handleSpec(args: string): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    const parts = args.split(/\s+/);
    const subCmd = parts[0] || "list";
    const featureName = parts[1] || "";

    try {
      if (subCmd === "list") {
        const result = await ipcClient.request<{ specs: any[] }>("specList");
        const specs = result.specs || [];
        if (specs.length === 0)
          return "No specs yet. Use /spec new <feature-name> to create one.";
        let out = `📋 Specs (${specs.length})\n\n`;
        for (const s of specs) {
          const progress = s.task_progress
            ? ` [${s.task_progress.completed}/${s.task_progress.total}]`
            : "";
          out += `• ${s.feature_name}  ${s.phase}${progress}\n`;
        }
        return out;
      } else if (subCmd === "new") {
        if (!featureName)
          return "Usage: /spec new <feature-name> [requirements|design]";
        const workflow = parts[2] || "requirements";
        const result = await ipcClient.request<any>("specNew", {
          feature_name: featureName,
          workflow,
        });
        return `✅ Spec "${featureName}" created (${workflow})`;
      } else if (subCmd === "show") {
        if (!featureName) return "Usage: /spec show <feature-name>";
        const result = await ipcClient.request<any>("specShow", {
          feature_name: featureName,
        });
        const progress = result.task_progress
          ? `\nProgress: ${result.task_progress.completed}/${result.task_progress.total}`
          : "";
        return `📄 ${result.feature_name}\nPhase: ${result.phase}\nType: ${result.spec_type}${progress}`;
      } else if (subCmd === "status") {
        if (!featureName) return "Usage: /spec status <feature-name>";
        const result = await ipcClient.request<any>("specStatus", {
          feature_name: featureName,
        });
        return `📊 ${featureName}\nTotal: ${result.total} | Completed: ${result.completed} | In progress: ${result.in_progress}`;
      } else if (subCmd === "run") {
        if (!featureName) return "Usage: /spec run <feature-name> [task-id]";
        const taskId = parts[2] || undefined;
        const result = await ipcClient.request<any>("specRun", {
          feature_name: featureName,
          task_id: taskId,
        });
        if (result.status === "all_complete") return "✅ All tasks completed";
        return `▶️ Ready to run: [${result.task_id}] ${result.task_description}`;
      } else if (subCmd === "edit") {
        if (!featureName)
          return "Usage: /spec edit <feature-name> [requirements|design|tasks]";
        const phase = parts[2] || "requirements";
        const result = await ipcClient.request<any>("specEdit", {
          feature_name: featureName,
          phase,
        });
        const content = result.content || "";
        if (content.length > 4000) {
          return content.slice(0, 4000) + "\n\n...[message truncated]";
        }
        return content;
      } else {
        return "Usage: /spec [list|new|show|status|run|edit] <feature-name>";
      }
    } catch (err) {
      return formatError(err);
    }
  }

  // Command handler dispatch table
  async function handleCron(args: string): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    const parts = args.split(/\s+/);
    const subCmd = parts[0] || "";

    try {
      if (subCmd === "list" || subCmd === "") {
        const result = await ipcClient.request<{ jobs: any[]; count: number }>(
          "cronList",
        );
        if (result.count === 0)
          return "No cron jobs. Use /cron add to create one.";
        let out = `⏰ Cron Jobs (${result.count})\n\n`;
        for (const j of result.jobs) {
          const status = j.enabled ? "✅" : "⏸️";
          const last = j.last_run ? j.last_run.slice(0, 19) : "never run";
          const prompt =
            j.prompt.length > 50 ? j.prompt.slice(0, 50) + "…" : j.prompt;
          out += `${status} [${j.id}] ${j.name}  ${j.schedule}\n`;
          out += `  ${last}  ${prompt}\n\n`;
        }
        return out;
      } else if (subCmd === "add") {
        const match = args.match(/add\s+"([^"]+)"\s+"([^"]+)"\s+(.+)/);
        if (!match)
          return 'Usage: /cron add "name" "every 1h" prompt\n\nSupported: every 30m, daily 09:00, weekly mon 09:00';
        const result = await ipcClient.request<{ job: any }>("cronAdd", {
          name: match[1],
          schedule: match[2],
          prompt: match[3],
        });
        return `✅ Cron job created [${result.job.id}] ${result.job.name} (${result.job.schedule})`;
      } else if (subCmd === "remove" || subCmd === "rm") {
        const jobId = parts[1];
        if (!jobId) return "Usage: /cron remove <id>";
        const result = await ipcClient.request<{ removed: boolean }>(
          "cronRemove",
          { id: jobId },
        );
        return result.removed ? "✅ Deleted" : "❌ Job not found";
      } else if (subCmd === "toggle") {
        const jobId = parts[1];
        if (!jobId) return "Usage: /cron toggle <id>";
        const result = await ipcClient.request<{ enabled: boolean }>(
          "cronToggle",
          { id: jobId },
        );
        return result.enabled ? "✅ Enabled" : "⏸️ Disabled";
      } else {
        return '⏰ Cron job commands\n\n/cron list — list all jobs\n/cron add "name" "schedule" prompt\n/cron remove <id>\n/cron toggle <id>';
      }
    } catch (err) {
      return formatError(err);
    }
  }

  async function handleProjects(args: string): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    const parts = args.split(/\s+/);
    const subCmd = parts[0] || "";

    try {
      if (subCmd === "list" || subCmd === "") {
        const result = await ipcClient.request<{
          projects: any[];
          count: number;
        }>("projectsList");
        if (result.count === 0)
          return "No projects. Use /projects new <path> [description] to create one.";
        let out = `📂 Projects (${result.count})\n\n`;
        for (const p of result.projects) {
          const last = p.last_accessed ? p.last_accessed.slice(0, 10) : "";
          const sid = p.session_id ? `  session:${p.session_id}` : "";
          out += `[${p.id}] ${p.description}${last ? "  (" + last + ")" : ""}${sid}\n`;
          out += `  ${p.cwd}\n\n`;
        }
        out +=
          "Switch: /projects <id>  ·  New: /projects new <path> [description]";
        return out;
      } else if (subCmd === "new") {
        const rest = args.slice(3).trim();
        const spaceIdx = rest.indexOf(" ");
        let targetPath: string;
        let desc: string | undefined;
        if (spaceIdx > 0) {
          targetPath = rest.slice(0, spaceIdx);
          desc = rest.slice(spaceIdx + 1).trim() || undefined;
        } else {
          targetPath = rest;
        }
        if (!targetPath) return "Usage: /projects new <path> [description]";
        const params: Record<string, unknown> = { cwd: targetPath };
        if (desc) params.description = desc;
        const result = await ipcClient.request<{ project: any }>(
          "projectsNew",
          params,
        );
        return `✅ Created and switched to: ${result.project.description}\n  [${result.project.id}] ${result.project.cwd}`;
      } else {
        // /projects <id_prefix> — switch
        const result = await ipcClient.request<{
          project: any;
          message_count: number;
        }>("projectsSwitch", { id_prefix: subCmd });
        let msg = `📂 Switched to: ${result.project.description}\n  [${result.project.id}] ${result.project.cwd}`;
        if (result.message_count > 0)
          msg += `\n  Restored ${result.message_count} messages`;
        return msg;
      }
    } catch (err) {
      return formatError(err);
    }
  }

  async function handleTask(args: string): Promise<string> {
    if (!ipcClient.connected) return formatDisconnected();
    const parts = args.split(/\s+/);
    const subCmd = parts[0] || "";

    try {
      if (subCmd === "run") {
        const desc = args
          .slice(3)
          .trim()
          .replace(/^["']|["']$/g, "");
        if (!desc) return 'Usage: /task run "task description"';
        const result = await ipcClient.request<{ task_id: string }>(
          "taskCreate",
          { description: desc, prompt: desc },
        );
        return `✅ Background task created [${result.task_id}]`;
      } else if (subCmd === "list" || subCmd === "") {
        const result = await ipcClient.request<{ tasks: any[]; count: number }>(
          "taskList",
        );
        if (result.count === 0) return "No background tasks.";
        let out = `📋 Background Tasks (${result.count})\n\n`;
        for (const t of result.tasks) {
          const status =
            typeof t.status === "string" ? t.status : JSON.stringify(t.status);
          out += `[${t.id}] ${status} ${t.description}\n`;
        }
        return out;
      } else if (subCmd === "status") {
        const taskId = parts[1];
        if (!taskId) return "Usage: /task status <id>";
        const t = await ipcClient.request<any>("taskStatus", {
          task_id: taskId,
        });
        return `📋 Task ${t.id}\nStatus: ${typeof t.status === "string" ? t.status : JSON.stringify(t.status)}\nDescription: ${t.description}`;
      } else if (subCmd === "stop") {
        const taskId = parts[1];
        if (!taskId) return "Usage: /task stop <id>";
        const result = await ipcClient.request<{ stopped: boolean }>(
          "taskStop",
          { task_id: taskId },
        );
        return result.stopped ? "✅ Stopped" : "❌ Not found or not running";
      } else {
        return '📋 Background task commands\n\n/task run "description" — create a task\n/task list — list tasks\n/task status <id> — show task status\n/task stop <id> — stop a task';
      }
    } catch (err) {
      return formatError(err);
    }
  }

  // Command handler dispatch table
  const commandHandlers: Record<
    string,
    (args: string, chatId: number) => Promise<string> | string
  > = {
    "/tools": (args) => handleTools(),
    "/skills": (args) => handleSkills(),
    "/mcp": (args) => handleMcp(),
    "/plugins": (args) => handlePlugins(),
    "/compact": (args) => handleCompact(),
    "/think": (args) => handleThink(),
    "/model": (args) => handleModel(args),
    "/diff": (args) => handleDiff(),
    "/commit": (args) => handleCommit(args),
    "/git": (args) => handleGit(),
    "/abort": (args) => handleAbort(),
    "/help": () => handleHelp(),
    "/status": () => handleStatus(),
    "/start": (_args, chatId) => handleStart(chatId),
    "/clear": () => handleClear(),
    "/shutdown": () => handleShutdown(),
    "/quit": (_args, chatId) => handleQuit(chatId),
    "/memory": (args) => handleMemory(args),
    "/cron": (args) => handleCron(args),
    "/projects": (args) => handleProjects(args),
    "/task": (args) => handleTask(args),
    "/history": (args) => handleHistory(args),
    "/export": async (args, chatId) => handleExport(chatId, args),
    "/search": (args) => handleSearch(args),
    "/spec": (args) => handleSpec(args),
  };

  return commandHandlers;
}
