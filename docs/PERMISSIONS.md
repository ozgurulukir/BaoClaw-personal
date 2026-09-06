# BaoClaw Permission System

This document describes BaoClaw's tool execution permission control mechanism, including architecture, data structures, checking flow, and configuration.

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Core Data Structures](#2-core-data-structures)
3. [PermissionManager — Rule Matching Engine](#3-permissionmanager--rule-matching-engine)
4. [PermissionGate — Async Decision Channel](#4-permissiongate--async-decision-channel)
5. [ToolExecutor — Execution Pipeline](#5-toolexecutor--execution-pipeline)
6. [RuleBasedPermissionGate — Engine-Level Rule Cache](#6-rulebasedpermissiongate--engine-level-rule-cache)
7. [Security Module — Dangerous Command Blocking](#7-security-module--dangerous-command-blocking)
8. [Full Permission Check Flow](#8-full-permission-check-flow)
9. [Configuration](#9-configuration)

---

## 1. Architecture Overview

The BaoClaw permission system consists of the following components (layered design):

```
┌────────────────────────────────────────────────────────────────────┐
│                         CLI (ts-ipc/cli.ts)                         │
│                Slash commands /permission, /permissions             │
│                       User interaction layer                        │
└───────────────┬────────────────────────────────────┬───────────────┘
                │ JSON-RPC                            │ PermissionRequest
                │ events                              │ events (EngineEvent)
┌───────────────▼────────────────────────────────────▼───────────────┐
│                      Daemon (main.rs)                               │
│  ┌─────────────────────┐  ┌──────────────────────────────────────┐ │
│  │   IPC Router        │  │   QueryEngine                         │ │
│  │   ClientMethod enum │  │   ┌──────────────────────────────┐   │ │
│  │   - PermissionStatus│  │   │  ToolExecutor                 │   │ │
│  │   - PermissionGrant │  │   │  execute_tool_with_permission │   │ │
│  │   - PermissionRevoke│  │   └──────────┬───────────────────┘   │ │
│  └─────────────────────┘  │              │                        │ │
│                           │  ┌───────────▼────────────┐           │ │
│                           │  │ PermissionManager      │           │ │
│                           │  │ (rules + glob match)   │           │ │
│                           │  └────────────────────────┘           │ │
│                           │  ┌────────────────────────┐           │ │
│                           │  │ PermissionGate         │           │ │
│                           │  │ (oneshot channels)     │           │ │
│                           │  └────────────────────────┘           │ │
│                           └──────────────────────────────────────┘ │
│  ┌─────────────────────────────────────────────────────────────┐  │
│  │  RuleBasedPermissionGate (engine/permission_gate)           │  │
│  │  - Built-in security rules (deny rm -rf, sudo, etc.)        │  │
│  │  - Cached user grants (AllowSession / AllowPermanent)       │  │
│  └─────────────────────────────────────────────────────────────┘  │
│  ┌─────────────────────────────────────────────────────────────┐  │
│  │  Security module (engine/security.rs)                       │  │
│  │  - check_dangerous_command() hard blocking                  │  │
│  │  - check_ssrf_url() SSRF protection                         │  │
│  │  - validate_memory_content() credential leak detection      │  │
│  └─────────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────┘
```

### File Inventory

| File                                              | Responsibility                                                                     |
| ------------------------------------------------- | ---------------------------------------------------------------------------------- |
| `baoclaw-core/src/permissions/manager.rs`         | `PermissionManager` + `PermissionMode` + `PermissionRule` + glob matching          |
| `baoclaw-core/src/permissions/gate.rs`            | `PermissionGate` (pending request queue + oneshot channel) + `PermissionDecision`  |
| `baoclaw-core/src/tools/executor.rs`              | `ToolExecutor` — tool execution pipeline, calls PermissionManager + PermissionGate |
| `baoclaw-core/src/engine/permission_gate/gate.rs` | `RuleBasedPermissionGate` — engine-level rule policy + cache                       |
| `baoclaw-core/src/engine/security.rs`             | Dangerous command blocking, SSRF protection, content validation                    |

---

## 2. Core Data Structures

### PermissionMode

```rust
pub enum PermissionMode {
    Default,            // Default mode: tools not matching an allow rule → Ask
    Plan,               // Plan mode: read-only tools auto-Allow, everything else Ask
    BypassPermissions,  // Bypass mode: all non-deny tools auto-Allow
    Auto,               // Auto mode (reserved)
}
```

| Mode              | Read operations                    | Writes     | deny rules        |
| ----------------- | ---------------------------------- | ---------- | ----------------- |
| Default           | Ask (unless an allow rule matches) | Ask        | ✅ enforced block |
| Plan              | Allow (Read/Grep/Glob/Search)      | Ask        | ✅ enforced block |
| BypassPermissions | Allow                              | Allow      | ✅ enforced block |
| Auto              | (reserved)                         | (reserved) | ✅ enforced block |

### PermissionRule

```rust
pub struct PermissionRule {
    pub tool_name: String,        // Tool name (case-insensitive)
    pub rule_content: Option<String>,  // glob pattern, e.g. "rm -rf *"; None = match any input
}
```

### ToolPermissionContext

```rust
pub struct ToolPermissionContext {
    pub mode: PermissionMode,
    pub additional_working_directories: HashMap<String, String>,
    pub always_allow_rules: ToolPermissionRulesBySource,  // HashMap<source, Vec<PermissionRule>>
    pub always_deny_rules: ToolPermissionRulesBySource,
    pub always_ask_rules: ToolPermissionRulesBySource,
    pub is_bypass_permissions_mode_available: bool,
}
```

**Rules are grouped by source**, where `source` can be `"builtin"`, `"user"`, `"config"`, etc. Checks iterate over the rules from all sources.

### PermissionResult

```rust
pub enum PermissionResult {
    Allow,
    Ask { message: String },
    Deny { message: String },
}
```

---

## 3. PermissionManager — Rule Matching Engine

**File**: `baoclaw-core/src/permissions/manager.rs`

### check_permission() evaluation order

`check_permission(tool_name, input_description)` is evaluated in the following order (short-circuits on return):

```
Step 1: deny rule check (highest priority)
  → match → return Deny

Step 2: BypassPermissions mode
  → return Allow (skips all subsequent checks)

Step 3: allow rule check
  → match → return Allow

Step 4: ask rule check
  → match → return Ask

Step 5: Plan mode
  → read-only tools (Read/Grep/Glob/Search) → Allow
  → everything else → Ask

Step 6: default → Ask
```

**Key design**: deny rules are always checked first, ensuring that even in BypassPermissions mode, dangerous operations are still blocked.

### glob_matches() — wildcard matching

Implements `*` wildcard matching with dynamic programming:

- `*` matches any-length character sequence (including the empty string)
- Matching is case-insensitive
- Examples:
  - `"git *"` matches `"git push origin main"`
  - `"rm -rf *"` matches `"rm -rf /tmp/build"`
  - `"*"` matches any string

### matches_rule() — single-rule matching

```rust
fn matches_rule(rule: &PermissionRule, tool_name: &str, input_description: Option<&str>) -> bool {
    // 1. Case-insensitive tool name match
    if !rule.tool_name.eq_ignore_ascii_case(tool_name) { return false; }

    // 2. If the rule has a content pattern, the input description must match the glob
    match (&rule.rule_content, input_description) {
        (Some(pattern), Some(desc)) => glob_matches(pattern, desc),
        (Some(_), None) => false,  // has pattern but no input description → no match
        (None, _) => true,          // no pattern → match any input
    }
}
```

### API methods

| Method                                                        | Description                    |
| ------------------------------------------------------------- | ------------------------------ |
| `new(context: ToolPermissionContext)`                         | Create the manager             |
| `check_permission(tool_name, input_desc) -> PermissionResult` | Core check method              |
| `update_context(FnOnce(&mut ctx))`                            | Update the context via closure |
| `get_context() -> ToolPermissionContext`                      | Get a context snapshot         |
| `add_allow_always_rule(source, tool_name, rule_content)`      | Add an allow rule              |

---

## 4. PermissionGate — Async Decision Channel

**File**: `baoclaw-core/src/permissions/gate.rs`

`PermissionGate` is the async communication bridge between CLI ↔ Daemon, used to handle permission requests that require user confirmation.

### How it works

```
Daemon (ToolExecutor)                     CLI
    │                                       │
    │  PermissionGate.request(tool_use_id)   │
    │  → returns oneshot::Receiver           │
    │  → blocking wait...                    │
    │                           ◄──────────│  EngineEvent::PermissionRequest
    │                           (IPC event) │  show confirmation prompt to user
    │                                       │
    │                           ◄──────────│  PermissionResponse { decision }
    │  PermissionGate.respond(tool_use_id,  │  (allow/deny/allow_always)
    │    decision)                           │
    │  → oneshot::Sender.send(decision)      │
    │  ← oneshot::Receiver gets decision     │
    │  → continue execution or deny          │
```

### PermissionDecision

```rust
pub enum PermissionDecision {
    Allow,                  // Allow this once
    Deny,                   // Deny
    AllowAlways {           // Allow permanently (added to allow rules)
        rule: Option<String>,
    },
}
```

### Timeout mechanism

`ToolExecutor` waits for the user's decision inside `execute_tool_with_permission`; on timeout it automatically denies.
The timeout comes from the `permissions.ask_timeout_secs` config (default **300 seconds**, allowed range 5–3600 seconds).
The executor reads it live from the shared PermissionManager on every prompt, so changes take effect immediately:

```rust
let decision = match tokio::time::timeout(ask_timeout, rx).await {
    Ok(Ok(decision)) => decision,
    Ok(Err(_)) => PermissionDecision::Deny,  // channel closed → deny
    Err(_) => PermissionDecision::Deny,      // timeout → auto-deny
};
```

### API methods

| Method                                                 | Description                                                  |
| ------------------------------------------------------ | ------------------------------------------------------------ |
| `new()`                                                | Create an empty gate                                         |
| `request(tool_use_id) -> Receiver<PermissionDecision>` | Register a pending request, return the wait channel          |
| `respond(tool_use_id, decision) -> bool`               | Submit the user's decision, returns whether it was delivered |
| `pending_count() -> usize`                             | Current number of pending requests                           |

---

## 5. ToolExecutor — Execution Pipeline

**File**: `baoclaw-core/src/tools/executor.rs`

### execute_tool_with_permission()

This is the core tool execution function, integrating PermissionManager + PermissionGate:

```
┌─────────────────────────────────────────────────────┐
│  Step 1: validate_input(&request.input)             │
│  → Invalid → return error                           │
├─────────────────────────────────────────────────────┤
│  Step 2: permission_manager.check_permission(       │
│            tool_name, input_description)             │
│                                                      │
│  → Allow  → execute directly (call_tool_and_wrap)   │
│  → Deny   → return "Permission denied"              │
│  → Ask    → enter interactive confirmation flow ──┐ │
├───────────────────────────────◄──┘                   │
│  Step 3 (Ask branch):                                │
│  a. send EngineEvent::PermissionRequest              │
│  b. PermissionGate.request(tool_use_id)              │
│  c. wait up to ask_timeout_secs                      │
│                                                      │
│  → Allow          → execute                         │
│  → AllowAlways    → add rule + execute              │
│  → Deny           → return "Permission denied by user"│
├─────────────────────────────────────────────────────┤
│  Step 4: tool.call(input, context, progress)        │
│  → maybe_persist_or_truncate(result)                │
└─────────────────────────────────────────────────────┘
```

### Two execution paths

| Function                         | Purpose                                   | Permission check method                                                                                                                                         |
| -------------------------------- | ----------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `execute_tool()`                 | Simple path (direct/batch execution)      | Checks the Tool trait's `check_permissions`; non-read-only tools hit `Ask` and are blocked Fail-Closed by default (read-only tools are allowed after a warning) |
| `execute_tool_with_permission()` | Full path (with PermissionManager + Gate) | PermissionManager → Gate interactive confirmation (auto Fail-Closed deny after ask_timeout_secs timeout, default 300 seconds)                                   |

---

## 6. RuleBasedPermissionGate — Engine-Level Rule Cache

**File**: `baoclaw-core/src/engine/permission_gate/gate.rs`

This is a standalone permission policy engine used for rule management and session caching at the QueryEngine layer.

### Built-in default rules

| Tool                  | Pattern                                                          | Policy                  |
| --------------------- | ---------------------------------------------------------------- | ----------------------- |
| FileRead              | `*`                                                              | ✅ Always Allow         |
| FileWrite             | `*.env`, `.git/*`, `*/.ssh/*`                                    | ❌ Auto Deny            |
| FileWrite             | `*.md`                                                           | ✅ Allow                |
| FileWrite             | `*` (everything else)                                            | ❓ Require Confirmation |
| Bash                  | `rm -rf /`, `sudo `, `chmod 777`, `dd if=`, `mkfs.`, `> /dev/sd` | ❌ Auto Deny            |
| Bash                  | `git status`, `git diff`, `ls `, `cat `, `grep `, `find `, `pwd` | ✅ Allow                |
| Bash                  | `*` (everything else)                                            | ❓ Require Confirmation |
| FileDelete / FileEdit | `*`                                                              | ❓ Require Confirmation |
| WebFetch              | `localhost:*`, `127.*`, `10.*`                                   | ❌ Auto Deny            |
| WebFetch              | `*` (external)                                                   | ❓ Require Confirmation |
| WebSearch             | `*`                                                              | ✅ Allow                |

### Cached decision types

```rust
pub enum DecisionType {
    AllowOnce,        // This time only (not cached)
    AllowSession,     // Valid within the session (default TTL 24h)
    AllowPermanent,   // Permanent
    Deny,
    AskUser,
}
```

### Evaluation order

1. **Check cache** — if there is a cached AllowSession/AllowPermanent grant → return immediately
2. **Match rules in order** — the first matching rule wins
3. **Default** → AskUser

---

## 7. Security Module — Dangerous Command Blocking

**File**: `baoclaw-core/src/engine/security.rs`

The Security module provides three additional layers of protection (independent of PermissionManager):

### 7.1 Dangerous command blocking — `check_dangerous_command()`

A hardcoded blocklist of dangerous commands (substring match, case-insensitive):

- `rm -rf /*` / `rm -rf /` — recursive root deletion
- `:(){ :|:& };:` — fork bomb
- `dd if=` / `of=/dev/sd*` — block device writes
- `mkfs` — filesystem formatting
- `chmod 777 /` / `chmod -r 777 /` — world-writable root
- `> /etc/passwd` / `> /etc/shadow` — overwrite auth files
- `shutdown` / `reboot` / `poweroff` / `halt` — system power operations
- `> /dev/sda*` / `> /dev/nvme*` — direct block device writes

### 7.2 SSRF protection — `check_ssrf_url()`

Blocks URLs pointing to internal/private networks:

- `127.0.0.0/8` — Loopback
- `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` — RFC 1918
- `169.254.0.0/16` — Link-local (including `169.254.169.254` cloud metadata)
- `100.64.0.0/10` — CGNAT
- `::1`, `fc00::/7`, `fe80::/10` — IPv6 private
- `metadata.google.internal`, `metadata.internal` — cloud metadata endpoints

### 7.3 Memory content validation — `validate_memory_content()`

Detects the following and refuses to write to long-term memory:

- **Credential leaks**: `sk-*`, `ghp_*`, `AKIA*`, `xox[bpas]-*`, `Bearer *`
- **Invisible Unicode**: zero-width space `\u200B`, BOM `\uFEFF`, RTL override `\u202E`, etc.
- **Prompt injection**: phrases like "ignore previous instructions"

---

## 8. Full Permission Check Flow

```
User sends message → LLM returns tool_use
       │
       ▼
┌──────────────────────────┐
│ ToolExecutor             │
│ .execute_tool_with_perm  │
└──────────┬───────────────┘
           │
     ┌─────▼─────┐
     │ validate  │──invalid──► return error
     └─────┬─────┘
           │ ok
     ┌─────▼──────────────────┐
     │ PermissionManager      │
     │ .check_permission()    │
     └─────┬──────┬──────┬────┘
           │      │      │
      Allow│   Ask│   Deny│
           │      │      │
           │      │  ┌───▼──────────────┐
           │      │  │ return "Denied"  │
           │      │  └──────────────────┘
           │  ┌───▼──────────────────────┐
           │  │ EngineEvent::Permission  │
           │  │ Request → CLI            │
           │  ├──────────────────────────┤
           │  │ PermissionGate.request() │
           │  │ wait ask_timeout_secs    │
           │  └───┬──────────┬──────┬─────┘
           │      │          │      │
           │   Allow    AllowAlways Deny
           │      │          │      │
           │      │    ┌─────▼──────────┐
           │      │    │ add_allow_rule │
           │      │    └─────┬──────────┘
           │      │          │
     ┌─────▼──────▼──────────▼──┐
     │ call_tool_and_wrap()     │
     │  tool.call(input, ctx)   │
     │  → maybe_persist/truncate│
     └──────────────────────────┘
```

---

## 9. Configuration

### The permissions field in ~/.baoclaw/config.json

```json
{
  "permissions": {
    "mode": "default",
    "additional_working_directories": {},
    "always_allow_rules": {
      "builtin": [
        { "tool_name": "Read", "rule_content": null },
        { "tool_name": "Bash", "rule_content": "git status" },
        { "tool_name": "Bash", "rule_content": "git diff *" }
      ]
    },
    "always_deny_rules": {
      "builtin": [
        { "tool_name": "Bash", "rule_content": "rm -rf *" },
        { "tool_name": "Bash", "rule_content": "sudo *" }
      ]
    },
    "always_ask_rules": {
      "builtin": [{ "tool_name": "Bash", "rule_content": "*" }]
    },
    "auto_allow_channels": { "tui": true },
    "ask_timeout_secs": 300,
    "persist_grants": true
  }
}
```

### Runtime modification

Via IPC methods or slash commands:

- `/permission status` — view engine-level rule status
- `/permission grant <tool> <action> <target> [--permanent]` — grant
- `/permission revoke <tool> <action> <target>` — revoke
- `/permissions` — view the PermissionManager context (mode + the three rule kinds)
- `/permissions mode <default|plan|bypass|auto>` — switch permission mode
- `/permissions allow <tool> [glob]` — add an allow rule
- `/permissions deny <tool> [glob]` — add a deny rule
- `/permissions ask <tool> [glob]` — add an ask rule
- `/permissions timeout <5-3600>` — set the prompt timeout (seconds, takes effect immediately)
- `/permissions persist <on|off>` — toggle whether allow-always rules are written to config
- TUI: `p` / `Ctrl+P` — toggle auto-allow for the TUI channel (`permissions.setAutoAllow`)

### Backward compatibility

- When the `permissions` field is absent from `config.json`, defaults are used (mode=Default, empty rules)
- `BaoclawConfig` preserves unknown fields via `#[serde(flatten)] extra`, ensuring forward compatibility

## See also

- [Configuration reference](CONFIGURATION.md)
- [Engine internals](INTERNALS.md)
- [Usage guide](USAGE.md)
