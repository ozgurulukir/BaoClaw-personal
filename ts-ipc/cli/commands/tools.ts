import {
  BOLD,
  DIM,
  FG_BLUE,
  FG_GRAY,
  FG_GREEN,
  FG_ORANGE,
  FG_RED,
  FG_WHITE,
  RESET,
} from "../../colors.js";
import type { McpRefreshResult, McpServerList } from "../../mcp.js";
import type { CliCommand, CliContext } from "../types.js";

export const toolsCommand: CliCommand = {
  name: "/tools",
  section: "Tools & Extensions",
  description: "List registered tools",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    try {
      const result = await ctx.client.call("listTools");
      console.log(
        `\n${FG_ORANGE}${BOLD}Registered Tools${RESET} ${DIM}(${result.count})${RESET}\n`,
      );

      const groups: Record<string, typeof result.tools> = {};
      for (const tool of result.tools) {
        const t = tool.type || "other";
        if (!groups[t]) groups[t] = [];
        groups[t].push(tool);
      }

      for (const [type, tools] of Object.entries(groups)) {
        const badge =
          type === "builtin"
            ? `${FG_GREEN}${type}${RESET}`
            : `${FG_BLUE}${type}${RESET}`;
        console.log(
          `  ${FG_GRAY}── ${badge} ${FG_GRAY}(${tools.length}) ──${RESET}`,
        );
        for (const tool of tools) {
          const desc = tool.description
            ? tool.description.length > 60
              ? tool.description.slice(0, 60) + "…"
              : tool.description
            : "";
          const marker = (tool as { deferred?: boolean }).deferred
            ? ` ${DIM}(deferred)${RESET}`
            : "";
          console.log(
            `  ${FG_WHITE}${tool.name}${RESET}  ${DIM}${desc}${RESET}${marker}`,
          );
        }
        console.log();
      }
    } catch (err) {
      console.error(`${FG_RED}Failed to list tools: ${err}${RESET}`);
    }
    ctx.rl.prompt();
  },
};

export const mcpCommand: CliCommand = {
  name: "/mcp",
  section: "Tools & Extensions",
  description: "List MCP servers (/mcp refresh [server] refreshes)",
  execute: async (args: string, ctx: CliContext): Promise<void> => {
    try {
      const requestArgs = args?.trim();
      if (requestArgs) {
        const [verb, ...rest] = requestArgs.split(/\s+/);
        if (verb !== "refresh") {
          console.log(
            `${FG_RED}Unknown argument: ${verb}${RESET} — usage: /mcp [refresh [server]]`,
          );
          ctx.rl.prompt();
          return;
        }
        const target = rest.join(" ");
        const result = await ctx.client.call("mcpRefresh", {
          server: target || undefined,
        });
        console.log(
          `\n${FG_ORANGE}${BOLD}MCP refresh requested${RESET} ${DIM}(${result.count})${RESET}\n`,
        );
        for (const srv of result.servers) {
          console.log(
            `  ${srv.runtime?.state ?? "?"} ${FG_WHITE}${srv.name}${RESET} ${DIM}${srv.runtime?.reason ?? ""}${RESET}`,
          );
        }
        console.log();
        ctx.rl.prompt();
        return;
      }
      const result = await ctx.client.call("listMcpServers");
      if (result.count === 0) {
        console.log(`\n${DIM}No MCP servers configured.${RESET}`);
        console.log(
          `${DIM}Add servers to .baoclaw/mcp.json or ~/.baoclaw/mcp.json${RESET}\n`,
        );
      } else {
        console.log(
          `\n${FG_ORANGE}${BOLD}MCP Servers${RESET} ${DIM}(${result.count})${RESET}\n`,
        );
        for (const srv of result.servers) {
          const state = srv.runtime?.state;
          const inactive =
            state === "skipped" ||
            state === "requires_restart" ||
            state === "disabled_by_config";
          const statusIcon = state
            ? state === "ready"
              ? `${FG_GREEN}●${RESET}`
              : state === "connecting"
                ? `${FG_ORANGE}●${RESET}`
                : inactive
                  ? `${DIM}●${RESET}`
                  : `${FG_RED}●${RESET}`
            : srv.disabled
              ? `${DIM}●${RESET}`
              : `${FG_GREEN}●${RESET}`;
          const source = `${DIM}[${srv.source}]${RESET}`;
          console.log(
            `  ${statusIcon} ${FG_WHITE}${BOLD}${srv.name}${RESET} ${source}`,
          );
          if (srv.command) {
            const cmdArgs = srv.args?.join(" ") || "";
            const cmd = `${srv.command} ${cmdArgs}`.trim();
            const short = cmd.length > 60 ? cmd.slice(0, 60) + "…" : cmd;
            console.log(`    ${DIM}${srv.server_type}: ${short}${RESET}`);
          } else if (srv.url) {
            console.log(`    ${DIM}${srv.server_type}: ${srv.url}${RESET}`);
          }
          if (state) {
            const parts = [state];
            if (srv.runtime?.tool_count) {
              parts.push(`${srv.runtime.tool_count} tools`);
            }
            if (srv.runtime?.restarts) {
              parts.push(`${srv.runtime.restarts} restarts`);
            }
            if (srv.runtime?.reason) {
              parts.push(srv.runtime.reason);
            }
            console.log(`    ${DIM}${parts.join(" — ")}${RESET}`);
          }
        }
        console.log();
      }
    } catch (err) {
      console.error(`${FG_RED}Failed to list MCP servers: ${err}${RESET}`);
    }
    ctx.rl.prompt();
  },
};

