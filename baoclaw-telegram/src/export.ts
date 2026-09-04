/**
 * BaoClaw 对话导出模块 — 将 talkTail RPC 返回的对话条目格式化为 Markdown 及 PDF。
 * 逻辑与 baoclaw-core/src/engine/export.rs 和 baoclaw-web/src/export.ts 保持一致。
 */

import fs from "fs";
import PDFDocument from "pdfkit";

export interface TranscriptEntry {
  role: "user" | "assistant";
  text: string;
  timestamp?: string;
  tools?: { name: string; detail?: string }[];
}

export interface ExportOptions {
  sessionId?: string;
  format?: "markdown" | "pdf";
  includeToolCalls?: boolean;
}

/**
 * Format a list of transcript entries into a Markdown document.
 *
 * Output follows the design spec format:
 * - Title and session metadata
 * - Each message as a section with timestamp
 * - Tool calls listed under assistant messages
 */
export function formatTranscriptToMarkdown(
  entries: TranscriptEntry[],
  options?: ExportOptions,
): string {
  const exportTime = new Date().toLocaleString("sv-SE").replace("T", " ");
  const sessionId = options?.sessionId ?? "未知";
  const includeToolCalls = options?.includeToolCalls ?? true;

  let md = "";

  // Header
  md += "# BaoClaw 对话导出\n";
  md += `**会话**: ${sessionId}\n`;
  md += `**时间**: ${exportTime}\n`;
  md += `**消息数**: ${entries.length}\n`;
  md += "\n---\n\n";

  for (const entry of entries) {
    const ts = entry.timestamp ?? "";

    if (entry.role === "user") {
      md += `## 用户 (${ts})\n`;
      md += entry.text;
      md += "\n";
    } else {
      md += `## 助手 (${ts})\n`;
      md += entry.text;
      md += "\n";

      // Render tool calls if present and enabled
      if (includeToolCalls && entry.tools && entry.tools.length > 0) {
        md += "\n### 工具调用\n";
        for (const tool of entry.tools) {
          const detail = tool.detail ? `: ${tool.detail}` : "";
          md += `- ⚡ ${tool.name}${detail}\n`;
        }
      }
    }

    md += "\n---\n\n";
  }

  return md;
}

/**
 * Generate a default export filename with current date.
 */
export function defaultExportFilename(
  format: "markdown" | "pdf" = "markdown",
): string {
  const now = new Date();
  const pad = (n: number) => n.toString().padStart(2, "0");
  const date = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
  const ext = format === "pdf" ? "pdf" : "md";
  return `baoclaw-export-${date}.${ext}`;
}

function cleanMarkdownFormatting(text: string): string {
  return text
    .replace(/\*\*(.*?)\*\*/g, "$1")
    .replace(/\*(.*?)\*/g, "$1")
    .replace(/`(.*?)`/g, "$1");
}

function tryApplyFont(doc: InstanceType<typeof PDFDocument>): boolean {
  const candidateFonts = [
    "/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    "/usr/share/fonts/truetype/droid/DroidSansFallback.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    "/usr/share/fonts/truetype/arphic/gkai00mp.ttf",
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/STHeiti Light.ttc",
    "C:\\Windows\\Fonts\\msyh.ttc",
    "C:\\Windows\\Fonts\\simhei.ttf",
    "C:\\Windows\\Fonts\\simsun.ttc",
  ];

  for (const fontPath of candidateFonts) {
    if (fs.existsSync(fontPath)) {
      try {
        doc.font(fontPath);
        return true;
      } catch {
        // Try next font candidate
      }
    }
  }

  console.warn(
    "[baoclaw-telegram] Warning: No CJK candidate font found on system; PDF text rendering may omit non-Latin characters.",
  );
  return false;
}

/**
 * Convert Markdown content to a PDF buffer using PDFKit.
 */
export async function markdownToPdf(markdown: string): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const doc = new PDFDocument({
      margin: 50,
      size: "A4",
      info: {
        Title: "BaoClaw Export",
        Creator: "BaoClaw",
      },
    });

    const chunks: Buffer[] = [];
    doc.on("data", (chunk: Buffer) => chunks.push(chunk));
    doc.on("end", () => resolve(Buffer.concat(chunks)));
    doc.on("error", (err: Error) => reject(err));

    tryApplyFont(doc);

    const lines = markdown.split(/\r?\n/);
    let inCodeBlock = false;
    let codeBuffer: string[] = [];

    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];

      if (line.trim().startsWith("```")) {
        if (inCodeBlock) {
          inCodeBlock = false;
          if (codeBuffer.length > 0) {
            doc.moveDown(0.3);
            const codeText = codeBuffer.join("\n");
            doc.fontSize(9).fillColor("#333333").text(codeText, {
              indent: 10,
              lineGap: 2,
            });
            doc.fillColor("#000000").fontSize(10);
            doc.moveDown(0.3);
            codeBuffer = [];
          }
        } else {
          inCodeBlock = true;
          codeBuffer = [];
        }
        continue;
      }

      if (inCodeBlock) {
        codeBuffer.push(line);
        continue;
      }

      if (line.trim() === "---" || line.trim() === "***") {
        doc.moveDown(0.5);
        const y = doc.y;
        doc
          .moveTo(50, y)
          .lineTo(doc.page.width - 50, y)
          .strokeColor("#cccccc")
          .lineWidth(0.8)
          .stroke();
        doc.strokeColor("#000000").lineWidth(1);
        doc.moveDown(0.5);
        continue;
      }

      if (line.startsWith("# ")) {
        doc.moveDown(0.5);
        doc
          .fontSize(18)
          .fillColor("#111111")
          .text(cleanMarkdownFormatting(line.slice(2)));
        doc.moveDown(0.5);
        doc.fontSize(10).fillColor("#000000");
        continue;
      }

      if (line.startsWith("## ")) {
        doc.moveDown(0.5);
        doc
          .fontSize(14)
          .fillColor("#1a5fb4")
          .text(cleanMarkdownFormatting(line.slice(3)));
        doc.moveDown(0.3);
        doc.fontSize(10).fillColor("#000000");
        continue;
      }

      if (line.startsWith("### ")) {
        doc.moveDown(0.4);
        doc
          .fontSize(12)
          .fillColor("#2e3440")
          .text(cleanMarkdownFormatting(line.slice(4)));
        doc.moveDown(0.2);
        doc.fontSize(10).fillColor("#000000");
        continue;
      }

      if (line.trim().startsWith("- ") || line.trim().startsWith("* ")) {
        const itemText = line.trim().slice(2);
        doc
          .fontSize(10)
          .fillColor("#222222")
          .text(`• ${cleanMarkdownFormatting(itemText)}`, {
            indent: 15,
            lineGap: 2,
          });
        continue;
      }

      if (line.trim() === "") {
        doc.moveDown(0.3);
        continue;
      }

      const cleanLine = cleanMarkdownFormatting(line);
      doc.fontSize(10).fillColor("#222222").text(cleanLine, {
        lineGap: 3,
      });
    }

    doc.end();
  });
}
