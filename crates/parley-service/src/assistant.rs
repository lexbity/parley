//! Owner Interviewer and scoped Briefer commands. Model runs are durable
//! proposals; owner decisions are the only writes to resource/brief facts.
use super::{AppState, WorkspaceError, digest, new_id, now_utc};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, SqlitePool, Transaction};
use std::collections::BTreeSet;

const DEFAULT_QUESTIONS: &str = include_str!("../roles/interviewer/questions.json");
const INTERVIEWER_PROMPT: &str = include_str!("../roles/interviewer/prompt.md");
const BRIEFER_PROMPT: &str = include_str!("../roles/briefer/prompt.md");
use crate::application::coach::PROMPT as COACH_PROMPT;
const MAX_TURN_BYTES: usize = 16_000;
const MAX_PROMPT_BYTES: usize = 40_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateChat {
    role: String,
    scope_kind: String,
    #[serde(default)]
    scope_id: Option<String>,
    #[serde(default)]
    asset_kind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ChatTurn {
    id: String,
    turn_no: i64,
    speaker: String,
    text: String,
    job_id: Option<String>,
    created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ChatRecord {
    id: String,
    role: String,
    scope_kind: String,
    scope_id: Option<String>,
    asset_kind: Option<String>,
    revision: i64,
    active_job_id: Option<String>,
    turns: Vec<ChatTurn>,
    questions: Vec<QuestionState>,
    candidates: Vec<CandidateRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow)]
struct ChatSummary {
    id: String,
    role: String,
    scope_kind: String,
    scope_id: Option<String>,
    asset_kind: Option<String>,
    revision: i64,
    last_turn: Option<String>,
    updated_at: String,
    active_job_id: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ChatListQuery {
    role: Option<String>,
    scope_kind: Option<String>,
    scope_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct QuestionState {
    question_id: String,
    state: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CandidateRecord {
    id: String,
    asset_kind: String,
    fields: Value,
    owner_quotes: Value,
    missing_fields: Vec<String>,
    state: String,
    resource_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TurnRequest {
    text: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionAction {
    question_id: String,
    action: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateConfirmation {
    title: String,
    fields: Value,
    expected_chat_revision: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    column_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BriefDecision {
    decision: String,
    corrected_text: Option<String>,
    expected_revision: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptUpdate {
    expected_version_id: String,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionBankUpdate {
    expected_version_id: String,
    content: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SegmentCreate {
    name: String,
    description: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SegmentParty {
    party_id: String,
    action: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BriefItem {
    id: String,
    scope_kind: String,
    scope_id: String,
    kind: String,
    text: String,
    detail: Value,
    tier: i64,
    quote: Option<String>,
    check_suggestion: Option<String>,
    speaker_kind: String,
    status: String,
    revision: i64,
    supersedes_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FluencyUpdate {
    status: String,
    expected_revision: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow)]
struct FluencyRecord {
    term: String,
    status: String,
    revision: i64,
}

type ChatScopeRow = (String, String, Option<String>, Option<String>, i64);
type SegmentRow = (String, String, Option<String>, i64);
type BriefItemRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    String,
    String,
    i64,
    Option<String>,
);
type BriefDecisionRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

pub(crate) fn allowed_fields(kind: &str) -> Option<BTreeSet<&'static str>> {
    let fields: &[&str] = match kind {
        "proof" => &[
            "what_i_did",
            "for_whom",
            "permission_to_name",
            "problem",
            "result_value",
            "result_unit",
            "baseline",
            "period",
            "my_role",
            "evidence_ref",
        ],
        "gift" => &[
            "what",
            "format",
            "problem_tags",
            "delivery",
            "time_to_produce_minutes",
            "evidence_ref",
            "cost_to_me",
        ],
        "artifact" => &[
            "title",
            "location",
            "domain_tags",
            "date",
            "what_it_shows",
            "public",
        ],
        _ => return None,
    };
    Some(fields.iter().copied().collect())
}

pub(crate) fn missing_fields(kind: &str, fields: &Value) -> Vec<String> {
    let required: &[&str] = match kind {
        "proof" => &[
            "what_i_did",
            "for_whom",
            "permission_to_name",
            "problem",
            "result_value",
            "result_unit",
            "period",
            "my_role",
        ],
        "gift" => &["what", "format", "problem_tags", "delivery", "cost_to_me"],
        "artifact" => &[
            "title",
            "location",
            "domain_tags",
            "date",
            "what_it_shows",
            "public",
        ],
        _ => return vec!["asset_kind".into()],
    };
    let mut missing: Vec<String> = required
        .iter()
        .filter(|key| {
            fields.get(**key).is_none_or(|value| {
                value.is_null() || value.as_str().is_some_and(|text| text.trim().is_empty())
            })
        })
        .map(|key| (*key).to_owned())
        .collect();
    if kind == "proof" {
        if fields["result_value"].as_f64().is_none()
            && !missing.iter().any(|field| field == "result_value")
        {
            missing.push("result_value (numeric)".into());
        }
        if fields["what_i_did"]
            .as_str()
            .is_none_or(|text| text.split_whitespace().count() < 6)
            && !missing.iter().any(|field| field == "what_i_did")
        {
            missing.push("what_i_did (at least 6 words)".into());
        }
        if fields["permission_to_name"].as_bool().is_none()
            && !missing.iter().any(|field| field == "permission_to_name")
        {
            missing.push("permission_to_name (boolean)".into());
        }
        if fields["result_unit"]
            .as_str()
            .is_none_or(|value| value.trim().is_empty())
            && !missing.iter().any(|field| field == "result_unit")
        {
            missing.push("result_unit".into());
        }
        if fields["period"]
            .as_str()
            .is_none_or(|value| value.trim().is_empty())
            && !missing.iter().any(|field| field == "period")
        {
            missing.push("period".into());
        }
        if fields["my_role"]
            .as_str()
            .is_none_or(|value| value.trim().is_empty())
            && !missing.iter().any(|field| field == "my_role")
        {
            missing.push("my_role".into());
        }
    }
    if kind == "gift" {
        if !matches!(
            fields["format"].as_str(),
            Some("teardown" | "intro" | "resource" | "audit" | "bug report" | "template" | "other")
        ) && !missing.iter().any(|field| field == "format")
        {
            missing.push("format (unsupported value)".into());
        }
        if !matches!(fields["delivery"].as_str(), Some("exists" | "on_request"))
            && !missing.iter().any(|field| field == "delivery")
        {
            missing.push("delivery (unsupported value)".into());
        }
        if !matches!(
            fields["cost_to_me"].as_str(),
            Some("low" | "medium" | "high")
        ) && !missing.iter().any(|field| field == "cost_to_me")
        {
            missing.push("cost_to_me (unsupported value)".into());
        }
        if fields["problem_tags"].as_array().is_none_or(Vec::is_empty)
            && !missing.iter().any(|field| field == "problem_tags")
        {
            missing.push("problem_tags (at least one)".into());
        }
        if fields["delivery"] == "on_request"
            && fields["time_to_produce_minutes"]
                .as_f64()
                .is_none_or(|value| value <= 0.0)
        {
            missing.push("time_to_produce_minutes (positive estimate)".into());
        }
        if fields["delivery"] == "exists"
            && fields["evidence_ref"].as_str().is_none_or(str::is_empty)
        {
            missing.push("evidence_ref".into());
        }
    }
    if kind == "proof"
        && fields["result_unit"] == "%"
        && fields["baseline"].as_str().is_none_or(str::is_empty)
    {
        missing.push("baseline".into());
    }
    if kind == "artifact" {
        if fields["domain_tags"].as_array().is_none_or(Vec::is_empty)
            && !missing.iter().any(|field| field == "domain_tags")
        {
            missing.push("domain_tags (at least one)".into());
        }
        if fields["public"].as_bool() != Some(true) {
            missing.push("public (share permission required)".into());
        }
    }
    missing.sort();
    missing.dedup();
    missing
}

pub(super) async fn initialize_defaults(pool: &SqlitePool) -> Result<(), WorkspaceError> {
    let now = now_utc();
    let defaults = [
        ("interviewer", INTERVIEWER_PROMPT, "interviewer-v1"),
        ("briefer", BRIEFER_PROMPT, "briefer-v1"),
        ("coach", COACH_PROMPT, "coach-v2"),
    ];
    for (role, prompt, id) in defaults {
        let hash = digest(prompt.as_bytes());
        sqlx::query("INSERT OR IGNORE INTO agent_prompt_version(role,version_no,version_id,prompt_text,prompt_sha256,created_by,created_at) VALUES(?,1,?,?,?,'bundled',?)")
            .bind(role).bind(id).bind(prompt).bind(hash).bind(&now).execute(pool).await?;
        sqlx::query("INSERT OR IGNORE INTO agent_prompt_active(role,version_id) VALUES(?,?)")
            .bind(role)
            .bind(id)
            .execute(pool)
            .await?;
    }
    let questions: Value = serde_json::from_str(DEFAULT_QUESTIONS)
        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
    let questions_text =
        serde_json::to_string(&questions).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
    let questions_hash = digest(questions_text.as_bytes());
    sqlx::query("INSERT OR IGNORE INTO interviewer_question_bank_version(version_id,version_no,content_json,content_sha256,created_by,created_at) VALUES('question-bank-v1',1,?,?,'bundled',?)")
        .bind(&questions_text).bind(questions_hash).bind(&now).execute(pool).await?;
    sqlx::query("INSERT OR IGNORE INTO interviewer_question_bank_active(singleton,version_id) VALUES(1,'question-bank-v1')")
        .execute(pool).await?;
    Ok(())
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/agent/chats", get(list_chats).post(create_chat))
        .route("/api/v1/agent/chats/{id}", get(get_chat))
        .route("/api/v1/agent/chats/{id}/turns", post(add_turn))
        .route("/api/v1/agent/chats/{id}/questions", post(question_action))
        .route(
            "/api/v1/agent/candidates/{id}/confirm",
            post(confirm_candidate),
        )
        .route(
            "/api/v1/agent/candidates/{id}/dismiss",
            post(dismiss_candidate),
        )
        .route(
            "/api/v1/agent/question-bank",
            get(get_question_bank).put(update_question_bank),
        )
        .route(
            "/api/v1/agent/prompts/{role}",
            get(get_prompt).put(update_prompt),
        )
        .route("/api/v1/segments", get(list_segments).post(create_segment))
        .route("/api/v1/segments/{id}/parties", post(update_segment_party))
        .route(
            "/api/v1/briefs/{scope_kind}/{scope_id}/items",
            get(list_brief_items).post(generate_brief),
        )
        .route("/api/v1/brief-items/{id}/decision", post(decide_brief_item))
        .route("/api/v1/fluency", get(list_fluency))
        .route("/api/v1/fluency/{term}", put(update_fluency))
}

async fn create_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateChat>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if !matches!(request.role.as_str(), "interviewer" | "briefer") || !valid_chat_scope(&request) {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "CHAT_SCOPE_INVALID",
            "Role, scope, and asset kind are incompatible",
        );
    }
    let key = match idempotency_key(&headers) {
        Ok(value) => value,
        Err(message) => {
            return super::error(StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED", message);
        }
    };
    let canonical = digest(
        format!(
            "POST\0/agent/chats\0{}",
            serde_json::to_string(&request).unwrap_or_default()
        )
        .as_bytes(),
    );
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(response) =
        receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    if let Err(error) = validate_scope(
        &mut tx,
        &request.scope_kind,
        request.scope_id.as_deref(),
        &request.role,
    )
    .await
    {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "CHAT_SCOPE_INVALID",
            error,
        );
    }
    let id = new_id(&canonical);
    let now = now_utc();
    if let Err(error)=sqlx::query("INSERT INTO agent_chat(id,role,scope_kind,scope_id,asset_kind,created_at,updated_at) VALUES(?,?,?,?,?,?,?)")
        .bind(&id).bind(&request.role).bind(&request.scope_kind).bind(&request.scope_id).bind(&request.asset_kind).bind(&now).bind(&now).execute(&mut *tx).await {
        return super::command_error(error.into());
    }
    let cursor = match append_event(
        &mut tx,
        &id,
        "agent_chat.created",
        json!({"role":request.role,"scope_kind":request.scope_kind}),
    )
    .await
    {
        Ok(cursor) => cursor,
        Err(error) => return super::command_error(error),
    };
    let result = json!({"id":id,"role":request.role,"scope_kind":request.scope_kind,"scope_id":request.scope_id,"asset_kind":request.asset_kind,"revision":1,"active_job_id":null,"turns":[],"questions":[],"candidates":[]});
    if let Err(error) = write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        201,
        &result,
        cursor,
    )
    .await
    {
        return super::command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    response_json(StatusCode::CREATED, result, state.workspace.epoch())
}

fn valid_chat_scope(request: &CreateChat) -> bool {
    match (request.role.as_str(), request.scope_kind.as_str()) {
        ("interviewer", "owner") => {
            request.scope_id.is_none()
                && request
                    .asset_kind
                    .as_deref()
                    .is_some_and(|kind| matches!(kind, "gift" | "proof" | "artifact"))
        }
        ("briefer", "party") => {
            request.scope_id.as_deref().is_some_and(|id| !id.is_empty())
                && request.asset_kind.is_none()
        }
        ("briefer", "segment") => {
            request.scope_id.as_deref().is_some_and(|id| !id.is_empty())
                && request.asset_kind.is_none()
        }
        _ => false,
    }
}

async fn validate_scope(
    tx: &mut Transaction<'_, Sqlite>,
    scope_kind: &str,
    scope_id: Option<&str>,
    role: &str,
) -> Result<(), String> {
    match scope_kind {
        "owner" if role == "interviewer" => Ok(()),
        "party" => {
            let exists: Option<String> =
                sqlx::query_scalar("SELECT id FROM entity WHERE id=? AND kind IN ('person','org')")
                    .bind(scope_id.unwrap_or_default())
                    .fetch_optional(&mut **tx)
                    .await
                    .map_err(|e| e.to_string())?;
            exists
                .map(|_| ())
                .ok_or_else(|| "Party scope must be an existing person or organization".into())
        }
        "segment" => {
            let exists: Option<String> = sqlx::query_scalar("SELECT id FROM segment WHERE id=?")
                .bind(scope_id.unwrap_or_default())
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| e.to_string())?;
            exists
                .map(|_| ())
                .ok_or_else(|| "Segment scope must be an existing segment".into())
        }
        _ => Err("Scope kind is not permitted for this role".into()),
    }
}

async fn get_chat(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    match read_chat(state.workspace.pool(), &id).await {
        Ok(Some(chat)) => response_json(
            StatusCode::OK,
            serde_json::to_value(chat).unwrap_or(Value::Null),
            state.workspace.epoch(),
        ),
        Ok(None) => super::error(
            StatusCode::NOT_FOUND,
            "CHAT_NOT_FOUND",
            "Chat does not exist",
        ),
        Err(error) => super::command_error(error),
    }
}

async fn list_chats(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ChatListQuery>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if query
        .role
        .as_deref()
        .is_some_and(|role| !matches!(role, "interviewer" | "briefer"))
        || query
            .scope_kind
            .as_deref()
            .is_some_and(|scope| !matches!(scope, "owner" | "party" | "segment"))
    {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "CHAT_FILTER_INVALID",
            "Chat filter is unsupported",
        );
    }
    let rows:Result<Vec<ChatSummary>,_>=sqlx::query_as(
        "SELECT c.id,c.role,c.scope_kind,c.scope_id,c.asset_kind,c.revision,(SELECT t.text FROM agent_turn t WHERE t.chat_id=c.id ORDER BY t.turn_no DESC LIMIT 1) AS last_turn,c.updated_at,(SELECT j.id FROM durable_job j WHERE j.status IN ('queued','running') AND j.chat_id=c.id ORDER BY j.created_at DESC LIMIT 1) AS active_job_id FROM agent_chat c WHERE (? IS NULL OR c.role=?) AND (? IS NULL OR c.scope_kind=?) AND (? IS NULL OR c.scope_id=?) ORDER BY c.updated_at DESC,c.id",
    ).bind(&query.role).bind(&query.role).bind(&query.scope_kind).bind(&query.scope_kind).bind(&query.scope_id).bind(&query.scope_id).fetch_all(state.workspace.pool()).await;
    match rows {
        Ok(rows) => response_json(StatusCode::OK, json!(rows), state.workspace.epoch()),
        Err(error) => super::command_error(error.into()),
    }
}

async fn read_chat(pool: &SqlitePool, id: &str) -> Result<Option<ChatRecord>, WorkspaceError> {
    let row: Option<ChatScopeRow> = sqlx::query_as(
        "SELECT role,scope_kind,scope_id,asset_kind,revision FROM agent_chat WHERE id=?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some((role, scope_kind, scope_id, asset_kind, revision)) = row else {
        return Ok(None);
    };
    let turns:Vec<ChatTurn>=sqlx::query_as("SELECT id,turn_no,speaker,text,job_id,created_at FROM agent_turn WHERE chat_id=? ORDER BY turn_no")
        .bind(id).fetch_all(pool).await?.into_iter().map(|row:(String,i64,String,String,Option<String>,String)|ChatTurn{id:row.0,turn_no:row.1,speaker:row.2,text:row.3,job_id:row.4,created_at:row.5}).collect();
    let questions:Vec<QuestionState>=sqlx::query_as("SELECT question_id,state FROM interviewer_question_state WHERE chat_id=? ORDER BY question_id")
        .bind(id).fetch_all(pool).await?.into_iter().map(|row:(String,String)|QuestionState{question_id:row.0,state:row.1}).collect();
    let candidates:Vec<CandidateRecord>=sqlx::query_as("SELECT id,asset_kind,fields_json,owner_quotes_json,missing_fields_json,state,resource_id FROM interviewer_candidate WHERE chat_id=? ORDER BY created_at,id")
        .bind(id).fetch_all(pool).await?.into_iter().map(|row:(String,String,String,String,String,String,Option<String>)|CandidateRecord{id:row.0,asset_kind:row.1,fields:serde_json::from_str(&row.2).unwrap_or(Value::Null),owner_quotes:serde_json::from_str(&row.3).unwrap_or(Value::Null),missing_fields:serde_json::from_str(&row.4).unwrap_or_default(),state:row.5,resource_id:row.6}).collect();
    let active_job_id:Option<String>=sqlx::query_scalar("SELECT id FROM durable_job WHERE status IN ('queued','running') AND chat_id=? ORDER BY created_at DESC LIMIT 1").bind(id).fetch_optional(pool).await?;
    Ok(Some(ChatRecord {
        id: id.to_owned(),
        role,
        scope_kind,
        scope_id,
        asset_kind,
        revision,
        active_job_id,
        turns,
        questions,
        candidates,
    }))
}

async fn add_turn(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<TurnRequest>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let text = request.text.trim().to_owned();
    if text.is_empty() || text.len() > MAX_TURN_BYTES {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "TURN_INVALID",
            "Owner turn must contain 1–16,000 UTF-8 bytes",
        );
    }
    let key = match idempotency_key(&headers) {
        Ok(value) => value,
        Err(message) => {
            return super::error(StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED", message);
        }
    };
    let canonical = digest(format!("POST\0/agent/chats/{id}/turns\0{text}").as_bytes());
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(response) =
        receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM agent_chat WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
    // One typed intake per role. Neither handler path resolves scope, prompts
    // or jobs itself; the application operations own admission (AR-001 AR04).
    let (result, cursor): (Value, i64) = match role.as_deref() {
        Some("interviewer") => {
            let input = crate::application::interview::OwnerTurnInput {
                chat_id: id.clone(),
                text,
                epoch: state.workspace.epoch().to_owned(),
                create_chat_if_missing: false,
                asset_kind: None,
                coach_question_id: None,
            };
            let outcome = match crate::application::interview::start_owner_turn(&mut tx, &input).await
            {
                Ok(outcome) => outcome,
                Err(error) => return interview_error(error),
            };
            let cursor = match append_event(
                &mut tx,
                &id,
                "interviewer.owner_turn",
                json!({"turn_id":outcome.turn_id,"job_id":outcome.job_id}),
            )
            .await
            {
                Ok(cursor) => cursor,
                Err(error) => return super::command_error(error),
            };
            (
                json!({"chat_id":id,"turn_id":outcome.turn_id,"job_id":outcome.job_id,"revision":outcome.revision}),
                cursor,
            )
        }
        Some("briefer") => {
            let start = crate::application::brief::OwnerBriefStart::ChatTurn {
                chat_id: id.clone(),
                text,
            };
            let outcome = match crate::application::brief::start_owner(
                &mut tx,
                state.workspace.epoch(),
                &start,
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(error) => return brief_error(error),
            };
            (
                json!({
                    "chat_id":outcome.chat_id,
                    "turn_id":outcome.turn_id,
                    "job_id":outcome.job_id,
                    "revision":outcome.revision,
                    "event_cursor":outcome.event_cursor,
                }),
                outcome.event_cursor.parse::<i64>().unwrap_or_default(),
            )
        }
        _ => {
            return super::error(
                StatusCode::NOT_FOUND,
                "CHAT_NOT_FOUND",
                "Chat does not exist",
            );
        }
    };
    if let Err(error) = write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        202,
        &result,
        cursor,
    )
    .await
    {
        return super::command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    state.jobs.notify_one();
    response_json(StatusCode::ACCEPTED, result, state.workspace.epoch())
}

fn interview_error(error: crate::application::interview::InterviewError) -> Response {
    match error {
        crate::application::interview::InterviewError::NotFound => super::error(
            StatusCode::NOT_FOUND,
            "CHAT_NOT_FOUND",
            "Owner Interviewer chat does not exist",
        ),
        crate::application::interview::InterviewError::StaleRevision => super::error(
            StatusCode::CONFLICT,
            "CHAT_STALE",
            "Chat changed; reload and retry",
        ),
        crate::application::interview::InterviewError::InvalidTurn => super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "TURN_INVALID",
            "Owner turn must contain 1–16,000 UTF-8 bytes",
        ),
        crate::application::interview::InterviewError::InvalidKind => super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "CHAT_ROLE_INVALID",
            "Turns require an owner Interviewer chat",
        ),
        crate::application::interview::InterviewError::BankMissing => super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "QUESTION_BANK_MISSING",
            "Interviewer question bank is not configured",
        ),
        crate::application::interview::InterviewError::QueueAtCapacity
        | crate::application::interview::InterviewError::InferenceBusy => {
            let mut response = super::error(
                StatusCode::TOO_MANY_REQUESTS,
                "INFERENCE_BUSY",
                "The local inference queue is full",
            );
            response.headers_mut().insert(
                "retry-after",
                axum::http::HeaderValue::from_static("2"),
            );
            response
        }
        crate::application::interview::InterviewError::Database => super::command_error(
            crate::WorkspaceError::Invalid("interview intake failed".into()),
        ),
    }
}

fn brief_error(error: crate::application::brief::BriefError) -> Response {
    match error {
        crate::application::brief::BriefError::NotFound | crate::application::brief::BriefError::TargetNotGranted => super::error(
            StatusCode::NOT_FOUND,
            "BRIEF_SCOPE_NOT_FOUND",
            "Briefer scope no longer exists",
        ),
        crate::application::brief::BriefError::StaleRevision => super::error(
            StatusCode::CONFLICT,
            "BRIEF_SCOPE_STALE",
            "Brief scope changed; reload and retry",
        ),
        crate::application::brief::BriefError::InvalidPurpose => super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "BRIEF_INPUT_INVALID",
            "Briefer input is invalid",
        ),
        crate::application::brief::BriefError::QueueAtCapacity => {
            let mut response = super::error(
                StatusCode::TOO_MANY_REQUESTS,
                "INFERENCE_BUSY",
                "The local inference queue is full",
            );
            response.headers_mut().insert(
                "retry-after",
                axum::http::HeaderValue::from_static("2"),
            );
            response
        }
        crate::application::brief::BriefError::Database => {
            super::command_error(crate::WorkspaceError::Invalid("brief intake failed".into()))
        }
    }
}

async fn question_action(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<QuestionAction>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if !matches!(
        request.action.as_str(),
        "ask" | "skip" | "defer" | "unknown"
    ) || request.question_id.trim().is_empty()
    {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "QUESTION_ACTION_INVALID",
            "Question action must be ask, skip, defer, or unknown",
        );
    }
    let key = match idempotency_key(&headers) {
        Ok(value) => value,
        Err(message) => {
            return super::error(StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED", message);
        }
    };
    let canonical = digest(
        format!(
            "POST\0/agent/chats/{id}/questions\0{}",
            serde_json::to_string(&request).unwrap_or_default()
        )
        .as_bytes(),
    );
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(response) =
        receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    let chat: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT asset_kind,role,revision FROM agent_chat WHERE id=? AND scope_kind='owner'",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await
    .ok()
    .flatten();
    let Some((kind, role, revision)) = chat else {
        return super::error(
            StatusCode::NOT_FOUND,
            "CHAT_NOT_FOUND",
            "Owner Interviewer chat does not exist",
        );
    };
    if role != "interviewer" {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "CHAT_ROLE_INVALID",
            "Question bank actions are only available in the Interviewer",
        );
    }
    let bank:Option<String>=sqlx::query_scalar("SELECT content_json FROM interviewer_question_bank_version WHERE version_id=(SELECT version_id FROM interviewer_question_bank_active WHERE singleton=1)").fetch_optional(&mut *tx).await.ok().flatten();
    let bank: Value = bank
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or(Value::Null);
    let question = bank["questions"][kind.as_str()]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == request.question_id))
        .or_else(|| {
            bank["questions"]["discovery"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["id"] == request.question_id))
        });
    let Some(question) = question else {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "QUESTION_UNKNOWN",
            "Question is not in the active Interviewer bank",
        );
    };
    let state_value = match request.action.as_str() {
        "ask" => "asked",
        "skip" => "skipped",
        "defer" => "deferred",
        _ => "unknown",
    };
    let now = now_utc();
    if let Err(error)=sqlx::query("INSERT INTO interviewer_question_state(chat_id,question_id,state,updated_at) VALUES(?,?,?,?) ON CONFLICT(chat_id,question_id) DO UPDATE SET state=excluded.state,updated_at=excluded.updated_at").bind(&id).bind(&request.question_id).bind(state_value).bind(&now).execute(&mut *tx).await{return super::command_error(error.into());}
    let mut result = json!({"chat_id":id,"question_id":request.question_id,"state":state_value,"revision":revision+1});
    if request.action == "ask" {
        let text = question["text"].as_str().unwrap_or_default();
        let turn_no: i64 = match sqlx::query_scalar(
            "SELECT coalesce(max(turn_no),0)+1 FROM agent_turn WHERE chat_id=?",
        )
        .bind(&id)
        .fetch_one(&mut *tx)
        .await
        {
            Ok(value) => value,
            Err(error) => return super::command_error(error.into()),
        };
        let turn_id = new_id(&format!("{id}:{turn_no}"));
        if let Err(error)=sqlx::query("INSERT INTO agent_turn(id,chat_id,turn_no,speaker,text,created_at) VALUES(?,?,?,'agent',?,?)").bind(&turn_id).bind(&id).bind(turn_no).bind(text).bind(&now).execute(&mut *tx).await{return super::command_error(error.into());}
        result["turn_id"] = json!(turn_id);
        result["text"] = json!(text);
    }
    if let Err(error) = sqlx::query(
        "UPDATE agent_chat SET revision=revision+1,updated_at=? WHERE id=? AND revision=?",
    )
    .bind(&now)
    .bind(&id)
    .bind(revision)
    .execute(&mut *tx)
    .await
    {
        return super::command_error(error.into());
    }
    let cursor = match append_event(
        &mut tx,
        &id,
        "interviewer.question_disposition",
        result.clone(),
    )
    .await
    {
        Ok(cursor) => cursor,
        Err(error) => return super::command_error(error),
    };
    if let Err(error) = write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        200,
        &result,
        cursor,
    )
    .await
    {
        return super::command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    response_json(StatusCode::OK, result, state.workspace.epoch())
}

