# BaoClaw Usage Guide

BaoClaw is an AI coding assistant built on a daemon architecture. A single resident daemon process serves all frontends (CLI / TUI / Web / Telegram / Feishu / WhatsApp), sharing configuration, the session pool, and memory.

---

## 1. Installation

### Linux / macOS

```bash
git clone https://github.com/baohx/BaoClaw.git
cd BaoClaw
./install.sh
```

- Install directory: `~/.baoclaw/`
- Launcher directory: `~/.local/bin/` (make sure it is in your `$PATH`)
- Automatically builds the Rust core + installs all TS gateway dependencies
- Automatically generates 6 launchers: `baoclaw`, `baoclaw-tui`, `baoclaw-web`, `baoclaw-telegram`, `baoclaw-feishu`, `baoclaw-whatsapp`

### Windows

```powershell
git clone https://github.com/baohx/BaoClaw.git
cd BaoClaw
cd baoclaw-core
cargo build --release
cd ..\deploy\windows
PowerShell -ExecutionPolicy Bypass -File install.ps1
```

---

## 2. Configuring Models (~/.baoclaw/config.json)

Edit `~/.baoclaw/config.json` using the `model_profiles` table (primary/fallback models can mix `api_type`):

```json
{
  "model_profiles": {
    "glm52": {
      "model": "glm-5.2",
      "api_type": "anthropic",
      "api_key": "your-key-here",
      "base_url": "https://open.bigmodel.cn/api/anthropic",
      "context_window": 1000000,
      "auto_compact_threshold_ratio": 0.85
    },
    "ds": {
      "model": "deepseek-chat",
      "api_type": "openai",
      "api_key": "your-ds-key",
      "base_url": "https://api.deepseek.com",
      "context_window": 64000
    }
  },
  "primary_profile": "glm52",
  "fallback_profiles": ["ds"]
}
```

**The old format** (`model` + `fallback_models` string array) is still supported and migrated automatically at startup.

API key precedence: `model_profiles.*.api_key` > environment variables (`ANTHROPIC_API_KEY` / `OPENAI_API_KEY`).

---

## 3. Running the Daemon

### Option A: Automatic startup (default, works out of the box)

**No need to start the daemon manually.** When you open any client, if the daemon is not running, one is forked automatically.

Socket paths:

- Linux: `$XDG_RUNTIME_DIR/baoclaw.sock` (usually `/run/user/<UID>/baoclaw.sock`)
- macOS: `/tmp/baoclaw-sockets/baoclaw.sock`
- Windows: `%TEMP%\baoclaw-sockets\baoclaw.sock`

### Option B: Register as a system service (recommended for production)

More robust: starts at boot and restarts automatically on crash.

#### Linux (systemd user service)

```bash
mkdir -p ~/.config/systemd/user/
cp deploy/systemd/baoclaw.service ~/.config/systemd/user/
# Edit the service file if you need to change the ExecStart path
systemctl --user daemon-reload
systemctl --user enable --now baoclaw        # start at boot + start now

# Management commands
systemctl --user status baoclaw
systemctl --user restart baoclaw
systemctl --user stop baoclaw
journalctl --user -u baoclaw -f              # view logs
```

#### macOS (launchd)

```bash
cp deploy/launchd/com.baoclaw.daemon.plist ~/Library/LaunchAgents/
sed -i '' "s/YOUR_USERNAME/$(whoami)/g" ~/Library/LaunchAgents/com.baoclaw.daemon.plist
launchctl load ~/Library/LaunchAgents/com.baoclaw.daemon.plist
launchctl start com.baoclaw.daemon

# Management
launchctl list | grep baoclaw
launchctl stop com.baoclaw.daemon
launchctl unload ~/Library/LaunchAgents/com.baoclaw.daemon.plist  # uninstall
```

#### Windows (Service)

```powershell
cd deploy\windows
PowerShell -ExecutionPolicy Bypass -File install.ps1

# Management
Get-Service BaoClawDaemon
Start-Service BaoClawDaemon
Stop-Service BaoClawDaemon
Restart-Service BaoClawDaemon

# Uninstall
PowerShell -ExecutionPolicy Bypass -File uninstall.ps1
```

### How the daemon shuts down gracefully

When the daemon receives a shutdown signal (SIGTERM/SIGINT or Windows SCM Stop):

1. It triggers `persist_all()` — writing all active sessions to `~/.baoclaw/sessions/`
2. It exits safely

**Sessions are not lost**: after the daemon restarts, sessions are automatically restored from disk (message history + memory summaries).

---

## 4. Starting Each Frontend

> **All frontends share the same daemon.** No matter which frontend you send messages from, they all go through the same IPC and see the same sessions.

### 1. CLI (terminal chat, most common)

```bash
baoclaw                  # connects to the daemon by default (auto-forks if not running)
baoclaw --sandbox docker # Docker sandbox mode
baoclaw --think          # enable extended thinking
baoclaw --vim            # Vim mode
baoclaw --debug          # debug mode
```

**Exit**: type `/exit` or press `Ctrl+C`

### 2. TUI (rich terminal UI built with React + ink)

```bash
baoclaw-tui              # requires the daemon to already be running (systemd, or run baoclaw once first)
```

**Exit**: press `Ctrl+C`

### 3. Web (browser chat)

```bash
baoclaw-web              # defaults to http://localhost:8080
baoclaw-web --port 9090  # custom port
```

