//! Shared engine resources for headless engines.
//!
//! [`HeadlessEngineKit`] bundles everything the interactive engine gets that
//! makes sense without a human attached — the skills/profile system prompt,
//! the model fallback chain, telemetry, trajectory recording, the live
//! memory store, caches and the daemon-wide tool-health tracker. It is
//! threaded into cron jobs, background tasks, team agents and sub-agents so
//! they run with the same capabilities instead of bare engines.
//!
//! The interactive permission bridge is deliberately NOT part of the kit:
//! headless engines must never hang on a prompt. Mutating tools on those
//! paths fail closed instead.

use std::sync::Arc;

use crate::engine::evolution::EvolutionEngine;
use crate::engine::file_cache::FileCache;
use crate::engine::memory::MemoryStore;
use crate::engine::query_engine::MicroCompactConfig;
use crate::engine::telemetry::collector::TelemetryCollector;
use crate::engine::tool_health::ToolHealthTracker;
use crate::engine::tool_result_store::ToolResultStore;

/// Cloneable bundle of shared engine resources for headless engines.
#[derive(Clone)]
pub struct HeadlessEngineKit {
    /// Skills + user profile prompt fragment (memory is injected live per
    /// query via [`Self::memory_store`]).
    pub append_system_prompt: Option<String>,
    /// Model fallback chain and per-model retry budget.
    pub fallback_models: Vec<String>,
    pub max_retries_per_model: u32,
    /// Context window and compaction threshold, matching the interactive
    /// engine so token accounting behaves identically.
    pub context_window: u64,
    pub auto_compact_threshold_ratio: f64,
    /// Output token cap sent with each model request (default 16_384).
    pub max_tokens: u32,
    /// Micro-compact thresholds for clearing old large tool results.
    pub micro_compact: MicroCompactConfig,
    /// Per-query cost ceiling from the daemon config. None = the engine
    /// runs unbounded (cron jobs override with their own tighter limit).
    /// Token ceilings are a team-policy concern, not a kit one.
    pub max_budget_usd: Option<f64>,
    /// Telemetry recording (turn/session cost and usage trends).
    pub telemetry: Option<Arc<TelemetryCollector>>,
    /// Trajectory recording — headless runs become rateable via `/rate`.
    pub evolution: Option<Arc<EvolutionEngine>>,
    /// Live long-term memory (re-read per query, so MemoryTool writes made
    /// by one engine reach the others without a restart).
    pub memory_store: Option<Arc<MemoryStore>>,
    /// Shared file read cache (warmup preloads land here too).
    pub file_cache: Option<Arc<tokio::sync::Mutex<FileCache>>>,
    /// Oversized tool-result persistence (per-daemon store).
    pub tool_result_store: Option<Arc<ToolResultStore>>,
    /// Daemon-wide tool-health tracker: failure stats accumulate across
    /// every engine, and Disabled tools are blocked everywhere.
    pub tool_health: Arc<ToolHealthTracker>,
}

#[cfg(test)]
impl HeadlessEngineKit {
    /// Minimal kit for tests: defaults everywhere, no real resources.
    pub fn for_test() -> Self {
        Self {
            append_system_prompt: None,
            fallback_models: vec![],
            max_retries_per_model: 2,
            context_window: 200_000,
            auto_compact_threshold_ratio: 0.7,
            max_tokens: 16_384,
            micro_compact: MicroCompactConfig::default(),
            max_budget_usd: None,
            telemetry: None,
            evolution: None,
            memory_store: None,
            file_cache: None,
            tool_result_store: None,
            tool_health: Arc::new(ToolHealthTracker::new()),
        }
    }
}
