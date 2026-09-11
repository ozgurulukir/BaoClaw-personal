import type { ChannelFormatter } from "./index.js";

export const markdownFormatter: ChannelFormatter = {
  maxOutputLength: 4000,
  bold(text: string): string {
    return `**${text}**`;
  },
  italic(text: string): string {
    return `*${text}*`;
  },
  code(text: string): string {
    return `\`${text}\``;
  },
  codeBlock(text: string, lang = ""): string {
    return `\`\`\`${lang}\n${text}\n\`\`\``;
  },
  link(text: string, url: string): string {
    return `[${text}](${url})`;
  },
  header(text: string, level = 2): string {
    const prefix = "#".repeat(Math.max(1, Math.min(6, level)));
    return `${prefix} ${text}`;
  },
  bullet(text: string): string {
    return `• ${text}`;
  },
  escape(text: string): string {
    return text;
  },
  truncate(text: string, maxLength = 4000): string {
    if (text.length <= maxLength) return text;
    const suffix = "\n\n… [truncated]";
    if (maxLength <= suffix.length) {
      return text.slice(0, maxLength);
    }
    return text.slice(0, maxLength - suffix.length) + suffix;
  },
};
