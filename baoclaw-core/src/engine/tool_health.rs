//! Tool health tracking — learns success/failure rates and auto-degrades failing tools.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Health record for a single tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolHealthRecord {
    pub tool_name: String,
    pub total_calls: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub timeout_count: u64,
    /// Recent failure reasons (last 10).
    pub recent_failures: Vec<String>,
    /// Current health status.
    pub status: ToolStatus,
    /// Consecutive failures (reset on success).
    pub consecutive_failures: u32,
    /// Timestamp of last status change.
    pub last_status_change: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToolStatus {
    /// Tool is healthy — normal operation.
    Healthy,
    /// Tool is degraded — added warnings to prompts.
    Degraded,
    /// Tool is disabled — temporarily removed from available tools.
    Disabled,
}

/// Manages tool health across sessions.
/// Shared daemon-wide handle for the tracker (interior mutability via the
/// records Mutex means `Arc` alone is enough to share and mutate).
pub type ToolHealthHandle = std::sync::Arc<ToolHealthTracker>;

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolHealthTracker {
    /// Interior mutability: the tracker is reached through a shared
    /// &QueryLoopConfig on the record path.
    pub records: std::sync::Mutex<HashMap<String, ToolHealthRecord>>,
    /// Threshold: consecutive failures before degrading.
    pub degrade_threshold: u32,
    /// Threshold: consecutive failures before disabling.
    pub disable_threshold: u32,
    /// Auto-recovery: after N minutes of being degraded, try healthy again.
    pub recovery_minutes: u32,
}

impl Default for ToolHealthTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for ToolHealthTracker {
    fn clone(&self) -> Self {
        Self {
            records: std::sync::Mutex::new(
                self.records
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone(),
            ),
            degrade_threshold: self.degrade_threshold,
            disable_threshold: self.disable_threshold,
            recovery_minutes: self.recovery_minutes,
        }
    }
}

impl ToolHealthTracker {
    pub fn new() -> Self {
        Self {
            records: std::sync::Mutex::new(HashMap::new()),
            degrade_threshold: 3,
            disable_threshold: 6,
            recovery_minutes: 30,
        }
    }

    /// Record a successful tool call.
    pub fn record_success(&self, tool_name: &str) {
        let mut records = self.records.lock().unwrap_or_else(|p| p.into_inner());
        let record = records
            .entry(tool_name.to_string())
            .or_insert_with(|| ToolHealthRecord::new(tool_name));
        record.total_calls += 1;
        record.success_count += 1;
        record.consecutive_failures = 0;
        // If was degraded, check if we should recover
        if record.status == ToolStatus::Degraded && record.success_count.is_multiple_of(5) {
            record.status = ToolStatus::Healthy;
            record.last_status_change = chrono::Utc::now().to_rfc3339();
        }
    }

    /// Record a failed tool call.
    pub fn record_failure(&self, tool_name: &str, reason: &str) {
        let mut records = self.records.lock().unwrap_or_else(|p| p.into_inner());
        let record = records
            .entry(tool_name.to_string())
            .or_insert_with(|| ToolHealthRecord::new(tool_name));
        record.total_calls += 1;
        record.failure_count += 1;
        record.consecutive_failures += 1;
        record.recent_failures.push(reason.to_string());
        if record.recent_failures.len() > 10 {
            record.recent_failures.drain(0..1);
        }
        // Check thresholds
        if record.consecutive_failures >= self.disable_threshold {
            record.status = ToolStatus::Disabled;
            record.last_status_change = chrono::Utc::now().to_rfc3339();
        } else if record.consecutive_failures >= self.degrade_threshold {
            record.status = ToolStatus::Degraded;
            record.last_status_change = chrono::Utc::now().to_rfc3339();
        }
    }

    /// Record a timeout.
    pub fn record_timeout(&self, tool_name: &str) {
        let mut records = self.records.lock().unwrap_or_else(|p| p.into_inner());
        let record = records
            .entry(tool_name.to_string())
            .or_insert_with(|| ToolHealthRecord::new(tool_name));
        record.total_calls += 1;
        record.timeout_count += 1;
        record.consecutive_failures += 1;
        if record.consecutive_failures >= self.disable_threshold {
            record.status = ToolStatus::Disabled;
            record.last_status_change = chrono::Utc::now().to_rfc3339();
        } else if record.consecutive_failures >= self.degrade_threshold {
            record.status = ToolStatus::Degraded;
            record.last_status_change = chrono::Utc::now().to_rfc3339();
        }
    }

    /// Check if a tool is available (not disabled). A Disabled/Degraded
    /// record older than `recovery_minutes` is lazily reset to Healthy —
    /// the tool gets a fresh start and its next failure re-degrades it.
    pub fn is_available(&self, tool_name: &str) -> bool {
        let mut records = self.records.lock().unwrap_or_else(|p| p.into_inner());
        match records.get_mut(tool_name) {
            Some(record) => {
                self.maybe_recover(record);
                record.status != ToolStatus::Disabled
            }
            None => true, // unknown tools are available by default
        }
    }

