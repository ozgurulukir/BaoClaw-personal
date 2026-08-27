use crate::engine::model_router::budget::BudgetManager;
use crate::engine::model_router::router::ModelRouter;
use serde_json::{json, Value};

/// List available models and capabilities from ModelRouter.
pub fn handle_model_list() -> Value {
    let router = ModelRouter::new();
    let models = router.list_models();
    let model_list: Vec<Value> = models
        .iter()
        .map(|m| {
            json!({
                "name": m.name,
                "provider": m.provider,
                "max_tokens": m.max_tokens,
                "cost_per_1k_input": m.cost_per_1k_input,
                "cost_per_1k_output": m.cost_per_1k_output,
                "capabilities": m.capabilities,
                "priority": m.priority,
            })
        })
        .collect();
    let count = model_list.len();
    json!({
        "models": model_list,
        "count": count,
    })
}

/// Route task description to the optimal model.
pub fn handle_model_route(task: &str) -> Value {
    let router = ModelRouter::new();
    let decision = router.route(task, 0, 0.5);
    json!({
        "selected_model": decision.selected_model,
        "reason": decision.reason,
        "confidence": decision.confidence,
    })
}

/// Retrieve current budget and remaining quotas.
pub fn handle_model_budget() -> Value {
    let budget = BudgetManager::load();
    json!({
        "daily_limit": budget.daily_limit,
        "monthly_limit": budget.monthly_limit,
        "current_daily": budget.current_daily,
        "current_monthly": budget.current_monthly,
        "remaining_daily": budget.remaining_daily(),
        "remaining_monthly": budget.remaining_monthly(),
    })
}

/// Retrieve router stats and active rule count.
pub fn handle_model_stats() -> Value {
    let router = ModelRouter::new();
    let models = router.list_models();
    let rules = router.list_rules();
    json!({
        "models_count": models.len(),
        "rules_count": rules.len(),
        "rules": rules.iter().map(|r| json!({
            "id": r.id,
            "description": r.description,
            "target_model": r.target_model,
            "priority": r.priority,
            "enabled": r.enabled,
        })).collect::<Vec<_>>(),
    })
}
