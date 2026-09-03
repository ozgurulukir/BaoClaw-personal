import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { isOriginAllowed } from "./origin.js";

describe("isOriginAllowed", () => {
  describe("allows non-browser clients", () => {
    test("missing Origin header", () => {
      assert.equal(isOriginAllowed(undefined, "127.0.0.1:8080"), true);
    });

    test("empty Origin header", () => {
      assert.equal(isOriginAllowed("", "127.0.0.1:8080"), true);
    });

    test("missing Origin even when Host is also missing", () => {
      assert.equal(isOriginAllowed(undefined, undefined), true);
    });
  });

  describe("allows same-origin upgrades", () => {
    test("exact host match", () => {
      assert.equal(isOriginAllowed("http://localhost:8080", "localhost:8080"), true);
    });

    test("LAN IP host match", () => {
      assert.equal(
        isOriginAllowed("http://192.168.1.5:8080", "192.168.1.5:8080"),
        true,
      );
    });

    test("loopback equivalence: localhost vs 127.0.0.1", () => {
      assert.equal(isOriginAllowed("http://localhost:8080", "127.0.0.1:8080"), true);
      assert.equal(isOriginAllowed("http://127.0.0.1:8080", "localhost:8080"), true);
    });

    test("loopback equivalence: ::1", () => {
      assert.equal(isOriginAllowed("http://[::1]:8080", "[::1]:8080"), true);
      assert.equal(isOriginAllowed("http://localhost:8080", "[::1]:8080"), true);
    });

    test("scheme-default ports omitted on both sides", () => {
      // Page at https://example.com upgrades wss://example.com — both Origin
      // and Host omit the default port.
      assert.equal(isOriginAllowed("https://example.com", "example.com"), true);
    });

    test("case-insensitive hostnames", () => {
      assert.equal(isOriginAllowed("http://LOCALHOST:8080", "localhost:8080"), true);
    });
  });

  describe("rejects cross-origin upgrades", () => {
    test("different hostname", () => {
      assert.equal(isOriginAllowed("http://evil.com:8080", "127.0.0.1:8080"), false);
    });

    test("different port", () => {
      assert.equal(isOriginAllowed("http://localhost:9090", "localhost:8080"), false);
    });

    test("loopback does not equate to remote host", () => {
      assert.equal(
        isOriginAllowed("http://localhost:8080", "192.168.1.5:8080"),
        false,
      );
    });

    test("malformed Origin header", () => {
      assert.equal(isOriginAllowed("not a url", "localhost:8080"), false);
    });

    test("non-http(s) scheme", () => {
      assert.equal(isOriginAllowed("file:///etc/passwd", "localhost:8080"), false);
      assert.equal(isOriginAllowed("javascript:alert(1)", "localhost:8080"), false);
    });

    test("Origin present but Host missing", () => {
      assert.equal(isOriginAllowed("http://localhost:8080", undefined), false);
    });
  });

  describe("boundary: attacker-controlled hostnames that match the Host header", () => {
    test("DNS-rebinding style equal non-loopback host passes the Origin check", () => {
      // A rebound DNS name makes Origin == Host, so this check alone cannot
      // reject it — the auth token remains the primary control there. This
      // test documents the deliberate boundary of this module.
      assert.equal(
        isOriginAllowed("http://ddns.example:8080", "ddns.example:8080"),
        true,
      );
    });
  });
});
