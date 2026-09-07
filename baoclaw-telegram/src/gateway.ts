/**
 * BaoClaw Telegram Gateway — connects to the daemon as a second client via UDS.
 * Each connection gets its own QueryEngine with independent conversation history.
 * The gateway is a SEPARATE process from the daemon and CLI.
 */
import * as fs from "fs";
import * as path from "path";
import * as os from "os";
import {
  IpcClient,
  attachControlChannel,
  type ControlChannel,
  type DaemonConnector,
  type DaemonInfo,
} from "baoclaw-ipc";
import { createLogger } from "baoclaw-ipc/logger";
import { Bot, InputFile, type Context, type User } from "node-telegram-bot-api";
import { fromPath, run } from "node-telegram-bot-api/node";
import {
  parseDocument,
  buildDocumentBlock,
  buildImageBlock,
} from "./docParser.js";
import {
  parseCommand,
  isRegisteredCommand,
  formatError,
  COMMAND_REGISTRY,
  type SessionState,
} from "./commands.js";
import { splitMessage } from "./messageSplitter.js";
import { isAllowedChat } from "./authorization.js";
import {
  TelegramPermissionManager,
  buildPermissionKeyboard,
  formatPermissionRequest,
  parsePermissionReply,
  type PermissionDecision,
} from "./permission.js";
import { loadConfig } from "./config.js";
import { connectToDaemon } from "./daemon.js";
import { ChatQueue } from "./chatQueue.js";
import { markdownToTelegramHtml, extractBase64Images } from "./media.js";
import { createCommandHandlers } from "./handlers.js";

const logger = createLogger("telegram");

// ── Global error handlers ──
process.on("uncaughtException", (err) => {
  logger.error(`UNCAUGHT: ${String(err)}`);
  process.exit(1);
});
process.on("unhandledRejection", (err) => {
  logger.error(`UNHANDLED REJECTION: ${String(err)}`);
  process.exit(1);
});

const PID_FILE = path.join(os.homedir(), ".baoclaw", "telegram-gateway.pid");
const MAX_TG_MSG = 4096;
/** Characters of thinking text / tool input shown as a preview. */
const PREVIEW_CHARS = 200;
/** Characters of tool output kept in error notifications. */
const TOOL_OUTPUT_PREVIEW_CHARS = 500;

