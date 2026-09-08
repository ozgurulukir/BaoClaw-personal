//! Team execution policy — tool permissions, per-agent limits, and result collection.
//!
//! This module provides fine-grained control over sub-agent execution:
//! - Tool permission restriction (whitelist / blacklist)
//! - Per-agent turn, cost, token and wall-clock limits
//! - Structured result collection with metadata
//!
//! # Example
//!
//! ```rust,ignore
//! use baoclaw_core::engine::team::policy::{TeamPolicy, AgentPolicy};
//!
//! let policy = TeamPolicy::default()
//!     .with_tool_whitelist(vec!["FileRead", "Grep", "Glob"])
//!     .with_max_cost_per_agent(0.5);
//!
//! // The per-agent policy enforces the limits in the query loop.
//! let agent_policy = AgentPolicy::from_team_policy(&policy);
//! assert!(agent_policy.is_tool_allowed("FileRead"));
//! assert!(!agent_policy.is_tool_allowed("WebSearch"));
//! ```

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Policy governing team execution.
///
/// Only limits the engine can actually enforce are modelled: tools
/// (whitelist/blacklist), per-agent turns, per-agent cost/tokens (checked in
/// the query loop) and a per-agent wall-clock timeout. Nesting depth and
/// team-wide budgets were removed rather than left decorative — no code path
/// can produce nested teams or per-team cost aggregation today.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamPolicy {
    /// Tools that sub-agents are allowed to use.
    /// Empty means all tools are allowed (inherit from parent).
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub tool_whitelist: HashSet<String>,

    /// Tools that sub-agents are explicitly denied.
    /// Takes precedence over whitelist.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub tool_blacklist: HashSet<String>,

    /// Maximum cost per sub-agent in USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_per_agent: Option<f64>,

    /// Maximum tokens per sub-agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_per_agent: Option<u64>,

    /// Maximum turns per sub-agent.
    #[serde(default = "default_max_turns")]
    pub max_turns_per_agent: u32,

    /// Maximum execution time per agent in seconds.
    #[serde(default = "default_timeout_secs")]
    pub agent_timeout_secs: u64,
}

fn default_max_turns() -> u32 {
    10
}

fn default_timeout_secs() -> u64 {
    300 // 5 minutes
}

impl Default for TeamPolicy {
    fn default() -> Self {
        Self {
            tool_whitelist: HashSet::new(),
            tool_blacklist: HashSet::new(),
            max_cost_per_agent: Some(1.0),
            max_tokens_per_agent: Some(50_000),
            max_turns_per_agent: default_max_turns(),
            agent_timeout_secs: default_timeout_secs(),
        }
    }
}

impl TeamPolicy {
    /// Set the tool whitelist.
    pub fn with_tool_whitelist(mut self, tools: Vec<String>) -> Self {
        self.tool_whitelist = tools.into_iter().collect();
        self
    }

    /// Set the tool blacklist.
    pub fn with_tool_blacklist(mut self, tools: Vec<String>) -> Self {
        self.tool_blacklist = tools.into_iter().collect();
        self
    }

    /// Set the maximum cost per agent.
    pub fn with_max_cost_per_agent(mut self, cost: f64) -> Self {
        self.max_cost_per_agent = Some(cost);
        self
    }

    /// Set the maximum tokens per agent.
    pub fn with_max_tokens_per_agent(mut self, tokens: u64) -> Self {
        self.max_tokens_per_agent = Some(tokens);
        self
    }

    /// Set the maximum turns per agent.
    pub fn with_max_turns_per_agent(mut self, turns: u32) -> Self {
        self.max_turns_per_agent = turns;
        self
    }
}

/// Per-agent execution policy derived from the team policy.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentPolicy {
    /// Tools allowed for this agent.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub allowed_tools: HashSet<String>,

    /// Tools denied for this agent.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub denied_tools: HashSet<String>,

    /// Maximum turns for this agent.
    pub max_turns: u32,

    /// Maximum cost for this agent in USD.
    pub max_cost_usd: Option<f64>,

    /// Maximum tokens for this agent.
    pub max_tokens: Option<u64>,

    /// Timeout for this agent in seconds.
    pub timeout_secs: u64,
}

impl AgentPolicy {
    /// Create an agent policy from a team policy.
    pub fn from_team_policy(team_policy: &TeamPolicy) -> Self {
        Self {
            allowed_tools: team_policy.tool_whitelist.clone(),
            denied_tools: team_policy.tool_blacklist.clone(),
            max_turns: team_policy.max_turns_per_agent,
            max_cost_usd: team_policy.max_cost_per_agent,
            max_tokens: team_policy.max_tokens_per_agent,
            timeout_secs: team_policy.agent_timeout_secs,
        }
    }

