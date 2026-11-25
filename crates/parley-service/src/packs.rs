//! Pack settings surface: the active config pack, validation of new pack
//! documents and activation with an explicit stage mapping. Reference packs
//! (job_search, lead_gen) are embedded; owner-activated documents live in
//! `pack_document` keyed by content digest, and activation installs the
//! document's stages as `pack_stage` rows, remaps open opportunities through
//! the owner-provided mapping and derives the `pack_policy` fragment the
//! queue and the gate read. The model never touches any of this.

use axum::{
    Json,
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use parley_domain::pack as pack_domain;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::WorkspaceError;
use crate::AppState;

/// The reference job_search document. Its stages MUST stay in lockstep with
/// the `pack_stage` rows seeded by 0019 (same stage ids, labels, classes and
/// order); `INSERT OR IGNORE` keeps the seed canonical when this document is
/// activated.
pub const JOB_SEARCH_DOCUMENT: &str = concat!(
    "id: job_search\n",
    "name: Job search\n",
    "version: 1.0.0\n",
    "vocabulary:\n",
    "  party_noun: hiring contact\n",
    "  win_noun: role\n",
    "stages:\n",
    "  - { name: Exploring, class: identified }\n",
    "  - { name: Preparing, class: prepared }\n",
    "  - { name: In conversation, class: responded }\n",
    "  - { name: Closed, class: closed }\n",
    "ask_ladder:\n",
    "  - { rung: 1, weight: 1, label: one-line question }\n",
    "  - { rung: 2, weight: 2, label: short review }\n",
    "cadence:\n",
    "  max_touches: 3\n",
    "limits:\n",
    "  daily_send_cap: 12\n",
);

/// The reference lead_gen document, mirroring the Pack study's second mode.
pub const LEAD_GEN_DOCUMENT: &str = concat!(
    "id: lead_gen\n",
    "name: Client lead generation\n",
    "version: 1.0.0\n",
    "vocabulary:\n",
    "  party_noun: prospect\n",
    "  win_noun: engagement\n",
    "stages:\n",
    "  - { name: Identified, class: identified }\n",
    "  - { name: Briefed, class: prepared }\n",
    "  - { name: Contacted, class: touched }\n",
    "ask_ladder:\n",
    "  - { rung: 1, weight: 1, label: yes/no question }\n",
    "  - { rung: 2, weight: 2, label: review }\n",
    "cadence:\n",
    "  max_touches: 3\n",
    "limits:\n",
    "  daily_send_cap: 15\n",
);

const REFERENCE_PACKS: [(&str, &str, &str, &str); 2] = [
    ("job_search", "Job search", "1.0.0", JOB_SEARCH_DOCUMENT),
    ("lead_gen", "Client lead generation", "1.0.0", LEAD_GEN_DOCUMENT),
];

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/settings", get(get_pack_settings))
        .route("/api/v1/packs/validate", post(validate_pack))
        .route("/api/v1/packs/activate", post(activate_pack))
}

#[derive(Deserialize)]
struct PackValidateInput {
    document: String,
}

#[derive(Deserialize, Serialize)]
struct PackActivateInput {
    pack_id: String,
    version: String,
    document: String,
    expected_settings_revision: i64,
    stage_mapping: std::collections::BTreeMap<String, String>,
}

struct ActivePack {
    pack_id: String,
    pack_version: String,
    settings_revision: i64,
    document: String,
}

struct StoredPack {
    pack_id: String,
    version: String,
    name: String,
    document: String,
}

async fn read_active_pack(
    connection: &mut sqlx::SqliteConnection,
) -> Result<ActivePack, WorkspaceError> {
    let row: (String, String, i64) = sqlx::query_as(
        "SELECT pack_id,pack_version,settings_revision FROM owner_profile WHERE id=1",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| WorkspaceError::Invalid(format!("Pack settings are unavailable: {error}")))?;
    let document: Option<String> =
        sqlx::query_scalar("SELECT document FROM pack_document WHERE pack_id=? AND version=?")
            .bind(&row.0)
            .bind(&row.1)
            .fetch_optional(&mut *connection)
            .await
            .map_err(WorkspaceError::Database)?;
    let document = match document {
        Some(document) => document,
        None => REFERENCE_PACKS
            .iter()
            .find(|(id, _, version, _)| *id == row.0 && *version == row.1)
            .map(|(_, _, _, document)| (*document).to_owned())
            .ok_or_else(|| WorkspaceError::Invalid("The active pack document is missing".into()))?,
    };
    Ok(ActivePack { pack_id: row.0, pack_version: row.1, settings_revision: row.2, document })
}

