use baoclaw_core::engine;
use baoclaw_core::ipc::protocol::RequestId;

use super::{ScmFlow, WriterRef};
use crate::SharedState;

pub(super) async fn scm_telemetry_stats(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
) -> ScmFlow {
    let collector = match engine::telemetry::collector::TelemetryCollector::new() {
        Ok(c) => c,
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Telemetry unavailable: {}", e))
                .await;
            return ScmFlow::Continue;
        }
    };
    match collector.get_stats() {
        Ok(stats) => {
            // Recording switch lives on the shared collector (None = the DB
            // never opened, i.e. recording is off).
            let enabled = shared.telemetry.as_ref().is_some_and(|c| c.is_enabled());
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "total_turns": stats.total_turns,
                        "total_tokens": stats.total_tokens,
                        "total_cost_usd": stats.total_cost_usd,
                        "total_tools_called": stats.total_tools_called,
                        "sessions_count": stats.sessions_count,
                        "files_modified": stats.files_modified,
                        "avg_response_time_ms": stats.avg_response_time_ms,
                        "most_used_tool": stats.most_used_tool,
                        "enabled": enabled,
                    }),
                )
                .await;
        }
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Failed to get stats: {}", e))
                .await;
        }
    }
    ScmFlow::Continue
}

pub(super) async fn scm_telemetry_trends(
    writer: WriterRef<'_>,
    id: RequestId,
    days: u32,
) -> ScmFlow {
    let collector = match engine::telemetry::collector::TelemetryCollector::new() {
        Ok(c) => c,
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Telemetry unavailable: {}", e))
                .await;
            return ScmFlow::Continue;
        }
    };
    let daily = match collector.get_daily_stats(days) {
        Ok(d) => d,
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Failed to get trends: {}", e))
                .await;
            return ScmFlow::Continue;
        }
    };
    let daily_list: Vec<serde_json::Value> = daily
        .iter()
        .map(|d| {
            serde_json::json!({
                "date": d.date,
                "turns": d.turns,
                "tokens": d.tokens,
                "cost": d.cost,
                "tools": d.tools,
                "sessions": d.sessions,
            })
        })
        .collect();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"days": days, "daily": daily_list, "count": daily_list.len()}),
        )
        .await;
    ScmFlow::Continue
}

pub(super) async fn scm_telemetry_export(
    writer: WriterRef<'_>,
    id: RequestId,
    format: String,
) -> ScmFlow {
    let collector = match engine::telemetry::collector::TelemetryCollector::new() {
        Ok(c) => c,
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Telemetry unavailable: {}", e))
                .await;
            return ScmFlow::Continue;
        }
    };
    let exporter = engine::telemetry::export::TelemetryExporter::new(collector);
    let result = match format.to_lowercase().as_str() {
        "json" => exporter.export_json(None),
        "csv" => exporter.export_csv(None),
        "summary" | "md" | "markdown" => exporter.export_summary(),
        _ => Err(format!(
            "Unknown export format: {}. Use json, csv, or summary.",
            format
        )),
    };
    match result {
        Ok(data) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(id, serde_json::json!({"format": format, "data": data}))
                .await;
        }
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Export failed: {}", e))
                .await;
        }
    }
    ScmFlow::Continue
}

pub(super) async fn scm_telemetry_set_enabled(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    enabled: bool,
) {
    // Live switch on the shared collector (both record paths read it
    // per-event), then persisted to config.json so the choice survives
    // daemon restarts — same idiom as the permissions.* knobs.
    if let Some(ref telemetry) = shared.telemetry {
        telemetry.set_enabled(enabled);
    }
    let mut cfg = crate::config::load_config();
    cfg.telemetry_enabled = enabled;
    if let Err(e) = cfg.save() {
        eprintln!(
            "[telemetry] WARN: could not persist enabled={}: {}",
            enabled, e
        );
    }
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "success": true,
                "enabled": enabled,
                "message": format!(
                    "Telemetry {}",
                    if enabled { "enabled" } else { "disabled" }
                )
            }),
        )
        .await;
}
