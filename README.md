# 🐾 BaoClaw v2.1.0

**An AI coding agent with persistent memory, multi-client access, and experimental self-improvement features.**

[English](#english) · [中文](docs/README.zh-CN.md)

---

## Documentation

| Topic                                                     | Document                                                                               |
| --------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| Engine internals (query loop, tools, permissions, memory) | [docs/INTERNALS.md](docs/INTERNALS.md)                                                 |
| Configuration reference                                   | [docs/CONFIGURATION.md](docs/CONFIGURATION.md)                                         |
| Important files tour                                      | [docs/IMPORTANT_FILES.md](docs/IMPORTANT_FILES.md)                                     |
| Permission system                                         | [docs/PERMISSIONS.md](docs/PERMISSIONS.md)                                             |
| Operations runbook                                        | [docs/OPERATIONS_RUNBOOK.md](docs/OPERATIONS_RUNBOOK.md)                               |
| Usage guide                                               | [docs/USAGE.md](docs/USAGE.md)                                                         |
| Daemon migration                                          | [docs/DAEMON_MIGRATION.md](docs/DAEMON_MIGRATION.md)                                   |
| Release process                                           | [docs/RELEASE.md](docs/RELEASE.md)                                                     |
| Code audit                                                | [docs/CODE_AUDIT.md](docs/CODE_AUDIT.md), [docs/AUDIT_REPORT.md](docs/AUDIT_REPORT.md) |
| Chinese README                                            | [docs/README.zh-CN.md](docs/README.zh-CN.md)                                           |

---

<a name="english"></a>

## What is BaoClaw?

BaoClaw is an open-source AI coding agent with a Rust core engine, persistent memory, local multi-client session sharing, a cron scheduler, and experimental self-improvement features. It runs as a single global daemon on your machine, managing multiple project sessions simultaneously. Your terminal, Telegram, WhatsApp, and Feishu can connect to this daemon — each routed to a project session by working directory.

BaoClaw can retain selected knowledge about you and your projects over time. Self-improvement features are heuristic and should be reviewed by the user.

## Key Features

### 🧠 Persistent Memory

- **Project-level memory** — each project directory gets its own `memory.jsonl`
- **Global memory** — cross-project facts, preferences, and decisions in `~/.baoclaw/`
- **Long-term recall** — memories are injected into the system prompt automatically
- **Manual control** — `/memory add`, `/memory list`, `/memory delete`

## Support Matrix

| Client/platform | Status    | Verified scope and limitations                                                                                                                                                                                                                                                                 |
| --------------- | --------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| CLI             | Supported | Unix-socket IPC, project sessions, tools, and streaming; see [`ts-ipc/cli.ts`](ts-ipc/cli.ts) and [`ts-ipc/client.test.ts`](ts-ipc/client.test.ts).                                                                                                                                            |
| Telegram        | Supported | Allowlisted chats and command gateway; see [`gateway.ts`](baoclaw-telegram/src/gateway.ts) and [`authorization.ts`](baoclaw-telegram/src/authorization.ts). Provider credentials and network access are required.                                                                              |
| WhatsApp        | Supported | Allowlist/rate-limit gateway; see [`allowlist.test.ts`](baoclaw-whatsapp/src/allowlist.test.ts). Baileys session credentials are local and provider behavior is external.                                                                                                                      |
| Feishu          | Supported | Exact chat allowlist, command gateway, and interactive permission cards; requires the official [`lark-cli`](https://github.com/larksuite/cli) on PATH (known-good v1.0.93) — see [`baoclaw-feishu/README.md`](baoclaw-feishu/README.md). Provider credentials and network access are required. |
| Linux           | Supported | Rust daemon and Unix-socket IPC; CI platform smoke coverage runs on Ubuntu.                                                                                                                                                                                                                    |
| macOS           | Supported | Rust daemon and Unix-socket IPC; CI platform smoke coverage runs on macOS.                                                                                                                                                                                                                     |
| Windows/WSL2    | WSL2 only | Run the Unix daemon inside WSL2; native Windows daemon IPC is not supported until named-pipe transport exists.                                                                                                                                                                                 |

Security boundaries are defense-in-depth, not a guarantee that prompts or tool
inputs are safe. Review generated skills before promotion and do not provide
real credentials in examples or test fixtures.

### 📱 Multi-Client, Global Daemon

- **One daemon, all projects** — a single daemon process manages sessions for all your project directories
- **Per-project sessions** — each cwd gets its own session with independent history and memory
- **Multi-device access** — continue a task from another device when it can reach the daemon through a configured gateway
- **Real-time streaming** — all clients see tool calls and responses as they happen
- **No conflicts** — two CLI terminals in different directories use different sessions, no interference
- **Session persistence** — conversations survive daemon restarts, auto-resumed per project

### 🔄 Self-Evolution Engine (Experimental)

Inspired by [Hermes Agent](https://github.com/NousResearch/hermes-agent)'s learning loop:

- **Trajectory recording** — every interaction is logged with tools used, outcomes, and timing
- **Skill auto-generation** — complex successful tasks are extracted as reusable skill candidates
- **Self-evaluation nudge** — every 15 tasks, the agent reflects on patterns and creates/improves skills
- **User ratings** — rate interactions as good/bad to build preference data
- **Training-data export** — export trajectories as JSONL in a format that can be adapted for DPO/RLHF fine-tuning
- **Personal evolution** — skills and trajectories are cross-project (`~/.baoclaw/evolution/`)
- **Evolve tool** — agent can propose, improve, and promote skills; review generated skills before relying on them

### ⏰ Cron Scheduler

- **Periodic tasks** — schedule prompts to run automatically inside the daemon
- **Flexible schedules** — `every 30m`, `every 2h`, `daily 09:00`, `weekly mon 09:00`
- **Result broadcast** — cron results pushed to all connected clients (CLI + Telegram)
- **Persistent** — jobs saved in `~/.baoclaw/cron.json`, survive daemon restarts
- **Agent tool access** — each job runs with the tools permitted by the daemon configuration

### 📄 Document Q&A

- **Upload files** — PDF, DOCX, and images via Telegram or CLI (`@file.pdf`)
- **Route A** — client-side text extraction (mammoth for DOCX, pdf-parse for PDF)
- **Route B** — native API document blocks (PDF sent directly to Claude/OpenAI)
- **Image understanding** — photos analyzed via multimodal API (both Anthropic and OpenAI compatible)
- **Tab completion** — `@` triggers file path completion in CLI

### 🗂️ Project-Scoped Everything

- **`/cd` command** — switch working directory at runtime, like changing projects
- **Auto-scaffold** — `.baoclaw/` directory with config files created automatically
- **Session per project** — each directory maps to its own persistent session file
- **Auto-resume** — reconnecting to a project automatically restores conversation history
- **Project instructions** — `BAOCLAW.md` loaded into system prompt per project
- **Memory isolation** — each project has its own memory store

### 🛠️ Built-in Tools

| Tool                            | Description                                                   |
| ------------------------------- | ------------------------------------------------------------- |
| Bash                            | Shell commands (respects project cwd)                         |
| FileRead / FileWrite / FileEdit | File operations with path validation                          |
| Grep / Glob                     | Code search and file discovery                                |
| WebSearch                       | Brave Search API with retry on rate limits                    |
| WebFetch                        | Fetch and parse web pages                                     |
| Memory                          | Long-term memory management                                   |
| Agent                           | Sub-agent for parallel tasks                                  |
| Evolve                          | Self-improvement: create/improve skills, export training data |
| Todo                            | Task list management                                          |
| Notebook                        | Jupyter notebook editing                                      |
| ProjectNote                     | Project-level notes                                           |
| ToolSearch                      | Search across all registered tools                            |

### 🔌 Extensible

- **MCP support** — connect external MCP servers for additional tools
- **Skills** — markdown-based skill files loaded into system prompt (personal + project scope)
- **Plugins** — directory-based plugin system with tools, skills, and MCP configs
- **Many LLM models** — Anthropic native + compatible OpenAI-style APIs (OpenRouter, Ollama, vLLM, etc.)

### 🔁 Model Fallback

- **Automatic retry** — rate-limited requests retry with exponential backoff
- **Fallback chain** — configure multiple models; if one is rate-limited, fall back to the next
- **Transparent** — CLI shows model switches in real-time

### ⌨️ Keyboard Shortcuts

- **Ctrl+C** during task → abort current task
- **Ctrl+C** when idle → hint to press again or `/quit`
- **Ctrl+C × 2** → disconnect from daemon
- **Tab** → autocomplete commands and file paths

### 🚀 v2.0 — Intelligence Layer (NEW)

Phase 2–4 additions that make BaoClaw smarter, safer, and faster:

#### 🔍 Cross-Session Search (#5)

- **SQLite + FTS5** full-text search across all past sessions
- Search by keyword, get ranked results with context snippets
- Find that solution you saw 3 weeks ago in seconds

#### ❄️ Frozen Snapshot Caching (#6)

- System prompt and tools list are built **once** and frozen for the entire session
- Maximizes Anthropic prompt cache hit rate — only the dynamic reminder changes per turn
- Reduces cost and latency on every API call

#### 👤 User Profile (#7)

- `~/.baoclaw/USER.md` — persistent user profile (name, language, coding style, tool preferences)
- Auto-loaded into system prompt for personalized responses
- Session stats merged automatically (total turns, cost, top tools)

#### 🔄 Skill Self-Improvement Loop (#8)

- **5-stage cycle**: Collect → Evaluate → Improve → Validate → Retire
- Scores skills on relevance rate, success rate, user rating, and staleness
- Auto-retires persistently poor skills, suggests improvements for mediocre ones
- Runs periodically to keep your skill set healthy

#### 📐 Adaptive Compact (#9)

- `AdaptiveCompactTracker` adjusts `keep_recent` heuristically from compression history
- If the user re-asks about pre-compact content → increase `keep_recent` (preserve more)
- If compression ratio is poor and no information loss → decrease `keep_recent` (compact harder)
- Range: 6–30 messages, auto-adjusted per session

#### 🏥 Tool Health Monitoring (#10)

- Tracks success/failure/timeout rates per tool in real time
- **3 statuses**: Healthy → Degraded (3 consecutive failures) → Disabled (6 failures)
- Degraded tools get warning messages in the system prompt
- Auto-recovers after 5 consecutive successes

#### 🎯 Intent Prediction (#11)

- Predicts user intent (coding, debugging, testing, refactoring, git, research…) from message keywords
- Heuristic transition matrix records what intent typically follows what (e.g., CodeWriting → Testing)
- High-confidence predictions trigger tool preloading hints in the system prompt

#### 🧮 Context Window Allocator (#12)

- Attention score = 0.5×relevance + 0.3×recency + 0.2×frequency
- Mandatory blocks (system prompt, tools) always included
- Optional blocks (memory, skills, search results) greedy-fill by score
- Budget exceeded → lowest-scoring blocks trimmed first

#### 🏖️ Sandbox Execution (#13)

- Three backends: **Bubblewrap** (Linux namespaces) → **Docker** (containers) → None (direct)
- Auto-detects best available backend at startup
- Configurable: read-only/read-write mounts, network isolation, memory/CPU limits, timeouts
- Wrap any command for sandboxed execution with a single `wrap_command()` call

#### 🛡️ Prompt Injection Detection (#14)

- **20 patterns** across 6 categories: instruction override, role hijack, data exfiltration, encoding tricks, hidden payloads, jailbreak
- Heuristic scoring with diminishing returns + multi-category boost
- Four severity levels: Clean → Suspicious → Dangerous → Critical
- `sanitize()` method redacts detected patterns with `[REDACTED]` placeholders

#### 🔐 Subagent Depth Policy (#15)

- Maximum nesting depth: 3 levels
- **Progressive tool restriction**: Depth 0 = all tools, Depth 1 = safe tools, Depth 2 = read-only, Depth 3 = minimal (FileRead + Bash only)
- Per-depth budgets: turns cap (100→30→15→5), cost cap ($10→$2→$0.50→$0.10)
- Exceeded budget → auto-terminate sub-agent

#### 📡 Streaming Tool Executor (#16)

- Real-time chunked output: Started → Progress → Stdout → Stderr → Completed → Error → Heartbeat
- `StreamWriter` / `StreamReader` pair via `tokio::sync::mpsc`
- Configurable timeout (5 min default), buffer size, max output (1MB default)
- Concurrent stdout/stderr reading with `tokio::select!`

### 🚀 v2.1 — Evolution Engine (NEW)

#### 📋 Workflow Template Engine (#17)

- **5 built-in templates**: `code_review`, `bug_fix`, `feature`, `docs`, `refactor`
- Trigger-based matching (`/review` → code_review template)
- Variable substitution with `${variable}` syntax and step output references `${stepN.output}`
- Conditional workflow steps with `condition` field
- Import/export templates as JSON for sharing
- Create custom templates with custom workflows and variables

#### 🌿 Git Integration (#18)

- **Branch Management**: create, list, switch, merge with name validation and protected branch detection
- **Commit Management**: stage files, commit with conventional format (`feat:`, `fix:`, `chore:`), amend, undo
- **Conflict Resolution**: detect conflicts from merge markers, resolve by taking ours/theirs
- **PR Management**: create pull requests, list by status, review, merge
- SSH and HTTPS credential management with host-based lookup

#### 🧭 Model Router (#19)

- **Intelligent routing**: select model by task type (code/completion/creative/analysis)
- **Cost-aware**: prefer cheaper models for simple tasks, route to premium models for complex work
- **Budget tracking**: set spending limits, track token usage, alert on threshold exceeded
- **Usage learning**: record route history, generate optimization suggestions based on usage patterns
- **Fallback chain**: automatic failover when primary model unavailable

#### 📊 Telemetry & Monitoring (#20)

- **Event collection**: record tool calls, model invocations, errors, session events
- **Trend analysis**: detect increasing/decreasing/stable patterns over time windows
- **Multi-format export**: JSON for programmatic use, CSV for spreadsheet analysis
- **Aggregated statistics**: per-tool usage counts, model distribution, error rates

#### 🔐 Permission Gate (#21)

- **Tool-level access control**: grant/revoke permissions per tool per session
- **Interactive prompts**: ask user for approval before executing sensitive operations
- **Permission caching**: cache decisions with configurable TTL to avoid prompt fatigue
- **Default-deny mode**: start with all tools denied, explicitly grant as needed

### 🖥️ CLI & TUI

- **18 new CLI commands** across 5 modules (`/template`, `/git`, `/model`, `/telemetry`, `/permission`)
- **Terminal UI (TUI)** built with Ink (React terminal framework):
  - Split-pane layout: message list + streaming output
  - Tool execution panel with live status
  - Syntax-highlighted code blocks
  - Keyboard shortcuts overlay (`Ctrl+H`)
- **Unix socket IPC**: JSON-RPC 2.0 over Unix domain sockets with NDJSON streaming
- Auto-discovers the fixed daemon socket first (`$XDG_RUNTIME_DIR/baoclaw.sock` on Linux, `/tmp/baoclaw-sockets/baoclaw.sock` on macOS), then falls back to the cwd-hash socket

## Architecture

```
┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│  CLI (TUI)   │  │  CLI (TUI)   │  │  Telegram    │
│  cwd: proj-a │  │  cwd: proj-b │  │  Bot         │
└──────┬───────┘  └──────┬───────┘  └──────┬───────┘
       │                 │                 │
       └────────┬────────┴────────┬────────┘
                │  Unix Socket (IPC)       │
                │  JSON-RPC 2.0 / NDJSON   │
       ┌────────┴──────────────────────────┐
       │    Global BaoClaw Daemon (Rust)   │
       │    One daemon, multiple sessions  │
       │                                   │
       │  ┌────────────┐ ┌────────────┐   │
       │  │ Session A   │ │ Session B   │  │
       │  │ (proj-a)    │ │ (proj-b)    │  │
       │  │ own history │ │ own history │  │
       │  │ own memory  │ │ own memory  │  │
       │  └──────┬──────┘ └──────┬──────┘  │
       │         └───────┬───────┘         │
       │         ┌───────┴───────┐         │
       │         │ Tool Executor │         │
       │         │ Built-in tools│         │
       │         │ + MCP servers │         │
       │         └───────────────┘         │
       │  ┌──────────────┐ ┌────────────┐ │
       │  │Cron Scheduler│ │ Evolution  │ │
       │  └──────────────┘ │ Engine     │ │
       │                   └────────────┘ │
       └───────────────────────────────────┘
                       │
              ┌────────┴────────┐
              │ Anthropic/OpenAI│
              │ Compatible API  │
              └─────────────────┘
```

Key design: **one global daemon process manages all projects**. Each project directory gets its own session with independent conversation history and memory. Multiple CLI terminals and Telegram can connect simultaneously — each routed to the correct project session by its working directory.

For a deep dive into the query engine, tool execution, permission
system, and memory architecture, see [docs/INTERNALS.md](docs/INTERNALS.md).

## Installation

### Prerequisites

- **Rust** (1.96+) — [rustup.rs](https://rustup.rs)
- **Node.js** (18+) — [nodejs.org](https://nodejs.org)
- An LLM API key (Anthropic, OpenRouter, or any OpenAI-compatible provider)

### Linux / macOS

```bash
git clone https://github.com/baohx/BaoClaw.git
cd BaoClaw
./install.sh
```

The installer builds the Rust core, installs Node.js dependencies, and creates the `baoclaw` launcher in `~/.local/bin/`.

### Windows (WSL2)

BaoClaw requires a Unix environment. On Windows, use WSL2:

```powershell
# Install WSL2 if not already installed
wsl --install

# Inside WSL2
git clone https://github.com/baohx/BaoClaw.git
cd BaoClaw
./install.sh
```

### Manual Setup

```bash
# 1. Build Rust core
cd baoclaw-core
cargo build --release
cd ..

# 2. Install CLI dependencies
cd ts-ipc
npm install
cd ..

# 3. Set your API key
export ANTHROPIC_API_KEY=sk-ant-...
# Or for OpenAI-compatible:
export ANTHROPIC_API_KEY=your-key
export ANTHROPIC_BASE_URL=https://your-provider.com/v1

# 4. Run
npx --prefix ts-ipc tsx ts-ipc/cli.ts
```

Full reference: [docs/CONFIGURATION.md](docs/CONFIGURATION.md).

## Conventions

- Use type hints everywhere
- Tests go in tests/ directory
- Use pytest for testing
- Database migrations with alembic

Annotated tour: [docs/IMPORTANT_FILES.md](docs/IMPORTANT_FILES.md).

## CLI Commands

| Command         | Description                                  |
| --------------- | -------------------------------------------- |
| `/projects`     | Project management: list, switch, new, desc  |
| `/tools`        | List registered tools                        |
| `/mcp`          | List MCP servers                             |
| `/skills`       | List loaded skills                           |
| `/plugins`      | List installed plugins                       |
| `/model [name]` | Show or switch model                         |
| `/think`        | Toggle extended thinking mode                |
| `/compact`      | Compress conversation context                |
| `/memory`       | Long-term memory: list, add, delete, clear   |
| `/cron`         | Scheduled tasks: add, list, remove, toggle   |
| `/diff`         | Git diff summary                             |
| `/commit <msg>` | Stage all and commit                         |
| `/git`          | Git status (branch, changes)                 |
| `/task`         | Background tasks: run, list, status, stop    |
| `/voice`        | Voice input (requires whisper.cpp)           |
| `/telegram`     | Manage Telegram gateway: start, stop, status |
| `/telemetry`    | Toggle telemetry on/off                      |
| `@file.pdf`     | Attach file for Q&A (PDF, DOCX, images)      |
| `/abort`        | Cancel current request (or press Ctrl+C)     |
| `/clear`        | Clear screen                                 |
| `/help`         | Show all commands                            |
| `/quit`         | Disconnect (daemon keeps running)            |
| `/shutdown`     | Stop the daemon process                      |

## Telegram Commands

All CLI commands are also available in Telegram:

| Command                              | Description                       |
| ------------------------------------ | --------------------------------- |
| `/tools` `/skills` `/mcp` `/plugins` | List resources                    |
| `/model [name]`                      | Show or switch model              |
| `/think`                             | Toggle extended thinking          |
| `/compact`                           | Compress context                  |
| `/memory`                            | Manage memories                   |
| `/cron`                              | Manage scheduled tasks            |
| `/projects`                          | 项目管理: list, switch, new, desc |
| `/task`                              | Manage background tasks           |
| `/diff` `/commit` `/git`             | Git operations                    |
| `/abort`                             | Cancel current task               |
| `/status`                            | Gateway status                    |
| `/help`                              | Show all commands                 |
| 📎 Upload file                       | Send PDF/DOCX/image for Q&A       |

## Telegram Setup

1. Create a bot via [@BotFather](https://t.me/BotFather)
2. Add token to `~/.baoclaw/config.json`
3. Start from CLI: `/telegram start`

Upload documents and images directly in Telegram chat — the bot extracts text and sends it to the AI.

## Cron Examples

```
/cron add "Daily git summary" "daily 09:00" Summarize yesterday's git commits
/cron add "Dep check" "weekly mon 10:00" Check for dependency security updates
/cron add "Evolution review" "every 2h" Review pending skill candidates and improve
/cron list
/cron toggle abc123
/cron remove abc123
```

Results are pushed to all connected clients (CLI shows ⏰ notification, Telegram receives a message).

## Self-Evolution: How It Works

```
 Use BaoClaw ──→ Trajectories recorded
                        │
                        ▼
              Complex task succeeds?
                   │          │
                  Yes         No
                   │          │
                   ▼          ▼
           Extract skill    (skip)
           candidate
                   │
                   ▼
          Every 15 tasks ──→ Self-evaluation nudge
                   │
                   ▼
          Agent creates/improves skills
                   │
                   ▼
          Skills loaded in next session
                   │
                   ▼
          Better performance ──→ Loop continues
                   │
                   ▼
           Export trajectories ──→ DPO/RLHF dataset preparation
                                  for smaller models
```

### Training Data Export

```bash
# Inside BaoClaw, ask the agent:
> Export training data for fine-tuning

# Or use the Evolve tool directly:
# The agent calls Evolve(operation: "export_training")
# Output: ~/.baoclaw/evolution/training_export.jsonl
```

Each trajectory contains: prompt, tool actions, outcome, user rating (good/bad/neutral). Rated trajectories can be used as inputs when preparing preference pairs for DPO training.

## License

MIT

---
