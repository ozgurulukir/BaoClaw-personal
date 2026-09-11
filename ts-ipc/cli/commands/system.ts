import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import {
  BOLD,
  DIM,
  ESC,
  FG_GREEN,
  FG_ORANGE,
  FG_RED,
  RESET,
} from "../../colors.js";
import { formatToolHealth, type ToolHealthData } from "../../toolHealth.js";
import type { CliCommand, CliContext } from "../types.js";

export const quitCommand: CliCommand = {
  name: "/quit",
  aliases: ["/exit", "/q"],
  section: "Session & Info",
  description: "Disconnect (daemon keeps running; aliases: /exit, /q)",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    console.log(`\n${DIM}Disconnecting (daemon stays running)...${RESET}`);
    await ctx.control.close().catch(() => {});
    await ctx.client.disconnect();
    process.exit(0);
  },
};

export const shutdownCommand: CliCommand = {
  name: "/shutdown",
  section: "Session & Info",
  description: "Stop the daemon process",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    console.log(`\n${DIM}Shutting down daemon...${RESET}`);
    let daemonPid: number | null = null;
    try {
      const socketDir = path.join(os.tmpdir(), "baoclaw-sockets");
      for (const file of fs.readdirSync(socketDir)) {
        if (!file.endsWith(".json")) continue;
        try {
          const meta = JSON.parse(
            fs.readFileSync(path.join(socketDir, file), "utf-8"),
          );
          if (meta.socket === ctx.socketPath) {
            daemonPid = meta.pid;
            break;
          }
        } catch {}
      }
    } catch {}
    try {
      await ctx.client.request("shutdown");
    } catch {}
    await ctx.control.close().catch(() => {});
    await ctx.client.disconnect();
    if (daemonPid) {
      const deadline = Date.now() + 3000;
      while (Date.now() < deadline) {
        try {
          process.kill(daemonPid, 0);
        } catch {
          break;
        }
        await new Promise((r) => setTimeout(r, 200));
      }
      try {
        process.kill(daemonPid, 0);
        process.kill(daemonPid, "SIGKILL");
      } catch {}
    }
    process.exit(0);
  },
};

export const clearCommand: CliCommand = {
  name: "/clear",
  section: "Session & Info",
  description: "Clear screen",
  execute: (_args: string, ctx: CliContext): void => {
    process.stdout.write(`${ESC}2J${ESC}H`);
    ctx.rl.prompt();
  },
};

export const verboseCommand: CliCommand = {
  name: "/verbose",
  section: "Session & Info",
  description: "Verbose output controls: /verbose [on|off|status]",
  execute: (args: string, ctx: CliContext): void => {
    const arg = args.trim().toLowerCase();
    type LogLevel = "quiet" | "normal" | "verbose";
    const levels: LogLevel[] = ["quiet", "normal", "verbose"];
    const currentLevel = (): LogLevel =>
      (globalThis as any).__baoclaw_log_level ?? "verbose";
    if (arg === "" || arg === "status") {
      console.log(`${DIM}Log level: ${currentLevel()}${RESET}`);
      console.log(`${DIM}Usage: /verbose <on|off|status>${RESET}`);
    } else if (arg === "on") {
      (globalThis as any).__baoclaw_log_level = "verbose";
      console.log(`${FG_GREEN}✓ Log level: verbose${RESET}`);
    } else if (arg === "off") {
      (globalThis as any).__baoclaw_log_level = "quiet";
      console.log(`${FG_GREEN}✓ Log level: quiet${RESET}`);
    } else if (levels.includes(arg as LogLevel)) {
      (globalThis as any).__baoclaw_log_level = arg;
      console.log(`${FG_GREEN}✓ Log level: ${arg}${RESET}`);
    } else {
      console.log(`${FG_RED}Unknown level: ${arg}${RESET}`);
      console.log(`${DIM}Usage: /verbose <on|off|status>${RESET}`);
    }
    ctx.rl.prompt();
  },
};

export const healthCommand: CliCommand = {
  name: "/health",
  section: "Session & Info",
  description: "Tool health overview: /health [all]",
  execute: async (args: string, ctx: CliContext): Promise<void> => {
    try {
      const data = await ctx.client.request<ToolHealthData>("toolHealth", {});
      console.log(
        `\n${FG_ORANGE}${BOLD}🏥 Tool Health${RESET}\n${formatToolHealth(data, {
          verbose: args.trim() === "all",
        })}\n`,
      );
    } catch (err) {
      console.error(`${FG_RED}Failed to get tool health: ${err}${RESET}\n`);
    }
    ctx.rl.prompt();
  },
};

export const configCommand: CliCommand = {
  name: "/config",
  section: "Session & Info",
  description: "Show full config JSON (keys masked)",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    try {
      const result = await ctx.client.request<any>("config.show", {});
      const maskKeys = (obj: any): any => {
        if (obj === null || typeof obj !== "object") return obj;
        if (Array.isArray(obj)) return obj.map(maskKeys);
        const masked: Record<string, any> = {};
        for (const [k, v] of Object.entries(obj)) {
          if (
            typeof k === "string" &&
            /(api[_-]?key|token|secret|password)/i.test(k) &&
            typeof v === "string" &&
            v.length > 0
          ) {
            masked[k] =
              v.length > 8 ? `${v.slice(0, 4)}****${v.slice(-4)}` : "****";
          } else {
            masked[k] = maskKeys(v);
          }
        }
        return masked;
      };
      const masked = maskKeys(result);
      console.log(
        `\n${FG_ORANGE}${BOLD}⚙ Configuration${RESET} ${DIM}(keys masked)${RESET}\n`,
      );
      console.log(JSON.stringify(masked, null, 2));
      console.log();
    } catch (err) {
      console.error(`${FG_RED}Failed to get config: ${err}${RESET}\n`);
    }
    ctx.rl.prompt();
  },
};

export const systemCommands: CliCommand[] = [
  quitCommand,
  shutdownCommand,
  clearCommand,
  verboseCommand,
  healthCommand,
  configCommand,
];
