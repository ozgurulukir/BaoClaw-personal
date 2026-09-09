import { describe, test } from "node:test";
import assert from "node:assert/strict";
import {
  formatSearchResults,
  type FormatSearchResultsOptions,
  type SearchResult,
} from "./search.js";

const opts: FormatSearchResultsOptions = {
  maxChars: 4000,
  emptyMessage: (q) => `empty:${q}`,
  header: (q, n) => `head:${q}:${n}\n\n`,
  userLabel: "👤",
  assistantLabel: "🤖",
};

describe("shared formatSearchResults", () => {
  test("empty results delegate to emptyMessage", () => {
    assert.equal(formatSearchResults([], "zzz", opts), "empty:zzz");
    assert.equal(
      formatSearchResults(undefined as never, "zzz", opts),
      "empty:zzz",
    );
  });

  test("renders DB-path rows without a role label", () => {
    const rows: SearchResult[] = [
      {
        snippet: "the indexer writes to FTS5",
        timestamp: "2026-09-09T10:00:12Z",
        session_id: "s-1",
        cwd: "/tmp/proj",
      },
    ];
    const out = formatSearchResults(rows, "FTS", opts);
    assert.match(out, /head:FTS:1/);
    assert.match(out, /\[2026-09-09 10:00:12\]\nthe indexer writes to FTS5/);
    assert.doesNotMatch(out, /👤|🤖/);
  });

  test("renders fallback rows with role labels and prefers snippet over text", () => {
    const rows: SearchResult[] = [
      {
        role: "user",
        snippet: "hi there",
        text: "ignored",
        timestamp: "2026-09-09T10:00:00Z",
      },
      {
        role: "assistant",
        text: "assistant prose only",
        timestamp: "2026-09-09T10:00:01Z",
      },
    ];
    const out = formatSearchResults(rows, "q", opts);
    assert.match(out, /\[2026-09-09 10:00:00\] 👤\nhi there/);
    assert.match(out, /\[2026-09-09 10:00:01\] 🤖\nassistant prose only/);
    assert.doesNotMatch(out, /ignored/);
  });

  test("appends the truncation marker past maxChars and stops", () => {
    const rows: SearchResult[] = [
      { snippet: "a".repeat(3000), timestamp: "2026-09-09T10:00:00Z" },
      { snippet: "second row", timestamp: "2026-09-09T10:00:01Z" },
    ];
    const out = formatSearchResults(rows, "q", { ...opts, maxChars: 200 });
    assert.match(out, /…$/);
    assert.doesNotMatch(out, /second row/);
  });

  test("truncatedMarker is configurable", () => {
    const rows: SearchResult[] = [
      { snippet: "x".repeat(300), timestamp: "2026-09-09T10:00:00Z" },
    ];
    const out = formatSearchResults(rows, "q", {
      ...opts,
      maxChars: 100,
      truncatedMarker: "…(more results truncated)",
    });
    assert.match(out, /…\(more results truncated\)$/);
  });
});
