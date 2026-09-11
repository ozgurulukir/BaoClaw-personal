import {
  BOLD,
  DIM,
  FG_CYAN,
  FG_GRAY,
  FG_GREEN,
  FG_ORANGE,
  FG_RED,
  FG_WHITE,
  FG_YELLOW,
  RESET,
} from "../../colors.js";
import type { SearchResult } from "../../search.js";
import type { CliCommand, CliContext } from "../types.js";

function timeSince(dateStr: string): string {
  const diffMs = Date.now() - new Date(dateStr).getTime();
  const mins = Math.floor(diffMs / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

export const compactCommand: CliCommand = {
  name: "/compact",
  section: "Conversation",
  description: "Compress conversation context",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    ctx.startSpinner("Compacting conversation...");
    try {
      const result = await ctx.client.request<{
        tokens_saved: number;
        summary_tokens: number;
        tokens_before: number;
        tokens_after: number;
      }>("compact");
      ctx.stopSpinner();
      if (result.tokens_saved === 0) {
        console.log(`\n${DIM}Not enough messages to compact.${RESET}\n`);
      } else {
        const pct = (
          (result.tokens_saved / result.tokens_before) *
          100
        ).toFixed(0);
        console.log(`\n${FG_GREEN}${BOLD}Compacted${RESET}`);
        console.log(
          `  ${FG_WHITE}Before:${RESET}  ${result.tokens_before.toLocaleString()} tokens`,
        );
        console.log(
          `  ${FG_WHITE}After:${RESET}   ${result.tokens_after.toLocaleString()} tokens`,
        );
        console.log(
          `  ${FG_WHITE}Saved:${RESET}   ${FG_GREEN}${result.tokens_saved.toLocaleString()} tokens (${pct}%)${RESET}`,
        );
        console.log(
          `  ${FG_WHITE}Summary:${RESET} ${result.summary_tokens.toLocaleString()} tokens\n`,
        );
      }
    } catch (err) {
      ctx.stopSpinner();
      console.error(`${FG_RED}Failed to compact: ${err}${RESET}`);
    }
    ctx.rl.prompt();
  },
};

export const sessionCommand: CliCommand = {
  name: "/session",
  section: "Session & Info",
  description: "Show current session info",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    try {
      const result = await ctx.client.request<any>("session.info", {});
      const formatDate = (iso?: string) => {
        if (!iso) return "Just now";
        try {
          const d = new Date(iso);
          return isNaN(d.getTime())
            ? iso
            : d
                .toISOString()
                .replace("T", " ")
                .replace(/\.\d+Z$/, "");
        } catch {
          return iso;
        }
      };
      console.log(`\n${FG_ORANGE}${BOLD}🔧 Session Info${RESET}`);
      console.log(`  ${FG_GRAY}─────────────────────────────────${RESET}`);
      console.log(
        `  ${FG_WHITE}ID:${RESET}                 ${FG_CYAN}${result.session_id ?? "?"}${RESET}`,
      );
      console.log(
        `  ${FG_WHITE}Working Dir:${RESET}        ${result.cwd ?? "?"}`,
      );
      console.log(
        `  ${FG_WHITE}Active Clients:${RESET}     ${result.client_count ?? 0}`,
      );
      console.log(
        `  ${FG_WHITE}Message Turns:${RESET}      ${result.message_count ?? 0}`,
      );
      console.log(
        `  ${FG_WHITE}Model:${RESET}              ${FG_CYAN}${result.model ?? "?"}${RESET}`,
      );
      console.log(
        `  ${FG_WHITE}Created At:${RESET}         ${formatDate(result.created_at)}`,
      );
      console.log(
        `  ${FG_WHITE}Last Active:${RESET}        ${formatDate(result.last_active)}\n`,
      );
    } catch (err) {
      console.error(`${FG_RED}Failed to get session info: ${err}${RESET}\n`);
    }
    ctx.rl.prompt();
  },
};

