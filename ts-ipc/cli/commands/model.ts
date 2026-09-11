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
import type { CliCommand, CliContext } from "../types.js";

export const thinkCommand: CliCommand = {
  name: "/think",
  section: "Conversation",
  description: "Toggle extended thinking mode",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    ctx.thinkingEnabled = !ctx.thinkingEnabled;
    const thinkingBudget = 10000;
    const settings = ctx.thinkingEnabled
      ? { thinking: { mode: "enabled", budget_tokens: thinkingBudget } }
      : { thinking: { mode: "disabled" } };
    try {
      await ctx.client.call("updateSettings", { settings });
      if (ctx.thinkingEnabled) {
        console.log(
          `\n${FG_GREEN}${BOLD}Extended thinking enabled${RESET} ${DIM}(budget: ${thinkingBudget} tokens)${RESET}\n`,
        );
      } else {
        console.log(`\n${FG_YELLOW}Extended thinking disabled${RESET}\n`);
      }
    } catch (err) {
      console.error(
        `${FG_RED}Failed to update thinking settings: ${err}${RESET}`,
      );
    }
    ctx.rl.prompt();
  },
};

export const abortCommand: CliCommand = {
  name: "/abort",
  section: "Conversation",
  description: "Cancel current request",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    ctx.stopSpinner();
    try {
      await ctx.control.call("abort");
    } catch {}
    ctx.currentText = "";
    ctx.isStreaming = false;
    ctx.toolCount = 0;
    ctx.queryStartTime = 0;
    console.log(`${FG_YELLOW}⚠ Aborted.${RESET}`);
    ctx.rl.prompt();
  },
};

export const debugCommand: CliCommand = {
  name: "/debug",
  section: "Conversation",
  description: "Toggle timing debug for next query",
  execute: (_args: string, ctx: CliContext): void => {
    ctx.debugMode = !ctx.debugMode;
    console.log(
      `\n${ctx.debugMode ? FG_GREEN : FG_YELLOW}Debug timing for next query: ${ctx.debugMode ? "on" : "off"}${RESET}\n`,
    );
    ctx.rl.prompt();
  },
};

export const tokensCommand: CliCommand = {
  name: "/tokens",
  aliases: ["/token"],
  section: "Session & Info",
  description: "Show token usage stats",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    try {
      const result = await ctx.client.call("session.tokens");
      if (result && result.current_tokens !== undefined) {
        const ctxWin = result.context_window ?? 0;
        const pct =
          ctxWin > 0
            ? ((result.current_tokens / ctxWin) * 100).toFixed(1)
            : "?";
        const remaining = Math.max(0, ctxWin - result.current_tokens);
        const thrRatio = result.threshold_ratio ?? 0;
        const untilCompact =
          thrRatio > 0
            ? Math.max(0, Math.round(ctxWin * thrRatio) - result.current_tokens)
            : remaining;
        console.log(
          `\n${FG_ORANGE}${BOLD}📊 Token Usage${RESET} ${DIM}(session: ${String(result.session_id ?? "").slice(0, 8) || "?"})${RESET}`,
        );
        console.log(`  ${FG_GRAY}─────────────────────────────────${RESET}`);
        console.log(
          `  ${FG_WHITE}In use:${RESET}     ${FG_CYAN}${(result.current_tokens ?? 0).toLocaleString()}${RESET} / ${ctxWin.toLocaleString()} tokens ${DIM}(${pct}%)${RESET}`,
        );
        console.log(
          `  ${FG_WHITE}Until compact:${RESET}     ${FG_YELLOW}${untilCompact.toLocaleString()}${RESET} tokens ${DIM}(${(thrRatio * 100).toFixed(0)}% threshold)${RESET}`,
        );
        console.log(`  ${FG_GRAY}─────────────────────────────────${RESET}`);
        console.log(
          `  ${FG_WHITE}Total input:${RESET}     ${result.total_input_tokens != null ? (result.total_input_tokens as number).toLocaleString() : "N/A"}`,
        );
        console.log(
          `  ${FG_WHITE}Total output:${RESET}     ${result.total_output_tokens != null ? (result.total_output_tokens as number).toLocaleString() : "N/A"}`,
        );
        console.log(`  ${FG_GRAY}─────────────────────────────────${RESET}`);
        console.log(
          `  ${DIM}model: ${result.model ?? "unknown"} | window: ${ctxWin > 0 ? (ctxWin / 1_000_000).toFixed(1) + "M" : "?"}${RESET}\n`,
        );
      } else {
        console.log(`\n${FG_YELLOW}⚠ Token data unavailable${RESET}\n`);
      }
    } catch (err) {
      console.error(`${FG_RED}Failed to get token usage: ${err}${RESET}\n`);
    }
    ctx.rl.prompt();
  },
};

export const rateCommand: CliCommand = {
  name: "/rate",
  section: "Session & Info",
  description: "Rate the last interaction: /rate <good|bad|neutral>",
  execute: async (args: string, ctx: CliContext): Promise<void> => {
    const rating = args.trim().toLowerCase();
    if (!["good", "bad", "neutral"].includes(rating)) {
      console.log(
        `\n${FG_YELLOW}Usage: /rate <good|bad|neutral> — rate the last interaction for preference data${RESET}\n`,
      );
      ctx.rl.prompt();
      return;
    }
    try {
      await ctx.client.call("evolution.rateTrajectory", { rating });
      console.log(`\n${FG_GREEN}✓ Rated last interaction: ${rating}${RESET}\n`);
    } catch (err: any) {
      console.error(
        `${FG_RED}Failed to record rating: ${err?.message || err}${RESET}\n`,
      );
    }
    ctx.rl.prompt();
  },
};

export const costCommand: CliCommand = {
  name: "/cost",
  section: "Session & Info",
  description: "Show cost estimate",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    try {
      const result = await ctx.client.request<any>("session.cost", {});
      const fmtCost = (v: any) =>
        typeof v === "number" ? v.toFixed(4) : "N/A";
      const fmtTokens = (v: any) =>
        typeof v === "number" ? v.toLocaleString() : "0";
      console.log(`\n${FG_ORANGE}${BOLD}💰 Cost Estimate${RESET}`);
      console.log(`  ${FG_GRAY}─────────────────────────────────${RESET}`);
      console.log(
        `  ${FG_WHITE}This session:${RESET}   ${FG_GREEN}$${fmtCost(result.session_cost)}${RESET}`,
      );
      console.log(
        `  ${FG_WHITE}  Input:${RESET}     $${fmtCost(result.input_cost)} ${DIM}(${fmtTokens(result.input_tokens)} tokens)${RESET}`,
      );
      console.log(
        `  ${FG_WHITE}  Output:${RESET}     $${fmtCost(result.output_cost)} ${DIM}(${fmtTokens(result.output_tokens)} tokens)${RESET}`,
      );
      console.log(`  ${FG_GRAY}─────────────────────────────────${RESET}`);
      console.log(
        `  ${DIM}model: ${result.model ?? "unknown"} | in $${result.input_price_per_million ?? "?"}/M | out $${result.output_price_per_million ?? "?"}/M${RESET}\n`,
      );
    } catch (err) {
      console.error(`${FG_RED}Failed to get cost estimate: ${err}${RESET}\n`);
    }
    ctx.rl.prompt();
  },
};

export const modelCommands: CliCommand[] = [
  thinkCommand,
  abortCommand,
  debugCommand,
  tokensCommand,
  rateCommand,
  costCommand,
];
