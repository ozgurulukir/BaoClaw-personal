/**
 * Baileys Session Manager (v7.x).
 * WhatsApp Web connection via QR code (default) or pairing code (--pairing flag).
 */
import * as fs from "fs";
import * as path from "path";
import * as os from "os";
import * as readline from "readline";
import { createLogger } from "../../ts-ipc/logger.js";

const runtimeLogger = createLogger("whatsapp");
const log = (level: "info" | "warn" | "error", args: unknown[]) =>
  runtimeLogger[level](args.map(String).join(" "));

let makeWASocket: any;
let useMultiFileAuthState: any;
let DisconnectReason: any;
let Browsers: any;
let fetchLatestBaileysVersion: any;

async function loadDeps() {
  const baileys = await import("@whiskeysockets/baileys");
  makeWASocket = baileys.makeWASocket ?? baileys.default;
  useMultiFileAuthState = baileys.useMultiFileAuthState;
  DisconnectReason = baileys.DisconnectReason;
  Browsers = baileys.Browsers;
  fetchLatestBaileysVersion = baileys.fetchLatestBaileysVersion;
}

const AUTH_DIR_NAME = "whatsapp-auth";
const MAX_RETRIES = 5;
const RETRY_BASE_DELAY_MS = 3_000;
const RETRY_MAX_DELAY_MS = 30_000;
const usePairingMode = process.argv.includes("--pairing");

export function retryDelayMs(retry: number): number {
  const exponent = Math.max(0, retry - 1);
  return Math.min(RETRY_BASE_DELAY_MS * 2 ** exponent, RETRY_MAX_DELAY_MS);
}

const logger = {
  level: "warn" as const,
  info: (...args: any[]) => log("info", args),
  warn: (...args: any[]) => log("warn", ["[Baileys warn]", ...args]),
  error: (...args: any[]) => log("error", ["[Baileys error]", ...args]),
  debug: () => {},
  trace: () => {},
  fatal: (...args: any[]) => log("error", ["[Baileys fatal]", ...args]),
  child: () => logger,
} as any;

export function getAuthDir(): string {
  return path.join(os.homedir(), ".baoclaw", AUTH_DIR_NAME);
}

function secureAuthDirectory(authDir: string): void {
  fs.chmodSync(authDir, 0o700);
  for (const entry of fs.readdirSync(authDir, { withFileTypes: true })) {
    if (entry.isFile()) fs.chmodSync(path.join(authDir, entry.name), 0o600);
  }
}

function prompt(question: string): Promise<string> {
  const rl = readline.createInterface({
    input: process.stdin,
    output: process.stdout,
  });
  return new Promise((resolve) => {
    rl.question(question, (answer) => {
      rl.close();
      resolve(answer.trim());
    });
  });
}

function displayQR(qr: string) {
  // qrcode-terminal prints directly to stdout when called without callback
  try {
    // ESM dynamic import of CJS — qrcode-terminal writes to process.stdout
    import("qrcode-terminal")
      .then((mod: any) => {
        const qt = mod.default ?? mod;
        if (typeof qt.generate === "function") {
          qt.generate(qr, { small: true });
        } else {
          printQRAsURL(qr);
        }
      })
      .catch(() => printQRAsURL(qr));
  } catch {
    printQRAsURL(qr);
  }
}

function printQRAsURL(qr: string) {
  runtimeLogger.info(
    `\n📷 Open this URL in browser to get QR code, then scan with WhatsApp:\n`,
  );
  runtimeLogger.info(
    "QR data was not written to logs. Use terminal QR output instead.",
  );
}

export class SessionManager {
  private sock: any = null;
  private phoneNumber: string | null = null;
  private _isConnected = false;
  private authDir: string;
  private pairingPhone: string | null;
  private proxyUrl: string | null = null;
  private proxyAgent: any = undefined;
  private initializePromise: Promise<any> | null = null;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private connectionGeneration = 0;
  private lifecycleGeneration = 0;
  private pendingReject: ((reason?: unknown) => void) | null = null;
  private stopping = false;

