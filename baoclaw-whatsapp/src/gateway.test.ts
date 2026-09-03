import { strict as assert } from "node:assert";
import test from "node:test";
import * as fs from "fs";
import * as path from "path";
import * as os from "os";
import { WhatsAppGateway } from "./gateway.js";

function createMockSock() {
  const listeners: Record<string, Function[]> = {};
  const sentMessages: { jid: string; content: any }[] = [];

  return {
    ev: {
      on(event: string, cb: Function) {
        if (!listeners[event]) listeners[event] = [];
        listeners[event].push(cb);
      },
      emit(event: string, data: any) {
        if (listeners[event]) {
          for (const cb of listeners[event]) {
            cb(data);
          }
        }
      },
    },
    async sendMessage(jid: string, content: any) {
      sentMessages.push({ jid, content });
      return { key: { id: "msg_reply_" + Math.random().toString(36).substring(7) } };
    },
    sentMessages,
    listeners,
  };
}

function createMockIpcClient() {
  const notificationListeners: Record<string, Function[]> = {};
  const requests: { method: string; params: any }[] = [];
  let shouldFailSubmit = false;

  return {
    connected: true,
    onNotification(event: string, cb: Function) {
      if (!notificationListeners[event]) notificationListeners[event] = [];
      notificationListeners[event].push(cb);
    },
    emitNotification(event: string, params: any) {
      if (notificationListeners[event]) {
        for (const cb of notificationListeners[event]) {
          cb(params);
        }
      }
    },
    async request(method: string, params: any) {
      requests.push({ method, params });
      if (shouldFailSubmit && method === "submitMessage") {
        throw new Error("RPC Connection Failed");
      }
      return { success: true };
    },
    setShouldFailSubmit(fail: boolean) {
      shouldFailSubmit = fail;
    },
    requests,
    onDisconnect(_cb: Function) {},
  };
}

test("WhatsAppGateway start throws error when allowFrom is empty or invalid", async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "gateway-test-"));
  const configPath = path.join(tmpDir, "config.json");

  fs.writeFileSync(
    configPath,
    JSON.stringify({
      enabled: true,
      allowFrom: [],
      sharedSessionId: "test-session",
    })
  );

  const gateway = new WhatsAppGateway({ configPath });

  await assert.rejects(
    async () => {
      await gateway.start();
    },
    (err: Error) => {
      assert.match(
        err.message,
        /Cannot start WhatsApp gateway because allowFrom is empty or invalid/
      );
      return true;
    }
  );

  fs.rmSync(tmpDir, { recursive: true, force: true });
});

test("WhatsAppGateway PID file management", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "gateway-pid-test-"));
  const pidFile = path.join(tmpDir, "whatsapp-gateway.pid");
  const gateway = new WhatsAppGateway({ pidFile });

  // Call writePidFile
  (gateway as any).writePidFile();
  assert.equal(fs.existsSync(pidFile), true);

  const content = JSON.parse(fs.readFileSync(pidFile, "utf-8"));
  assert.equal(content.pid, process.pid);

  // Call removePidFile
  (gateway as any).removePidFile();
  assert.equal(fs.existsSync(pidFile), false);

  fs.rmSync(tmpDir, { recursive: true, force: true });
});

test("setupInboundHandler filters disallowed senders", async () => {
  const gateway = new WhatsAppGateway();
  const mockSock = createMockSock();

  (gateway as any).config = {
    allowFrom: ["+1234567890"],
    groupPolicy: "allow",
    dmPolicy: "allow",
    maxQueueSize: 10,
  };

  (gateway as any).setupInboundHandler(mockSock);

  // Emit inbound message from unauthorized number
  mockSock.ev.emit("messages.upsert", {
    messages: [
      {
        key: { id: "msg_1", remoteJid: "9999999999@s.whatsapp.net" },
        message: { conversation: "Hello from unknown" },
      },
    ],
  });

  // Check no messages sent and message queue is empty
  assert.equal(mockSock.sentMessages.length, 0);
  assert.equal((gateway as any).messageQueue.hasQueued("+9999999999"), false);
});

