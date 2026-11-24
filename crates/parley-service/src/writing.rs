//! S09 authoring commands. Deterministic ranking and lint preparation happen
//! here; local model roles return proposal data through the durable worker.

use super::{AppState, WorkspaceError, authenticate, command_error, digest, error, now_utc};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use parley_domain::{lint, template};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, Sqlite, Transaction};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/opportunities/{id}/matches",
            post(start_match).get(get_match),
        )
        .route("/api/v1/templates/build", post(build_templates))
        .route(
            "/api/v1/template-families",
            get(list_template_families).post(create_template_family),
        )
        .route(
            "/api/v1/matching/calibrations",
            get(get_calibrations).post(reject_client_calibration),
        )
        .route(
            "/api/v1/embedding-model",
            get(get_embedding_model).put(set_embedding_model),
        )
        .route(
            "/api/v1/voice-samples",
            get(list_voice_samples).post(create_voice_sample),
        )
        .route(
            "/api/v1/voice-samples/{id}/retire",
            post(retire_voice_sample),
        )
        .route(
            "/api/v1/templates/{family_id}/versions",
            post(create_template_version),
        )
        .route(
            "/api/v1/opportunities/{id}/materials/compose",
            post(compose_material),
        )
        .route(
            "/api/v1/opportunities/{id}/materials/compose/{job_id}/save",
            post(save_composer_candidate),
        )
        .route("/api/v1/materials/{id}/lint", post(lint_material))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingModelRequest {
    model: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct VoiceSampleRequest {
    text: String,
    channel: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CalibrationRequest {
    asset_kind: parley_domain::matching::AssetKind,
    fixture_id: String,
    samples: Vec<parley_domain::matching::LabeledScore>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TemplateFamilyRequest {
    name: String,
}

async fn list_template_families(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT id,name,created_at FROM template_family ORDER BY name COLLATE NOCASE,id",
    )
    .fetch_all(state.workspace.pool())
    .await;
    match rows{Ok(rows)=>Json(json!({"items":rows.into_iter().map(|(id,name,created_at)|json!({"id":id,"name":name,"created_at":created_at})).collect::<Vec<_>>()})).into_response(),Err(error)=>command_error(error.into())}
}

async fn create_template_family(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<TemplateFamilyRequest>,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    if input.name.trim().is_empty() || input.name.len() > 200 {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "TEMPLATE_FAMILY_INVALID",
            "Template family name must contain 1–200 bytes.",
        );
    }
    let key = match command_key(&headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let canonical = match canonical_command("POST /api/v1/template-families", &input) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(value) => value,
        Err(error) => return command_error(error.into()),
    };
    match receipt_replay(&mut tx, &state.workspace, &key, &canonical).await {
        Ok(Some(response)) => {
            let _ = tx.commit().await;
            return response;
        }
        Ok(None) => {}
        Err(response) => {
            let _ = tx.rollback().await;
            return response;
        }
    }
    let id = super::new_id("template-family");
    let now = now_utc();
    if let Err(error) = sqlx::query("INSERT INTO template_family(id,name,created_at) VALUES(?,?,?)")
        .bind(&id)
        .bind(input.name.trim())
        .bind(&now)
        .execute(&mut *tx)
        .await
    {
        let _ = tx.rollback().await;
        return command_error(error.into());
    }
    let revision: Result<i64, sqlx::Error> = sqlx::query_scalar(
        "UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1 RETURNING revision",
    )
    .fetch_one(&mut *tx)
    .await;
    let revision = match revision {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return command_error(error.into());
        }
    };
    if let Err(error)=sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'template_family',?,?, 'template_family.created',?)").bind(super::new_id(&id)).bind(&now).bind(&id).bind(revision).bind(json!({"name":input.name.trim()}).to_string()).execute(&mut *tx).await{let _=tx.rollback().await;return command_error(error.into());}
    let result = json!({"id":id,"name":input.name.trim(),"created_at":now});
    if let Err(error) = receipt_store(
        &mut tx,
        &state.workspace,
        &key,
        &canonical,
        StatusCode::CREATED,
        &result,
    )
    .await
    {
        let _ = tx.rollback().await;
        return command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return command_error(error.into());
    }
    (StatusCode::CREATED, Json(result)).into_response()
}

fn scoring_config_digest(weights: &Value, half_life: f64) -> Result<String, String> {
    let bytes = serde_json::to_vec(
        &json!({"weights":weights,"proof_half_life_months":half_life,"scoring_revision":1}),
    )
    .map_err(|error| error.to_string())?;
    Ok(digest(&bytes))
}

async fn get_calibrations(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let provider: Result<(String, Option<String>), sqlx::Error> = sqlx::query_as(
        "SELECT embed_model,embed_model_digest FROM agent_provider WHERE singleton=1",
    )
    .fetch_one(state.workspace.pool())
    .await;
    let (model, model_digest) = match provider {
        Ok(value) => value,
        Err(error) => return command_error(error.into()),
    };
    let settings_json: String =
        match sqlx::query_scalar("SELECT settings_json FROM owner_profile WHERE id=1")
            .fetch_one(state.workspace.pool())
            .await
        {
            Ok(value) => value,
            Err(error) => return command_error(error.into()),
        };
    let settings: Value = match serde_json::from_str(&settings_json) {
        Ok(value) => value,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "OWNER_SETTINGS_CORRUPT",
                &error.to_string(),
            );
        }
    };
    let half_life = settings
        .pointer("/pack/limits/proof_half_life_months")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(24.0);
    let weights = settings
        .pointer("/matcher/weights")
        .cloned()
        .unwrap_or(json!({"cosine":0.5,"tags":0.25,"recency":0.15,"track":0.1}));
    let config_digest = match scoring_config_digest(&weights, half_life) {
        Ok(value) => value,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "MATCH_CONFIG_SERIALIZE_FAILED",
                &error,
            );
        }
    };
    let rows=sqlx::query_as::<_,(String,f64,String,i64,i64,i64,i64,String)>("SELECT asset_kind,threshold,fixture_digest,positives,negatives,true_positives,false_positives,scoring_config_digest FROM matching_calibration WHERE trusted=1 AND model_digest=? AND scoring_config_digest=? AND calibration_revision=(SELECT max(c2.calibration_revision) FROM matching_calibration c2 WHERE c2.trusted=1 AND c2.model_digest=matching_calibration.model_digest AND c2.asset_kind=matching_calibration.asset_kind AND c2.scoring_config_digest=matching_calibration.scoring_config_digest) ORDER BY asset_kind").bind(model_digest.clone().unwrap_or_default()).bind(config_digest.clone()).fetch_all(state.workspace.pool()).await;
    match rows{Ok(rows)=>Json(json!({"model":model,"model_digest":model_digest,"scoring_config_digest":config_digest,"calibrations":rows.into_iter().map(|(kind,threshold,fixture_digest,positives,negatives,true_positives,false_positives,_)|json!({"asset_kind":kind,"threshold":threshold,"fixture_digest":fixture_digest,"positives":positives,"negatives":negatives,"true_positives":true_positives,"false_positives":false_positives,"trusted":true})).collect::<Vec<_>>()})).into_response(),Err(error)=>command_error(error.into())}
}

async fn reject_client_calibration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(_input): Json<CalibrationRequest>,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    err(
        StatusCode::FORBIDDEN,
        "CALIBRATION_REQUIRES_EVAL",
        "Calibration scores are accepted only from a successful local E-match evaluation; client-supplied labels are not evaluation evidence.",
    )
}

