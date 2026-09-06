/**
 * Telegram gateway configuration — reads token and chat allowlist from
 * ~/.baoclaw/config.json with env-var fallback.
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { securePrivateFile } from "baoclaw-ipc/security";

export const CONFIG_PATH = path.join(os.homedir(), ".baoclaw", "config.json");

export interface TelegramConfig {
  token: string;
  allowedChatIds: number[];
}

export function loadConfig(): TelegramConfig {
  let raw: any = {};
  securePrivateFile(CONFIG_PATH);
  try {
    raw = JSON.parse(fs.readFileSync(CONFIG_PATH, "utf-8"));
  } catch {}
  const tg = raw?.telegram ?? {};
  return {
    token: tg.token || process.env.TELEGRAM_BOT_TOKEN || "",
    allowedChatIds: Array.isArray(tg.allowedChatIds)
      ? tg.allowedChatIds.filter(
          (id: unknown): id is number =>
            typeof id === "number" && Number.isSafeInteger(id),
        )
      : [],
  };
}
