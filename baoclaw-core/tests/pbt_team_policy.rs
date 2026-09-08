//! Property-based tests for team policy and sub-agent execution.
//!
//! These tests validate:
//! - Tool permission restriction (whitelist / blacklist, via the per-agent
//!   policy the executor actually consults)
//! - Policy-to-agent limit propagation (turns, cost, tokens, timeout)
//! - Policy serialization round-trips
//!
//! **Validates: Requirements FR-2.4 资源限制**
//! - Each Agent's tool permission inheritance

use proptest::prelude::*;
use std::collections::HashSet;

// Import from the crate
use baoclaw_core::engine::team::policy::{AgentPolicy, TeamPolicy};

/// Strategy for generating valid tool names
fn tool_name_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("FileRead".to_string()),
        Just("FileWrite".to_string()),
        Just("FileEdit".to_string()),
        Just("Bash".to_string()),
        Just("Grep".to_string()),
        Just("Glob".to_string()),
        Just("WebSearch".to_string()),
        Just("WebFetch".to_string()),
    ]
}

/// Strategy for generating a set of tool names
fn tool_set_strategy() -> impl Strategy<Value = HashSet<String>> {
    proptest::collection::hash_set(tool_name_strategy(), 0..8)
}

/// Strategy for generating cost values
fn cost_strategy() -> impl Strategy<Value = f64> {
    0.0f64..100.0
}

/// Strategy for generating token counts
fn tokens_strategy() -> impl Strategy<Value = u64> {
    0u64..1000000
}

proptest! {
    /// Test that tool whitelist filtering is consistent
    ///
    /// **Validates: FR-2.4 工具权限继承**
    /// - Tools in whitelist are allowed
    /// - Tools not in whitelist are denied
    #[test]
    fn prop_tool_whitelist_consistency(
        whitelist in tool_set_strategy(),
        tool in tool_name_strategy()
    ) {
        let policy = TeamPolicy::default()
            .with_tool_whitelist(whitelist.iter().cloned().collect());
        let agent_policy = AgentPolicy::from_team_policy(&policy);

        let is_allowed = agent_policy.is_tool_allowed(&tool);
        let is_in_whitelist = whitelist.contains(&tool);

        // If whitelist is empty, all tools are allowed
        if whitelist.is_empty() {
            prop_assert!(is_allowed);
        } else {
            // Otherwise, only whitelisted tools are allowed
            prop_assert_eq!(is_allowed, is_in_whitelist);
        }
    }

    /// Test that tool blacklist filtering is consistent
    ///
    /// **Validates: FR-2.4 工具权限限制**
    /// - Tools in blacklist are always denied
    #[test]
    fn prop_tool_blacklist_consistency(
        blacklist in tool_set_strategy(),
        tool in tool_name_strategy()
    ) {
        let policy = TeamPolicy::default()
            .with_tool_blacklist(blacklist.iter().cloned().collect());
        let agent_policy = AgentPolicy::from_team_policy(&policy);

        let is_allowed = agent_policy.is_tool_allowed(&tool);
        let is_in_blacklist = blacklist.contains(&tool);

        // Blacklisted tools are always denied
        if is_in_blacklist {
            prop_assert!(!is_allowed);
        } else {
            // Non-blacklisted tools are allowed (when no whitelist)
            prop_assert!(is_allowed);
        }
    }

    /// Test that blacklist takes precedence over whitelist
    ///
    /// **Validates: FR-2.4 工具权限继承与限制**
    #[test]
    fn prop_blacklist_precedence(
        whitelist in tool_set_strategy(),
        blacklist in tool_set_strategy(),
        tool in tool_name_strategy()
    ) {
        let policy = TeamPolicy::default()
            .with_tool_whitelist(whitelist.iter().cloned().collect())
            .with_tool_blacklist(blacklist.iter().cloned().collect());
        let agent_policy = AgentPolicy::from_team_policy(&policy);

        let is_allowed = agent_policy.is_tool_allowed(&tool);
        let is_in_blacklist = blacklist.contains(&tool);

        // Blacklisted tools are always denied, even if in whitelist
        if is_in_blacklist {
            prop_assert!(!is_allowed);
        }
    }

    /// Test agent policy limit propagation
    ///
    /// **Validates: FR-2.4 Agent 的限制继承**
    #[test]
    fn prop_agent_policy_limit_propagation(
        max_turns in 1u32..20u32,
        max_cost in cost_strategy(),
        max_tokens in tokens_strategy()
    ) {
        let team_policy = TeamPolicy::default()
            .with_max_turns_per_agent(max_turns)
            .with_max_cost_per_agent(max_cost)
            .with_max_tokens_per_agent(max_tokens);

        let agent_policy = AgentPolicy::from_team_policy(&team_policy);

        // Agent policy should inherit the team policy limits verbatim
        prop_assert_eq!(agent_policy.max_turns, team_policy.max_turns_per_agent);
        prop_assert_eq!(agent_policy.max_cost_usd, team_policy.max_cost_per_agent);
        prop_assert_eq!(agent_policy.max_tokens, team_policy.max_tokens_per_agent);
        prop_assert_eq!(agent_policy.timeout_secs, team_policy.agent_timeout_secs);
    }

    /// Test policy JSON round-trip keeps the enforced limits
    ///
    /// **Validates: RPC teamSpawn policy parsing**
    #[test]
    fn prop_policy_serialization_round_trip(
        whitelist in tool_set_strategy(),
        max_cost in cost_strategy(),
        max_tokens in tokens_strategy()
    ) {
        let policy = TeamPolicy::default()
            .with_tool_whitelist(whitelist.iter().cloned().collect())
            .with_max_cost_per_agent(max_cost)
            .with_max_tokens_per_agent(max_tokens);

        let json = serde_json::to_string(&policy).unwrap();
        let deserialized: TeamPolicy = serde_json::from_str(&json).unwrap();

        prop_assert_eq!(deserialized.tool_whitelist, policy.tool_whitelist);
        // serde_json's default (non float_roundtrip) parser can be off by an
        // ulp on f64, so compare costs with a tolerance.
        prop_assert!(
            (deserialized.max_cost_per_agent.unwrap() - policy.max_cost_per_agent.unwrap()).abs()
                < 1e-9
        );
        prop_assert_eq!(deserialized.max_tokens_per_agent, policy.max_tokens_per_agent);
    }
}