async fn confirm_candidate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CandidateConfirmation>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if request.title.trim().is_empty()
        || request.title.chars().count() > 240
        || !request.fields.is_object()
    {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "CANDIDATE_INVALID",
            "Candidate title and fields are invalid",
        );
    }
    let key = match idempotency_key(&headers) {
        Ok(value) => value,
        Err(message) => {
            return super::error(StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED", message);
        }
    };
    let canonical = digest(
        format!(
            "POST\0/agent/candidates/{id}/confirm\0{}",
            serde_json::to_string(&request).unwrap_or_default()
        )
        .as_bytes(),
    );
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(response) =
        receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    let row:Option<(String,String,String,i64,String,String,String)>=sqlx::query_as("SELECT c.chat_id,c.asset_kind,c.owner_quotes_json,ch.revision,c.state,c.missing_fields_json,c.fields_json FROM interviewer_candidate c JOIN agent_chat ch ON ch.id=c.chat_id WHERE c.id=?").bind(&id).fetch_optional(&mut *tx).await.ok().flatten();
    let Some((chat_id, kind, quotes_json, chat_revision, candidate_state, _, _candidate_fields)) =
        row
    else {
        return super::error(
            StatusCode::NOT_FOUND,
            "CANDIDATE_NOT_FOUND",
            "Interviewer candidate does not exist",
        );
    };
    if candidate_state != "proposed" || chat_revision != request.expected_chat_revision {
        return super::error(
            StatusCode::CONFLICT,
            "CANDIDATE_STALE",
            "Candidate changed; reload and review the latest version",
        );
    }
    let Some(allowed) = allowed_fields(&kind) else {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "ASSET_KIND_INVALID",
            "Asset type is unsupported",
        );
    };
    let Some(fields) = request.fields.as_object() else {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "CANDIDATE_INVALID",
            "Candidate fields must be an object",
        );
    };
    if fields.keys().any(|key| !allowed.contains(key.as_str())) {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "CANDIDATE_FIELDS_INVALID",
            "Candidate contains fields that are not part of its asset schema",
        );
    }
    let owner_quotes: Value = serde_json::from_str(&quotes_json).unwrap_or(Value::Null);
    if let Some(quotes) = owner_quotes.as_object() {
        for (field, provenance) in quotes {
            if !allowed.contains(field.as_str()) {
                return super::error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "OWNER_QUOTE_INVALID",
                    "Candidate evidence references an unknown resource field",
                );
            }
            let Some(turn_id) = provenance.get("turn_id").and_then(Value::as_str) else {
                return super::error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "OWNER_QUOTE_INVALID",
                    "Candidate evidence has no owner turn reference",
                );
            };
            let Some(quote) = provenance.get("quote").and_then(Value::as_str) else {
                return super::error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "OWNER_QUOTE_INVALID",
                    "Candidate evidence has no quote",
                );
            };
            let text: Option<String> = sqlx::query_scalar(
                "SELECT text FROM agent_turn WHERE id=? AND chat_id=? AND speaker='owner'",
            )
            .bind(turn_id)
            .bind(&chat_id)
            .fetch_optional(&mut *tx)
            .await
            .unwrap_or(None);
            if text.as_deref().is_none_or(|source| {
                parley_domain::evidence::verify_quote(source, quote, None).is_err()
            }) {
                return super::error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "OWNER_QUOTE_INVALID",
                    "Candidate quote is not present in its owner turn",
                );
            }
        }
    }
    let missing = missing_fields(&kind, &request.fields);
    let readiness = if missing.is_empty() { "ready" } else { "draft" };
    let resource_id = new_id(&format!("candidate:{id}"));
    let now = now_utc();
    let body = request.fields.to_string();
    let subtype_locked = if readiness == "ready" { 1 } else { 0 };
    if let Err(error)=sqlx::query("INSERT INTO entity(id,kind,name,focus,notes_md,notes_rev,revision,created_at,updated_at,subtype,body_json,readiness,resource_content_revision,subtype_locked) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&resource_id).bind("resource").bind(request.title.trim()).bind("").bind("").bind(1_i64).bind(1_i64).bind(&now).bind(&now).bind(&kind).bind(&body).bind(readiness).bind(1_i64).bind(subtype_locked).execute(&mut *tx).await{return super::command_error(error.into());}
    if let Some(column_id) = &request.column_id
        && let Err(error) = state
            .workspace
            .place_confirmed_resource(&mut tx, &resource_id, column_id)
            .await
    {
        return super::command_error(error);
    }
    if let Err(error)=sqlx::query("UPDATE interviewer_candidate SET fields_json=?,missing_fields_json=?,state='saved',resource_id=?,updated_at=? WHERE id=? AND state='proposed'")
        .bind(&body).bind(serde_json::to_string(&missing).unwrap_or_default()).bind(&resource_id).bind(&now).bind(&id).execute(&mut *tx).await{return super::command_error(error.into());}
    let coach_question: Option<String> = match sqlx::query_scalar(
        "SELECT json_extract(j.input_json,'$.coach_question_id')
         FROM interviewer_candidate c JOIN durable_job j ON j.id=c.job_id WHERE c.id=?",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(value) => value.flatten(),
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(question_id) = coach_question {
        let linked = sqlx::query(
            "UPDATE coach_question SET status='answered',disposition_json=json_set(disposition_json,'$.resource_id',?),
             revision=revision+1,updated_at=? WHERE id=? AND status='answer_pending'
             AND json_extract(disposition_json,'$.job_id')=(SELECT job_id FROM interviewer_candidate WHERE id=?)",
        ).bind(&resource_id).bind(&now).bind(&question_id).bind(&id).execute(&mut *tx).await;
        match linked {
            Ok(result) if result.rows_affected() == 1 => {
                if let Err(error) = append_event(
                    &mut tx,
                    &question_id,
                    "coach.question_answer_confirmed",
                    json!({"candidate_id":id,"resource_id":resource_id}),
                )
                .await
                {
                    return super::materials::workspace_error(error);
                }
            }
            Ok(_) => {}
            Err(error) => return super::command_error(error.into()),
        }
    }
    sqlx::query("UPDATE agent_chat SET revision=revision+1,updated_at=? WHERE id=? AND revision=?")
        .bind(&now)
        .bind(&chat_id)
        .bind(chat_revision)
        .execute(&mut *tx)
        .await
        .ok();
    let result = json!({"id":resource_id,"kind":kind,"title":request.title.trim(),"body":request.fields,"readiness":readiness,"missing_fields":missing,"provenance":quotes_json,"column_id":request.column_id});
    if let Err(error) =
        sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await
    {
        return super::command_error(error.into());
    }
    let cursor = match append_event(
        &mut tx,
        &resource_id,
        "interviewer.resource_confirmed",
        json!({"candidate_id":id,"readiness":readiness}),
    )
    .await
    {
        Ok(cursor) => cursor,
        Err(error) => return super::command_error(error),
    };
    if let Err(error) = write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        201,
        &result,
        cursor,
    )
    .await
    {
        return super::command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    response_json(StatusCode::CREATED, result, state.workspace.epoch())
}

async fn dismiss_candidate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(_input): Json<Empty>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let key = match idempotency_key(&headers) {
        Ok(value) => value,
        Err(message) => {
            return super::error(StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED", message);
        }
    };
    let canonical = digest(format!("POST\0/agent/candidates/{id}/dismiss\0").as_bytes());
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(response) =
        receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    let now = now_utc();
    let updated = sqlx::query(
        "UPDATE interviewer_candidate SET state='dismissed',updated_at=? WHERE id=? AND state='proposed'",
    )
    .bind(&now)
    .bind(&id)
    .execute(&mut *tx)
    .await;
    let updated = match updated {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return super::command_error(error.into());
        }
    };
    if updated.rows_affected() != 1 {
        let _ = tx.rollback().await;
        return super::error(
            StatusCode::NOT_FOUND,
            "CANDIDATE_NOT_FOUND",
            "Proposed candidate does not exist",
        );
    }
    let result = json!({"id":id,"state":"dismissed"});
    let cursor =
        match append_event(&mut tx, &id, "interviewer.candidate_dismissed", json!({})).await {
            Ok(cursor) => cursor,
            Err(error) => {
                let _ = tx.rollback().await;
                return super::command_error(error);
            }
        };
    if let Err(error) = write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        200,
        &result,
        cursor,
    )
    .await
    {
        let _ = tx.rollback().await;
        return super::command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    response_json(StatusCode::OK, result, state.workspace.epoch())
}

