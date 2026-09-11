import { BOLD, DIM, FG_GRAY, FG_ORANGE, FG_WHITE, RESET } from "../colors.js";
import type { CliCommand, CliContext, CommandEntry } from "./types.js";

export class CommandRegistry {
  private entries: CommandEntry[] = [];
  private map: Map<string, CommandEntry> = new Map();

  register(entry: CommandEntry): this {
    this.entries.push(entry);
    for (const name of entry.names) {
      this.map.set(name, entry);
    }
    return this;
  }

  registerCommand(cmd: CliCommand): this {
    const names = [cmd.name, ...(cmd.aliases ?? [])];
    return this.register({
      names,
      section: cmd.section,
      help: cmd.description,
      handler: cmd.execute,
    });
  }

  registerAll(entries: CommandEntry[]): this {
    for (const entry of entries) {
      this.register(entry);
    }
    return this;
  }

  find(name: string): CommandEntry | undefined {
    return this.map.get(name);
  }

  getEntries(): readonly CommandEntry[] {
    return this.entries;
  }

  renderHelp(): void {
    console.log(`\n${FG_ORANGE}${BOLD}Commands${RESET}\n`);
    const rows = this.entries.filter((e) => e.section && e.help);
    if (rows.length === 0) return;

    const width = Math.max(
      ...rows.map((e) => (e.label ?? e.names[0] ?? "").length),
    );
    let lastSection = "";
    for (const entry of rows) {
      if (entry.section && entry.section !== lastSection) {
        lastSection = entry.section;
        console.log(`\n  ${FG_GRAY}── ${lastSection} ──${RESET}`);
      }
      const label = (entry.label ?? entry.names[0] ?? "").padEnd(width);
      console.log(`  ${FG_WHITE}${label}${RESET}  ${DIM}${entry.help}${RESET}`);
    }
    console.log();
  }

  async execute(input: string, ctx: CliContext): Promise<boolean> {
    const trimmed = input.trim();
    if (!trimmed.startsWith("/")) {
      return false;
    }

    const spaceIdx = trimmed.indexOf(" ");
    const cmd = spaceIdx === -1 ? trimmed : trimmed.slice(0, spaceIdx);
    const cmdArgs = spaceIdx === -1 ? "" : trimmed.slice(spaceIdx + 1);

    const entry = this.find(cmd);
    if (!entry || !entry.handler) {
      return false;
    }

    await entry.handler(cmdArgs, ctx);
    return true;
  }
}
