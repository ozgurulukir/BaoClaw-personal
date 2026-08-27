#!/bin/bash
# BaoClaw Installer — installs baoclaw command globally
set -e

# Ensure execution with bash even if invoked with 'sh install.sh'
if [ -z "$BASH_VERSION" ]; then
  exec bash "$0" "$@"
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTALL_DIR="${BAOCLAW_HOME:-$HOME/.baoclaw}"
BIN_DIR="${BAOCLAW_BIN_DIR:-$HOME/.local/bin}"

FORCE_DEPS=0
for arg in "$@"; do
  case "$arg" in
    --force-deps|--reinstall-deps|-f)
      FORCE_DEPS=1
      ;;
  esac
done

echo "╔═══════════════════════════════════════╗"
echo "║       BaoClaw Installer v2.1.0        ║"
echo "╚═══════════════════════════════════════╝"
echo ""

# 1. Build Rust core
echo "🔨 Building Rust core (release)..."
cd "$SCRIPT_DIR/baoclaw-core"
cargo build --release --quiet
cd "$SCRIPT_DIR"
echo "✓ Rust core built"

# 2. Build ts-ipc bundle
if [ -d "$SCRIPT_DIR/ts-ipc" ]; then
  if [ ! -d "$SCRIPT_DIR/ts-ipc/node_modules" ] || [ "$FORCE_DEPS" -eq 1 ]; then
    echo "📦 Installing ts-ipc dependencies..."
    cd "$SCRIPT_DIR/ts-ipc"
    npm install --prefer-offline --no-audit --no-fund --silent
    cd "$SCRIPT_DIR"
  fi
  echo "⚡ Building fast CLI bundle (esbuild)..."
  npm --prefix "$SCRIPT_DIR/ts-ipc" run build --silent
  echo "✓ CLI bundle ready (dist/baoclaw.mjs)"
fi

# 3. Ensure TS gateway dependencies in source directories
for dir in baoclaw-telegram baoclaw-web baoclaw-feishu baoclaw-whatsapp; do
  if [ -d "$SCRIPT_DIR/$dir" ]; then
    if [ ! -d "$SCRIPT_DIR/$dir/node_modules" ] || [ "$FORCE_DEPS" -eq 1 ]; then
      echo "📦 Installing $dir dependencies..."
      cd "$SCRIPT_DIR/$dir"
      npm install --prefer-offline --no-audit --no-fund --silent
      cd "$SCRIPT_DIR"
    fi
    echo "✓ $dir ready"
  fi
done

# 4. Create install dirs
mkdir -p "$INSTALL_DIR/bin" "$BIN_DIR"

# 5. Copy Rust binary (unlink first to avoid ETXTBUSY if daemon is running)
rm -f "$INSTALL_DIR/bin/baoclaw-core"
cp "$SCRIPT_DIR/baoclaw-core/target/release/baoclaw-core" "$INSTALL_DIR/bin/baoclaw-core"
chmod +x "$INSTALL_DIR/bin/baoclaw-core"
echo "✓ Rust binary → $INSTALL_DIR/bin/"

