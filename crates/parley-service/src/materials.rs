//! Opportunity-scoped manual authoring and recipient-facing release.
//!
//! The public handlers do no policy work themselves: they authorize the local
//! owner, then execute the corresponding transaction in the workspace.

use super::release::{
    AttachmentInfo, dependency_vector, evaluate_material, exchange_digest, exchange_interests,
    load_head,
};
use super::{AppState, Workspace, WorkspaceError, authenticate, digest, new_id, now_utc};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use parley_domain::{gate, template};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::QueryBuilder;
use sqlx::{Sqlite, Transaction};
use std::collections::{BTreeMap, BTreeSet};

fn materials_canonical(route: &str, input: &impl Serialize) -> String {
    let body = serde_json::to_vec(input).unwrap_or_default();
    let mut bytes = route.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(&body);
    digest(&bytes)
}

pub(super) async fn material_receipt_replay(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    epoch: &str,
    key: &str,
    canonical: &str,
) -> Result<Option<Value>, WorkspaceError> {
    let old: Option<(String, String)> = sqlx::query_as(
        "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
    )
    .bind(epoch)
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((old_canonical, result)) = old else {
        return Ok(None);
    };
    if old_canonical != canonical {
        return Err(WorkspaceError::IdempotencyConflict);
    }
    Ok(Some(serde_json::from_str(&result).map_err(|error| {
        WorkspaceError::Invalid(error.to_string())
    })?))
}

