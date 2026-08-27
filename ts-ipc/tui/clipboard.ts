// Universal Terminal Clipboard utility using OSC 52 ANSI escape codes.
// Supports modern terminals (iTerm2, Alacritty, Kitty, WezTerm, VS Code, Windows Terminal).

export function copyToClipboard(text: string): boolean {
  if (!text) return false;
  try {
    // 64KB safe limit for OSC 52 sequence across standard terminal emulators
    const MAX_CLIPBOARD_CHARS = 65536;
    const truncated =
      text.length > MAX_CLIPBOARD_CHARS
        ? text.slice(0, MAX_CLIPBOARD_CHARS)
        : text;
    const base64 = Buffer.from(truncated, "utf-8").toString("base64");
    // OSC 52 sequence: ESC ] 52 ; c ; <base64> BEL
    process.stdout.write(`\x1b]52;c;${base64}\x07`);
    return true;
  } catch {
    return false;
  }
}
