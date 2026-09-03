/**
 * Origin validation for WebSocket upgrade requests.
 *
 * Browsers always attach an Origin header to WebSocket connections, so a
 * cross-site page (or a DNS-rebinding page) is identifiable by an Origin host
 * that differs from the host the client actually connected to. Non-browser
 * clients (daemon integrations, curl-based tooling) send no Origin header and
 * must not be rejected.
 */

interface HostParts {
  hostname: string;
  port: string;
}

/** Split a `Host`-style header value (`hostname[:port]`, IPv6 in brackets). */
function splitHost(host: string): HostParts {
  if (host.startsWith("[")) {
    const end = host.indexOf("]");
    if (end !== -1) {
      const rest = host.slice(end + 1);
      return {
        hostname: host.slice(1, end),
        port: rest.startsWith(":") ? rest.slice(1) : "",
      };
    }
  }
  const idx = host.lastIndexOf(":");
  if (idx === -1) return { hostname: host, port: "" };
  return { hostname: host.slice(0, idx), port: host.slice(idx + 1) };
}

function isLoopbackHostname(hostname: string): boolean {
  const h = hostname.toLowerCase();
  return h === "localhost" || h === "::1" || /^127(\.\d{1,3}){3}$/.test(h);
}

/**
 * Whether a WebSocket upgrade request may proceed based on its Origin header.
 *
 * Allowed when:
 *  - no Origin header is present (non-browser client), or
 *  - the Origin host equals the request's Host header host, where
 *    `localhost` / `127.0.0.1` / `::1` are treated as equivalent loopback
 *    names and an omitted port is normalized to the scheme default.
 *
 * This is defense-in-depth on top of the auth token: it blocks cross-site
 * pages from opening the socket even when they have obtained a token.
 */
export function isOriginAllowed(
  origin: string | undefined,
  requestHost: string | undefined,
): boolean {
  if (origin === undefined || origin === "") return true;
  if (!requestHost) return false;

  let originUrl: URL;
  try {
    originUrl = new URL(origin);
  } catch {
    return false;
  }
  if (originUrl.protocol !== "http:" && originUrl.protocol !== "https:") {
    return false;
  }

  const originParts = {
    // URL.hostname keeps brackets for IPv6 literals ([::1]) — strip them so
    // the loopback comparison matches the bracketless Host-side form.
    hostname: originUrl.hostname.replace(/^\[|\]$/g, "").toLowerCase(),
    // Compare ports as-written: browsers omit the scheme-default port from
    // both Origin and Host, so "omitted on both sides" is a match.
    port: originUrl.port,
  };
  const requestParts = splitHost(requestHost);
  const requestHostname = requestParts.hostname.toLowerCase();

  if (originParts.port !== requestParts.port) return false;

  if (originParts.hostname === requestHostname) return true;
  return (
    isLoopbackHostname(originParts.hostname) &&
    isLoopbackHostname(requestHostname)
  );
}