async fn get_question_bank(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let row:Option<(String,String)>=sqlx::query_as("SELECT b.version_id,b.content_json FROM interviewer_question_bank_active a JOIN interviewer_question_bank_version b ON b.version_id=a.version_id WHERE a.singleton=1").fetch_optional(state.workspace.pool()).await.ok().flatten();
    match row {
        Some((version_id, content)) => response_json(
            StatusCode::OK,
            json!({"version_id":version_id,"content":serde_json::from_str::<Value>(&content).unwrap_or(Value::Null)}),
            state.workspace.epoch(),
        ),
        None => super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "QUESTION_BANK_MISSING",
            "Interviewer question bank is not configured",
        ),
    }
}

async fn update_question_bank(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<QuestionBankUpdate>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if !valid_question_bank(&request.content) {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "QUESTION_BANK_INVALID",
            "Question bank needs non-empty unique questions for proof, gift, and artifact",
        );
    }
    let key = match idempotency_key(&headers) {
        Ok(value) => value,
        Err(message) => {
            return super::error(StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED", message);
        }
    };
    let content = serde_json::to_string(&request.content).unwrap_or_default();
    let canonical = digest(
        format!(
            "PUT\0/agent/question-bank\0{}\0{content}",
            request.expected_version_id
        )
        .as_bytes(),
    );
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(response) =
        receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    let active: Option<String> = sqlx::query_scalar(
        "SELECT version_id FROM interviewer_question_bank_active WHERE singleton=1",
    )
    .fetch_optional(&mut *tx)
    .await
    .ok()
    .flatten();
    if active.as_deref() != Some(&request.expected_version_id) {
        return super::error(
            StatusCode::CONFLICT,
            "QUESTION_BANK_STALE",
            "Question bank changed; reload before editing",
        );
    }
    let next: i64 = sqlx::query_scalar(
        "SELECT coalesce(max(version_no),0)+1 FROM interviewer_question_bank_version",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap_or(1);
    let version_id = new_id(&format!("question-bank:{next}:{content}"));
    let hash = digest(content.as_bytes());
    let now = now_utc();
    if let Err(error)=sqlx::query("INSERT INTO interviewer_question_bank_version(version_id,version_no,content_json,content_sha256,created_by,created_at) VALUES(?,?,?,?, 'owner',?)").bind(&version_id).bind(next).bind(&content).bind(hash).bind(&now).execute(&mut *tx).await{return super::command_error(error.into());}
    if let Err(error)=sqlx::query("UPDATE interviewer_question_bank_active SET version_id=? WHERE singleton=1 AND version_id=?").bind(&version_id).bind(&request.expected_version_id).execute(&mut *tx).await{return super::command_error(error.into());}
    let result = json!({"version_id":version_id,"content":request.content});
    let cursor = match append_event(
        &mut tx,
        "interviewer",
        "interviewer.question_bank_activated",
        json!({"version_id":version_id,"version_no":next}),
    )
    .await
    {
        Ok(cursor) => cursor,
        Err(error) => return super::command_error(error),
    };
    if let Err(error) = write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        200,
        &result,
        cursor,
    )
    .await
    {
        return super::command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    response_json(StatusCode::OK, result, state.workspace.epoch())
}

fn valid_question_bank(value: &Value) -> bool {
    let Some(questions) = value.get("questions").and_then(Value::as_object) else {
        return false;
    };
    ["proof", "gift", "artifact", "discovery"]
        .iter()
        .all(|kind| {
            let Some(items) = questions.get(*kind).and_then(Value::as_array) else {
                return false;
            };
            let mut ids = BTreeSet::new();
            !items.is_empty()
                && items.iter().all(|item| {
                    let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                    let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                    !id.trim().is_empty()
                        && id.len() <= 80
                        && !text.trim().is_empty()
                        && text.len() <= 600
                        && ids.insert(id.to_owned())
                })
        })
}

async fn get_prompt(
    State(state): State<AppState>,
    Path(role): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if !matches!(role.as_str(), "interviewer" | "briefer") {
        return super::error(
            StatusCode::NOT_FOUND,
            "ROLE_NOT_FOUND",
            "No editable prompt exists for this role",
        );
    }
    let row:Option<(String,i64,String,String)>=sqlx::query_as("SELECT p.version_id,p.version_no,p.prompt_text,p.prompt_sha256 FROM agent_prompt_active a JOIN agent_prompt_version p ON p.version_id=a.version_id WHERE a.role=?").bind(&role).fetch_optional(state.workspace.pool()).await.ok().flatten();
    match row {
        Some((version_id, version, content, sha256)) => response_json(
            StatusCode::OK,
            json!({"role":role,"version_id":version_id,"version":version,"content":content,"sha256":sha256}),
            state.workspace.epoch(),
        ),
        None => super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "PROMPT_MISSING",
            "No active role prompt is configured",
        ),
    }
}