# 6. Copy each gateway source
copy_gateway() {
  local name="$1"
  local src="$SCRIPT_DIR/$name"
  local dst="$INSTALL_DIR/$name"
  [ ! -d "$src" ] && return 0
  mkdir -p "$dst/src" "$dst/public" "$dst/tui" 2>/dev/null
  # Root level TS files (cli.ts, client.ts, colors.ts, images.ts, etc.)
  for f in "$src"/*.ts "$src"/*.tsx; do
    [ -f "$f" ] && cp "$f" "$dst/"
  done
  # TS/TSX sources in src/
  for f in "$src"/src/*.ts "$src"/src/*.tsx; do
    [ -f "$f" ] && cp "$f" "$dst/src/"
  done
  # TUI subfolder (ts-ipc)
  if [ "$name" = "ts-ipc" ] && [ -d "$src/tui" ]; then
    mkdir -p "$dst/tui/components"
    for f in "$src"/tui/*.ts "$src"/tui/*.tsx; do
      [ -f "$f" ] && cp "$f" "$dst/tui/"
    done
    for f in "$src"/tui/components/*.tsx; do
      [ -f "$f" ] && cp "$f" "$dst/tui/components/"
    done
  fi
  # public static assets (web)
  [ -d "$src/public" ] && cp -r "$src/public/." "$dst/public/" 2>/dev/null
  # Gateway-specific install hooks and patch-package patches
  [ -d "$src/scripts" ] && cp -r "$src/scripts/." "$dst/scripts/" 2>/dev/null
  [ -d "$src/patches" ] && cp -r "$src/patches/." "$dst/patches/" 2>/dev/null
  # dist bundle directory
  [ -d "$src/dist" ] && mkdir -p "$dst/dist" && cp -r "$src/dist/." "$dst/dist/" 2>/dev/null
  # package metadata
  cp "$src/package.json" "$dst/" 2>/dev/null
  cp "$src/package-lock.json" "$dst/" 2>/dev/null
  cp "$src/tsconfig.json" "$dst/" 2>/dev/null

  # Only install dependencies in destination if node_modules missing or forced
  if [ ! -d "$dst/node_modules" ] || [ "$FORCE_DEPS" -eq 1 ]; then
    if [ -d "$src/node_modules" ]; then
      cp -r "$src/node_modules" "$dst/" 2>/dev/null || (cd "$dst" && npm install --prefer-offline --no-audit --no-fund --silent)
    else
      cd "$dst" && npm install --prefer-offline --no-audit --no-fund --silent
    fi
  fi

  echo "✓ $name → $dst"
}
copy_gateway ts-ipc
copy_gateway baoclaw-telegram
copy_gateway baoclaw-web
copy_gateway baoclaw-feishu
copy_gateway baoclaw-whatsapp

# 7. Launcher functions
make_launcher() {
  local name="$1" target_subpath="$2" help="$3"
  cat > "$BIN_DIR/$name" << EOF
#!/bin/bash
# BaoClaw $name — $help
BAOCLAW_HOME="\${BAOCLAW_HOME:-\$HOME/.baoclaw}"
export BAOCLAW_CORE_BIN="\$BAOCLAW_HOME/bin/baoclaw-core"
exec npx --prefix "\$BAOCLAW_HOME/$(dirname "$target_subpath")" tsx "\$BAOCLAW_HOME/$target_subpath" "\$@"
EOF
  chmod +x "$BIN_DIR/$name"
  echo "✓ $name → $BIN_DIR/$name"
}

# CLI launcher (fast prebundled .mjs first, tsx fallback)
cat > "$BIN_DIR/baoclaw" << 'LAUNCHER'
#!/bin/bash
BAOCLAW_HOME="${BAOCLAW_HOME:-$HOME/.baoclaw}"
export BAOCLAW_CORE_BIN="$BAOCLAW_HOME/bin/baoclaw-core"

if [ -f "$BAOCLAW_HOME/ts-ipc/dist/baoclaw.mjs" ]; then
  exec node "$BAOCLAW_HOME/ts-ipc/dist/baoclaw.mjs" "$@"
else
  exec npx --prefix "$BAOCLAW_HOME/ts-ipc" tsx "$BAOCLAW_HOME/ts-ipc/cli.ts" "$@"
fi
LAUNCHER
chmod +x "$BIN_DIR/baoclaw"
echo "✓ baoclaw → $BIN_DIR/baoclaw"

# Other gateway launchers
make_launcher "baoclaw-tui"       "ts-ipc/tui/index.tsx"           "Rich terminal UI (ink)"
make_launcher "baoclaw-web"       "baoclaw-web/src/server.ts"      "Web browser chat"
make_launcher "baoclaw-telegram"  "baoclaw-telegram/src/gateway.ts" "Telegram bot gateway"
make_launcher "baoclaw-feishu"    "baoclaw-feishu/src/gateway.ts"  "Feishu bot gateway"
make_launcher "baoclaw-whatsapp"  "baoclaw-whatsapp/src/gateway.ts" "WhatsApp gateway"

# 8. MCP server launcher script
mkdir -p "$INSTALL_DIR/bin"
if [ -f "$SCRIPT_DIR/scripts/mcp-servers.sh" ]; then
  cp "$SCRIPT_DIR/scripts/mcp-servers.sh" "$INSTALL_DIR/bin/mcp-servers"
  chmod +x "$INSTALL_DIR/bin/mcp-servers"
  echo "✓ mcp-servers → $INSTALL_DIR/bin/mcp-servers"
  echo "  Usage: $INSTALL_DIR/bin/mcp-servers {start|stop|restart|status|debug}"
fi

# 9. Docs
mkdir -p "$INSTALL_DIR/docs"
[ -f "$SCRIPT_DIR/docs/USAGE.md" ] && cp "$SCRIPT_DIR/docs/USAGE.md" "$INSTALL_DIR/docs/" && echo "✓ docs/USAGE.md → $INSTALL_DIR/docs/"
[ -f "$SCRIPT_DIR/docs/DAEMON_MIGRATION.md" ] && cp "$SCRIPT_DIR/docs/DAEMON_MIGRATION.md" "$INSTALL_DIR/docs/" && echo "✓ docs/DAEMON_MIGRATION.md → $INSTALL_DIR/docs/"

# 10. Systemd service update (if exists)
SYSTEMD_DIR="$HOME/.config/systemd/user"
if [ -d "$SYSTEMD_DIR" ] && [ -f "$SYSTEMD_DIR/baoclaw.service" ]; then
  if ! grep -q "ExecStartPre.*mcp-servers" "$SYSTEMD_DIR/baoclaw.service" 2>/dev/null; then
    echo ""
    echo "⚠️  Updating systemd service to start MCP servers..."
    BAOCLAW_BIN="$HOME/.baoclaw/bin"
    sed -i "s|ExecStart=${BAOCLAW_BIN}/baoclaw-core --daemon|ExecStartPre=${BAOCLAW_BIN}/mcp-servers start\nExecStart=${BAOCLAW_BIN}/baoclaw-core --daemon|" "$SYSTEMD_DIR/baoclaw.service"
    echo "✓ systemd service updated"
    echo "  Run: systemctl --user daemon-reload && systemctl --user restart baoclaw"
  fi
fi

echo ""
echo "═══════════════════════════════════════"
echo "  Installation complete!"
echo ""
echo "  Quick start:"
echo "    baoclaw              # terminal chat (auto-starts daemon)"
echo "    baoclaw-tui          # rich terminal UI"
echo "    baoclaw-web          # browser chat (http://localhost:8080)"
echo ""
echo "  Config: ~/.baoclaw/config.json"
echo "    { \"model_profiles\": { \"glm52\": { \"model\": \"glm-5.2\","
echo "        \"api_type\": \"anthropic\", \"api_key\": \"...\","
echo "        \"base_url\": \"...\", \"context_window\": 1000000 } },"
echo "      \"primary_profile\": \"glm52\" }"
echo ""
echo "  Optional: register daemon as system service"
echo "    Linux:   cp deploy/systemd/baoclaw.service ~/.config/systemd/user/"
echo "             systemctl --user enable --now baoclaw"
echo "    macOS:   cp deploy/launchd/com.baoclaw.daemon.plist ~/Library/LaunchAgents/"
echo "             launchctl load ~/Library/LaunchAgents/com.baoclaw.daemon.plist"
echo "    Windows: PowerShell -File deploy/windows/install.ps1"
echo ""
echo "  Docs: ~/.baoclaw/docs/USAGE.md"
echo "═══════════════════════════════════════"
