# Important Files

> Extracted from the root README — see [README.md](../README.md) for the project overview.

A quick annotated tour of the repository, grouped by area. Verify paths locally if the tree has moved on.

---

### Rust core (`baoclaw-core/`)

- **baoclaw-core/src/main.rs** — daemon entry point: a short startup sequence plus connection handshake and session lifecycle cleanup.
- **baoclaw-core/src/startup.rs** — startup phases: CLI options, socket bind + announce (Linux `$XDG_RUNTIME_DIR/baoclaw.sock`, flat; macOS/Windows `baoclaw-sockets/baoclaw.sock`), config/API client, engine tools, prompts/memory/user profile, shared state assembly, cron scheduler, accept loop.
- **baoclaw-core/src/shared_client/** — the shared-session RPC loop decomposed into domain submodules (`types.rs`, `router.rs`, `system.rs`, `turn.rs`, `sessions.rs`, `tools.rs`, `cron.rs`, `skills.rs`, `team.rs`, `mcp.rs`, `config.rs`, `memory.rs`) with one named `scm_*` handler per `ClientMethod`.
- **baoclaw-core/src/ipc/router.rs** — JSON-RPC method parsing and dispatch of incoming IPC requests.
- **baoclaw-core/src/tools/executor.rs** — the `execute_tool_with_permission` pipeline: validate → permission check → allow/deny/ask.
- **baoclaw-core/src/permissions/manager.rs** — `ToolPermissionContext`, permission rules, and knobs (`auto_allow_channels`, `ask_timeout_secs`, `persist_grants`).
- **baoclaw-core/src/engine/** — the query engine: `query_engine.rs`/`query_loop/` (agent loop, split into `context.rs`, `model.rs`, `overflow.rs`, `tool_execution.rs`, `turn_records.rs`), `memory/store.rs` (async global long-term memory at `~/.baoclaw/memory.jsonl`), `transcript.rs` (async session transcripts), `session_persistence.rs` (async state snapshots), and `cron.rs` (scheduled jobs persisted to `~/.baoclaw/cron.json`).

### TypeScript IPC SDK (`ts-ipc/`, package `baoclaw-ipc`)

- **ts-ipc/cli.ts** & **ts-ipc/cli/** — interactive CLI client for the daemon with modular command registry (`cli/commands/`, `cli/registry.ts`).
- **ts-ipc/protocol/** — strongly-typed JSON-RPC 2.0 contract (`methods.ts`, `payloads.ts`, `base.ts`).
- **ts-ipc/gateway/** — unified Gateway SDK (`commandBridge.ts`, `permissionBridge.ts`, `sessionManager.ts`, `formatters/`) shared by all surfaces.
- **ts-ipc/client.ts** — `IpcClient`: NDJSON JSON-RPC over the Unix domain socket.
- **ts-ipc/controlChannel.ts** — second connection used for abort and permission decisions.
- **ts-ipc/daemon.ts** — socket discovery conventions across Linux/macOS/Windows.
- **ts-ipc/logger.ts** — logging with redaction and rotation.
- **ts-ipc/tui/** — Ink-based terminal UI app.

### Gateways

- **baoclaw-telegram/** — Telegram gateway backed by Gateway SDK; `src/gateway.ts` is the entry.
- **baoclaw-feishu/** — Feishu/Lark gateway backed by Gateway SDK; `src/gateway.ts` is the entry. Depends on `lark-cli`; see its own README.
- **baoclaw-whatsapp/** — WhatsApp gateway backed by Gateway SDK; `src/gateway.ts` is the entry. Uses patch-package for crypto patches (see `patches/`).
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