async fn update_prompt(
    State(state): State<AppState>,
    Path(role): Path<String>,
    headers: HeaderMap,
    Json(request): Json<PromptUpdate>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if !matches!(role.as_str(), "interviewer" | "briefer")
        || request.content.trim().is_empty()
        || request.content.len() > MAX_PROMPT_BYTES
    {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "PROMPT_INVALID",
            "Prompt role or content is invalid",
        );
    }
    let key = match idempotency_key(&headers) {
        Ok(value) => value,
        Err(message) => {
            return super::error(StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED", message);
        }
    };
    let canonical = digest(
        format!(
            "PUT\0/agent/prompts/{role}\0{}\0{}",
            request.expected_version_id, request.content
        )
        .as_bytes(),
    );
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(response) =
        receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    let active: Option<String> =
        sqlx::query_scalar("SELECT version_id FROM agent_prompt_active WHERE role=?")
            .bind(&role)
            .fetch_optional(&mut *tx)
            .await
            .ok()
            .flatten();
    if active.as_deref() != Some(&request.expected_version_id) {
        return super::error(
            StatusCode::CONFLICT,
            "PROMPT_STALE",
            "Prompt changed; reload before editing",
        );
    }
    let next: i64 = sqlx::query_scalar(
        "SELECT coalesce(max(version_no),0)+1 FROM agent_prompt_version WHERE role=?",
    )
    .bind(&role)
    .fetch_one(&mut *tx)
    .await
    .unwrap_or(1);
    let version_id = new_id(&format!("prompt:{role}:{next}:{}", request.content));
    let hash = digest(request.content.as_bytes());
    let now = now_utc();
    if let Err(error)=sqlx::query("INSERT INTO agent_prompt_version(role,version_no,version_id,prompt_text,prompt_sha256,created_by,created_at) VALUES(?,?,?,?,?,'owner',?)").bind(&role).bind(next).bind(&version_id).bind(&request.content).bind(&hash).bind(&now).execute(&mut *tx).await{return super::command_error(error.into());}
    if let Err(error) =
        sqlx::query("UPDATE agent_prompt_active SET version_id=? WHERE role=? AND version_id=?")
            .bind(&version_id)
            .bind(&role)
            .bind(&request.expected_version_id)
            .execute(&mut *tx)
            .await
    {
        return super::command_error(error.into());
    }
    let result = json!({"role":role,"version_id":version_id,"version":next,"sha256":hash});
    let cursor = match append_event(
        &mut tx,
        "agent_prompt",
        "agent_prompt.activated",
        result.clone(),
    )
    .await
    {
        Ok(cursor) => cursor,
        Err(error) => return super::command_error(error),
    };
    if let Err(error) = write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        200,
        &result,
        cursor,
    )
    .await
    {
        return super::command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    response_json(StatusCode::OK, result, state.workspace.epoch())
}