export const skillsCommand: CliCommand = {
  name: "/skills",
  section: "Tools & Extensions",
  description: "List discovered skills",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    try {
      const result = await ctx.client.call("listSkills");
      if (result.count === 0) {
        console.log(`\n${DIM}No skills found.${RESET}`);
        console.log(
          `${DIM}Add skills to .baoclaw/skills/ or ~/.baoclaw/skills/${RESET}\n`,
        );
      } else {
        console.log(
          `\n${FG_ORANGE}${BOLD}Skills${RESET} ${DIM}(${result.count})${RESET}\n`,
        );
        for (const skill of result.skills) {
          const source = `${DIM}[${skill.source}]${RESET}`;
          console.log(`  ${FG_WHITE}${BOLD}${skill.name}${RESET} ${source}`);
          if (skill.description) {
            console.log(`    ${DIM}${skill.description}${RESET}`);
          }
          console.log(`    ${DIM}${skill.path}${RESET}`);
        }
        console.log();
      }
    } catch (err) {
      console.error(`${FG_RED}Failed to list skills: ${err}${RESET}`);
    }
    ctx.rl.prompt();
  },
};

export const pluginsCommand: CliCommand = {
  name: "/plugins",
  section: "Tools & Extensions",
  description: "List discovered plugins",
  execute: async (_args: string, ctx: CliContext): Promise<void> => {
    try {
      const result = await ctx.client.call("listPlugins");
      if (result.count === 0) {
        console.log(`\n${DIM}No plugins found.${RESET}`);
        console.log(
          `${DIM}Add plugins to .baoclaw/plugins/ or ~/.baoclaw/plugins/${RESET}\n`,
        );
      } else {
        console.log(
          `\n${FG_ORANGE}${BOLD}Plugins${RESET} ${DIM}(${result.count})${RESET}\n`,
        );
        for (const plugin of result.plugins) {
          const ver = plugin.version ? ` ${DIM}v${plugin.version}${RESET}` : "";
          const source = `${DIM}[${plugin.source}]${RESET}`;
          const features: string[] = [];
          if (plugin.has_tools) features.push("tools");
          if (plugin.has_skills) features.push("skills");
          if (plugin.has_mcp) features.push("mcp");
          const featureStr =
            features.length > 0
              ? ` ${DIM}(${features.join(", ")})${RESET}`
              : "";
          console.log(
            `  ${FG_WHITE}${BOLD}${plugin.name}${RESET}${ver} ${source}${featureStr}`,
          );
          if (plugin.description) {
            console.log(`    ${DIM}${plugin.description}${RESET}`);
          }
        }
        console.log();
      }
    } catch (err) {
      console.error(`${FG_RED}Failed to list plugins: ${err}${RESET}`);
    }
    ctx.rl.prompt();
  },
};

export const toolsCommands: CliCommand[] = [
  toolsCommand,
  mcpCommand,
  skillsCommand,
  pluginsCommand,
];
