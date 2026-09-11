import { describe, it } from "node:test";
import * as assert from "node:assert/strict";
import {
  plainFormatter,
  markdownFormatter,
  telegramHtmlFormatter,
  whatsappFormatter,
  escapeHtml,
} from "./formatters/index.js";

describe("Gateway Formatters", () => {
  it("plainFormatter preserves text without markup", () => {
    assert.equal(plainFormatter.bold("hello"), "hello");
    assert.equal(plainFormatter.italic("world"), "world");
    assert.equal(plainFormatter.code("foo()"), "foo()");
    assert.equal(
      plainFormatter.link("BaoClaw", "https://example.com"),
      "BaoClaw (https://example.com)",
    );
    assert.equal(plainFormatter.header("Title"), "=== Title ===");
    assert.equal(plainFormatter.bullet("Item"), "• Item");
  });

  it("markdownFormatter applies standard markdown", () => {
    assert.equal(markdownFormatter.bold("hello"), "**hello**");
    assert.equal(markdownFormatter.italic("world"), "*world*");
    assert.equal(markdownFormatter.code("foo()"), "`foo()`");
    assert.equal(
      markdownFormatter.codeBlock("let x = 1;", "js"),
      "```js\nlet x = 1;\n```",
    );
    assert.equal(
      markdownFormatter.link("BaoClaw", "https://example.com"),
      "[BaoClaw](https://example.com)",
    );
    assert.equal(markdownFormatter.header("Title", 2), "## Title");
  });

  it("telegramHtmlFormatter escapes HTML and uses supported tags", () => {
    assert.equal(
      escapeHtml("<script>&\"'</script>"),
      "&lt;script&gt;&amp;&quot;'&lt;/script&gt;",
    );
    assert.equal(
      telegramHtmlFormatter.bold("hello & goodbye"),
      "<b>hello &amp; goodbye</b>",
    );
    assert.equal(
      telegramHtmlFormatter.italic("italic <tag>"),
      "<i>italic &lt;tag&gt;</i>",
    );
    assert.equal(telegramHtmlFormatter.code("foo()"), "<code>foo()</code>");
    assert.equal(
      telegramHtmlFormatter.codeBlock("let x = 1;", "ts"),
      '<pre><code class="language-ts">let x = 1;</code></pre>',
    );
    assert.equal(
      telegramHtmlFormatter.link("Site", "https://example.com?a=1&b=2"),
      '<a href="https://example.com?a=1&amp;b=2">Site</a>',
    );
  });

  it("whatsappFormatter uses whatsapp-specific markers", () => {
    assert.equal(whatsappFormatter.bold("hello"), "*hello*");
    assert.equal(whatsappFormatter.italic("world"), "_world_");
    assert.equal(whatsappFormatter.code("foo()"), "`foo()`");
    assert.equal(whatsappFormatter.codeBlock("code"), "```\ncode\n```");
    assert.equal(
      whatsappFormatter.link("Link", "https://example.com"),
      "Link (https://example.com)",
    );
  });

  it("truncate limits output when string exceeds maximum", () => {
    const shortText = "abc";
    assert.equal(plainFormatter.truncate(shortText, 10), "abc");

    const longText = "a".repeat(100);
    const truncated = plainFormatter.truncate(longText, 50);
    assert.ok(truncated.length <= 50);
    assert.match(truncated, /\[truncated\]/);
  });
});
