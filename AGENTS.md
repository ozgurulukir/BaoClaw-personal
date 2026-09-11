# AGENTS.md

Guidance for coding agents working in this repository.

BaoClaw is a personal-fork coding-agent monorepo: a Rust daemon
(`baoclaw-core`) plus TypeScript surfaces sharing one npm workspace root
(`ts-ipc` = CLI/TUI + shared IPC library; `baoclaw-telegram`,
`baoclaw-feishu`, `baoclaw-whatsapp`, `baoclaw-web` gateways). Forked from
[baohx/BaoClaw](https://github.com/baohx/BaoClaw) (MIT — see root `LICENSE`
and `ATTRIBUTION.md`).

## Commands

- Format/lint gate (pre-commit runs these on staged files): `cargo fmt`,
  `cargo clippy --all-targets --all-features -- -D warnings`,
  `cargo check`, `prettier` for md/json — all from the repo root.
- Rust tests: `cargo test` (run inside `baoclaw-core/`). Lib tests:
  `cargo test --lib`; `startup.rs`/`shared_client/` tests live in the bin
  crate: `cargo test --bin baoclaw-core`.
- TS: `npm run typecheck` / `npm run test` (workspaces), from the root.
- Deploy to the live daemon: `cargo build --release`, then
  `cp target/release/baoclaw-core ~/.baoclaw/bin/` and restart the daemon
  **and the gateways** (they exit when the daemon socket drops).

## Architecture

- The daemon owns the session: one socket at `$XDG_RUNTIME_DIR/baoclaw.sock`
  (flat path on Linux; subdirs only on macOS/Windows). CLI/TUI/gateways are
  IPC clients; `ts-ipc` is the shared client library.
- `ts-ipc` exports a unified Gateway SDK (`baoclaw-ipc/gateway`) for
  cross-surface slash commands, card formatting, and permission handling.
- Concurrency & Async I/O: All disk persistence (`MemoryStore`, `MemoryArchive`,
  `SessionPersistence`, `TranscriptWriter`) uses non-blocking async operations
  (`tokio::fs`, `tokio::task::spawn_blocking`, atomic rename); mutex locks are
  scoped tightly and never held across await points or file I/O.
- Engine internals are documented in `docs/INTERNALS.md` (English docs are
  canonical; `docs/README.zh-CN.md` mirrors them). New user-facing docs go to
  `docs/` as UPPERCASE-NAME.md with a "See also" footer and an index line in
  the docs README.
- Long-term memory (`memory.jsonl`) is single-writer: the daemon's shared
  `MemoryStore` instance serves the prompt fragment, `MemoryTool`,
  `MemorySearch`, and IPC. Hand-editing the file while the daemon runs needs
  a restart.

## Conventions

- **Wire-or-remove**: connect dead-but-useful code, delete the rest. No
  compensating hacks, no `#[allow(dead_code)]`.
- A behavior change ships with its documentation update in the same commit.
- Repo artifacts (docs, code comments, commit messages) must not name
  third-party projects as inspiration sources — describe the mechanism.
  Credits live only in `ATTRIBUTION.md`.
- Comments state constraints, not narration. Match surrounding style.
- Tests must be hermetic: use the `_in` / `with_`-injected path seams
  (tempdirs), never touch the real `~/.baoclaw`. Test modules go at the END
  of the file. Regression tests accompany every bug fix.
- `startup.rs` and `shared_client/` are part of the **bin** crate: lib
  items they need must be `pub` (not `pub(crate)`).

## Gotchas

- Lookup tools MUST override `is_read_only()` / `is_concurrency_safe()` to
  `true` — otherwise every call prompts interactively and headless engines
  (sub-agents, cron, teams) fail closed.
- Recording through `&QueryLoopConfig` needs interior mutability
  (`Mutex` + manual `Clone`).
- One-shot CLI auto-allows permission asks; the user's live daemon may have
  Bash wildcard rules — isolate with a temp `XDG_RUNTIME_DIR` + `HOME`.
- Piped stdin to `ts-ipc/cli.ts` is a one-shot prompt, not a REPL (use
  `script -qec` for PTY tests).
- npm 11 blocks lifecycle scripts: `allowScripts` in the root package.json
  is hand-written (the CLI cannot approve workspaces); patch-package needs
  `cwd=workspaceRoot` + relative `--patch-dir`.
- Never log or print `~/.baoclaw/config.json` contents (plaintext
  `api_key`, `telegram.token`). Memory content passes
  `validate_memory_content` at the store write path — keep it that way.