pub(super) async fn material_receipt_store(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    epoch: &str,
    key: &str,
    canonical: &str,
    status: u16,
    result: &Value,
) -> Result<(), WorkspaceError> {
    let result_json = serde_json::to_string(result)
        .map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
    let cursor: i64 = sqlx::query_scalar("SELECT coalesce(max(cursor),0) FROM event")
        .fetch_one(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,?,?,?,?)")
        .bind(epoch).bind(key).bind(canonical).bind(i64::from(status)).bind(result_json).bind(cursor).bind(now_utc()).execute(&mut **tx).await?;
    Ok(())
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/opportunities",
            get(list_opportunities).post(create_opportunity),
        )
        .route(
            "/api/v1/opportunities/{id}/tags",
            axum::routing::put(set_opportunity_tags),
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
        .route(
            "/api/v1/opportunities/{id}/interests",
            get(get_interests).post(add_interest),
        )
        .route(
            "/api/v1/opportunities/{id}/exchange",
            get(get_exchange).post(save_exchange),
        )
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
        .route("/api/v1/materials/{id}/gate", post(evaluate_material_gate))
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
struct AttachmentInput {
    upload_id: String,
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
    #[serde(default)]
    attachment: Option<AttachmentInput>,
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
    pub attachment: Option<AttachmentInfo>,
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
    Query(filters): Query<OpportunityFilters>,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.opportunity_summaries(&filters).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct OpportunityFilters {
    pub(super) q: Option<String>,
    #[serde(default)]
    pub(super) party_id: Vec<String>,
    #[serde(default)]
    pub(super) tag: Vec<String>,
    pub(super) status: Option<String>,
    pub(super) missing_material: Option<bool>,
    pub(super) deadline_before: Option<String>,
    pub(super) sort: Option<String>,
    pub(super) order: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct OpportunityParticipant {
    id: String,
    name: String,
    kind: String,
}

#[derive(Debug, Serialize)]
pub(super) struct OpportunitySummary {
    pub id: String,
    pub title: String,
    pub state: String,
    pub revision: i64,
    pub participants: Vec<OpportunityParticipant>,
    pub tags: Vec<String>,
    pub next_task: Option<String>,
    pub next_deadline: Option<String>,
    pub missing_material: bool,
}

#[derive(sqlx::FromRow)]
struct OpportunitySummaryRow {
    id: String,
    title: String,
    state: String,
    revision: i64,
    participants: String,
    tags: String,
    next_task: Option<String>,
    next_deadline: Option<String>,
    missing_material: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OpportunityTagsInput {
    expected_revision: i64,
    tags: Vec<String>,
}

async fn set_opportunity_tags(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<OpportunityTagsInput>,
) -> Response {
    if let Err(e) = authenticate(&headers, &state).await {
        return (e.0, e.1).into_response();
    }
    if input.tags.len() > 100
        || input
            .tags
            .iter()
            .any(|t| t.trim().is_empty() || t.trim().len() > 120)
    {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "TAGS_INVALID",
            "tags must be nonempty and at most 120 bytes",
        );
    }
    let mut tags = input
        .tags
        .iter()
        .map(|t| t.trim().to_lowercase())
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    if tags.len() != input.tags.len() {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "TAGS_DUPLICATE",
            "tags must be unique after normalization",
        );
    }
    let key = match headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 200)
    {
        Some(k) => k.to_owned(),
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "IDEMPOTENCY_REQUIRED",
                "Idempotency-Key is required",
            );
        }
    };
    let canonical = materials_canonical(&format!("/api/v1/opportunities/{id}/tags"), &input);
    let epoch = state.workspace.epoch().to_owned();
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(e) => return workspace_error(e.into()),
    };
    match material_receipt_replay(&mut tx, &epoch, &key, &canonical).await {
        Ok(Some(v)) => {
            let _ = tx.commit().await;
            let mut r = Json(v).into_response();
            super::private_headers(&mut r, state.workspace.epoch());
            return r;
        }
        Err(e) => return workspace_error(e),
        _ => {}
    }
    let current: Option<i64> =
        match sqlx::query_scalar("SELECT revision FROM opportunity WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(v) => v,
            Err(e) => return workspace_error(e.into()),
        };
    let Some(revision) = current else {
        return error_response(StatusCode::NOT_FOUND, "NOT_FOUND", "opportunity not found");
    };
    if revision != input.expected_revision {
        return error_response(
            StatusCode::CONFLICT,
            "REVISION_CONFLICT",
            "opportunity revision changed",
        );
    }
    let next = revision + 1;
    if let Err(e) =
        sqlx::query("UPDATE opportunity SET revision=?,updated_at=? WHERE id=? AND revision=?")
            .bind(next)
            .bind(now_utc())
            .bind(&id)
            .bind(revision)
            .execute(&mut *tx)
            .await
    {
        return workspace_error(e.into());
    }
    if let Err(e) = sqlx::query("DELETE FROM opportunity_tag WHERE opportunity_id=?")
        .bind(&id)
        .execute(&mut *tx)
        .await
    {
        return workspace_error(e.into());
    }
    for tag in &tags {
        if let Err(e) = sqlx::query("INSERT INTO opportunity_tag(opportunity_id,tag) VALUES(?,?)")
            .bind(&id)
            .bind(tag)
            .execute(&mut *tx)
            .await
        {
            return workspace_error(e.into());
        }
    }
    if let Err(e) = event(
        &mut tx,
        "opportunity",
        &id,
        next,
        "opportunity.tags_changed",
        json!({"tags":tags}),
    )
    .await
    {
        return workspace_error(e);
    }
    let result = json!({"id":id,"revision":next,"tags":tags});
    if let Err(e) = material_receipt_store(&mut tx, &epoch, &key, &canonical, 200, &result).await {
        return workspace_error(e);
    }
    if let Err(e) = tx.commit().await {
        return workspace_error(e.into());
    }
    let mut r = Json(result).into_response();
    super::private_headers(&mut r, state.workspace.epoch());
    r
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecipientQuery {
    recipient_id: String,
}

