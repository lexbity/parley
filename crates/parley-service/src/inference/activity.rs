//! AR-001 AR09 · Owner Activity.
//!
//! One bounded union read over durable role jobs and delegated runs. Items are
//! sorted by `(updated_at DESC, kind DESC, id DESC)` and paged with a signed
//! keyset cursor so a page never scans the full history and the list carries
//! no transcript, output or raw payload rows. The endpoint is owner-only and
//! read-only: cancellation and review still flow through the existing
//! `POST /jobs/{id}` and run surfaces.

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::QueryBuilder;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{AppState, OwnerPrincipal, authenticate_owner, private_headers};

const DOMAIN: &[u8] = b"activity-cursor";
const CURSOR_VERSION: u8 = 1;
const CURSOR_TTL_SECS: u64 = 600;
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;
const RUN_MAX_TURNS: i64 = 16;

const JOB_STATES: [&str; 6] = [
    "queued",
    "running",
    "succeeded",
    "invalid",
    "failed",
    "cancelled",
];
const RUN_STATES: [&str; 8] = [
    "queued",
    "running",
    "waiting_owner",
    "waiting_job",
    "succeeded",
    "failed",
    "cancelled",
    "expired",
];
const SCOPE_KINDS: [&str; 6] = [
    "owner",
    "party",
    "segment",
    "opportunity",
    "material",
    "delegated",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivityQuery {
    kind: Option<String>,
    status: Option<String>,
    scope: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ActivityItem {
    kind: &'static str,
    id: String,
    role: String,
    status: String,
    state_revision: i64,
    attempt_count: Option<i64>,
    progress: Option<Progress>,
    scope: Option<Value>,
    error_code: Option<String>,
    review_ref: Option<Value>,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Progress {
    current: i64,
    total: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct Cursor {
    exp: u64,
    limit: usize,
    t: String,
    kind: String,
    id: String,
}

fn failure(status: StatusCode, code: &'static str, message: &str) -> Response {
    let mut response = Json(json!({
        "ok": false,
        "error": {"code": code, "message": message, "details": []}
    }))
    .into_response();
    *response.status_mut() = status;
    response
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// RFC 4648 base64url without padding (local codec, no dependency).
fn base64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

fn base64url_decode(data: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut values = [0u8; 128];
    for (index, byte) in ALPHABET.iter().enumerate() {
        values[*byte as usize] = index as u8;
    }
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for byte in data.bytes() {
        let value = match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => values[byte as usize] as u32,
            _ => return None,
        };
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if bits >= 6 {
        return None; // non-canonical trailing bits
    }
    Some(out)
}

fn encode_cursor(epoch: &str, secret: &[u8], cursor: &Cursor) -> String {
    let payload = json!({
        "v": CURSOR_VERSION,
        "exp": cursor.exp,
        "limit": cursor.limit,
        "t": cursor.t,
        "k": cursor.kind,
        "id": cursor.id,
    })
    .to_string();
    let tag =
        parley_domain::hmac::sign_hex(secret, DOMAIN, format!("{epoch}\0{payload}").as_bytes());
    format!("{}.{}", base64url(payload.as_bytes()), tag)
}

fn decode_cursor(epoch: &str, secret: &[u8], token: &str, limit: usize) -> Result<Cursor, ()> {
    let (encoded, tag) = token.split_once('.').ok_or(())?;
    let payload_bytes = base64url_decode(encoded).ok_or(())?;
    let payload = std::str::from_utf8(&payload_bytes).map_err(|_| ())?;
    let expect =
        parley_domain::hmac::sign_hex(secret, DOMAIN, format!("{epoch}\0{payload}").as_bytes());
    let tag = tag.as_bytes();
    let expect = expect.as_bytes();
    if tag.len() != expect.len() {
        return Err(());
    }
    let mut diff = 0u8;
    for (a, b) in tag.iter().zip(expect.iter()) {
        diff |= a ^ b;
    }
    if diff != 0 {
        return Err(());
    }
    let value: Value = serde_json::from_str(payload).map_err(|_| ())?;
    if value["v"].as_u64() != Some(u64::from(CURSOR_VERSION)) {
        return Err(());
    }
    let exp = value["exp"].as_u64().ok_or(())?;
    if exp < now_unix() {
        return Err(());
    }
    let cursor_limit = value["limit"].as_u64().ok_or(())? as usize;
    if cursor_limit != limit {
        return Err(());
    }
    let t = value["t"].as_str().ok_or(())?.to_owned();
    let kind = value["k"].as_str().ok_or(())?.to_owned();
    if !matches!(kind.as_str(), "job" | "run") {
        return Err(());
    }
    let id = value["id"].as_str().ok_or(())?.to_owned();
    Ok(Cursor {
        exp,
        limit,
        t,
        kind,
        id,
    })
}

pub(crate) struct JobRow {
    id: String,
    role: String,
    status: String,
    state_revision: i64,
    current_attempt: i64,
    progress_current: i64,
    progress_total: i64,
    scope_kind: String,
    scope_id: String,
    error_code: Option<String>,
    updated_at: String,
}

pub(crate) struct RunRow {
    id: String,
    state: String,
    state_revision: i64,
    turn_count: i64,
    updated_at: String,
    wake_condition: Option<String>,
}

#[allow(clippy::too_many_arguments)]
async fn read_jobs(
    pool: &sqlx::SqlitePool,
    epoch: &str,
    status: Option<&str>,
    scope: Option<&str>,
    limit: usize,
    cursor: Option<&Cursor>,
) -> Result<(Vec<JobRow>, bool), sqlx::Error> {
    let mut builder = QueryBuilder::new(
        "SELECT j.id, j.role, j.status, j.state_revision, j.current_attempt, \
         j.progress_current, j.progress_total, j.scope_kind, j.scope_id, \
         j.last_error_code, j.updated_at \
         FROM durable_job j WHERE j.epoch = ",
    );
    builder.push_bind(epoch);
    if let Some(status) = status {
        builder.push(" AND j.status = ");
        builder.push_bind(status);
    }
    if let Some(scope) = scope
        && scope != "delegated"
    {
        builder.push(" AND j.scope_kind = ");
        builder.push_bind(scope);
    }
    if let Some(cursor) = cursor {
        if cursor.kind == "job" {
            builder.push(" AND (j.updated_at < ");
            builder.push_bind(&cursor.t);
            builder.push(" OR (j.updated_at = ");
            builder.push_bind(&cursor.t);
            builder.push(" AND j.id < ");
            builder.push_bind(&cursor.id);
            builder.push("))");
        } else {
            builder.push(" AND j.updated_at < ");
            builder.push_bind(&cursor.t);
        }
    }
    builder.push(" ORDER BY j.updated_at DESC, j.id DESC LIMIT ");
    builder.push_bind(limit as i64);
    let rows = builder
        .build_query_as::<(
            String,
            String,
            String,
            i64,
            i64,
            i64,
            i64,
            String,
            String,
            Option<String>,
            String,
        )>()
        .fetch_all(pool)
        .await?;
    let rows = rows
        .into_iter()
        .map(
            |(
                id,
                role,
                status,
                state_revision,
                current_attempt,
                progress_current,
                progress_total,
                scope_kind,
                scope_id,
                error_code,
                updated_at,
            )| JobRow {
                id,
                role,
                status,
                state_revision,
                current_attempt,
                progress_current,
                progress_total,
                scope_kind,
                scope_id,
                error_code,
                updated_at,
            },
        )
        .collect::<Vec<_>>();
    let more = rows.len() >= limit;
    Ok((rows, more))
}

async fn read_runs(
    pool: &sqlx::SqlitePool,
    epoch: &str,
    owner: &str,
    status: Option<&str>,
    limit: usize,
    cursor: Option<&Cursor>,
) -> Result<(Vec<RunRow>, bool), sqlx::Error> {
    let mut builder = QueryBuilder::new(
        "SELECT r.run_id, r.state, r.state_revision, r.turn_count, r.updated_at, \
         r.wake_condition \
         FROM agent_run r WHERE r.epoch = ",
    );
    builder.push_bind(epoch);
    builder.push(" AND r.owner_pseudonym = ");
    builder.push_bind(owner);
    if let Some(status) = status {
        builder.push(" AND r.state = ");
        builder.push_bind(status);
    }
    if let Some(cursor) = cursor {
        if cursor.kind == "run" {
            builder.push(" AND (r.updated_at < ");
            builder.push_bind(&cursor.t);
            builder.push(" OR (r.updated_at = ");
            builder.push_bind(&cursor.t);
            builder.push(" AND r.run_id < ");
            builder.push_bind(&cursor.id);
            builder.push("))");
        } else {
            builder.push(" AND r.updated_at <= ");
            builder.push_bind(&cursor.t);
        }
    }
    builder.push(" ORDER BY r.updated_at DESC, r.run_id DESC LIMIT ");
    builder.push_bind(limit as i64);
    let rows = builder
        .build_query_as::<(String, String, i64, i64, String, Option<String>)>()
        .fetch_all(pool)
        .await?;
    let rows = rows
        .into_iter()
        .map(
            |(id, state, state_revision, turn_count, updated_at, wake_condition)| RunRow {
                id,
                state,
                state_revision,
                turn_count,
                updated_at,
                wake_condition,
            },
        )
        .collect::<Vec<_>>();
    let more = rows.len() >= limit;
    Ok((rows, more))
}

fn job_scope(row: &JobRow) -> Value {
    let kind = if row.role == "coach" && row.scope_kind == "owner" {
        "weekly"
    } else {
        row.scope_kind.as_str()
    };
    json!({"kind": kind, "id": row.scope_id})
}

async fn job_review_refs(
    pool: &sqlx::SqlitePool,
    rows: &[JobRow],
) -> Result<Vec<Option<Value>>, sqlx::Error> {
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    let mut refs = vec![None; rows.len()];
    if ids.is_empty() {
        return Ok(refs);
    }

    let mut coach = QueryBuilder::new("SELECT job_id, id FROM coach_report WHERE job_id IN (");
    let mut separated = coach.separated(", ");
    for id in &ids {
        separated.push_bind(*id);
    }
    coach.push(")");
    let coach = coach
        .build_query_as::<(String, String)>()
        .fetch_all(pool)
        .await?;

    let mut templates = QueryBuilder::new(
        "SELECT c.job_id, c.id FROM template_build_candidate c JOIN (SELECT job_id, MIN(variant_no) AS first_no FROM template_build_candidate WHERE job_id IN (",
    );
    let mut separated = templates.separated(", ");
    for id in &ids {
        separated.push_bind(*id);
    }
    templates.push(") GROUP BY job_id) m ON m.job_id = c.job_id AND m.first_no = c.variant_no");
    let templates = templates
        .build_query_as::<(String, String)>()
        .fetch_all(pool)
        .await?;

    let mut interviewers = QueryBuilder::new(
        "SELECT c.job_id, c.id FROM interviewer_candidate c JOIN (SELECT job_id, MAX(created_at) AS latest_at FROM interviewer_candidate WHERE job_id IN (",
    );
    let mut separated = interviewers.separated(", ");
    for id in &ids {
        separated.push_bind(*id);
    }
    interviewers.push(") GROUP BY job_id) m ON m.job_id = c.job_id AND m.latest_at = c.created_at");
    let interviewers = interviewers
        .build_query_as::<(String, String)>()
        .fetch_all(pool)
        .await?;

    let mut batches = QueryBuilder::new("SELECT job_id, id FROM proposal_batch WHERE job_id IN (");
    let mut separated = batches.separated(", ");
    for id in &ids {
        separated.push_bind(*id);
    }
    batches.push(")");
    let batches = batches
        .build_query_as::<(String, String)>()
        .fetch_all(pool)
        .await?;

    let mut matches =
        QueryBuilder::new("SELECT why_job_id, id FROM match_run WHERE why_job_id IN (");
    let mut separated = matches.separated(", ");
    for id in &ids {
        separated.push_bind(*id);
    }
    matches.push(")");
    let matches = matches
        .build_query_as::<(String, String)>()
        .fetch_all(pool)
        .await?;

    for (index, row) in rows.iter().enumerate() {
        refs[index] = match row.role.as_str() {
            "coach" => coach
                .iter()
                .find(|(job_id, _)| job_id == &row.id)
                .map(|(_, id)| json!({"kind":"coach_report","id":id})),
            "template_builder" => templates
                .iter()
                .find(|(job_id, _)| job_id == &row.id)
                .map(|(_, id)| json!({"kind":"template_build_candidate","id":id})),
            "interviewer" => interviewers
                .iter()
                .find(|(job_id, _)| job_id == &row.id)
                .map(|(_, id)| json!({"kind":"interviewer_candidate","id":id})),
            "briefer" => Some(json!({"kind":"brief_scope","id":row.scope_id})),
            "extractor" => batches
                .iter()
                .find(|(job_id, _)| job_id == &row.id)
                .map(|(_, id)| json!({"kind":"proposal_batch","id":id})),
            "matcher" => matches
                .iter()
                .find(|(job_id, _)| job_id == &row.id)
                .map(|(_, id)| json!({"kind":"match_run","id":id})),
            _ => None,
        };
    }
    Ok(refs)
}

async fn run_review_refs(
    pool: &sqlx::SqlitePool,
    rows: &[RunRow],
) -> Result<Vec<Option<Value>>, sqlx::Error> {
    let mut refs = vec![None; rows.len()];
    // Positions of succeeded runs in the page, by run id.
    let succeeded: Vec<(String, usize)> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.state == "succeeded")
        .map(|(index, row)| (row.id.clone(), index))
        .collect();
    if succeeded.is_empty() {
        return Ok(refs);
    }
    let mut builder =
        QueryBuilder::new("SELECT run_id, result_ref FROM agent_tool_intent WHERE run_id IN (");
    let mut separated = builder.separated(", ");
    for (run_id, _) in &succeeded {
        separated.push_bind(run_id);
    }
    builder.push(
        ") AND state='completed' AND result_ref IS NOT NULL ORDER BY turn_no DESC, ordinal DESC",
    );
    let intents: Vec<(String, String)> = builder
        .build_query_as::<(String, String)>()
        .fetch_all(pool)
        .await?;
    for (run_id, text) in intents {
        let Some(index) = succeeded
            .iter()
            .find(|(id, _)| id == &run_id)
            .map(|(_, index)| *index)
        else {
            continue;
        };
        let Some(value) = serde_json::from_str::<Value>(&text).ok() else {
            continue;
        };
        let Some(result) = value.get("result") else {
            continue;
        };
        let (Some(job_id), Some(scope_kind), Some(scope_id)) = (
            result.get("job_id").and_then(Value::as_str),
            result.get("scope_kind").and_then(Value::as_str),
            result.get("scope_id").and_then(Value::as_str),
        ) else {
            continue;
        };
        refs[index] = Some(json!({
            "kind":"delegated_review",
            "job_id":job_id,
            "scope_kind":scope_kind,
            "scope_id":scope_id,
        }));
    }
    Ok(refs)
}

fn etag(epoch: &str, items: &[ActivityItem], next: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(epoch.as_bytes());
    for item in items {
        hasher.update(item.kind.as_bytes());
        hasher.update([0u8]);
        hasher.update(item.id.as_bytes());
        hasher.update([0u8]);
        hasher.update(item.status.as_bytes());
        hasher.update([0u8]);
        hasher.update(item.state_revision.to_string().as_bytes());
        hasher.update([0u8]);
        hasher.update(item.attempt_count.unwrap_or(-1).to_string().as_bytes());
        hasher.update([0u8]);
    }
    hasher.update(next.unwrap_or("").as_bytes());
    format!("\"parley-activity-{}\"", hex::encode(hasher.finalize()))
}
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ActivityQuery>,
) -> Response {
    let principal: OwnerPrincipal = match authenticate_owner(&headers, &state).await {
        Ok(value) => value,
        Err((status, body)) => return (status, body).into_response(),
    };
    let epoch = state.workspace.epoch().to_owned();
    let secret = state.workspace.command_secret();
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let kind = query.kind.as_deref().unwrap_or("");
    if !matches!(kind, "" | "job" | "run") {
        return failure(
            StatusCode::BAD_REQUEST,
            "ACTIVITY_KIND_INVALID",
            "kind must be job, run, or omitted",
        );
    }
    let status_filter = query.status.as_deref().unwrap_or("");
    if !status_filter.is_empty() {
        let valid = match kind {
            "job" => JOB_STATES.contains(&status_filter),
            "run" => RUN_STATES.contains(&status_filter),
            _ => JOB_STATES.contains(&status_filter) || RUN_STATES.contains(&status_filter),
        };
        if !valid {
            return failure(
                StatusCode::BAD_REQUEST,
                "ACTIVITY_STATUS_INVALID",
                "Unknown status filter for the selected kind",
            );
        }
    }
    let scope_filter = query.scope.as_deref().unwrap_or("");
    if !scope_filter.is_empty() && !SCOPE_KINDS.contains(&scope_filter) {
        return failure(
            StatusCode::BAD_REQUEST,
            "ACTIVITY_SCOPE_INVALID",
            "Unknown scope filter",
        );
    }

    let cursor = match query.cursor.as_deref() {
        Some(token) if !token.is_empty() => match decode_cursor(&epoch, &secret, token, limit) {
            Ok(cursor) => Some(cursor),
            Err(()) => {
                return failure(
                    StatusCode::BAD_REQUEST,
                    "ACTIVITY_CURSOR_INVALID",
                    "The activity cursor is invalid, expired, or bound to another workspace",
                );
            }
        },
        _ => None,
    };

    let read_jobs_side = kind != "run";
    let read_runs_side = kind != "job";
    let (job_rows, jobs_more) = if read_jobs_side {
        match read_jobs(
            state.workspace.pool(),
            &epoch,
            if status_filter.is_empty() {
                None
            } else {
                Some(status_filter)
            },
            if scope_filter.is_empty() {
                None
            } else {
                Some(scope_filter)
            },
            limit,
            cursor.as_ref(),
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                return failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "PERSISTENCE_FAILED",
                    &error.to_string(),
                );
            }
        }
    } else {
        (Vec::new(), false)
    };
    let (run_rows, runs_more) = if read_runs_side {
        match read_runs(
            state.workspace.pool(),
            &epoch,
            &principal.pseudonym,
            if status_filter.is_empty() {
                None
            } else {
                Some(status_filter)
            },
            limit,
            cursor.as_ref(),
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                return failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "PERSISTENCE_FAILED",
                    &error.to_string(),
                );
            }
        }
    } else {
        (Vec::new(), false)
    };

    let job_refs = match job_review_refs(state.workspace.pool(), &job_rows).await {
        Ok(refs) => refs,
        Err(error) => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "PERSISTENCE_FAILED",
                &error.to_string(),
            );
        }
    };
    let run_refs = match run_review_refs(state.workspace.pool(), &run_rows).await {
        Ok(refs) => refs,
        Err(error) => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "PERSISTENCE_FAILED",
                &error.to_string(),
            );
        }
    };

    let mut merged: Vec<ActivityItem> = Vec::with_capacity(job_rows.len() + run_rows.len());
    for (index, row) in job_rows.iter().enumerate() {
        let progress = if row.progress_total > 0 {
            Some(Progress {
                current: row.progress_current,
                total: row.progress_total,
            })
        } else {
            None
        };
        merged.push(ActivityItem {
            kind: "job",
            id: row.id.clone(),
            role: row.role.clone(),
            status: row.status.clone(),
            state_revision: row.state_revision,
            attempt_count: Some(row.current_attempt),
            progress,
            scope: Some(job_scope(row)),
            error_code: row.error_code.clone(),
            review_ref: job_refs[index].clone(),
            updated_at: row.updated_at.clone(),
        });
    }
    for (index, row) in run_rows.iter().enumerate() {
        merged.push(ActivityItem {
            kind: "run",
            id: row.id.clone(),
            role: "delegated".into(),
            status: row.state.clone(),
            state_revision: row.state_revision,
            attempt_count: None,
            progress: Some(Progress {
                current: row.turn_count,
                total: RUN_MAX_TURNS,
            }),
            scope: Some(json!({"kind":"delegated","id":null})),
            error_code: row.wake_condition.clone(),
            review_ref: run_refs[index].clone(),
            updated_at: row.updated_at.clone(),
        });
    }
    merged.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.kind.cmp(left.kind))
            .then_with(|| right.id.cmp(&left.id))
    });
    merged.truncate(limit);

    let has_more = jobs_more || runs_more;
    let next_cursor = if has_more {
        merged.last().map(|last| {
            let cursor = Cursor {
                exp: now_unix() + CURSOR_TTL_SECS,
                limit,
                t: last.updated_at.clone(),
                kind: last.kind.to_owned(),
                id: last.id.clone(),
            };
            encode_cursor(&epoch, &secret, &cursor)
        })
    } else {
        None
    };

    let tag = etag(&epoch, &merged, next_cursor.as_deref());
    let mut response = Json(json!({
        "items": merged,
        "next_cursor": next_cursor,
        "epoch": epoch,
    }))
    .into_response();
    response.headers_mut().insert(
        header::ETAG,
        tag.parse()
            .unwrap_or_else(|_| header::HeaderValue::from_static("\"parley-activity\"")),
    );
    private_headers(&mut response, &epoch);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cursor(exp: u64) -> Cursor {
        Cursor {
            exp,
            limit: 20,
            t: "2026-11-24T10:00:00.000Z".into(),
            kind: "job".into(),
            id: "job-0001".into(),
        }
    }

    #[test]
    fn cursor_roundtrip_and_tamper_detection() {
        let secret = b"test-secret";
        let cursor = sample_cursor(now_unix() + 600);
        let token = encode_cursor("epoch-1", secret, &cursor);
        let decoded = decode_cursor("epoch-1", secret, &token, 20).unwrap();
        assert_eq!(decoded.kind, "job");
        assert_eq!(decoded.id, "job-0001");
        assert_eq!(decoded.limit, 20);
        assert_eq!(decoded.t, cursor.t);

        // Any tampering of payload or signature is rejected.
        let mut forged = token.clone();
        let len = forged.len();
        let byte = forged.as_bytes()[len - 1];
        forged.replace_range(
            len - 1..len,
            &char::from(if byte == b'a' { b'b' } else { b'a' }).to_string(),
        );
        assert!(decode_cursor("epoch-1", secret, &forged, 20).is_err());
        // Different epoch (workspace binding) fails even with a valid signature
        // for the other epoch.
        assert!(decode_cursor("epoch-2", secret, &token, 20).is_err());
        // Different page size is bound to the token.
        assert!(decode_cursor("epoch-1", secret, &token, 50).is_err());
        // Signature key mismatch fails.
        assert!(decode_cursor("epoch-1", b"other-secret", &token, 20).is_err());
    }

    #[test]
    fn cursor_expiry_is_rejected() {
        let secret = b"test-secret";
        let expired = sample_cursor(now_unix() - 1);
        let token = encode_cursor("epoch-1", secret, &expired);
        assert!(decode_cursor("epoch-1", secret, &token, 20).is_err());
    }

    #[test]
    fn base64url_codec_roundtrip() {
        for input in [
            b"".as_slice(),
            b"a",
            b"ab",
            b"abc",
            b"abcd",
            &[0u8, 1, 2, 3, 250],
        ] {
            let encoded = base64url(input);
            assert_eq!(base64url_decode(&encoded).unwrap(), input);
        }
        assert!(base64url_decode("a").is_none()); // non-canonical trailing bits
        assert!(base64url_decode("!!").is_none()); // invalid alphabet
    }
}