async fn list_segments(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let rows: Result<Vec<SegmentRow>, _> = sqlx::query_as(
        "SELECT id,name,description,revision FROM segment ORDER BY name COLLATE NOCASE,id",
    )
    .fetch_all(state.workspace.pool())
    .await;
    match rows{Ok(rows)=>response_json(StatusCode::OK,json!(rows.into_iter().map(|(id,name,description,revision)|json!({"id":id,"name":name,"description":description,"revision":revision})).collect::<Vec<_>>()),state.workspace.epoch()),Err(error)=>super::command_error(error.into())}
}

async fn create_segment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SegmentCreate>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let name = request.name.trim();
    if name.is_empty()
        || name.chars().count() > 200
        || request
            .description
            .as_ref()
            .is_some_and(|value| value.len() > 4000)
    {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "SEGMENT_INVALID",
            "Segment needs a name of 1–200 characters and a bounded description",
        );
    }
    let key = match idempotency_key(&headers) {
        Ok(value) => value,
        Err(message) => {
            return super::error(StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED", message);
        }
    };
    let canonical = digest(
        format!(
            "POST\0/segments\0{name}\0{}",
            request.description.as_deref().unwrap_or_default()
        )
        .as_bytes(),
    );
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(response) =
        receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    let id = new_id(&canonical);
    let now = now_utc();
    if let Err(error) = sqlx::query(
        "INSERT INTO segment(id,name,description,created_at,updated_at) VALUES(?,?,?,?,?)",
    )
    .bind(&id)
    .bind(name)
    .bind(&request.description)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await
    {
        return super::command_error(error.into());
    }
    let result = json!({"id":id,"name":name,"description":request.description,"revision":1});
    let cursor = match append_event(&mut tx, &id, "segment.created", json!({"name":name})).await {
        Ok(cursor) => cursor,
        Err(error) => return super::command_error(error),
    };
    if let Err(error) = write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        201,
        &result,
        cursor,
    )
    .await
    {
        return super::command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    response_json(StatusCode::CREATED, result, state.workspace.epoch())
}