async fn get_embedding_model(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let row: Result<(String, Option<String>), sqlx::Error> = sqlx::query_as(
        "SELECT embed_model,embed_model_digest FROM agent_provider WHERE singleton=1",
    )
    .fetch_one(state.workspace.pool())
    .await;
    let (model, digest) = match row {
        Ok(value) => value,
        Err(error) => return command_error(error.into()),
    };
    let calibrated: Result<Vec<String>,sqlx::Error>=sqlx::query_scalar("SELECT DISTINCT asset_kind FROM matching_calibration WHERE trusted=1 AND model_digest=? ORDER BY asset_kind").bind(digest.clone().unwrap_or_default()).fetch_all(state.workspace.pool()).await;
    match calibrated {
        Ok(kinds) => Json(json!({"model":model,"model_digest":digest,"calibrated_kinds":kinds}))
            .into_response(),
        Err(error) => command_error(error.into()),
    }
}

async fn set_embedding_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<EmbeddingModelRequest>,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let key = match command_key(&headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let canonical = match canonical_command("PUT /api/v1/embedding-model", &input) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if input.model.trim().is_empty() || input.model.len() > 200 {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "EMBED_MODEL_INVALID",
            "Select an installed local embedding model.",
        );
    }
    let config: Result<String, sqlx::Error> =
        sqlx::query_scalar("SELECT endpoint FROM agent_provider WHERE singleton=1")
            .fetch_one(state.workspace.pool())
            .await;
    let endpoint = match config {
        Ok(value) => value,
        Err(error) => return command_error(error.into()),
    };
    let digest = match local_model_digest(&endpoint, &input.model).await {
        Ok(value) => value,
        Err(error) => {
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                "EMBED_MODEL_HEALTH_FAILED",
                &error,
            );
        }
    };
    let now = now_utc();
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(value) => value,
        Err(error) => return command_error(error.into()),
    };
    match receipt_replay(&mut tx, &state.workspace, &key, &canonical).await {
        Ok(Some(response)) => {
            let _ = tx.commit().await;
            return response;
        }
        Ok(None) => {}
        Err(response) => {
            let _ = tx.rollback().await;
            return response;
        }
    }
    if let Err(error)=sqlx::query("UPDATE agent_provider SET embed_model=?,embed_model_digest=?,updated_at=? WHERE singleton=1").bind(&input.model).bind(&digest).bind(&now).execute(&mut *tx).await{let _=tx.rollback().await;return command_error(error.into());}
    let revision: Result<i64, sqlx::Error> = sqlx::query_scalar(
        "UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1 RETURNING revision",
    )
    .fetch_one(&mut *tx)
    .await;
    let revision = match revision {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return command_error(error.into());
        }
    };
    if let Err(error)=sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'embedding_model','workspace',?, 'embedding_model.changed',?)").bind(super::new_id(&digest)).bind(&now).bind(revision).bind(json!({"model":input.model,"model_digest":digest}).to_string()).execute(&mut *tx).await{let _=tx.rollback().await;return command_error(error.into());}
    let response_body =
        json!({"model":input.model,"model_digest":digest,"calibration_required":true});
    if let Err(error) = receipt_store(
        &mut tx,
        &state.workspace,
        &key,
        &canonical,
        StatusCode::OK,
        &response_body,
    )
    .await
    {
        let _ = tx.rollback().await;
        return command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return command_error(error.into());
    }
    (StatusCode::OK, Json(response_body)).into_response()
}

async fn list_voice_samples(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let rows = sqlx::query_as::<_, (String, String, Option<String>, i64, String)>(
        "SELECT id,text,channel,revision,updated_at FROM voice_sample ORDER BY created_at,id",
    )
    .fetch_all(state.workspace.pool())
    .await;
    match rows{Ok(rows)=>Json(json!({"items":rows.into_iter().map(|(id,text,channel,revision,updated_at)|json!({"id":id,"text":text,"channel":channel,"revision":revision,"updated_at":updated_at})).collect::<Vec<_>>()})).into_response(),Err(error)=>command_error(error.into())}
}

async fn create_voice_sample(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<VoiceSampleRequest>,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let key = match command_key(&headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let canonical = match canonical_command("POST /api/v1/voice-samples", &input) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if input.text.trim().is_empty()
        || input.text.len() > 20_000
        || input
            .channel
            .as_ref()
            .is_some_and(|channel| channel.trim().is_empty() || channel.len() > 80)
    {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VOICE_SAMPLE_INVALID",
            "Voice sample text must be 1–20,000 bytes and an optional channel must be nonempty.",
        );
    }
    let id = super::new_id("voice-sample");
    let now = now_utc();
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(value) => value,
        Err(error) => return command_error(error.into()),
    };
    match receipt_replay(&mut tx, &state.workspace, &key, &canonical).await {
        Ok(Some(response)) => {
            let _ = tx.commit().await;
            return response;
        }
        Ok(None) => {}
        Err(response) => {
            let _ = tx.rollback().await;
            return response;
        }
    }
    if let Err(error)=sqlx::query("INSERT INTO voice_sample(id,text,channel,revision,status,created_at,updated_at) VALUES(?,?,?,1,'active',?,?)").bind(&id).bind(&input.text).bind(&input.channel).bind(&now).bind(&now).execute(&mut *tx).await{let _=tx.rollback().await;return command_error(error.into());}
    let revision: Result<i64, sqlx::Error> = sqlx::query_scalar(
        "UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1 RETURNING revision",
    )
    .fetch_one(&mut *tx)
    .await;
    let revision = match revision {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return command_error(error.into());
        }
    };
    if let Err(error)=sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'voice_sample',?,?, 'voice_sample.created',?)").bind(super::new_id(&id)).bind(&now).bind(&id).bind(revision).bind(json!({"channel":input.channel,"text_sha256":digest(input.text.as_bytes())}).to_string()).execute(&mut *tx).await{let _=tx.rollback().await;return command_error(error.into());}
    let response_body = json!({"id":id,"text":input.text,"channel":input.channel,"revision":1,"status":"active","updated_at":now});
    if let Err(error) = receipt_store(
        &mut tx,
        &state.workspace,
        &key,
        &canonical,
        StatusCode::CREATED,
        &response_body,
    )
    .await
    {
        let _ = tx.rollback().await;
        return command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return command_error(error.into());
    }
    (StatusCode::CREATED, Json(response_body)).into_response()
}

async fn retire_voice_sample(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let key = match command_key(&headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let canonical = match canonical_command(
        &format!("POST /api/v1/voice-samples/{id}/retire"),
        &json!({"id":id}),
    ) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(value) => value,
        Err(error) => return command_error(error.into()),
    };
    match receipt_replay(&mut tx, &state.workspace, &key, &canonical).await {
        Ok(Some(response)) => {
            let _ = tx.commit().await;
            return response;
        }
        Ok(None) => {}
        Err(response) => {
            let _ = tx.rollback().await;
            return response;
        }
    }
    let now = now_utc();
    let changed=match sqlx::query("UPDATE voice_sample SET status='retired',revision=revision+1,updated_at=? WHERE id=? AND status='active'").bind(&now).bind(&id).execute(&mut *tx).await{Ok(value)=>value.rows_affected(),Err(error)=>{let _=tx.rollback().await;return command_error(error.into())}};
    if changed == 0 {
        let _ = tx.rollback().await;
        return err(
            StatusCode::NOT_FOUND,
            "VOICE_SAMPLE_NOT_FOUND",
            "Active voice sample does not exist.",
        );
    }
    let revision: Result<i64, sqlx::Error> = sqlx::query_scalar(
        "UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1 RETURNING revision",
    )
    .fetch_one(&mut *tx)
    .await;
    let revision = match revision {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return command_error(error.into());
        }
    };
    if let Err(error)=sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'voice_sample',?,?, 'voice_sample.retired','{}')").bind(super::new_id(&id)).bind(&now).bind(&id).bind(revision).execute(&mut *tx).await{let _=tx.rollback().await;return command_error(error.into());}
    let body = json!({"id":id,"status":"retired","revision":revision});
    if let Err(error) = receipt_store(
        &mut tx,
        &state.workspace,
        &key,
        &canonical,
        StatusCode::OK,
        &body,
    )
    .await
    {
        let _ = tx.rollback().await;
        return command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return command_error(error.into());
    }
    (StatusCode::OK, Json(body)).into_response()
}