Open `http://localhost:8080` in your browser. **Exit**: `Ctrl+C`

### 4. Telegram Bot

```bash
baoclaw-telegram         # long-running process that listens for Telegram updates
```

**Prerequisite**: `telegram.token` is configured in `~/.baoclaw/config.json`.
**Exit**: `Ctrl+C`

### 5. Feishu Bot

```bash
baoclaw-feishu           # long-running process that listens for Feishu events
```

**Prerequisite**: Feishu app credentials are configured.
**Exit**: `Ctrl+C`

### 6. WhatsApp

```bash
baoclaw-whatsapp         # long-running process
```

**Prerequisite**: `whatsapp.phoneNumber` is configured in `~/.baoclaw/config.json`.
**Exit**: `Ctrl+C`

---

## 5. Useful Slash Commands (shared by CLI/TUI)

```
/help        Show all commands
/tokens      Show token usage (current / cumulative / distance to compaction)
/cost        Show cost estimate
/memory      Memory system info (/memory list to view entries)
/model       Current model configuration (API keys masked automatically)
/config      Full configuration JSON (API keys masked automatically)
/session     Current session info
/rate       Rate the last interaction (good|bad|neutral) for preference data
/clear       Clear the screen
/exit        Exit
```

---

## 6. Verifying the daemon Is Running

### Linux

```bash
ls -la $XDG_RUNTIME_DIR/baoclaw.sock
systemctl --user status baoclaw
```

### macOS

```bash
ls -la /tmp/baoclaw-sockets/baoclaw.sock
launchctl list | grep baoclaw
```

### Windows

```powershell
Get-Service BaoClawDaemon
ls $env:TEMP\baoclaw-sockets\baoclaw.sock
```

---

## 7. Directory Layout

```
~/.baoclaw/
├── bin/
│   ├── baoclaw-core              # Rust daemon binary
│   └── mcp-servers               # MCP server start/stop helper
├── package.json                  # mini npm workspace root
├── package-lock.json
├── tsconfig.base.json
├── node_modules/                 # single hoisted dependency tree
├── ts-ipc/                       # CLI + TUI source
├── baoclaw-web/                  # Web gateway
├── baoclaw-telegram/             # Telegram gateway
├── baoclaw-feishu/               # Feishu gateway
├── baoclaw-whatsapp/             # WhatsApp gateway
├── docs/                         # Documentation (USAGE.md / DAEMON_MIGRATION.md)
├── config.json                   # Configuration file (model_profiles)
├── memory.jsonl                  # Global long-term memory (single JSONL file)
└── sessions/                     # Session persistence
    ├── registry.json             # session index
    ├── <session-id>.json         # individual session state
    └── archive/                  # archive for sessions inactive > 7 days
```

---

## 8. Troubleshooting

### daemon fails to start

```bash
# Check whether the socket file is occupied (stale socket)
ls -la /run/user/$(id -u)/baoclaw.sock

# If it is a stale socket (daemon dead but the file remains), delete it
rm /run/user/$(id -u)/baoclaw.sock

# Start again
systemctl --user restart baoclaw      # Linux
launchctl start com.baoclaw.daemon    # macOS
Start-Service BaoClawDaemon           # Windows
```

### Client cannot connect to the daemon

```bash
# 1. Confirm the daemon is running
systemctl --user status baoclaw

# 2. Confirm the socket file exists
ls -la /run/user/$(id -u)/baoclaw.sock

# 3. Check the logs
journalctl --user -u baoclaw -f      # Linux
tail -f /tmp/baoclaw-daemon.stderr.log  # macOS
Get-EventLog -LogName Application -Source BaoClawDaemon  # Windows
```

### Lost sessions

```bash
# Check the persistence files
ls ~/.baoclaw/sessions/

# Check the index
cat ~/.baoclaw/sessions/registry.json | python3 -m json.tool

# The daemon automatically runs load_from_disk() at startup; manual recovery is usually unnecessary
```

### API key not taking effect

1. Check `model_profiles.*.api_key` in `~/.baoclaw/config.json`
2. If using environment variables, check `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`
3. Precedence: `api_key` in `config.json` > environment variables

---

## 9. Uninstallation

### Remove clients only (keep the daemon and configuration)

```bash
rm ~/.local/bin/baoclaw*
rm -rf ~/.baoclaw/ts-ipc ~/.baoclaw/baoclaw-*
```

### Full uninstall

```bash
# 1. Stop and remove the service
systemctl --user stop baoclaw
systemctl --user disable baoclaw
rm ~/.config/systemd/user/baoclaw.service
systemctl --user daemon-reload

# 2. Delete the install directory and configuration
rm -rf ~/.baoclaw/
rm ~/.local/bin/baoclaw*
```

---

## 10. Further Documentation

- [Daemon architecture migration guide](DAEMON_MIGRATION.md) — migrating from the old PID socket to a fixed socket + systemd
- [systemd service installation](../deploy/systemd/README.md)
- [launchd service installation](../deploy/launchd/README.md)
- [Windows Service installation](../deploy/windows/README.md)

---

**Version**: v2.1.0  
**Last updated**: 2026-06-19

## See also

- [Configuration reference](CONFIGURATION.md)
- [Features & command reference](FEATURES.md)
- [Permission system](PERMISSIONS.md)
- [Operations runbook](OPERATIONS_RUNBOOK.md)