    /// If `record` has sat in a non-Healthy status longer than
    /// `recovery_minutes`, reset it to Healthy (consecutive failures
    /// cleared). Callers must hold the records lock in write mode.
    fn maybe_recover(&self, record: &mut ToolHealthRecord) {
        if record.status == ToolStatus::Healthy {
            return;
        }
        let Ok(changed) = chrono::DateTime::parse_from_rfc3339(&record.last_status_change) else {
            return;
        };
        let elapsed = chrono::Utc::now().signed_duration_since(changed);
        if elapsed.num_minutes() >= self.recovery_minutes as i64 {
            record.status = ToolStatus::Healthy;
            record.consecutive_failures = 0;
            record.last_status_change = chrono::Utc::now().to_rfc3339();
        }
    }

    /// Get warning message for degraded tools (to inject into system prompt).
    pub fn get_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for record in self
            .records
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values_mut()
        {
            self.maybe_recover(record);
            match record.status {
                ToolStatus::Degraded => {
                    let rate = if record.total_calls > 0 {
                        record.failure_count as f64 / record.total_calls as f64 * 100.0
                    } else {
                        0.0
                    };
                    warnings.push(format!(
                        "⚠️ Tool '{}' is degraded (failure rate: {:.0}%, {} consecutive failures). Consider using an alternative.",
                        record.tool_name, rate, record.consecutive_failures
                    ));
                }
                ToolStatus::Disabled => {
                    warnings.push(format!(
                        "🚫 Tool '{}' is temporarily disabled due to repeated failures. Last failures: {}",
                        record.tool_name,
                        record.recent_failures.last().unwrap_or(&"unknown".to_string())
                    ));
                }
                ToolStatus::Healthy => {}
            }
        }
        warnings
    }

    /// Get list of currently disabled tool names.
    pub fn disabled_tools(&self) -> Vec<String> {
        self.records
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(_, r)| r.status == ToolStatus::Disabled)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Get list of degraded tool names.
    pub fn degraded_tools(&self) -> Vec<String> {
        self.records
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(_, r)| r.status == ToolStatus::Degraded)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Build a system prompt fragment warning about tool health.
    pub fn build_health_prompt(&self) -> Option<String> {
        let warnings = self.get_warnings();
        if warnings.is_empty() {
            None
        } else {
            Some(format!(
                "\n## Tool Health Warnings\n{}\n",
                warnings.join("\n")
            ))
        }
    }

    /// Force-enable a disabled tool (manual override).
    pub fn force_enable(&self, tool_name: &str) {
        let mut records = self.records.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(record) = records.get_mut(tool_name) {
            record.status = ToolStatus::Healthy;
            record.consecutive_failures = 0;
            record.last_status_change = chrono::Utc::now().to_rfc3339();
        }
    }
}

impl ToolHealthRecord {
    fn new(name: &str) -> Self {
        Self {
            tool_name: name.to_string(),
            total_calls: 0,
            success_count: 0,
            failure_count: 0,
            timeout_count: 0,
            recent_failures: Vec::new(),
            status: ToolStatus::Healthy,
            consecutive_failures: 0,
            last_status_change: chrono::Utc::now().to_rfc3339(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_degrade_then_disable() {
        let t = ToolHealthTracker::new();
        for _ in 0..3 {
            t.record_failure("Bash", "boom");
        }
        assert_eq!(t.degraded_tools(), vec!["Bash".to_string()]);
        for _ in 0..3 {
            t.record_failure("Bash", "boom");
        }
        assert_eq!(t.disabled_tools(), vec!["Bash".to_string()]);
        assert!(!t.is_available("Bash"));
        assert!(t.is_available("FileRead")); // untouched tools stay available
    }

    #[tokio::test]
    async fn disabled_tool_recovers_after_recovery_minutes() {
        let t = ToolHealthTracker::new();
        for _ in 0..6 {
            t.record_failure("Bash", "boom");
        }
        assert!(!t.is_available("Bash"));

        // Age the status change past the recovery window (30 min default).
        {
            let mut records = t.records.lock().unwrap();
            let record = records.get_mut("Bash").unwrap();
            record.last_status_change =
                (chrono::Utc::now() - chrono::Duration::minutes(31)).to_rfc3339();
        }
        assert!(t.is_available("Bash")); // lazily recovered
        assert!(t.get_warnings().is_empty());

        // And a fresh failure counts from zero again — the full threshold
        // is needed to re-degrade (fresh-start semantics).
        t.record_failure("Bash", "boom again");
        assert!(t.degraded_tools().is_empty());
        assert!(t.is_available("Bash"));
    }

    #[tokio::test]
    async fn degraded_tool_recovers_on_successes() {
        let t = ToolHealthTracker::new();
        for _ in 0..3 {
            t.record_failure("Grep", "nope");
        }
        assert_eq!(t.degraded_tools(), vec!["Grep".to_string()]);
        for _ in 0..5 {
            t.record_success("Grep");
        }
        assert!(t.degraded_tools().is_empty());
        assert!(t.is_available("Grep"));
    }

    #[test]
    fn warnings_describe_degraded_and_disabled() {
        let t = ToolHealthTracker::new();
        for _ in 0..3 {
            t.record_failure("FileWrite", "disk on fire");
        }
        for _ in 0..6 {
            t.record_failure("Bash", "segfault");
        }
        let warnings = t.get_warnings();
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().any(|w| w.contains("degraded")));
        assert!(warnings.iter().any(|w| w.contains("disabled")));
    }
}