async fn read_stored_packs(connection: &mut sqlx::SqliteConnection) -> Result<Vec<StoredPack>, WorkspaceError> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT pack_id,version,name,document FROM pack_document ORDER BY validated_at DESC",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(WorkspaceError::Database)?;
    Ok(rows
        .into_iter()
        .map(|(pack_id, version, name, document)| StoredPack { pack_id, version, name, document })
        .collect())
}

fn available_packs(active: &ActivePack, stored: &[StoredPack]) -> Value {
    let mut packs: Vec<Value> = Vec::new();
    let mut push = |id: &str, name: &str, version: &str, document: &str| {
        if packs.iter().any(|pack| pack["id"] == *id && pack["version"] == *version) {
            return;
        }
        packs.push(json!({"id": id, "name": name, "version": version, "document": document}));
    };
    // A stored document with the same id/version replaces the reference text:
    // the owner's validated edit is the authority for that identity.
    for pack in stored {
        push(&pack.pack_id, &pack.name, &pack.version, &pack.document);
    }
    for (id, name, version, document) in REFERENCE_PACKS {
        push(id, name, version, document);
    }
    push(&active.pack_id, &active.pack_id, &active.pack_version, &active.document);
    Value::Array(packs)
}

fn settings_body(active: &ActivePack, stored: &[StoredPack]) -> Value {
    json!({
        "revision": active.settings_revision,
        "active_pack_id": active.pack_id,
        "active_pack_version": active.pack_version,
        "active_document": active.document,
        "available_packs": available_packs(active, stored),
    })
}

async fn get_pack_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let result = async {
        let mut connection = state.workspace.pool().acquire().await?;
        let active = read_active_pack(&mut connection).await?;
        let stored = read_stored_packs(&mut connection).await?;
        Ok(settings_body(&active, &stored))
    }
    .await;
    match result {
        Ok(body) => {
            let mut response = Json(body).into_response();
            super::private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(error) => super::command_error(error),
    }
}

async fn validate_pack(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PackValidateInput>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let normalized = pack_domain::normalize_pack_document(&input.document);
    let issues = match pack_domain::parse_pack(&normalized) {
        Ok(_) => Vec::new(),
        Err(issues) => issues,
    };
    let valid = issues.is_empty();
    let mut stored_ok = false;
    if valid {
        let digest = crate::digest(normalized.as_bytes());
        let now = super::now_utc();
        let result = async {
            let mut connection = state.workspace.pool().acquire().await?;
            let pack = pack_domain::parse_pack(&normalized).map_err(|_| WorkspaceError::Invalid("pack re-parse failed".into()))?;
            sqlx::query("INSERT INTO pack_document(digest,pack_id,version,name,document,validated_at) VALUES(?,?,?,?,?,?) ON CONFLICT(digest) DO UPDATE SET validated_at=excluded.validated_at")
                .bind(&digest)
                .bind(&pack.id)
                .bind(&pack.version)
                .bind(&pack.name)
                .bind(&normalized)
                .bind(&now)
                .execute(&mut *connection)
                .await
                .map_err(WorkspaceError::Database)?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            return super::command_error(error);
        }
        stored_ok = true;
    }
    let pack = if valid { pack_domain::parse_pack(&normalized).ok() } else { None };
    let (pack_id, version, normalized_document) = match (&pack, valid && stored_ok) {
        (Some(pack), true) => (Value::String(pack.id.clone()), Value::String(pack.version.clone()), Value::String(normalized.clone())),
        _ => (Value::Null, Value::Null, Value::Null),
    };
    let body = json!({
        "valid": valid,
        "pack_id": pack_id,
        "version": version,
        "normalized_document": normalized_document,
        "errors": issues.iter().map(|issue| json!({
            "line": issue.line.map(|line| line as i64),
            "field": issue.field,
            "message": issue.message,
        })).collect::<Vec<_>>(),
    });
    Json(body).into_response()
}

