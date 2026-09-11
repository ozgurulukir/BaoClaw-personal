use baoclaw_core::engine;
use baoclaw_core::ipc::protocol::RequestId;
use baoclaw_core::permissions;

use super::WriterRef;
use crate::SharedState;

pub(super) async fn scm_permission_status(writer: WriterRef<'_>, id: RequestId) {
    let gate = engine::permission_gate::gate::RuleBasedPermissionGate::new();
    let rules = gate.list_rules();
    let rule_list: Vec<serde_json::Value> = rules
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "description": r.description,
                "tool": r.tool,
                "action": r.action,
                "target_pattern": r.target_pattern,
                "require_confirmation": r.require_confirmation,
                "auto_deny": r.auto_deny,
            })
        })
        .collect();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"rules": rule_list, "count": rule_list.len()}),
        )
        .await;
}

pub(super) async fn scm_permission_grant(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    tool: String,
    action: String,
    target: String,
    permanent: bool,
) {
    let rule_content = if target.is_empty() || target == "*" {
        None
    } else {
        Some(target.clone())
    };
    let category = match action.to_ascii_lowercase().as_str() {
        "deny" => "deny",
        "ask" => "ask",
        _ => "allow",
    };
    {
        let mgr = shared.permission_manager.write().await;
        mgr.add_rule(category, "user", &tool, rule_content.clone());
        if permanent {
            crate::permissions::persist_context_to_config(&mgr.get_context());
        }
    }
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "tool": tool,
                "action": action,
                "target": target,
                "permanent": permanent
            }),
        )
        .await;
}

pub(super) async fn scm_permission_revoke(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    tool: String,
    target: String,
) {
    let rule_content = if target.is_empty() || target == "*" {
        None
    } else {
        Some(target.as_str())
    };
    let removed = {
        let mgr = shared.permission_manager.write().await;
        let count = mgr.remove_rule(None, &tool, rule_content);
        if count > 0 {
            crate::permissions::persist_context_to_config(&mgr.get_context());
        }
        count
    };
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"success": removed > 0, "removed": removed}),
        )
        .await;
}

pub(super) async fn scm_permissions_info(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
) {
    let mgr = shared.permission_manager.read().await;
    let ctx = mgr.get_context();
    let perms = serde_json::to_value(&ctx).unwrap_or(serde_json::json!({
        "mode": "default",
        "always_allow_rules": {},
        "always_deny_rules": {},
        "always_ask_rules": {}
    }));
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, perms).await;
}

pub(super) async fn scm_permissions_add_rule(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    category: String,
    tool_name: String,
    rule_content: Option<String>,
) {
    {
        let mgr = shared.permission_manager.write().await;
        mgr.add_rule(&category, "config", &tool_name, rule_content.clone());
        crate::permissions::persist_context_to_config(&mgr.get_context());
    }
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "message": "Rule added and persisted to config.",
                "category": category,
                "tool_name": tool_name,
                "rule_content": rule_content
            }),
        )
        .await;
}

pub(super) async fn scm_permissions_remove_rule(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    category: String,
    tool_name: String,
    rule_content: Option<String>,
) {
    let removed = {
        let mgr = shared.permission_manager.write().await;
        let count = mgr.remove_rule(Some(&category), &tool_name, rule_content.as_deref());
        if count > 0 {
            crate::permissions::persist_context_to_config(&mgr.get_context());
        }
        count
    };
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": removed > 0,
                "message": format!("Removed {} rule(s)", removed),
                "category": category,
                "tool_name": tool_name,
                "rule_content": rule_content
            }),
        )
        .await;
}

pub(super) async fn scm_permissions_set_mode(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    mode: String,
) {
    let parsed_mode = match mode.to_ascii_lowercase().as_str() {
        "plan" => permissions::manager::PermissionMode::Plan,
        "bypass" | "bypasspermissions" => permissions::manager::PermissionMode::BypassPermissions,
        "auto" => permissions::manager::PermissionMode::Auto,
        _ => permissions::manager::PermissionMode::Default,
    };
    {
        let mgr = shared.permission_manager.write().await;
        mgr.set_mode(parsed_mode);
        crate::permissions::persist_context_to_config(&mgr.get_context());
    }
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "mode": mode,
                "message": format!("Permission mode updated to {}", mode)
            }),
        )
        .await;
}

pub(super) async fn scm_permissions_set_auto_allow(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    channel: String,
    enabled: bool,
) {
    // Persisted knob for clients that answer their own
    // permission prompts (the TUI toggle; see ToolPermissionContext::
    // auto_allow_channels). Enforcement is client-side.
    {
        let mgr = shared.permission_manager.write().await;
        mgr.update_context(|c| {
            c.auto_allow_channels.insert(channel.clone(), enabled);
        });
        crate::permissions::persist_context_to_config(&mgr.get_context());
    }
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "channel": channel,
                "enabled": enabled,
                "message": format!(
                    "Auto-allow for channel '{}' {}",
                    channel,
                    if enabled { "enabled" } else { "disabled" }
                )
            }),
        )
        .await;
}

pub(super) async fn scm_permissions_set_ask_timeout(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    seconds: u64,
) {
    // Persisted prompt-timeout knob, read live by the
    // executor at every prompt. Reject out-of-range
    // values with an error instead of clamping.
    if (5..=3600).contains(&seconds) {
        {
            let mgr = shared.permission_manager.write().await;
            mgr.update_context(|c| c.ask_timeout_secs = seconds);
            crate::permissions::persist_context_to_config(&mgr.get_context());
        }
        let mut conn_guard = writer.lock().await;
        let _ = conn_guard
            .send_response(
                id,
                serde_json::json!({
                    "success": true,
                    "seconds": seconds,
                    "message": format!(
                        "Permission ask timeout set to {}s",
                        seconds
                    )
                }),
            )
            .await;
    } else {
        let mut conn_guard = writer.lock().await;
        let _ = conn_guard
            .send_error(
                Some(id),
                -32000,
                format!("ask timeout must be 5-3600 seconds, got {}", seconds),
            )
            .await;
    }
}

pub(super) async fn scm_permissions_set_persist_grants(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    enabled: bool,
) {
    // Persisted knob: when false, allow-always grants
    // still allow the current tool but are not written
    // back to config.json.
    {
        let mgr = shared.permission_manager.write().await;
        mgr.update_context(|c| c.persist_grants = enabled);
        crate::permissions::persist_context_to_config(&mgr.get_context());
    }
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "enabled": enabled,
                "message": format!(
                    "Allow-always grant persistence {}",
                    if enabled { "enabled" } else { "disabled" }
                )
            }),
        )
        .await;
}
