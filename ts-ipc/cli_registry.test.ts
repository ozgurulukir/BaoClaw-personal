import assert from "node:assert/strict";
import { describe, test } from "node:test";
import {
  abortCommand,
  allModularCommands,
  clearCommand,
  CommandRegistry,
  debugCommand,
  thinkCommand,
  toolsCommand,
  verboseCommand,
} from "./cli/index.js";
import type { CliContext, CommandEntry } from "./cli/types.js";

function createMockContext(overrides: Partial<CliContext> = {}): CliContext {
  return {
    client: {
      connected: true,
      request: async () => ({}),
      disconnect: async () => {},
    } as any,
    control: {
      request: async () => ({}),
      close: async () => {},
    } as any,
    socketPath: "/tmp/mock.sock",
    rl: { prompt: () => {} } as any,
    currentModel: "claude-3-5-sonnet",
    availableModels: ["claude-3-5-sonnet"],
    verboseMode: false,
    thinkingEnabled: false,
    debugMode: false,
    isStreaming: false,
    currentText: "",
    toolCount: 0,
    queryStartTime: 0,
    startSpinner: () => {},
    stopSpinner: () => {},
    updatePrompt: () => {},
    printPrompt: () => {},
    ...overrides,
  };
}

describe("CommandRegistry", () => {
  test("registers command and finds by primary name or alias", () => {
    const registry = new CommandRegistry();
    let executed = false;
    let receivedArgs = "";

    const entry: CommandEntry = {
      names: ["/quit", "/exit", "/q"],
      section: "Session & Info",
      help: "Disconnect",
      handler: async (args: string) => {
        executed = true;
        receivedArgs = args;
      },
    };

    registry.register(entry);

    assert.equal(registry.find("/quit"), entry);
    assert.equal(registry.find("/exit"), entry);
    assert.equal(registry.find("/q"), entry);
    assert.equal(registry.find("/unknown"), undefined);
  });

  test("executes command matching name and passes arguments", async () => {
    const registry = new CommandRegistry();
    let executedArgs = "";

    registry.register({
      names: ["/model"],
      section: "Conversation",
      help: "Select model",
      handler: async (args: string) => {
        executedArgs = args;
      },
    });

    const ctx = createMockContext();

    const res1 = await registry.execute("/model", ctx);
    assert.equal(res1, true);
    assert.equal(executedArgs, "");

    const res2 = await registry.execute("/model gpt-4o", ctx);
    assert.equal(res2, true);
    assert.equal(executedArgs, "gpt-4o");

    const res3 = await registry.execute("hello world", ctx);
    assert.equal(res3, false);

    const res4 = await registry.execute("/unregistered_cmd foo", ctx);
    assert.equal(res4, false);
  });

  test("does not prefix-match glued commands", async () => {
    const registry = new CommandRegistry();
    let executed = false;

    registry.register({
      names: ["/doc"],
      section: "Input",
      help: "Attach document",
      handler: () => {
        executed = true;
      },
    });

    const ctx = createMockContext();
    const handled = await registry.execute("/docx file.docx", ctx);
    assert.equal(handled, false);
    assert.equal(executed, false);
  });

  test("registerCommand registers all aliases", () => {
    const registry = new CommandRegistry();
    registry.registerCommand({
      name: "/clear",
      aliases: ["/cls"],
      section: "Session",
      description: "Clear screen",
      execute: () => {},
    });

    assert.ok(registry.find("/clear"));
    assert.ok(registry.find("/cls"));
  });

  test("allModularCommands registers all submodules without duplicates", () => {
    const registry = new CommandRegistry();
    for (const cmd of allModularCommands) {
      registry.registerCommand(cmd);
    }

    assert.ok(registry.find("/quit"));
    assert.ok(registry.find("/exit"));
    assert.ok(registry.find("/q"));
    assert.ok(registry.find("/clear"));
    assert.ok(registry.find("/verbose"));
    assert.ok(registry.find("/health"));
    assert.ok(registry.find("/tools"));
    assert.ok(registry.find("/mcp"));
    assert.ok(registry.find("/skills"));
    assert.ok(registry.find("/plugins"));
    assert.ok(registry.find("/think"));
    assert.ok(registry.find("/abort"));
    assert.ok(registry.find("/debug"));
    assert.ok(registry.find("/tokens"));
    assert.ok(registry.find("/rate"));
    assert.ok(registry.find("/cost"));
    assert.ok(registry.find("/compact"));
    assert.ok(registry.find("/session"));
    assert.ok(registry.find("/status"));
    assert.ok(registry.find("/export"));
    assert.ok(registry.find("/search"));
  });

  test("verboseCommand updates global log level", () => {
    let prompted = false;
    const ctx = createMockContext({
      rl: { prompt: () => (prompted = true) } as any,
    });

    verboseCommand.execute("on", ctx);
    assert.equal((globalThis as any).__baoclaw_log_level, "verbose");
    assert.equal(prompted, true);

    verboseCommand.execute("off", ctx);
    assert.equal((globalThis as any).__baoclaw_log_level, "quiet");
  });

  test("debugCommand toggles debugMode flag", () => {
    let prompted = false;
    const ctx = createMockContext({
      rl: { prompt: () => (prompted = true) } as any,
      debugMode: false,
    });

    debugCommand.execute("", ctx);
    assert.equal(ctx.debugMode, true);
    assert.equal(prompted, true);

    debugCommand.execute("", ctx);
    assert.equal(ctx.debugMode, false);
  });

  test("abortCommand stops spinner and requests abort on control channel", async () => {
    let spinnerStopped = false;
    let abortRequested = false;
    let prompted = false;

    const ctx = createMockContext({
      stopSpinner: () => (spinnerStopped = true),
      control: {
        request: async (method: string) => {
          if (method === "abort") abortRequested = true;
          return {};
        },
      } as any,
      rl: { prompt: () => (prompted = true) } as any,
      isStreaming: true,
      currentText: "generating...",
    });

    await abortCommand.execute("", ctx);
    assert.equal(spinnerStopped, true);
    assert.equal(abortRequested, true);
    assert.equal(ctx.isStreaming, false);
    assert.equal(ctx.currentText, "");
    assert.equal(prompted, true);
  });

  test("thinkCommand updates daemon settings", async () => {
    let requestedSettings: any = null;
    let prompted = false;

    const ctx = createMockContext({
      thinkingEnabled: false,
      client: {
        request: async (method: string, params: any) => {
          if (method === "updateSettings") requestedSettings = params.settings;
          return {};
        },
      } as any,
      rl: { prompt: () => (prompted = true) } as any,
    });

    await thinkCommand.execute("", ctx);
    assert.equal(ctx.thinkingEnabled, true);
    assert.deepEqual(requestedSettings, {
      thinking: { mode: "enabled", budget_tokens: 10000 },
    });
    assert.equal(prompted, true);
  });

  test("toolsCommand queries listTools and prompts", async () => {
    let queried = false;
    let prompted = false;

    const ctx = createMockContext({
      client: {
        request: async (method: string) => {
          if (method === "listTools") {
            queried = true;
            return {
              tools: [
                {
                  name: "bash",
                  description: "Execute bash",
                  type: "builtin",
                },
              ],
              count: 1,
            };
          }
          return {};
        },
      } as any,
      rl: { prompt: () => (prompted = true) } as any,
    });

    await toolsCommand.execute("", ctx);
    assert.equal(queried, true);
    assert.equal(prompted, true);
  });

  test("state proxies correctly mutate outer scope variables", () => {
    let outerThinking = false;
    let outerDebug = false;

    const ctx = createMockContext();
    Object.defineProperty(ctx, "thinkingEnabled", {
      get: () => outerThinking,
      set: (v) => {
        outerThinking = !!v;
      },
    });
    Object.defineProperty(ctx, "debugMode", {
      get: () => outerDebug,
      set: (v) => {
        outerDebug = !!v;
      },
    });

    debugCommand.execute("", ctx);
    assert.equal(outerDebug, true);

    debugCommand.execute("", ctx);
    assert.equal(outerDebug, false);
  });
});
