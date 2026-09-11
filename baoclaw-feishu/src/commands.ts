/**
 * Command system for BaoClaw Feishu Gateway.
 * Delegates core RPC command handlers to the shared Gateway SDK (`GatewayCommandBridge`)
 * while providing Feishu-specific CommandContext and gateway telemetry (/gateway, /status).
 */
import * as fs from "node:fs";
import * as os from "node:os";
import {
  GatewayCommandBridge,
  markdownFormatter,
  parseCommand as sharedParseCommand,
  isRegisteredCommand as sharedIsRegisteredCommand,
} from "baoclaw-ipc/gateway";
import { type IpcClient, type ControlChannel } from "baoclaw-ipc";
import { logger } from "./log.js";

const LOG_TAIL_CHARS = 3000;

// ── Feishu CommandContext & Types ──────────────────────────────────────────

export interface CommandContext {
  ipcClient: IpcClient;
  /** Dedicated connection for mid-turn RPCs (abort) — see attachControlChannel. */
  control: ControlChannel;
  args: string;
  sender: string;
  chatId: string;
  sendReply: (text: string) => Promise<void>;
}

export interface ParsedCommand {
  name: string;
  args: string;
}

export interface CommandDef {
  name: string;
  description: string;
  usage?: string;
  handler: (ctx: CommandContext) => Promise<string>;
}

export interface GatewayInfo {
  pid: number;
  startTime: number;
  logFile: string;
  name: string;
}

// ── Gateway & Daemon Runtime State ─────────────────────────────────────────

let _daemonInfo: { pid: number; session_id: string; cwd: string } | null = null;
let _daemonMetrics = { reconnectCount: 0, lastConnectAt: null as Date | null };
let _gatewayInfo: GatewayInfo | null = null;

export function setDaemonInfo(info: typeof _daemonInfo): void {
  _daemonInfo = info;
}

export function setDaemonMetrics(metrics: typeof _daemonMetrics): void {
  _daemonMetrics = metrics;
}

export function setGatewayInfo(info: GatewayInfo): void {
  _gatewayInfo = info;
}

// ── Parsing & Registration ────────────────────────────────────────────────

export function parseCommand(text: string): ParsedCommand | null {
  const parsed = sharedParseCommand(text);
  if (!parsed) return null;
  return { name: parsed.command, args: parsed.args };
}

export function isRegisteredCommand(nameOrText: string): boolean {
  return sharedIsRegisteredCommand(nameOrText, COMMAND_REGISTRY);
}

// ── Local Handlers (/gateway, /status, /abort, /start, /help) ──────────────

async function handleGateway(ctx: CommandContext): Promise<string> {
  const parts = ctx.args.trim().split(/\s+/);
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

// ── Command Registry ───────────────────────────────────────────────────────

function makeSharedHandler(cmdName: string) {
  return async (ctx: CommandContext): Promise<string> => {
    const bridge = new GatewayCommandBridge(ctx.ipcClient, markdownFormatter);
    const res = await bridge.dispatch(cmdName, ctx.args, ctx.control);
    return res ?? `Unknown command: ${cmdName}`;
  };
}

export const COMMAND_REGISTRY: Record<string, CommandDef> = {
  "/compact": {
    name: "/compact",
    description: "Compact session context",
    handler: makeSharedHandler("/compact"),
  },
  "/think": {
    name: "/think",
    description: "Extended thinking mode",
    handler: makeSharedHandler("/think"),
  },
  "/model": {
    name: "/model",
    description: "View or switch model",
    usage: "/model [name]",
    handler: makeSharedHandler("/model"),
  },
  "/history": {
    name: "/history",
    description: "Show recent messages",
    usage: "/history [n]",
    handler: makeSharedHandler("/history"),
  },
  "/search": {
    name: "/search",
    description: "Search conversation history",
    usage: "/search <query>",
    handler: makeSharedHandler("/search"),
  },
  "/export": {
    name: "/export",
    description: "Export session as Markdown",
    handler: makeSharedHandler("/export"),
  },
  "/abort": {
    name: "/abort",
    description: "Abort current task",
    handler: makeSharedHandler("/abort"),
  },
  "/git": {
    name: "/git",
    description: "Show git status",
    handler: makeSharedHandler("/git"),
  },
  "/diff": {
    name: "/diff",
    description: "Show git diff",
    handler: makeSharedHandler("/diff"),
  },
  "/commit": {
    name: "/commit",
    description: "Commit git changes",
    usage: "/commit <message>",
    handler: makeSharedHandler("/commit"),
  },
  "/tools": {
    name: "/tools",
    description: "List available tools",
    handler: makeSharedHandler("/tools"),
  },
  "/health": {
    name: "/health",
    description: "Tool health status",
    usage: "/health [all]",
    handler: makeSharedHandler("/health"),
  },
  "/mcp": {
    name: "/mcp",
    description: "List MCP servers",
    handler: makeSharedHandler("/mcp"),
  },
  "/skills": {
    name: "/skills",
    description: "List available skills",
    handler: makeSharedHandler("/skills"),
  },
  "/plugins": {
    name: "/plugins",
    description: "List installed plugins",
    handler: makeSharedHandler("/plugins"),
  },
  "/projects": {
    name: "/projects",
    description: "Manage projects",
    usage: "/projects [list|switch|add]",
    handler: makeSharedHandler("/projects"),
  },
  "/task": {
    name: "/task",
    description: "Create background task",
    usage: "/task <description>",
    handler: makeSharedHandler("/task"),
  },
  "/tasks": {
    name: "/tasks",
    description: "List background tasks",
    handler: makeSharedHandler("/tasks"),
  },
  "/task_stop": {
    name: "/task_stop",
    description: "Stop background task",
    usage: "/task_stop <id>",
    handler: makeSharedHandler("/task_stop"),
  },
  "/cron": {
    name: "/cron",
    description: "Manage cron jobs",
    usage: "/cron [list|remove|toggle]",
    handler: makeSharedHandler("/cron"),
  },
  "/help": {
    name: "/help",
    description: "Show help",
    handler: async () => formatHelp(),
  },
  "/status": {
    name: "/status",
    description: "Gateway status",
    handler: handleStatus,
  },
  "/start": {
    name: "/start",
    description: "Welcome message",
    handler: async () =>
      "🐾 BaoClaw Feishu Gateway\n\nWelcome to BaoClaw!\n\nSend a message to chat with the AI, or use / commands.\nType /help to see all available commands.",
  },
  "/gateway": {
    name: "/gateway",
    description: "Gateway management",
    usage: "/gateway [status|ping|logs]",
    handler: handleGateway,
  },
  "/spec": {
    name: "/spec",
    description: "Specification-driven development",
    usage: "/spec [list|new|show|status|run]",
    handler: makeSharedHandler("/spec"),
  },
};

// ── Dispatch ───────────────────────────────────────────────────────────────

export async function dispatchCommand(
  cmd: ParsedCommand,
  ctx: CommandContext,
): Promise<string | null> {
  const command = COMMAND_REGISTRY[cmd.name];
  if (!command) return null;

  const fullCtx: CommandContext = { ...ctx, args: cmd.args };

  try {
    return await command.handler(fullCtx);
  } catch (err: any) {
    const message = err instanceof Error ? err.message : String(err);
    logger.error(`Command ${cmd.name} failed: ${message}`);
    return `❌ Command failed\n${message}`;
  }
}
