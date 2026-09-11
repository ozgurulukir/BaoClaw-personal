import type { CliCommand } from "../types.js";
import { gitCommands } from "./git.js";
import { modelCommands } from "./model.js";
import { sessionCommands } from "./session.js";
import { systemCommands } from "./system.js";
import { toolsCommands } from "./tools.js";

export * from "./system.js";
export * from "./tools.js";
export * from "./model.js";
export * from "./session.js";
export * from "./git.js";

export const allModularCommands: CliCommand[] = [
  ...systemCommands,
  ...toolsCommands,
  ...modelCommands,
  ...sessionCommands,
  ...gitCommands,
];
