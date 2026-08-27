/**
 * ANSI Color and styling helpers for BaoClaw CLI
 */
export const noColor = Boolean(
  process.env.NO_COLOR || process.env.TERM === "dumb",
);
export const ESC = noColor ? "" : "\x1b[";
export const RESET = noColor ? "" : `${ESC}0m`;
export const BOLD = noColor ? "" : `${ESC}1m`;
export const DIM = noColor ? "" : `${ESC}2m`;
export const ITALIC = noColor ? "" : `${ESC}3m`;
export const UNDERLINE = noColor ? "" : `${ESC}4m`;

// Colors (optimized for dark terminal backgrounds)
export const FG_ORANGE = noColor ? "" : `${ESC}38;2;217;119;40m`; // BaoClaw orange
export const FG_CYAN = noColor ? "" : `${ESC}96m`; // bright cyan
export const FG_GREEN = noColor ? "" : `${ESC}92m`; // bright green
export const FG_YELLOW = noColor ? "" : `${ESC}93m`; // bright yellow
export const FG_RED = noColor ? "" : `${ESC}91m`; // bright red
export const FG_MAGENTA = noColor ? "" : `${ESC}95m`; // bright magenta
export const FG_BLUE = noColor ? "" : `${ESC}94m`; // bright blue
export const FG_WHITE = noColor ? "" : `${ESC}97m`; // bright white
export const FG_GRAY = noColor ? "" : `${ESC}38;2;160;160;160m`; // lighter gray (visible on dark bg)
export const FG_BRIGHT_WHITE = noColor ? "" : `${ESC}97m`;
export const BG_DARK = noColor ? "" : `${ESC}48;2;30;30;30m`;

// Clawd body color (warm tan/beige)
export const FG_CLAWD = noColor ? "" : `${ESC}38;2;210;180;140m`;
export const BG_CLAWD = noColor ? "" : `${ESC}48;2;60;50;40m`;
