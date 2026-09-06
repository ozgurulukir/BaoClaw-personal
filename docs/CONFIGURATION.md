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
├── mcp.json                         # User-level MCP servers
├── mcp-auth/                        # MCP OAuth tokens
├── models/                          # Local model files (whisper etc.)
│   └── ggml-base.bin
├── telemetry.db                    # Local telemetry (SQLite; turns + sessions)
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

| Field                                                | Type     | Default  | Description                                                               |
| ---------------------------------------------------- | -------- | -------- | ------------------------------------------------------------------------- |
| `model_profiles.<name>`                              | object   | `{}`     | Named profile: `model`, `api_type`, `api_key?`, `base_url?`               |
| `model_profiles.<name>.context_window`               | number   | `200000` | Context window in tokens                                                  |
| `model_profiles.<name>.auto_compact_threshold_ratio` | number   | `0.7`    | Auto-compact at this fraction of the window                               |
| `model_profiles.<name>.max_retries_per_model`        | number   | `2`      | Retries before falling back                                               |
| `primary_profile`                                    | string   | —        | Profile used by default                                                   |
| `fallback_profiles`                                  | string[] | `[]`     | Profiles to try when the primary fails                                    |
| `context_window`                                     | number   | `200000` | Top-level default (flat format)                                           |
| `auto_compact_threshold_ratio`                       | number   | `0.7`    | Top-level default (flat format)                                           |
| `tool_output_threshold_chars`                        | number   | `200000` | Tool output above this size is persisted to disk                          |
| `permissions`                                        | object   | —        | Tool permission rules and knobs — see [PERMISSIONS.md](PERMISSIONS.md)    |
| `telegram.token`                                     | string   | —        | Telegram bot token from @BotFather                                        |
| `telegram.allowedChatIds`                            | number[] | `[]`     | Allowed chat IDs (required; empty = reject all and refuse startup)        |
| `feishu.allowedChatIds`                              | string[] | `[]`     | Allowed Feishu chat IDs (required; empty = reject all and refuse startup) |

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

Per-profile `api_key` is only expressible in the profiles format.

WhatsApp session credentials are stored under `~/.baoclaw/whatsapp-auth/`.
The directory is restricted to the owner (`0700`) and credential files to the
owner (`0600`) after each credentials update.

Environment variables:

- `ANTHROPIC_API_KEY` — API key, used when the active profile has no `api_key`
- `ANTHROPIC_MODEL` — overrides the active model name
- `ANTHROPIC_BASE_URL` — used as the base URL when `base_url` / `openai_base_url` is not set in config
- `BRAVE_SEARCH_API_KEY` — for WebSearch tool

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
