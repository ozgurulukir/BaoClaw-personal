import { strict as assert } from "node:assert";
import { mkdtempSync, rmSync } from "node:fs";
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";
import { DaemonConnector, type DaemonInfo } from "./daemon.js";

test("DaemonConnector uses its configured session tag by default", async () => {
  const tempDir = mkdtempSync(path.join(os.tmpdir(), "baoclaw-daemon-test-"));
  const socketPath = path.join(tempDir, "daemon.sock");
  let initializeParams: Record<string, unknown> | undefined;
  const server = net.createServer((socket) => {
    let buffer = "";
    socket.on("data", (chunk) => {
      buffer += chunk.toString();
      const newline = buffer.indexOf("\n");
      if (newline === -1) return;
      const request = JSON.parse(buffer.slice(0, newline));
      initializeParams = request.params;
      socket.write(
        JSON.stringify({
          jsonrpc: "2.0",
          id: request.id,
          result: { session_id: "telegram" },
        }) + "\n",
      );
    });
  });

  try {
    await new Promise<void>((resolve) => server.listen(socketPath, resolve));
    const connector = new DaemonConnector({ sessionTag: "telegram" });
    const info: DaemonInfo = {
      pid: process.pid,
      cwd: process.cwd(),
      session_id: "telegram",
      socket: socketPath,
      started_at: new Date().toISOString(),
    };
    const client = await connector.connect(info);

    assert.equal(initializeParams?.shared_session_id, "telegram");
    assert.equal(connector.lastConnectAt instanceof Date, true);
    await client.disconnect();
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
    rmSync(tempDir, { recursive: true, force: true });
  }
});
