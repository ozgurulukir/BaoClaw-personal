#!/usr/bin/env tsx
/**
 * BaoClaw Feishu Gateway — bridges Feishu IM and BaoClaw daemon.
 *
 * Features:
 *   - Slash commands (/help, /status, /git, /tools, etc.)
 *   - Structured logging with levels, timestamps, and file output
 *   - PID file for daemon management
 *   - Long-connection event subscription via lark-cli
 *
 * Architecture:
 *   lark-cli event consume (NDJSON stdout) → gateway.ts → Unix Socket → daemon
 *   daemon stream/event → Unix Socket → gateway.ts → lark-cli messages-send
 *
 * IPC protocol (same as WhatsApp/Telegram gateways):
 *   Request:  submitMessage { prompt, uuid }
 *   Notify:   stream/event { type: assistant_chunk | tool_use | tool_result | result | error }
 *   Commands: direct JSON-RPC to daemon (compact, listTools, etc.)
 */

import { spawn, ChildProcess } from "child_process";
import * as fs from "fs";
import * as path from "path";
import * as readline from "readline";
import { randomUUID } from "crypto";
import { createDaemonConnector, type DaemonInfo } from "./daemon.js";
import {
  IpcClient,
  attachControlChannel,
  buildDaemonInitParams,
  type ControlChannel,
} from "baoclaw-ipc";
import { securePrivateFile } from "baoclaw-ipc/security";
import { isAllowedChat } from "./authorization.js";
import { logger, setLogLevel, setLogFile } from "./log.js";
import {
  parseCommand,
  isRegisteredCommand,
  dispatchCommand,
  COMMAND_REGISTRY,
  setDaemonInfo,
  setDaemonMetrics,
  setGatewayInfo,
  formatHelp,
} from "./commands.js";
import { formatForFeishu, splitMessage } from "./formatter.js";
import {
  PermissionManager,
  buildPermissionCard,
  formatPermissionRequest,
  parseCardAction,
  type PermissionDecision,
} from "./permission.js";

// ── Types ──────────────────────────────────────────────────────────────────

interface FeishuEvent {
  chat_id: string;
  chat_type: "p2p" | "group";
  content: string;
  sender_id: string;
  message_id: string;
  message_type: string;
  event_id: string;
  create_time: string;
  timestamp: string;
  type: string;
}

interface StreamEvent {
  type: string;
  content?: string;
  tool_name?: string;
  tool_use_id?: string;
  input?: unknown;
  is_error?: boolean;
  output?: unknown;
  message?: string;
}

// ── Config ─────────────────────────────────────────────────────────────────

// This deployment's bot identity, used to ignore message echoes. Override via
// env when pointing the gateway at a different Feishu app.
const BOT_OPEN_ID =
  process.env.BAOCLAW_FEISHU_BOT_OPEN_ID ??
  "ou_0c3d070e43739551854de5a3b546e821";
const MAX_MSG_LEN = 15000;
/** Characters of auxiliary text (stderr, JSON payload) kept in messages. */
const PREVIEW_CHARS = 200;
/** Characters of tool output kept in error notifications. */
const TOOL_OUTPUT_PREVIEW_CHARS = 500;
const PID_FILE = path.join(process.env.HOME || "/tmp", ".baoclaw-feishu.pid");
const LOG_DIR = path.join(process.env.HOME || "/tmp", ".baoclaw", "logs");

function loadAllowedChatIds(): string[] {
  try {
    const configPath = path.join(
      process.env.HOME || "/tmp",
      ".baoclaw",
      "config.json",
    );
    securePrivateFile(configPath);
    const raw = JSON.parse(fs.readFileSync(configPath, "utf-8"));
    return Array.isArray(raw?.feishu?.allowedChatIds)
      ? raw.feishu.allowedChatIds.filter(
          (id: unknown): id is string =>
            typeof id === "string" && id.length > 0,
        )
      : [];
  } catch {
    return [];
  }
}

// Parse CLI flags
const args = process.argv.slice(2);
const FLAGS = {
  debug: args.includes("--debug") || args.includes("-d"),
  daemon: args.includes("--daemon"),
  help: args.includes("--help") || args.includes("-h"),
};

// ── PID file management ────────────────────────────────────────────────────