  constructor(authDir?: string, pairingPhone?: string, proxyUrl?: string) {
    this.authDir = authDir ?? getAuthDir();
    this.pairingPhone = pairingPhone ?? null;
    this.proxyUrl = proxyUrl ?? null;
  }

  private async initProxy(): Promise<void> {
    if (this.proxyUrl && !this.proxyAgent) {
      try {
        const mod = await import("socks-proxy-agent");
        const SocksProxyAgent = mod.SocksProxyAgent || (mod as any).default;
        this.proxyAgent = new SocksProxyAgent(this.proxyUrl);
        runtimeLogger.info("Using configured proxy.");
      } catch (err: any) {
        runtimeLogger.warn(`Failed to create proxy agent: ${err.message}`);
      }
    }
  }

  async initialize(): Promise<any> {
    if (this._isConnected && this.sock) return this.sock;
    if (this.initializePromise) return this.initializePromise;

    if (this.retryTimer) {
      clearTimeout(this.retryTimer);
      this.retryTimer = null;
    }
    this.stopping = false;
    const lifecycleGeneration = ++this.lifecycleGeneration;
    const initialization = this.initializeInternal(lifecycleGeneration);
    this.initializePromise = initialization;
    try {
      return await initialization;
    } finally {
      if (this.initializePromise === initialization) {
        this.initializePromise = null;
      }
    }
  }

