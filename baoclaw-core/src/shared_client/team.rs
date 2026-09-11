use std::path::PathBuf;
use std::sync::Arc;

use baoclaw_core::engine;
use baoclaw_core::ipc::protocol::RequestId;

use super::{ScmFlow, WriterRef};
use crate::SharedState;

pub(super) async fn scm_team_spawn(
    shared: &SharedState,
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    count: Option<usize>,
    mode: String,
    task: String,
    policy: Option<serde_json::Value>,
) -> ScmFlow {
    use engine::team::{TeamConfig, TeamExecutor, TeamMode as EngineTeamMode, TeamPolicy};
    use std::str::FromStr;

    // Parse mode
    let team_mode = match EngineTeamMode::from_str(&mode) {
        Ok(m) => m,
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard.send_error(Some(id), -32602, e).await;
            return ScmFlow::Continue;
        }
    };

    // Parse policy if provided
    let team_policy: Option<TeamPolicy> = match policy {
        Some(p) => match serde_json::from_value(p) {
            Ok(policy) => Some(policy),
            Err(e) => {
                let mut conn_guard = writer.lock().await;
                let _ = conn_guard
                    .send_error(Some(id), -32602, format!("Invalid policy: {}", e))
                    .await;
                return ScmFlow::Continue;
            }
        },
        None => None,
    };

    // Create team config
    let config = TeamConfig {
        mode: team_mode.clone(),
        policy: team_policy,
        cwd: Some(work_cwd.to_string_lossy().to_string()),
        model: Some(shared.state_manager.get().model.clone()),
        ..Default::default()
    };

    // Create the executor and team
    let executor = TeamExecutor::with_registry(
        Arc::clone(&shared.api_client),
        Arc::clone(&shared.tool_registry),
        work_cwd.clone(),
        shared.state_manager.get().model.clone(),
        shared.headless_kit.clone(),
    );

    match executor.create_team(task.clone(), config).await {
        Ok(mut team) => {
            // For parallel mode, create the specified number of agents
            if team_mode == EngineTeamMode::Parallel {
                if let Err(e) = executor
                    .add_parallel_agents(&mut team, count.unwrap_or(1), &task)
                    .await
                {
                    let mut conn_guard = writer.lock().await;
                    let _ = conn_guard
                        .send_error(
                            Some(id),
                            -32000,
                            format!("Failed to add agents: {}", e.message),
                        )
                        .await;
                    return ScmFlow::Continue;
                }
            }

            let team_id = team.id.clone();
            let team_json = serde_json::to_value(&team).unwrap_or_default();

            // Store the team
            shared.team_executor.store_team(team).await;

            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "team_id": team_id,
                        "team": team_json,
                        "message": "Team created successfully"
                    }),
                )
                .await;
        }
        Err(e) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard.send_error(Some(id), -32000, e.message).await;
        }
    }
    ScmFlow::Continue
}

pub(super) async fn scm_team_list(shared: &SharedState, writer: WriterRef<'_>, id: RequestId) {
    let teams = shared.team_executor.list_teams().await;
    let count = teams.len();
    let mut conn_guard = writer.lock().await;
    let _ = conn_guard
        .send_response(
            id,
            serde_json::json!({
                "teams": teams,
                "count": count
            }),
        )
        .await;
}

pub(super) async fn scm_team_status(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    team_id: String,
) {
    match shared.team_executor.get_team(&team_id).await {
        Some(team) => {
            let summary = team.summary();
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "team": team,
                        "summary": summary
                    }),
                )
                .await;
        }
        None => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Team not found: {}", team_id))
                .await;
        }
    }
}

pub(super) async fn scm_team_results(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    team_id: String,
) {
    match shared.team_executor.get_team(&team_id).await {
        Some(team) => {
            let results = team.collect_results();
            let agents: Vec<serde_json::Value> = team
                .agents
                .iter()
                .map(|a| {
                    serde_json::json!({
                        "id": a.id,
                        "status": a.status.to_string(),
                        "result": a.result,
                        "error": a.error,
                        "tokens_used": a.tokens_used,
                        "cost_usd": a.cost_usd,
                    })
                })
                .collect();
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "team_id": team_id,
                        "status": team.status.to_string(),
                        "results": results,
                        "agents": agents,
                        "total_tokens": team.total_tokens,
                        "total_cost_usd": team.total_cost_usd,
                    }),
                )
                .await;
        }
        None => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Team not found: {}", team_id))
                .await;
        }
    }
}

pub(super) async fn scm_team_abort(
    shared: &SharedState,
    writer: WriterRef<'_>,
    id: RequestId,
    team_id: String,
) {
    match shared.team_executor.abort_team(&team_id).await {
        Some(team) => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "team_id": team_id,
                        "status": team.status.to_string(),
                        "message": "Team aborted"
                    }),
                )
                .await;
        }
        None => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Team not found: {}", team_id))
                .await;
        }
    }
}

pub(super) async fn scm_team_execute(
    shared: &SharedState,
    work_cwd: &PathBuf,
    writer: WriterRef<'_>,
    id: RequestId,
    team_id: String,
) {
    match shared.team_executor.get_team(&team_id).await {
        Some(team) => {
            // Spawn execution in background
            let executor = engine::team::TeamExecutor::with_registry(
                Arc::clone(&shared.api_client),
                Arc::clone(&shared.tool_registry),
                work_cwd.clone(),
                shared.state_manager.get().model.clone(),
                shared.headless_kit.clone(),
            );

            // Execute the team
            let result = executor.execute(team).await;

            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_response(
                    id,
                    serde_json::json!({
                        "team_id": result.team.id,
                        "success": result.success,
                        "error": result.error,
                        "duration_ms": result.duration_ms,
                        "status": result.team.status.to_string(),
                        "total_tokens": result.team.total_tokens,
                        "total_cost_usd": result.team.total_cost_usd,
                    }),
                )
                .await;
        }
        None => {
            let mut conn_guard = writer.lock().await;
            let _ = conn_guard
                .send_error(Some(id), -32000, format!("Team not found: {}", team_id))
                .await;
        }
    }
}