function writePidFile(): void {
  try {
    fs.mkdirSync(path.dirname(PID_FILE), { recursive: true });
    fs.writeFileSync(PID_FILE, String(process.pid));
    logger.info(`PID file written: ${PID_FILE}`);
  } catch (e: any) {
    logger.error(`Failed to write PID file: ${e.message}`);
  }
}

function removePidFile(): void {
  try {
    if (fs.existsSync(PID_FILE)) {
      fs.unlinkSync(PID_FILE);
      logger.info(`PID file removed: ${PID_FILE}`);
    }
  } catch {}
}

// ── Subprocess: lark-cli message sender ───────────────────────────────────

function sendFeishuMessage(
  chatId: string,
  text: string,
  useMarkdown: boolean = false,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const args = ["im", "+messages-send", "--as", "bot", "--chat-id", chatId];
    if (useMarkdown) {
      args.push("--markdown", text);
    } else {
      args.push("--text", text);
    }
    const proc = spawn("lark-cli", args, { stdio: ["ignore", "pipe", "pipe"] });
    let stderr = "";
    proc.stderr.on("data", (d: Buffer) => {
      stderr += d.toString();
    });
    proc.on("close", (code) => {
      if (code === 0) resolve();
      else
        reject(
          new Error(
            `messages-send exited ${code}: ${stderr.slice(0, PREVIEW_CHARS)}`,
          ),
        );
    });
    proc.on("error", reject);
  });
}

/** Send an interactive card (permission prompts) as raw card JSON. */
function sendFeishuCard(
  chatId: string,
  card: Record<string, unknown>,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const args = [
      "im",
      "+messages-send",
      "--as",
      "bot",
      "--chat-id",
      chatId,
      "--msg-type",
      "interactive",
      "--content",
      JSON.stringify(card),
    ];
    const proc = spawn("lark-cli", args, { stdio: ["ignore", "pipe", "pipe"] });
    let stderr = "";
    proc.stderr.on("data", (d: Buffer) => {
      stderr += d.toString();
    });
    proc.on("close", (code) => {
      if (code === 0) resolve();
      else
        reject(
          new Error(
            `card send exited ${code}: ${stderr.slice(0, PREVIEW_CHARS)}`,
          ),
        );
    });
    proc.on("error", reject);
  });
}

/**
 * Verify the installed lark-cli supports consuming card.action.trigger
 * events before wiring buttons into prompts. On an unsupported CLI the
 * gateway runs text-only (reply keywords), never sending cards whose
 * clicks it could not receive.
 */
function probeCardActionSupport(): Promise<boolean> {
  return new Promise((resolve) => {
    const proc = spawn(
      "lark-cli",
      ["event", "consume", "card.action.trigger", "--as", "bot", "--dry-run"],
      { stdio: ["ignore", "ignore", "pipe"] },
    );
    const timer = setTimeout(() => {
      proc.kill("SIGKILL");
      resolve(false);
    }, 15_000);
    timer.unref();
    proc.on("close", (code) => {
      clearTimeout(timer);
      resolve(code === 0);
    });
    proc.on("error", () => {
      clearTimeout(timer);
      resolve(false);
    });
  });
}

/** Spawn the card.action.trigger event consumer (NDJSON stdout). */
function startCardActionConsumer(): ChildProcess {
  return spawn(
    "lark-cli",
    ["event", "consume", "card.action.trigger", "--as", "bot"],
    { stdio: ["pipe", "pipe", "pipe"] },
  );
}

// ── Message splitting for Feishu length limit ──────────────────────────────

async function sendReply(chatId: string, text: string): Promise<void> {
  const formatted = formatForFeishu(text);
  const chunks = splitMessage(formatted);
  for (const chunk of chunks) {
    await sendFeishuMessage(chatId, chunk, true); // use --markdown
  }
}

/**
 * Send a plain-text notification (for tool/error messages, not AI output).
 */
async function sendPlainReply(chatId: string, text: string): Promise<void> {
  const chunks = splitMessage(text);
  for (const chunk of chunks) {
    await sendFeishuMessage(chatId, chunk, false); // use --text
  }
}

// ── Subprocess: lark-cli event consumer ───────────────────────────────────