// ═══════════════════════════════════════════════════════════════
// Main gateway — bot construction, event wiring, shutdown
// ═══════════════════════════════════════════════════════════════
async function main() {
  const config = loadConfig();

  if (!config.token) {
    logger.error("Error: Telegram bot token not set.");
    logger.error(
      "Set telegram.token in ~/.baoclaw/config.json or TELEGRAM_BOT_TOKEN env var.",
    );
    process.exit(1);
  }
  if (config.allowedChatIds.length === 0) {
    logger.error(
      "Cannot start because no chat allowlist is configured. To fix, set allowedChatIds in config.json.",
    );
    process.exit(1);
  }

  logger.info("BaoClaw Telegram Gateway starting (daemon mode)...");

  // ── Discover and connect to daemon ──
  logger.info("Discovering BaoClaw daemon...");
  let ipcClient: IpcClient;
  let daemonInfo: DaemonInfo;
  let sessionState: SessionState;
  let daemonConnector: DaemonConnector;
  let control: ControlChannel;
  try {
    const conn = await connectToDaemon();
    ipcClient = conn.client;
    daemonInfo = conn.info;
    daemonConnector = conn.connector;
    sessionState = conn.sessionState;
    // Abort must not wait behind an in-flight turn on the serial main
    // connection — deliver it via the dedicated control channel.
    control = await attachControlChannel({
      socketPath: conn.info.socket,
      initParams: conn.initParams,
      fallbackClient: ipcClient,
    });
    logger.info(
      `Connected to daemon pid=${daemonInfo.pid} cwd=${daemonInfo.cwd} session=${daemonInfo.session_id}`,
    );
  } catch (err: any) {
    logger.error(`Failed to connect to daemon: ${err.message}`);
    process.exit(1);
  }

  // ── Start Telegram bot ──
  const bot = new Bot(config.token);
  const sendMessage = (
    chatId: number,
    text: string,
    options?: Record<string, unknown>,
  ) => bot.api.sendMessage({ chat_id: chatId, text, ...options });
  const sendChatAction = (chatId: number, action: string) =>
    bot.api.sendChatAction({ chat_id: chatId, action });
  const sendPhoto = async (
    chatId: number,
    photo: string | InputFile,
    options?: Record<string, unknown>,
  ) =>
    bot.api.sendPhoto({
      chat_id: chatId,
      photo: typeof photo === "string" ? await fromPath(photo) : photo,
      ...options,
    });
  const sendDocument = async (
    chatId: number,
    document: string | InputFile,
    options?: Record<string, unknown>,
  ) =>
    bot.api.sendDocument({
      chat_id: chatId,
      document:
        typeof document === "string" ? await fromPath(document) : document,
      ...options,
    });
  const getFileLink = async (fileId: string): Promise<string> => {
    const file = await bot.api.getFile({ file_id: fileId });
    if (!file.file_path) throw new Error("Telegram returned no file path");
    return `https://api.telegram.org/file/bot${config.token}/${file.file_path}`;
  };

  let botInfo: User;
  try {
    botInfo = await bot.api.getMe();
    logger.info(`Telegram bot @${botInfo.username} ready.`);

    // Keep the server-side command menu in sync with the registry. The menu
    // persists per-bot at Telegram; without this, entries set by whatever
    // previously used this bot token (e.g. stale "update hermes agent")
    // would linger forever.
    try {
      await bot.api.setMyCommands({
        commands: Object.entries(COMMAND_REGISTRY).map(([name, def]) => ({
          command: name.slice(1),
          description: def.description,
        })),
      });
    } catch (err: any) {
      logger.warn(`Failed to update command menu: ${err.message}`);
    }
    // Same persistence applies to the bot description — keep it clean too.
    try {
      await bot.api.setMyDescription({
        description:
          "BaoClaw — AI coding assistant with persistent memory. Talk to your project from anywhere.",
      });
    } catch (err: any) {
      logger.warn(`Failed to update bot description: ${err.message}`);
    }
  } catch (err: any) {
    logger.error(`Failed to connect to Telegram API: ${err.message}`);
    process.exit(1);
  }

  bot.catch((err: unknown) => {
    logger.error(
      `Telegram update error: ${err instanceof Error ? err.message : String(err)}`,
    );
  });

  // ── Write PID file ──
  const pidData = {
    pid: process.pid,
    bot_username: botInfo.username,
    daemon_pid: daemonInfo.pid,
    daemon_session_id: daemonInfo.session_id,
    started_at: new Date().toISOString(),
  };
  fs.mkdirSync(path.dirname(PID_FILE), { recursive: true });
  fs.writeFileSync(PID_FILE, JSON.stringify(pidData, null, 2));
  logger.info(`PID file: ${PID_FILE}`);

  // ── Per-chat state ──
  const chatQueue = new ChatQueue();
  // Per-chat response accumulator and completion signal
  const accumulators = new Map<number, string>();
  const thinkingAccumulators = new Map<number, string>();
  const resultResolvers = new Map<number, () => void>();
  // Per-chat pending attachments (for document/image uploads)
  const pendingAttachments = new Map<number, Record<string, unknown>[]>();
  let activeChatId: number | null = null;

  // ── Permission prompt state ──
  const permissionManager = new TelegramPermissionManager();

  /**
   * Apply a user's decision to the chat's pending permission request: clear
   * the pending entry, forward the decision via the control channel (the
   * daemon's main loop is parked mid-turn), and replace the prompt message.
   */
  async function applyPermissionDecision(
    chatId: number,
    decision: PermissionDecision,
  ): Promise<"none" | "applied" | "stale"> {
    const pending = permissionManager.resolve(chatId);
    if (!pending) return "none";
    // "Always" records a whole-tool allow rule keyed by the tool name.
    const rule = decision === "allow_always" ? pending.tool_name : undefined;
    let delivered = false;
    try {
      const res = await control.request<{ delivered: boolean }>(
        "permissionResponse",
        { tool_use_id: pending.tool_use_id, decision, rule },
      );
      delivered = res?.delivered === true;
    } catch {}
    const label =
      decision === "allow"
        ? "✅ Allowed"
        : decision === "allow_always"
          ? "🔁 Always allowed this tool"
          : "❌ Denied";
    const stale = delivered
      ? ""
      : "\n\n<i>(request already handled elsewhere)</i>";
    if (pending.message_id !== undefined) {
      // Replacing the text also drops the inline keyboard.
      bot.api
        .editMessageText({
          chat_id: chatId,
          message_id: pending.message_id,
          text: `${label} <code>${pending.tool_name}</code>${stale}`,
          parse_mode: "HTML",
        })
        .catch(() => {});
    }
    return delivered ? "applied" : "stale";
  }

  // ── Stream event handler ──
  ipcClient.onNotification("stream/event", async (params: unknown) => {
    const event = params as Record<string, unknown>;
    if (!event || typeof event !== "object") return;
    const chatId = activeChatId;
    if (chatId === null) return;

    switch (event.type) {
      case "assistant_chunk": {
        const content = (event as { content: string }).content || "";
        // If we were accumulating thinking, send it first
        const thinkingAcc = thinkingAccumulators.get(chatId);
        if (thinkingAcc && thinkingAcc.length > 0) {
          const thinkLen = Math.round(thinkingAcc.length / 4);
          const preview =
            thinkingAcc.length > PREVIEW_CHARS
              ? thinkingAcc.slice(0, PREVIEW_CHARS) + "…"
              : thinkingAcc;
          try {
            await sendMessage(
              chatId,
              `💭 <i>Thought (${thinkLen}tok)</i>\n<blockquote>${preview.replace(/</g, "&lt;").replace(/>/g, "&gt;")}</blockquote>`,
              { parse_mode: "HTML" },
            );
          } catch {}
          thinkingAccumulators.delete(chatId);
        }
        const current = accumulators.get(chatId) ?? "";
        accumulators.set(chatId, current + content);
        break;
      }

      case "thinking_chunk": {
        const content = (event as { content: string }).content || "";
        const current = thinkingAccumulators.get(chatId) ?? "";
        thinkingAccumulators.set(chatId, current + content);
        break;
      }

      case "tool_use": {
        const toolName =
          (event as { tool_name: string }).tool_name || "unknown";
        try {
          await sendMessage(chatId, `⚡ ${toolName}`);
        } catch {}
        break;
      }

      case "permission_request": {
        const pr = event as {
          tool_use_id: string;
          tool_name: string;
          input?: unknown;
          ask_timeout_secs?: number;
        };
        const preview = JSON.stringify(pr.input ?? {}).slice(0, PREVIEW_CHARS);
        // Mirror the daemon's exact auto-deny window for this ask (fallback
        // = the daemon default, for malformed/old-daemon events).
        const timeoutSecs = Math.max(1, pr.ask_timeout_secs ?? 300);
        try {
          const sent = await sendMessage(
            chatId,
            formatPermissionRequest(
              pr.tool_name || "unknown",
              preview,
              timeoutSecs,
            ),
            {
              parse_mode: "HTML",
              reply_markup: buildPermissionKeyboard(),
            },
          );
          permissionManager.register(
            chatId,
            {
              tool_use_id: pr.tool_use_id || "",
              tool_name: pr.tool_name || "unknown",
              message_id: (sent as { message_id?: number })?.message_id,
            },
            async (cid, toolUseId, reason) => {
              // Expiry/supersede must deny with the daemon so the parked turn
              // resumes. The daemon's own auto-deny always fires first (its
              // timer starts before ours), so `delivered` is usually false
              // here — notify the user for a real expiry regardless.
              try {
                await control.request("permissionResponse", {
                  tool_use_id: toolUseId,
                  decision: "deny",
                });
              } catch {}
              if (reason === "timeout") {
                try {
                  await sendMessage(
                    cid,
                    "⏰ Permission request timed out and was auto-denied.",
                  );
                } catch {}
              }
            },
            timeoutSecs * 1000,
          );
        } catch {}
        break;
      }

      case "tool_result": {
        const tr = event as { is_error: boolean; output: unknown };
        if (tr.is_error) {
          const output =
            typeof tr.output === "string"
              ? tr.output
              : JSON.stringify(tr.output);
          const truncated =
            output.length > TOOL_OUTPUT_PREVIEW_CHARS
              ? output.slice(0, TOOL_OUTPUT_PREVIEW_CHARS) + "..."
              : output;
          try {
            await sendMessage(chatId, `❌ Tool error: ${truncated}`);
          } catch {}
        } else {
          // Get output as string
          const outputStr =
            typeof tr.output === "string"
              ? tr.output
              : JSON.stringify(tr.output ?? "");

          // Helper: extract images from tool result content items
          // Supports multiple formats:
          //   MCP format:      { type: "image", data: "base64...", mimeType: "image/png" }
          //   Anthropic format: { type: "image", source: { type: "base64", media_type: "image/png", data: "base64..." } }
          //   Content array:   { content: [{ type: "image", ... }] }
          function extractImagesFromContent(
            content: any[],
          ): { buffer: Buffer; mediaType: string }[] {
            const imgs: { buffer: Buffer; mediaType: string }[] = [];
            for (const item of content) {
              if (item?.type !== "image") continue;
              // Anthropic format: data inside source
              if (
                item.source?.type === "base64" &&
                typeof item.source.data === "string" &&
                item.source.data.length > 100
              ) {
                const mediaType = item.source.media_type || "image/png";
                const ext = mediaType.split("/")[1] || "png";
                imgs.push({
                  buffer: Buffer.from(item.source.data, "base64"),
                  mediaType: ext,
                });
              }
              // MCP format: data at top level
              else if (
                typeof item.data === "string" &&
                item.data.length > 100
              ) {
                const ext =
                  (item.mimeType || item.media_type || "image/png").split(
                    "/",
                  )[1] || "png";
                imgs.push({
                  buffer: Buffer.from(item.data, "base64"),
                  mediaType: ext,
                });
              }
            }
            return imgs;
          }

          // Helper: send an image buffer via the Telegram photo adapter
          async function sendToolResultImage(
            chatId: number,
            img: { buffer: Buffer; mediaType: string },
            index: number,
            caption?: string,
          ): Promise<void> {
            const ext = img.mediaType === "jpeg" ? "jpg" : img.mediaType;
            const tmpFile = path.join(
              os.tmpdir(),
              `baoclaw-img-${Date.now()}-${index}.${ext}`,
            );
            fs.writeFileSync(tmpFile, img.buffer);
            const cap =
              caption ||
              (index === 0
                ? "📸 Image generated"
                : `📸 Image generated (${index + 1})`);
            await sendPhoto(chatId, tmpFile, { caption: cap });
            try {
              fs.unlinkSync(tmpFile);
            } catch {}
          }

          let sent = false;
          try {
            const parsed =
              typeof tr.output === "object" && tr.output !== null
                ? (tr.output as any)
                : JSON.parse(outputStr);

            // Case 1: Top-level image object (ImageGenTool format)
            // { type: "image", source: { type: "base64", media_type: "...", data: "..." } }
            if (
              parsed?.type === "image" &&
              parsed?.source?.data &&
              parsed.source.data.length > 100
            ) {
              const mediaType = parsed.source.media_type || "image/png";
              const ext = mediaType.split("/")[1] || "png";
              const buffer = Buffer.from(parsed.source.data, "base64");
              const caption = parsed.prompt
                ? `📸 ${parsed.prompt}`
                : "📸 Image generated";
              await sendToolResultImage(
                chatId,
                { buffer, mediaType: ext },
                0,
                caption,
              );
              sent = true;
            }
            // Case 2: Content array format (MCP tools)
            // { content: [{ type: "image", data: "...", mimeType: "..." }] }
            else if (Array.isArray(parsed?.content)) {
              const images = extractImagesFromContent(parsed.content);
              for (let i = 0; i < images.length; i++) {
                try {
                  await sendToolResultImage(chatId, images[i], i);
                  sent = true;
                } catch (err) {
                  logger.error(`Failed to send tool result image: ${err}`);
                }
              }
            }
            // Case 3: Top-level MCP image (data at root)
            // { type: "image", data: "base64...", mimeType: "image/png" }
            else if (
              parsed?.type === "image" &&
              typeof parsed?.data === "string" &&
              parsed.data.length > 100
            ) {
              const ext =
                (parsed.mimeType || "image/png").split("/")[1] || "png";
              const buffer = Buffer.from(parsed.data, "base64");
              await sendToolResultImage(chatId, { buffer, mediaType: ext }, 0);
              sent = true;
            }
          } catch {
            // JSON parse failed (likely truncated output) — extract base64 with regex
            const b64Match = outputStr.match(
              /"data"\s*:\s*"([A-Za-z0-9+/=]{1000,})"/,
            );
            if (b64Match) {
              try {
                const tmpFile = path.join(
                  os.tmpdir(),
                  `baoclaw-img-${Date.now()}.png`,
                );
                fs.writeFileSync(tmpFile, Buffer.from(b64Match[1], "base64"));
                await sendPhoto(chatId, tmpFile, {
                  caption: "📸 Image generated",
                });
                try {
                  fs.unlinkSync(tmpFile);
                } catch {}
                sent = true;
              } catch (err) {
                logger.error(
                  `Failed to extract/send image from truncated output: ${err}`,
                );
              }
            }
          }
          if (sent) {
            // Don't send redundant text message — the photo was already sent
          }
        }
        break;
      }

      case "error": {
        const err = event as { code: string; message: string };
        try {
          await sendMessage(
            chatId,
            `❌ [${err.code || "ERROR"}] ${err.message || "Unknown error"}`,
          );
        } catch {}
        // Signal completion
        const resolver = resultResolvers.get(chatId);
        if (resolver) {
          resultResolvers.delete(chatId);
          resolver();
        }
        break;
      }

      case "result": {
        const accumulated = accumulators.get(chatId) ?? "";
        if (accumulated.length > 0) {
          // Extract and send base64 images as real photos
          const { text, images } = extractBase64Images(accumulated);
          if (images.length > 0) {
            logger.info(
              `Extracted ${images.length} image(s) from accumulated text (${accumulated.length} chars)`,
            );
          }
          // Send text first
          if (text.trim().length > 0) {
            const chunks = splitMessage(text, MAX_TG_MSG);
            for (const chunk of chunks) {
              try {
                await sendMessage(chatId, markdownToTelegramHtml(chunk), {
                  parse_mode: "HTML",
                });
              } catch {
                try {
                  await sendMessage(chatId, chunk);
                } catch (err) {
                  logger.error(`Failed to send Telegram message: ${err}`);
                }
              }
            }
          }
          // Then send images
          for (const img of images) {
            try {
              const tmpFile = path.join(
                os.tmpdir(),
                `baoclaw-img-${Date.now()}-${Math.random().toString(36).slice(2, 6)}.png`,
              );
              fs.writeFileSync(tmpFile, img.buffer);
              await sendPhoto(chatId, tmpFile, {
                caption: img.caption || undefined,
              });
              fs.unlinkSync(tmpFile);
            } catch (err) {
              logger.error(
                `Failed to send photo (${img.buffer.length} bytes): ${err}`,
              );
            }
          }
        }
        accumulators.delete(chatId);
        thinkingAccumulators.delete(chatId);
        // Signal completion
        const resolver = resultResolvers.get(chatId);
        if (resolver) {
          resultResolvers.delete(chatId);
          resolver();
        }
        break;
      }
    }
  });

  // ── Handle daemon disconnect ──
  ipcClient.onDisconnect(() => {
    logger.warn("Daemon connection lost. Shutting down.");
    bot.stop();
    try {
      fs.unlinkSync(PID_FILE);
    } catch {}
    process.exit(1);
  });

  // ── Command handler dispatch table ──
  const commandHandlers = createCommandHandlers({
    ipcClient,
    control,
    daemonInfo,
    botUsername: botInfo.username!,
    sessionState,
    daemonConnector,
    sendDocument,
    quitGateway: () => {
      bot.stop();
      // Close the control channel before the main connection: disconnecting
      // the main client fires its onDisconnect handler, which exits the
      // process and would skip this cleanup.
      control.close().catch(() => {});
      ipcClient.disconnect().catch(() => {});
      try {
        fs.unlinkSync(PID_FILE);
      } catch {}
      process.exit(0);
    },
  });

  // ── Process a single message for a chat ──
  async function processMessage(
    chatId: number,
    text: string,
    attachments?: Record<string, unknown>[],
  ): Promise<void> {
    // Single-slot rule: never steal the active slot from a chat whose turn is
    // still in flight (e.g. parked on a permission prompt), or that chat's
    // stream events would be dropped and its queue wedged forever.
    if (activeChatId !== null && activeChatId !== chatId) {
      await sendMessage(
        chatId,
        "⏳ Another session is being processed. Please wait for the current request to finish and try again.",
      );
      return;
    }
    const previousChatId = activeChatId;
    activeChatId = chatId;
    accumulators.set(chatId, "");

    // Create a promise that resolves when result/error event arrives
    const resultPromise = new Promise<void>((resolve) => {
      resultResolvers.set(chatId, resolve);
    });

    try {
      await sendChatAction(chatId, "typing");
      const params: Record<string, unknown> = { prompt: text };
      if (attachments && attachments.length > 0) {
        params.attachments = attachments;
      }
      await ipcClient.request("submitMessage", params);
      // Wait for the stream to complete (result or error event)
      await resultPromise;
    } catch (err: any) {
      const msg = err.message || "";
      if (msg.includes("session busy")) {
        // -32001: another client is submitting a message
        try {
          await sendMessage(
            chatId,
            "⏳ Session busy — another client is submitting a message. Please try again later.",
          );
        } catch {}
      } else {
        logger.error(`submitMessage error for chat ${chatId}: ${msg}`);
        try {
          await sendMessage(chatId, `❌ ${msg}`);
        } catch {}
      }
      // Clean up in case result never came
      accumulators.delete(chatId);
      thinkingAccumulators.delete(chatId);
      resultResolvers.delete(chatId);
    }

    // Restore, don't null: an outer turn may still be streaming its events.
    activeChatId = previousChatId;
  }

  // ── Process queue for a chat ──
  async function processQueue(chatId: number): Promise<void> {
    chatQueue.startProcessing(chatId);
    while (chatQueue.hasQueued(chatId)) {
      const text = chatQueue.dequeue(chatId);
      if (!text) break;
      // Check for pending attachments
      const attachments = pendingAttachments.get(chatId);
      pendingAttachments.delete(chatId);
      await processMessage(chatId, text, attachments);
    }
    chatQueue.finishProcessing(chatId);
  }

  // ── Bot message handler ──
  bot.on("message", async (ctx: Context) => {
    const msg = ctx.message;
    if (!msg) return;
    const chatId = msg.chat.id;

    // Allowlist is validated at startup; reject every non-member.
    if (!isAllowedChat(chatId, config.allowedChatIds)) {
      logger.info(`Rejected: chat ${chatId}`);
      return;
    }

    // ── Handle document uploads (PDF, DOCX) ──
    if (msg.document) {
      const doc = msg.document;
      const fileName = doc.file_name || "unknown";
      const mimeType = doc.mime_type || "application/octet-stream";
      const caption = msg.caption || `Please analyze this file: ${fileName}`;

      try {
        await sendMessage(chatId, `📄 Processing file: ${fileName}...`);
        const fileLink = await getFileLink(doc.file_id);
        const resp = await fetch(fileLink);
        const buffer = Buffer.from(await resp.arrayBuffer());

        // Route B: try native document block (PDF only)
        const docBlock = buildDocumentBlock(buffer, mimeType);
        if (docBlock) {
          // Send as attachment for native API support
          chatQueue.enqueue(chatId, caption);
          // Store attachments for the next processMessage call
          pendingAttachments.set(chatId, [docBlock]);
          if (!chatQueue.isProcessing(chatId)) {
            processQueue(chatId);
          }
          return;
        }

        // Route A: extract text for non-PDF or as fallback
        const parsed = await parseDocument(buffer, mimeType, fileName);
        if (parsed.error) {
          await sendMessage(chatId, `❌ ${parsed.error}`);
          return;
        }
        if (!parsed.text.trim()) {
          await sendMessage(
            chatId,
            "⚠️ File content is empty or no text could be extracted.",
          );
          return;
        }

        // Truncate if too large (keep ~100k chars to stay within context limits)
        const maxChars = 100_000;
        let docText = parsed.text;
        if (docText.length > maxChars) {
          docText =
            docText.slice(0, maxChars) +
            `\n\n[... document truncated, ${parsed.text.length} characters total]`;
        }

        const prompt = `[File: ${fileName}${parsed.pageCount ? ` (${parsed.pageCount} pages)` : ""}]\n\n${docText}\n\n---\n${caption}`;
        chatQueue.enqueue(chatId, prompt);
        if (!chatQueue.isProcessing(chatId)) {
          processQueue(chatId);
        }
      } catch (err: any) {
        logger.error(`Document processing error: ${err.message}`);
        try {
          await sendMessage(
            chatId,
            `❌ File processing failed: ${err.message}`,
          );
        } catch {}
      }
      return;
    }

    // ── Handle photo uploads ──
    if (msg.photo && msg.photo.length > 0) {
      const photo = msg.photo[msg.photo.length - 1]; // highest resolution
      const caption = msg.caption || "Please describe this image";

      try {
        await sendMessage(chatId, "🖼️ Processing image...");
        const fileLink = await getFileLink(photo.file_id);
        const resp = await fetch(fileLink);
        const buffer = Buffer.from(await resp.arrayBuffer());

        // Detect mime type from file extension
        const ext = fileLink.split(".").pop()?.toLowerCase() || "jpg";
        const mimeMap: Record<string, string> = {
          jpg: "image/jpeg",
          jpeg: "image/jpeg",
          png: "image/png",
          gif: "image/gif",
          webp: "image/webp",
        };
        const mimeType = mimeMap[ext] || "image/jpeg";

        const imageBlock = buildImageBlock(buffer, mimeType);
        chatQueue.enqueue(chatId, caption);
        pendingAttachments.set(chatId, [imageBlock]);
        if (!chatQueue.isProcessing(chatId)) {
          processQueue(chatId);
        }
      } catch (err: any) {
        logger.error(`Photo processing error: ${err.message}`);
        try {
          await sendMessage(
            chatId,
            `❌ Image processing failed: ${err.message}`,
          );
        } catch {}
      }
      return;
    }

    // ── Handle text messages ──
    if (!msg.text) return;

    // ── Permission reply fallback (before command routing) ──
    // A decision keyword only counts while a prompt is pending in this chat;
    // anything else falls through to commands/chat as usual.
    if (permissionManager.get(chatId)) {
      const decision = parsePermissionReply(msg.text);
      if (decision) {
        const outcome = await applyPermissionDecision(chatId, decision);
        await sendMessage(
          chatId,
          outcome === "applied"
            ? "✅ Processed."
            : "⚠️ This request has expired.",
        );
        return;
      }
    }

    // Command routing
    const parsed = parseCommand(msg.text);
    if (parsed && isRegisteredCommand(msg.text)) {
      const handler = commandHandlers[parsed.command];
      if (handler) {
        try {
          const result = await handler(parsed.args, chatId);
          if (result) {
            const chunks = splitMessage(result, MAX_TG_MSG);
            for (const chunk of chunks) {
              await sendMessage(chatId, chunk);
            }
          }
        } catch (err) {
          await sendMessage(chatId, formatError(err));
        }
        return;
      }
    }

    // Unregistered commands and regular messages → enqueue for AI
    chatQueue.enqueue(chatId, msg.text);
    if (!chatQueue.isProcessing(chatId)) {
      processQueue(chatId);
    }
  });

  // ── Permission inline-button callbacks ──
  // callback_data carries the decision only ("perm:allow" / "perm:always" /
  // "perm:deny") — the pending request is looked up per chat, keeping the
  // payload far below Telegram's 64-byte callback_data cap.
  bot.on("callback_query", async (ctx: Context) => {
    const cq = ctx.callbackQuery as
      | {
          data?: string;
          message?: { message_id?: number; chat?: { id?: number } };
        }
      | undefined;
    try {
      const decisions: Record<string, PermissionDecision> = {
        "perm:allow": "allow",
        "perm:always": "allow_always",
        "perm:deny": "deny",
      };
      const decision = cq?.data ? decisions[cq.data] : undefined;
      const chatId = cq?.message?.chat?.id;
      if (!decision || chatId === undefined) {
        await ctx.answerCallbackQuery({ text: "Invalid request" });
        return;
      }
      // Allowlist keys on the CHAT id (same policy as text messages) — never
      // on cq.from.id, which in groups is a member, not the chat.
      if (!isAllowedChat(chatId, config.allowedChatIds)) {
        await ctx.answerCallbackQuery({ text: "Unauthorized chat" });
        return;
      }
      const outcome = await applyPermissionDecision(chatId, decision);
      await ctx.answerCallbackQuery({
        text:
          outcome === "applied"
            ? "Processed"
            : outcome === "stale"
              ? "This request has expired"
              : "No pending request",
      });
    } catch {
      try {
        await ctx.answerCallbackQuery({ text: "Failed to process" });
      } catch {}
    }
  });

  // ── Graceful shutdown ──
  const shutdown = (signal: string) => {
    logger.info(`Shutdown (${signal})`);
    permissionManager.cleanup();
    bot.stop();
    // Control first: the main client's onDisconnect handler exits the
    // process and would skip this cleanup.
    control.close().catch(() => {});
    ipcClient.disconnect().catch(() => {});
    try {
      fs.unlinkSync(PID_FILE);
    } catch {}
    process.exit(0);
  };
  process.on("SIGTERM", () => shutdown("SIGTERM"));
  process.on("SIGINT", () => shutdown("SIGINT"));

  logger.info("Telegram Gateway ready.");
  await run(bot);
}

main().catch((err) => {
  logger.error(`Gateway failed: ${err.message}`);
  process.exit(1);
});
