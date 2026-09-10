# 🐾 BaoClaw v2.1.0

**An AI coding agent with persistent memory, multi-client access, and experimental self-improvement features.**

[English](#english) · [中文](docs/README.zh-CN.md)

---

## Documentation

**Using BaoClaw**

- [Usage guide](docs/USAGE.md) — install, first run, daily workflow
- [Features & command reference](docs/FEATURES.md) — feature walkthrough, CLI/Telegram commands, cron examples
- [Configuration reference](docs/CONFIGURATION.md) — `config.json`, model profiles, knobs
- [Permission system](docs/PERMISSIONS.md) — tool permissions, channels, ask timeout, grants

**How it works**

- [Engine internals](docs/INTERNALS.md) — query loop, tool execution, memory architecture
- [Important files tour](docs/IMPORTANT_FILES.md) — annotated map of the codebase
- [Daemon architecture & migration](docs/DAEMON_MIGRATION.md) — socket conventions, upgrading from legacy layouts

**Operating**

- [Operations runbook](docs/OPERATIONS_RUNBOOK.md) — running, monitoring, troubleshooting
- [Release process](docs/RELEASE.md) — versioning and release flow

**Historical**

- [Audits & design plans](docs/history/) — past code audits, specs, and plan documents

**中文（Chinese）** — [docs/README.zh-CN.md](docs/README.zh-CN.md)
The English documentation above is authoritative and kept current; the
Chinese translation may lag behind it.

---

<a name="english"></a>

## What is BaoClaw?

BaoClaw is an open-source AI coding agent with a Rust core engine, persistent memory, local multi-client session sharing, a cron scheduler, and experimental self-improvement features. It runs as a single global daemon on your machine, managing multiple project sessions simultaneously. Your terminal, Telegram, WhatsApp, and Feishu can connect to this daemon — each routed to a project session by working directory.

BaoClaw can retain selected knowledge about you and your projects over time. Self-improvement features are heuristic and should be reviewed by the user.

Detailed feature walkthrough and command reference: [docs/FEATURES.md](docs/FEATURES.md).

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
- **Node.js** (22+) — [nodejs.org](https://nodejs.org)
- An LLM API key (Anthropic, OpenRouter, or any OpenAI-compatible provider)

### Linux / macOS

```bash
git clone https://github.com/baohx/BaoClaw.git
cd BaoClaw
./install.sh
```

The installer builds the Rust core, installs Node.js dependencies at the npm workspace root, and creates the `baoclaw`, `baoclaw-tui`, `baoclaw-web`, `baoclaw-telegram`, `baoclaw-feishu`, and `baoclaw-whatsapp` launchers in `~/.local/bin/`.

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

# 2. Install Node.js dependencies (single npm workspace root)
npm install

# 3. Set your API key
export ANTHROPIC_API_KEY=sk-ant-...
# Or for OpenAI-compatible:
export ANTHROPIC_API_KEY=your-key
export ANTHROPIC_BASE_URL=https://your-provider.com/v1

# 4. Run
npx tsx ts-ipc/cli.ts
```

Full reference: [docs/CONFIGURATION.md](docs/CONFIGURATION.md).

## Conventions

- Rust: `rustfmt` + `clippy -D warnings` gate every commit via lint-staged.
- TypeScript: prettier + eslint; each workspace package typechecks with `tsc --noEmit`.
- Tests live next to the code: `*.test.ts` (node:test via tsx) for TS, `cargo test` for Rust.

Annotated tour: [docs/IMPORTANT_FILES.md](docs/IMPORTANT_FILES.md).
