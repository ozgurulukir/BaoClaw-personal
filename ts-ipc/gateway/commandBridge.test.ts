import { describe, it } from "node:test";
import * as assert from "node:assert/strict";
import {
  GatewayCommandBridge,
  parseCommand,
  isRegisteredCommand,
} from "./commandBridge.js";
import { markdownFormatter } from "./formatters/markdown.js";

describe("Gateway CommandBridge", () => {
  it("parseCommand parses slash command and arguments", () => {
    assert.equal(parseCommand("not a command"), null);
    assert.deepEqual(parseCommand("/tools"), { command: "/tools", args: "" });
    assert.deepEqual(parseCommand("/model gpt-4o"), {
      command: "/model",
      args: "gpt-4o",
    });
    assert.deepEqual(parseCommand("/task run some task"), {
      command: "/task",
      args: "run some task",
    });
  });

  it("isRegisteredCommand detects known commands", () => {
    assert.equal(isRegisteredCommand("/tools"), true);
    assert.equal(isRegisteredCommand("/compact"), true);
    assert.equal(isRegisteredCommand("/unknown_foo"), false);
  });

  it("dispatches compact command and formats result", async () => {
    const mockClient = {
      async request(method: string) {
        assert.equal(method, "compact");
        return {
          tokens_saved: 500,
          summary_tokens: 100,
          tokens_before: 1000,
          tokens_after: 500,
        };
      },
    };

    const bridge = new GatewayCommandBridge(mockClient, markdownFormatter);
    const output = await bridge.handleCompact();
    assert.match(output, /Context Compacted/);
    assert.match(output, /500/);
    assert.match(output, /50%/);
  });

  it("dispatches git status command", async () => {
    const mockClient = {
      async request(method: string) {
        assert.equal(method, "gitStatus");
        return {
          branch: "master",
          has_changes: true,
          staged_files: ["src/a.ts"],
          modified_files: ["src/b.ts"],
          untracked_files: [],
        };
      },
    };

    const bridge = new GatewayCommandBridge(mockClient, markdownFormatter);
    const output = await bridge.handleGit();
    assert.match(output, /Git Status/);
    assert.match(output, /master/);
    assert.match(output, /src\/a\.ts/);
    assert.match(output, /src\/b\.ts/);
  });

  it("dispatches commit with message validation", async () => {
    let committedMessage = "";
    const mockClient = {
      async request(method: string, params: any) {
        assert.equal(method, "gitCommit");
        committedMessage = params.message;
        return { hash: "abc1234", message: params.message };
      },
    };

    const bridge = new GatewayCommandBridge(mockClient, markdownFormatter);
    const emptyRes = await bridge.handleCommit("  ");
    assert.match(emptyRes, /Missing commit message/);

    const successRes = await bridge.handleCommit("feat: new bridge");
    assert.equal(committedMessage, "feat: new bridge");
    assert.match(successRes, /Committed/);
    assert.match(successRes, /abc1234/);
  });
});
