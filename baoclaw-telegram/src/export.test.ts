import { test, describe } from "node:test";
import assert from "node:assert/strict";
import {
  formatTranscriptToMarkdown,
  markdownToPdf,
  type TranscriptEntry,
} from "./export.js";

// The implementation is shared with baoclaw-web (this package re-exports
// baoclaw-web/src/export.ts); the full test suite lives there. This smoke
// test only guards the re-export path from Telegram's context.
describe("export module (shared re-export)", () => {
  test("re-exports the shared implementation", async () => {
    const entries: TranscriptEntry[] = [
      { role: "user", text: "ping", timestamp: "2026-01-01 10:00:00" },
    ];
    const markdown = formatTranscriptToMarkdown(entries, { sessionId: "s1" });
    assert.match(markdown, /## 用户 \(2026-01-01 10:00:00\)/);
    assert.match(markdown, /\*\*会话\*\*: s1/);

    const pdfBuffer = await markdownToPdf(markdown);
    assert.ok(Buffer.isBuffer(pdfBuffer));
    assert.equal(pdfBuffer.subarray(0, 5).toString("ascii"), "%PDF-");
  });
});