async fn local_model_digest(endpoint: &str, model: &str) -> Result<String, String> {
    let (host, port) = super::writing_jobs::local_endpoint_for_settings(endpoint)?;
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await
    .map_err(|_| "local model health check timed out".to_owned())?
    .map_err(|error| error.to_string())?;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream
        .write_all(
            format!("GET /api/tags HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    stream
        .take(1_048_576)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    let response = String::from_utf8(bytes).map_err(|error| error.to_string())?;
    let (head, payload) = response
        .split_once("\r\n\r\n")
        .ok_or("local model returned invalid HTTP")?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err("selected embedding model is unavailable locally".into());
    }
    let payload = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        super::writing_jobs::decode_chunks_for_settings(payload)?
    } else {
        payload.to_owned()
    };
    let value: Value = serde_json::from_str(&payload).map_err(|error| error.to_string())?;
    let models = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or("provider model catalog is invalid")?;
    let digest = models
        .iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(model))
        .and_then(|entry| entry.get("digest"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or("selected model is not installed or provider omitted its digest")?;
    Ok(digest.to_owned())
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MatchRequest {
    recipient_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchQuery {
    recipient_id: String,
}

#[derive(Debug, Serialize, FromRow)]
struct MatchRunRow {
    id: String,
    opportunity_id: String,
    recipient_id: String,
    embed_model_digest: String,
    calibration_json: String,
    shortlist_json: String,
    why_job_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TemplateBuildRequest {
    segment_id: Option<String>,
    stage_class: String,
    rungs: Vec<u32>,
    rules: Vec<String>,
    channel: String,
    max_chars: usize,
    #[serde(default)]
    voice_sample_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TemplateVersionRequest {
    name: String,
    body: String,
    proof_fallback: Option<String>,
    stage_class: String,
    channel: String,
    sentence_rules: Vec<Vec<String>>,
    parent_version_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ComposeRequest {
    recipient_id: String,
    kind: String,
    template_version_id: String,
    exchange_version: i64,
    slot_fills: Vec<ComposeSlot>,
    #[serde(default)]
    selected_match_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SaveComposerRequest {
    body: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposeSlot {
    slot: String,
    value: String,
    resource_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LintRequest {
    version: i64,
    content_sha256: String,
}

async fn start_match(
    State(state): State<AppState>,
    Path(opportunity_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<MatchRequest>,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    if input.recipient_id.trim().is_empty() {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "MATCH_RECIPIENT_REQUIRED",
            "Select one recipient in this opportunity.",
        );
    }
    // Owner wire preconditions are resolved here so the published error codes
    // stay stable; the shared operation re-checks them under the transaction
    // and owns the canonical resource/recipient snapshot (UCR-001 U07).
    let revision: Option<i64> =
        match sqlx::query_scalar("SELECT revision FROM opportunity WHERE id=? AND state='open'")
            .bind(&opportunity_id)
            .fetch_optional(state.workspace.pool())
            .await
        {
            Ok(value) => value,
            Err(error) => return command_error(error.into()),
        };
    let Some(opportunity_revision) = revision else {
        return err(
            StatusCode::NOT_FOUND,
            "OPPORTUNITY_NOT_OPEN",
            "Matching requires an open opportunity.",
        );
    };
    let participant: Option<(String,i64)> = match sqlx::query_as("SELECT e.kind,e.revision FROM opportunity_party p JOIN entity e ON e.id=p.party_id WHERE p.opportunity_id=? AND p.party_id=?").bind(&opportunity_id).bind(&input.recipient_id).fetch_optional(state.workspace.pool()).await { Ok(value) => value, Err(error) => return command_error(error.into()) };
    let Some((kind, recipient_revision)) = participant else {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "RECIPIENT_NOT_PARTICIPANT",
            "Recipient must be an explicit participant in this opportunity.",
        );
    };
    if !matches!(kind.as_str(), "person" | "org") {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "RECIPIENT_KIND_INVALID",
            "Recipient must be a person or organization.",
        );
    }
    let key = match command_key(&headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let canonical = match canonical_command(
        &format!("POST /api/v1/opportunities/{opportunity_id}/matches"),
        &input,
    ) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return command_error(error.into()),
    };
    if let Some(response) = match receipt_replay(&mut tx, &state.workspace, &key, &canonical).await
    {
        Ok(response) => response,
        Err(response) => return response,
    } {
        let _ = tx.commit().await;
        return response;
    }
    let outcome = match super::application::match_start::start(
        &mut tx,
        &super::application::match_start::MatchStartInput {
            opportunity_id: opportunity_id.clone(),
            recipient_id: input.recipient_id.clone(),
            expected_opportunity_revision: opportunity_revision,
            expected_recipient_revision: recipient_revision,
            epoch: state.workspace.epoch().to_string(),
            run_id: None,
            owner_pseudonym: None,
            granted_targets: Vec::new(),
        },
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(super::application::match_start::MatchStartError::NotFound) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::NOT_FOUND,
                "OPPORTUNITY_NOT_OPEN",
                "Matching requires an open opportunity.",
            );
        }
        Err(super::application::match_start::MatchStartError::RecipientInvalid) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "RECIPIENT_NOT_PARTICIPANT",
                "Recipient must be an explicit participant in this opportunity.",
            );
        }
        Err(super::application::match_start::MatchStartError::NeedMissing) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "MATCH_NEED_MISSING",
                "Add an active, recipient-specific need before matching resources.",
            );
        }
        Err(super::application::match_start::MatchStartError::StaleRevision) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::CONFLICT,
                "STALE_REVISION",
                "The opportunity or recipient changed; reload and retry with a fresh request.",
            );
        }
        Err(super::application::match_start::MatchStartError::QueueAtCapacity) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::TOO_MANY_REQUESTS,
                "BUDGET_EXCEEDED",
                "The job queue is at capacity.",
            );
        }
        Err(super::application::match_start::MatchStartError::TargetNotGranted) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::NOT_FOUND,
                "TARGET_UNAVAILABLE",
                "Target record is not available.",
            );
        }
        Err(super::application::match_start::MatchStartError::Database) => {
            let _ = tx.rollback().await;
            return command_error(super::WorkspaceError::Database(sqlx::Error::Protocol(
                "storage failure".into(),
            )));
        }
    };
    let result = json!({"job_id": outcome.job_id, "role": outcome.role});
    if let Err(failure) = receipt_store(
        &mut tx,
        &state.workspace,
        &key,
        &canonical,
        StatusCode::ACCEPTED,
        &result,
    )
    .await
    {
        let _ = tx.rollback().await;
        return command_error(failure);
    }
    if tx.commit().await.is_err() {
        return command_error(super::WorkspaceError::Database(sqlx::Error::Protocol(
            "commit failure".into(),
        )));
    }
    state.jobs.notify_one();
    (StatusCode::ACCEPTED, Json(result)).into_response()
}