async fn update_segment_party(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SegmentParty>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if !matches!(request.action.as_str(), "add" | "remove") {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "SEGMENT_ACTION_INVALID",
            "Membership action must be add or remove",
        );
    }
    let key = match idempotency_key(&headers) {
        Ok(value) => value,
        Err(message) => {
            return super::error(StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED", message);
        }
    };
    let canonical = digest(
        format!(
            "POST\0/segments/{id}/parties\0{}",
            serde_json::to_string(&request).unwrap_or_default()
        )
        .as_bytes(),
    );
    let now = now_utc();
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(response) =
        receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    let segment: Option<String> = sqlx::query_scalar("SELECT id FROM segment WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
    if segment.is_none() {
        return super::error(
            StatusCode::NOT_FOUND,
            "SEGMENT_NOT_FOUND",
            "Segment does not exist",
        );
    }
    let party: Option<String> =
        sqlx::query_scalar("SELECT id FROM entity WHERE id=? AND kind IN ('person','org')")
            .bind(&request.party_id)
            .fetch_optional(&mut *tx)
            .await
            .ok()
            .flatten();
    if party.is_none() {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "PARTY_INVALID",
            "Segment members must be people or organizations",
        );
    }
    let result = if request.action == "add" {
        sqlx::query(
            "INSERT OR IGNORE INTO segment_party(segment_id,party_id,created_at) VALUES(?,?,?)",
        )
        .bind(&id)
        .bind(&request.party_id)
        .bind(&now)
        .execute(&mut *tx)
        .await
    } else {
        sqlx::query("DELETE FROM segment_party WHERE segment_id=? AND party_id=?")
            .bind(&id)
            .bind(&request.party_id)
            .execute(&mut *tx)
            .await
    };
    if let Err(error) = result {
        return super::command_error(error.into());
    }
    if let Err(error) =
        sqlx::query("UPDATE segment SET revision=revision+1,updated_at=? WHERE id=?")
            .bind(&now)
            .bind(&id)
            .execute(&mut *tx)
            .await
    {
        return super::command_error(error.into());
    }
    if let Err(error) = sqlx::query("UPDATE entity SET revision=revision+1,updated_at=? WHERE id=?")
        .bind(&now)
        .bind(&request.party_id)
        .execute(&mut *tx)
        .await
    {
        return super::command_error(error.into());
    }
    if let Err(error) =
        sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await
    {
        return super::command_error(error.into());
    }
    let event = append_event(
        &mut tx,
        &id,
        "segment.party_membership_changed",
        json!({"party_id":request.party_id,"action":request.action}),
    )
    .await;
    let cursor = match event {
        Ok(cursor) => cursor,
        Err(error) => return super::command_error(error),
    };
    let result = json!({"segment_id":id,"party_id":request.party_id,"action":request.action});
    if let Err(error) = write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        200,
        &result,
        cursor,
    )
    .await
    {
        return super::command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    response_json(StatusCode::OK, result, state.workspace.epoch())
}

