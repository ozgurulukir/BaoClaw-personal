/**
 * Document parser — extracts text from PDF and DOCX files.
 * Used for Route A (client-side text extraction).
 */
import { createRequire } from "module";
const require = createRequire(import.meta.url);
const pdf = require("pdf-parse");
const mammoth = require("mammoth");

export interface ParsedDocument {
  text: string;
  pageCount?: number;
  error?: string;
}

/**
 * Extract text from a PDF buffer.
 */
export async function parsePdf(buffer: Buffer): Promise<ParsedDocument> {
  try {
    const data = await pdf(buffer);
    return { text: data.text, pageCount: data.numpages };
  } catch (err: any) {
    return { text: "", error: `PDF parse failed: ${err.message}` };
  }
}

/**
 * Extract text from a DOCX buffer.
 */
export async function parseDocx(buffer: Buffer): Promise<ParsedDocument> {
  try {
    const result = await mammoth.extractRawText({ buffer });
    return { text: result.value };
  } catch (err: any) {
    return { text: "", error: `DOCX parse failed: ${err.message}` };
  }
}

/**
 * Detect file type and extract text.
 */
export async function parseDocument(
  buffer: Buffer,
  mimeType: string,
  fileName: string,
): Promise<ParsedDocument> {
  const ext = fileName.toLowerCase().split(".").pop() || "";
  if (mimeType === "application/pdf" || ext === "pdf") {
    return parsePdf(buffer);
  }
  if (
    mimeType ===
      "application/vnd.openxmlformats-officedocument.wordprocessingml.document" ||
    ext === "docx"
  ) {
    return parseDocx(buffer);
  }
  // For .doc (legacy Word), we can't easily parse without LibreOffice
  if (ext === "doc") {
    return {
      text: "",
      error:
        "Legacy .doc format is not supported. Please convert it to .docx and try again.",
    };
  }
  return { text: "", error: `Unsupported file type: ${mimeType} (${ext})` };
}

/**
 * Build Anthropic-format content blocks for Route B (native API support).
 * Returns a document content block for PDF, or null if unsupported.
 */
export function buildDocumentBlock(
  buffer: Buffer,
  mimeType: string,
): Record<string, unknown> | null {
  if (mimeType === "application/pdf") {
    return {
      type: "document",
      source: {
        type: "base64",
        media_type: "application/pdf",
        data: buffer.toString("base64"),
      },
    };
  }
  // DOCX is not natively supported by Anthropic/OpenAI — must use text extraction
  return null;
}

/**
 * Build an image content block from a photo buffer.
 */
export function buildImageBlock(
  buffer: Buffer,
  mimeType: string,
): Record<string, unknown> {
  return {
    type: "image",
    source: {
      type: "base64",
      media_type: mimeType,
      data: buffer.toString("base64"),
    },
  };
}
