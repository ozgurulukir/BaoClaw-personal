# Important Files

> Extracted from the root README — see [README.md](../README.md) for the project overview.

A quick annotated tour of the repository, grouped by area. Verify paths locally if the tree has moved on.

---

### Rust core (`baoclaw-core/`)

- **baoclaw-core/src/main.rs** — daemon entry point: a short startup sequence plus the legacy connection handshake and session-close cleanup.
- **baoclaw-core/src/startup.rs** — startup phases: CLI options, socket bind + announce (Linux `$XDG_RUNTIME_DIR/baoclaw.sock`, flat; macOS/Windows `baoclaw-sockets/baoclaw.sock`), config/API client, engine tools, prompts/memory/user profile, shared state assembly, cron scheduler, accept loop.
- **baoclaw-core/src/shared_client.rs** — the shared-session RPC loop: one named `scm_*` handler per `ClientMethod` (90+ RPC handlers).
- **baoclaw-core/src/ipc/router.rs** — JSON-RPC method parsing and dispatch of incoming IPC requests.
- **baoclaw-core/src/tools/executor.rs** — the `execute_tool_with_permission` pipeline: validate → permission check → allow/deny/ask.
- **baoclaw-core/src/permissions/manager.rs** — `ToolPermissionContext`, permission rules, and knobs (`auto_allow_channels`, `ask_timeout_secs`, `persist_grants`).
- **baoclaw-core/src/engine/** — the query engine: `query_engine.rs`/`query_loop.rs` (agent loop), `memory/store.rs` (global long-term memory at `~/.baoclaw/memory.jsonl`), `cron.rs` (scheduled jobs persisted to `~/.baoclaw/cron.json`).

### TypeScript IPC SDK (`ts-ipc/`, package `baoclaw-ipc`)

- **ts-ipc/cli.ts** — interactive CLI client for the daemon (slash commands dispatch through an exact-match registry).
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

### Docs

- **docs/** — the documentation set (CONFIGURATION, FEATURES, USAGE, PERMISSIONS, RELEASE, OPERATIONS_RUNBOOK, and more).
- **docs/history/** — past audits, plans, and specs.

### User state at runtime (not in the repo)

- **~/.baoclaw/** — per-user daemon state: `config.json`, `sessions/`, memories (`memory.jsonl`), the user profile (`USER.md`), `evolution/`, `telemetry.db`, `cross_session.db` (search index, backfilled from `sessions/` snapshots at daemon startup), and `cron.json`.

---

## See also

- [Engine internals](INTERNALS.md)
- [Permission system](PERMISSIONS.md)
