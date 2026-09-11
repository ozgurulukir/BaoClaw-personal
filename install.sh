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

# 2. Install workspace dependencies and build the CLI bundle.
# The repo is a single npm workspace root: one lockfile, one hoisted
# node_modules shared by all TS packages (ts-ipc + 4 gateways).
if [ ! -d "$SCRIPT_DIR/node_modules" ] || [ "$FORCE_DEPS" -eq 1 ]; then
  echo "📦 Installing workspace dependencies..."
  (cd "$SCRIPT_DIR" && npm ci --prefer-offline --no-audit --no-fund --silent) \
    || (cd "$SCRIPT_DIR" && npm install --prefer-offline --no-audit --no-fund --silent)
fi
if [ -d "$SCRIPT_DIR/ts-ipc" ]; then
  echo "⚡ Building fast CLI bundle (esbuild)..."
  npm run build -w baoclaw-ipc --silent
  echo "✓ CLI bundle ready (ts-ipc/dist/baoclaw.mjs)"
fi

# 3. Create install dirs
mkdir -p "$INSTALL_DIR/bin" "$BIN_DIR"

# 4. Copy Rust binary (unlink first to avoid ETXTBUSY if daemon is running)
rm -f "$INSTALL_DIR/bin/baoclaw-core"
cp "$SCRIPT_DIR/baoclaw-core/target/release/baoclaw-core" "$INSTALL_DIR/bin/baoclaw-core"
chmod +x "$INSTALL_DIR/bin/baoclaw-core"
echo "✓ Rust binary → $INSTALL_DIR/bin/"

# 5. Copy each gateway source
copy_gateway() {
  local name="$1"
  local src="$SCRIPT_DIR/$name"
  local dst="$INSTALL_DIR/$name"
  [ ! -d "$src" ] && return 0
  mkdir -p "$dst"

  # Copy all TS/TSX sources while preserving subdirectories (cli, gateway,
  # protocol, tui, src, etc.), excluding test files and build artifacts.
  (
    cd "$src"
    find . -type f \( -name "*.ts" -o -name "*.tsx" -o -name "*.d.ts" \) \
      ! -name "*.test.ts" ! -name "*.test.tsx" \
      ! -path "*/node_modules/*" ! -path "*/dist/*" | while IFS= read -r rel; do
        mkdir -p "$dst/$(dirname "$rel")"
        cp "$rel" "$dst/$rel"
      done
  )
  # public static assets (web)
  if [ -d "$src/public" ]; then
    mkdir -p "$dst/public" && cp -r "$src/public/." "$dst/public/"
  fi
  # Gateway-specific install hooks and patch-package patches
  if [ -d "$src/scripts" ]; then
    mkdir -p "$dst/scripts" && cp -r "$src/scripts/." "$dst/scripts/"
  fi
  if [ -d "$src/patches" ]; then
    mkdir -p "$dst/patches" && cp -r "$src/patches/." "$dst/patches/"
  fi
  # dist bundle directory
  if [ -d "$src/dist" ]; then
    mkdir -p "$dst/dist" && cp -r "$src/dist/." "$dst/dist/"
  fi
  # package metadata
  cp "$src/package.json" "$dst/" 2>/dev/null
  cp "$src/tsconfig.json" "$dst/" 2>/dev/null

  # Dependencies are NOT installed per package: the destination is a mini
  # workspace root and shares one hoisted node_modules (installed below).
  # Remove stale per-package node_modules from pre-workspaces installs so
  # old copies can never shadow the hoisted tree.
  rm -rf "$dst/node_modules"

  echo "✓ $name → $dst"
}
copy_gateway ts-ipc
copy_gateway baoclaw-telegram
copy_gateway baoclaw-web
copy_gateway baoclaw-feishu
copy_gateway baoclaw-whatsapp

# 6. Turn the destination into a mini workspace root and install its
# dependencies. cp -RP preserves the relative workspace symlinks inside
# node_modules (node_modules/baoclaw-ipc -> ../ts-ipc), which resolve
# correctly against the copied sibling package dirs.
# Refresh destination deps when missing, forced, or when the lockfile moved
# (compared BEFORE the new lockfile is copied over it).
DEST_DEPS_STALE=0
if [ ! -d "$INSTALL_DIR/node_modules" ]; then
  DEST_DEPS_STALE=1
