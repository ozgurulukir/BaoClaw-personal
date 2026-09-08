//! Team Manager for sharing team state across clients.
//!
//! This module provides the team store that allows teams to be created,
//! listed, and managed from different client connections. Execution itself
//! builds a fresh [`TeamExecutor`](super::executor::TeamExecutor) per RPC
//! from the daemon's shared resources.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::engine::team::types::AgentTeam;

/// Manages teams across multiple client connections.
pub struct TeamManager {
    /// Active teams being managed.
    teams: Arc<RwLock<HashMap<String, AgentTeam>>>,
}

impl Default for TeamManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TeamManager {
    /// Create a new TeamManager.
    pub fn new() -> Self {
        Self {
            teams: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Store a team in the manager.
    pub async fn store_team(&self, team: AgentTeam) {
        self.teams.write().await.insert(team.id.clone(), team);
    }

    /// Get a team by ID.
    pub async fn get_team(&self, team_id: &str) -> Option<AgentTeam> {
        self.teams.read().await.get(team_id).cloned()
    }

    /// List all teams.
    pub async fn list_teams(&self) -> Vec<AgentTeam> {
        self.teams.read().await.values().cloned().collect()
    }

    /// Abort a team.
    pub async fn abort_team(&self, team_id: &str) -> Option<AgentTeam> {
        let mut teams = self.teams.write().await;
        if let Some(team) = teams.get_mut(team_id) {
            team.abort("User requested abort".to_string());
            return Some(team.clone());
        }
        None
    }
}