async fn get_interests(
    State(state): State<AppState>,
    Path(opportunity_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<RecipientQuery>,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    let participant: Option<i64> = match sqlx::query_scalar(
        "SELECT count(*) FROM opportunity_party WHERE opportunity_id=? AND party_id=?",
    )
    .bind(&opportunity_id)
    .bind(&query.recipient_id)
    .fetch_optional(state.workspace.pool())
    .await
    {
        Ok(value) => value,
        Err(error) => return workspace_error(error.into()),
    };
    if participant != Some(1) {
        return error_response(
            StatusCode::NOT_FOUND,
            "recipient_not_found",
            "recipient is not a participant in this opportunity",
        );
    }
    let rows=sqlx::query_as::<_,(String,String,String,String,Option<String>,Option<String>,Option<i64>,i64,String,Option<String>,Option<i64>,Option<String>)>("SELECT i.id,i.side,i.statement,i.knowledge_kind,i.check_suggestion,i.claim_id,i.claim_revision,i.revision,i.status,cr.wording,cr.tier,cr.speaker_kind FROM opportunity_interest i LEFT JOIN claim_revision cr ON cr.claim_id=i.claim_id AND cr.revision=i.claim_revision WHERE i.opportunity_id=? AND i.party_id=? ORDER BY i.created_at,i.id").bind(&opportunity_id).bind(&query.recipient_id).fetch_all(state.workspace.pool()).await;
    match rows{Ok(rows)=>Json(json!({"opportunity_id":opportunity_id,"recipient_id":query.recipient_id,"items":rows.into_iter().map(|(id,side,statement,knowledge_kind,check_suggestion,claim_id,claim_revision,revision,status,wording,tier,speaker_kind)|json!({"id":id,"side":side,"statement":statement,"knowledge_kind":knowledge_kind,"check_suggestion":check_suggestion,"claim_id":claim_id,"claim_revision":claim_revision,"revision":revision,"status":status,"claim":claim_id.zip(claim_revision).map(|(claim_id,claim_revision)|json!({"claim_id":claim_id,"revision":claim_revision,"wording":wording,"tier":tier,"speaker_kind":speaker_kind}))})).collect::<Vec<_>>()})).into_response(),Err(error)=>workspace_error(error.into())}
}

async fn get_exchange(
    State(state): State<AppState>,
    Path(opportunity_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<RecipientQuery>,
) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    let participant: Option<i64> = match sqlx::query_scalar(
        "SELECT count(*) FROM opportunity_party WHERE opportunity_id=? AND party_id=?",
    )
    .bind(&opportunity_id)
    .bind(&query.recipient_id)
    .fetch_optional(state.workspace.pool())
    .await
    {
        Ok(value) => value,
        Err(error) => return workspace_error(error.into()),
    };
    if participant != Some(1) {
        return error_response(
            StatusCode::NOT_FOUND,
            "recipient_not_found",
            "recipient is not a participant in this opportunity",
        );
    }
    let versions=sqlx::query_as::<_,(i64,String,String,i64,Option<String>,Option<String>)>("SELECT version_no,they_get,i_get,fairness_attested,attestation_digest,attested_at FROM exchange_version WHERE opportunity_id=? AND recipient_id=? ORDER BY version_no").bind(&opportunity_id).bind(&query.recipient_id).fetch_all(state.workspace.pool()).await;
    let versions = match versions {
        Ok(value) => value,
        Err(error) => return workspace_error(error.into()),
    };
    let mut results = Vec::new();
    for (version, they_get, i_get, fairness_attested, attestation_digest, attested_at) in versions {
        let interests=sqlx::query_as::<_,(String,String,String,String)>("SELECT i.id,i.side,i.statement,i.knowledge_kind FROM exchange_interest_link l JOIN opportunity_interest i ON i.id=l.interest_id WHERE l.opportunity_id=? AND l.recipient_id=? AND l.exchange_version=? ORDER BY i.side,i.id").bind(&opportunity_id).bind(&query.recipient_id).bind(version).fetch_all(state.workspace.pool()).await;
        let interests = match interests {
            Ok(value) => value,
            Err(error) => return workspace_error(error.into()),
        };
        results.push(json!({"version":version,"they_get":they_get,"i_get":i_get,"fairness_attested":fairness_attested==1,"attestation_digest":attestation_digest,"attested_at":attested_at,"interest_ids":interests.iter().map(|(id,_,_,_)|id).collect::<Vec<_>>(),"interests":interests.into_iter().map(|(id,side,statement,knowledge_kind)|json!({"id":id,"side":side,"statement":statement,"knowledge_kind":knowledge_kind})).collect::<Vec<_>>()}));
    }
    let current = results.last().cloned();
    Json(json!({"opportunity_id":opportunity_id,"recipient_id":query.recipient_id,"current":current,"versions":results})).into_response()
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
    let key = match super::idempotency_key(&headers) {
        Ok(value) => value,
        Err(error) => {
            return (
                error.0,
                Json(json!({"error":{"code":error.1,"message":error.2}})),
            )
                .into_response();
        }
    };
    let canonical = materials_canonical(
        &format!("POST /api/v1/opportunities/{id}/interests"),
        &input,
    );
    match state
        .workspace
        .add_interest(&id, &input, &key, &canonical)
        .await
    {
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
    let key = match super::idempotency_key(&headers) {
        Ok(value) => value,
        Err(error) => {
            return (
                error.0,
                Json(json!({"error":{"code":error.1,"message":error.2}})),
            )
                .into_response();
        }
    };
    let canonical =
        materials_canonical(&format!("POST /api/v1/opportunities/{id}/exchange"), &input);
    match state
        .workspace
        .save_exchange(&id, &input, &key, &canonical)
        .await
    {
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
    let key = match super::idempotency_key(&headers) {
        Ok(value) => value,
        Err(error) => {
            return (
                error.0,
                Json(json!({"error":{"code":error.1,"message":error.2}})),
            )
                .into_response();
        }
    };
    let canonical = materials_canonical(
        &format!("POST /api/v1/opportunities/{id}/exchange/{recipient_id}/attest"),
        &json!({"recipient_id":recipient_id}),
    );
    match state
        .workspace
        .attest_exchange(&id, &recipient_id, &key, &canonical)
        .await
    {
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
    let key = match super::idempotency_key(&headers) {
        Ok(value) => value,
        Err(error) => {
            return (
                error.0,
                Json(json!({"error":{"code":error.1,"message":error.2}})),
            )
                .into_response();
        }
    };
    let canonical = materials_canonical("POST /api/v1/templates", &input);
    match state
        .workspace
        .create_template(&input, &key, &canonical)
        .await
    {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn list_templates(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match sqlx::query_as::<_, (String,String,String,i64,String,String,String,String,String,Option<String>)>("SELECT tv.id,tv.family_id,tv.display_name,tv.version_no,tv.status,tv.body,tv.rules_json,tv.stage_class,tv.channel,tv.parent_version_id FROM template_version tv ORDER BY tv.created_at DESC,tv.id").fetch_all(state.workspace.pool()).await {
        Ok(rows) => {
            let items:Result<Vec<Value>,serde_json::Error>=rows.into_iter().map(|(id,family_id,name,version,status,body,rules_json,stage_class,channel,parent_version_id)|Ok(json!({"id":id,"family_id":family_id,"name":name,"version":version,"status":status,"body":body,"sentence_rules":serde_json::from_str::<Value>(&rules_json)?,"stage_class":stage_class,"channel":channel,"parent_version_id":parent_version_id}))).collect();
            match items{Ok(value)=>(StatusCode::OK,Json(value)).into_response(),Err(error)=>error_response(StatusCode::INTERNAL_SERVER_ERROR,"template_corrupt",&error.to_string())}
        },
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
    let key = match super::idempotency_key(&headers) {
        Ok(value) => value,
        Err(error) => {
            return (
                error.0,
                Json(json!({"error":{"code":error.1,"message":error.2}})),
            )
                .into_response();
        }
    };
    let canonical = materials_canonical(
        &format!("POST /api/v1/templates/{id}/approve"),
        &json!({"id":id,"to":"approved"}),
    );
    match state
        .workspace
        .transition_template(&id, "draft", "approved", &key, &canonical)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
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
    let key = match super::idempotency_key(&headers) {
        Ok(value) => value,
        Err(error) => {
            return (
                error.0,
                Json(json!({"error":{"code":error.1,"message":error.2}})),
            )
                .into_response();
        }
    };
    let canonical = materials_canonical(
        &format!("POST /api/v1/templates/{id}/activate"),
        &json!({"id":id,"to":"active"}),
    );
    match state
        .workspace
        .transition_template(&id, "approved", "active", &key, &canonical)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
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

async fn evaluate_material_gate(
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
        .evaluate_material_gate(&id, &input.override_reasons)
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
    async fn evaluate_material_gate(
        &self,
        id: &str,
        overrides: &BTreeMap<String, String>,
    ) -> Result<gate::GateResult, WorkspaceError> {
        validate_override_reasons(overrides)?;
        let mut tx = self.pool.begin().await?;
        let material = load_head(&mut tx, id).await?;
        // Gate preview (also used by approval and copy) evaluates the hard and
        // soft content gates, not the send-time schedule gates (S7 cadence,
        // S8 daily cap) which only Mark sent may apply.
        let result = evaluate_material(&mut tx, &material, overrides, false).await?;
        tx.commit().await?;
        Ok(result)
    }

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
        let party_kind: Option<String> = sqlx::query_scalar(
            "SELECT kind FROM graph_card WHERE id=? AND kind IN ('person','org')",
        )
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

    pub(super) async fn opportunity_summaries(
        &self,
        f: &OpportunityFilters,
    ) -> Result<Vec<OpportunitySummary>, WorkspaceError> {
        if f.status
            .as_deref()
            .is_some_and(|s| !matches!(s, "open" | "resolved" | "dormant" | "closed"))
            || f.sort
                .as_deref()
                .is_some_and(|s| !matches!(s, "title" | "next_task" | "deadline"))
            || f.order
                .as_deref()
                .is_some_and(|s| !matches!(s, "asc" | "desc"))
        {
            return Err(WorkspaceError::Invalid(
                "invalid opportunity filter or sort".into(),
            ));
        }
        let mut q = QueryBuilder::<Sqlite>::new(
            "SELECT o.id,o.title,o.state,o.revision, \
          (SELECT json_group_array(json_object('id',e.id,'name',e.name,'kind',e.kind)) FROM opportunity_party op JOIN entity e ON e.id=op.party_id WHERE op.opportunity_id=o.id ORDER BY op.role,e.id) AS participants, \
          (SELECT json_group_array(tag) FROM opportunity_tag ot WHERE ot.opportunity_id=o.id ORDER BY tag) AS tags, \
          o.notes_next_task AS next_task, (SELECT min(t.expires_at) FROM contact_thread t WHERE t.opportunity_id=o.id AND t.state='waiting') AS next_deadline, \
          NOT EXISTS(SELECT 1 FROM material m WHERE m.opportunity_id=o.id) AS missing_material \
          FROM opportunity o WHERE 1=1",
        );
        if let Some(s) = &f.status {
            q.push(" AND o.state=").push_bind(s);
        }
        if let Some(term) = f.q.as_deref().filter(|s| !s.trim().is_empty()) {
            let like = format!("%{}%", term.trim());
            q.push(" AND (o.title LIKE ").push_bind(like.clone()).push(" OR EXISTS(SELECT 1 FROM opportunity_party x JOIN entity e ON e.id=x.party_id WHERE x.opportunity_id=o.id AND e.name LIKE ").push_bind(like).push(")");
        }
        if !f.party_id.is_empty() {
            q.push(" AND EXISTS(SELECT 1 FROM opportunity_party x WHERE x.opportunity_id=o.id AND x.party_id IN (");
            for (i, id) in f.party_id.iter().enumerate() {
                if i > 0 {
                    q.push(",");
                }
                q.push_bind(id);
            }
            q.push("))");
        }
        if !f.tag.is_empty() {
            q.push(" AND EXISTS(SELECT 1 FROM opportunity_tag x WHERE x.opportunity_id=o.id AND x.tag IN (");
            for (i, t) in f.tag.iter().enumerate() {
                if i > 0 {
                    q.push(",");
                }
                q.push_bind(t);
            }
            q.push("))");
        }
        if let Some(missing) = f.missing_material {
            q.push(if missing {
                " AND NOT EXISTS(SELECT 1 FROM material m WHERE m.opportunity_id=o.id)"
            } else {
                " AND EXISTS(SELECT 1 FROM material m WHERE m.opportunity_id=o.id)"
            });
        }
        if let Some(before) = &f.deadline_before {
            q.push(" AND EXISTS(SELECT 1 FROM contact_thread t WHERE t.opportunity_id=o.id AND t.state='waiting' AND t.expires_at<=").push_bind(before);
        }
        // Close the deadline predicate only when present (kept explicit to avoid
        // accidentally broadening the query on malformed dates).
        if f.deadline_before.is_some() {
            q.push(")");
        }
        let sort = match f.sort.as_deref() {
            Some("title") => "o.title",
            Some("next_task") => "o.notes_next_task",
            Some("deadline") => {
                "(SELECT min(t.expires_at) FROM contact_thread t WHERE t.opportunity_id=o.id AND t.state='waiting')"
            }
            _ => "o.updated_at",
        };
        q.push(" ORDER BY ")
            .push(sort)
            .push(if f.order.as_deref() == Some("desc") {
                " DESC"
            } else {
                " ASC"
            })
            .push(",o.id ASC");
        let rows: Vec<OpportunitySummaryRow> = q.build_query_as().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let participants = serde_json::from_str(&row.participants).map_err(|e| {
                    WorkspaceError::Invalid(format!("participant projection invalid: {e}"))
                })?;
                let tags = serde_json::from_str(&row.tags).unwrap_or_default();
                Ok(OpportunitySummary {
                    id: row.id,
                    title: row.title,
                    state: row.state,
                    revision: row.revision,
                    participants,
                    tags,
                    next_task: row.next_task,
                    next_deadline: row.next_deadline,
                    missing_material: row.missing_material != 0,
                })
            })
            .collect()
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
        let kind: Option<String> = sqlx::query_scalar(
            "SELECT kind FROM graph_card WHERE id=? AND kind IN ('person','org')",
        )
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
        idempotency_key: &str,
        canonical: &str,
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
        if let Some(value) =
            material_receipt_replay(&mut tx, &self.epoch, idempotency_key, canonical).await?
        {
            tx.commit().await?;
            return Ok(value);
        }
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
        let result = json!({"id":id,"opportunity_id":opportunity_id,"recipient_id":input.recipient_id,"side":input.side,"statement":input.statement,"knowledge_kind":input.knowledge_kind,"check_suggestion":input.check_suggestion,"revision":1,"status":"active"});
        material_receipt_store(
            &mut tx,
            &self.epoch,
            idempotency_key,
            canonical,
            201,
            &result,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn save_exchange(
        &self,
        opportunity_id: &str,
        input: &ExchangeInput,
        idempotency_key: &str,
        canonical: &str,
    ) -> Result<Value, WorkspaceError> {
        if input.they_get.len() > 3000 || input.i_get.len() > 3000 {
            return Err(WorkspaceError::Invalid(
                "exchange text exceeds 3000 bytes".into(),
            ));
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(value) =
            material_receipt_replay(&mut tx, &self.epoch, idempotency_key, canonical).await?
        {
            tx.commit().await?;
            return Ok(value);
        }
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
        let result = json!({"opportunity_id":opportunity_id,"recipient_id":input.recipient_id,"version":next,"they_get":input.they_get,"i_get":input.i_get,"interest_ids":input.interest_ids,"fairness_attested":false});
        material_receipt_store(
            &mut tx,
            &self.epoch,
            idempotency_key,
            canonical,
            201,
            &result,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn attest_exchange(
        &self,
        opportunity_id: &str,
        recipient_id: &str,
        idempotency_key: &str,
        canonical: &str,
    ) -> Result<Value, WorkspaceError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(value) =
            material_receipt_replay(&mut tx, &self.epoch, idempotency_key, canonical).await?
        {
            tx.commit().await?;
            return Ok(value);
        }
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
        let result = json!({"opportunity_id":opportunity_id,"recipient_id":recipient_id,"version":version,"fairness_attested":true,"attestation_digest":current_digest});
        material_receipt_store(
            &mut tx,
            &self.epoch,
            idempotency_key,
            canonical,
            200,
            &result,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn create_template(
        &self,
        input: &TemplateInput,
        idempotency_key: &str,
        canonical: &str,
    ) -> Result<Value, WorkspaceError> {
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
        if let Some(value) =
            material_receipt_replay(&mut tx, &self.epoch, idempotency_key, canonical).await?
        {
            tx.commit().await?;
            return Ok(value);
        }
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
        let result = json!({"id":id,"family_id":family,"version":1,"status":"draft","body":input.body,"slots":checked.slots});
        material_receipt_store(
            &mut tx,
            &self.epoch,
            idempotency_key,
            canonical,
            201,
            &result,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn transition_template(
        &self,
        id: &str,
        from: &str,
        to: &str,
        idempotency_key: &str,
        canonical: &str,
    ) -> Result<Value, WorkspaceError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(value) =
            material_receipt_replay(&mut tx, &self.epoch, idempotency_key, canonical).await?
        {
            tx.commit().await?;
            return Ok(value);
        }
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
        let result = json!({"id":id,"status":to});
        material_receipt_store(
            &mut tx,
            &self.epoch,
            idempotency_key,
            canonical,
            204,
            &Value::Null,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
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
            let attachment = if let Some(attachment_input) = &input.attachment {
                let row = sqlx::query_as::<_, (String, String, String, i64, i64)>(
                    "SELECT u.blob_sha256,u.filename,u.mime,b.size,u.revision FROM blob_upload u JOIN blob b ON b.sha256=u.blob_sha256 WHERE u.id=? AND u.reviewed_by_owner=1 AND u.share_permission=1",
                )
                .bind(&attachment_input.upload_id)
                .fetch_optional(&mut *tx)
                .await?;
                let Some((sha256, filename, mime, size, _revision)) = row else {
                    return Err(WorkspaceError::Invalid(
                        "attachment requires a reviewed, shareable upload".into(),
                    ));
                };
                Some(AttachmentInfo {
                    upload_id: attachment_input.upload_id.clone(),
                    sha256,
                    filename,
                    content_type: mime,
                    size,
                })
            } else {
                None
            };
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
            let hash = attachment
                .as_ref()
                .map(|attachment| attachment.sha256.clone())
                .unwrap_or_else(|| digest(input.body.as_bytes()));
            sqlx::query("INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at,blob_upload_id,blob_sha256,filename,content_type,attachment_size) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
                .bind(&material_id).bind(version).bind(&opportunity_id).bind(&input.recipient_id).bind(&input.template_version_id).bind(input.exchange_version).bind(&input.body).bind(slot_json).bind(bindings_json).bind(&hash).bind(&version_now)
                .bind(attachment.as_ref().map(|a| a.upload_id.as_str()))
                .bind(attachment.as_ref().map(|a| a.sha256.as_str()))
                .bind(attachment.as_ref().map(|a| a.filename.as_str()))
                .bind(attachment.as_ref().map(|a| a.content_type.as_str()))
                .bind(attachment.as_ref().map(|a| a.size))
                .execute(&mut *tx).await?;
            persist_material_references(
                &mut tx,
                &material_id,
                version,
                &input.slot_fills,
                &input.sentence_bindings,
                &input.body,
                &input.recipient_id,
                attachment.as_ref(),
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
        let row = sqlx::query_as::<_,(String,String,String,String,String,i64,String,String,Option<String>,Option<String>,Option<String>,Option<String>,Option<i64>)>("SELECT m.id,m.opportunity_id,m.recipient_id,e.name,m.kind,mv.version_no,mv.body,mv.content_sha256,mv.blob_upload_id,mv.blob_sha256,mv.filename,mv.content_type,mv.attachment_size FROM material m JOIN entity e ON e.id=m.recipient_id JOIN material_version mv ON mv.material_id=m.id AND mv.version_no=m.head_version WHERE m.id=?")
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
            blob_upload_id,
            blob_sha256,
            filename,
            content_type,
            attachment_size,
        )) = row
        else {
            return Ok(None);
        };
        let approved_gate: Option<String> = sqlx::query_scalar(
            "SELECT gate_snapshot_json FROM approval WHERE material_id=? AND version_no=?",
        )
        .bind(&id)
        .bind(version)
        .fetch_optional(&self.pool)
        .await?;
        let released: Option<String> = sqlx::query_scalar("SELECT id FROM release_record WHERE material_id=? AND version_no=? ORDER BY issued_at DESC LIMIT 1").bind(&id).bind(version).fetch_optional(&self.pool).await?;
        let status = if released.is_some() {
            "released"
        } else if approved_gate.is_some() {
            "approved"
        } else {
            "draft"
        };
        let attachment = match (
            blob_upload_id,
            blob_sha256,
            filename,
            content_type,
            attachment_size,
        ) {
            (Some(upload_id), Some(sha256), Some(filename), Some(content_type), Some(size)) => {
                Some(AttachmentInfo {
                    upload_id,
                    sha256,
                    filename,
                    content_type,
                    size,
                })
            }
            (None, None, None, None, None) => None,
            _ => {
                return Err(WorkspaceError::Invalid(
                    "material attachment metadata is incomplete".into(),
                ));
            }
        };
        let gate = approved_gate
            .map(|json| {
                serde_json::from_str(&json)
                    .map_err(|error| WorkspaceError::Invalid(error.to_string()))
            })
            .transpose()?;
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
            gate,
            attachment,
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
        let checked = evaluate_material(&mut tx, &material, overrides, false).await?;
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

#[expect(
    clippy::too_many_arguments,
    reason = "the reference set covers slots, bindings, content and the attachment in one revision-bound command"
)]
async fn persist_material_references(
    tx: &mut Transaction<'_, Sqlite>,
    material_id: &str,
    version: i64,
    fills: &[SlotFill],
    bindings: &[SentenceBinding],
    body: &str,
    recipient: &str,
    attachment: Option<&AttachmentInfo>,
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
    if let Some(attachment) = attachment {
        let revision: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM blob_upload WHERE id=? AND reviewed_by_owner=1 AND share_permission=1",
        )
        .bind(&attachment.upload_id)
        .fetch_optional(&mut **tx)
        .await?;
        let Some(revision) = revision else {
            return Err(WorkspaceError::Invalid(
                "attachment upload must be reviewed and shareable".into(),
            ));
        };
        sqlx::query("INSERT INTO version_dependency(material_id,version_no,kind,source_id,source_revision) VALUES(?,?,'blob',?,?)")
            .bind(material_id)
            .bind(version)
            .bind(&attachment.upload_id)
            .bind(revision.to_string())
            .execute(&mut **tx)
            .await?;
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
            "custom" | "capacity_line" | "jargon_term" | "identity_link" | "hook"
                if fill.resource_id.is_none() => {}
            _ => {
                return Err(WorkspaceError::Invalid(format!(
                    "slot resolver for '{kind}' is unavailable or its value does not match the saved record"
                )));
            }
        }
    }
    Ok(())
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

pub(crate) async fn event(
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

pub(crate) fn validate_binding_shape(
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

pub(crate) fn split_sentences(body: &str) -> Vec<String> {
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

pub(crate) fn validate_override_reasons(
    reasons: &BTreeMap<String, String>,
) -> Result<(), WorkspaceError> {
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