export const statusCommand: CliCommand = {
  name: "/status",
  section: "Session & Info",
  description: "Daemon connection & session overview",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    const connected = ctx.client.connected
      ? `${FG_GREEN}connected${RESET}`
      : `${FG_RED}disconnected${RESET}`;
    console.log(`\n${FG_ORANGE}${BOLD}Status${RESET}`);
    console.log(`  ${FG_WHITE}Daemon:${RESET}  ${connected}`);
    try {
      const info = await ctx.client.request<any>("session.info", {});
      console.log(
        `  ${FG_WHITE}Session:${RESET} ${FG_CYAN}${String(info.session_id ?? "?").slice(0, 8)}${RESET}`,
      );
      console.log(`  ${FG_WHITE}CWD:${RESET}     ${info.cwd ?? "?"}`);
      console.log(`  ${FG_WHITE}Model:${RESET}   ${info.model ?? "?"}`);
      console.log(`  ${FG_WHITE}Turns:${RESET}   ${info.message_count ?? 0}`);
      console.log(
        `  ${FG_WHITE}Active:${RESET}  ${info.last_active ? timeSince(info.last_active) : "now"}`,
      );
    } catch (err) {
      console.error(`${FG_RED}Failed to get session info: ${err}${RESET}`);
    }
    console.log();
    ctx.rl.prompt();
  },
};

export const exportCommand: CliCommand = {
  name: "/export",
  section: "Session & Info",
  description: "Export session transcript to Markdown: /export [path]",
  execute: async (args: string, ctx: CliContext): Promise<void> => {
    const outputPath = args.trim();
    ctx.startSpinner("Exporting conversation...");
    try {
      const result = await ctx.client.request<{
        file_path: string;
        message_count: number;
        size_bytes: number;
      }>("export", outputPath ? { output_path: outputPath } : {});
      ctx.stopSpinner();
      console.log(`\n${FG_GREEN}${BOLD}Exported${RESET}`);
      console.log(`  ${FG_WHITE}File:${RESET}     ${result.file_path}`);
      console.log(`  ${FG_WHITE}Messages:${RESET} ${result.message_count}`);
      console.log(
        `  ${FG_WHITE}Size:${RESET}     ${(result.size_bytes / 1024).toFixed(1)} KB\n`,
      );
    } catch (err: any) {
      ctx.stopSpinner();
      console.error(`${FG_RED}${err?.message || err}${RESET}\n`);
    }
    ctx.rl.prompt();
  },
};

export const searchCommand: CliCommand = {
  name: "/search",
  section: "Session & Info",
  description: "Search conversation history: /search <query> [limit]",
  execute: async (args: string, ctx: CliContext): Promise<void> => {
    const trimmed = args.trim();
    if (!trimmed) {
      console.log(`\n${FG_YELLOW}Usage: /search <query> [limit]${RESET}\n`);
      ctx.rl.prompt();
      return;
    }
    const tokens = trimmed.split(/\s+/);
    let limit = 10;
    if (tokens.length > 1 && /^\d+$/.test(tokens[tokens.length - 1])) {
      limit = Math.min(50, parseInt(tokens[tokens.length - 1], 10));
      tokens.pop();
    }
    const query = tokens.join(" ");
    ctx.startSpinner("Searching history...");
    try {
      const result = await ctx.client.request<{
        results: SearchResult[];
        count: number;
      }>("searchHistory", { query, max_results: limit });
      ctx.stopSpinner();
      if (!result.results || result.results.length === 0) {
        console.log(`\n${DIM}No results for "${query}"${RESET}\n`);
      } else {
        console.log(
          `\n${FG_ORANGE}${BOLD}Search${RESET} ${DIM}"${query}" (${result.results.length})${RESET}\n`,
        );
        for (const r of result.results) {
          const ts = (r.timestamp || "").slice(0, 19).replace("T", " ");
          const sid = (r.session_id || "").slice(0, 8);
          const raw = r.snippet || r.text || "";
          const oneLine = raw.replace(/\s+/g, " ").trim();
          const preview =
            oneLine.length > 120 ? oneLine.slice(0, 120) + "…" : oneLine;
          console.log(
            `  ${DIM}[${ts}]${RESET} ${FG_CYAN}${sid}${RESET}  ${preview}`,
          );
          if (r.cwd) console.log(`    ${DIM}${r.cwd}${RESET}`);
        }
        console.log();
      }
    } catch (err) {
      ctx.stopSpinner();
      console.error(`${FG_RED}Search failed: ${err}${RESET}`);
    }
    ctx.rl.prompt();
  },
};

export const sessionCommands: CliCommand[] = [
  compactCommand,
  sessionCommand,
  statusCommand,
  exportCommand,
  searchCommand,
];
