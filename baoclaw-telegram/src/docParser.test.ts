import { test, describe } from "node:test";
import assert from "node:assert/strict";
import {
  parseDocument,
  buildDocumentBlock,
  buildImageBlock,
} from "./docParser.js";

describe("docParser", () => {
  describe("parseDocument", () => {
    test("rejects legacy .doc format", async () => {
      const res = await parseDocument(Buffer.from("dummy"), "application/msword", "test.doc");
      assert.equal(res.text, "");
      assert.equal(res.error, "不支持旧版 .doc 格式，请转换为 .docx 后重试。");
    });

    test("rejects unsupported file mime/types", async () => {
      const res = await parseDocument(Buffer.from("dummy"), "application/zip", "archive.zip");
      assert.equal(res.text, "");
      assert.match(res.error || "", /不支持的文件类型: application\/zip \(zip\)/);
    });

    test("handles pdf error gracefully with invalid pdf buffer", async () => {
      const res = await parseDocument(Buffer.from("not a pdf"), "application/pdf", "sample.pdf");
      assert.equal(res.text, "");
      assert.match(res.error || "", /PDF parse failed/);
    });

    test("handles docx error gracefully with invalid docx buffer", async () => {
      const res = await parseDocument(
        Buffer.from("not a docx"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "sample.docx"
      );
      assert.equal(res.text, "");
      assert.match(res.error || "", /DOCX parse failed/);
    });
  });

  describe("buildDocumentBlock", () => {
    test("returns document block for PDF", () => {
      const buf = Buffer.from("pdf data");
      const block = buildDocumentBlock(buf, "application/pdf");
      assert.deepEqual(block, {
        type: "document",
        source: {
          type: "base64",
          media_type: "application/pdf",
          data: buf.toString("base64"),
        },
      });
    });

    test("returns null for non-PDF mime types", () => {
      const buf = Buffer.from("docx data");
      const block = buildDocumentBlock(
        buf,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
      );
      assert.equal(block, null);
    });
  });

  describe("buildImageBlock", () => {
    test("returns image block with correct media type and base64 data", () => {
      const buf = Buffer.from("image data");
      const block = buildImageBlock(buf, "image/jpeg");
      assert.deepEqual(block, {
        type: "image",
        source: {
          type: "base64",
          media_type: "image/jpeg",
          data: buf.toString("base64"),
        },
      });
    });
  });
});
