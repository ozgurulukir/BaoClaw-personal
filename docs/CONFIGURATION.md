# Configuration Reference

> Extracted from the root README — see [README.md](../README.md) for the project overview.

### Directory Structure

```
~/.baoclaw/                          # User-level (global, cross-project)
├── config.json                      # Main configuration
├── memory.jsonl                     # Global long-term memory
├── USER.md                         # Persistent user profile (injected into the system prompt)
├── cron.json                        # Scheduled tasks
├── sessions/                        # Session transcripts (per-project)
│   └── {cwd_hash}-{uuid}.jsonl
├── skills/                          # Personal skills (cross-project)
│   └── my-skill.md
├── plugins/                         # User-level plugins
│   └── my-plugin/
│       ├── skills/
│       └── mcp.json
├── mcp.json                         # User-level MCP servers (executed by the daemon: stdio, http, sse)
├── models/                          # Local model files (whisper etc.)
│   └── ggml-base.bin
├── telemetry.db                    # Local telemetry (SQLite; turns + sessions)
├── cross_session.db                # Cross-session search index (SQLite FTS5; backfilled from session snapshots at startup)
├── evolution/                       # Self-evolution data
│   ├── trajectories.jsonl           # Interaction history for RLHF
│   ├── candidates/                  # Auto-extracted skill candidates
│   └── training_export.jsonl        # Exported training data
├── telegram-gateway.pid             # Telegram gateway PID file
└── telegram-gateway.log             # Telegram gateway log

<project>/.baoclaw/                  # Project-level
├── BAOCLAW.md                       # Project instructions → system prompt
├── mcp.json                         # Project MCP servers
├── mcp.local.json                   # Local MCP overrides (gitignored)
├── memory.jsonl                     # Project-level memories
├── skills/                          # Project-specific skills
├── plugins/                         # Project-level plugins
├── backups/                         # File backups before edits
└── todo.json                        # Project todo list
```

### `~/.baoclaw/config.json` — Main Configuration

The recommended format uses **named model profiles** — each profile carries its
own API key, base URL, and context window:

```json
{
  "model_profiles": {
    "glm52": {
      "model": "glm-5.2",
      "api_type": "anthropic",
      "api_key": "sk-...",
      "base_url": "https://open.bigmodel.cn/api/anthropic",
      "context_window": 1000000
    },
    "haiku": {
      "model": "claude-3-5-haiku-20241022",
      "api_type": "anthropic",
      "api_key": "sk-ant-..."
    }
  },
  "primary_profile": "glm52",
  "fallback_profiles": ["haiku"],
  "telegram": {
    "token": "<telegram-bot-token>",
    "allowedChatIds": [12345678]
  },
  "feishu": {
    "allowedChatIds": ["oc_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"]
  },
  "permissions": {}
}
```

