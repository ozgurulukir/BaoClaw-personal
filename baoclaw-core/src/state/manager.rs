use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::models::message::Usage;
use crate::models::task::TaskState;

/// The core application state managed by the Rust process.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoreState {
    pub session_id: String,
    pub model: String,
    pub verbose: bool,
    pub tasks: HashMap<String, TaskState>,
    pub usage: Usage,
    pub total_cost_usd: f64,
}

/// Manages the core application state (session info, model, tasks, usage).
pub struct StateManager {
    state: Arc<RwLock<CoreState>>,
}

impl StateManager {
    /// Creates a new StateManager with the given initial state.
    pub fn new(initial: CoreState) -> Self {
        Self {
            state: Arc::new(RwLock::new(initial)),
        }
    }

    /// Returns a clone of the current state.
    pub fn get(&self) -> CoreState {
        self.state.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Applies the updater function under the write lock.
    pub fn update(&self, updater: impl FnOnce(&mut CoreState)) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        updater(&mut state);
    }

    /// Returns the full state as a JSON Value (for full sync).
    pub fn snapshot(&self) -> Value {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        serde_json::to_value(&*state).unwrap_or(Value::Null)
    }

    /// Convenience method to add or update a task.
    pub fn update_task(&self, task: TaskState) {
        self.update(|state| {
            state.tasks.insert(task.id.clone(), task);
        });
    }

    /// Convenience method to remove a task by ID.
    pub fn remove_task(&self, task_id: &str) {
        self.update(|state| {
            state.tasks.remove(task_id);
        });
    }

    /// Convenience method to update usage.
    pub fn update_usage(&self, usage: Usage) {
        self.update(|state| {
            state.usage = usage;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::task::{TaskStatus, TaskType};
    use std::path::PathBuf;

    fn default_usage() -> Usage {
        Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        }
    }

    fn default_state() -> CoreState {
        CoreState {
            session_id: "test-session".to_string(),
            model: "claude-3".to_string(),
            verbose: false,
            tasks: HashMap::new(),
            usage: default_usage(),
            total_cost_usd: 0.0,
        }
    }

    fn sample_task(id: &str) -> TaskState {
        TaskState {
            id: id.to_string(),
            task_type: TaskType::LocalBash,
            status: TaskStatus::Running,
            description: "test task".to_string(),
            tool_use_id: None,
            start_time: 1700000000000,
            end_time: None,
            output_file: PathBuf::from("/tmp/output.txt"),
            output_offset: 0,
        }
    }

    #[test]
    fn test_new_and_get() {
        let initial = default_state();
        let manager = StateManager::new(initial);
        let state = manager.get();
        assert_eq!(state.session_id, "test-session");
        assert_eq!(state.model, "claude-3");
        assert!(!state.verbose);
        assert!(state.tasks.is_empty());
        assert_eq!(state.total_cost_usd, 0.0);
    }

    #[test]
    fn test_update_applies_changes() {
        let manager = StateManager::new(default_state());
        manager.update(|s| {
            s.model = "claude-4".to_string();
            s.total_cost_usd = 1.5;
        });
        let state = manager.get();
        assert_eq!(state.model, "claude-4");
        assert_eq!(state.total_cost_usd, 1.5);
    }

    #[test]
    fn test_update_task_add() {
        let manager = StateManager::new(default_state());
        let task = sample_task("b12345678");
        manager.update_task(task);
        let state = manager.get();
        assert_eq!(state.tasks.len(), 1);
        assert!(state.tasks.contains_key("b12345678"));
    }

    #[test]
    fn test_update_task_modify() {
        let manager = StateManager::new(default_state());
        let task = sample_task("b12345678");
        manager.update_task(task);

        let mut updated = sample_task("b12345678");
        updated.status = TaskStatus::Completed;
        updated.end_time = Some(1700000001000);
        manager.update_task(updated);

        let state = manager.get();
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.tasks["b12345678"].status, TaskStatus::Completed);
    }

    #[test]
    fn test_remove_task() {
        let manager = StateManager::new(default_state());
        manager.update_task(sample_task("b12345678"));
        assert_eq!(manager.get().tasks.len(), 1);

        manager.remove_task("b12345678");
        assert!(manager.get().tasks.is_empty());
    }

    #[test]
    fn test_remove_nonexistent_task() {
        let manager = StateManager::new(default_state());
        // Should not panic
        manager.remove_task("nonexistent");
        assert!(manager.get().tasks.is_empty());
    }

    #[test]
    fn test_update_usage() {
        let manager = StateManager::new(default_state());
        let new_usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: Some(10),
            cache_read_input_tokens: None,
        };
        manager.update_usage(new_usage);
        let state = manager.get();
        assert_eq!(state.usage.input_tokens, 100);
        assert_eq!(state.usage.output_tokens, 50);
        assert_eq!(state.usage.cache_creation_input_tokens, Some(10));
    }

    #[test]
    fn test_snapshot_returns_json_value() {
        let manager = StateManager::new(default_state());
        let snapshot = manager.snapshot();
        assert!(snapshot.is_object());
        assert_eq!(snapshot["session_id"], "test-session");
        assert_eq!(snapshot["model"], "claude-3");
        assert_eq!(snapshot["verbose"], false);
        assert_eq!(snapshot["total_cost_usd"], 0.0);
    }

    #[test]
    fn test_snapshot_reflects_updates() {
        let manager = StateManager::new(default_state());
        manager.update(|s| {
            s.model = "claude-4".to_string();
            s.total_cost_usd = 2.5;
        });
        let snapshot = manager.snapshot();
        assert_eq!(snapshot["model"], "claude-4");
        assert_eq!(snapshot["total_cost_usd"], 2.5);
    }

    #[test]
    fn test_core_state_serialization_roundtrip() {
        let mut state = default_state();
        state
            .tasks
            .insert("b12345678".to_string(), sample_task("b12345678"));
        let json = serde_json::to_string(&state).unwrap();
        let deserialized: CoreState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.session_id, state.session_id);
        assert_eq!(deserialized.model, state.model);
        assert_eq!(deserialized.tasks.len(), 1);
    }
}