fn issue_list(issues: &[pack_domain::PackIssue]) -> Value {
    Value::Array(issues
        .iter()
        .map(|issue| json!({
            "line": issue.line.map(|line| line as i64),
            "field": issue.field,
            "message": issue.message,
        }))
        .collect())
}

async fn activate_pack(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PackActivateInput>,
) -> Response {
    activate(State(state), headers, Json(input)).await
}

async fn activate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PackActivateInput>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if input.pack_id.trim().is_empty()
        || input.version.trim().is_empty()
        || input.pack_id.len() > 64
        || input.version.len() > 32
        || input.document.is_empty()
        || input.document.len() > 64 * 1024
    {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "PACK_INPUT_INVALID",
            "Pack identity or document is missing or oversized",
        );
    }
    let key = match headers.get("idempotency-key").and_then(|value| value.to_str().ok()) {
        Some(value) if !value.trim().is_empty() && value.len() <= 200 => value.to_owned(),
        _ => return super::error(StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED", "A 1-200 byte Idempotency-Key is required"),
    };
    let canonical = match serde_json::to_vec(&input) {
        Ok(bytes) => crate::digest(&bytes),
        Err(_) => return super::error(StatusCode::BAD_REQUEST, "PACK_INPUT_INVALID", "Request body is not canonical JSON"),
    };

    // Validate BEFORE opening the write transaction: an invalid document is a
    // read-only refusal and never takes the workspace lock.
    let normalized = pack_domain::normalize_pack_document(&input.document);
    let pack = match pack_domain::parse_pack(&normalized) {
        Ok(pack) => pack,
        Err(issues) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": {
                        "code": "PACK_NOT_VALIDATED",
                        "message": "Activate only a valid pack document",
                        "details": issue_list(&issues),
                    }
                })),
            )
                .into_response();
        }
    };
    if pack.id != input.pack_id || pack.version != input.version {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "PACK_NOT_VALIDATED",
            "The document's pack id and version must match the activation request",
        );
    }

    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    let prior: Result<Option<(String, String)>, sqlx::Error> = sqlx::query_as(
        "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
    )
    .bind(state.workspace.epoch())
    .bind(&key)
    .fetch_optional(&mut *tx)
    .await;
    match prior {
        Ok(Some((old, result))) if old == canonical => {
            let _ = tx.commit().await;
            return Json(serde_json::from_str::<Value>(&result).unwrap_or(Value::Null)).into_response();
        }
        Ok(Some(_)) => {
            let _ = tx.rollback().await;
            return super::error(StatusCode::CONFLICT, "IDEMPOTENCY_CONFLICT", "This key was already used for another pack activation");
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return super::command_error(error.into());
        }
        Ok(None) => {}
    }

    let (current_pack_id, current_pack_version, settings_revision): (String, String, i64) =
        match sqlx::query_as("SELECT pack_id,pack_version,settings_revision FROM owner_profile WHERE id=1")
            .fetch_one(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(error) => {
                let _ = tx.rollback().await;
                return super::command_error(error.into());
            }
        };
    if input.expected_settings_revision != settings_revision {
        let _ = tx.rollback().await;
        return super::error(
            StatusCode::PRECONDITION_FAILED,
            "SETTINGS_STALE",
            "Pack settings changed; reload and review the current selection",
        );
    }
    let identity_changed = input.pack_id != current_pack_id || input.version != current_pack_version;
    if identity_changed && input.stage_mapping.is_empty() {
        let _ = tx.rollback().await;
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "STAGE_MAPPING_REQUIRED",
            "Map existing stage names before activating this pack version",
        );
    }

    let stage_rows = pack.stage_rows();
    for (slug, label, class, kind, ordinal) in &stage_rows {
        if let Err(error) = sqlx::query("INSERT INTO pack_stage(id,pack_id,pack_version,stage_id,label,class,kind,ordinal) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(pack_id,pack_version,stage_id) DO NOTHING")
            .bind(format!("{}@{}@{slug}", input.pack_id, input.version))
            .bind(&input.pack_id)
            .bind(&input.version)
            .bind(slug)
            .bind(label)
            .bind(class)
            .bind(kind)
            .bind(ordinal)
            .execute(&mut *tx)
            .await
        {
            let _ = tx.rollback().await;
            return super::command_error(error.into());
        }
    }

    // Resolve mapping targets by stage slug first, then by exact label.
    let mut remap: Vec<(String, String)> = Vec::new();
    for (old_stage, target) in &input.stage_mapping {
        let resolved = stage_rows
            .iter()
            .find(|(slug, _, _, _, _)| slug == target)
            .or_else(|| stage_rows.iter().find(|(_, label, _, _, _)| label == target))
            .map(|(slug, _, _, _, _)| slug.clone());
        let Some(new_stage) = resolved else {
            let _ = tx.rollback().await;
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "STAGE_MAPPING_TARGET_INVALID",
                format!("Mapping target `{target}` is not a stage of the new pack"),
            );
        };
        remap.push((old_stage.clone(), new_stage));
    }

    if identity_changed {
        for (old_stage, new_stage) in &remap {
            if let Err(error) = sqlx::query(
                "UPDATE opportunity SET stage_id=?,stage_pack_id=?,stage_pack_version=?,phase_revision=phase_revision+1 WHERE stage_pack_id=? AND stage_pack_version=? AND stage_id=?",
            )
            .bind(new_stage)
            .bind(&input.pack_id)
            .bind(&input.version)
            .bind(&current_pack_id)
            .bind(&current_pack_version)
            .bind(old_stage)
            .execute(&mut *tx)
            .await
            {
                let _ = tx.rollback().await;
                return super::command_error(error.into());
            }
        }
        // Opportunities under the old pack whose stage has no mapping become
        // Unmapped; history is never invented for them.
        if let Err(error) = sqlx::query(
            "UPDATE opportunity SET stage_id=NULL,stage_pack_id=NULL,stage_pack_version=NULL,phase_revision=phase_revision+1 WHERE stage_pack_id=? AND stage_pack_version=? AND stage_id IS NOT NULL",
        )
        .bind(&current_pack_id)
        .bind(&current_pack_version)
        .execute(&mut *tx)
        .await
        {
            let _ = tx.rollback().await;
            return super::command_error(error.into());
        }
    }

    let policy = pack.policy_json().to_string();
    let new_revision: Result<i64, sqlx::Error> = sqlx::query_scalar(
        "UPDATE owner_profile SET pack_id=?,pack_version=?,settings_json=json_set(settings_json,'$.pack_policy',json(?)),settings_revision=settings_revision+1 WHERE id=1 RETURNING settings_revision",
    )
    .bind(&input.pack_id)
    .bind(&input.version)
    .bind(&policy)
    .fetch_one(&mut *tx)
    .await;
    let new_revision = match new_revision {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return super::command_error(error.into());
        }
    };

    let now = super::now_utc();
    let active_after = ActivePack {
        pack_id: input.pack_id.clone(),
        pack_version: input.version.clone(),
        settings_revision: new_revision,
        document: normalized.clone(),
    };
    let stored_after: Vec<StoredPack> = match sqlx::query_as(
        "SELECT pack_id,version,name,document FROM pack_document ORDER BY validated_at DESC",
    )
    .fetch_all(&mut *tx)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|(pack_id, version, name, document)| StoredPack { pack_id, version, name, document })
            .collect(),
        Err(error) => {
            let _ = tx.rollback().await;
            return super::command_error(error.into());
        }
    };
    let body = settings_body(&active_after, &stored_after);
    let event_cursor: Result<i64, sqlx::Error> = sqlx::query_scalar(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'settings','pack',?, 'pack.activated', ?) RETURNING cursor",
    )
    .bind(crate::new_id(&canonical))
    .bind(&now)
    .bind(new_revision)
    .bind(json!({"pack_id": input.pack_id, "version": input.version}).to_string())
    .fetch_one(&mut *tx)
    .await;
    let event_cursor = match event_cursor {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return super::command_error(error.into());
        }
    };
    if let Err(error) = sqlx::query(
        "INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,200,?,?,?)",
    )
    .bind(state.workspace.epoch())
    .bind(&key)
    .bind(&canonical)
    .bind(body.to_string())
    .bind(event_cursor)
    .bind(&now)
    .execute(&mut *tx)
    .await
    {
        let _ = tx.rollback().await;
        return super::command_error(error.into());
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    let mut response = Json(body).into_response();
    super::private_headers(&mut response, state.workspace.epoch());
    response
}
