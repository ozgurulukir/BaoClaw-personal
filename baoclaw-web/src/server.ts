/**
 * BaoClaw Web Gateway — HTTP + WebSocket server that bridges to the daemon via UDS IPC.
 * Usage: cd /your/project && baoclaw-web [--port 8080]
 */
import * as http from "http";
import * as fs from "fs";
import * as path from "path";
import * as os from "os";
import * as crypto from "crypto";
import { WebSocketServer, WebSocket } from "ws";
import { isOriginAllowed } from "./origin.js";
import {
  IpcClient,
  type DaemonInfo,
  discoverLegacyDaemons,
  resolveFixedSocket,
  selectNewestDaemon,
} from "../../ts-ipc/index.js";

function loadExpectedToken(): string {
  if (process.env.BAOCLAW_WEB_TOKEN) {
    return process.env.BAOCLAW_WEB_TOKEN;
  }
  const configPath = path.join(
    process.env.BAOCLAW_HOME || path.join(os.homedir(), ".baoclaw"),
    "config.json",
  );
  if (fs.existsSync(configPath)) {
    try {
      const cfg = JSON.parse(fs.readFileSync(configPath, "utf-8"));
      const token = cfg.web?.token || cfg.extra?.web?.token;
      if (typeof token === "string" && token.length > 0) {
        return token;
      }
    } catch {}
  }
  return crypto.randomBytes(16).toString("hex");
}

function isValidToken(
  providedToken: string | null | undefined,
  expectedToken: string,
): boolean {
  if (!providedToken) return false;
  const providedBuf = Buffer.from(providedToken, "utf-8");
  const expectedBuf = Buffer.from(expectedToken, "utf-8");
  if (providedBuf.length !== expectedBuf.length) return false;
  return crypto.timingSafeEqual(providedBuf, expectedBuf);
}

// ═══════════════════════════════════════════════════════════════
// Daemon discovery
// ═══════════════════════════════════════════════════════════════
// IpcClient, DaemonInfo, getSocketDir and resolveFixedSocket come from the
// shared ts-ipc package (single source of truth for JSON-RPC/UDS).

/**
 * Discover running BaoClaw daemons: prefer the fixed socket (P3-1c) with its
 * sibling meta file, then fall back to the shared legacy-directory scan.
 * Unlike ts-ipc's DaemonConnector, this reads the fixed socket's meta file so
 * the gateway can display the daemon's real pid/cwd.
 */
function discoverDaemons(): DaemonInfo[] {
  const fixed = resolveFixedSocket();
  if (fixed && fs.existsSync(fixed)) {
    const metaPath = fixed.replace(/\.sock$/, ".json");
    try {
      const meta: DaemonInfo = JSON.parse(fs.readFileSync(metaPath, "utf-8"));
      try {
        process.kill(meta.pid, 0);
      } catch {
        /* fall through to synthesized */
      }
      if (!fs.existsSync(meta.socket)) throw new Error("stale meta");
      return [meta];
    } catch {
      // No/stale meta file — the socket itself is the source of truth.
      // pid -1 marks the entry as synthesized (display only).
      return [
        { pid: -1, cwd: "", session_id: "", socket: fixed, started_at: "" },
      ];
    }
  }
  return discoverLegacyDaemons();
}

// ═══════════════════════════════════════════════════════════════
// Static file server
// ═══════════════════════════════════════════════════════════════
const MIME: Record<string, string> = {
  ".html": "text/html",
  ".css": "text/css",
  ".js": "application/javascript",
  ".json": "application/json",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".ico": "image/x-icon",
  ".txt": "text/plain",
  ".woff": "font/woff",
  ".woff2": "font/woff2",
};

function getPublicDir(): string {
  // Try multiple strategies to find the public directory
  // Strategy 1: relative to the script being executed (process.argv[1])
  const scriptPath = process.argv[1];
  if (scriptPath) {
    const candidate = path.join(path.dirname(scriptPath), "..", "public");
    if (fs.existsSync(path.join(candidate, "index.html")))
      return path.resolve(candidate);
  }
  // Strategy 2: import.meta.url
  try {
    const thisFile = decodeURIComponent(new URL(import.meta.url).pathname);
    const candidate = path.join(path.dirname(thisFile), "..", "public");
    if (fs.existsSync(path.join(candidate, "index.html")))
      return path.resolve(candidate);
  } catch {}
  // Strategy 3: __dirname equivalent via cwd
  const candidate = path.join(process.cwd(), "public");
  if (fs.existsSync(path.join(candidate, "index.html")))
    return path.resolve(candidate);
  // Fallback
  console.error("Cannot find public directory!");
  return path.resolve("public");
}

const PUBLIC_DIR = getPublicDir();

