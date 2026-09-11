import { describe, it } from "node:test";
import * as assert from "node:assert";
import * as fs from "node:fs";
import * as path from "node:path";
import * as net from "node:net";
import { ALL_IPC_METHODS, isIpcMethod, IpcClient } from "./index.js";

describe("IPC Protocol Contract & Schema Parity", () => {
  it("ALL_IPC_METHODS contains exactly 87 distinct RPC methods", () => {
    assert.strictEqual(ALL_IPC_METHODS.length, 87);
    const unique = new Set(ALL_IPC_METHODS);
    assert.strictEqual(unique.size, 87, "All method names must be unique");
  });

  it("isIpcMethod guard validates valid and invalid methods correctly", () => {
    assert.strictEqual(isIpcMethod("listTools"), true);
    assert.strictEqual(isIpcMethod("gitCommit"), true);
    assert.strictEqual(isIpcMethod("session.tokens"), true);
    assert.strictEqual(isIpcMethod("telemetry.setEnabled"), true);
    assert.strictEqual(isIpcMethod("unknownMethod"), false);
    assert.strictEqual(isIpcMethod(""), false);
  });

  it("Parity with Rust: ALL_IPC_METHODS matches router.rs ClientMethod definitions", () => {
    const routerPath = path.resolve(
      path.dirname(new URL(import.meta.url).pathname),
      "../baoclaw-core/src/ipc/router.rs",
    );
    assert.ok(
      fs.existsSync(routerPath),
      `router.rs must exist at ${routerPath}`,
    );
    const routerSource = fs.readFileSync(routerPath, "utf-8");

    // Extract all serde rename method names from ClientMethod enum
    const regex = /#\[serde\(rename = "([^"]+)"\)\]/g;
    const rustMethods: string[] = [];
    let match: RegExpExecArray | null;
    while ((match = regex.exec(routerSource)) !== null) {
      rustMethods.push(match[1]);
    }

    assert.strictEqual(
      rustMethods.length,
      87,
      `Expected 87 serde rename attributes in ClientMethod, found ${rustMethods.length}`,
    );

    const rustSet = new Set(rustMethods);
    const tsSet = new Set(ALL_IPC_METHODS);

    const missingInTs = rustMethods.filter((m) => !tsSet.has(m as never));
    const missingInRust = ALL_IPC_METHODS.filter((m) => !rustSet.has(m));

    assert.deepStrictEqual(
      missingInTs,
      [],
      `Methods defined in Rust router.rs but missing in TypeScript protocol: ${missingInTs.join(", ")}`,
    );
    assert.deepStrictEqual(
      missingInRust,
      [],
      `Methods defined in TypeScript protocol but missing in Rust router.rs: ${missingInRust.join(", ")}`,
    );
  });

  it("IpcClient.call() sends correct JSON-RPC and receives typed result", async () => {
    const socketPath = path.join(
      process.cwd(),
      `test-protocol-${Date.now()}-${Math.random().toString(36).slice(2, 6)}.sock`,
    );

    let receivedRequest: Record<string, unknown> | null = null;
    const server = net.createServer((socket) => {
      let buf = "";
      socket.on("data", (chunk) => {
        buf += chunk.toString("utf-8");
        const idx = buf.indexOf("\n");
        if (idx !== -1) {
          const line = buf.slice(0, idx).trim();
          receivedRequest = JSON.parse(line);
          const response = {
            jsonrpc: "2.0",
            id: (receivedRequest as { id: number }).id,
            result: {
              tools: [
                { name: "bash", description: "Run command", type: "builtin" },
              ],
              count: 1,
            },
          };
          socket.write(JSON.stringify(response) + "\n");
        }
      });
    });

    await new Promise<void>((resolve) => server.listen(socketPath, resolve));

    try {
      const client = new IpcClient({ requestTimeoutMs: 5000 });
      await client.connect(socketPath);

      const result = await client.call("listTools");

      assert.strictEqual(result.count, 1);
      assert.strictEqual(result.tools[0].name, "bash");
      assert.strictEqual(result.tools[0].type, "builtin");

      assert.ok(receivedRequest);
      assert.strictEqual(
        (receivedRequest as { method: string }).method,
        "listTools",
      );

      await client.disconnect();
    } finally {
      server.close();
      if (fs.existsSync(socketPath)) {
        fs.unlinkSync(socketPath);
      }
    }
  });

  it("IpcClient.call() with params sends payload correctly", async () => {
    const socketPath = path.join(
      process.cwd(),
      `test-protocol-params-${Date.now()}-${Math.random().toString(36).slice(2, 6)}.sock`,
    );

    let receivedPayload: unknown = null;
    const server = net.createServer((socket) => {
      let buf = "";
      socket.on("data", (chunk) => {
        buf += chunk.toString("utf-8");
        const idx = buf.indexOf("\n");
        if (idx !== -1) {
          const line = buf.slice(0, idx).trim();
          const req = JSON.parse(line);
          receivedPayload = req.params;
          const response = {
            jsonrpc: "2.0",
            id: req.id,
            result: { hash: "abc1234", message: req.params.message },
          };
          socket.write(JSON.stringify(response) + "\n");
        }
      });
    });

    await new Promise<void>((resolve) => server.listen(socketPath, resolve));

    try {
      const client = new IpcClient({ requestTimeoutMs: 5000 });
      await client.connect(socketPath);

      const result = await client.call("gitCommit", { message: "feat: types" });

      assert.strictEqual(result.hash, "abc1234");
      assert.strictEqual(result.message, "feat: types");
      assert.deepStrictEqual(receivedPayload, { message: "feat: types" });

      await client.disconnect();
    } finally {
      server.close();
      if (fs.existsSync(socketPath)) {
        fs.unlinkSync(socketPath);
      }
    }
  });
});
