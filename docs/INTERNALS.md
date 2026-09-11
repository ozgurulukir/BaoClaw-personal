# BaoClaw Internals: How the Engine Works

> Extracted from the root README — see [README.md](../README.md) for the project overview.

This section describes the five core mechanisms that make BaoClaw tick: **Memory**, **Context**, **Evolution**, **System Prompt**, and **Model Fallback**. All are implemented in the Rust core engine (`baoclaw-core/src/engine/`).

---

### 1. 🧠 Memory Mechanism

BaoClaw has two complementary memory layers: **Long-Term Memory** (cross-session facts/preferences) and **Session Memory** (rolling summary within a conversation).

#### Long-Term Memory (`memory.jsonl`)

| Aspect         | Detail                                                                                         |
| -------------- | ---------------------------------------------------------------------------------------------- |
| **Scope**      | Two levels: global (`~/.baoclaw/memory.jsonl`) and project (`<project>/.baoclaw/memory.jsonl`) |
| **Categories** | `fact` (user told me X), `preference` (user prefers Y), `decision` (we decided Z)              |
| **Storage**    | Append-only JSONL, one JSON object per line, owner-only permissions (0600)                     |
| **Injection**  | Loaded at daemon startup → `build_prompt_fragment()` → appended to system prompt               |
| **Recall**     | `MemorySearch` tool — keyword search over the store; hits get a decay recall boost             |
| **Management** | `/memory add`, `/memory list`, `/memory delete`, `/memory clear`                               |

When the daemon starts, `MemoryStore::load()` reads the global
`~/.baoclaw/memory.jsonl`, and `build_prompt_fragment()` generates a formatted
block that becomes part of the `append_system_prompt` injected into every
conversation turn. Project-level `<project>/.baoclaw/memory.jsonl` is supported
by the store API but is not currently loaded into the prompt.

**Bounded prompt fragment:** the fragment does NOT dump the whole store.
Entries are ranked by decayed importance (`importance × decay_rate^days`) and
rendered within a character budget (`memory.prompt_char_budget`, default
6000). The header carries a usage meter (`[3/21 memories · 480/6000 chars]`),
and when entries don't fit, a trailing note points the model at the
`MemorySearch` tool. Exact-content duplicates are dropped at load time.

**Single writer & non-blocking I/O:** `MemoryTool` and `MemorySearchTool` share the daemon's
in-memory `MemoryStore` instance — saves are validated (`validate_memory_content`,
so credentials/injection text never enter the store), deduplicated (an exact
re-save is idempotent), and visible to the prompt fragment without a restart.
All disk writes and rewrites use asynchronous `tokio::fs` with atomic temporary-file
rename (`.tmp` sibling rename) and owner-only (0600) permissions. In-memory locks
are released before disk I/O, preventing thread pool stalls and lock contention.
Hand-editing `memory.jsonl` while the daemon runs requires a restart to be seen.

**Live recall:** `MemorySearch` gives the model keyword search over everything
that didn't fit (or wasn't worth always-on injection). Matches are scored by
keyword coverage + importance, and every returned entry is recorded as a
recall (`recall_count`, `last_recalled_at`, importance boost) — the signal
that keeps frequently-used memories off the age-only decay→archive path.

**Write-path etiquette (MemoryTool):** `content` (one declarative sentence),
`category`, and an optional `importance` (0.0–1.0, default 0.5) — importance
ranks entries in the fragment and slows their decay.

#### Session Memory (`session_memory.rs`)

A per-session rolling summary that persists across the lifetime of a session — like meeting notes that get refined over time.

| Aspect               | Detail                                                                                                                |
| -------------------- | --------------------------------------------------------------------------------------------------------------------- |
| **Storage**          | `~/.baoclaw/sessions/{session_id}.memory.md`                                                                          |
| **First update**     | Triggers at **6 messages** (if summary is empty)                                                                      |
| **Refresh interval** | Every **10 messages** after the last update                                                                           |
| **Stall protection** | The interval baseline re-anchors when history shrinks (compaction used to freeze updates for the rest of the session) |
| **Trigger sites**    | After tool-call turns AND on text-only final turns                                                                    |
| **Thread safety**    | `std::sync::Mutex` — safe to share via `Arc<SessionMemory>`                                                           |
| **Persistence**      | Written to disk on every `update()` call                                                                              |