function startEventConsumer(): ChildProcess {
  const proc = spawn(
    "lark-cli",
    ["event", "consume", "im.message.receive_v1", "--as", "bot"],
    { stdio: ["pipe", "pipe", "pipe"] },
  );
  proc.stdin!.on("error", () => {});
  proc.on("error", (err) => {
    logger.error(`lark-cli event consume spawn error: ${err.message}`);
    process.exit(1);
  });
  return proc;
}

// ── Daemon IPC bridge ──────────────────────────────────────────────────────

class DaemonBridge {
  private client: IpcClient | null = null;
  private info: DaemonInfo | null = null;
  private connector = createDaemonConnector();
  private control: ControlChannel | null = null;

  // Per-chat interaction state
  private accumulators = new Map<string, string>();
  private activeChat: string | null = null;
  private interactionResolve!: () => void;
  private interactionError: Error | null = null;
  private processing = false;
  private permissionManager = new PermissionManager();

  async connect(): Promise<void> {
    const sharedSessionId = "feishu";
    const { client, info } = await this.connector.discoverAndConnect(
      30_000,
      3_000,
      sharedSessionId,
    );
    this.client = client;
    this.info = info;
    // Abort must not wait behind an in-flight turn on the serial main
    // connection — deliver it via the dedicated control channel, joining
    // the same session the main connection just initialized.
    this.control = await attachControlChannel({
      socketPath: info.socket,
      initParams: buildDaemonInitParams(info, sharedSessionId),
      fallbackClient: client,
    });
    setDaemonInfo({
      pid: info.pid,
      session_id: info.session_id,
      cwd: info.cwd,
    });
    setGatewayInfo({
      pid: process.pid,
      startTime: Date.now(),
      logFile: path.join(LOG_DIR, "baoclaw-feishu.log"),
      name: "Feishu",
    });
    logger.info(
      `Connected to daemon (pid=${info.pid}, session=${info.session_id}, cwd=${info.cwd})`,
    );

    client.onNotification("stream/event", (params: unknown) => {
      this.handleStreamEvent(params as StreamEvent);
    });
  }

  private handleStreamEvent(event: StreamEvent): void {
    if (!event?.type) return;
    const chatId = this.activeChat;
    if (!chatId) return;

    switch (event.type) {
      case "assistant_chunk": {
        const acc =
          (this.accumulators.get(chatId) || "") + (event.content || "");
        this.accumulators.set(chatId, acc);
        break;
      }
      case "tool_use": {
        const tn = event.tool_name || "?";
        logger.info(`Tool use: ${tn}`);
        sendFeishuMessage(chatId, `🔧 Using tool: ${tn}`).catch(() => {});
        break;
      }
      case "permission_request": {
        const preview = JSON.stringify(event.input ?? {}).slice(
          0,
          PREVIEW_CHARS,
        );
        const toolName = event.tool_name || "unknown";
        logger.info(`Permission request: ${toolName} (${event.tool_use_id})`);
        // Mirror the daemon's exact auto-deny window for this ask (fallback
        // = the daemon default, for malformed/old-daemon events).
        const timeoutSecs = Math.max(
          1,
          (event as { ask_timeout_secs?: number }).ask_timeout_secs ?? 300,
        );
        // Card-first (rich buttons); an older lark-cli that rejects
        // interactive content degrades to the plain-text prompt with the
        // same reply keywords.
        const promptSend: Promise<void> = cardActionSupport
          ? sendFeishuCard(
              chatId,
              buildPermissionCard(toolName, preview, timeoutSecs),
            ).catch(() =>
              sendFeishuMessage(
                chatId,
                formatPermissionRequest(toolName, preview, timeoutSecs),
              ),
            )
          : sendFeishuMessage(
              chatId,
              formatPermissionRequest(toolName, preview, timeoutSecs),
            );
        promptSend.catch(() => {});
        this.permissionManager.registerRequest(
          chatId,
          event.tool_use_id || "",
          toolName,
          (cid, toolUseId, reason) => {
            // Expiry/supersede must deny with the daemon so the parked turn
            // resumes. The daemon's own auto-deny always fires first (its
            // timer starts before ours), so `delivered` is usually false
            // here — notify the user for a real expiry regardless.
            this.control
              ?.request("permissionResponse", {
                tool_use_id: toolUseId,
                decision: "deny",
              })
              .catch(() => {});
            if (reason === "timeout") {
              sendFeishuMessage(
                cid,
                "⏰ Permission request timed out, auto-denied.",
              ).catch(() => {});
            }
          },
          timeoutSecs * 1000,
        );
        break;
      }
      case "tool_result": {
        if (event.is_error) {
          const out =
            typeof event.output === "string"
              ? event.output
              : JSON.stringify(event.output);
          sendFeishuMessage(
            chatId,
            `⚠️ Tool error: ${out.slice(0, TOOL_OUTPUT_PREVIEW_CHARS)}`,
          ).catch(() => {});
        }
        break;
      }
      case "result": {
        logger.info(`Interaction completed for chat=${chatId}`);
        this.processing = false;
        this.interactionResolve();
        break;
      }
      case "error": {
        logger.error(`Interaction error: ${event.message || "unknown"}`);
        this.interactionError = new Error(event.message || "Unknown error");
        this.processing = false;
        this.interactionResolve();
        break;
      }
    }
  }

