import type { ChannelFormatter } from "./index.js";

export const plainFormatter: ChannelFormatter = {
  maxOutputLength: 4000,
  bold(text: string): string {
    return text;
  },
  italic(text: string): string {
    return text;
  },
  code(text: string): string {
    return text;
  },
  codeBlock(text: string): string {
    return text;
  },
  link(text: string, url: string): string {
    return `${text} (${url})`;
  },
  header(text: string): string {
    return `=== ${text} ===`;
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