async fn get_match(
    State(state): State<AppState>,
    Path(opportunity_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<MatchQuery>,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let row = sqlx::query_as::<_,MatchRunRow>("SELECT mr.id,mr.opportunity_id,mr.recipient_id,mr.embed_model_digest,mr.calibration_json,mr.shortlist_json,mr.why_job_id FROM match_run mr JOIN opportunity o ON o.id=mr.opportunity_id JOIN entity e ON e.id=mr.recipient_id WHERE mr.opportunity_id=? AND mr.recipient_id=? AND mr.opportunity_revision=o.revision AND mr.recipient_revision=e.revision ORDER BY mr.created_at DESC,mr.id DESC LIMIT 1").bind(opportunity_id).bind(query.recipient_id).fetch_optional(state.workspace.pool()).await;
    match row {
        Ok(Some(value)) => Json(value).into_response(),
        Ok(None) => err(
            StatusCode::NOT_FOUND,
            "MATCH_NOT_FOUND",
            "No completed match exists for this opportunity and recipient.",
        ),
        Err(error) => command_error(error.into()),
    }
}

async fn build_templates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<TemplateBuildRequest>,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    if !matches!(
        input.stage_class.as_str(),
        "identified" | "prepared" | "touched" | "responded" | "converging"
    ) || input.rungs.is_empty()
        || input.rungs.iter().any(|r| *r == 0 || *r > 5)
        || !input.rungs.windows(2).all(|pair| pair[0] < pair[1])
        || input.rules.is_empty()
        || input.rules.iter().any(|rule| {
            !matches!(
                rule.as_str(),
                "R1" | "R2" | "R3" | "R4" | "R5" | "R6" | "R7"
            )
        })
        || input.max_chars == 0
        || input.max_chars > 20_000
        || input.channel.trim().is_empty()
        || input.channel.len() > 80
    {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "TEMPLATE_BUILD_INPUT_INVALID",
            "Builder needs a supported stage, increasing ask rungs, valid rule set, channel, and character limit.",
        );
    }
    let (scope_kind, scope_id, revision, segment_context) =
        if let Some(segment) = input.segment_id.as_deref() {
            match sqlx::query_as::<_, (String, Option<String>, i64)>(
                "SELECT name,description,revision FROM segment WHERE id=?",
            )
            .bind(segment)
            .fetch_optional(state.workspace.pool())
            .await
            {
                Ok(Some((name, description, rev))) => (
                    "segment",
                    segment.to_owned(),
                    rev,
                    json!({"id":segment,"name":name,"description":description}),
                ),
                Ok(None) => {
                    return err(
                        StatusCode::NOT_FOUND,
                        "SEGMENT_NOT_FOUND",
                        "Template segment does not exist.",
                    );
                }
                Err(error) => return command_error(error.into()),
            }
        } else {
            ("owner", "owner".to_owned(), 1, Value::Null)
        };
    let mut input_json = match serde_json::to_value(&input) {
        Ok(value) => value,
        Err(error) => {
            return err(
                StatusCode::BAD_REQUEST,
                "TEMPLATE_BUILD_INVALID",
                &error.to_string(),
            );
        }
    };
    if let Some(object) = input_json.as_object_mut() {
        object.insert("segment_context".into(), segment_context);
    }
    if !input.voice_sample_ids.is_empty() {
        let unique = input.voice_sample_ids.iter().collect::<BTreeSet<_>>();
        if unique.len() != input.voice_sample_ids.len() {
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "VOICE_SAMPLE_DUPLICATE",
                "Voice sample IDs must be unique.",
            );
        }
        let ids_json = match serde_json::to_string(&input.voice_sample_ids) {
            Ok(value) => value,
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "VOICE_SAMPLE_SERIALIZE_FAILED",
                    &error.to_string(),
                );
            }
        };
        let samples: Result<Vec<(String,String)>,sqlx::Error> = sqlx::query_as("SELECT id,text FROM voice_sample WHERE id IN (SELECT value FROM json_each(?)) AND status='active' ORDER BY id")
            .bind(&ids_json)
            .fetch_all(state.workspace.pool()).await;
        let samples = match samples {
            Ok(value) => value,
            Err(error) => return command_error(error.into()),
        };
        if samples.len() != unique.len() {
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "VOICE_SAMPLE_MISSING",
                "Every selected voice sample must exist in the owner bank.",
            );
        }
        let sample_revisions: Result<Vec<(String,i64)>,sqlx::Error> = sqlx::query_as("SELECT id,revision FROM voice_sample WHERE id IN (SELECT value FROM json_each(?)) AND status='active' ORDER BY id").bind(&ids_json).fetch_all(state.workspace.pool()).await;
        let sample_revisions = match sample_revisions {
            Ok(value) => value,
            Err(error) => return command_error(error.into()),
        };
        if sample_revisions.len() != unique.len() {
            return err(
                StatusCode::CONFLICT,
                "VOICE_SAMPLE_STALE",
                "A selected voice sample was retired while building the template request.",
            );
        }
        let revision_map = sample_revisions
            .into_iter()
            .map(|(id, revision)| (id, json!(revision)))
            .collect::<serde_json::Map<String, Value>>();
        let sample_values = samples
            .into_iter()
            .map(|(id, text)| json!({"id":id,"text":text}))
            .collect::<Vec<_>>();
        if let Some(object) = input_json.as_object_mut() {
            object.insert("voice_samples".into(), Value::Array(sample_values));
            object.insert("voice_sample_revisions".into(), Value::Object(revision_map));
        }
    }
    let key = match command_key(&headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let canonical = match canonical_command("POST /api/v1/templates/build", &input) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match enqueue_role_command(
        &state,
        &key,
        &canonical,
        "template_builder",
        scope_kind,
        &scope_id,
        revision,
        &input_json,
        3,
    )
    .await
    {
        Ok(EnqueueOutcome::Replay(response)) => response,
        Ok(EnqueueOutcome::New(job_id)) => (
            StatusCode::ACCEPTED,
            Json(json!({"job_id":job_id,"role":"template_builder"})),
        )
            .into_response(),
        Err(response) => response,
    }
}

async fn create_template_version(
    State(state): State<AppState>,
    Path(family_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<TemplateVersionRequest>,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let key = match command_key(&headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let canonical = match canonical_command(
        &format!("POST /api/v1/templates/{family_id}/versions"),
        &input,
    ) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if input.name.trim().is_empty()
        || input.name.len() > 200
        || input.body.len() > 10_000
        || input.channel.trim().is_empty()
        || input.channel.len() > 80
        || !matches!(
            input.stage_class.as_str(),
            "identified" | "prepared" | "touched" | "responded" | "converging"
        )
    {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "TEMPLATE_VERSION_INVALID",
            "Template metadata or content is outside supported limits.",
        );
    }
    let detailed = template::validate_detailed(
        &input.body,
        input.proof_fallback.as_deref(),
        &input.sentence_rules,
    );
    if !detailed.errors.is_empty() {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "TEMPLATE_VALIDATION_FAILED",
            &detailed.errors.join("; "),
        );
    }
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return command_error(error.into()),
    };
    match receipt_replay(&mut tx, &state.workspace, &key, &canonical).await {
        Ok(Some(response)) => {
            let _ = tx.commit().await;
            return response;
        }
        Ok(None) => {}
        Err(response) => {
            let _ = tx.rollback().await;
            return response;
        }
    }
    let family: Option<String> =
        match sqlx::query_scalar("SELECT id FROM template_family WHERE id=?")
            .bind(&family_id)
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(value) => value,
            Err(error) => {
                let _ = tx.rollback().await;
                return command_error(error.into());
            }
        };
    if family.is_none() {
        let _ = tx.rollback().await;
        return err(
            StatusCode::NOT_FOUND,
            "TEMPLATE_FAMILY_NOT_FOUND",
            "Template family does not exist.",
        );
    }
    let parent = if let Some(parent_id) = input.parent_version_id.as_deref() {
        let valid: Option<String> =
            match sqlx::query_scalar("SELECT id FROM template_version WHERE id=? AND family_id=?")
                .bind(parent_id)
                .bind(&family_id)
                .fetch_optional(&mut *tx)
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    let _ = tx.rollback().await;
                    return command_error(error.into());
                }
            };
        if valid.is_none() {
            let _ = tx.rollback().await;
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "TEMPLATE_PARENT_INVALID",
                "Parent version must belong to this immutable template family.",
            );
        }
        Some(parent_id.to_owned())
    } else {
        None
    };
    let version: i64 = match sqlx::query_scalar(
        "SELECT coalesce(max(version_no),0)+1 FROM template_version WHERE family_id=?",
    )
    .bind(&family_id)
    .fetch_one(&mut *tx)
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return command_error(error.into());
        }
    };
    let id = super::new_id(&format!(
        "{family_id}:{version}:{}",
        digest(input.body.as_bytes())
    ));
    let now = now_utc();
    let fallbacks = match serde_json::to_string(&json!({"proof":input.proof_fallback})) {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "TEMPLATE_SERIALIZE_FAILED",
                &error.to_string(),
            );
        }
    };
    let rules = match serde_json::to_string(&input.sentence_rules) {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "TEMPLATE_SERIALIZE_FAILED",
                &error.to_string(),
            );
        }
    };
    let inserted=sqlx::query("INSERT INTO template_version(id,family_id,version_no,body,fallbacks_json,status,content_sha256,created_at,rules_json,display_name,stage_class,channel,parent_version_id) VALUES(?,?,?,?,?,'draft',?,?,?,?,?,?,?)").bind(&id).bind(&family_id).bind(version).bind(&input.body).bind(fallbacks).bind(digest(input.body.as_bytes())).bind(&now).bind(rules).bind(&input.name).bind(&input.stage_class).bind(&input.channel).bind(parent).execute(&mut *tx).await;
    if let Err(error) = inserted {
        let _ = tx.rollback().await;
        return command_error(error.into());
    }
    let workspace_revision: i64 = match sqlx::query_scalar(
        "UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1 RETURNING revision",
    )
    .fetch_one(&mut *tx)
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return command_error(error.into());
        }
    };
    if let Err(error)=sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'template',?,?,?,?)").bind(super::new_id(&id)).bind(&now).bind(&id).bind(version).bind("template.version_created").bind(json!({"family_id":family_id,"workspace_revision":workspace_revision,"parent_version_id":input.parent_version_id,"content_sha256":digest(input.body.as_bytes())}).to_string()).execute(&mut *tx).await { let _=tx.rollback().await; return command_error(error.into()); }
    let response_body = json!({"id":id,"family_id":family_id,"version":version,"status":"draft","body":input.body,"rules":input.sentence_rules,"stage_class":input.stage_class,"channel":input.channel,"parent_version_id":input.parent_version_id});
    if let Err(error) = receipt_store(
        &mut tx,
        &state.workspace,
        &key,
        &canonical,
        StatusCode::CREATED,
        &response_body,
    )
    .await
    {
        let _ = tx.rollback().await;
        return command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return command_error(error.into());
    }
    (StatusCode::CREATED, Json(response_body)).into_response()
}

