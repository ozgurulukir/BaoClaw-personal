import { test, describe } from "node:test";
import assert from "node:assert/strict";
import {
  formatTranscriptToMarkdown,
  defaultExportFilename,
  markdownToPdf,
  type TranscriptEntry,
} from "./export.js";

describe("export module", () => {
  describe("formatTranscriptToMarkdown", () => {
    test("formats user and assistant messages with metadata and tool calls", () => {
      const entries: TranscriptEntry[] = [
        {
          role: "user",
          text: "Hello BaoClaw",
          timestamp: "2025-01-01 10:00:00",
        },
        {
          role: "assistant",
          text: "Hello! How can I help?",
          timestamp: "2025-01-01 10:00:05",
          tools: [
            { name: "bash_execute", detail: "ls -la" },
            { name: "read_file" },
          ],
        },
      ];

      const markdown = formatTranscriptToMarkdown(entries, {
        sessionId: "session-123",
      });

      assert.match(markdown, /# BaoClaw 对话导出/);
      assert.match(markdown, /\*\*会话\*\*: session-123/);
      assert.match(markdown, /\*\*消息数\*\*: 2/);
      assert.match(markdown, /## 用户 \(2025-01-01 10:00:00\)/);
      assert.match(markdown, /Hello BaoClaw/);
      assert.match(markdown, /## 助手 \(2025-01-01 10:00:05\)/);
      assert.match(markdown, /Hello! How can I help\?/);
      assert.match(markdown, /### 工具调用/);
      assert.match(markdown, /- ⚡ bash_execute: ls -la/);
      assert.match(markdown, /- ⚡ read_file/);
    });

    test("omits tool calls when includeToolCalls is false", () => {
      const entries: TranscriptEntry[] = [
        {
          role: "assistant",
          text: "Response",
          tools: [{ name: "bash_execute", detail: "echo test" }],
        },
      ];

      const markdown = formatTranscriptToMarkdown(entries, {
        includeToolCalls: false,
      });

      assert.doesNotMatch(markdown, /### 工具调用/);
      assert.doesNotMatch(markdown, /bash_execute/);
    });

    test("handles empty entries and default options", () => {
      const markdown = formatTranscriptToMarkdown([]);
      assert.match(markdown, /\*\*会话\*\*: 未知/);
      assert.match(markdown, /\*\*消息数\*\*: 0/);
    });
  });

  describe("defaultExportFilename", () => {
    test("returns filename matching baoclaw-export pattern", () => {
      const filename = defaultExportFilename();
      assert.match(filename, /^baoclaw-export-\d{8}-\d{6}\.md$/);
    });
  });

  describe("markdownToPdf", () => {
    test("encodes markdown string as UTF-8 buffer", () => {
      const content = "# Test Document";
      const buffer = markdownToPdf(content);
      assert.ok(Buffer.isBuffer(buffer));
      assert.equal(buffer.toString("utf-8"), content);
    });
  });
});
