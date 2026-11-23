//! Owner HTTP surface for delegated runs (UCR-001 §4.1).
//!
//! Every handler uses the owner session boundary and the shared writer. A run
//! ID or a claimed role in a body never conveys authority to a tool.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Connection;
use std::time::Instant;

use super::grants::{self, GrantTarget};
use crate::{AppState, OwnerPrincipal, authenticate_owner, command_operation_key, private_headers};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/agent/runs", post(start).get(list))
        .route("/api/v1/agent/runs/{id}", get(status))
        .route("/api/v1/agent/runs/{id}/cancel", post(cancel))
        .route("/api/v1/agent/runs/{id}/transcript", get(transcript))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartInput {
    task: String,
    targets: Vec<GrantTarget>,
    permissions: Vec<String>,
    #[serde(default)]
    allow_created_opportunities: bool,
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    status: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TranscriptQuery {
    limit: Option<usize>,
    cursor: Option<String>,
}

#[allow(clippy::result_large_err)] // axum Response is the transport error type
async fn owner(headers: &HeaderMap, state: &AppState) -> Result<OwnerPrincipal, Response> {
    authenticate_owner(headers, state)
        .await
        .map_err(|error| (error.0, error.1).into_response())
}

#[allow(clippy::result_large_err)]
fn failure(state: &AppState, status: StatusCode, code: &str, message: &str) -> Response {
    let mut response = Json(json!({
        "ok": false,
        "error": {"code": code, "message": message, "details": []}
    }))
    .into_response();
    *response.status_mut() = status;
    private_headers(&mut response, state.workspace.epoch());
    response
}

async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<StartInput>,
) -> Response {
    let principal = match owner(&headers, &state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let key = match command_operation_key(&headers) {
        Ok(key) => key,
        Err((status, code, message)) => return failure(&state, status, code, message),
    };
    if input.task.is_empty() || input.task.len() > 8 * 1024 {
        return failure(
            &state,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_args",
            "Task must be 1..=8192 bytes",
        );
    }
    let grant = match grants::build_grant(
        input.permissions.clone(),
        input.targets.clone(),
        input.allow_created_opportunities,
    ) {
        Ok(grant) => grant,
        Err(message) => {
            return failure(
                &state,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_args",
                &message,
            );
        }
    };
    let epoch = state.workspace.epoch().to_owned();
    let secret = state.workspace.command_secret();
    let key_digest = crate::commands::ledger::key_digest(
        &secret,
        &epoch,
        &principal.pseudonym,
        "agent.run",
        &key,
    );
    let grant_json = serde_json::to_string(&grant).unwrap_or_default();
    let scope_digest = crate::digest(grant_json.as_bytes());
    let deadline_at = crate::now_utc_offset(15 * 60);
    let task = input.task.clone();
    let targets = input.targets.clone();
    let permissions = input.permissions.clone();

    let job = crate::application::writer::WriterJob {
        deadline: Instant::now() + crate::application::writer::EXECUTION_DEADLINE,
        generation: state.workspace.generation_token().await,
        run: Box::new(move |connection, deadline| {
            let epoch = epoch.clone();
            let principal = principal.clone();
            let key_digest = key_digest.clone();
            let grant_json = grant_json.clone();
            let scope_digest = scope_digest.clone();
            let deadline_at = deadline_at.clone();
            let task = task.clone();
            let targets = targets.clone();
            Box::pin(async move {
                let mut tx = match connection.begin_with("BEGIN IMMEDIATE").await {
                    Ok(tx) => tx,
                    Err(_) => return busy_job(),
                };
                let existing: Option<String> = match sqlx::query_scalar(
                    "SELECT run_id FROM agent_run WHERE epoch=? AND owner_pseudonym=? AND key_digest=?",
                )
                .bind(&epoch)
                .bind(&principal.pseudonym)
                .bind(&key_digest)
                .fetch_optional(&mut *tx)
                .await
                {
                    Ok(value) => value,
                    Err(_) => return busy_job(),
                };
                let run_id = if let Some(run_id) = existing {
                    run_id
                } else {
                    let run_id = crate::new_id("agent-run");
                    let now = crate::now_utc();
                    if sqlx::query(
                        "INSERT INTO agent_run(run_id,epoch,owner_pseudonym,task,key_digest,state,state_revision,grant_json,scope_digest,created_at,updated_at,deadline_at) VALUES(?,?,?,?,?,'queued',1,?,?,?,?,?)",
                    )
                    .bind(&run_id)
                    .bind(&epoch)
                    .bind(&principal.pseudonym)
                    .bind(&task)
                    .bind(&key_digest)
                    .bind(&grant_json)
                    .bind(&scope_digest)
                    .bind(&now)
                    .bind(&now)
                    .bind(&deadline_at)
                    .execute(&mut *tx)
                    .await
                    .is_err()
                    {
                        return busy_job();
                    }
                    for target in &targets {
                        let _ = sqlx::query(
                            "INSERT INTO agent_run_target(run_id,kind,target_id) VALUES(?,?,?)",
                        )
                        .bind(&run_id)
                        .bind(&target.kind)
                        .bind(&target.id)
                        .execute(&mut *tx)
                        .await;
                    }
                    let _ =
                        sqlx::query("INSERT INTO agent_run_budget(run_id,updated_at) VALUES(?,?)")
                            .bind(&run_id)
                            .bind(&now)
                            .execute(&mut *tx)
                            .await;
                    let _ = sqlx::query(
                        "INSERT OR IGNORE INTO agent_run_turn(run_id,turn_no,role,ordinal,body,detail_json,created_at) VALUES(?,1,'owner',1,?,'{}',?)",
                    )
                    .bind(&run_id)
                    .bind(&task)
                    .bind(&now)
                    .execute(&mut *tx)
                    .await;
                    run_id
                };
                let body = json!({
                    "run_id": run_id,
                    "state": "queued",
                    "revision": 1,
                    "granted_actions": permissions,
                    "targets": targets,
                    "deadline_at": deadline_at,
                    "phase": "queued"
                });
                match crate::application::writer::commit_tx(tx, deadline, "").await {
                    Ok(()) => crate::application::writer::JobResult {
                        commit: true,
                        body,
                        failure: None,
                    },
                    Err(failure) => crate::application::writer::JobResult {
                        commit: false,
                        body: Value::Null,
                        failure: Some(failure),
                    },
                }
            })
        }),
        reply: tokio::sync::oneshot::channel().0,
    };
    match state.writer.run(job).await {
        crate::application::writer::WriteOutcome::Committed(body) => {
            let mut response = Json(body).into_response();
            *response.status_mut() = StatusCode::CREATED;
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        crate::application::writer::WriteOutcome::Rejected(_) => failure(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "The workspace writer is unavailable; retry with the same key",
        ),
    }
}

fn busy_job() -> crate::application::writer::JobResult {
    crate::application::writer::JobResult {
        commit: false,
        body: Value::Null,
        failure: Some(crate::application::writer::Failure::Unavailable),
    }
}

async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    let principal = match owner(&headers, &state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(10).clamp(1, 20);
    let status_filter = query.status.unwrap_or_default();
    if !status_filter.is_empty()
        && !matches!(
            status_filter.as_str(),
            "queued"
                | "running"
                | "waiting_owner"
                | "waiting_job"
                | "succeeded"
                | "failed"
                | "cancelled"
                | "expired"
        )
    {
        return failure(
            &state,
            StatusCode::BAD_REQUEST,
            "invalid_args",
            "Unknown status filter",
        );
    }
    let cursor = query.cursor.unwrap_or_default();
    let rows: Vec<(String, String, i64, String, String)> = sqlx::query_as(
        "SELECT run_id,state,state_revision,deadline_at,scope_digest FROM agent_run WHERE epoch=? AND owner_pseudonym=? AND (?='' OR state=?) AND run_id>? ORDER BY run_id LIMIT ?",
    )
    .bind(state.workspace.epoch())
    .bind(&principal.pseudonym)
    .bind(&status_filter)
    .bind(&status_filter)
    .bind(&cursor)
    .bind(limit as i64)
    .fetch_all(state.workspace.pool())
    .await
    .unwrap_or_default();
    let next = rows.last().map(|row| row.0.clone());
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(run_id, state_name, revision, deadline_at, _)| {
            json!({"run_id": run_id, "state": state_name, "revision": revision, "deadline_at": deadline_at})
        })
        .collect();
    let mut response = Json(json!({"items": items, "next_cursor": next})).into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let principal = match owner(&headers, &state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let row: Option<(String, i64, String, i64, i64, String)> = sqlx::query_as(
        "SELECT state,state_revision,deadline_at,turn_count,transcript_bytes,wake_condition FROM agent_run WHERE run_id=? AND epoch=? AND owner_pseudonym=?",
    )
    .bind(&id)
    .bind(state.workspace.epoch())
    .bind(&principal.pseudonym)
    .fetch_optional(state.workspace.pool())
    .await
    .unwrap_or(None);
    let Some((state_name, revision, deadline_at, turn_count, transcript_bytes, wake)) = row else {
        return failure(
            &state,
            StatusCode::NOT_FOUND,
            "target_unavailable",
            "Run is not available",
        );
    };
    let mut response = Json(json!({
        "run_id": id,
        "state": state_name,
        "revision": revision,
        "deadline_at": deadline_at,
        "turn_count": turn_count,
        "transcript_bytes": transcript_bytes,
        "wake_condition": wake,
    }))
    .into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

async fn transcript(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<TranscriptQuery>,
) -> Response {
    let principal = match owner(&headers, &state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let owned: Option<String> = sqlx::query_scalar(
        "SELECT run_id FROM agent_run WHERE run_id=? AND epoch=? AND owner_pseudonym=?",
    )
    .bind(&id)
    .bind(state.workspace.epoch())
    .bind(&principal.pseudonym)
    .fetch_optional(state.workspace.pool())
    .await
    .unwrap_or(None);
    if owned.is_none() {
        return failure(
            &state,
            StatusCode::NOT_FOUND,
            "target_unavailable",
            "Run is not available",
        );
    }
    let limit = query.limit.unwrap_or(20).clamp(1, 20);
    let cursor = query.cursor.unwrap_or_default();
    let (turn_no, ordinal): (i64, i64) = decode_cursor(&cursor).unwrap_or((0, 0));
    let rows: Vec<(i64, String, i64, String)> = sqlx::query_as(
        "SELECT turn_no,role,ordinal,body FROM agent_run_turn WHERE run_id=? AND (turn_no>? OR (turn_no=? AND ordinal>?)) ORDER BY turn_no,ordinal,role LIMIT ?",
    )
    .bind(&id)
    .bind(turn_no)
    .bind(turn_no)
    .bind(ordinal)
    .bind(limit as i64)
    .fetch_all(state.workspace.pool())
    .await
    .unwrap_or_default();
    let next = rows.last().map(|row| encode_cursor(row.0, row.2));
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(turn, role, ordinal, body)| {
            json!({"turn_no": turn, "speaker": role, "ordinal": ordinal, "text": body})
        })
        .collect();
    let mut response = Json(json!({"items": items, "next_cursor": next})).into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

fn encode_cursor(turn_no: i64, ordinal: i64) -> String {
    hex::encode(format!("{turn_no}:{ordinal}"))
}

fn decode_cursor(cursor: &str) -> Option<(i64, i64)> {
    let bytes = hex::decode(cursor).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let (turn, ordinal) = text.split_once(':')?;
    Some((turn.parse().ok()?, ordinal.parse().ok()?))
}

async fn cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let principal = match owner(&headers, &state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let _key = match command_operation_key(&headers) {
        Ok(key) => key,
        Err((status, code, message)) => return failure(&state, status, code, message),
    };
    let epoch = state.workspace.epoch().to_owned();
    let job = crate::application::writer::WriterJob {
        deadline: Instant::now() + crate::application::writer::EXECUTION_DEADLINE,
        generation: state.workspace.generation_token().await,
        run: Box::new(move |connection, deadline| {
            let id = id.clone();
            let epoch = epoch.clone();
            let principal = principal.clone();
            Box::pin(async move {
                let mut tx = match connection.begin_with("BEGIN IMMEDIATE").await {
                    Ok(tx) => tx,
                    Err(_) => return busy_job(),
                };
                let updated = sqlx::query(
                    "UPDATE agent_run SET state='cancelled', grant_json='{}', lease_token=NULL, lease_deadline=NULL, finished_at=?, state_revision=state_revision+1, updated_at=? WHERE run_id=? AND epoch=? AND owner_pseudonym=? AND state NOT IN ('succeeded','failed','cancelled','expired')",
                )
                .bind(crate::now_utc())
                .bind(crate::now_utc())
                .bind(&id)
                .bind(&epoch)
                .bind(&principal.pseudonym)
                .execute(&mut *tx)
                .await;
                let ok = matches!(updated, Ok(result) if result.rows_affected() == 1);
                let body =
                    json!({"run_id": id, "state": if ok { "cancelled" } else { "unchanged" }});
                match crate::application::writer::commit_tx(tx, deadline, "").await {
                    Ok(()) => crate::application::writer::JobResult {
                        commit: true,
                        body,
                        failure: None,
                    },
                    Err(failure) => crate::application::writer::JobResult {
                        commit: false,
                        body: Value::Null,
                        failure: Some(failure),
                    },
                }
            })
        }),
        reply: tokio::sync::oneshot::channel().0,
    };
    match state.writer.run(job).await {
        crate::application::writer::WriteOutcome::Committed(body) => {
            let mut response = Json(body).into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        crate::application::writer::WriteOutcome::Rejected(_) => failure(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "The workspace writer is unavailable",
        ),
    }
}