async fn compose_material(
    State(state): State<AppState>,
    Path(opportunity_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ComposeRequest>,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    if !matches!(input.kind.as_str(), "outreach" | "followup" | "handoff")
        || input.exchange_version <= 0
    {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "COMPOSER_INPUT_INVALID",
            "Composition requires a supported material kind and exchange version.",
        );
    }
    // Owner wire preconditions are resolved here so the published error codes
    // stay stable; the shared operation re-checks them under the transaction
    // and owns the canonical snapshot (UCR-001 U07).
    let context = sqlx::query_as::<_,(i64,i64,String,String)>("SELECT o.revision,e.revision,o.state,e.kind FROM opportunity o JOIN opportunity_party p ON p.opportunity_id=o.id JOIN entity e ON e.id=p.party_id WHERE o.id=? AND p.party_id=?").bind(&opportunity_id).bind(&input.recipient_id).fetch_optional(state.workspace.pool()).await;
    let Some((opp_revision, recipient_revision, state_value, kind)) = (match context {
        Ok(value) => value,
        Err(error) => return command_error(error.into()),
    }) else {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "COMPOSER_RECIPIENT_INVALID",
            "Recipient must be a participant in the opportunity.",
        );
    };
    if state_value != "open" || !matches!(kind.as_str(), "person" | "org") {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "COMPOSER_SCOPE_INVALID",
            "Composition requires an open opportunity and person or organization recipient.",
        );
    }
    let active: Option<String> =
        match sqlx::query_scalar("SELECT id FROM template_version WHERE id=? AND status='active'")
            .bind(&input.template_version_id)
            .fetch_optional(state.workspace.pool())
            .await
        {
            Ok(value) => value,
            Err(error) => return command_error(error.into()),
        };
    if active.is_none() {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "TEMPLATE_NOT_ACTIVE",
            "Only an active immutable template version can be composed.",
        );
    }
    let exchange: Option<(String,String,i64)> = match sqlx::query_as("SELECT they_get,i_get,fairness_attested FROM exchange_version WHERE opportunity_id=? AND recipient_id=? AND version_no=?").bind(&opportunity_id).bind(&input.recipient_id).bind(input.exchange_version).fetch_optional(state.workspace.pool()).await { Ok(value)=>value,Err(error)=>return command_error(error.into()) };
    let Some((they_get, i_get, attested)) = exchange else {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "EXCHANGE_MISSING",
            "Selected recipient exchange version does not exist.",
        );
    };
    if attested != 1 || they_get.trim().is_empty() || i_get.trim().is_empty() {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "EXCHANGE_NOT_REVIEWED",
            "The recipient's exchange must be complete and explicitly attested before composition.",
        );
    }
    let key = match command_key(&headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let canonical = match canonical_command(
        &format!("POST /api/v1/opportunities/{opportunity_id}/materials/compose"),
        &input,
    ) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return command_error(error.into()),
    };
    if let Some(response) = match receipt_replay(&mut tx, &state.workspace, &key, &canonical).await
    {
        Ok(response) => response,
        Err(response) => return response,
    } {
        let _ = tx.commit().await;
        return response;
    }
    let outcome = match super::application::compose_start::start(
        &mut tx,
        &super::application::compose_start::ComposeStartInput {
            opportunity_id: opportunity_id.clone(),
            recipient_id: input.recipient_id.clone(),
            expected_opportunity_revision: opp_revision,
            expected_recipient_revision: recipient_revision,
            kind: input.kind.clone(),
            template_version_id: input.template_version_id.clone(),
            exchange_version: input.exchange_version,
            slot_fills: input
                .slot_fills
                .iter()
                .map(|fill| super::application::compose_start::SlotFill {
                    slot: fill.slot.clone(),
                    value: fill.value.clone(),
                    resource_id: fill.resource_id.clone(),
                })
                .collect(),
            selected_match_ids: input.selected_match_ids.clone(),
            epoch: state.workspace.epoch().to_string(),
            run_id: None,
            owner_pseudonym: None,
            granted_targets: Vec::new(),
        },
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(super::application::compose_start::ComposeStartError::RecipientInvalid) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "COMPOSER_RECIPIENT_INVALID",
                "Recipient must be a participant in the opportunity.",
            );
        }
        Err(super::application::compose_start::ComposeStartError::StaleRevision) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::CONFLICT,
                "STALE_REVISION",
                "The opportunity or recipient changed; reload and retry with a fresh request.",
            );
        }
        Err(super::application::compose_start::ComposeStartError::TemplateInvalid) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "TEMPLATE_NOT_ACTIVE",
                "Only an active immutable template version can be composed.",
            );
        }
        Err(super::application::compose_start::ComposeStartError::ExchangeMissing) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "EXCHANGE_MISSING",
                "Selected recipient exchange version does not exist.",
            );
        }
        Err(super::application::compose_start::ComposeStartError::ExchangeNotReviewed) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "EXCHANGE_NOT_REVIEWED",
                "The recipient's exchange must be complete and explicitly attested before composition.",
            );
        }
        Err(super::application::compose_start::ComposeStartError::SlotInvalid) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "SLOT_SELECTION_INVALID",
                "Choose exactly one non-empty owner-approved value for each active slot.",
            );
        }
        Err(super::application::compose_start::ComposeStartError::ResourceInvalid) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "RESOURCE_NOT_READY",
                "A selected resource is missing or not ready.",
            );
        }
        Err(super::application::compose_start::ComposeStartError::MatchSelectionInvalid) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "MATCH_SELECTION_UNBOUND",
                "Selected Matcher ids must also appear in the slot fills and the current shortlist.",
            );
        }
        Err(super::application::compose_start::ComposeStartError::MatchShortlistMissing) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::CONFLICT,
                "MATCH_SHORTLIST_MISSING",
                "Run matching for this recipient before selecting matched resources.",
            );
        }
        Err(super::application::compose_start::ComposeStartError::TargetNotGranted) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::NOT_FOUND,
                "TARGET_UNAVAILABLE",
                "Target record is not available.",
            );
        }
        Err(super::application::compose_start::ComposeStartError::QueueAtCapacity) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::TOO_MANY_REQUESTS,
                "BUDGET_EXCEEDED",
                "The job queue is at capacity.",
            );
        }
        Err(super::application::compose_start::ComposeStartError::Database) => {
            let _ = tx.rollback().await;
            return command_error(super::WorkspaceError::Database(sqlx::Error::Protocol(
                "storage failure".into(),
            )));
        }
    };
    let result = json!({"job_id": outcome.job_id, "role": outcome.role});
    if let Err(failure) = receipt_store(
        &mut tx,
        &state.workspace,
        &key,
        &canonical,
        StatusCode::ACCEPTED,
        &result,
    )
    .await
    {
        let _ = tx.rollback().await;
        return command_error(failure);
    }
    if tx.commit().await.is_err() {
        return command_error(super::WorkspaceError::Database(sqlx::Error::Protocol(
            "commit failure".into(),
        )));
    }
    state.jobs.notify_one();
    (StatusCode::ACCEPTED, Json(result)).into_response()
}

