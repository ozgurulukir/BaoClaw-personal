use baoclaw_core::engine;
use baoclaw_core::ipc::protocol::RequestId;

use super::WriterRef;
use crate::SharedState;

pub(super) async fn scm_memory_list(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let entries = shared.memory_store.list().await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"memories": entries, "count": entries.len()}),
        )
        .await;
}

pub(super) async fn scm_memory_add(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    content: String,
    category: String,
) {
    let cat = engine::memory::parse_category(&category);
    let result = shared
        .memory_store
        .add(content, cat, "user".to_string())
        .await;
    let mut conn_guard = writer.lock().await;
    match result {
        Ok(outcome) => {
            // The store rejects credential/injection content and collapses
            // exact duplicates itself, so the response reflects its verdict.
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({"memory": outcome.entry, "created": outcome.created}),
                )
                .await;
        }
        Err(e) => {
            eprintln!("ERROR: memory add failed: {}", e);
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Memory write failed: {}", e))
                .await;
        }
    }
}

pub(super) async fn scm_memory_delete(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    mem_id: String,
) {
    let result = shared.memory_store.delete(&mem_id).await;
    let mut conn_guard = writer.lock().await;
    match result {
        Ok(deleted) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"deleted": deleted}))
                .await;
        }
        Err(e) => {
            eprintln!("ERROR: memory delete failed: {}", e);
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Memory delete failed: {}", e))
                .await;
        }
    }
}

pub(super) async fn scm_memory_clear(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let result = shared.memory_store.clear().await;
    let mut conn_guard = writer.lock().await;
    match result {
        Ok(count) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"cleared": count}))
                .await;
        }
        Err(e) => {
            eprintln!("ERROR: memory clear failed: {}", e);
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Memory clear failed: {}", e))
                .await;
        }
    }
}

pub(super) async fn scm_memory_stats(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let stats = shared.memory_store.stats().await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard.send_response(id, serde_json::json!(stats)).await;
}

pub(super) async fn scm_memory_archive(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    mem_id: String,
) {
    let archived = shared
        .memory_store
        .archive_by_id(&mem_id, &shared.memory_archive)
        .await;
    let mut conn_guard = writer.lock().await;
    match archived {
        Some(entry) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"archived": entry}))
                .await;
        }
        None => {
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Memory not found: {}", mem_id))
                .await;
        }
    }
}

pub(super) async fn scm_memory_restore(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    mem_id: String,
) {
    let restored = shared
        .memory_store
        .restore_from_archive(&mem_id, &shared.memory_archive)
        .await;
    let mut conn_guard = writer.lock().await;
    match restored {
        Some(entry) => {
            let _ = conn_guard
                .send_response(id, serde_json::json!({"restored": entry}))
                .await;
        }
        None => {
            let _ = conn_guard
                .send_error(
                    Some(id),
                    -32000,
                    format!("Archived memory not found: {}", mem_id),
                )
                .await;
        }
    }
}

pub(super) async fn scm_memory_archive_list(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
) {
    let archived = shared.memory_archive.list_archived().await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({"archived": archived, "count": archived.len()}),
        )
        .await;
}

pub(super) async fn scm_memory_cleanup(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let result = shared.memory_cleanup.run_now().await;
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "archived_count": result.archived_count,
                "deleted_count": result.deleted_count,
                "timestamp": result.timestamp,
                "duration_ms": result.duration_ms,
            }),
        )
        .await;
}
