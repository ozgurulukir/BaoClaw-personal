import type * as readline from "readline";
import type { IpcClient } from "../client.js";
import type { ControlChannel } from "../controlChannel.js";

export interface CliContext {
  client: IpcClient;
  control: ControlChannel;
  socketPath: string;
  rl: readline.Interface;
  currentModel?: string;
  availableModels?: string[];
  verboseMode?: boolean;
  thinkingEnabled?: boolean;
  debugMode?: boolean;
  sessionId?: string;
  isStreaming: boolean;
  currentText: string;
  toolCount: number;
  queryStartTime: number;
  startSpinner: (text: string) => void;
  stopSpinner: () => void;
  updatePrompt?: () => void;
  printPrompt: () => void;
  renderMarkdown?: (text: string) => string;
}

export type CommandHandler = (
  args: string,
  ctx: CliContext,
) => Promise<void> | void;

export interface CommandEntry {
  names: string[];
  section?: string;
  help?: string;
  label?: string;
  handler?: CommandHandler;
}

export interface CliCommand {
  name: string;
  aliases?: string[];
  section: string;
  description: string;
  usage?: string;
  execute: CommandHandler;
}