async fn save_composer_candidate(
    State(state): State<AppState>,
    Path((opportunity_id, job_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<SaveComposerRequest>,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    if input.body.trim().is_empty() || input.body.len() > 20_000 {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "MATERIAL_BODY_INVALID",
            "Draft material must contain 1–20,000 bytes.",
        );
    }
    let key = match command_key(&headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let canonical = match canonical_command(
        &format!("POST /api/v1/opportunities/{opportunity_id}/materials/compose/{job_id}/save"),
        &input,
    ) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(value) => value,
        Err(error) => return command_error(error.into()),
    };
    match receipt_replay(&mut tx, &state.workspace, &key, &canonical).await {
        Ok(Some(response)) => {
            let _ = tx.commit().await;
            return response;
        }
        Ok(None) => {}
        Err(response) => {
            let _ = tx.rollback().await;
            return response;
        }
    }
    let row:Option<(String,String,i64,String,String,String)>=match sqlx::query_as("SELECT j.status,j.role,j.current_attempt,j.input_json,a.output_json,a.prompt_sha256 FROM durable_job j JOIN job_attempt a ON a.job_id=j.id AND a.attempt_no=j.current_attempt WHERE j.id=?").bind(&job_id).fetch_optional(&mut *tx).await{Ok(value)=>value,Err(error)=>{let _=tx.rollback().await;return command_error(error.into())}};
    let Some((status, role, _attempt, input_json, output_json, prompt_sha)) = row else {
        let _ = tx.rollback().await;
        return err(
            StatusCode::NOT_FOUND,
            "COMPOSER_JOB_NOT_FOUND",
            "Completed Composer job does not exist.",
        );
    };
    if status != "succeeded" || role != "composer" {
        let _ = tx.rollback().await;
        return err(
            StatusCode::CONFLICT,
            "COMPOSER_JOB_NOT_READY",
            "Only a successful Composer proposal can be saved.",
        );
    }
    let job_input: Value = match serde_json::from_str(&input_json) {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "COMPOSER_INPUT_CORRUPT",
                &error.to_string(),
            );
        }
    };
    if job_input.get("opportunity_id").and_then(Value::as_str) != Some(opportunity_id.as_str()) {
        let _ = tx.rollback().await;
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "COMPOSER_SCOPE_MISMATCH",
            "Composer job belongs to a different opportunity.",
        );
    }
    let proposal: Value = match serde_json::from_str(&output_json) {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "COMPOSER_OUTPUT_CORRUPT",
                &error.to_string(),
            );
        }
    };
    let fills = job_input
        .get("slot_fills")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if fills.iter().any(|fill| {
        fill.get("value")
            .and_then(Value::as_str)
            .is_none_or(|value| !input.body.contains(value))
    }) {
        let _ = tx.rollback().await;
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "PROTECTED_SLOT_CHANGED",
            "Every approved slot value must remain byte-for-byte in the saved draft.",
        );
    }
    let template_id = job_input
        .get("template_version_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let recipient = job_input
        .get("recipient_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = job_input
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let exchange = job_input
        .get("exchange_version")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    if !matches!(kind, "outreach" | "followup" | "handoff")
        || template_id.is_empty()
        || recipient.is_empty()
        || exchange <= 0
    {
        let _ = tx.rollback().await;
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "COMPOSER_INPUT_INVALID",
            "Composer job snapshot lacks required material bindings.",
        );
    }
    if let Err(failure) =
        super::writing_jobs::ensure_scope_current(&mut tx, &job_id, &job_input).await
    {
        let _ = tx.rollback().await;
        return err(
            StatusCode::CONFLICT,
            "COMPOSER_INPUT_STALE",
            &failure.message,
        );
    }
    let template_hash: Option<String> = match sqlx::query_scalar(
        "SELECT content_sha256 FROM template_version WHERE id=? AND status='active'",
    )
    .bind(template_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return command_error(error.into());
        }
    };
    let Some(template_hash) = template_hash else {
        let _ = tx.rollback().await;
        return err(
            StatusCode::CONFLICT,
            "TEMPLATE_NOT_ACTIVE",
            "Selected template is no longer active.",
        );
    };
    let exchange_attested:Option<i64>=match sqlx::query_scalar("SELECT fairness_attested FROM exchange_version WHERE opportunity_id=? AND recipient_id=? AND version_no=?").bind(&opportunity_id).bind(recipient).bind(exchange).fetch_optional(&mut *tx).await{Ok(value)=>value,Err(error)=>{let _=tx.rollback().await;return command_error(error.into())}};
    if exchange_attested != Some(1) {
        let _ = tx.rollback().await;
        return err(
            StatusCode::CONFLICT,
            "EXCHANGE_NOT_ATTESTED",
            "Recipient exchange is not currently attested.",
        );
    }
    let resource_revisions = job_input
        .get("resource_revisions")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let party_revision = job_input
        .get("recipient_revision")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let opportunity_revision = job_input
        .get("opportunity_revision")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let material_id = super::new_id(&format!("{job_id}:material"));
    let now = now_utc();
    let content_hash = digest(input.body.as_bytes());
    if let Err(error)=sqlx::query("INSERT INTO material(id,opportunity_id,recipient_id,kind,head_version,created_at,updated_at) VALUES(?,?,?,?,1,?,?)").bind(&material_id).bind(&opportunity_id).bind(recipient).bind(kind).bind(&now).bind(&now).execute(&mut *tx).await{let _=tx.rollback().await;return command_error(error.into());}
    let fill_json = match serde_json::to_string(&fills) {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "MATERIAL_SERIALIZE_FAILED",
                &error.to_string(),
            );
        }
    };
    let sentence_audit = proposal
        .get("sentences")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    // Convert the worker's sentence-level claim audit into typed owner-review
    // bindings. Each sentence receives exactly one binding whose approved
    // wording is this exact sentence; the typed binding set is what the gate
    // and release checks evaluate (UCR-001 U07).
    let sentence_list = super::materials::split_sentences(&input.body);
    let mut sentence_bindings: Vec<super::materials::SentenceBinding> = Vec::new();
    for (index, sentence) in sentence_list.iter().enumerate() {
        let claims = sentence_audit
            .get(index)
            .and_then(Value::as_object)
            .and_then(|object| object.get("claims"))
            .and_then(Value::as_array);
        let claim = claims.and_then(|items| items.first()).cloned();
        let claim_type = claim
            .as_ref()
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
            .filter(|kind| {
                matches!(
                    *kind,
                    "generic"
                        | "offer"
                        | "self_fact"
                        | "other_fact"
                        | "question"
                        | "ask"
                        | "identity"
                        | "hook"
                )
            })
            .unwrap_or("generic")
            .to_owned();
        let (ref_kind, ref_id) = if claim_type == "generic" {
            (None, None)
        } else {
            let kind = match claim_type.as_str() {
                "offer" | "self_fact" => Some("resource".to_owned()),
                "other_fact" => Some("claim".to_owned()),
                "question" => Some("interest".to_owned()),
                "ask" => Some("ladder".to_owned()),
                "hook" | "identity" => None,
                _ => None,
            };
            (
                kind,
                claim
                    .as_ref()
                    .and_then(|value| value.get("ref"))
                    .and_then(|value| value.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            )
        };
        sentence_bindings.push(super::materials::SentenceBinding {
            sentence_index: index,
            claim_type,
            ref_kind,
            ref_id,
            ref_revision: None,
            approved_wording: Some(sentence.clone()),
            ask_rung: None,
        });
    }
    let sentence_bindings_json = match serde_json::to_string(&sentence_bindings) {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return command_error(super::WorkspaceError::Invalid(error.to_string()));
        }
    };
    if let Err(error)=sqlx::query("INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at,job_id,prompt_id,prompt_sha256) VALUES(?,1,?,?,?,?,?,?,?,?,?,?,?,?)").bind(&material_id).bind(&opportunity_id).bind(recipient).bind(template_id).bind(exchange).bind(&input.body).bind(fill_json).bind(sentence_bindings_json).bind(&content_hash).bind(&now).bind(&job_id).bind(proposal.get("prompt_id").and_then(Value::as_str)).bind(&prompt_sha).execute(&mut *tx).await{let _=tx.rollback().await;return command_error(error.into());}
    let deps = [
        (
            "opportunity",
            opportunity_id.as_str(),
            opportunity_revision.to_string(),
        ),
        ("party", recipient, party_revision.to_string()),
        (
            "exchange",
            &format!("{opportunity_id}:{recipient}:{exchange}"),
            exchange.to_string(),
        ),
        ("template", template_id, template_hash),
    ];
    for (dep_kind, source_id, source_revision) in deps {
        if let Err(error)=sqlx::query("INSERT INTO version_dependency(material_id,version_no,kind,source_id,source_revision) VALUES(?,1,?,?,?)").bind(&material_id).bind(dep_kind).bind(source_id).bind(source_revision).execute(&mut *tx).await{let _=tx.rollback().await;return command_error(error.into());}
    }
    for (resource_id, revision) in resource_revisions {
        let revision_text = revision.as_i64().unwrap_or_default().to_string();
        if let Err(error)=sqlx::query("INSERT INTO version_dependency(material_id,version_no,kind,source_id,source_revision) VALUES(?,1,'resource',?,?)").bind(&material_id).bind(&resource_id).bind(revision_text).execute(&mut *tx).await{let _=tx.rollback().await;return command_error(error.into());}
    }
    for fill in &fills {
        if let (Some(resource_id), Some(slot)) = (
            fill.get("resource_id").and_then(Value::as_str),
            fill.get("slot").and_then(Value::as_str),
        ) && let Err(error)=sqlx::query("INSERT INTO resource_use(material_id,version_no,resource_id,slot_name) VALUES(?,1,?,?)").bind(&material_id).bind(resource_id).bind(slot).execute(&mut *tx).await{
            let _=tx.rollback().await;return command_error(error.into());
        }
    }
    let revision: Result<i64, sqlx::Error> = sqlx::query_scalar(
        "UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1 RETURNING revision",
    )
    .fetch_one(&mut *tx)
    .await;
    let revision = match revision {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return command_error(error.into());
        }
    };
    if let Err(error)=sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'material',?,?, 'material.composer_candidate_saved',?)").bind(super::new_id(&material_id)).bind(&now).bind(&material_id).bind(revision).bind(json!({"job_id":job_id,"version":1,"content_sha256":content_hash,"approved":false}).to_string()).execute(&mut *tx).await{let _=tx.rollback().await;return command_error(error.into());}
    let response_body = json!({"id":material_id,"opportunity_id":opportunity_id,"recipient_id":recipient,"kind":kind,"version":1,"content_sha256":content_hash,"status":"draft","approved":false,"released":false});
    if let Err(error) = receipt_store(
        &mut tx,
        &state.workspace,
        &key,
        &canonical,
        StatusCode::CREATED,
        &response_body,
    )
    .await
    {
        let _ = tx.rollback().await;
        return command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return command_error(error.into());
    }
    (StatusCode::CREATED, Json(response_body)).into_response()
}

