# Daemon Architecture Migration Guide

This document describes the evolution of the BaoClaw daemon architecture and how to migrate from the old version to the new one.

## Architecture Evolution

### Phase 0 (original): original socket naming

- Socket path: `/tmp/baoclaw-sockets/baoclaw-<PID>.sock`
- Problem: every CLI launch forks a new daemon; even with the same cwd, sessions cannot be shared

### Phase 1 (P1-2, 2026-06-19): cwd-hash socket

- Socket path: `/tmp/baoclaw-sockets/baoclaw-cwd-${hash}.sock`
- Improvement: all clients in the same cwd share one daemon
- Limitation: still depends on cwd; cross-directory usage requires switching

### Phase 2 (P3-1c, 2026-06-19): fixed socket + graceful shutdown

Socket paths:

- Linux/macOS: `$XDG_RUNTIME_DIR/baoclaw.sock` (/run/user/UID/) or `/tmp/baoclaw-sockets/baoclaw.sock`
- Windows: `%TEMP%/baoclaw.sock`
  Improvement: one machine-level daemon shared by all sessions
  Graceful shutdown: persist_all() on SIGTERM/SIGINT

### Phase 3 (P3-1a/b, 2026-06-19): systemd/launchd service

- Daemon runs 24/7
- Starts automatically at boot
- Automatic restart on crash

## Connection Logic

Clients locate the daemon in the following order:

1. **Fixed socket** (`fixed_socket_path()`)
   - Highest priority; used once systemd/launchd service is set up
2. **cwd-hash socket** (`make_socket_path(cwd)`)
   - Fallback, for compatibility with non-service environments

If neither exists, the CLI forks its own daemon (legacy behavior, backward compatible).

## Migration Steps

### Migrating from Phase 0/1 to Phase 2 (automatic, no action needed)

After upgrading to `3252fb8` or later, the client connection logic automatically becomes:

- Try the fixed socket first (new behavior)
- If not found, try the cwd-hash socket (old behavior)
- If neither is found, fork a new daemon

**No configuration changes required.**

### Migrating from Phase 2 to Phase 3 (manual, optional)

To use the systemd service (Linux):

1. Install the service following `deploy/systemd/README.md`
2. Start the service: `systemctl --user start baoclaw`
3. Verify: `ls $XDG_RUNTIME_DIR/baoclaw.sock`
4. From then on, every terminal connects to the daemon immediately; no CLI fork needed

### Cleaning up old socket files

Old socket files may remain after upgrading; clean them up with:

```bash
# Linux/macOS
rm -f /tmp/baoclaw-sockets/baoclaw-*.sock
rm -f /tmp/baoclaw-sockets/baoclaw-cwd-*.sock
rm -f /run/user/$(id -u)/baoclaw-cwd-*.sock

# Keep only the fixed socket
ls /run/user/$(id -u)/baoclaw.sock          # Linux
ls /tmp/baoclaw-sockets/baoclaw.sock         # macOS
```

## Session Persistence

Phase 2 introduces session persistence:

- Storage path: `~/.baoclaw/sessions/<session-id>.json`
- Index file: `~/.baoclaw/sessions/registry.json`
- Archive directory: `~/.baoclaw/sessions/archive/` (sessions inactive for >7 days are archived automatically)
- Trigger points: end of each conversation turn + daemon receiving SIGTERM/SIGINT

After a daemon crash or restart, sessions (message history + memory summaries) are automatically restored from disk at startup.

## Troubleshooting

### Daemon fails to start

```bash
# Check whether the socket file is occupied
ls -la /run/user/$(id -u)/baoclaw.sock

# If it is a stale socket (daemon is dead but the file remains), remove it
rm /run/user/$(id -u)/baoclaw.sock

# Restart the daemon
systemctl --user restart baoclaw
```

### Client cannot connect to the daemon

```bash
# 1. Confirm the daemon is running
systemctl --user status baoclaw

# 2. Confirm the socket file exists
ls -la /run/user/$(id -u)/baoclaw.sock

# 3. Test the connection (if socat is available)
socat - UNIX-CONNECT:/run/user/$(id -u)/baoclaw.sock

# 4. Check the logs
journalctl --user -u baoclaw -f
```

### Session lost

```bash
# Check the persistence files
ls ~/.baoclaw/sessions/

# Check the registry
cat ~/.baoclaw/sessions/registry.json | jq .

# Manual recovery (usually automatic)
# The daemon calls load_from_disk() automatically at startup
```

## Rollback

If the new version has problems, you can roll back to the old version:

```bash
git checkout 929e161    # Phase 0/1
cargo build --release --bin baoclaw-core
```

Session persistence files (`~/.baoclaw/sessions/`) are compatible between old and new versions (both are JSON); no cleanup is needed.

## See also

- [Engine internals](INTERNALS.md)
- [Operations runbook](OPERATIONS_RUNBOOK.md)