**How it's used:**

1. **Free compaction** — `session_memory_compact()` uses the existing summary to replace old messages without any API call (keeps last 10 messages)
2. **Dynamic reminder** — injected into `<system-reminder>` alongside git status, once per **user turn**; tool-result continuation turns inside a task don't re-receive it (re-appending it after every tool call made the model re-acknowledge the same summary each turn)
3. **Session resume** — **surface-scoped**: a session only ever resumes its own transcript (`{cwd_hash}-{surface}` exact match first, then the same surface's newest), never another surface's; snapshot restores seed the summary only when the session's own `.memory.md` is empty, so stale snapshot copies can't overwrite fresher files

**Freshness marker:** when the summary is more than **10 messages** behind the
live history, the dynamic reminder adds a `# Summary Freshness` note ("last
updated N messages ago; later work may be missing") so the model weighs it as
a snapshot instead of current fact. Right after a restore the age is unknown,
so no note is shown rather than a misleading one.

**Summary freshness:** the summarizer input keeps the **most recent ~40K chars**
of the conversation (so the summary tracks current work rather than freezing
on the session's opening minutes), and each tool-result block inside it is
elided to a verbatim 400-char head (`[...N chars elided]`) — raw tool JSON
used to crowd the actual conversation out of the 40K window. The summary
itself follows a fixed section layout (**Task Overview / Current State / Key
Discoveries / Next Steps / Context to Preserve**) with a copy-literals-
verbatim rule, so exact identifiers, paths and error messages survive
compaction.

---

### 2. 📐 Context Mechanism

BaoClaw targets a 200K-token context window with a multi-layer compaction strategy and calibrated token counting.

#### Token Counting (`token_counter.rs`)

| Aspect                     | Detail                                                                                 |
| -------------------------- | -------------------------------------------------------------------------------------- |
| **Context window**         | 200,000 tokens (default)                                                               |
| **Auto-compact threshold** | 70% = **140,000 tokens**                                                               |
| **Tokenizer**              | `cl100k_base` (GPT-4 tokenizer, ~5-10% over-count for Claude)                          |
| **Counting strategy**      | Calibrated from API response `usage.input_tokens` → anchored baseline + tiktoken delta |
| **Baseline persistence**   | `~/.baoclaw/sessions/{id}.baseline.json` — restores calibration after restart          |

**Budget levels (for 200K window):**

| Level        | Threshold    | Action                            |
| ------------ | ------------ | --------------------------------- |
| **Normal**   | < 140K       | Continue normally                 |
| **Compact**  | ≥ 140K (70%) | Pre-emptive compaction triggered  |
| **Warning**  | ≥ 147K       | Log warning                       |
| **Blocking** | ≥ 164K       | MUST compact before next API call |

#### 5-Level Compaction Hierarchy

Compaction is tried from cheapest to most expensive:

```
┌─────────────────────────────────────────────────────────────────┐
│ Level 1: micro_compact  (FREE, every turn)                      │
│   • Clears tool_result content > 8192 chars AND > 24 h old      │
│     (config: micro_compact_min_chars / _min_age_secs)           │
│   • Skips last 4 messages (current turn)                        │
│   • Replacement names the tool: "[Old tool result cleared —     │
│     Bash output, originally N chars]"                           │
├─────────────────────────────────────────────────────────────────┤
│ Level 2: session_memory_compact  (FREE, no API call)            │
│   • Uses existing SessionMemory rolling summary                 │
│   • Keeps last 10 messages, prepends CompactBoundary           │
│   • Triggered when budget = Compact/Blocking                    │
├─────────────────────────────────────────────────────────────────┤
│ Level 3: compact_messages  (1 API call, cache-safe)             │
│   • Keeps the last 8-30 messages (adaptive, starts at 10; the   │
│     tracker adjusts from post-compact token ratios), summarizes │
│     older ones via API                                          │
│   • Cache-safe forking: reuses system prompt + old messages     │
│     as API messages → cache prefix reuse on provider side       │
│   • Summary input truncated to 60,000 chars (~15K tokens)       │
│   • Circuit breaker: skipped after 3 consecutive failures       │
├─────────────────────────────────────────────────────────────────┤
│ Level 4: reactive_compact  (FREE, last resort)                  │
│   • Groups messages into turns, drops oldest 20%                │
│   • Guard: won't drop if ≤ 4 messages or ≤ 2 turns             │
├─────────────────────────────────────────────────────────────────┤
│ Level 5: inline compact  (on context_overflow error)            │
│   • Triggered when API returns "model_context_window_exceeded"  │
│   • Keeps last 4 messages, summarizes old via inline API call   │
│   • Retries the query with compacted context                    │
└─────────────────────────────────────────────────────────────────┘
```

#### System Prompt Architecture

The system prompt is split into **static** (cached) and **dynamic** (per-turn) parts to maximize API prompt caching:

**Static part** (`build_system_prompt()`) — tagged with `cache_control: ephemeral`:

1. Core system prompt (or custom override)
2. Working directory + "show full content" instruction
3. Project instructions from `BAOCLAW.md`
4. Project rules from `.baoclaw/rules/*.md` (path-filtered against recent files)
5. Append system prompt = **skills** + **long-term memory** + **evolution prompt**

**Dynamic part** (`build_dynamic_reminder()`) — injected into the **last user message** as `<system-reminder>`:

1. Git status (branch, staged/modified/untracked files)
2. Session memory (rolling summary)

This split ensures the cached system prompt prefix stays stable across turns — only the dynamic reminder changes.

#### Session Resume Flow — Summary-First Three-Tier Strategy

```
1. find_latest_session_for_cwd(cwd, session_id)
     → FNV-1a hash of cwd → scan ~/.baoclaw/sessions/ for matching .jsonl,
       restricted to the caller's own surface (exact id first, then the
       same `{cwd_hash}-{surface}` suffix) — cross-surface resumes never happen
2. TranscriptWriter::load_async(session_id) → read all entries asynchronously
3. SessionMemory::load(session_id) → check .memory.md for existing summary
4. Three-tier loading (10 min → < 5 sec):
   Tier 1 (best):   summary exists → load summary + last 200 entries only
   Tier 2 (small):  entries ≤ 400, no summary → safe to rebuild all
   Tier 3 (fallback): large session, no summary → last 200 entries + warning
5. engine.set_messages(messages)
6. engine.load_token_baseline(session_id)   // restore calibrated count
7. engine.seed_session_memory(&old_summary) // carry forward to new session
```

**Turn-aligned tails:** the 200-entry cuts in Tier 1 and Tier 3 snap back to
the nearest complete user turn (`align_cut_to_user_turn`), so a restored tail
never opens with an assistant tool call whose result was cut off — that would
be an orphan the API rejects.

**Main session:** the daemon's own engine uses a deterministic `{cwd_hash}-main`
session id — it never adopts a client surface's transcript (an older
implementation reused the newest transcript for the cwd, letting a busy
gateway surface hijack the main session's snapshot and memory). Legacy
8-char-hash `-main` sessions are migrated forward on boot.

**Background summary generation** ensures Tier 1 is always available:

- First update at 6 messages, then every 10 messages (`tokio::spawn`, non-blocking)
- Session close heuristic fallback if background never ran
- Pre-query compact safety: >500 messages → `session_memory_compact` (free) or tail-trim (no API call)

---

### 3. 🔄 Evolution Mechanism

The experimental self-evolution engine records interaction data and can create or improve reusable skill candidates.

#### File Layout

```
~/.baoclaw/evolution/
├── trajectories.jsonl          # Every interaction record (append-only)
├── session_summaries.jsonl     # Structured summary per session close
├── skill_stats.json            # Per-skill usage tracking
├── pending_review.json         # Cross-session review → next session's prompt
├── pending_eval.json           # Self-evaluation nudge (one-shot, consumed)
└── candidates/
    └── {skill-name}.json       # Auto-extracted skill candidates
```

#### Key Thresholds

| Constant                   | Value        | Purpose                                                      |
| -------------------------- | ------------ | ------------------------------------------------------------ |
| `SKILL_CREATION_THRESHOLD` | 3 tool calls | Min complexity to auto-extract a skill candidate             |
| `SELF_EVAL_INTERVAL`       | 15 tasks     | Trigger self-evaluation nudge                                |
| Review trigger             | ≥ 2 turns    | Only generate `pending_review.json` if session had ≥ 2 turns |
| Candidate name max         | 60 chars     | Slugified from user prompt                                   |
| Topic truncation           | 200 chars    | Per topic in session summary                                 |

#### Evolution Lifecycle

**During interaction** (`record_trajectory`):

```
Every user interaction
    ├── Append trajectory to trajectories.jsonl
    ├── Increment task_count
    ├── IF tool_count ≥ 3 AND outcome = Completed:
    │     → Auto-extract SkillCandidate → save to candidates/{name}.json
    └── Every 15 tasks → write pending_eval.json (one-shot nudge)
```

**Session close** (`on_session_close`) — pure Rust, no LLM call:

```
Last client disconnects
    ├── Extract from message history:
    │     user_topics, tool_usage frequency, errors, skills_used
    ├── Write session_summaries.jsonl
    └── IF turn_count ≥ 2:
          Write pending_review.json (for next session's system prompt)
```

**System prompt injection** (`build_prompt_fragment`):

```
Start of new session
    ├── Check pending_review.json from previous session
    │     → Generate "Last Session Review" with self-improvement nudges
    ├── Check pending_eval.json
    │     → Generate "Self-Evaluation Nudge"
    ├── List pending skill candidates
    └── All injected into append_system_prompt → system prompt layer 5
```

**Skill promotion** (`promote_skill`):

```
Candidate approved → Move from candidates/ to ~/.baoclaw/skills/{name}.md
                     Remove candidate file
                     Skill loaded in all future sessions
```

**Training export** (`export_training_data`):

```
Read all trajectories → Create preference pairs
    Each pair: { prompt, response, rating: chosen/rejected/neutral }
    Output: ~/.baoclaw/evolution/training_export.jsonl
    Can be adapted for DPO/RLHF fine-tuning
```

#### Full Evolution Loop

```
 Use BaoClaw ────→ Trajectories recorded (every interaction)
      │                     │
      │                     ▼
      │            Complex task succeeds? (≥3 tools)
      │                 │          │
      │                Yes         No
      │                 │          │
      │                 ▼          ▼
      │         Extract skill   (skip)
      │         candidate
      │                 │
      │                 ▼
      │        Every 15 tasks → Self-evaluation nudge
      │                 │
      │                 ▼
      │        Agent creates/improves skills (via Evolve tool)
      │                 │
      │                 ▼
      │        Skills loaded in next session
      │                 │
      │                 ▼
      │        Better performance → Loop continues
      │                 │
      ▼                 ▼
 Session closes → on_session_close()
      │
      ├── Write session_summaries.jsonl
      ├── Write pending_review.json → next session prompt
      │
      ▼
  Export trajectories → data suitable for adapting DPO/RLHF datasets
```

---

### 4. 📋 System Prompt Construction

The system prompt is assembled in 5 ordered layers. The order matters for API prompt caching — stable layers come first:

```
┌──────────────────────────────────────────────────────────┐
│ Layer 1: Core System Prompt                              │
│   • Default: "You are a helpful AI coding assistant."    │
│   • Override: custom_system_prompt in config             │
│   • Includes: "show full content" instruction            │
│   • Cache: cache_control = ephemeral                     │
├──────────────────────────────────────────────────────────┤
│ Layer 2: Working Directory                               │
│   • Current cwd path                                     │
│   • Instructs agent to output full file content          │
├──────────────────────────────────────────────────────────┤
│ Layer 3: Project Instructions                            │
│   • From BAOCLAW.md (project root or .baoclaw/)          │
│   • Loaded once, cached across turns                     │
│   • Also loads .baoclaw/rules/*.md (path-filtered)       │
├──────────────────────────────────────────────────────────┤
│ Layer 4: Append System Prompt                            │
│   • Skills (personal ~/.baoclaw/skills/ + project)       │
│   • Long-term memory (facts, preferences, decisions)     │
│   • Evolution prompt (pending reviews, skill candidates) │
├──────────────────────────────────────────────────────────┤
│ Dynamic <system-reminder> (in last USER message)         │
│   • NOT in system prompt — preserves cache stability     │
│   • Git status (branch, changed files)                   │
│   • Session memory (rolling summary)                     │
└──────────────────────────────────────────────────────────┘
```

**Why this split?** The static layers (1-4) are tagged with `cache_control: ephemeral` so the API provider can cache the prefix. Only the dynamic `<system-reminder>` changes every turn — and it's injected into the user message, not the system prompt, so the system prompt cache stays warm.

**How skills & memory are loaded:**

1. At daemon startup: `load_skills_for_prompt(cwd)` → discovers all skill `.md` files; `MemoryStore::load()` → reads global `~/.baoclaw/memory.jsonl`
2. Skills are frozen into the append prompt at startup (they don't change mid-session)
3. Long-term memory and evolution nudges are the exception: the engine re-renders that fragment **once per query** (`query_engine.rs`), so mid-session `MemoryTool` saves and pending reviews reach the model without a restart — but they change rarely enough that the prefix cache stays warm between changes

---

### 5. 🔁 Model Fallback Mechanism

When the primary model is unavailable or rate-limited, BaoClaw automatically falls back through a configurable chain of models.

#### Fallback Chain

```
Request with primary model
    │
    ▼
┌─ Rate Limited (429)? ─── Yes ──→ retry_count < max_retries?
│                                   │              │
│                                  Yes             No
│                                   │              │
│                                   ▼              ▼
│                              Retry with      Next model in chain?
│                              exponential       │           │
│                              backoff          Yes          No
│                                   │            │           │
│◀──────────────────────────────────┘            ▼           ▼
│                                          Fallback to    EXHAUSTED
│                                          next model     (all tried)
│                                          (reset counters)
│
├─ Server Error (5xx)? ──── Yes ──→ server_error_count < 3?
│                                   │              │
│                                  Yes             No
│                                   │              │
│                                   ▼              ▼
│                              Retry with      Fallback chain
│                              backoff         (same as above)
│                              (1s, 2s, 4s)
│
├─ Context Overflow? ────── Yes ──→ Try compaction first
│                                   │
│                                   ▼
│                              compact_messages() or reactive_compact()
│                                   │
│                              Retry with compacted context
│
└─ Success ◀────────────── Return response
```

#### Configuration

```json
{
  "model": "claude-sonnet-4-20250514",
  "fallback_models": ["claude-3-5-haiku-20241022"],
  "max_retries_per_model": 2
}
```

| Parameter                | Default                    | Description                            |
| ------------------------ | -------------------------- | -------------------------------------- |
| `model`                  | `claude-sonnet-4-20250514` | Primary model (tried first every time) |
| `fallback_models`        | `[]`                       | Ordered list of fallback models        |
| `max_retries_per_model`  | `2`                        | Retries per model before falling back  |
| Server error max retries | `3`                        | Built-in limit for 5xx errors          |

#### Error Recovery Strategies

| Error Type                         | Strategy           | Parameters                         |
| ---------------------------------- | ------------------ | ---------------------------------- |
| IPC disconnect                     | Restart process    | Full daemon restart                |
| State sync failed                  | Full state sync    | Re-sync from scratch               |
| API rate limited (429)             | Retry with backoff | 3 attempts, 1s initial delay       |
| API server error (5xx)             | Retry with backoff | 3 attempts, exponential (1s→2s→4s) |
| API auth error                     | Fatal              | Cannot recover automatically       |
| API bad request (context overflow) | Auto-compact       | Compact → retry                    |
| Tool timeout                       | Fatal              | Report to user                     |

**Key behaviors:**

- The fallback controller **resets** to the primary model for each new query (cross-turn stateless)
- **Exponential backoff** prevents hammering a rate-limited endpoint
- **Circuit breaker**: After 3 consecutive compaction failures, auto-compaction is disabled to avoid wasting API calls
- **5-minute timeout** per API call — on timeout, the user message is removed to keep history clean

---

### 6. 🔌 MCP Client (stdio, SSE, HTTP)

`src/mcp/` turns configured MCP servers into first-class engine tools. The
engine reads tools from a live `ToolRegistry` (`src/tools/registry.rs`):
builtin head → MCP buckets (one per server) → pinned tail (AgentTool,
ToolSearchTool). Consumers snapshot per unit of work — per query for the
session engine, per construction for sub-agents/tasks/teams, per job for
cron — so a mid-flight refresh never splits a query's tool array.

#### Boot flow

```
main(): discover_mcp_servers(cwd)            — mcp.json × 5 locations, first-name-wins dedup
      → ConnectionManager::start_all         — per-slot supervisor tasks
      → build_engine_registry(...)           — builtin head + attach registry
      → manager.boot_and_register()          — one bridge bucket per slot with a catalog
      → registry.install_tail(...)           — AgentTool (registry view) + ToolSearchTool
```

#### Constraints and mechanisms

| Aspect           | Detail                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| ---------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Live catalog** | `notifications/tools/list_changed` (or `/mcp refresh`) triggers a bounded re-fetch; unchanged catalogs are not republished. The registry swaps one server's bucket atomically; consumers adopt it at their next snapshot. A changed tool set costs one full-price prompt-prefix re-read per session that adopts it — the per-query snapshot boundary keeps in-flight queries stable. Failed slots park after `mcp_max_restarts` and revive via `/mcp refresh` (manual retry resets the budget)                                                 |
| **Transports**   | `stdio` — spawn + NDJSON over the child's stdio (env sanitization: sensitive daemon keys removed, configured `env` wins; stderr drained or the server would deadlock). `http` (Streamable HTTP) — every message POSTs to the fixed endpoint; replies come back inline JSON or as an SSE stream through the same demux; `Mcp-Session-Id` echoed after initialize; no standing GET stream (server-initiated messages outside POST responses are not observed). `sse` (legacy) — standing GET stream; the `endpoint` event announces the POST URL |
| **Handshake**    | `initialize` → `notifications/initialized` → `tools/list` with cursor pagination capped at 100 pages. `initialize` and the whole catalog fetch are EACH bounded by `mcp_startup_timeout_secs` (boot stall per server ≈ 2 budgets); writes carry the same bound so a server that stops reading fails calls instead of wedging the pipe. Protocol: `2024-11-05` for stdio/legacy SSE, `2025-03-26` for Streamable HTTP (response version logged, never enforced)                                                                                 |
| **Reconnect**    | Every failed connect counts toward `mcp_max_restarts`; the budget exhausted, the slot parks as Failed (fail-storm protection) and revives via `/mcp refresh`, which retries immediately with a fresh budget. While disconnected the registered tools return error results telling the model the set may have changed and to retry                                                                                                                                                                                                              |
| **Demux**        | One reader task per client multiplexes by JSON-RPC id (pending oneshot map); late responses after timeout/abort are dropped. Server→client requests: `ping` answered `{}`, everything else rejected `-32601` (sampling/roots are out of scope). Non-JSON lines (banners, logs) are skipped; stdio stderr is drained or the server would deadlock on a full pipe                                                                                                                                                                                |
| **Cancellation** | Bridges declare `aborts_internally()`: the executor skips its own select (a dropped future could skip cleanup) and the client, on the abort signal, sends `notifications/cancelled {requestId}` (best-effort, 2s budget) and fails deterministically with `ToolError::Aborted`. If the response wins the race, it is returned normally                                                                                                                                                                                                         |
| **Deferred**     | `mcp_deferred_tools` (default true): MCP tools advertise as stubs — `{name, short description, input_schema: {"type":"object","properties":{}}}`, no wire defer field (third-party gateways reject unknown fields). Once the model invokes the tool or surfaces it via tool search, the full schema rides every request for the rest of the query (`expanded_tools`, reset per query). The cache breakpoint sits after the non-deferred group, so activations never invalidate the prefix                                                      |
| **Shutdown**     | `shutdown_all()` runs before `std::process::exit` on the signal path — destructors do not run there, so `kill_on_drop` alone would orphan children. stdio: close stdin → 2s grace → kill; HTTP: best-effort session teardown                                                                                                                                                                                                                                                                                                                   |
| **Secrets**      | mcp.json `env` and `headers` values ride `McpServerInfo` with `skip_serializing` (never in `listMcpServers`) and are never logged. Transport defaults are applied first so explicit config wins; reserved transport headers (`host`, `content-type`, ...) cannot be overridden via config                                                                                                                                                                                                                                                      |
| **Permissions**  | Bridges leave the trait defaults: fail-closed Ask for every call, sequential execution. Whole-tool allow rules work by registry name. Consequence: mutating MCP tools are unavailable on headless engines (cron/teams/sub-agents) unless allow rules pre-exist — same semantics as builtins                                                                                                                                                                                                                                                    |
| **Naming**       | `mcp__<server>__<tool>`, segments sanitized to `[A-Za-z0-9_-]`; `scm_list_tools` classifies them as `"type": "mcp"` by prefix and carries a `deferred` marker                                                                                                                                                                                                                                                                                                                                                                                  |
| **tool-health**  | Error results (including "server disconnected") feed the failure counters, so a dead server's tools auto-disable after repeated failures. Every Ready transition resets the slot's tool-health records — a recovered server's tools are never left hard-blocked by stale transport failures                                                                                                                                                                                                                                                    |
| **Session cwd**  | stdio servers spawn with the daemon's launch cwd and are never respawned on cwd change; project `mcp.json` is resolved against the launch cwd                                                                                                                                                                                                                                                                                                                                                                                                  |

---

### Data Flow Summary

```
User Input
    │
    ▼
main.rs (loads skills + memory + evolution → append_system_prompt)
    │
    ▼
QueryEngine.submit_message_with_attachments()
    ├── Token budget check → auto-compact if needed
    │     ├── session_memory_compact()  (free)
    │     └── compact_messages()        (1 API call, cache-safe)
    │
    └── tokio::spawn(run_query_loop)
          │
          ▼  Per Turn:
          ├── micro_compact()              (every turn, free)
          ├── Budget status check          → may trigger compaction
          ├── build_system_prompt()        → static cached prefix (5 layers)
          ├── build_dynamic_reminder()     → inject into last user message
          ├── FallbackController           → model selection
          │     ├── 429 → retry / fallback
          │     ├── 5xx → retry / fallback
          │     └── context_overflow → compact + retry
          ├── validate_and_fix_tool_messages() → strip unpaired blocks
          ├── UnifiedClient.stream()       → Anthropic or OpenAI
          ├── Tool execution               → emit events
          ├── SessionMemory.should_update() → update summary if interval met
          └── TranscriptWriter.append()    → persist to JSONL
                │
                ▼  Session Close:
          EvolutionEngine.on_session_close()
                ├── Write session_summaries.jsonl
                ├── Write pending_review.json  (→ next session)
                └── Skill extraction if applicable
```

### Tool-Use / Tool-Result Pairing Hygiene

A user message whose `tool_result` has no matching assistant `tool_use` (and
the reverse) is rejected by the API, so pairing is repaired at two stages:

| Stage                 | Where                              | Action                                                                                                                                                                       |
| --------------------- | ---------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **History load**      | `cleanup_incomplete_tool_calls()`  | Strips results whose call vanished (older-format transcripts, manual edits) and converts string-content user turns to block arrays, stubbing unrecoverable results as errors |
| **Before every call** | `validate_and_fix_tool_messages()` | Re-checks with the same predicate and drops anything still unpaired, so malformed sequences never reach the API                                                              |

Both stages share one orphan definition (`collect_tool_ids` +
`strip_orphan_tool_result_blocks`), so a result can never be orphaned by one
stage's cut and rescued by the other's rule.

### Event Delivery and Terminal Hand-Off

Stream events reach a client connection through two paths: the turn's
drain task writes them directly to the submitting connection, and a
per-client broadcast task forwards the shared broadcast channel to every
_other_ connected client. The broadcast task skips events while the
submitter lock is held — but the lock is released after the drain's final
disk sync, so without extra bookkeeping the broadcast task could observe a
terminal `Result`/`Error` event _after_ the release and write it a second
time. Clients that send one answer per terminal event (chat gateways)
would then show the assistant message twice.

The fix is deterministic: the drain arms a per-client terminal hand-off
flag _before_ broadcasting a terminal event, and the receiving broadcast
task consumes the flag instead of writing. Because the flag is always set
before the event can enter the broadcast channel, delivery is decided by
ordering, never by timing. `clearSession` follows the same
conversation-scoped discipline: it empties the in-memory messages,
truncates the JSONL transcript and writes a fresh snapshot, while
long-term memory files (`{id}.memory.md`, the shared memory store) stay
intact.

## See also

- [Permission system](PERMISSIONS.md)
- [Important files tour](IMPORTANT_FILES.md)
- [Configuration reference](CONFIGURATION.md)
