import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { formatForFeishu } from "./formatter.js";

describe("formatForFeishu", () => {
  test("returns empty string when input is empty or falsy", () => {
    assert.strictEqual(formatForFeishu(""), "");
    // @ts-expect-error testing falsy runtime input
    assert.strictEqual(formatForFeishu(null), "");
    // @ts-expect-error testing falsy runtime input
    assert.strictEqual(formatForFeishu(undefined), "");
  });

  test("converts supported HTML formatting", () => {
    assert.strictEqual(
      formatForFeishu("<strong>bold</strong><br><em>italic</em>"),
      "**bold**\n*italic*",
    );
  });

  test("converts details/summary HTML blocks", () => {
    const withBody = formatForFeishu(
      "<details><summary>Click me</summary>Hidden details content</details>",
    );
    assert.strictEqual(withBody, "📋 **Click me**\n\nHidden details content\n\n---");

    const withoutBody = formatForFeishu(
      "<details><summary>Empty section</summary></details>",
    );
    assert.strictEqual(withoutBody, "📋 **Empty section**");
  });

  test("converts HTML tables to an ASCII table", () => {
    const result = formatForFeishu(
      "<table><tr><th>Name</th><th>Count</th></tr><tr><td>jobs</td><td>2</td></tr></table>",
    );
    assert.match(result, /\| Name \| Count \|/);
    assert.match(result, /\| jobs \| 2\s+\|/);
  });

  test("falls back to simple row format for wide HTML tables", () => {
    const wideHeader = "<th>" + "A".repeat(40) + "</th><th>" + "B".repeat(40) + "</th>";
    const wideRow = "<td>" + "C".repeat(40) + "</td><td>" + "D".repeat(40) + "</td>";
    const wideTable = `<table><tr>${wideHeader}</tr><tr>${wideRow}</tr></table>`;

    const result = formatForFeishu(wideTable);
    assert.strictEqual(result, `${"A".repeat(40)} | ${"B".repeat(40)}\n${"C".repeat(40)} | ${"D".repeat(40)}`);
  });

  test("converts links and images to markdown", () => {
    assert.strictEqual(
      formatForFeishu('<a href="https://example.com">Example</a>'),
      "[Example](https://example.com)",
    );

    assert.strictEqual(
      formatForFeishu('<img src="https://example.com/a.png" alt="An image" />'),
      "![An image](https://example.com/a.png)",
    );

    assert.strictEqual(
      formatForFeishu('<img alt="Alt first" src="https://example.com/b.png" />'),
      "![Alt first](https://example.com/b.png)",
    );
  });

  test("converts lists and blockquotes to markdown", () => {
    assert.strictEqual(
      formatForFeishu("<ul><li>First</li><li>Second</li></ul>").replace(/\s+/g, " "),
      "- First - Second",
    );

    assert.strictEqual(
      formatForFeishu("<blockquote>Quote line 1\nQuote line 2</blockquote>"),
      "> Quote line 1\n> Quote line 2",
    );
  });

  test("strips unsupported tags while preserving content", () => {
    assert.strictEqual(
      formatForFeishu("<script>alert(1)</script><p>Hello</p>"),
      "alert(1)\nHello",
    );
  });

  test("decodes HTML entities and preserves code blocks", () => {
    const code = "```js\nconst value = '<b>&amp;</b>';\n```";
    assert.strictEqual(
      formatForFeishu(`${code}\n&amp;`),
      "```js\nconst value = '<b>&</b>';\n```\n&",
    );
  });

  test("cleans up excessive whitespace and newlines", () => {
    const input = "Line 1   \n\n\n\nLine 2\n   ";
    assert.strictEqual(formatForFeishu(input), "Line 1\n\nLine 2");
  });

  test("limits output to Feishu's message size", () => {
    assert.strictEqual(formatForFeishu("x".repeat(15001)).length, 15000);
  });
});