  /** Submit message to daemon and wait for stream to complete */
  async submitMessage(chatId: string, text: string): Promise<string> {
    if (!this.client) throw new Error("Not connected to daemon");
    if (this.processing)
      throw new Error("Daemon busy — another request in flight");

    this.processing = true;
    this.activeChat = chatId;
    this.accumulators.set(chatId, "");

    const done = new Promise<void>((resolve) => {
      this.interactionResolve = resolve;
    });

    await this.client.request("submitMessage", {
      prompt: text,
      uuid: randomUUID(),
    });
    await done;

    if (this.interactionError) {
      const err = this.interactionError;
      this.interactionError = null;
      throw err;
    }
    return this.accumulators.get(chatId) || "";
  }

  /** Send arbitrary JSON-RPC to daemon */
  async rpc<T = unknown>(
    method: string,
    params?: Record<string, unknown>,
  ): Promise<T> {
    if (!this.client) throw new Error("Not connected to daemon");
    return this.client.request<T>(method, params || {});
  }

  get daemonInfo(): DaemonInfo | null {
    return this.info;
  }
  get ipcClient(): IpcClient | null {
    return this.client;
  }
  get controlChannel(): ControlChannel | null {
    return this.control;
  }
  getReconnectCount(): number {
    return this.connector.reconnectCount;
  }
  getLastConnectAt(): Date | null {
    return this.connector.lastConnectAt;
  }
  get isProcessing(): boolean {
    return this.processing;
  }

  /** Public access for the standalone handleMessage interceptor. */
  get permissions(): PermissionManager {
    return this.permissionManager;
  }

  isCommandBusy(): boolean {
    // Some commands can run even while AI is processing
    return false;
  }
}

const bridge = new DaemonBridge();

/**
 * Whether the installed lark-cli can consume card.action.trigger events
 * (probed at startup). When false the gateway stays text-only: prompts are
 * sent as plain text so no buttons exist that we could never receive.
 */
let cardActionSupport = false;

/**
 * Handle a parsed card.action.trigger: resolve the chat's pending permission
 * request and acknowledge with a follow-up message. Stale clicks (already
 * timed out or answered elsewhere) get an "expired" ack instead.
 */
/** Acknowledgement text for a resolved permission request. */
function ackFor(decision: PermissionDecision): string {
  return decision === "allow"
    ? "✅ Allowed."
    : decision === "allow_always"
      ? "🔁 Allowed and remembered for this tool."
      : "❌ Denied.";
}

async function handleCardAction(
  chatId: string,
  decision: PermissionDecision,
): Promise<void> {
  const reply = await bridge.permissions.resolvePending(
    chatId,
    decision,
    bridge.controlChannel!,
  );
  if (!reply) {
    // Clicked a card whose prompt is no longer pending (e.g. superseded).
    await sendFeishuMessage(chatId, "⚠️ This request no longer exists.").catch(
      () => {},
    );
    return;
  }
  if (!reply.delivered) {
    await sendFeishuMessage(chatId, "⚠️ This request has expired.").catch(
      () => {},
    );
    return;
  }
  await sendFeishuMessage(chatId, ackFor(reply.decision)).catch(() => {});
}

// ── Message handling ───────────────────────────────────────────────────────