async function serveStatic(res: http.ServerResponse, urlPath: string) {
  const filePath = path.join(
    PUBLIC_DIR,
    urlPath === "/" ? "index.html" : urlPath,
  );
  const resolved = path.resolve(filePath);
  // Boundary check with an explicit separator so sibling directories such as
  // "public-evil" cannot pass a bare startsWith(PUBLIC_DIR) prefix test.
  if (resolved !== PUBLIC_DIR && !resolved.startsWith(PUBLIC_DIR + path.sep)) {
    res.writeHead(403);
    res.end("Forbidden");
    return;
  }
  try {
    const data = await fs.promises.readFile(resolved);
    const ext = path.extname(resolved);
    res.writeHead(200, {
      "Content-Type": MIME[ext] || "application/octet-stream",
    });
    res.end(data);
  } catch {
    console.error(`404: ${resolved}`);
    res.writeHead(404);
    res.end("Not Found");
  }
}

// ═══════════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════════
async function main() {
  const args = process.argv.slice(2);
  const hostIdx =
    args.indexOf("--host") !== -1
      ? args.indexOf("--host")
      : args.indexOf("--bind");
  const host =
    hostIdx >= 0 && args[hostIdx + 1]
      ? args[hostIdx + 1]
      : (process.env.BAOCLAW_WEB_HOST ?? "127.0.0.1");
  const portIdx = args.indexOf("--port");
  const port =
    portIdx >= 0 && args[portIdx + 1] ? parseInt(args[portIdx + 1], 10) : 8080;
  const cwd = process.cwd();
  const expectedToken = loadExpectedToken();

  // Find daemon
  const daemons = discoverDaemons();
  const daemon = selectNewestDaemon(daemons);
  if (!daemon) {
    console.error("No BaoClaw daemon found. Start one first with: baoclaw");
    process.exit(1);
  }
  console.log(`Found daemon pid=${daemon.pid} cwd=${daemon.cwd}`);

  // HTTP server
  const server = http.createServer((req, res) => {
    if (req.method !== "GET") {
      res.writeHead(405);
      res.end();
      return;
    }
    // Strip the query string and decode percent-escapes before resolving the
    // file path: the printed entry URL is "/?token=...", which must serve
    // index.html, and "/..%2F.." style escapes must hit the boundary check.
    let pathname: string;
    try {
      pathname = decodeURIComponent(
        new URL(req.url || "/", "http://localhost").pathname,
      );
    } catch {
      res.writeHead(400);
      res.end("Bad Request");
      return;
    }
    void serveStatic(res, pathname);
  });

  // Surface listen failures (e.g. port already in use) instead of letting the
  // unhandled 'error' event crash with a raw stack trace.
  server.on("error", (err: Error) => {
    console.error(`Failed to start BaoClaw Web gateway: ${err.message}`);
    process.exit(1);
  });

  // WebSocket server
  const wss = new WebSocketServer({ noServer: true });

  server.on("upgrade", (req, socket, head) => {
    if (
      !isOriginAllowed(
        req.headers.origin as string | undefined,
        req.headers.host,
      )
    ) {
      console.warn(
        `[web-auth] 403 Forbidden WebSocket upgrade from ${req.socket.remoteAddress} (origin: ${req.headers.origin})`,
      );
      socket.write(
        "HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nForbidden: Cross-origin WebSocket connections are not allowed.\n",
      );
      socket.destroy();
      return;
    }

    const reqUrl = new URL(
      req.url || "/",
      `http://${req.headers.host || "127.0.0.1"}`,
    );
    let token = reqUrl.searchParams.get("token");
    if (!token && req.headers.authorization) {
      const auth = req.headers.authorization;
      if (auth.startsWith("Bearer ")) {
        token = auth.slice(7).trim();
      }
    }
    if (!token && req.headers["x-baoclaw-token"]) {
      token = req.headers["x-baoclaw-token"] as string;
    }
    if (!token && req.headers["sec-websocket-protocol"]) {
      token = req.headers["sec-websocket-protocol"] as string;
    }

    if (!isValidToken(token, expectedToken)) {
      console.warn(
        `[web-auth] 401 Unauthorized WebSocket upgrade attempt from ${req.socket.remoteAddress}`,
      );
      socket.write(
        "HTTP/1.1 401 Unauthorized\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nUnauthorized: Missing or invalid authentication token.\n",
      );
      socket.destroy();
      return;
    }

    console.log(`HTTP upgrade request (authenticated): ${req.url}`);
    wss.handleUpgrade(req, socket, head, (ws) => {
      wss.emit("connection", ws, req);
    });
  });

  wss.on("connection", async (ws: WebSocket, req: http.IncomingMessage) => {
    // Extract cwd from query parameter: ws://host/?cwd=/path/to/project
    const reqUrl = new URL(req.url || "/", `http://${req.headers.host}`);
    const wsCwd = reqUrl.searchParams.get("cwd") || cwd;
    console.log(`WebSocket client connected (cwd: ${wsCwd})`);

    // Each WS connection gets its own IPC client to the daemon
    const ipc = new IpcClient();
    try {
      console.log(`Connecting to daemon socket: ${daemon.socket}`);
      await ipc.connect(daemon.socket);
      console.log("IPC connected, sending initialize...");
      const initResult = await ipc.request("initialize", {
        cwd: wsCwd,
        settings: {},
        shared_session_id: "web",
      });
      console.log("Initialize done, sending to browser");
      // Send init info to browser
      ws.send(
        JSON.stringify({
          type: "init",
          data: initResult,
          cwd: wsCwd,
          daemon: { pid: daemon.pid },
        }),
      );
    } catch (err: any) {
      console.error("IPC init failed:", err.message);
      ws.send(
        JSON.stringify({
          type: "error",
          message: `Failed to connect to daemon: ${err.message}`,
        }),
      );
      ws.close();
      return;
    }

    // Forward daemon stream events to browser
    ipc.onNotification("stream/event", (params) => {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "stream", data: params }));
      }
    });

    ipc.onDisconnect((err) => {
      if (ws.readyState === WebSocket.OPEN) {
        console.error(`[web] daemon IPC disconnected: ${err.message}`);
        ws.send(
          JSON.stringify({
            type: "error",
            message: `Daemon disconnected: ${err.message}`,
          }),
        );
        ws.close();
      }
    });

    // Handle messages from browser
    ws.on("message", async (raw: Buffer) => {
      let msg: { action: string; [k: string]: unknown };
      try {
        msg = JSON.parse(raw.toString());
      } catch {
        return;
      }

      try {
        switch (msg.action) {
          case "submit": {
            const submitParams: Record<string, unknown> = {
              prompt: msg.prompt,
            };
            if (msg.attachments) submitParams.attachments = msg.attachments;
            // A submit spans a whole agent turn — no timeout (0 disables).
            const result = await ipc.request("submitMessage", submitParams, 0);
            ws.send(JSON.stringify({ type: "submitDone", data: result }));
            break;
          }
          case "abort": {
            // The daemon's RPC loop is serial per connection: an abort sent
            // mid-turn is only read (and answered) once the turn drains, so
            // a client timeout here would always fire on long turns.
            await ipc.request("abort", undefined, 0);
            ws.send(JSON.stringify({ type: "abortDone" }));
            break;
          }
          case "compact": {
            // Compaction summarizes the whole context with an LLM call —
            // no timeout (0 disables).
            const result = await ipc.request("compact", undefined, 0);
            ws.send(JSON.stringify({ type: "compactDone", data: result }));
            break;
          }
          case "rpc": {
            // Generic RPC passthrough: { action: 'rpc', method: '...', params: {...} }
            if (typeof msg.method !== "string" || msg.method.length === 0) {
              ws.send(
                JSON.stringify({
                  type: "error",
                  message: "Missing 'method' for rpc action",
                }),
              );
              break;
            }
            // Passthrough target is daemon-defined; duration unknown → no timeout.
            const result = await ipc.request(msg.method, msg.params, 0);
            ws.send(
              JSON.stringify({
                type: "rpcResult",
                method: msg.method,
                data: result,
              }),
            );
            break;
          }
          case "permission": {
            // Decisions arrive mid-turn and are read only when the daemon's
            // serial RPC loop yields; its own 300s permission gate bounds the
            // wait, so no client-side timeout.
            await ipc.request(
              "permissionResponse",
              {
                tool_use_id: msg.tool_use_id,
                decision: msg.decision,
                rule: msg.rule,
              },
              0,
            );
            break;
          }
          default:
            ws.send(
              JSON.stringify({
                type: "error",
                message: `Unknown action: ${msg.action}`,
              }),
            );
        }
      } catch (err: any) {
        ws.send(JSON.stringify({ type: "error", message: err.message }));
      }
    });

    ws.on("close", () => {
      console.log("WebSocket client disconnected");
      ipc.disconnect();
    });
  });

  server.listen(port, host, () => {
    const displayUrl = `http://${host === "0.0.0.0" ? "localhost" : host}:${port}/?token=${expectedToken}`;
    console.log(`\n🐾 BaoClaw Web running at ${displayUrl}`);
    console.log(
      `   Host: ${host} (localhost-only default; use --host 0.0.0.0 for LAN access)`,
    );
    console.log(`   CWD: ${cwd}`);
    console.log(`   Auth Token: ${expectedToken}`);
    console.log(`   Daemon: pid=${daemon.pid}`);
    console.log(`   Public: ${PUBLIC_DIR}\n`);
  });
}

main().catch((err) => {
  console.error("Fatal:", err);
  process.exit(1);
});