| Field                                                                     | Type          | Default  | Description                                                                                                                                                                                                                                       |
| ------------------------------------------------------------------------- | ------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `model_profiles.<name>`                                                   | object        | `{}`     | Named profile: `model`, `api_type`, `api_key?`, `base_url?`                                                                                                                                                                                       |
| `model_profiles.<name>.context_window`                                    | number        | `200000` | Context window in tokens                                                                                                                                                                                                                          |
| `model_profiles.<name>.auto_compact_threshold_ratio`                      | number        | `0.7`    | Auto-compact at this fraction of the window                                                                                                                                                                                                       |
| `model_profiles.<name>.max_retries_per_model`                             | number        | `2`      | Retries before falling back                                                                                                                                                                                                                       |
| `primary_profile`                                                         | string        | —        | Profile used by default                                                                                                                                                                                                                           |
| `fallback_profiles`                                                       | string[]      | `[]`     | Profiles to try when the primary fails                                                                                                                                                                                                            |
| `context_window`                                                          | number        | `200000` | Top-level default (flat format)                                                                                                                                                                                                                   |
| `auto_compact_threshold_ratio`                                            | number        | `0.7`    | Top-level default (flat format)                                                                                                                                                                                                                   |
| `tool_output_threshold_chars`                                             | number        | `200000` | Global cap on a single tool output (chars). The executor applies the smaller of this and the tool's own limit; oversized output is persisted to disk                                                                                              |
| `max_turns`                                                               | number        | `null`   | Hard cap on agent turns per query (null = unbounded; the loop stops when the model stops calling tools). Headless engines keep their own tighter limits                                                                                           |
| `max_budget_usd`                                                          | number        | `null`   | Per-query cost ceiling in USD. One grace call is allowed at the limit, then the query stops with a `budget_exceeded` error. Cron jobs always cap at 0.5                                                                                           |
| `max_tokens`                                                              | number        | `16384`  | Output token cap sent with every model request                                                                                                                                                                                                    |
| `bash_max_timeout_ms`                                                     | number        | `300000` | Hard ceiling for a single Bash call; a tool-call `timeout` above it is clamped down                                                                                                                                                               |
| `max_parallel_tools`                                                      | number        | `8`      | Cap on how many concurrency-safe tools run in parallel per turn; excess requests queue                                                                                                                                                            |
| `micro_compact_min_age_secs`                                              | number        | `86400`  | Minimum age (seconds) of a tool result before micro-compact may replace it with a placeholder. Lower (e.g. `3600`) for aggressive clearing                                                                                                        |
| `micro_compact_min_chars`                                                 | number        | `8192`   | Minimum serialized size (chars) of a tool result before micro-compact may clear it. Only results BOTH older than the age threshold AND larger than this are cleared                                                                               |
| `telemetry_enabled`                                                       | boolean       | `true`   | Master switch for telemetry recording; `false` drops events instead of writing to `telemetry.db`. Toggle at runtime with `/telemetry on\|off`                                                                                                     |
| `mcp_enabled`                                                             | boolean       | `true`   | MCP client master switch; `false` skips MCP discovery entirely and no MCP tools are registered                                                                                                                                                    |
| `mcp_startup_timeout_secs`                                                | number        | `10`     | Seconds budgeted to EACH MCP handshake step (initialize, tools/list page); a hung server cannot stall daemon boot beyond a bounded multiple of this                                                                                               |
| `mcp_call_timeout_secs`                                                   | number        | `300`    | Seconds budgeted to a single MCP tool call                                                                                                                                                                                                        |
| `mcp_max_restarts`                                                        | number        | `10`     | Reconnect attempts for a down MCP server (exponential backoff 1s→60s) before the slot parks; `/mcp refresh` revives it with a fresh budget                                                                                                        |
| `mcp_deferred_tools`                                                      | boolean       | `true`   | Register MCP tools as deferred stubs (placeholder schema) that expand to the full schema for the rest of a query once the model calls the tool or finds it via tool search                                                                        |
| `permissions`                                                             | object        | —        | Tool permission rules and knobs — see [PERMISSIONS.md](PERMISSIONS.md)                                                                                                                                                                            |
| `telegram.token`                                                          | string        | —        | Telegram bot token from @BotFather                                                                                                                                                                                                                |
| `telegram.allowedChatIds`                                                 | number[]      | `[]`     | Allowed chat IDs (required; empty = reject all and refuse startup)                                                                                                                                                                                |
| `feishu.allowedChatIds`                                                   | string[]      | `[]`     | Allowed Feishu chat IDs (required; empty = reject all and refuse startup)                                                                                                                                                                         |
| `whatsapp.enabled`                                                        | boolean       | `false`  | Start the WhatsApp gateway                                                                                                                                                                                                                        |
| `whatsapp.phoneNumber`                                                    | string        | —        | Bot's own phone number (E.164)                                                                                                                                                                                                                    |
| `whatsapp.allowFrom`                                                      | string[]      | `[]`     | Allowed sender numbers, E.164 (empty = reject all)                                                                                                                                                                                                |
| `whatsapp.dmPolicy` / `whatsapp.groupPolicy`                              | string        | —        | `allow` / `ignore` per conversation type (defaults: dm `allow`, group `ignore`)                                                                                                                                                                   |
| `whatsapp.maxQueueSize`                                                   | number        | —        | Per-chat message queue bound                                                                                                                                                                                                                      |
| `whatsapp.mediaEnabled` / `whatsapp.mediaMaxSizeMb`                       | bool / number | —        | Inbound media handling                                                                                                                                                                                                                            |
| `whatsapp.reconnectMaxMs` / `whatsapp.proxy` / `whatsapp.sharedSessionId` | —             | —        | Reconnect backoff cap, proxy URL, daemon session tag                                                                                                                                                                                              |
| `web.token`                                                               | string        | random   | Web UI auth token (`BAOCLAW_WEB_TOKEN` env overrides)                                                                                                                                                                                             |
| `memory.*`                                                                | object        | —        | Long-term memory decay: `decay_rate`, `recall_boost`, `confirm_boost`, `reject_penalty`, `archive_threshold`, `max_entries`, `cleanup_interval_hours`, `prompt_char_budget` (chars allowed in the always-on memory prompt fragment, default 6000) |

The legacy **flat format** still works and is auto-migrated to a `"primary"`
profile (plus `fallback_N` profiles) on load:

```json
{
  "model": "claude-sonnet-4-20250514",
  "fallback_models": ["claude-3-5-haiku-20241022"],
  "max_retries_per_model": 2,
  "api_type": "anthropic",
  "openai_base_url": null
}
```

