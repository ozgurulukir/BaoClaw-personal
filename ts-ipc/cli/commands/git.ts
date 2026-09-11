import {
  BOLD,
  DIM,
  FG_GREEN,
  FG_ORANGE,
  FG_RED,
  FG_YELLOW,
  RESET,
} from "../../colors.js";
import type { CliCommand, CliContext } from "../types.js";

export const diffCommand: CliCommand = {
  name: "/diff",
  section: "Projects & Git",
  description: "Git diff summary",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    ctx.startSpinner("Running git diff...");
    try {
      const result = await ctx.client.call("gitDiff");
      ctx.stopSpinner();
      console.log(`\n${FG_ORANGE}${BOLD}Git Diff${RESET}\n`);
      console.log(result.diff);
      console.log();
    } catch (err) {
      ctx.stopSpinner();
      console.error(`${FG_RED}${err}${RESET}`);
    }
    ctx.rl.prompt();
  },
};

export const commitCommand: CliCommand = {
  name: "/commit",
  section: "Projects & Git",
  description: "Stage all and commit: /commit <message>",
  execute: async (args: string, ctx: CliContext): Promise<void> => {
    const message = args.trim();
    if (!message) {
      console.log(`\n${FG_YELLOW}Usage: /commit <message>${RESET}\n`);
      ctx.rl.prompt();
      return;
    }
    ctx.startSpinner("Committing...");
    try {
      const result = await ctx.client.call("gitCommit", { message });
      ctx.stopSpinner();
      console.log(
        `\n${FG_GREEN}${BOLD}Committed${RESET} ${DIM}${result.hash}${RESET} ${result.message}\n`,
      );
    } catch (err) {
      ctx.stopSpinner();
      console.error(`${FG_RED}${err}${RESET}`);
    }
    ctx.rl.prompt();
  },
};

export const gitCommands: CliCommand[] = [diffCommand, commitCommand];
