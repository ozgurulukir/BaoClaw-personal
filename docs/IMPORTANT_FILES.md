# Important Files

> Extracted from the root README — see [README.md](../README.md) for the project overview.

A quick annotated tour of the repository, grouped by area. Verify paths locally if the tree has moved on.

---

### Rust core (`baoclaw-core/`)

- **baoclaw-core/src/main.rs** — daemon entry point: Unix socket setup (Linux `$XDG_RUNTIME_DIR/baoclaw-sockets/baoclaw.sock`, fallback `/tmp/baoclaw-sockets/`), per-connection handling, RPC router registration, and startup config/permissions loading.
- **baoclaw-core/src/ipc/router.rs** — JSON-RPC method parsing and dispatch of incoming IPC requests.
- **baoclaw-core/src/tools/executor.rs** — the `execute_tool_with_permission` pipeline: validate → permission check → allow/deny/ask.
- **baoclaw-core/src/permissions/manager.rs** — `ToolPermissionContext`, permission rules, and knobs (`auto_allow_channels`, `ask_timeout_secs`, `persist_grants`).
- **baoclaw-core/src/engine/** — the query engine: `query_engine.rs`/`query_loop.rs` (agent loop), `memory/store.rs` (global long-term memory at `~/.baoclaw/memory.jsonl`), `cron.rs` (scheduled jobs persisted to `~/.baoclaw/cron.json`).

### TypeScript IPC SDK (`ts-ipc/`, package `baoclaw-ipc`)

- **ts-ipc/cli.ts** — interactive CLI client for the daemon.
- **ts-ipc/client.ts** — `IpcClient`: NDJSON JSON-RPC over the Unix domain socket.
- **ts-ipc/controlChannel.ts** — second connection used for abort and permission decisions.
- **ts-ipc/daemon.ts** — socket discovery conventions across Linux/macOS/Windows.
- **ts-ipc/logger.ts** — logging with redaction and rotation.
- **ts-ipc/tui/** — Ink-based terminal UI app.

### Gateways

- **baoclaw-telegram/** — Telegram gateway; `src/gateway.ts` is the entry.
- **baoclaw-feishu/** — Feishu/Lark gateway; `src/gateway.ts` is the entry. Depends on `lark-cli`; see its own README.
- **baoclaw-whatsapp/** — WhatsApp gateway; `src/gateway.ts` is the entry. Uses patch-package for crypto patches (see `patches/`).
- **baoclaw-web/** — web gateway; `src/server.ts` is the entry.

### Repo root

- **package.json** — single npm workspaces root for all TS packages.
- **package-lock.json** — the only Node lockfile.
- **tsconfig.base.json** — shared strict TypeScript config.
- **install.sh** — installs BaoClaw to `~/.baoclaw` as a mini workspace root.
- **Makefile** — common build/dev tasks.
- **lint-staged.config.js** + **.husky/** — pre-commit gates: prettier, eslint, per-package `tsc`, and `cargo clippy`/`cargo check` for the Rust core.

### Deployment & scripts

- **deploy/** — systemd, launchd, and Windows service units, plus `Dockerfile.sandbox` (Node 24 image).
- **scripts/build-sandbox.sh** — builds the sandbox image/artifacts.
- **scripts/mcp-servers.sh** — manages MCP server configuration.

### Docs

- **docs/** — the documentation set (CONFIGURATION, FEATURES, USAGE, PERMISSIONS, RELEASE, OPERATIONS_RUNBOOK, and more).
- **docs/history/** — past audits, plans, and specs.

### User state at runtime (not in the repo)

- **~/.baoclaw/** — per-user daemon state: `config.json`, `sessions/`, memories (`memory.jsonl`), `evolution/`, and `cron.json`.

---

## See also

- [Engine internals](INTERNALS.md)
- [Permission system](PERMISSIONS.md)
