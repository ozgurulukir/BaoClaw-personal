import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import { describe, test } from "node:test";
import { attachControlChannel, type ControlChannel } from "./controlChannel.js";
import { IpcClient } from "./client.js";

interface FakeDaemon {
  socketPath: string;
  /** Methods received per connection, in arrival order. */
  receivedPerConn: string[][];
  /** When set, the connection at this index gets an initialize error. */
  failInitializeOnConn?: number;
  close: () => Promise<void>;
}

async function withFakeDaemon(
  opts: { failInitializeOnConn?: number } = {},
  run: (daemon: FakeDaemon) => Promise<void>,
): Promise<void> {
  const directory = await mkdtemp(path.join(os.tmpdir(), "baoclaw-ctl-"));
  const socketPath = path.join(directory, "daemon.sock");
  const daemon: FakeDaemon = {
    socketPath,
    receivedPerConn: [],
    failInitializeOnConn: opts.failInitializeOnConn,
    close: async () => {
      await new Promise<void>((resolve) => server.close(() => resolve()));
      await rm(directory, { recursive: true, force: true });
    },
  };

  let connIdx = 0;
  const server = net.createServer((socket) => {
    const id = connIdx++;
    daemon.receivedPerConn[id] = [];
    let buffer = "";
    socket.on("data", (data) => {
      buffer += data.toString();
      let newline: number;
      while ((newline = buffer.indexOf("\n")) !== -1) {
        const line = buffer.slice(0, newline);
        buffer = buffer.slice(newline + 1);
        if (!line) continue;
        const request = JSON.parse(line);
        daemon.receivedPerConn[id].push(request.method);
        if (request.id == null) continue;
        const failed =
          request.method === "initialize" && daemon.failInitializeOnConn === id;
        socket.write(
          JSON.stringify(
            failed
              ? { jsonrpc: "2.0", id: request.id, error: { message: "no" } }
              : { jsonrpc: "2.0", id: request.id, result: { ok: true } },
          ) + "\n",
        );
      }
    });
  });
  await new Promise<void>((resolve) => server.listen(socketPath, resolve));
  try {
    await run(daemon);
  } finally {
    await daemon.close();
  }
}

describe("attachControlChannel", () => {
  test("delivers control RPCs on a second connection sharing the session", async () => {
    await withFakeDaemon({}, async (daemon) => {
      const main = new IpcClient({ requestTimeoutMs: 0 });
      await main.connect(daemon.socketPath);
      const initParams = { cwd: "/p", settings: {}, shared_session_id: "t" };
      await main.request("initialize", initParams);

      const control: ControlChannel = await attachControlChannel({
        socketPath: daemon.socketPath,
        initParams,
        fallbackClient: main,
      });

      await control.request("abort");
      // Both connections joined the same session: identical initialize.
      assert.deepEqual(daemon.receivedPerConn[0], ["initialize"]);
      assert.deepEqual(daemon.receivedPerConn[1], ["initialize", "abort"]);

      await control.close();
      await main.disconnect();
    });
  });

  test("degrades to the fallback client when initialize fails", async () => {
    await withFakeDaemon({ failInitializeOnConn: 1 }, async (daemon) => {
      const main = new IpcClient({ requestTimeoutMs: 0 });
      await main.connect(daemon.socketPath);
      const initParams = { cwd: "/p", settings: {}, shared_session_id: "t" };
      await main.request("initialize", initParams);

      const control = await attachControlChannel({
        socketPath: daemon.socketPath,
        initParams,
        fallbackClient: main,
      });

      await control.request("abort");
      // conn1 only saw the failed initialize; the abort degraded to conn0.
      assert.deepEqual(daemon.receivedPerConn[0], ["initialize", "abort"]);

      await control.close();
      await main.disconnect();
    });
  });

  test("setup failure at connect time still routes via the fallback", async () => {
    const directory = await mkdtemp(path.join(os.tmpdir(), "baoclaw-ctl-"));
    const deadPath = path.join(directory, "missing.sock");
    try {
      const main = new IpcClient({ requestTimeoutMs: 0 });
      await assert.rejects(main.connect(deadPath));
      const control = await attachControlChannel({
        socketPath: deadPath,
        initParams: { cwd: "/p", shared_session_id: "t" },
        fallbackClient: main,
      });
      await assert.rejects(
        control.request("abort"),
        /Not connected|ENOENT|Connection/,
      );
      await control.close();
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });
});