    /// Check if a tool is allowed.
    pub fn is_tool_allowed(&self, tool_name: &str) -> bool {
        // Check denied first
        if self.denied_tools.contains(tool_name) {
            return false;
        }

        // If allowed_tools is non-empty, check it
        if !self.allowed_tools.is_empty() {
            return self.allowed_tools.contains(tool_name);
        }

        // Empty allowed_tools means all tools are permitted (except denied)
        true
    }
}

/// Result from a sub-agent execution with metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentResult {
    /// ID of the agent that produced this result.
    pub agent_id: String,

    /// The text output from the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    /// Whether execution was successful.
    pub success: bool,

    /// Error message if execution failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Token usage breakdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AgentUsage>,

    /// Cost of this execution in USD.
    #[serde(default)]
    pub cost_usd: f64,

    /// Duration of execution in milliseconds.
    #[serde(default)]
    pub duration_ms: u64,

    /// Number of turns taken.
    #[serde(default)]
    pub turns: u32,

    /// Tools used during execution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_used: Vec<String>,

    /// Files read during execution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_read: Vec<String>,

    /// Files written during execution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_written: Vec<String>,

    /// Commands executed during execution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands_executed: Vec<String>,

    /// Additional metadata.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl AgentResult {
    /// Create a new successful result.
    pub fn success(agent_id: impl Into<String>, text: String) -> Self {
        Self {
            agent_id: agent_id.into(),
            text: Some(text),
            success: true,
            error: None,
            usage: None,
            cost_usd: 0.0,
            duration_ms: 0,
            turns: 0,
            tools_used: Vec::new(),
            files_read: Vec::new(),
            files_written: Vec::new(),
            commands_executed: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Create a new failed result.
    pub fn failure(agent_id: impl Into<String>, error: String) -> Self {
        Self {
            agent_id: agent_id.into(),
            text: None,
            success: false,
            error: Some(error),
            usage: None,
            cost_usd: 0.0,
            duration_ms: 0,
            turns: 0,
            tools_used: Vec::new(),
            files_read: Vec::new(),
            files_written: Vec::new(),
            commands_executed: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Set the usage information.
    pub fn with_usage(mut self, usage: AgentUsage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// Set the cost.
    pub fn with_cost(mut self, cost: f64) -> Self {
        self.cost_usd = cost;
        self
    }

    /// Set the duration.
    pub fn with_duration(mut self, ms: u64) -> Self {
        self.duration_ms = ms;
        self
    }

    /// Set the number of turns.
    pub fn with_turns(mut self, turns: u32) -> Self {
        self.turns = turns;
        self
    }

    /// Add a tool that was used.
    pub fn add_tool_used(&mut self, tool: String) {
        if !self.tools_used.contains(&tool) {
            self.tools_used.push(tool);
        }
    }

    /// Add a file that was read.
    pub fn add_file_read(&mut self, path: String) {
        if !self.files_read.contains(&path) {
            self.files_read.push(path);
        }
    }

    /// Add a file that was written.
    pub fn add_file_written(&mut self, path: String) {
        if !self.files_written.contains(&path) {
            self.files_written.push(path);
        }
    }

    /// Add a command that was executed.
    pub fn add_command(&mut self, command: String) {
        if !self.commands_executed.contains(&command) {
            self.commands_executed.push(command);
        }
    }

    /// Add metadata.
    pub fn add_metadata(&mut self, key: String, value: serde_json::Value) {
        self.metadata.insert(key, value);
    }
}

/// Token usage for an agent execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentUsage {
    /// Input tokens.
    pub input_tokens: u64,

    /// Output tokens.
    pub output_tokens: u64,

    /// Cache creation tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u64>,

    /// Cache read tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
}

impl AgentUsage {
    /// Create new usage stats.
    pub fn new(input: u64, output: u64) -> Self {
        Self {
            input_tokens: input,
            output_tokens: output,
            cache_creation_tokens: None,
            cache_read_tokens: None,
        }
    }

    /// Get total tokens.
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

impl Default for AgentUsage {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

/// Collected results from a team execution.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TeamResults {
    /// Team ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,

    /// Individual agent results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_results: Vec<AgentResult>,

    /// Combined text output from all agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combined_text: Option<String>,

    /// Total token usage.
    #[serde(default)]
    pub total_usage: AgentUsage,

    /// Total cost in USD.
    #[serde(default)]
    pub total_cost_usd: f64,

    /// Total duration in milliseconds.
    #[serde(default)]
    pub total_duration_ms: u64,

    /// Total turns across all agents.
    #[serde(default)]
    pub total_turns: u32,

    /// Number of successful agents.
    #[serde(default)]
    pub success_count: u32,

    /// Number of failed agents.
    #[serde(default)]
    pub failure_count: u32,

    /// All files read by any agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all_files_read: Vec<String>,

    /// All files written by any agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all_files_written: Vec<String>,

    /// All commands executed by any agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all_commands: Vec<String>,

    /// All tools used by any agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all_tools_used: Vec<String>,

    /// Additional metadata.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl TeamResults {
    /// Create a new empty results collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with a team ID.
    pub fn with_team_id(team_id: impl Into<String>) -> Self {
        Self {
            team_id: Some(team_id.into()),
            ..Self::default()
        }
    }

    /// Add an agent result.
    pub fn add_result(&mut self, result: AgentResult) {
        // Update counts
        if result.success {
            self.success_count += 1;
        } else {
            self.failure_count += 1;
        }

        // Accumulate usage
        if let Some(usage) = &result.usage {
            self.total_usage.input_tokens += usage.input_tokens;
            self.total_usage.output_tokens += usage.output_tokens;
            if let Some(cache) = usage.cache_creation_tokens {
                self.total_usage.cache_creation_tokens =
                    Some(self.total_usage.cache_creation_tokens.unwrap_or(0) + cache);
            }
            if let Some(cache) = usage.cache_read_tokens {
                self.total_usage.cache_read_tokens =
                    Some(self.total_usage.cache_read_tokens.unwrap_or(0) + cache);
            }
        }

        // Accumulate totals
        self.total_cost_usd += result.cost_usd;
        self.total_duration_ms += result.duration_ms;
        self.total_turns += result.turns;

        // Merge file/command/tool lists
        for file in &result.files_read {
            if !self.all_files_read.contains(file) {
                self.all_files_read.push(file.clone());
            }
        }
        for file in &result.files_written {
            if !self.all_files_written.contains(file) {
                self.all_files_written.push(file.clone());
            }
        }
        for cmd in &result.commands_executed {
            if !self.all_commands.contains(cmd) {
                self.all_commands.push(cmd.clone());
            }
        }
        for tool in &result.tools_used {
            if !self.all_tools_used.contains(tool) {
                self.all_tools_used.push(tool.clone());
            }
        }

        // Add result
        self.agent_results.push(result);
    }

    /// Build combined text from all agent results.
    pub fn build_combined_text(&mut self) {
        let texts: Vec<_> = self
            .agent_results
            .iter()
            .filter_map(|r| r.text.as_ref())
            .map(|t| t.as_str())
            .collect();

        if !texts.is_empty() {
            self.combined_text = Some(texts.join("\n\n---\n\n"));
        }
    }

    /// Check if all agents succeeded.
    pub fn all_succeeded(&self) -> bool {
        self.failure_count == 0 && self.success_count > 0
    }

    /// Check if all agents failed.
    pub fn all_failed(&self) -> bool {
        self.success_count == 0 && self.failure_count > 0
    }

    /// Get the total number of agents.
    pub fn total_agents(&self) -> u32 {
        self.success_count + self.failure_count
    }

    /// Get a summary string.
    pub fn summary(&self) -> String {
        format!(
            "TeamResults: {} agents ({} succeeded, {} failed), {} tokens, ${:.4}, {} ms",
            self.total_agents(),
            self.success_count,
            self.failure_count,
            self.total_usage.total(),
            self.total_cost_usd,
            self.total_duration_ms
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::engine::team::policy::AgentPolicy;
    use crate::engine::team::policy::TeamPolicy;
    #[test]
    fn test_team_policy_default() {
        let policy = TeamPolicy::default();
        assert!(policy.tool_whitelist.is_empty());
        assert!(policy.tool_blacklist.is_empty());
        assert!(policy.max_cost_per_agent.is_some());
        assert!(policy.max_tokens_per_agent.is_some());
        assert_eq!(policy.max_turns_per_agent, 10);
    }

    #[test]
    fn test_team_policy_tool_whitelist() {
        let policy =
            TeamPolicy::default().with_tool_whitelist(vec!["FileRead".into(), "Grep".into()]);

        let agent_policy = AgentPolicy::from_team_policy(&policy);
        assert!(agent_policy.is_tool_allowed("FileRead"));
        assert!(agent_policy.is_tool_allowed("Grep"));
        assert!(!agent_policy.is_tool_allowed("WebSearch"));
    }

    #[test]
    fn test_team_policy_tool_blacklist() {
        let policy = TeamPolicy::default().with_tool_blacklist(vec!["WebSearch".into()]);

        // All tools allowed except WebSearch
        let agent_policy = AgentPolicy::from_team_policy(&policy);
        assert!(agent_policy.is_tool_allowed("FileRead"));
        assert!(agent_policy.is_tool_allowed("Bash"));
        assert!(!agent_policy.is_tool_allowed("WebSearch"));
    }

    #[test]
    fn test_agent_policy_from_team_policy() {
        let team_policy = TeamPolicy::default()
            .with_tool_whitelist(vec!["FileRead".into(), "Grep".into()])
            .with_max_turns_per_agent(5)
            .with_max_cost_per_agent(0.5)
            .with_max_tokens_per_agent(25_000);

        let agent_policy = AgentPolicy::from_team_policy(&team_policy);

        assert!(agent_policy.is_tool_allowed("FileRead"));
        assert!(!agent_policy.is_tool_allowed("WebSearch"));
        assert_eq!(agent_policy.max_turns, 5);
        assert_eq!(agent_policy.max_cost_usd, Some(0.5));
        assert_eq!(agent_policy.max_tokens, Some(25_000));
        assert_eq!(agent_policy.timeout_secs, team_policy.agent_timeout_secs);
    }

    #[test]
    fn test_agent_result_success() {
        let result = AgentResult::success("agent-1", "Task completed".to_string());

        assert_eq!(result.agent_id, "agent-1");
        assert!(result.success);
        assert_eq!(result.text, Some("Task completed".to_string()));
        assert!(result.error.is_none());
    }

    #[test]
    fn test_agent_result_failure() {
        let result = AgentResult::failure("agent-1", "Something went wrong".to_string());

        assert_eq!(result.agent_id, "agent-1");
        assert!(!result.success);
        assert!(result.text.is_none());
        assert_eq!(result.error, Some("Something went wrong".to_string()));
    }

    #[test]
    fn test_agent_result_add_tools_files() {
        let mut result = AgentResult::success("agent-1", "Done".to_string());

        result.add_tool_used("FileRead".to_string());
        result.add_tool_used("Grep".to_string());
        result.add_tool_used("FileRead".to_string()); // duplicate should not be added

        result.add_file_read("/src/main.rs".to_string());
        result.add_file_written("/src/lib.rs".to_string());

        assert_eq!(result.tools_used, vec!["FileRead", "Grep"]);
        assert_eq!(result.files_read, vec!["/src/main.rs"]);
        assert_eq!(result.files_written, vec!["/src/lib.rs"]);
    }

    #[test]
    fn test_team_results() {
        let mut results = TeamResults::with_team_id("team-1");

        let result1 = AgentResult::success("agent-1", "Result 1".to_string())
            .with_usage(AgentUsage::new(100, 50))
            .with_cost(0.01)
            .with_duration(1000);

        let result2 = AgentResult::success("agent-2", "Result 2".to_string())
            .with_usage(AgentUsage::new(200, 100))
            .with_cost(0.02)
            .with_duration(2000);

        results.add_result(result1);
        results.add_result(result2);

        assert_eq!(results.total_agents(), 2);
        assert_eq!(results.success_count, 2);
        assert_eq!(results.failure_count, 0);
        assert!(results.all_succeeded());
        assert_eq!(results.total_usage.input_tokens, 300);
        assert_eq!(results.total_usage.output_tokens, 150);
        assert!((results.total_cost_usd - 0.03).abs() < 1e-10);
        assert_eq!(results.total_duration_ms, 3000);
    }

    #[test]
    fn test_team_results_mixed() {
        let mut results = TeamResults::new();

        results.add_result(AgentResult::success("agent-1", "OK".to_string()));
        results.add_result(AgentResult::failure("agent-2", "Failed".to_string()));

        assert!(!results.all_succeeded());
        assert!(!results.all_failed());
        assert_eq!(results.success_count, 1);
        assert_eq!(results.failure_count, 1);
    }

    #[test]
    fn test_policy_serialization() {
        let policy = TeamPolicy::default()
            .with_tool_whitelist(vec!["FileRead".into()])
            .with_max_cost_per_agent(0.5);

        let json = serde_json::to_string(&policy).unwrap();
        let deserialized: TeamPolicy = serde_json::from_str(&json).unwrap();

        assert!(deserialized.tool_whitelist.contains("FileRead"));
        assert_eq!(deserialized.max_cost_per_agent, Some(0.5));
    }

    #[test]
    fn test_agent_result_serialization() {
        let result = AgentResult::success("agent-1", "Done".to_string())
            .with_cost(0.05)
            .with_duration(1500);

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: AgentResult = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.agent_id, "agent-1");
        assert!(deserialized.success);
        assert!((deserialized.cost_usd - 0.05).abs() < 1e-10);
        assert_eq!(deserialized.duration_ms, 1500);
    }

    #[test]
    fn test_team_results_build_combined_text() {
        let mut results = TeamResults::new();

        results.add_result(AgentResult::success("a", "First result".to_string()));
        results.add_result(AgentResult::success("b", "Second result".to_string()));
        results.build_combined_text();

        let combined = results.combined_text.unwrap();
        assert!(combined.contains("First result"));
        assert!(combined.contains("Second result"));
    }

    #[test]
    fn test_agent_usage_total() {
        let usage = AgentUsage::new(100, 50);
        assert_eq!(usage.total(), 150);
    }
}