test("setupInboundHandler skips broadcast messages and respects dmPolicy/groupPolicy", async () => {
  const gateway = new WhatsAppGateway();
  const mockSock = createMockSock();

  (gateway as any).config = {
    allowFrom: ["+1234567890"],
    groupPolicy: "ignore",
    dmPolicy: "ignore",
    maxQueueSize: 10,
  };

  (gateway as any).setupInboundHandler(mockSock);

  // 1. Broadcast message
  mockSock.ev.emit("messages.upsert", {
    messages: [
      {
        key: { id: "msg_bcast", remoteJid: "status@broadcast" },
        message: { conversation: "Broadcast msg" },
      },
    ],
  });

  // 2. Group message (ignored per groupPolicy)
  mockSock.ev.emit("messages.upsert", {
    messages: [
      {
        key: {
          id: "msg_group",
          remoteJid: "1234567890-12345@g.us",
          participant: "1234567890@s.whatsapp.net",
        },
        message: { conversation: "Group msg" },
      },
    ],
  });

  // 3. DM message (ignored per dmPolicy)
  mockSock.ev.emit("messages.upsert", {
    messages: [
      {
        key: { id: "msg_dm", remoteJid: "1234567890@s.whatsapp.net" },
        message: { conversation: "DM msg" },
      },
    ],
  });

  assert.equal(mockSock.sentMessages.length, 0);
  assert.equal((gateway as any).messageQueue.hasQueued("+1234567890"), false);
});

test("setupInboundHandler deduplicates messages and handles unknown commands", async () => {
  const gateway = new WhatsAppGateway();
  const mockSock = createMockSock();

  (gateway as any).config = {
    allowFrom: ["+1234567890"],
    groupPolicy: "allow",
    dmPolicy: "allow",
    maxQueueSize: 10,
  };

  (gateway as any).setupInboundHandler(mockSock);

  // Pre-mark message ID as duplicate
  (gateway as any).messageQueue.isDuplicate("msg_dup_1");

  // Send duplicate message
  mockSock.ev.emit("messages.upsert", {
    messages: [
      {
        key: { id: "msg_dup_1", remoteJid: "1234567890@s.whatsapp.net" },
        message: { conversation: "Duplicate message" },
      },
    ],
  });

  assert.equal(mockSock.sentMessages.length, 0);

  // Send unknown command
  mockSock.ev.emit("messages.upsert", {
    messages: [
      {
        key: { id: "msg_cmd_unknown", remoteJid: "1234567890@s.whatsapp.net" },
        message: { conversation: "/unknowncommand test" },
      },
    ],
  });

  assert.equal(mockSock.sentMessages.length, 1);
  assert.match(
    mockSock.sentMessages[0].content.text,
    /未知命令 \/unknowncommand/
  );
});

test("setupInboundHandler handles rate limiting", async () => {
  const gateway = new WhatsAppGateway();
  const mockSock = createMockSock();

  (gateway as any).config = {
    allowFrom: ["+1234567890"],
    groupPolicy: "allow",
    dmPolicy: "allow",
    maxQueueSize: 10,
  };

  (gateway as any).setupInboundHandler(mockSock);

  // Exhaust rate limit tokens for +1234567890 based on rateLimiter's maxMessages
  const rateLimiter = (gateway as any).rateLimiter;
  const maxMessages = (rateLimiter as any).maxMessages ?? 20;
  for (let i = 0; i < maxMessages; i++) {
    rateLimiter.tryConsume("+1234567890");
  }

  // Send message when rate limited
  mockSock.ev.emit("messages.upsert", {
    messages: [
      {
        key: { id: "msg_ratelimit", remoteJid: "1234567890@s.whatsapp.net" },
        message: { conversation: "Fast message" },
      },
    ],
  });

  assert.equal(mockSock.sentMessages.length, 1);
  assert.match(mockSock.sentMessages[0].content.text, /Rate limit exceeded/);
});

