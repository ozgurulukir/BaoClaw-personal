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
| **Storage**    | Append-only JSONL, one JSON object per line                                                    |
| **Injection**  | Loaded at daemon startup → `build_prompt_fragment()` → appended to system prompt               |
| **Management** | `/memory add`, `/memory list`, `/memory delete`, `/memory clear`                               |

When the daemon starts, `MemoryStore::load()` reads the global
`~/.baoclaw/memory.jsonl`, and `build_prompt_fragment()` generates a formatted
block that becomes part of the `append_system_prompt` injected into every
conversation turn. Project-level `<project>/.baoclaw/memory.jsonl` is supported
by the store API but is not currently loaded into the prompt.

#### Session Memory (`session_memory.rs`)

A per-session rolling summary that persists across the lifetime of a session — like meeting notes that get refined over time.

| Aspect               | Detail                                                      |
| -------------------- | ----------------------------------------------------------- |
| **Storage**          | `~/.baoclaw/sessions/{session_id}.memory.md`                |
| **First update**     | Triggers at **6 messages** (if summary is empty)            |
| **Refresh interval** | Every **10 messages** after the last update                 |
| **Thread safety**    | `std::sync::Mutex` — safe to share via `Arc<SessionMemory>` |
| **Persistence**      | Written to disk on every `update()` call                    |

**How it's used:**

1. **Free compaction** — `session_memory_compact()` uses the existing summary to replace old messages without any API call (keeps last 10 messages)
2. **Dynamic reminder** — injected into `<system-reminder>` in the user message each turn alongside git status
3. **Session resume** — when reconnecting, the old summary seeds the new session and triggers immediate compaction if > 50 messages

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
│   • Clears tool_result content > 500 chars AND > 60 min old    │
│   • Skips last 4 messages (current turn)                        │
│   • Replacement text: "[Old tool result cleared — N chars]"     │
├─────────────────────────────────────────────────────────────────┤
│ Level 2: session_memory_compact  (FREE, no API call)            │
│   • Uses existing SessionMemory rolling summary                 │
│   • Keeps last 10 messages, prepends CompactBoundary           │
│   • Triggered when budget = Compact/Blocking                    │
├─────────────────────────────────────────────────────────────────┤
│ Level 3: compact_messages  (1 API call, cache-safe)             │
│   • Keeps last 10 messages, summarizes older ones via API       │
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
1. find_latest_session_for_cwd(cwd)
     → FNV-1a hash of cwd → scan ~/.baoclaw/sessions/ for matching .jsonl
2. TranscriptWriter::load(session_id) → read all entries
3. SessionMemory::load(session_id) → check .memory.md for existing summary
4. Three-tier loading (10 min → < 5 sec):
   Tier 1 (best):   summary exists → load summary + last 200 entries only
   Tier 2 (small):  entries ≤ 400, no summary → safe to rebuild all
   Tier 3 (fallback): large session, no summary → last 200 entries + warning
5. engine.set_messages(messages)
6. engine.load_token_baseline(session_id)   // restore calibrated count
7. engine.seed_session_memory(&old_summary) // carry forward to new session
```

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

1. At daemon startup: `load_skills_for_prompt(cwd)` → discovers all skill `.md` files
2. At daemon startup: `MemoryStore::load()` → reads global `~/.baoclaw/memory.jsonl`
3. Combined via `build_append_prompt()` → becomes Layer 4
4. Evolution engine's `build_prompt_fragment()` adds pending reviews and skill candidates
5. All of this is computed once at startup and reused across turns

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
| MCP disconnect                     | Retry              | 5 attempts, 2s initial delay       |
| Tool timeout                       | Fatal              | Report to user                     |

**Key behaviors:**

- The fallback controller **resets** to the primary model for each new query (cross-turn stateless)
- **Exponential backoff** prevents hammering a rate-limited endpoint
- **Circuit breaker**: After 3 consecutive compaction failures, auto-compaction is disabled to avoid wasting API calls
- **5-minute timeout** per API call — on timeout, the user message is removed to keep history clean

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

## See also

- [Permission system](PERMISSIONS.md)
- [Important files tour](IMPORTANT_FILES.md)
- [Configuration reference](CONFIGURATION.md)