async fn lint_material(
    State(state): State<AppState>,
    Path(material_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<LintRequest>,
) -> impl IntoResponse {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let key = match command_key(&headers) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let canonical = match canonical_command(
        &format!("POST /api/v1/materials/{material_id}/lint"),
        &input,
    ) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let version:Option<(i64,String,String,String,String)>=match sqlx::query_as("SELECT mv.version_no,mv.body,mv.content_sha256,mv.recipient_id,tv.channel FROM material m JOIN material_version mv ON mv.material_id=m.id AND mv.version_no=? JOIN template_version tv ON tv.id=mv.template_version_id WHERE m.id=?").bind(input.version).bind(&material_id).fetch_optional(state.workspace.pool()).await {Ok(value)=>value,Err(error)=>return command_error(error.into())};
    let Some((actual_version, body, hash, recipient_id, channel)) = version else {
        return err(
            StatusCode::NOT_FOUND,
            "MATERIAL_VERSION_NOT_FOUND",
            "Material version does not exist.",
        );
    };
    if actual_version != input.version || hash != input.content_sha256 {
        return err(
            StatusCode::CONFLICT,
            "MATERIAL_VERSION_STALE",
            "Lint requires the exact current material version and digest.",
        );
    }
    let settings_json: String =
        match sqlx::query_scalar("SELECT settings_json FROM owner_profile WHERE id=1")
            .fetch_one(state.workspace.pool())
            .await
        {
            Ok(value) => value,
            Err(error) => return command_error(error.into()),
        };
    let settings: Value = match serde_json::from_str(&settings_json) {
        Ok(value) => value,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "OWNER_SETTINGS_CORRUPT",
                &error.to_string(),
            );
        }
    };
    let max_chars = settings
        .pointer(&format!("/pack/limits/max_chars/{channel}"))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1200);
    let banned_phrases = settings
        .pointer("/lint/banned_phrases")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let unfluent_terms = settings
        .pointer("/fluency/unfluent_terms")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let highest_allowed_rung: u32 =
        match sqlx::query_scalar("SELECT highest_rung+1 FROM party_policy WHERE party_id=?")
            .bind(&recipient_id)
            .fetch_optional(state.workspace.pool())
            .await
        {
            Ok(Some(value)) => value,
            Ok(None) => 0,
            Err(error) => return command_error(error.into()),
        };
    let findings = lint::lint(
        &body,
        &lint::Config {
            max_chars,
            ask_count: body.chars().filter(|ch| *ch == '?').count(),
            highest_allowed_rung,
            banned_phrases,
            unfluent_terms,
            ..lint::Config::default()
        },
    );
    if let Some(finding) = findings.iter().find(|finding| {
        finding.start_byte > body.len()
            || finding.end_byte > body.len()
            || !body.is_char_boundary(finding.start_byte)
            || !body.is_char_boundary(finding.end_byte)
    }) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "LINT_SPAN_INVALID",
            &format!(
                "deterministic finding {} has an invalid source span",
                finding.code
            ),
        );
    }
    let json_findings = match serde_json::to_string(&findings) {
        Ok(value) => value,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "LINT_SERIALIZE_FAILED",
                &error.to_string(),
            );
        }
    };
    let now = now_utc();
    let job_input = json!({"material_id":material_id,"version":input.version,"content_sha256":hash,"body":body,"deterministic_findings":findings,"voice_sample_ids":[]});
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(value) => value,
        Err(error) => return command_error(error.into()),
    };
    match receipt_replay(&mut tx, &state.workspace, &key, &canonical).await {
        Ok(Some(response)) => {
            let _ = tx.commit().await;
            return response;
        }
        Ok(None) => {}
        Err(response) => {
            let _ = tx.rollback().await;
            return response;
        }
    }
    if let Err(error)=sqlx::query("INSERT INTO material_lint(material_id,version_no,content_sha256,deterministic_json,advisory_json,created_at) VALUES(?,?,?,?,'[]',?) ON CONFLICT(material_id,version_no,content_sha256) DO NOTHING").bind(&material_id).bind(input.version).bind(&hash).bind(&json_findings).bind(&now).execute(&mut *tx).await{let _=tx.rollback().await;return command_error(error.into());}
    let job_id = match super::jobs::enqueue_role_in_tx(
        &mut tx,
        "linter",
        "material",
        &material_id,
        input.version,
        &job_input,
        2,
        state.workspace.epoch(),
        None,
        None,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return super::materials::workspace_error(error);
        }
    };
    let response_body = json!({"job_id":job_id,"role":"linter","deterministic_findings":findings});
    if let Err(error) = receipt_store(
        &mut tx,
        &state.workspace,
        &key,
        &canonical,
        StatusCode::ACCEPTED,
        &response_body,
    )
    .await
    {
        let _ = tx.rollback().await;
        return command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return command_error(error.into());
    }
    state.jobs.notify_one();
    (StatusCode::ACCEPTED, Json(response_body)).into_response()
}