test("setupStreamHandler processes chunks, tool use, permission, error and result events", async () => {
  const gateway = new WhatsAppGateway();
  const mockSock = createMockSock();
  const mockIpc = createMockIpcClient();

  (gateway as any).ipcClient = mockIpc;
  (gateway as any).activeSender = "+1234567890";
  (gateway as any).senderTracker.registerSender(
    "+1234567890",
    "1234567890@s.whatsapp.net"
  );
  (gateway as any).processingFlags.add("+1234567890");

  (gateway as any).setupStreamHandler(mockSock);

  // 1. assistant_chunk event
  mockIpc.emitNotification("stream/event", {
    type: "assistant_chunk",
    content: "Hello, ",
  });
  mockIpc.emitNotification("stream/event", {
    type: "assistant_chunk",
    content: "WhatsApp world!",
  });

  assert.equal(
    (gateway as any).senderTracker.getAccumulated("+1234567890"),
    "Hello, WhatsApp world!"
  );

  // 2. tool_use event
  mockIpc.emitNotification("stream/event", {
    type: "tool_use",
    tool_name: "bash",
  });

  assert.equal(mockSock.sentMessages.length, 1);
  assert.match(mockSock.sentMessages[0].content.text, /🔨 使用工具: bash/);

  // 3. permission_request event
  mockIpc.emitNotification("stream/event", {
    type: "permission_request",
    tool_use_id: "req_100",
    tool_name: "bash",
    description: "Run command ls",
  });

  assert.equal(mockSock.sentMessages.length, 2);
  assert.match(mockSock.sentMessages[1].content.text, /⚠️ 权限请求/);

  // 4. result event
  mockIpc.emitNotification("stream/event", {
    type: "result",
  });

  assert.equal(mockSock.sentMessages.length, 3);
  assert.equal(
    mockSock.sentMessages[2].content.text,
    "Hello, WhatsApp world!"
  );
  assert.equal(
    (gateway as any).senderTracker.getAccumulated("+1234567890"),
    ""
  );
  assert.equal((gateway as any).processingFlags.has("+1234567890"), false);
});

test("setupStreamHandler handles error events and clears state", async () => {
  const gateway = new WhatsAppGateway();
  const mockSock = createMockSock();
  const mockIpc = createMockIpcClient();

  (gateway as any).ipcClient = mockIpc;
  (gateway as any).activeSender = "+1234567890";
  (gateway as any).senderTracker.registerSender(
    "+1234567890",
    "1234567890@s.whatsapp.net"
  );
  (gateway as any).processingFlags.add("+1234567890");

  (gateway as any).setupStreamHandler(mockSock);

  // Emit assistant_chunk then error
  mockIpc.emitNotification("stream/event", {
    type: "assistant_chunk",
    content: "Partial content",
  });

  mockIpc.emitNotification("stream/event", {
    type: "error",
    code: "TIMEOUT",
    message: "Operation timed out",
  });

  assert.equal(mockSock.sentMessages.length, 1);
  assert.match(mockSock.sentMessages[0].content.text, /Operation timed out/);
  assert.equal(
    (gateway as any).senderTracker.getAccumulated("+1234567890"),
    ""
  );
  assert.equal((gateway as any).processingFlags.has("+1234567890"), false);
});

test("processQueue handles submitMessage RPC failure", async () => {
  const gateway = new WhatsAppGateway();
  const mockSock = createMockSock();
  const mockIpc = createMockIpcClient();

  mockIpc.setShouldFailSubmit(true);
  (gateway as any).ipcClient = mockIpc;
  (gateway as any).config = { maxQueueSize: 10 };
  (gateway as any).senderTracker.registerSender(
    "+1234567890",
    "1234567890@s.whatsapp.net"
  );

  (gateway as any).messageQueue.enqueue("+1234567890", "Hello test", 10);

  await (gateway as any).processQueue("+1234567890", mockSock);

  assert.equal(mockSock.sentMessages.length, 1);
  assert.match(
    mockSock.sentMessages[0].content.text,
    /RPC Connection Failed/
  );
  assert.equal((gateway as any).processingFlags.has("+1234567890"), false);
});
