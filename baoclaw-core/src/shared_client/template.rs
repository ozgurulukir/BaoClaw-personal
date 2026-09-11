use baoclaw_core::engine;
use baoclaw_core::ipc::protocol::RequestId;

use super::WriterRef;

pub(super) async fn scm_template_list(writer: WriterRef<'_>, id: RequestId) {
    let engine = engine::template::engine::TemplateEngine::new();
    let templates = engine.list_all();
    let result: Vec<serde_json::Value> = templates
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "trigger": t.trigger,
                "description": t.description,
                "version": t.version,
                "author": t.author,
                "builtin": t.builtin,
                "tags": t.tags,
                "variables_count": t.variables.len(),
                "steps_count": t.workflow.len(),
            })
        })
        .collect();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"templates": result, "count": result.len()}),
        )
        .await;
}

pub(super) async fn scm_template_create(writer: WriterRef<'_>, id: RequestId, json: String) {
    let mut engine = engine::template::engine::TemplateEngine::new();
    match serde_json::from_str::<engine::template::types::Template>(&json) {
        Ok(template) => match engine.create_template(&template) {
            Ok(()) => {
                let mut conn_guard = writer.lock().await;
                let _ = conn_guard
                    .send_response(
                        id,
                        serde_json::json!({"success": true, "name": template.name}),
                    )
                    .await;
            }
            Err(e) => {
                let mut conn_guard = writer.lock().await;
                let _ = conn_guard
                    .send_error(
                        Some(id),
                        -32000,
                        format!("Failed to create template: {}", e),
                    )
                    .await;
            }
        },
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Invalid template JSON: {}", e))
                .await;
        }
    }
}

pub(super) async fn scm_template_delete(writer: WriterRef<'_>, id: RequestId, name: String) {
    let mut engine = engine::template::engine::TemplateEngine::new();
    match engine.delete_template(&name) {
        Ok(()) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(id, serde_json::json!({"success": true, "name": name}))
                .await;
        }
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("Failed to delete template: {}", e),
                )
                .await;
        }
    }
}

pub(super) async fn scm_template_export(writer: WriterRef<'_>, id: RequestId, name: String) {
    let engine = engine::template::engine::TemplateEngine::new();
    match engine.export_template(&name) {
        Ok(json_str) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard.send_response(id, serde_json::json!({"name": name, "template": serde_json::from_str::<serde_json::Value>(&json_str).unwrap_or_default()})).await;
        }
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("Failed to export template: {}", e),
                )
                .await;
        }
    }
}

pub(super) async fn scm_template_import(writer: WriterRef<'_>, id: RequestId, url: String) {
    let mut engine = engine::template::engine::TemplateEngine::new();
    match engine.import_template_url(&url).await {
        Ok(template) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard.send_response(id, serde_json::json!({"success": true, "name": template.name, "trigger": template.trigger})).await;
        }
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("Failed to import template: {}", e),
                )
                .await;
        }
    }
}