fn err(status: StatusCode, code: &'static str, message: &str) -> axum::response::Response {
    error(status, code, message)
}

#[allow(clippy::result_large_err)]
async fn receipt_replay(
    tx: &mut Transaction<'_, Sqlite>,
    workspace: &super::Workspace,
    key: &str,
    canonical: &str,
) -> Result<Option<axum::response::Response>, axum::response::Response> {
    let existing:Option<(String,i64,String)>=match sqlx::query_as("SELECT canonical_digest,status_code,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?").bind(workspace.epoch()).bind(key).fetch_optional(&mut **tx).await{Ok(value)=>value,Err(error)=>return Err(command_error(error.into()))};
    let Some((old_digest, status, result_json)) = existing else {
        return Ok(None);
    };
    if old_digest != canonical {
        return Err(err(
            StatusCode::CONFLICT,
            "KEY_REUSED",
            "Idempotency key was used for a different command.",
        ));
    }
    let value: Value = match serde_json::from_str(&result_json) {
        Ok(value) => value,
        Err(error) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "RECEIPT_CORRUPT",
                &error.to_string(),
            ));
        }
    };
    let status = StatusCode::from_u16(status as u16).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    Ok(Some((status, Json(value)).into_response()))
}

async fn receipt_store(
    tx: &mut Transaction<'_, Sqlite>,
    workspace: &super::Workspace,
    key: &str,
    canonical: &str,
    status: StatusCode,
    result: &Value,
) -> Result<(), WorkspaceError> {
    let result_json = serde_json::to_string(result)
        .map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
    let cursor: i64 = sqlx::query_scalar("SELECT coalesce(max(cursor),0) FROM event")
        .fetch_one(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,?,?,?,?)")
        .bind(workspace.epoch()).bind(key).bind(canonical).bind(status.as_u16() as i64).bind(result_json).bind(cursor).bind(now_utc()).execute(&mut **tx).await?;
    Ok(())
}

#[allow(clippy::result_large_err)]
fn command_key(headers: &HeaderMap) -> Result<String, axum::response::Response> {
    super::idempotency_key(headers).map_err(|(status, code, message)| err(status, code, message))
}

#[allow(clippy::result_large_err)]
fn canonical_command(
    route: &str,
    input: &impl Serialize,
) -> Result<String, axum::response::Response> {
    let body = serde_json::to_vec(input).map_err(|error| {
        err(
            StatusCode::BAD_REQUEST,
            "REQUEST_SERIALIZE_FAILED",
            &error.to_string(),
        )
    })?;
    let mut bytes = route.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(&body);
    Ok(digest(&bytes))
}

enum EnqueueOutcome {
    Replay(axum::response::Response),
    New(String),
}
#[allow(clippy::result_large_err, clippy::too_many_arguments)]
async fn enqueue_role_command(
    state: &AppState,
    key: &str,
    canonical: &str,
    role: &str,
    scope_kind: &str,
    scope_id: &str,
    scope_revision: i64,
    input: &Value,
    max_attempts: i64,
) -> Result<EnqueueOutcome, axum::response::Response> {
    let mut tx = state
        .workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| command_error(error.into()))?;
    if let Some(response) = receipt_replay(&mut tx, &state.workspace, key, canonical).await? {
        tx.commit()
            .await
            .map_err(|error| command_error(error.into()))?;
        return Ok(EnqueueOutcome::Replay(response));
    }
    let job_id = super::jobs::enqueue_role_in_tx(
        &mut tx,
        role,
        scope_kind,
        scope_id,
        scope_revision,
        input,
        max_attempts,
        state.workspace.epoch(),
        None,
        None,
    )
    .await
    .map_err(super::materials::workspace_error)?;
    let result = json!({"job_id":job_id,"role":role});
    receipt_store(
        &mut tx,
        &state.workspace,
        key,
        canonical,
        StatusCode::ACCEPTED,
        &result,
    )
    .await
    .map_err(command_error)?;
    tx.commit()
        .await
        .map_err(|error| command_error(error.into()))?;
    state.jobs.notify_one();
    Ok(EnqueueOutcome::New(job_id))
}

/// Validate explanations against the exact deterministic shortlist before a
/// matcher result is persisted. Unknown IDs, missing cited fields, or long
/// explanations are replaced by a code-generated line.
pub(super) fn validate_match_explanations(
    shortlist: &[String],
    explanations: &Value,
    party_fields: &BTreeSet<String>,
    asset_fields: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, String> {
    let mut accepted = BTreeMap::new();
    for item in explanations
        .get("explanations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = item.get("resource_id").and_then(Value::as_str) else {
            continue;
        };
        if !shortlist.iter().any(|candidate| candidate == id) {
            continue;
        }
        let why = item.get("why").and_then(Value::as_str).unwrap_or("");
        let party = item
            .get("cites_party_field")
            .and_then(Value::as_str)
            .unwrap_or("");
        let asset = item
            .get("cites_asset_field")
            .and_then(Value::as_str)
            .unwrap_or("");
        if why.split_whitespace().count() <= 25
            && !why.trim().is_empty()
            && party_fields.contains(party)
            && asset_fields
                .get(id)
                .is_some_and(|fields| fields.contains(asset))
        {
            accepted.insert(id.to_owned(), why.to_owned());
        }
    }
    accepted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher_explanations_cannot_add_unknown_shortlist_ids() {
        let explanation = json!({"explanations":[
            {"resource_id":"r1","why":"Fits the stated onboarding need.","cites_party_field":"need","cites_asset_field":"problem"},
            {"resource_id":"invented","why":"This fabricated item is an excellent fit.","cites_party_field":"need","cites_asset_field":"problem"}
        ]});
        let party_fields = BTreeSet::from(["need".to_owned()]);
        let asset_fields =
            BTreeMap::from([("r1".to_owned(), BTreeSet::from(["problem".to_owned()]))]);
        let accepted = validate_match_explanations(
            &["r1".to_owned()],
            &explanation,
            &party_fields,
            &asset_fields,
        );
        assert_eq!(accepted.len(), 1);
        assert!(accepted.contains_key("r1"));
        assert!(!accepted.contains_key("invented"));
    }
}