async fn list_brief_items(
    State(state): State<AppState>,
    Path((scope_kind, scope_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if !matches!(scope_kind.as_str(), "party" | "segment") {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "BRIEF_SCOPE_INVALID",
            "Brief scope must be party or segment",
        );
    }
    match fetch_brief_items(state.workspace.pool(), &scope_kind, &scope_id).await {
        Ok(items) => response_json(StatusCode::OK, json!(items), state.workspace.epoch()),
        Err(error) => super::command_error(error),
    }
}

async fn fetch_brief_items(
    pool: &SqlitePool,
    scope_kind: &str,
    scope_id: &str,
) -> Result<Vec<BriefItem>, WorkspaceError> {
    let rows: Vec<BriefItemRow> = sqlx::query_as("SELECT id,scope_kind,scope_id,kind,text,detail_json,tier,quote,check_suggestion,speaker_kind,status,revision,supersedes_id FROM brief_item WHERE scope_kind=? AND scope_id=? ORDER BY kind,id")
        .bind(scope_kind).bind(scope_id).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|row| BriefItem {
            id: row.0,
            scope_kind: row.1,
            scope_id: row.2,
            kind: row.3,
            text: row.4,
            detail: serde_json::from_str(&row.5).unwrap_or(Value::Null),
            tier: row.6,
            quote: row.7,
            check_suggestion: row.8,
            speaker_kind: row.9,
            status: row.10,
            revision: row.11,
            supersedes_id: row.12,
        })
        .collect())
}