elif ! cmp -s "$SCRIPT_DIR/package-lock.json" "$INSTALL_DIR/package-lock.json"; then
  DEST_DEPS_STALE=1
fi
[ "$FORCE_DEPS" -eq 1 ] && DEST_DEPS_STALE=1
cp "$SCRIPT_DIR/package.json" "$INSTALL_DIR/package.json"
cp "$SCRIPT_DIR/package-lock.json" "$INSTALL_DIR/package-lock.json"
[ -f "$SCRIPT_DIR/tsconfig.base.json" ] && cp "$SCRIPT_DIR/tsconfig.base.json" "$INSTALL_DIR/tsconfig.base.json"
if [ "$DEST_DEPS_STALE" -eq 1 ]; then
  echo "📦 Installing destination workspace dependencies..."
  rm -rf "$INSTALL_DIR/node_modules"
  cp -RP "$SCRIPT_DIR/node_modules" "$INSTALL_DIR/node_modules" 2>/dev/null \
    || (cd "$INSTALL_DIR" && npm ci --include=dev --prefer-offline --no-audit --no-fund --silent)
fi
# Lifecycle scripts did not run during the copy — probe the destination's
# native AES and apply the Baileys/libsignal patches there if needed.
if [ -d "$INSTALL_DIR/baoclaw-whatsapp" ]; then
  (cd "$INSTALL_DIR" && node baoclaw-whatsapp/scripts/apply-crypto-patch.cjs) \
    || echo "⚠️  crypto patch probe failed in $INSTALL_DIR — WhatsApp gateway may hit broken AES"
fi

# 7. Launcher functions
make_launcher() {
  local name="$1" target_subpath="$2" help="$3"
  cat > "$BIN_DIR/$name" << EOF
#!/bin/bash
# BaoClaw $name — $help
BAOCLAW_HOME="\${BAOCLAW_HOME:-\$HOME/.baoclaw}"
export BAOCLAW_CORE_BIN="\$BAOCLAW_HOME/bin/baoclaw-core"
# Exec the hoisted tsx bin directly: deterministic, no npx resolution or
# registry fallback.
exec "\$BAOCLAW_HOME/node_modules/.bin/tsx" "\$BAOCLAW_HOME/$target_subpath" "\$@"
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
  exec "$BAOCLAW_HOME/node_modules/.bin/tsx" "$BAOCLAW_HOME/ts-ipc/cli.ts" "$@"
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

# 9. Docs
mkdir -p "$INSTALL_DIR/docs"
[ -f "$SCRIPT_DIR/docs/USAGE.md" ] && cp "$SCRIPT_DIR/docs/USAGE.md" "$INSTALL_DIR/docs/" && echo "✓ docs/USAGE.md → $INSTALL_DIR/docs/"
[ -f "$SCRIPT_DIR/docs/DAEMON_MIGRATION.md" ] && cp "$SCRIPT_DIR/docs/DAEMON_MIGRATION.md" "$INSTALL_DIR/docs/" && echo "✓ docs/DAEMON_MIGRATION.md → $INSTALL_DIR/docs/"

# 10. Systemd service cleanup: MCP servers are now managed by the daemon
# itself; remove any leftover launcher hook and stale binary so the unit
# never references a file that no longer exists.
SYSTEMD_DIR="$HOME/.config/systemd/user"
if [ -d "$SYSTEMD_DIR" ] && [ -f "$SYSTEMD_DIR/baoclaw.service" ]; then
  if grep -q "ExecStartPre.*mcp-servers" "$SYSTEMD_DIR/baoclaw.service" 2>/dev/null; then
    sed -i "/ExecStartPre=.*mcp-servers/d" "$SYSTEMD_DIR/baoclaw.service"
    echo "✓ Removed obsolete mcp-servers hook from baoclaw.service"
    echo "  Run: systemctl --user daemon-reload"
  fi
fi
if [ -f "$INSTALL_DIR/bin/mcp-servers" ]; then
  rm -f "$INSTALL_DIR/bin/mcp-servers"
  echo "✓ Removed obsolete $INSTALL_DIR/bin/mcp-servers (daemon manages MCP servers now)"
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
