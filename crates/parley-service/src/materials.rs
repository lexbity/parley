//! Opportunity-scoped manual authoring and recipient-facing release.
//!
//! The public handlers do no policy work themselves: they authorize the local
//! owner, then execute the corresponding transaction in the workspace.

use super::{AppState, Workspace, WorkspaceError, authenticate, digest, new_id, now_utc};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use parley_domain::{gate, template};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/opportunities",
            get(list_opportunities).post(create_opportunity),
        )
        .route("/api/v1/opportunities/{id}", get(get_opportunity))
        .route(
            "/api/v1/opportunities/{id}/participants",
            post(add_participant),
        )
        .route(
            "/api/v1/opportunities/{id}/sources/{source_id}",
            post(link_source).delete(unlink_source),
        )
        .route("/api/v1/opportunities/{id}/interests", post(add_interest))
        .route("/api/v1/opportunities/{id}/exchange", post(save_exchange))
        .route(
            "/api/v1/opportunities/{id}/exchange/{recipient_id}/attest",
            post(attest_exchange),
        )
        .route(
            "/api/v1/templates",
            post(create_template).get(list_templates),
        )
        .route("/api/v1/templates/{id}/approve", post(approve_template))
        .route("/api/v1/templates/{id}/activate", post(activate_template))
        .route(
            "/api/v1/opportunities/{id}/materials",
            post(create_material).get(list_materials),
        )
        .route("/api/v1/materials/{id}", get(get_material))
        .route("/api/v1/materials/{id}/versions", post(edit_material))
        .route("/api/v1/materials/{id}/approve", post(approve_material))
        .route("/api/v1/materials/{id}/release", post(release_material))
        .route("/api/v1/releases/{id}/ack", post(ack_release))
        .route("/api/v1/outputs", get(list_outputs))
        .route("/api/v1/resources/{id}/ready", post(mark_resource_ready))
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct Opportunity {
    pub id: String,
    pub title: String,
    pub state: String,
    pub revision: i64,
    pub participant_ids: Vec<String>,
    pub source_variant_ids: Vec<String>,
    pub material_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpportunityCreate {
    title: String,
    primary_party_id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ParticipantInput {
    party_id: String,
    role: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct InterestInput {
    recipient_id: String,
    side: String,
    statement: String,
    knowledge_kind: String,
    check_suggestion: Option<String>,
    claim_id: Option<String>,
    claim_revision: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ExchangeInput {
    recipient_id: String,
    they_get: String,
    i_get: String,
    interest_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct TemplateInput {
    name: String,
    body: String,
    proof_fallback: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct SlotFill {
    pub slot: String,
    pub value: String,
    pub resource_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct SentenceBinding {
    pub sentence_index: usize,
    pub claim_type: String,
    pub ref_kind: Option<String>,
    pub ref_id: Option<String>,
    pub ref_revision: Option<i64>,
    /// An explicit owner-reviewed mapping from this exact sentence to evidence.
    pub approved_wording: Option<String>,
    pub ask_rung: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct MaterialInput {
    recipient_id: String,
    kind: String,
    template_version_id: String,
    body: String,
    slot_fills: Vec<SlotFill>,
    sentence_bindings: Vec<SentenceBinding>,
    exchange_version: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ApprovalInput {
    override_reasons: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct MaterialRecord {
    pub id: String,
    pub opportunity_id: String,
    pub recipient_id: String,
    pub recipient_name: String,
    pub kind: String,
    pub version: i64,
    pub body: String,
    pub content_sha256: String,
    pub status: String,
    pub gate: Option<gate::GateResult>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ReleaseAckInput {
    succeeded: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ReadyInput {
    #[serde(default)]
    body: Option<Value>,
}

async fn create_opportunity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<OpportunityCreate>,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.create_opportunity(&input).await {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn list_opportunities(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.list_opportunities().await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn get_opportunity(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.get_opportunity(&id).await {
        Ok(Some(value)) => (StatusCode::OK, Json(value)).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not_found", "opportunity not found"),
        Err(error) => workspace_error(error),
    }
}

async fn add_participant(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ParticipantInput>,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.add_participant(&id, &input).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn link_source(
    State(state): State<AppState>,
    Path((id, source_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.set_source_link(&id, &source_id, true).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn unlink_source(
    State(state): State<AppState>,
    Path((id, source_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state
        .workspace
        .set_source_link(&id, &source_id, false)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn add_interest(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<InterestInput>,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.add_interest(&id, &input).await {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn save_exchange(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ExchangeInput>,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.save_exchange(&id, &input).await {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn attest_exchange(
    State(state): State<AppState>,
    Path((id, recipient_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.attest_exchange(&id, &recipient_id).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn create_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<TemplateInput>,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.create_template(&input).await {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn list_templates(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match sqlx::query_as::<_, (String,String,i64,String,String)>("SELECT id,family_id,version_no,status,body FROM template_version ORDER BY created_at DESC,id").fetch_all(state.workspace.pool()).await {
        Ok(rows) => (StatusCode::OK, Json(rows.into_iter().map(|(id,family_id,version,status,body)| json!({"id":id,"family_id":family_id,"version":version,"status":status,"body":body})).collect::<Vec<_>>())).into_response(),
        Err(error) => workspace_error(error.into()),
    }
}

async fn approve_template(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state
        .workspace
        .transition_template(&id, "draft", "approved")
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn activate_template(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state
        .workspace
        .transition_template(&id, "approved", "active")
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn create_material(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<MaterialInput>,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.create_material(&id, &input, None).await {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn edit_material(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<MaterialInput>,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.create_material("", &input, Some(&id)).await {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn list_materials(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.list_materials(&id).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn get_material(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.get_material(&id).await {
        Ok(Some(value)) => (StatusCode::OK, Json(value)).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not_found", "material not found"),
        Err(error) => workspace_error(error),
    }
}

async fn approve_material(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ApprovalInput>,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state
        .workspace
        .approve_material(&id, &input.override_reasons)
        .await
    {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn release_material(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.release_material(&id).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn ack_release(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ReleaseAckInput>,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state
        .workspace
        .acknowledge_release(&id, input.succeeded)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn list_outputs(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.list_outputs().await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn mark_resource_ready(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ReadyInput>,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state
        .workspace
        .mark_resource_ready(&id, input.body.as_ref())
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => workspace_error(error),
    }
}

fn error_response(status: StatusCode, code: &str, message: &str) -> axum::response::Response {
    (
        status,
        Json(json!({"error":{"code":code,"message":message}})),
    )
        .into_response()
}

pub(super) fn workspace_error(error: WorkspaceError) -> axum::response::Response {
    let (status, code) = match &error {
        WorkspaceError::Database(_) => (StatusCode::INTERNAL_SERVER_ERROR, "storage_error"),
        WorkspaceError::Invalid(_) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
        WorkspaceError::IdempotencyConflict => (StatusCode::CONFLICT, "idempotency_conflict"),
        WorkspaceError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        WorkspaceError::StaleRevision => (StatusCode::CONFLICT, "stale_revision"),
    };
    error_response(status, code, &error.to_string())
}

impl Workspace {
    async fn create_opportunity(
        &self,
        input: &OpportunityCreate,
    ) -> Result<Opportunity, WorkspaceError> {
        if input.title.trim().is_empty() || input.title.len() > 500 {
            return Err(WorkspaceError::Invalid(
                "opportunity title must contain 1–500 bytes".into(),
            ));
        }
        let id = new_id(&input.primary_party_id);
        let now = now_utc();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let party_kind: Option<String> =
            sqlx::query_scalar("SELECT kind FROM entity WHERE id=? AND kind IN ('person','org')")
                .bind(&input.primary_party_id)
                .fetch_optional(&mut *tx)
                .await?;
        if party_kind.is_none() {
            return Err(WorkspaceError::Invalid(
                "primary participant must be an existing person or organization".into(),
            ));
        }
        sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES(?,?,'open',1,?,?)")
            .bind(&id).bind(input.title.trim()).bind(&now).bind(&now).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES(?,?,'primary',?)")
            .bind(&id).bind(&input.primary_party_id).bind(&now).execute(&mut *tx).await?;
        let home_list: Option<String> = sqlx::query_scalar(
            "SELECT id FROM board_list WHERE archived=0 ORDER BY position,id LIMIT 1",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let home_list = home_list.ok_or_else(|| {
            WorkspaceError::Invalid("workspace must retain an active board list".into())
        })?;
        let position: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(position),0)+1024 FROM board_placement WHERE list_id=?",
        )
        .bind(&home_list)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO board_placement(opportunity_id,list_id,position,revision) VALUES(?,?,?,1)",
        )
        .bind(&id)
        .bind(&home_list)
        .bind(position)
        .execute(&mut *tx)
        .await?;
        event(
            &mut tx,
            "board_placement",
            &id,
            1,
            "board.card_created",
            json!({"list_id":home_list,"position":position}),
        )
        .await?;
        ensure_party_policy(&mut tx, &input.primary_party_id).await?;
        event(
            &mut tx,
            "opportunity",
            &id,
            1,
            "opportunity.created",
            json!({"primary_party_id":input.primary_party_id}),
        )
        .await?;
        tx.commit().await?;
        self.get_opportunity(&id)
            .await?
            .ok_or_else(|| WorkspaceError::Invalid("created opportunity is not readable".into()))
    }

    async fn list_opportunities(&self) -> Result<Vec<Opportunity>, WorkspaceError> {
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM opportunity ORDER BY updated_at DESC,id")
                .fetch_all(&self.pool)
                .await?;
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(row) = self.get_opportunity(&id).await? {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    async fn get_opportunity(&self, id: &str) -> Result<Option<Opportunity>, WorkspaceError> {
        let row = sqlx::query_as::<_, (String, String, String, i64)>(
            "SELECT id,title,state,revision FROM opportunity WHERE id=?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((id, title, state, revision)) = row else {
            return Ok(None);
        };
        let participant_ids = sqlx::query_scalar(
            "SELECT party_id FROM opportunity_party WHERE opportunity_id=? ORDER BY role,party_id",
        )
        .bind(&id)
        .fetch_all(&self.pool)
        .await?;
        let source_variant_ids = sqlx::query_scalar("SELECT source_variant_id FROM source_opportunity_link WHERE opportunity_id=? ORDER BY source_variant_id").bind(&id).fetch_all(&self.pool).await?;
        let material_ids = sqlx::query_scalar(
            "SELECT id FROM material WHERE opportunity_id=? ORDER BY updated_at DESC,id",
        )
        .bind(&id)
        .fetch_all(&self.pool)
        .await?;
        Ok(Some(Opportunity {
            id,
            title,
            state,
            revision,
            participant_ids,
            source_variant_ids,
            material_ids,
        }))
    }

    async fn add_participant(
        &self,
        opportunity_id: &str,
        input: &ParticipantInput,
    ) -> Result<Opportunity, WorkspaceError> {
        if !matches!(input.role.as_str(), "primary" | "participant") {
            return Err(WorkspaceError::Invalid(
                "role must be primary or participant".into(),
            ));
        }
        let now = now_utc();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let state: Option<String> = sqlx::query_scalar("SELECT state FROM opportunity WHERE id=?")
            .bind(opportunity_id)
            .fetch_optional(&mut *tx)
            .await?;
        if state.as_deref() != Some("open") {
            return Err(WorkspaceError::Invalid(
                "participants can only be added to an open opportunity".into(),
            ));
        }
        let kind: Option<String> =
            sqlx::query_scalar("SELECT kind FROM entity WHERE id=? AND kind IN ('person','org')")
                .bind(&input.party_id)
                .fetch_optional(&mut *tx)
                .await?;
        if kind.is_none() {
            return Err(WorkspaceError::Invalid(
                "participant must be an existing person or organization".into(),
            ));
        }
        sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES(?,?,?,?) ON CONFLICT(opportunity_id,party_id) DO UPDATE SET role=excluded.role")
            .bind(opportunity_id).bind(&input.party_id).bind(&input.role).bind(&now).execute(&mut *tx).await?;
        ensure_party_policy(&mut tx, &input.party_id).await?;
        let rev = bump_opportunity(&mut tx, opportunity_id, &now).await?;
        event(
            &mut tx,
            "opportunity",
            opportunity_id,
            rev,
            "opportunity.participant_added",
            json!({"party_id":input.party_id,"role":input.role}),
        )
        .await?;
        tx.commit().await?;
        self.get_opportunity(opportunity_id)
            .await?
            .ok_or_else(|| WorkspaceError::Invalid("opportunity disappeared".into()))
    }

    async fn set_source_link(
        &self,
        opportunity_id: &str,
        source_variant_id: &str,
        link: bool,
    ) -> Result<(), WorkspaceError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let open: Option<String> = sqlx::query_scalar("SELECT state FROM opportunity WHERE id=?")
            .bind(opportunity_id)
            .fetch_optional(&mut *tx)
            .await?;
        if open.as_deref() != Some("open") {
            return Err(WorkspaceError::Invalid(
                "opportunity is missing or closed".into(),
            ));
        }
        let source_exists: Option<String> =
            sqlx::query_scalar("SELECT id FROM source_variant WHERE id=?")
                .bind(source_variant_id)
                .fetch_optional(&mut *tx)
                .await?;
        if source_exists.is_none() {
            return Err(WorkspaceError::Invalid(
                "source variant does not exist".into(),
            ));
        }
        if link {
            sqlx::query("INSERT INTO source_opportunity_link(source_variant_id,opportunity_id,created_at) VALUES(?,?,?) ON CONFLICT DO NOTHING").bind(source_variant_id).bind(opportunity_id).bind(now_utc()).execute(&mut *tx).await?;
        } else {
            sqlx::query("DELETE FROM source_opportunity_link WHERE opportunity_id=? AND source_variant_id=?").bind(opportunity_id).bind(source_variant_id).execute(&mut *tx).await?;
        }
        let rev = bump_opportunity(&mut tx, opportunity_id, &now_utc()).await?;
        event(
            &mut tx,
            "opportunity",
            opportunity_id,
            rev,
            if link {
                "opportunity.source_linked"
            } else {
                "opportunity.source_unlinked"
            },
            json!({"source_variant_id":source_variant_id}),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn add_interest(
        &self,
        opportunity_id: &str,
        input: &InterestInput,
    ) -> Result<Value, WorkspaceError> {
        if !matches!(input.side.as_str(), "them" | "me")
            || !matches!(
                input.knowledge_kind.as_str(),
                "observation" | "hypothesis" | "owner_report"
            )
            || input.statement.trim().is_empty()
            || input.statement.len() > 3000
            || (input.knowledge_kind == "hypothesis"
                && input
                    .check_suggestion
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty()))
        {
            return Err(WorkspaceError::Invalid(
                "interest requires side, knowledge kind and a non-empty statement".into(),
            ));
        }
        let id = new_id(&input.statement);
        let now = now_utc();
        let source_backed = input.claim_id.is_some() && input.claim_revision.is_some();
        if input.claim_id.is_some() != input.claim_revision.is_some() {
            return Err(WorkspaceError::Invalid(
                "claim id and revision must be supplied together".into(),
            ));
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let member: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM opportunity_party WHERE opportunity_id=? AND party_id=?",
        )
        .bind(opportunity_id)
        .bind(&input.recipient_id)
        .fetch_one(&mut *tx)
        .await?;
        if member == 0 {
            return Err(WorkspaceError::Invalid(
                "interest recipient is not an opportunity participant".into(),
            ));
        }
        if let (Some(claim), Some(rev)) = (&input.claim_id, input.claim_revision) {
            let valid: Option<String> = sqlx::query_scalar("SELECT c.id FROM claim c JOIN claim_revision r ON r.claim_id=c.id AND r.revision=? WHERE c.id=? AND r.tier IN (1,3)")
                .bind(rev).bind(claim).fetch_optional(&mut *tx).await?;
            if valid.is_none() {
                return Err(WorkspaceError::Invalid(
                    "source-backed interest requires a cited tier 1 or tier 3 claim revision"
                        .into(),
                ));
            }
        }
        if input.side == "me" && input.knowledge_kind != "owner_report" {
            return Err(WorkspaceError::Invalid(
                "owner-side interests must be identified as owner reports".into(),
            ));
        }
        if source_backed && input.knowledge_kind == "hypothesis" {
            return Err(WorkspaceError::Invalid(
                "source-backed interests cannot be labelled as unverified hypotheses".into(),
            ));
        }
        sqlx::query("INSERT INTO opportunity_interest(id,opportunity_id,party_id,side,statement,knowledge_kind,check_suggestion,provenance_kind,claim_id,claim_revision,revision,status,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,1,'active',?)")
            .bind(&id).bind(opportunity_id).bind(&input.recipient_id).bind(&input.side).bind(input.statement.trim()).bind(&input.knowledge_kind).bind(input.check_suggestion.as_deref()).bind(if source_backed { "source_claim" } else { "owner_report" }).bind(input.claim_id.as_deref()).bind(input.claim_revision).bind(&now).execute(&mut *tx).await?;
        let rev = bump_opportunity(&mut tx, opportunity_id, &now).await?;
        event(
            &mut tx,
            "opportunity",
            opportunity_id,
            rev,
            "opportunity.interest_added",
            json!({"interest_id":id,"recipient_id":input.recipient_id,"side":input.side,"knowledge_kind":input.knowledge_kind,"check_suggestion":input.check_suggestion}),
        )
        .await?;
        tx.commit().await?;
        Ok(
            json!({"id":id,"opportunity_id":opportunity_id,"recipient_id":input.recipient_id,"side":input.side,"statement":input.statement,"knowledge_kind":input.knowledge_kind,"check_suggestion":input.check_suggestion,"revision":1,"status":"active"}),
        )
    }

    async fn save_exchange(
        &self,
        opportunity_id: &str,
        input: &ExchangeInput,
    ) -> Result<Value, WorkspaceError> {
        if input.they_get.len() > 3000 || input.i_get.len() > 3000 {
            return Err(WorkspaceError::Invalid(
                "exchange text exceeds 3000 bytes".into(),
            ));
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let membership: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM opportunity_party WHERE opportunity_id=? AND party_id=?",
        )
        .bind(opportunity_id)
        .bind(&input.recipient_id)
        .fetch_one(&mut *tx)
        .await?;
        if membership == 0 {
            return Err(WorkspaceError::Invalid(
                "recipient is not an opportunity participant".into(),
            ));
        }
        let next: i64 = sqlx::query_scalar("SELECT coalesce(max(version_no),0)+1 FROM exchange_version WHERE opportunity_id=? AND recipient_id=?").bind(opportunity_id).bind(&input.recipient_id).fetch_one(&mut *tx).await?;
        let now = now_utc();
        sqlx::query("INSERT INTO exchange_version(opportunity_id,recipient_id,version_no,they_get,i_get,created_at) VALUES(?,?,?,?,?,?)").bind(opportunity_id).bind(&input.recipient_id).bind(next).bind(input.they_get.trim()).bind(input.i_get.trim()).bind(&now).execute(&mut *tx).await?;
        let unique: BTreeSet<_> = input.interest_ids.iter().collect();
        if unique.len() != input.interest_ids.len() {
            return Err(WorkspaceError::Invalid(
                "exchange cannot link the same interest twice".into(),
            ));
        }
        for interest_id in &input.interest_ids {
            let eligible: Option<String> = sqlx::query_scalar("SELECT id FROM opportunity_interest WHERE id=? AND opportunity_id=? AND party_id=? AND status='active'").bind(interest_id).bind(opportunity_id).bind(&input.recipient_id).fetch_optional(&mut *tx).await?;
            if eligible.is_none() {
                return Err(WorkspaceError::Invalid(
                    "linked interest must be active and scoped to this opportunity and recipient"
                        .into(),
                ));
            }
            sqlx::query("INSERT INTO exchange_interest_link(opportunity_id,recipient_id,exchange_version,interest_id) VALUES(?,?,?,?)").bind(opportunity_id).bind(&input.recipient_id).bind(next).bind(interest_id).execute(&mut *tx).await?;
        }
        let rev = bump_opportunity(&mut tx, opportunity_id, &now).await?;
        event(
            &mut tx,
            "opportunity",
            opportunity_id,
            rev,
            "opportunity.exchange_version_created",
            json!({"recipient_id":input.recipient_id,"exchange_version":next}),
        )
        .await?;
        tx.commit().await?;
        Ok(
            json!({"opportunity_id":opportunity_id,"recipient_id":input.recipient_id,"version":next,"they_get":input.they_get,"i_get":input.i_get,"interest_ids":input.interest_ids,"fairness_attested":false}),
        )
    }

    async fn attest_exchange(
        &self,
        opportunity_id: &str,
        recipient_id: &str,
    ) -> Result<Value, WorkspaceError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let (version,they_get,i_get): (i64,String,String) = sqlx::query_as("SELECT version_no,they_get,i_get FROM exchange_version WHERE opportunity_id=? AND recipient_id=? ORDER BY version_no DESC LIMIT 1")
            .bind(opportunity_id).bind(recipient_id).fetch_optional(&mut *tx).await?.ok_or_else(|| WorkspaceError::Invalid("exchange does not exist".into()))?;
        let interests = exchange_interests(&mut tx, opportunity_id, recipient_id, version).await?;
        if they_get.trim().is_empty()
            || i_get.trim().is_empty()
            || !interests.iter().any(|(_, side, _, _)| side == "them")
            || !interests.iter().any(|(_, side, _, _)| side == "me")
        {
            return Err(WorkspaceError::Invalid(
                "exchange needs both benefits and at least one active interest from each side"
                    .into(),
            ));
        }
        let current_digest = exchange_digest(
            opportunity_id,
            recipient_id,
            version,
            &they_get,
            &i_get,
            &interests,
        );
        let now = now_utc();
        sqlx::query("UPDATE exchange_version SET fairness_attested=1,attestation_digest=?,attested_at=? WHERE opportunity_id=? AND recipient_id=? AND version_no=?")
            .bind(&current_digest).bind(&now).bind(opportunity_id).bind(recipient_id).bind(version).execute(&mut *tx).await?;
        let rev = bump_opportunity(&mut tx, opportunity_id, &now).await?;
        event(&mut tx,"opportunity",opportunity_id,rev,"opportunity.exchange_attested",json!({"recipient_id":recipient_id,"exchange_version":version,"attestation_digest":current_digest})).await?;
        tx.commit().await?;
        Ok(
            json!({"opportunity_id":opportunity_id,"recipient_id":recipient_id,"version":version,"fairness_attested":true,"attestation_digest":current_digest}),
        )
    }

    async fn create_template(&self, input: &TemplateInput) -> Result<Value, WorkspaceError> {
        let checked = template::validate(&input.body, input.proof_fallback.as_deref());
        if !checked.errors.is_empty() {
            return Err(WorkspaceError::Invalid(checked.errors.join("; ")));
        }
        if input.name.trim().is_empty() || input.name.len() > 200 {
            return Err(WorkspaceError::Invalid(
                "template name must contain 1–200 bytes".into(),
            ));
        }
        let family = new_id(&input.name);
        let id = new_id(&input.body);
        let now = now_utc();
        let fallbacks = serde_json::to_string(&json!({"proof":input.proof_fallback}))
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("INSERT INTO template_family(id,name,created_at) VALUES(?,?,?)")
            .bind(&family)
            .bind(input.name.trim())
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO template_version(id,family_id,version_no,body,fallbacks_json,status,content_sha256,created_at) VALUES(?,?,1,?,?, 'draft',?,?)").bind(&id).bind(&family).bind(&input.body).bind(fallbacks).bind(digest(input.body.as_bytes())).bind(&now).execute(&mut *tx).await?;
        event(
            &mut tx,
            "template",
            &id,
            1,
            "template.created",
            json!({"family_id":family,"version":1,"body_sha256":digest(input.body.as_bytes())}),
        )
        .await?;
        tx.commit().await?;
        Ok(
            json!({"id":id,"family_id":family,"version":1,"status":"draft","body":input.body,"slots":checked.slots}),
        )
    }

    async fn transition_template(
        &self,
        id: &str,
        from: &str,
        to: &str,
    ) -> Result<(), WorkspaceError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let (body, fallback, status): (String, String, String) =
            sqlx::query_as("SELECT body,fallbacks_json,status FROM template_version WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| WorkspaceError::Invalid("template does not exist".into()))?;
        if status != from {
            return Err(WorkspaceError::Invalid(format!(
                "template must be {from} before it can become {to}"
            )));
        }
        if to == "active" {
            let fallback: Value = serde_json::from_str(&fallback)
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            let checked = template::validate(&body, fallback.get("proof").and_then(Value::as_str));
            if !checked.errors.is_empty() {
                return Err(WorkspaceError::Invalid(checked.errors.join("; ")));
            }
        }
        sqlx::query("UPDATE template_version SET status=? WHERE id=? AND status=?")
            .bind(to)
            .bind(id)
            .bind(from)
            .execute(&mut *tx)
            .await?;
        event(
            &mut tx,
            "template",
            id,
            if to == "approved" { 2 } else { 3 },
            if to == "approved" {
                "template.approved"
            } else {
                "template.activated"
            },
            json!({"status":to}),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn create_material(
        &self,
        opportunity_id: &str,
        input: &MaterialInput,
        edit_id: Option<&str>,
    ) -> Result<MaterialRecord, WorkspaceError> {
        if !matches!(input.kind.as_str(), "outreach" | "followup" | "handoff") {
            return Err(WorkspaceError::Invalid("unsupported material kind".into()));
        }
        if input.body.trim().is_empty() || input.body.len() > 20_000 {
            return Err(WorkspaceError::Invalid(
                "material body must contain 1–20000 bytes".into(),
            ));
        }
        let sentences = split_sentences(&input.body);
        validate_binding_shape(&sentences, &input.sentence_bindings)?;
        let (material_id, _, _, _) = {
            let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
            let (material_id, opportunity_id, version) = if let Some(existing_id) = edit_id {
                let (opp, recipient, kind, head): (String, String, String, i64) = sqlx::query_as(
                    "SELECT opportunity_id,recipient_id,kind,head_version FROM material WHERE id=?",
                )
                .bind(existing_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| WorkspaceError::Invalid("material does not exist".into()))?;
                if recipient != input.recipient_id
                    || kind != input.kind
                    || input.template_version_id.is_empty()
                {
                    return Err(WorkspaceError::Invalid(
                        "new version must preserve recipient and material kind".into(),
                    ));
                }
                (existing_id.to_owned(), opp, head + 1)
            } else {
                (new_id(&input.body), opportunity_id.to_owned(), 1)
            };
            let opp_state: Option<String> =
                sqlx::query_scalar("SELECT state FROM opportunity WHERE id=?")
                    .bind(&opportunity_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if opp_state.as_deref() != Some("open") {
                return Err(WorkspaceError::Invalid(
                    "material requires an open opportunity".into(),
                ));
            }
            let membership: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM opportunity_party WHERE opportunity_id=? AND party_id=?",
            )
            .bind(&opportunity_id)
            .bind(&input.recipient_id)
            .fetch_one(&mut *tx)
            .await?;
            if membership == 0 {
                return Err(WorkspaceError::Invalid(
                    "material recipient must be an explicit opportunity participant".into(),
                ));
            }
            let template_row: Option<(String, String)> =
                sqlx::query_as("SELECT body,status FROM template_version WHERE id=?")
                    .bind(&input.template_version_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let Some((template_body, template_status)) = template_row else {
                return Err(WorkspaceError::Invalid(
                    "template version does not exist".into(),
                ));
            };
            if template_status != "active" {
                return Err(WorkspaceError::Invalid(
                    "only an active template version can be used".into(),
                ));
            }
            let fallbacks: String =
                sqlx::query_scalar("SELECT fallbacks_json FROM template_version WHERE id=?")
                    .bind(&input.template_version_id)
                    .fetch_one(&mut *tx)
                    .await?;
            let fallbacks: Value = serde_json::from_str(&fallbacks)
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            let check = template::validate(
                &template_body,
                fallbacks.get("proof").and_then(Value::as_str),
            );
            if !check.errors.is_empty() {
                return Err(WorkspaceError::Invalid(check.errors.join("; ")));
            }
            let slot_names: BTreeSet<String> = check.slots.into_iter().collect();
            let provided: BTreeSet<String> = input
                .slot_fills
                .iter()
                .map(|fill| fill.slot.clone())
                .collect();
            if slot_names != provided {
                return Err(WorkspaceError::Invalid(
                    "slot fills must cover every active template slot exactly once".into(),
                ));
            }
            if input
                .slot_fills
                .iter()
                .any(|fill| fill.value.trim().is_empty() || !input.body.contains(&fill.value))
            {
                return Err(WorkspaceError::Invalid(
                    "each non-empty slot value must appear verbatim in the material body".into(),
                ));
            }
            validate_slot_fills(&mut tx, &input.slot_fills, &input.recipient_id).await?;
            let exchange = sqlx::query_as::<_,(String,String,String,i64)> ("SELECT opportunity_id,recipient_id,they_get,version_no FROM exchange_version WHERE opportunity_id=? AND recipient_id=? AND version_no=?")
                .bind(&opportunity_id).bind(&input.recipient_id).bind(input.exchange_version).fetch_optional(&mut *tx).await?;
            if exchange.is_none() {
                return Err(WorkspaceError::Invalid(
                    "material must bind an exchange version for its selected recipient".into(),
                ));
            }
            if edit_id.is_none() {
                sqlx::query("INSERT INTO material(id,opportunity_id,recipient_id,kind,head_version,created_at,updated_at) VALUES(?,?,?,?,1,?,?)").bind(&material_id).bind(&opportunity_id).bind(&input.recipient_id).bind(&input.kind).bind(now_utc()).bind(now_utc()).execute(&mut *tx).await?;
            } else {
                sqlx::query("UPDATE material SET head_version=?,updated_at=? WHERE id=?")
                    .bind(version)
                    .bind(now_utc())
                    .bind(&material_id)
                    .execute(&mut *tx)
                    .await?;
            }
            let version_now = now_utc();
            let slot_json = serde_json::to_string(&input.slot_fills)
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            let bindings_json = serde_json::to_string(&input.sentence_bindings)
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            let hash = digest(input.body.as_bytes());
            sqlx::query("INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)")
                .bind(&material_id).bind(version).bind(&opportunity_id).bind(&input.recipient_id).bind(&input.template_version_id).bind(input.exchange_version).bind(&input.body).bind(slot_json).bind(bindings_json).bind(&hash).bind(&version_now).execute(&mut *tx).await?;
            persist_material_references(
                &mut tx,
                &material_id,
                version,
                &input.slot_fills,
                &input.sentence_bindings,
                &input.body,
                &input.recipient_id,
            )
            .await?;
            let event_type = if edit_id.is_some() {
                "material.version_created"
            } else {
                "material.created"
            };
            event(&mut tx,"material",&material_id,version,event_type,json!({"opportunity_id":opportunity_id,"recipient_id":input.recipient_id,"version":version,"content_sha256":hash})).await?;
            tx.commit().await?;
            (material_id, opportunity_id, version, version_now)
        };
        self.get_material(&material_id)
            .await?
            .ok_or_else(|| WorkspaceError::Invalid("material version was not readable".into()))
    }

    async fn get_material(&self, id: &str) -> Result<Option<MaterialRecord>, WorkspaceError> {
        let row = sqlx::query_as::<_,(String,String,String,String,String,i64,String,String)>("SELECT m.id,m.opportunity_id,m.recipient_id,e.name,m.kind,mv.version_no,mv.body,mv.content_sha256 FROM material m JOIN entity e ON e.id=m.recipient_id JOIN material_version mv ON mv.material_id=m.id AND mv.version_no=m.head_version WHERE m.id=?")
            .bind(id).fetch_optional(&self.pool).await?;
        let Some((
            id,
            opportunity_id,
            recipient_id,
            recipient_name,
            kind,
            version,
            body,
            content_sha256,
        )) = row
        else {
            return Ok(None);
        };
        let approved: Option<String> =
            sqlx::query_scalar("SELECT id FROM approval WHERE material_id=? AND version_no=?")
                .bind(&id)
                .bind(version)
                .fetch_optional(&self.pool)
                .await?;
        let released: Option<String> = sqlx::query_scalar("SELECT id FROM release_record WHERE material_id=? AND version_no=? ORDER BY issued_at DESC LIMIT 1").bind(&id).bind(version).fetch_optional(&self.pool).await?;
        let status = if released.is_some() {
            "released"
        } else if approved.is_some() {
            "approved"
        } else {
            "draft"
        };
        Ok(Some(MaterialRecord {
            id,
            opportunity_id,
            recipient_id,
            recipient_name,
            kind,
            version,
            body,
            content_sha256,
            status: status.into(),
            gate: None,
        }))
    }

    async fn list_materials(
        &self,
        opportunity_id: &str,
    ) -> Result<Vec<MaterialRecord>, WorkspaceError> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM material WHERE opportunity_id=? ORDER BY updated_at DESC,id",
        )
        .bind(opportunity_id)
        .fetch_all(&self.pool)
        .await?;
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(row) = self.get_material(&id).await? {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    async fn list_outputs(&self) -> Result<Vec<MaterialRecord>, WorkspaceError> {
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM material ORDER BY updated_at DESC,id")
                .fetch_all(&self.pool)
                .await?;
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(row) = self.get_material(&id).await? {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    async fn approve_material(
        &self,
        id: &str,
        overrides: &BTreeMap<String, String>,
    ) -> Result<Value, WorkspaceError> {
        validate_override_reasons(overrides)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let material = load_head(&mut tx, id).await?;
        let deps = dependency_vector(&mut tx, &material).await?;
        let checked = evaluate_material(&mut tx, &material, overrides).await?;
        if !checked.copy_enabled {
            return Err(WorkspaceError::Invalid(format!(
                "release blocked: {}",
                checked
                    .checks
                    .iter()
                    .filter(|check| !check.pass && !check.overridden)
                    .map(|check| check.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if sqlx::query_scalar::<_, String>(
            "SELECT id FROM approval WHERE material_id=? AND version_no=?",
        )
        .bind(&material.id)
        .bind(material.version)
        .fetch_optional(&mut *tx)
        .await?
        .is_some()
        {
            let old: String = sqlx::query_scalar(
                "SELECT dependency_vector_json FROM approval WHERE material_id=? AND version_no=?",
            )
            .bind(&material.id)
            .bind(material.version)
            .fetch_one(&mut *tx)
            .await?;
            if old
                != serde_json::to_string(&deps)
                    .map_err(|e| WorkspaceError::Invalid(e.to_string()))?
            {
                return Err(WorkspaceError::Invalid("this immutable material version already has a different approval; create a new version".into()));
            }
            tx.commit().await?;
            return Ok(
                json!({"material_id":material.id,"version":material.version,"content_sha256":material.hash,"gate":checked,"already_approved":true}),
            );
        }
        let id_approval = new_id(&material.hash);
        let deps_json =
            serde_json::to_string(&deps).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let gate_json =
            serde_json::to_string(&checked).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let overrides_json =
            serde_json::to_string(overrides).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let now = now_utc();
        sqlx::query("INSERT INTO approval(id,material_id,version_no,content_sha256,dependency_vector_json,gate_snapshot_json,overrides_json,approved_at) VALUES(?,?,?,?,?,?,?,?)").bind(&id_approval).bind(&material.id).bind(material.version).bind(&material.hash).bind(&deps_json).bind(&gate_json).bind(&overrides_json).bind(&now).execute(&mut *tx).await?;
        event(&mut tx,"material",&material.id,material.version,"material.approved",json!({"approval_id":id_approval,"version":material.version,"content_sha256":material.hash,"override_ids":overrides.keys().collect::<Vec<_>>()})).await?;
        tx.commit().await?;
        Ok(
            json!({"id":id_approval,"material_id":material.id,"version":material.version,"content_sha256":material.hash,"gate":checked,"approved_at":now}),
        )
    }

    async fn release_material(&self, id: &str) -> Result<Value, WorkspaceError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let material = load_head(&mut tx, id).await?;
        let (approval_id,hash,dependency_json,overrides_json):(String,String,String,String)=sqlx::query_as("SELECT id,content_sha256,dependency_vector_json,overrides_json FROM approval WHERE material_id=? AND version_no=?")
            .bind(&material.id).bind(material.version).fetch_optional(&mut *tx).await?.ok_or_else(||WorkspaceError::Invalid("material version must be approved before release".into()))?;
        if hash != material.hash {
            return Err(WorkspaceError::Invalid(
                "approved bytes do not match the immutable material version".into(),
            ));
        }
        let current = dependency_vector(&mut tx, &material).await?;
        let current_json =
            serde_json::to_string(&current).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        if dependency_json != current_json {
            return Err(WorkspaceError::Invalid(
                "material dependencies changed after approval; create and review a new version"
                    .into(),
            ));
        }
        let overrides: BTreeMap<String, String> = serde_json::from_str(&overrides_json)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let checked = evaluate_material(&mut tx, &material, &overrides).await?;
        if !checked.copy_enabled {
            return Err(WorkspaceError::Invalid(
                "current release gate blocks this material".into(),
            ));
        }
        let release_id = new_id(&approval_id);
        let now = now_utc();
        sqlx::query("INSERT INTO release_record(id,approval_id,material_id,version_no,content_sha256,issued_at) VALUES(?,?,?,?,?,?)").bind(&release_id).bind(&approval_id).bind(&material.id).bind(material.version).bind(&material.hash).bind(&now).execute(&mut *tx).await?;
        event(&mut tx,"material",&material.id,material.version,"material.release_issued",json!({"release_id":release_id,"approval_id":approval_id,"content_sha256":material.hash})).await?;
        let result = json!({"release_id":release_id,"material_id":material.id,"version":material.version,"recipient_id":material.recipient_id,"body":material.body,"content_sha256":material.hash,"issued_at":now});
        tx.commit().await?;
        Ok(result)
    }

    async fn acknowledge_release(
        &self,
        release_id: &str,
        succeeded: bool,
    ) -> Result<(), WorkspaceError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let exists: Option<String> =
            sqlx::query_scalar("SELECT material_id FROM release_record WHERE id=?")
                .bind(release_id)
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(WorkspaceError::Invalid(
                "release record does not exist".into(),
            ));
        }
        let previous: Option<i64> =
            sqlx::query_scalar("SELECT succeeded FROM release_ack WHERE release_id=?")
                .bind(release_id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(previous) = previous {
            if previous != i64::from(succeeded) {
                return Err(WorkspaceError::Invalid(
                    "release acknowledgement cannot be changed after it is recorded".into(),
                ));
            }
            tx.commit().await?;
            return Ok(());
        }
        let now = now_utc();
        sqlx::query("INSERT INTO release_ack(release_id,succeeded,acknowledged_at) VALUES(?,?,?)")
            .bind(release_id)
            .bind(succeeded)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        let (material_id, version): (String, i64) =
            sqlx::query_as("SELECT material_id,version_no FROM release_record WHERE id=?")
                .bind(release_id)
                .fetch_one(&mut *tx)
                .await?;
        event(
            &mut tx,
            "material",
            &material_id,
            version,
            "material.release_acknowledged",
            json!({"release_id":release_id,"succeeded":succeeded}),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn mark_resource_ready(
        &self,
        id: &str,
        body_override: Option<&Value>,
    ) -> Result<(), WorkspaceError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let (kind, body, status, revision): (String, String, String, i64) =
            sqlx::query_as("SELECT kind,body_json,readiness,revision FROM resource WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| WorkspaceError::Invalid("resource does not exist".into()))?;
        if status != "draft" {
            return Err(WorkspaceError::Invalid(
                "only a draft resource can transition to ready".into(),
            ));
        }
        let value = if let Some(value) = body_override {
            value.clone()
        } else {
            serde_json::from_str(&body).map_err(|e| WorkspaceError::Invalid(e.to_string()))?
        };
        validate_resource(&kind, &value)?;
        let updated =
            serde_json::to_string(&value).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let now = now_utc();
        sqlx::query("UPDATE resource SET body_json=?,readiness='ready',revision=revision+1,updated_at=? WHERE id=? AND revision=?").bind(updated).bind(&now).bind(id).bind(revision).execute(&mut *tx).await?;
        event(
            &mut tx,
            "resource",
            id,
            revision + 1,
            "resource.readied",
            json!({"kind":kind}),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[derive(Debug)]
struct HeadMaterial {
    id: String,
    opportunity_id: String,
    recipient_id: String,
    recipient_name: String,
    version: i64,
    template_id: String,
    exchange_version: i64,
    body: String,
    slot_fills: Vec<SlotFill>,
    bindings: Vec<SentenceBinding>,
    hash: String,
}

async fn load_head(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<HeadMaterial, WorkspaceError> {
    let row=sqlx::query_as::<_,(String,String,String,String,i64,String,i64,String,String,String,String)>("SELECT m.id,m.opportunity_id,m.recipient_id,e.name,mv.version_no,mv.template_version_id,mv.exchange_version,mv.body,mv.slot_fills_json,mv.sentence_bindings_json,mv.content_sha256 FROM material m JOIN entity e ON e.id=m.recipient_id JOIN material_version mv ON mv.material_id=m.id AND mv.version_no=m.head_version WHERE m.id=?")
        .bind(id).fetch_optional(&mut **tx).await?.ok_or_else(||WorkspaceError::Invalid("material does not exist".into()))?;
    Ok(HeadMaterial {
        id: row.0,
        opportunity_id: row.1,
        recipient_id: row.2,
        recipient_name: row.3,
        version: row.4,
        template_id: row.5,
        exchange_version: row.6,
        body: row.7,
        slot_fills: serde_json::from_str(&row.8)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?,
        bindings: serde_json::from_str(&row.9)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?,
        hash: row.10,
    })
}

async fn dependency_vector(
    tx: &mut Transaction<'_, Sqlite>,
    material: &HeadMaterial,
) -> Result<BTreeMap<String, String>, WorkspaceError> {
    let mut result = BTreeMap::new();
    result.insert(
        format!("opportunity:{}", material.opportunity_id),
        sqlx::query_scalar::<_, i64>("SELECT revision FROM opportunity WHERE id=?")
            .bind(&material.opportunity_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| WorkspaceError::Invalid("opportunity no longer exists".into()))?
            .to_string(),
    );
    result.insert(
        format!("party:{}", material.recipient_id),
        sqlx::query_scalar::<_, i64>("SELECT revision FROM party_policy WHERE party_id=?")
            .bind(&material.recipient_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| WorkspaceError::Invalid("recipient policy no longer exists".into()))?
            .to_string(),
    );
    result.insert(
        format!("template:{}", material.template_id),
        sqlx::query_scalar::<_, String>(
            "SELECT content_sha256 FROM template_version WHERE id=? AND status='active'",
        )
        .bind(&material.template_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| WorkspaceError::Invalid("template is no longer active".into()))?,
    );
    let (exchange_digest, _, _, _) = current_exchange(
        tx,
        &material.opportunity_id,
        &material.recipient_id,
        material.exchange_version,
    )
    .await?;
    result.insert(
        format!(
            "exchange:{}:{}:{}",
            material.opportunity_id, material.recipient_id, material.exchange_version
        ),
        exchange_digest,
    );
    for fill in &material.slot_fills {
        if let Some(resource_id) = &fill.resource_id {
            let (revision, readiness): (i64, String) =
                sqlx::query_as("SELECT revision,readiness FROM resource WHERE id=?")
                    .bind(resource_id)
                    .fetch_optional(&mut **tx)
                    .await?
                    .ok_or_else(|| {
                        WorkspaceError::Invalid(format!("resource {resource_id} no longer exists"))
                    })?;
            if readiness != "ready" {
                return Err(WorkspaceError::Invalid(format!(
                    "resource {resource_id} is no longer ready"
                )));
            }
            result.insert(format!("resource:{resource_id}"), revision.to_string());
        }
    }
    for binding in &material.bindings {
        if binding.claim_type == "other_fact" {
            let revision: i64 = sqlx::query_scalar("SELECT current_revision FROM claim WHERE id=?")
                .bind(binding.ref_id.as_deref().unwrap_or(""))
                .fetch_optional(&mut **tx)
                .await?
                .ok_or_else(|| WorkspaceError::Invalid("claim reference is missing".into()))?;
            if Some(revision) != binding.ref_revision || binding.approved_wording.is_none() {
                return Err(WorkspaceError::Invalid(
                    "claim was corrected or its owner-approved wording changed".into(),
                ));
            }
            result.insert(
                format!("claim:{}", binding.ref_id.as_deref().unwrap_or("")),
                revision.to_string(),
            );
        }
    }
    Ok(result)
}

async fn evaluate_material(
    tx: &mut Transaction<'_, Sqlite>,
    material: &HeadMaterial,
    overrides: &BTreeMap<String, String>,
) -> Result<gate::GateResult, WorkspaceError> {
    let (exchange_state,they_get,i_get,_,attested,digest_stored)=sqlx::query_as::<_,(String,String,String,i64,i64,Option<String>)>("SELECT o.state,x.they_get,x.i_get,x.version_no,x.fairness_attested,x.attestation_digest FROM opportunity o JOIN exchange_version x ON x.opportunity_id=o.id WHERE x.opportunity_id=? AND x.recipient_id=? AND x.version_no=?")
        .bind(&material.opportunity_id).bind(&material.recipient_id).bind(material.exchange_version).fetch_optional(&mut **tx).await?.ok_or_else(||WorkspaceError::Invalid("exchange version is unavailable".into()))?;
    let interests = exchange_interests(
        tx,
        &material.opportunity_id,
        &material.recipient_id,
        material.exchange_version,
    )
    .await?;
    let current = exchange_digest(
        &material.opportunity_id,
        &material.recipient_id,
        material.exchange_version,
        &they_get,
        &i_get,
        &interests,
    );
    let exchange_complete = !they_get.trim().is_empty()
        && !i_get.trim().is_empty()
        && interests.iter().any(|(_, side, _, _)| side == "them")
        && interests.iter().any(|(_, side, _, _)| side == "me");
    let policy: Option<(String, i64, i64)> = sqlx::query_as(
        "SELECT state,do_not_contact,highest_rung FROM party_policy WHERE party_id=?",
    )
    .bind(&material.recipient_id)
    .fetch_optional(&mut **tx)
    .await?;
    let (party_state, dnc, highest_rung) =
        policy.ok_or_else(|| WorkspaceError::Invalid("recipient has no policy row".into()))?;
    let template_body: String = sqlx::query_scalar("SELECT body FROM template_version WHERE id=?")
        .bind(&material.template_id)
        .fetch_one(&mut **tx)
        .await?;
    let declared_slots = template::validate(&template_body, None).slots;
    let slot_values: Vec<String> = material
        .slot_fills
        .iter()
        .map(|fill| fill.value.clone())
        .collect();
    let sentence_list = split_sentences(&material.body);
    validate_binding_shape(&sentence_list, &material.bindings)?;
    let mut binding_valid = true;
    let mut ask_count = 0usize;
    let mut ask_rung = 0u32;
    let mut used_rules = BTreeSet::new();
    let mut gift = false;
    let mut anchor = false;
    let mut private_artifact = false;
    let mut uses_hook = false;
    let mut sensitive = false;
    let mut identity_valid = true;
    for binding in &material.bindings {
        let sentence = &sentence_list[binding.sentence_index];
        match binding.claim_type.as_str() {
            "generic" => {
                if binding.ref_kind.is_some()
                    || binding.ref_id.is_some()
                    || !generic_sentence_allowed(sentence, &material.recipient_name)
                {
                    binding_valid = false;
                }
            }
            "offer" => {
                let Some(resource_id) = binding
                    .ref_id
                    .as_deref()
                    .filter(|_| binding.ref_kind.as_deref() == Some("resource"))
                else {
                    binding_valid = false;
                    continue;
                };
                let row: Option<(String, String, String, String)> = sqlx::query_as(
                    "SELECT kind,readiness,title,body_json FROM resource WHERE id=?",
                )
                .bind(resource_id)
                .fetch_optional(&mut **tx)
                .await?;
                if let Some((kind, status, _title, body)) = row {
                    let value: Value = serde_json::from_str(&body)
                        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
                    let what = value.get("what").and_then(Value::as_str).unwrap_or("");
                    if kind != "gift"
                        || status != "ready"
                        || what.is_empty()
                        || !sentence.contains(what)
                        || !tokens_contained(sentence, &body, &material.recipient_name)
                    {
                        binding_valid = false;
                    }
                    gift = true;
                } else {
                    binding_valid = false;
                }
                used_rules.insert("R1");
            }
            "self_fact" => {
                let Some(resource_id) = binding
                    .ref_id
                    .as_deref()
                    .filter(|_| binding.ref_kind.as_deref() == Some("resource"))
                else {
                    binding_valid = false;
                    continue;
                };
                let row: Option<(String, String, String)> =
                    sqlx::query_as("SELECT kind,readiness,body_json FROM resource WHERE id=?")
                        .bind(resource_id)
                        .fetch_optional(&mut **tx)
                        .await?;
                if let Some((kind, status, body)) = row {
                    if status != "ready"
                        || !matches!(kind.as_str(), "proof" | "artifact")
                        || binding.approved_wording.as_deref() != Some(sentence)
                        || !tokens_contained(sentence, &body, &material.recipient_name)
                    {
                        binding_valid = false;
                    }
                    anchor |= kind == "proof";
                    if kind == "proof" {
                        used_rules.insert("R3");
                    }
                    if kind == "artifact" {
                        used_rules.insert("R4");
                    }
                } else {
                    binding_valid = false;
                }
            }
            "other_fact" => {
                let Some(claim_id) = binding
                    .ref_id
                    .as_deref()
                    .filter(|_| binding.ref_kind.as_deref() == Some("claim"))
                else {
                    binding_valid = false;
                    continue;
                };
                let valid:Option<(i64,i64,String,String)>=sqlx::query_as("SELECT c.current_revision,r.tier,r.wording,coalesce(r.approved_wording,'') FROM claim c JOIN claim_revision r ON r.claim_id=c.id AND r.revision=? WHERE c.id=?").bind(binding.ref_revision.unwrap_or_default()).bind(claim_id).fetch_optional(&mut **tx).await?;
                if let Some((current_rev, tier, wording, _approved)) = valid {
                    if current_rev != binding.ref_revision.unwrap_or_default()
                        || !matches!(tier, 1 | 3)
                        || binding.approved_wording.as_deref() != Some(sentence)
                        || !tokens_contained(sentence, &wording, &material.recipient_name)
                    {
                        binding_valid = false;
                    }
                } else {
                    binding_valid = false;
                }
                anchor = true;
                used_rules.insert("R5");
            }
            "question" => {
                let Some(interest_id) = binding
                    .ref_id
                    .as_deref()
                    .filter(|_| binding.ref_kind.as_deref() == Some("interest"))
                else {
                    binding_valid = false;
                    continue;
                };
                let valid: Option<String> = sqlx::query_scalar("SELECT statement FROM opportunity_interest WHERE id=? AND opportunity_id=? AND party_id=? AND status='active'")
                    .bind(interest_id).bind(&material.opportunity_id).bind(&material.recipient_id).fetch_optional(&mut **tx).await?;
                if valid.is_none()
                    || !sentence.trim_end().ends_with('?')
                    || binding.approved_wording.as_deref() != Some(sentence)
                {
                    binding_valid = false;
                }
            }
            "ask" => {
                if binding.ref_kind.as_deref() != Some("ladder")
                    || binding.ask_rung.is_none()
                    || binding.ask_rung.unwrap_or(0) == 0
                    || binding.ask_rung.unwrap_or(6) > 5
                    || !sentence.trim_end().ends_with('?')
                {
                    binding_valid = false;
                }
                ask_count += 1;
                ask_rung = ask_rung.max(binding.ask_rung.unwrap_or_default());
                used_rules.insert("R2");
            }
            "identity" => {
                identity_valid = false;
                anchor = true;
                used_rules.insert("R7");
            }
            "hook" => {
                uses_hook = true;
                anchor = true;
                used_rules.insert("R5");
            }
            _ => binding_valid = false,
        }
        sensitive |= has_sensitive_content(sentence);
    }
    for fill in &material.slot_fills {
        if let Some(resource_id) = &fill.resource_id {
            let row: Option<(String, String)> = sqlx::query_as(
                "SELECT kind,body_json FROM resource WHERE id=? AND readiness='ready'",
            )
            .bind(resource_id)
            .fetch_optional(&mut **tx)
            .await?;
            let Some((kind, body)) = row else {
                binding_valid = false;
                continue;
            };
            if kind == "proof" || kind == "artifact" {
                anchor = true;
            }
            if kind == "gift" {
                gift = true;
            }
            if kind == "proof" {
                used_rules.insert("R3");
            }
            if kind == "artifact" {
                used_rules.insert("R4");
            }
            if kind == "gift" {
                used_rules.insert("R1");
            }
            if kind == "artifact" {
                let v: Value = serde_json::from_str(&body)
                    .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
                private_artifact |= v.get("public").and_then(Value::as_bool) == Some(false);
            }
            let expected_binding = material.bindings.iter().any(|binding| {
                binding.ref_kind.as_deref() == Some("resource")
                    && binding.ref_id.as_deref() == Some(resource_id.as_str())
                    && match kind.as_str() {
                        "gift" => binding.claim_type == "offer",
                        "proof" | "artifact" => binding.claim_type == "self_fact",
                        _ => false,
                    }
            });
            if !expected_binding {
                binding_valid = false;
            }
            sensitive |= has_sensitive_content(&fill.value);
        } else {
            sensitive |= has_sensitive_content(&fill.value);
        }
    }
    let has_capacity_claim = has_capacity_content(&material.body);
    let banned = [
        "i hope this finds you well",
        "synergy",
        "circle back",
        "touch base",
        "leverage",
    ];
    let lint_error = banned
        .iter()
        .any(|phrase| material.body.to_lowercase().contains(phrase));
    let char_limit = 1200usize;
    let glossary: Vec<(String, String)> = sqlx::query_as("SELECT term,status FROM fluency")
        .fetch_all(&mut **tx)
        .await?;
    let fluency_ok = glossary
        .iter()
        .all(|(term, status)| status == "fluent" || !contains_glossary_term(&material.body, term));
    let mut input = gate::GateInput {
        exchange_complete,
        fairness_attested: attested == 1 && digest_stored.as_deref() == Some(current.as_str()),
        factual_sentences_grounded: binding_valid,
        sentence_count: sentence_list.len(),
        bound_sentence_count: material.bindings.len(),
        slot_fills: slot_values,
        body: material.body.clone(),
        scarcity_claim_valid: !has_capacity_claim,
        identity_claims_valid: identity_valid,
        ask_count,
        ask_rung,
        highest_rung: highest_rung.max(0) as u32,
        has_sensitive_content: sensitive,
        recipient_open: party_state == "open" && exchange_state == "open",
        recipient_do_not_contact: dnc != 0,
        references_private_artifact: private_artifact,
        gift_in_body: gift,
        gift_delivered_before: false,
        anchor_used: anchor,
        rule_count: used_rules.len(),
        rule_limit: if sqlx::query_scalar::<_, i64>(
            "SELECT touch_count FROM party_policy WHERE party_id=?",
        )
        .bind(&material.recipient_id)
        .fetch_one(&mut **tx)
        .await?
            == 0
        {
            3
        } else {
            4
        },
        fluency_ok,
        lint_error,
        hook_fresh: !uses_hook,
        cadence_ok: sqlx::query_scalar::<_, i64>(
            "SELECT touch_count FROM party_policy WHERE party_id=?",
        )
        .bind(&material.recipient_id)
        .fetch_one(&mut **tx)
        .await?
            < 3,
        daily_cap_ok: true,
        body_char_limit: char_limit,
        brief_reviewed: true,
        override_reasons: overrides.clone(),
    };
    if !declared_slots
        .iter()
        .all(|slot| material.slot_fills.iter().any(|fill| &fill.slot == slot))
    {
        input.factual_sentences_grounded = false;
    }
    Ok(gate::evaluate(&input))
}

fn contains_glossary_term(body: &str, term: &str) -> bool {
    let body = body.to_lowercase();
    let term = term.trim().to_lowercase();
    if term.is_empty() {
        return false;
    }
    body.match_indices(&term).any(|(start, matched)| {
        let end = start + matched.len();
        let before = body[..start].chars().next_back();
        let after = body[end..].chars().next();
        let word_edge = |ch: Option<char>| ch.is_none_or(|value| !value.is_alphanumeric());
        word_edge(before) && word_edge(after)
    })
}

async fn persist_material_references(
    tx: &mut Transaction<'_, Sqlite>,
    material_id: &str,
    version: i64,
    fills: &[SlotFill],
    bindings: &[SentenceBinding],
    body: &str,
    recipient: &str,
) -> Result<(), WorkspaceError> {
    for fill in fills {
        if let Some(resource_id) = &fill.resource_id {
            let revision: Option<i64> = sqlx::query_scalar(
                "SELECT revision FROM resource WHERE id=? AND readiness='ready'",
            )
            .bind(resource_id)
            .fetch_optional(&mut **tx)
            .await?;
            if revision.is_none() {
                return Err(WorkspaceError::Invalid(
                    "slot resource must be ready".into(),
                ));
            }
            sqlx::query("INSERT INTO resource_use(material_id,version_no,resource_id,slot_name) VALUES(?,?,?,?)").bind(material_id).bind(version).bind(resource_id).bind(&fill.slot).execute(&mut **tx).await?;
        }
    }
    for binding in bindings {
        if binding.claim_type == "other_fact" {
            let revision: Option<i64> =
                sqlx::query_scalar("SELECT current_revision FROM claim WHERE id=?")
                    .bind(binding.ref_id.as_deref().unwrap_or(""))
                    .fetch_optional(&mut **tx)
                    .await?;
            if revision != binding.ref_revision {
                return Err(WorkspaceError::Invalid(
                    "claim reference must use its current revision".into(),
                ));
            }
        }
    }
    let _ = (body, recipient);
    Ok(())
}

async fn validate_slot_fills(
    tx: &mut Transaction<'_, Sqlite>,
    fills: &[SlotFill],
    recipient_id: &str,
) -> Result<(), WorkspaceError> {
    let recipient: String = sqlx::query_scalar("SELECT name FROM entity WHERE id=?")
        .bind(recipient_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| WorkspaceError::Invalid("material recipient does not exist".into()))?;
    let sender: String = sqlx::query_scalar("SELECT display_name FROM owner_profile WHERE id=1")
        .fetch_one(&mut **tx)
        .await?;
    for fill in fills {
        let (kind, _) = fill
            .slot
            .split_once(':')
            .ok_or_else(|| WorkspaceError::Invalid(format!("slot '{}' is not typed", fill.slot)))?;
        match kind {
            "their_name" if fill.value == recipient && fill.resource_id.is_none() => {}
            "sender_name" if fill.value == sender && fill.resource_id.is_none() => {}
            "gift" | "proof" | "artifact" => {
                let resource_id = fill.resource_id.as_deref().ok_or_else(|| {
                    WorkspaceError::Invalid(format!(
                        "{kind} slot '{}' requires a resource",
                        fill.slot
                    ))
                })?;
                let row: Option<(String, String, String)> =
                    sqlx::query_as("SELECT kind,readiness,body_json FROM resource WHERE id=?")
                        .bind(resource_id)
                        .fetch_optional(&mut **tx)
                        .await?;
                let Some((resource_kind, readiness, body)) = row else {
                    return Err(WorkspaceError::Invalid(format!(
                        "resource {resource_id} does not exist"
                    )));
                };
                if readiness != "ready" || resource_kind != kind {
                    return Err(WorkspaceError::Invalid(format!(
                        "{kind} slot must resolve to a ready {kind} resource"
                    )));
                }
                let body: Value = serde_json::from_str(&body)
                    .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
                let resolved = if kind == "gift" {
                    body.get("what").and_then(Value::as_str)
                } else {
                    body.get("title").and_then(Value::as_str)
                }
                .unwrap_or_default();
                if fill.value != resolved {
                    return Err(WorkspaceError::Invalid(format!(
                        "{kind} slot value must equal the selected resource's approved name"
                    )));
                }
                if kind == "artifact" && body.get("public").and_then(Value::as_bool) != Some(true) {
                    return Err(WorkspaceError::Invalid(
                        "artifact slot must resolve to a public shareable artifact".into(),
                    ));
                }
            }
            "ask" if fill.resource_id.is_none() => {}
            "custom" if fill.resource_id.is_none() => {}
            _ => {
                return Err(WorkspaceError::Invalid(format!(
                    "slot resolver for '{kind}' is unavailable or its value does not match the saved record"
                )));
            }
        }
    }
    Ok(())
}

async fn current_exchange(
    tx: &mut Transaction<'_, Sqlite>,
    opportunity: &str,
    recipient: &str,
    version: i64,
) -> Result<(String, String, String, Vec<(String, String, String, i64)>), WorkspaceError> {
    let (they_get,i_get):(String,String)=sqlx::query_as("SELECT they_get,i_get FROM exchange_version WHERE opportunity_id=? AND recipient_id=? AND version_no=?").bind(opportunity).bind(recipient).bind(version).fetch_optional(&mut **tx).await?.ok_or_else(||WorkspaceError::Invalid("exchange version does not exist".into()))?;
    let interests = exchange_interests(tx, opportunity, recipient, version).await?;
    let digest = exchange_digest(
        opportunity,
        recipient,
        version,
        &they_get,
        &i_get,
        &interests,
    );
    Ok((digest, they_get, i_get, interests))
}

async fn exchange_interests(
    tx: &mut Transaction<'_, Sqlite>,
    opportunity: &str,
    recipient: &str,
    version: i64,
) -> Result<Vec<(String, String, String, i64)>, WorkspaceError> {
    Ok(sqlx::query_as("SELECT i.id,i.side,i.statement,i.revision FROM exchange_interest_link l JOIN opportunity_interest i ON i.id=l.interest_id WHERE l.opportunity_id=? AND l.recipient_id=? AND l.exchange_version=? AND i.status='active' ORDER BY i.id")
        .bind(opportunity).bind(recipient).bind(version).fetch_all(&mut **tx).await?)
}

fn exchange_digest(
    opportunity: &str,
    recipient: &str,
    version: i64,
    they_get: &str,
    i_get: &str,
    interests: &[(String, String, String, i64)],
) -> String {
    digest(
        &serde_json::to_vec(&(opportunity, recipient, version, they_get, i_get, interests))
            .unwrap_or_default(),
    )
}

async fn ensure_party_policy(
    tx: &mut Transaction<'_, Sqlite>,
    party_id: &str,
) -> Result<(), WorkspaceError> {
    sqlx::query("INSERT INTO party_policy(party_id) VALUES(?) ON CONFLICT(party_id) DO NOTHING")
        .bind(party_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn bump_opportunity(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    now: &str,
) -> Result<i64, WorkspaceError> {
    sqlx::query("UPDATE opportunity SET revision=revision+1,updated_at=? WHERE id=?")
        .bind(now)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(
        sqlx::query_scalar("SELECT revision FROM opportunity WHERE id=?")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?,
    )
}

async fn event(
    tx: &mut Transaction<'_, Sqlite>,
    entity_type: &str,
    entity_id: &str,
    revision: i64,
    event_type: &str,
    payload: Value,
) -> Result<(), WorkspaceError> {
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,?,?,?,?,?)")
        .bind(new_id(entity_id)).bind(now_utc()).bind(entity_type).bind(entity_id).bind(revision).bind(event_type).bind(payload.to_string()).execute(&mut **tx).await?;
    Ok(())
}

fn validate_binding_shape(
    sentences: &[String],
    bindings: &[SentenceBinding],
) -> Result<(), WorkspaceError> {
    if sentences.is_empty() {
        return Err(WorkspaceError::Invalid(
            "material needs at least one sentence".into(),
        ));
    }
    let mut indexes = BTreeSet::new();
    for binding in bindings {
        if binding.sentence_index >= sentences.len() || !indexes.insert(binding.sentence_index) {
            return Err(WorkspaceError::Invalid(
                "sentence bindings must use each sentence index at most once".into(),
            ));
        }
        if !matches!(
            binding.claim_type.as_str(),
            "generic"
                | "offer"
                | "self_fact"
                | "other_fact"
                | "question"
                | "ask"
                | "identity"
                | "hook"
        ) {
            return Err(WorkspaceError::Invalid(
                "unsupported sentence claim type".into(),
            ));
        }
    }
    if indexes.len() != sentences.len() {
        return Err(WorkspaceError::Invalid(
            "every sentence, including generic text, needs an explicit owner review classification"
                .into(),
        ));
    }
    Ok(())
}

fn split_sentences(body: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0;
    for (index, ch) in body.char_indices() {
        let end = index + ch.len_utf8();
        if matches!(ch, '.' | '!' | '?' | '\n') {
            let value = body[start..end].trim();
            if !value.is_empty() {
                result.push(value.to_owned());
            }
            start = end;
        }
    }
    let rest = body[start..].trim();
    if !rest.is_empty() {
        result.push(rest.to_owned());
    }
    result
}

fn generic_sentence_allowed(sentence: &str, recipient_name: &str) -> bool {
    if sentence.chars().any(|ch| ch.is_ascii_digit()) {
        return false;
    }
    let recipient_words: BTreeSet<String> = recipient_name
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let mut first = true;
    for token in sentence.split_whitespace() {
        let word = token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '\'');
        if word.is_empty() {
            continue;
        }
        let allowed = first || word == "I" || recipient_words.contains(&word.to_lowercase());
        if !allowed && word.chars().next().is_some_and(char::is_uppercase) {
            return false;
        }
        first = false;
    }
    true
}

fn tokens_contained(sentence: &str, source: &str, recipient_name: &str) -> bool {
    let recipient_lower = recipient_name.to_lowercase();
    let source_lower = source.to_lowercase();
    for token in sentence.split_whitespace() {
        let token = token
            .trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '%' && ch != '$' && ch != '.');
        if token.is_empty() {
            continue;
        }
        let numeric = token.chars().any(|ch| ch.is_ascii_digit());
        let named = token.chars().next().is_some_and(char::is_uppercase) && token.len() > 1;
        if (numeric || named)
            && !source_lower.contains(&token.to_lowercase())
            && !recipient_lower.contains(&token.to_lowercase())
        {
            return false;
        }
    }
    true
}

fn has_sensitive_content(text: &str) -> bool {
    const TERMS: &[&str] = &[
        "health",
        "disability",
        "medical",
        "family",
        "wife",
        "husband",
        "child",
        "children",
        "sexual orientation",
        "religion",
        "politics",
        "political",
        "race",
        "ethnicity",
        "immigration",
        "union membership",
        "financial hardship",
        "home address",
        "home location",
        "pregnant",
        "diagnosis",
    ];
    let normalized = text.to_lowercase();
    TERMS.iter().any(|term| normalized.contains(term))
}

fn has_capacity_content(text: &str) -> bool {
    let normalized = text.to_lowercase();
    [
        "capacity",
        "remaining slots",
        "available slots",
        "only have",
        "interview slots",
        "project slots",
    ]
    .iter()
    .any(|term| normalized.contains(term))
}

fn validate_override_reasons(reasons: &BTreeMap<String, String>) -> Result<(), WorkspaceError> {
    for (id, reason) in reasons {
        if !id.starts_with('S') || reason.trim().chars().count() < 10 {
            return Err(WorkspaceError::Invalid(format!(
                "soft-gate override {id} requires a reason of at least ten characters"
            )));
        }
    }
    Ok(())
}

fn validate_resource(kind: &str, value: &Value) -> Result<(), WorkspaceError> {
    let string = |name: &str, required: bool| -> bool {
        value
            .get(name)
            .and_then(Value::as_str)
            .is_some_and(|v| !v.trim().is_empty())
            || (!required && value.get(name).is_none())
    };
    let missing = match kind {
        "gift" => {
            let mut fields = Vec::new();
            for key in ["what", "format", "delivery", "cost_to_me"] {
                if !string(key, true) {
                    fields.push(key);
                }
            }
            match value.get("delivery").and_then(Value::as_str) {
                Some("on_request") => {
                    if !value
                        .get("time_to_produce_minutes")
                        .and_then(Value::as_u64)
                        .is_some()
                    {
                        fields.push("time_to_produce_minutes")
                    }
                }
                Some("exists") => {
                    if !string("evidence_ref", true) {
                        fields.push("evidence_ref")
                    }
                    if value.get("permission_to_share").and_then(Value::as_bool) != Some(true) {
                        fields.push("permission_to_share")
                    }
                }
                _ => fields.push("delivery (exists or on_request)"),
            }
            if !matches!(
                value.get("cost_to_me").and_then(Value::as_str),
                Some("low" | "medium" | "high")
            ) {
                fields.push("cost_to_me (low, medium or high)");
            }
            if !matches!(
                value.get("format").and_then(Value::as_str),
                Some(
                    "teardown"
                        | "intro"
                        | "resource"
                        | "audit"
                        | "bug report"
                        | "template"
                        | "other"
                )
            ) {
                fields.push("format");
            }
            fields
        }
        "proof" => {
            let mut fields = Vec::new();
            for key in [
                "what_i_did",
                "for_whom",
                "problem",
                "result_unit",
                "period",
                "my_role",
            ] {
                if !string(key, true) {
                    fields.push(key);
                }
            }
            if !value
                .get("permission_to_name")
                .is_some_and(Value::is_boolean)
            {
                fields.push("permission_to_name")
            }
            if value
                .get("for_whom")
                .and_then(Value::as_str)
                .is_some_and(|who| who.split_whitespace().count() < 2)
            {
                fields.push("for_whom (named recipient or a two-attribute descriptor)");
            }
            if !value.get("result_value").is_some_and(|v| v.is_number()) {
                fields.push("result_value")
            }
            if value.get("result_unit").and_then(Value::as_str) == Some("%")
                && !string("baseline", true)
            {
                fields.push("baseline")
            }
            if value
                .get("what_i_did")
                .and_then(Value::as_str)
                .is_some_and(|text| text.split_whitespace().count() < 6)
            {
                fields.push("what_i_did (at least six words)")
            }
            fields
        }
        "artifact" => {
            let mut fields = Vec::new();
            for key in ["title", "location", "what_it_shows"] {
                if !string(key, true) {
                    fields.push(key);
                }
            }
            if !value.get("public").is_some_and(Value::is_boolean) {
                fields.push("public")
            };
            if value
                .get("location")
                .and_then(Value::as_str)
                .is_some_and(|location| location.trim().is_empty())
            {
                fields.push("location (URL or local path)");
            }
            fields
        }
        _ => return Err(WorkspaceError::Invalid("unsupported resource kind".into())),
    };
    if !missing.is_empty() {
        return Err(WorkspaceError::Invalid(format!(
            "resource is missing required fields: {}",
            missing.join(", ")
        )));
    }
    Ok(())
}