  private async initializeInternal(lifecycleGeneration: number): Promise<any> {
    await loadDeps();
    if (lifecycleGeneration !== this.lifecycleGeneration || this.stopping) {
      throw new Error("WhatsApp initialization was stopped");
    }
    await this.initProxy();

    let waVersion: number[] | undefined;
    try {
      const latest = await fetchLatestBaileysVersion();
      if (latest?.version) waVersion = latest.version;
    } catch (err: any) {
      runtimeLogger.warn(
        `Could not fetch latest WhatsApp version: ${err.message}`,
      );
    }

    if (lifecycleGeneration !== this.lifecycleGeneration || this.stopping) {
      throw new Error("WhatsApp initialization was stopped");
    }

    fs.mkdirSync(this.authDir, { recursive: true, mode: 0o700 });
    secureAuthDirectory(this.authDir);
    const { state, saveCreds } = await useMultiFileAuthState(this.authDir);
    if (lifecycleGeneration !== this.lifecycleGeneration || this.stopping) {
      throw new Error("WhatsApp initialization was stopped");
    }
    const hasAuth =
      fs.existsSync(path.join(this.authDir, "creds.json")) &&
      state.creds?.registered;

    return new Promise((resolve, reject) => {
      let retries = 0;
      let pairingRequested = false;
      let settled = false;

      const settleReject = (reason?: unknown) => {
        if (settled) return;
        settled = true;
        this.pendingReject = null;
        reject(
          reason instanceof Error
            ? reason
            : new Error(String(reason ?? "WhatsApp initialization failed")),
        );
      };

      this.pendingReject = settleReject;

      const startSocket = () => {
        if (this.stopping || lifecycleGeneration !== this.lifecycleGeneration) {
          settleReject(new Error("WhatsApp initialization was stopped"));
          return;
        }

        const generation = ++this.connectionGeneration;
        const browserConfig = Browsers
          ? Browsers.ubuntu("Chrome")
          : ["BaoClaw", "Chrome", "22.04"];

        const sock = makeWASocket({
          auth: state,
          browser: browserConfig,
          connectTimeoutMs: 60_000,
          logger,
          ...(waVersion ? { version: waVersion } : {}),
          ...(this.proxyAgent
            ? { agent: this.proxyAgent, fetchAgent: this.proxyAgent }
            : {}),
        });
        this.sock = sock;

        const isCurrentSocket = () =>
          generation === this.connectionGeneration && this.sock === sock;

        const scheduleRetry = () => {
          if (this.stopping || this.retryTimer) return;
          this.retryTimer = setTimeout(() => {
            this.retryTimer = null;
            startSocket();
          }, retryDelayMs(retries));
        };

        sock.ev.on("creds.update", () => {
          void saveCreds()
            .then(() => secureAuthDirectory(this.authDir))
            .catch((err: unknown) => {
              const message = err instanceof Error ? err.message : String(err);
              logger.error(`Failed to save auth credentials: ${message}`);
            });
        });

        sock.ev.on("connection.update", async (update: any) => {
          const { connection, lastDisconnect, qr } = update;

          if (qr) {
            if (usePairingMode && !pairingRequested) {
              pairingRequested = true;
              try {
                let phone = this.pairingPhone;
                if (!phone) {
                  phone = await prompt(
                    "\n📱 Enter WhatsApp phone (e.g. +8613812345678): ",
                  );
                }
                const cleaned = phone.replace(/[^0-9]/g, "");
                runtimeLogger.info(
                  `\nRequesting pairing code for +${cleaned}...`,
                );
                const code = await sock.requestPairingCode(cleaned);
                runtimeLogger.info(`\n🔑 Pairing code: ${code}`);
                runtimeLogger.info(
                  `Open WhatsApp → Settings → Linked Devices → Link a Device`,
                );
                runtimeLogger.info(
                  `Choose "Link with phone number instead" and enter the code.\n`,
                );
              } catch (err: any) {
                runtimeLogger.error(`Pairing code failed: ${err.message}`);
                runtimeLogger.info("\nFalling back to QR code:");
                await displayQR(qr);
              }
            } else {
              runtimeLogger.info("\n📱 Scan this QR code with WhatsApp:");
              await displayQR(qr);
              runtimeLogger.info(
                "Open WhatsApp → Settings → Linked Devices → Link a Device → Scan QR\n",
              );
            }
          }

          if (connection === "open" && isCurrentSocket()) {
            retries = 0;
            this.sock = sock;
            this._isConnected = true;
            this.phoneNumber = sock.user?.id
              ? "+" + sock.user.id.split(":")[0]
              : null;
            runtimeLogger.info(
              `\n✅ WhatsApp connected${this.phoneNumber ? ` as ${this.phoneNumber}` : ""}.`,
            );
            if (!settled) {
              settled = true;
              this.pendingReject = null;
              resolve(sock);
            }
          }

          if (connection === "close" && isCurrentSocket()) {
            this._isConnected = false;
            this.phoneNumber = null;
            this.sock = null;
            const statusCode = (lastDisconnect?.error as any)?.output
              ?.statusCode;
            const isLoggedOut = statusCode === DisconnectReason?.loggedOut;
            if (this.stopping) {
              settleReject(new Error("WhatsApp connection stopped"));
              return;
            }
            if (isLoggedOut) {
              runtimeLogger.info("Logged out. Clearing auth state.");
              this.clearAuthState();
              settleReject(new Error("Logged out from WhatsApp"));
              return;
            }
            retries++;
            if (retries > MAX_RETRIES) {
              settleReject(
                new Error(
                  `Failed after ${MAX_RETRIES} retries (status=${statusCode})`,
                ),
              );
              return;
            }
            runtimeLogger.info(
              `Connection closed (status=${statusCode}). Retry ${retries}/${MAX_RETRIES} in ${retryDelayMs(retries)}ms...`,
            );
            scheduleRetry();
          }
        });
      };

      if (!hasAuth) {
        runtimeLogger.info(
          `\n📱 Mode: ${usePairingMode ? "Pairing Code" : "QR Code scan"}`,
        );
      }
      startSocket();
    });
  }

  getPhoneNumber(): string | null {
    return this.phoneNumber;
  }
  isConnected(): boolean {
    return this._isConnected;
  }
  getSocket(): any {
    return this.sock;
  }

  async disconnect(): Promise<void> {
    this.stopping = true;
    this.lifecycleGeneration++;
    this.connectionGeneration++;
    if (this.retryTimer) {
      clearTimeout(this.retryTimer);
      this.retryTimer = null;
    }
    this.pendingReject?.(new Error("WhatsApp connection stopped"));
    this.pendingReject = null;

    const socket = this.sock;
    this.sock = null;
    this._isConnected = false;
    this.phoneNumber = null;
    this.initializePromise = null;
    if (socket) {
      try {
        socket.end(undefined);
      } catch {}
    }
  }

  clearAuthState(): void {
    try {
      fs.rmSync(this.authDir, { recursive: true, force: true });
    } catch {}
  }
}
