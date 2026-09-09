# Attribution

This file covers four things: provenance, design influences, third-party
dependencies, and bundled components. The authoritative dependency
inventories are `baoclaw-core/Cargo.lock` and `package-lock.json` —
transitive dependencies inherit their packages' licenses. License data below
was verified against crates.io and installed package metadata on 2026-09-09.

## Provenance

This repository is a fork of
[baohx/BaoClaw](https://github.com/baohx/BaoClaw) (first published
2026-04-02). The upstream README declares the MIT license but ships no
LICENSE file; the root [`LICENSE`](LICENSE) here records the upstream MIT
grant (with its copyright) alongside the copyright for this fork's
modifications.

## Design influences

The long-term memory design (budgeted prompt fragment, recall search, curated
writes) and the self-evolution learning loop draw on ideas studied in:

- [NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent) (MIT)
- [XiaomiMiMo/MiMo-Code](https://github.com/XiaomiMiMo/MiMo-Code) (MIT)

No code was copied from either project; all implementations are original.

## Rust core (`baoclaw-core`)

| Crate                      | License           |
| -------------------------- | ----------------- |
| tokio, tokio-stream        | MIT               |
| serde, serde_json          | MIT OR Apache-2.0 |
| reqwest                    | MIT OR Apache-2.0 |
| tiktoken-rs                | MIT               |
| rand, futures, async-trait | MIT OR Apache-2.0 |
| bytes                      | MIT               |
| uuid, thiserror, chrono    | Apache-2.0 OR MIT |
| glob, regex, base64        | MIT OR Apache-2.0 |
| urlencoding                | MIT               |
| ignore                     | Unlicense OR MIT  |
| jsonwebtoken               | MIT               |
| libc                       | MIT OR Apache-2.0 |
| rusqlite                   | MIT               |
| docx-rs                    | MIT               |
| mimalloc                   | MIT               |
| windows-service            | MIT OR Apache-2.0 |

Bundled components:

- **SQLite** — `rusqlite` is built with the `bundled` feature, so the SQLite
  engine (public domain) is compiled into the binary.
- **tiktoken BPE data** — `tiktoken-rs` embeds the tokenizer encodings from
  OpenAI's tiktoken (MIT).

## TypeScript services (npm workspaces)

Runtime dependencies:

| Package                 | License      | Used by                       |
| ----------------------- | ------------ | ----------------------------- |
| @whiskeysockets/baileys | MIT          | baoclaw-whatsapp              |
| libsignal (via baileys) | **GPL-3.0**  | baoclaw-whatsapp              |
| aes-js                  | MIT          | baoclaw-whatsapp              |
| qrcode-terminal         | Apache-2.0   | baoclaw-whatsapp              |
| socks-proxy-agent       | MIT          | baoclaw-whatsapp              |
| node-telegram-bot-api   | MIT          | baoclaw-telegram              |
| ws                      | MIT          | baoclaw-web                   |
| ink                     | MIT          | ts-ipc (TUI)                  |
| react                   | MIT          | ts-ipc (TUI)                  |
| mammoth                 | BSD-2-Clause | ts-ipc, baoclaw-telegram      |
| pdf-parse               | Apache-2.0   | ts-ipc, baoclaw-telegram      |
| pdfkit                  | MIT          | baoclaw-telegram, baoclaw-web |

Build and development tooling: esbuild, eslint, @typescript-eslint/\*,
eslint-config-prettier, husky, lint-staged, prettier, tsx (all MIT);
typescript (Apache-2.0).

### Copyleft notice

`libsignal` (Signal's protocol library, GPL-3.0) enters through
`@whiskeysockets/baileys` and carries local modifications applied at install
time via patch-package (`baoclaw-whatsapp/patches/`). Running the WhatsApp
gateway privately is unaffected; **distributing** it would trigger GPL-3.0
obligations for that component and its modifications.