/**
 * Handle an incoming Feishu message event.
 *   - Slash commands → dispatch locally via commands.ts
 *   - Regular text → forward to daemon via submitMessage
 */
async function handleMessage(event: FeishuEvent): Promise<void> {
  const chatId = event.chat_id;
  const sender = event.sender_id;
  const text = event.content.trim();
  const chatLabel = event.chat_type === "p2p" ? "DM" : `group(${chatId})`;

  logger.info(`📩 message received in ${chatLabel} from ${sender}`);

  // ── Permission reply check (before commands and the busy bounce) ──
  // A decision necessarily arrives while the turn is parked on the gate, so
  // this must precede both parseCommand and the isProcessing rejection.
  if (bridge.permissions.getPending(chatId)) {
    const reply = await bridge.permissions.handleResponse(
      chatId,
      text,
      bridge.controlChannel!,
    );
    if (reply) {
      if (!reply.delivered) {
        await sendFeishuMessage(chatId, "⚠️ This request has expired.");
        return;
      }
      await sendFeishuMessage(chatId, ackFor(reply.decision));
      return;
    }
  }

  // ── Slash command detection ──
  const parsed = parseCommand(text);
  if (parsed) {
    if (isRegisteredCommand(parsed.name)) {
      logger.info(`Command: ${parsed.name} args="${parsed.args.slice(0, 40)}"`);
      try {
        const result = await dispatchCommand(parsed, {
          ipcClient: bridge.ipcClient!,
          control: bridge.controlChannel!,
          args: parsed.args,
          sender,
          chatId,
          sendReply: (r: string) => sendReply(chatId, r),
        });
        if (result) {
          await sendReply(chatId, result);
          logger.info(`📤 Command reply sent to ${chatLabel}`);
        }
      } catch (err: any) {
        logger.error(`Command dispatch error: ${err.message}`);
        await sendFeishuMessage(chatId, `❌ Command failed: ${err.message}`);
      }
      return;
    } else {
      // Unknown command — show help
      logger.warn(`Unknown command: ${parsed.name}`);
      await sendReply(
        chatId,
        `❓ Unknown command ${parsed.name}\n${formatHelp()}`,
      );
      return;
    }
  }

  // ── Regular message → AI daemon ──
  if (bridge.isProcessing) {
    logger.warn(`Rejected message from ${sender}: daemon busy`);
    await sendFeishuMessage(
      chatId,
      "⏳ Still processing the previous message, please wait…",
    );
    return;
  }

  try {
    const reply = await bridge.submitMessage(chatId, text);
    if (reply) {
      await sendReply(chatId, reply);
      logger.info(`📤 AI reply sent to ${chatLabel} (${reply.length} chars)`);
    }
  } catch (err: any) {
    logger.error(`Daemon error: ${err.message}`);
    await sendFeishuMessage(chatId, `❌ ${err.message}`);
  }
}

// ── Inactivity checker ─────────────────────────────────────────────────────

/**
 * Every 30 seconds, if daemon is idle and consumer is connected,
 * send a heartbeat-like check to ensure event stream is alive.
 */
let lastEventTime = Date.now();

// ── Main ───────────────────────────────────────────────────────────────────

