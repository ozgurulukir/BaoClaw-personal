/**
 * Media helpers for the Telegram gateway: Markdown → Telegram HTML
 * conversion and base64 image extraction from model/tool output.
 */

// ═══════════════════════════════════════════════════════════════
// Markdown → Telegram HTML converter
// ═══════════════════════════════════════════════════════════════

/**
 * Convert markdown-like text to Telegram-safe HTML.
 * Escapes raw HTML first, then applies safe formatting tags.
 */
export function markdownToTelegramHtml(text: string): string {
  // 1. Escape HTML entities first (so raw model HTML doesn't break Telegram)
  let html = text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");

  // 2. Code blocks: ```lang\n...\n``` → <pre><code class="language-lang">...</code></pre>
  html = html.replace(/```(\w*)\n([\s\S]*?)```/g, (_m, lang, code) => {
    const cls = lang ? ` class="language-${lang}"` : "";
    return `<pre><code${cls}>${code.trimEnd()}</code></pre>`;
  });

  // 3. Inline code: `code` → <code>code</code>
  html = html.replace(/`([^`\n]+)`/g, "<code>$1</code>");

  // 4. Bold: **text** → <b>text</b>
  html = html.replace(/\*\*(.+?)\*\*/g, "<b>$1</b>");

  // 5. Italic: *text* → <i>text</i> (but not inside bold)
  html = html.replace(/(?<!\*)\*(?!\*)(.+?)(?<!\*)\*(?!\*)/g, "<i>$1</i>");

  // 6. Strikethrough: ~~text~~ → <s>text</s>
  html = html.replace(/~~(.+?)~~/g, "<s>$1</s>");

  // 7. Links: [text](url) → <a href="url">text</a>
  html = html.replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2">$1</a>');

  return html;
}

// ═══════════════════════════════════════════════════════════════
// Base64 image extraction
// ═══════════════════════════════════════════════════════════════
export interface ExtractedImage {
  buffer: Buffer;
  caption?: string;
}

export function extractBase64Images(text: string): {
  text: string;
  images: ExtractedImage[];
} {
  const images: ExtractedImage[] = [];
  let cleaned = text;

  // 1. Markdown image syntax: ![alt](data:image/...;base64,...)
  const mdImgRegex =
    /!\[([^\]]*)\]\(data:image\/(png|jpeg|jpg|gif|webp);base64,([A-Za-z0-9+/=\s]+)\)/g;
  let match: RegExpExecArray | null;
  while ((match = mdImgRegex.exec(text)) !== null) {
    try {
      const base64Data = match[3].replace(/\s/g, "");
      const buffer = Buffer.from(base64Data, "base64");
      if (buffer.length > 100) {
        images.push({ buffer, caption: match[1] || undefined });
      }
    } catch {
      /* skip */
    }
  }
  cleaned = cleaned.replace(mdImgRegex, "");

  // 2. MCP content format: {"type":"image","data":"base64...","mimeType":"image/png"}
  // Also handles arrays: [{"type":"image",...}]
  try {
    const parsed = JSON.parse(cleaned);
    const contents = Array.isArray(parsed?.content)
      ? parsed.content
      : Array.isArray(parsed)
        ? parsed
        : [];
    for (const item of contents) {
      if (item?.type === "image" && item?.data) {
        try {
          const buffer = Buffer.from(item.data, "base64");
          if (buffer.length > 100) {
            images.push({ buffer, caption: "📸 Screenshot" });
          }
        } catch {
          /* skip */
        }
      }
    }
    if (images.length > 0 && contents.length > 0) {
      // Extract text content from MCP response
      const textParts = contents
        .filter((c: any) => c?.type === "text")
        .map((c: any) => c.text || "");
      cleaned = textParts.join("\n");
    }
  } catch {
    /* not JSON, continue */
  }

  // 3. Standalone data URIs not in markdown syntax
  const dataUriRegex =
    /data:image\/(png|jpeg|jpg|gif|webp);base64,([A-Za-z0-9+/=\s]+)/g;
  while ((match = dataUriRegex.exec(cleaned)) !== null) {
    try {
      const base64Data = match[2].replace(/\s/g, "");
      const buffer = Buffer.from(base64Data, "base64");
      if (buffer.length > 100) {
        images.push({ buffer });
      }
    } catch {
      /* skip */
    }
  }
  cleaned = cleaned.replace(dataUriRegex, "[image]");

  // 4. Clean up very long base64 blocks that might have been missed
  cleaned = cleaned.replace(/[A-Za-z0-9+/=]{500,}/g, "[image data]");

  // 5. Clean up empty markdown image remnants
  cleaned = cleaned
    .replace(/!\[\]\(\)/g, "")
    .replace(/!\[[^\]]*\]\(\s*\)/g, "");

  return { text: cleaned.trim(), images };
}
