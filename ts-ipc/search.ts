/**
 * Search-result formatting shared by every /search surface (Telegram,
 * WhatsApp, Feishu). Pure formatting: no I/O and no daemon knowledge — the
 * shape comes from the `searchHistory` RPC (DB-path rows carry
 * snippet/timestamp/session_id/cwd; the active-session fallback shape
 * carries role/text).
 *
 * Platform differences (header wording, labels, char limits, truncation
 * marker) are passed in as options so the row logic lives in exactly one
 * place.
 */

/** One row of the `searchHistory` RPC response. */
export interface SearchResult {
  timestamp?: string;
  snippet?: string;
  /** Present in the active-session fallback shape (DB unavailable). */
  role?: string;
  text?: string;
  session_id?: string;
  cwd?: string;
}

export interface FormatSearchResultsOptions {
  /** Hard cap on output length; the truncation marker is appended past it. */
  maxChars: number;
  /** Message shown when there are no results. */
  emptyMessage: (query: string) => string;
  /** Header line above the rows. */
  header: (query: string, count: number) => string;
  /** Label for fallback rows with role "user". */
  userLabel: string;
  /** Label for fallback rows with role "assistant". */
  assistantLabel: string;
  /** Marker appended when output is truncated (default "…"). */
  truncatedMarker?: string;
}

export function formatSearchResults(
  results: SearchResult[],
  query: string,
  options: FormatSearchResultsOptions,
): string {
  if (!results || results.length === 0) return options.emptyMessage(query);
  let out = options.header(query, results.length);
  for (const r of results) {
    const ts = r.timestamp?.slice(0, 19).replace("T", " ") || "";
    // DB-path hits carry no role — render without a label rather than
    // guessing one.
    const role =
      r.role === "user"
        ? options.userLabel
        : r.role === "assistant"
          ? options.assistantLabel
          : "";
    const body = r.snippet || r.text || "";
    out += `[${ts}]${role ? ` ${role}` : ""}\n${body}\n\n`;
    if (out.length > options.maxChars) {
      out += options.truncatedMarker ?? "…";
      break;
    }
  }
  return out;
}
