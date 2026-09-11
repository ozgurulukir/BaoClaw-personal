import type { ChannelFormatter } from "./index.js";

export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

export const telegramHtmlFormatter: ChannelFormatter = {
  maxOutputLength: 4096,
  bold(text: string): string {
    return `<b>${escapeHtml(text)}</b>`;
  },
  italic(text: string): string {
    return `<i>${escapeHtml(text)}</i>`;
  },
  code(text: string): string {
    return `<code>${escapeHtml(text)}</code>`;
  },
  codeBlock(text: string, lang = ""): string {
    const classAttr = lang ? ` class="language-${escapeHtml(lang)}"` : "";
    return `<pre><code${classAttr}>${escapeHtml(text)}</code></pre>`;
  },
  link(text: string, url: string): string {
    return `<a href="${escapeHtml(url)}">${escapeHtml(text)}</a>`;
  },
  header(text: string): string {
    return `<b>${escapeHtml(text)}</b>`;
  },
  bullet(text: string): string {
    return `• ${text}`;
  },
  escape(text: string): string {
    return escapeHtml(text);
  },
  truncate(text: string, maxLength = 4096): string {
    if (text.length <= maxLength) return text;
    const suffix = "\n\n… [truncated]";
    if (maxLength <= suffix.length) {
      return text.slice(0, maxLength);
    }
    return text.slice(0, maxLength - suffix.length) + suffix;
  },
};