async function main() {
  const allowedChatIds = loadAllowedChatIds();
  if (allowedChatIds.length === 0) {
    throw new Error(
      "Cannot start because no chat allowlist is configured. To fix, set feishu.allowedChatIds in config.json.",
    );
  }
  // ── Setup logging ──
  if (FLAGS.debug) setLogLevel("DEBUG");
  try {
    fs.mkdirSync(LOG_DIR, { recursive: true });
    setLogFile(path.join(LOG_DIR, "baoclaw-feishu.log"));
  } catch {}

  logger.info("═══════════════════════════════════════");
  logger.info("BaoClaw Feishu Gateway starting...");
  logger.info(`PID: ${process.pid}`);
  logger.info(`Log level: ${FLAGS.debug ? "DEBUG" : "INFO"}`);

  // ── PID file ──
  writePidFile();

  // ── Connect to daemon ──
  await bridge.connect();
  logger.info(`Daemon ready (session: ${bridge.daemonInfo?.session_id})`);
  setDaemonMetrics({
    reconnectCount: bridge.getReconnectCount(),
    lastConnectAt: bridge.getLastConnectAt(),
  });
  logger.info(
    `Registered ${Object.keys(COMMAND_REGISTRY).length} slash commands`,
  );

  // ── Start event consumer ──
  const consumer = startEventConsumer();
  const rl = readline.createInterface({
    input: consumer.stdout!,
    crlfDelay: Infinity,
  });

  consumer.stderr!.on("data", (d: Buffer) => {
    const msg = d.toString().trim();
    if (msg && !msg.includes("[bus]")) {
      logger.debug(`[lark-cli] ${msg}`);
    }
    // Extract meaningful status messages
    if (msg.includes("connected")) logger.info("Feishu WebSocket connected");
    if (msg.includes("disconnect"))
      logger.warn("Feishu WebSocket disconnected");
  });

  consumer.on("close", (code) => {
    logger.warn(`Event consumer exited (code=${code}), shutting down...`);
    cleanup();
  });

  // ── Card action consumer (interactive permission buttons) ──
  let cardConsumer: ChildProcess | null = null;
  cardActionSupport = await probeCardActionSupport();
  if (cardActionSupport) {
    cardConsumer = startCardActionConsumer();
    const cardRl = readline.createInterface({
      input: cardConsumer.stdout!,
      crlfDelay: Infinity,
    });
    cardRl.on("line", (line: string) => {
      let raw: unknown;
      try {
        raw = JSON.parse(line.trim());
      } catch {
        return;
      }
      const action = parseCardAction(raw);
      if (!action) {
        logger.debug(`Unrecognized card action event: ${line.slice(0, 300)}`);
        return;
      }
      if (!isAllowedChat(action.chatId, allowedChatIds)) {
        logger.warn(
          `Rejected card action from unallowlisted chat ${action.chatId}`,
        );
        return;
      }
      logger.info(`Card action: ${action.decision} in chat ${action.chatId}`);
      handleCardAction(action.chatId, action.decision).catch((err) => {
        logger.error(`Card action error: ${err.message}`);
      });
    });
    cardConsumer.stderr!.on("data", (d: Buffer) => {
      const msg = d.toString().trim();
      if (msg && !msg.includes("[bus]")) {
        logger.debug(`[lark-cli card] ${msg}`);
      }
    });
    cardConsumer.on("close", (code) => {
      // Clicks would silently die with the consumer; degrade to text-only
      // so future prompts stop carrying buttons we can no longer receive.
      logger.warn(
        `Card action consumer exited (code=${code}); falling back to text-only prompts`,
      );
      cardActionSupport = false;
    });
    logger.info("Interactive card permission prompts enabled");
  } else {
    logger.warn(
      "lark-cli does not support card.action.trigger — using text-reply permission prompts",
    );
  }

  // ── Process events ──
  rl.on("line", (line: string) => {
    lastEventTime = Date.now();
    let event: FeishuEvent;
    try {
      event = JSON.parse(line.trim());
    } catch {
      return;
    }

    if (event.type !== "im.message.receive_v1") return;
    if (event.sender_id === BOT_OPEN_ID) return;
    if (!event.content?.trim()) return;
    if (!isAllowedChat(event.chat_id, allowedChatIds)) {
      logger.warn(`Rejected message from unallowlisted chat ${event.chat_id}`);
      return;
    }

    handleMessage(event).catch((err) => {
      logger.error(`Unhandled error in handleMessage: ${err.message}`);
    });
  });

  // ── Graceful shutdown ──
  let cleaningUp = false;
  function cleanup() {
    if (cleaningUp) return;
    cleaningUp = true;
    logger.info("Shutting down...");
    bridge.permissions.cleanup();
    cardConsumer?.kill("SIGTERM");
    consumer.kill("SIGTERM");
    removePidFile();
    process.exit(0);
  }

  process.on("SIGINT", cleanup);
  process.on("SIGTERM", cleanup);
  process.on("uncaughtException", (err) => {
    logger.error(`Uncaught exception: ${err.message}`);
    logger.debug(err.stack || "");
  });

  logger.info("✅ Gateway ready — waiting for Feishu messages...");
}

main().catch((err) => {
  logger.error(`Fatal: ${err.message}`);
  process.exit(1);
});
