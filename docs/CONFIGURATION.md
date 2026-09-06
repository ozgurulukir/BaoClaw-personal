# Configuration Reference

> Extracted from the root README — see [README.md](../README.md) for the project overview.

### Directory Structure

```
~/.baoclaw/                          # User-level (global, cross-project)
├── config.json                      # Main configuration
├── memory.jsonl                     # Global memories (fallback)
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
├── telemetry/                       # Telemetry events (local only)
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

```json
{
  "model": "claude-sonnet-4-20250514",
  "fallback_models": ["claude-3-5-haiku-20241022"],
  "max_retries_per_model": 2,
  "api_type": "anthropic",
  "openai_base_url": null,
  "telegram": {
    "token": "<telegram-bot-token>",
    "allowedChatIds": [12345678]
  },
  "feishu": {
    "allowedChatIds": ["oc_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"]
  }
}
```

| Field                     | Type     | Default                    | Description                                                               |
| ------------------------- | -------- | -------------------------- | ------------------------------------------------------------------------- |
| `model`                   | string   | `claude-sonnet-4-20250514` | Primary LLM model                                                         |
| `fallback_models`         | string[] | `[]`                       | Models to try when primary is rate-limited                                |
| `max_retries_per_model`   | number   | `2`                        | Retries before falling back to next model                                 |
| `api_type`                | string   | `"anthropic"`              | `"anthropic"` or `"openai"`                                               |
| `openai_base_url`         | string?  | `null`                     | Base URL for OpenAI-compatible API                                        |
| `telegram.token`          | string   | —                          | Telegram bot token from @BotFather                                        |
| `telegram.allowedChatIds` | number[] | `[]`                       | Allowed chat IDs (required; empty = reject all and refuse startup)        |
| `feishu.allowedChatIds`   | string[] | `[]`                       | Allowed Feishu chat IDs (required; empty = reject all and refuse startup) |

WhatsApp session credentials are stored under `~/.baoclaw/whatsapp-auth/`.
The directory is restricted to the owner (`0700`) and credential files to the
owner (`0600`) after each credentials update.

Environment variable overrides:

- `ANTHROPIC_API_KEY` — API key (required)
- `ANTHROPIC_MODEL` — overrides `model` field
- `ANTHROPIC_BASE_URL` — overrides `openai_base_url`
- `BRAVE_SEARCH_API_KEY` — for WebSearch tool

OpenAI-compatible example:

```json
{
  "model": "deepseek-chat",
  "api_type": "openai",
  "openai_base_url": "https://api.deepseek.com/v1"
}
```

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
