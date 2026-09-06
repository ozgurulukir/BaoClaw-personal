# BaoClaw Feishu Gateway

Feishu (Lark) bridge for the BaoClaw daemon: allowlisted chats can chat with
the project session bound to the gateway's working directory, run slash
commands, and answer interactive **tool-permission prompts**.

## How it works

```
Feishu event ──▶ lark-cli event consume im.message.receive_v1 ──▶ gateway.ts
gateway.ts   ──▶ lark-cli im +messages-send ──────────────────▶ Feishu chat
gateway.ts   ──▶ Unix socket ──▶ BaoClaw daemon (shared session, tag "feishu")
```

All Feishu I/O goes through the external **`lark-cli`** subprocess — there is
no Feishu SDK dependency in this package.

## Requirements

1. **`lark-cli`** (the official [Lark CLI](https://github.com/larksuite/cli))
   on `PATH`. Known-good version: **v1.0.93**. Install the release binary:

   ```bash
   # Linux amd64 example — pick the asset for your platform
   curl -sLO https://github.com/larksuite/cli/releases/download/v1.0.93/lark-cli-1.0.93-linux-amd64.tar.gz
   tar xzf lark-cli-1.0.93-linux-amd64.tar.gz
   install -m 0755 lark-cli ~/.local/bin/lark-cli
   ```

2. **A lark-cli profile named `baoclaw`** holding your Feishu app credentials
   (app id + secret of the bot). `start.sh` switches to it on launch and
   restores your previous profile on exit:

   ```bash
   lark-cli profile add baoclaw   # follow the prompts with your app id/secret
   ```

3. **An allowlist** — `feishu.allowedChatIds` in `~/.baoclaw/config.json`
   (array of chat `oc_…` IDs). Empty or missing means the gateway refuses to
   start.

4. The bot must be a member of every allowlisted chat.

## Interactive permission prompts

When the daemon needs approval for a tool, the gateway sends an **interactive
card** with Allow / Always-allow / Deny buttons (lark-cli
`--msg-type interactive`) and streams the clicks from a second
`card.action.trigger` consumer. Decisions ride a dedicated control-channel
connection to the daemon, so they resolve mid-turn.

Graceful degradation — the feature needs a recent lark-cli:

- At startup the gateway probes `event consume card.action.trigger --dry-run`.
  If the installed lark-cli does not support it, the gateway logs a warning
  and stays **text-only**: prompts are plain text answered with the reply
  keywords `yes` / `always` / `no`.
- If a single card send fails (or the card consumer dies at runtime), the
  gateway falls back to the text prompt the same way.

Unanswered prompts expire after 60 s (denied with the daemon), so a turn is
never parked for long. The daemon-side timeout is configurable — see
[`docs/PERMISSIONS.md`](../docs/PERMISSIONS.md) (`ask_timeout_secs`) and
`/permissions timeout <seconds>` in the CLI.

## Run

```bash
./start.sh              # foreground (Ctrl+C to stop)
./start.sh --daemon     # background
./start.sh --stop       # stop
./start.sh --status     # status
./start.sh --logs       # tail the log
./start.sh --debug      # verbose logging
```

`start.sh` checks for `lark-cli` before launching and refuses to start with a
pointer to the install instructions above.

## Development

```bash
npm run typecheck   # tsc --noEmit
npm test            # node:test via tsx (permission, authorization, formatter)
```