Per-profile `api_key` is only expressible in the profiles format. The
per-profile `max_retries_per_model` is synced into the top-level field the
fallback chain reads; the top-level `tool_output_threshold_chars` caps every
tool result unless a tool declares a tighter limit.

`~/.baoclaw/warmup.json` (auto-created) stores context-warmup tuning.

WhatsApp session credentials are stored under `~/.baoclaw/whatsapp-auth/`
(the directory restricted to the owner `0700`, credential files `0600`).

### `mcp.json` — MCP servers

MCP servers are configured in `~/.baoclaw/mcp.json` (user scope),
`<cwd>/.baoclaw/mcp.json` (project), `<cwd>/.baoclaw/mcp.local.json`
(gitignored overrides), and `plugins/*/mcp.json` in either scope. The first
definition of a name wins. At boot the daemon connects every enabled server —
spawning `stdio` processes and reaching `http` (Streamable HTTP) / `sse`
(legacy HTTP+SSE) endpoints — and registers their tools as
`mcp__<server>__<tool>` in a live catalog: `notifications/tools/list_changed`
and `/mcp refresh [server]` re-fetch catalogs and republish without a
restart.

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "some-fs-server", "/home/me/projects"],
      "env": { "SOME_TOKEN": "..." }
    },
    "remote": {
      "type": "http",
      "url": "https://mcp.example.com/mcp",
      "headers": { "Authorization": "Bearer ..." }
    },
    "legacy": { "url": "http://localhost:8080/sse", "disabled": true }
  }
}
```

| Field      | Type     | Default                              | Description                                                                                                                            |
| ---------- | -------- | ------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------- |
| `command`  | string   | —                                    | Executable to spawn (required for stdio servers)                                                                                       |
| `args`     | string[] | `[]`                                 | Command-line arguments                                                                                                                 |
| `env`      | object   | `{}`                                 | Extra environment for the server process. Values are secrets: never logged, never sent to clients                                      |
| `headers`  | object   | `{}`                                 | HTTP headers for url-based transports (e.g. Authorization). Secret-handled like `env`; reserved transport headers cannot be overridden |
| `type`     | string   | `stdio` (or `sse` when `url` is set) | Transport: `stdio`, `http` (Streamable HTTP), or `sse` (legacy HTTP+SSE)                                                               |
| `disabled` | boolean  | `false`                              | Skip the server entirely                                                                                                               |

Every MCP tool call goes through the standard permission pipeline (default
Ask; whole-tool allow rules work by the `mcp__<server>__<tool>` name). Tools
register as deferred stubs by default (`mcp_deferred_tools`) and expand to
their full schema for the rest of a query once used or surfaced by tool
search. See also the `mcp_*` knobs in the main table above.
Environment variables:

- `ANTHROPIC_API_KEY` — API key, used when the active profile has no `api_key`
- `ANTHROPIC_MODEL` — overrides the active model name
- `ANTHROPIC_BASE_URL` — used as the base URL when `base_url` / `openai_base_url` is not set in config
- `ANTHROPIC_API_PATH` — overrides the Anthropic messages path
- `OPENAI_API_KEY` / `OPENAI_BASE_URL` — credentials for `api_type: "openai"` profiles without a profile key
- `BRAVE_SEARCH_API_KEY` — for WebSearch tool
- `IMAGE_GEN_MODEL` — image generation model override
- `BAOCLAW_SANDBOX_IMAGE` — Docker image for `--sandbox docker`
- `BAOCLAW_HTTP1_ONLY` — force HTTP/1.1 for the Anthropic endpoint
- `BAOCLAW_FEISHU_BOT_OPEN_ID` — Feishu gateway: bot identity used to ignore message echoes (per-deployment override)
- `TELEGRAM_BOT_TOKEN` — fallback for `telegram.token`
- `BAOCLAW_TELEGRAM_CWD` — Telegram gateway: project directory override
- `BAOCLAW_WEB_TOKEN` / `BAOCLAW_WEB_HOST` — Web gateway auth token and bind host (port comes from the `--port` flag)
- `BAOCLAW_HOME` — Web gateway: `~/.baoclaw` location override
- `XDG_RUNTIME_DIR` — daemon socket location on Linux

### `<project>/.baoclaw/BAOCLAW.md` — Project Instructions

Injected into the system prompt for every conversation in this project. Write anything the agent should know about your project.

```markdown
# My Project

This is a Python web app using FastAPI + SQLAlchemy.
```

## See also

- [Usage guide](USAGE.md)
- [Permission system](PERMISSIONS.md)
- [Features & command reference](FEATURES.md)