async fn generate_brief(
    State(state): State<AppState>,
    Path((scope_kind, scope_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(_request): Json<Empty>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if !matches!(scope_kind.as_str(), "party" | "segment") {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "BRIEF_SCOPE_INVALID",
            "Brief scope must be party or segment",
        );
    }
    let key = match idempotency_key(&headers) {
        Ok(value) => value,
        Err(message) => {
            return super::error(StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED", message);
        }
    };
    let canonical =
        digest(format!("POST\0/briefs/{scope_kind}/{scope_id}/items\0generate").as_bytes());
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(response) =
        receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    let outcome = match crate::application::brief::start_owner(
        &mut tx,
        state.workspace.epoch(),
        &crate::application::brief::OwnerBriefStart::Generate {
            scope_kind: scope_kind.clone(),
            scope_id: scope_id.clone(),
        },
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => return brief_error(error),
    };
    let result = json!({
        "job_id":outcome.job_id,
        "scope_kind":outcome.scope_kind,
        "scope_id":outcome.scope_id,
        "state":outcome.state
    });
    let cursor = outcome.event_cursor.parse::<i64>().unwrap_or_default();
    if let Err(error) = write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        202,
        &result,
        cursor,
    )
    .await
    {
        return super::command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    state.jobs.notify_one();
    response_json(StatusCode::ACCEPTED, result, state.workspace.epoch())
}

async fn decide_brief_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<BriefDecision>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if !matches!(request.decision.as_str(), "confirm" | "correct" | "dismiss")
        || (request.decision == "correct"
            && request
                .corrected_text
                .as_deref()
                .is_none_or(|text| text.trim().is_empty() || text.chars().count() > 1000))
    {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "BRIEF_DECISION_INVALID",
            "Decision or corrected wording is invalid",
        );
    }
    let key = match idempotency_key(&headers) {
        Ok(value) => value,
        Err(message) => {
            return super::error(StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED", message);
        }
    };
    let canonical = digest(
        format!(
            "POST\0/brief-items/{id}/decision\0{}",
            serde_json::to_string(&request).unwrap_or_default()
        )
        .as_bytes(),
    );
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(response) =
        receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    let row: Option<BriefDecisionRow> = sqlx::query_as("SELECT scope_kind,scope_id,kind,text,detail_json,tier,source_occurrence_id,source_variant_id,quote,check_suggestion FROM brief_item WHERE id=? AND revision=? AND status='proposed'").bind(&id).bind(request.expected_revision).fetch_optional(&mut *tx).await.ok().flatten();
    let Some((
        scope_kind,
        scope_id,
        kind,
        _original,
        detail,
        old_tier,
        occurrence,
        variant,
        quote,
        suggestion,
    )) = row
    else {
        return super::error(
            StatusCode::CONFLICT,
            "BRIEF_ITEM_STALE",
            "Brief item changed; reload the brief before deciding",
        );
    };
    let now = now_utc();
    let mut result = json!({"id":id,"decision":request.decision});
    match request.decision.as_str() {
        "confirm" => {
            let update=sqlx::query("UPDATE brief_item SET tier=3,speaker_kind='owner',confirmed_by_owner=1,status='confirmed',revision=revision+1,updated_at=? WHERE id=? AND revision=? AND status='proposed'").bind(&now).bind(&id).bind(request.expected_revision).execute(&mut *tx).await;
            if let Err(error) = update {
                return super::command_error(error.into());
            }
            result["revision"] = json!(request.expected_revision + 1);
        }
        "dismiss" => {
            let update=sqlx::query("UPDATE brief_item SET status='dismissed',revision=revision+1,updated_at=? WHERE id=? AND revision=? AND status='proposed'").bind(&now).bind(&id).bind(request.expected_revision).execute(&mut *tx).await;
            if let Err(error) = update {
                return super::command_error(error.into());
            }
            result["revision"] = json!(request.expected_revision + 1);
        }
        "correct" => {
            let new_text = request.corrected_text.as_deref().unwrap_or_default().trim();
            let update=sqlx::query("UPDATE brief_item SET status='corrected',revision=revision+1,updated_at=? WHERE id=? AND revision=? AND status='proposed'").bind(&now).bind(&id).bind(request.expected_revision).execute(&mut *tx).await;
            if let Err(error) = update {
                return super::command_error(error.into());
            }
            let replacement_id = new_id(&format!("{id}:correction:{}", new_text));
            if let Err(error)=sqlx::query("INSERT INTO brief_item(id,scope_kind,scope_id,kind,text,detail_json,tier,source_occurrence_id,source_variant_id,quote,source_start,source_end,check_suggestion,speaker_kind,confirmed_by_owner,status,revision,supersedes_id,created_at,updated_at) VALUES(?,?,?,?,?,?,3,?,?,?,NULL,NULL,NULL,'owner',1,'corrected',1,?,?,?)")
                .bind(&replacement_id).bind(&scope_kind).bind(&scope_id).bind(&kind).bind(new_text).bind(&detail).bind(&occurrence).bind(&variant).bind(&quote).bind(&id).bind(&now).bind(&now).execute(&mut *tx).await{return super::command_error(error.into());}
            result["replacement_id"] = json!(replacement_id);
            result["original_tier"] = json!(old_tier);
            result["check_suggestion"] = json!(suggestion);
        }
        _ => unreachable!(),
    }
    if scope_kind == "party" {
        if let Err(error) =
            sqlx::query("UPDATE entity SET revision=revision+1,updated_at=? WHERE id=?")
                .bind(&now)
                .bind(&scope_id)
                .execute(&mut *tx)
                .await
        {
            return super::command_error(error.into());
        }
    } else {
        if let Err(error) =
            sqlx::query("UPDATE segment SET revision=revision+1,updated_at=? WHERE id=?")
                .bind(&now)
                .bind(&scope_id)
                .execute(&mut *tx)
                .await
        {
            return super::command_error(error.into());
        }
        if let Err(error)=sqlx::query("UPDATE entity SET revision=revision+1,updated_at=? WHERE id IN (SELECT party_id FROM segment_party WHERE segment_id=?)")
            .bind(&now).bind(&scope_id).execute(&mut *tx).await{return super::command_error(error.into());}
    }
    if let Err(error) =
        sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await
    {
        return super::command_error(error.into());
    }
    let cursor = match append_event(
        &mut tx,
        &id,
        "brief.item_decided",
        json!({"decision":request.decision,"replacement_text":request.corrected_text}),
    )
    .await
    {
        Ok(cursor) => cursor,
        Err(error) => return super::command_error(error),
    };
    if let Err(error) = write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        200,
        &result,
        cursor,
    )
    .await
    {
        return super::command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    response_json(StatusCode::OK, result, state.workspace.epoch())
}

async fn list_fluency(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let rows: Result<Vec<FluencyRecord>, _> = sqlx::query_as(
        "SELECT term,status,revision FROM fluency ORDER BY term COLLATE NOCASE,term",
    )
    .fetch_all(state.workspace.pool())
    .await;
    match rows {
        Ok(rows) => response_json(StatusCode::OK, json!(rows), state.workspace.epoch()),
        Err(error) => super::command_error(error.into()),
    }
}

async fn update_fluency(
    State(state): State<AppState>,
    Path(term): Path<String>,
    headers: HeaderMap,
    Json(request): Json<FluencyUpdate>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let term = term.trim();
    if term.is_empty()
        || term.chars().count() > 120
        || !matches!(request.status.as_str(), "unknown" | "learning" | "fluent")
        || request.expected_revision < 0
    {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "FLUENCY_INVALID",
            "Term, status, or expected revision is invalid",
        );
    }
    let key = match idempotency_key(&headers) {
        Ok(value) => value,
        Err(message) => {
            return super::error(StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED", message);
        }
    };
    let canonical = digest(
        format!(
            "PUT\0/fluency/{term}\0{}\0{}",
            request.expected_revision, request.status
        )
        .as_bytes(),
    );
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(response) =
        receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    let current: Option<i64> = match sqlx::query_scalar("SELECT revision FROM fluency WHERE term=?")
        .bind(term)
        .fetch_optional(&mut *tx)
        .await
    {
        Ok(value) => value,
        Err(error) => return super::command_error(error.into()),
    };
    let now = now_utc();
    let next = request.expected_revision + 1;
    if current != Some(request.expected_revision) {
        if current.is_some() || request.expected_revision != 0 {
            return super::error(
                StatusCode::CONFLICT,
                "FLUENCY_STALE",
                "Vocabulary changed; reload before updating its fluency",
            );
        }
        if let Err(error) =
            sqlx::query("INSERT INTO fluency(term,status,revision,updated_at) VALUES(?,?,1,?)")
                .bind(term)
                .bind(&request.status)
                .bind(&now)
                .execute(&mut *tx)
                .await
        {
            return super::command_error(error.into());
        }
    } else if let Err(error) = sqlx::query(
        "UPDATE fluency SET status=?,revision=?,updated_at=? WHERE term=? AND revision=?",
    )
    .bind(&request.status)
    .bind(next)
    .bind(&now)
    .bind(term)
    .bind(request.expected_revision)
    .execute(&mut *tx)
    .await
    {
        return super::command_error(error.into());
    }
    let revision = if request.expected_revision == 0 {
        1
    } else {
        next
    };
    let result = json!({"term":term,"status":request.status,"revision":revision});
    let cursor = match append_event(&mut tx, term, "fluency.updated", result.clone()).await {
        Ok(cursor) => cursor,
        Err(error) => return super::command_error(error),
    };
    if let Err(error) = write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        200,
        &result,
        cursor,
    )
    .await
    {
        return super::command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    response_json(StatusCode::OK, result, state.workspace.epoch())
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, &'static str> {
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
    else {
        return Err("Idempotency-Key is required");
    };
    if key.is_empty() || key.len() > 100 {
        return Err("Idempotency-Key is invalid");
    }
    Ok(key.to_owned())
}

pub(super) async fn receipt_response(
    tx: &mut Transaction<'_, Sqlite>,
    epoch: &str,
    key: &str,
    canonical: &str,
) -> Option<Response> {
    let row: Option<(String, i64, String)> = sqlx::query_as(
        "SELECT canonical_digest,status_code,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
    )
    .bind(epoch)
    .bind(key)
    .fetch_optional(&mut **tx)
    .await
    .ok()?;
    let (stored, status, result) = row?;
    if stored != canonical {
        return Some(super::error(
            StatusCode::CONFLICT,
            "KEY_REUSED",
            "Idempotency key was used for a different command",
        ));
    }
    let value = serde_json::from_str(&result).unwrap_or(Value::Null);
    Some(response_json(
        StatusCode::from_u16(status as u16).unwrap_or(StatusCode::OK),
        value,
        epoch,
    ))
}

pub(super) async fn write_receipt(
    tx: &mut Transaction<'_, Sqlite>,
    epoch: &str,
    key: &str,
    canonical: &str,
    status: u16,
    result: &Value,
    cursor: i64,
) -> Result<(), WorkspaceError> {
    sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,?,?,?,?)")
        .bind(epoch).bind(key).bind(canonical).bind(status as i64).bind(result.to_string()).bind(cursor).bind(now_utc())
        .execute(&mut **tx).await?;
    Ok(())
}

pub(super) async fn append_event(
    tx: &mut Transaction<'_, Sqlite>,
    entity_id: &str,
    event_type: &str,
    payload: Value,
) -> Result<i64, WorkspaceError> {
    let cursor: i64 = sqlx::query_scalar("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'assistant',?,1,?,?) RETURNING cursor")
        .bind(new_id(event_type)).bind(now_utc()).bind(entity_id).bind(event_type).bind(payload.to_string())
        .fetch_one(&mut **tx).await?;
    Ok(cursor)
}

fn response_json(status: StatusCode, value: Value, epoch: &str) -> Response {
    let mut response = (status, Json(value)).into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(epoch) {
        response.headers_mut().insert("x-workspace-epoch", value);
    }
    response
}
