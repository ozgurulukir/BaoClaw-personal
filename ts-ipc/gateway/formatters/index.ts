export interface ChannelFormatter {
  readonly maxOutputLength: number;
  bold(text: string): string;
  italic(text: string): string;
  code(text: string): string;
  codeBlock(text: string, lang?: string): string;
  link(text: string, url: string): string;
  header(text: string, level?: number): string;
  bullet(text: string): string;
  escape(text: string): string;
  truncate(text: string, maxLength?: number): string;
}

export { plainFormatter } from "./plain.js";
export { markdownFormatter } from "./markdown.js";
export { telegramHtmlFormatter, escapeHtml } from "./telegramHtml.js";
export { whatsappFormatter } from "./whatsappText.js";
