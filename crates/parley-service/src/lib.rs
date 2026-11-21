//! Durable, local Parley application services.

use axum::{
    Json, Router,
    body::Body,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{
    path::Path,
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

static IDS: AtomicU64 = AtomicU64::new(0);

mod materials;
mod web_assets {
    include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("idempotency key was reused with a different request")]
    IdempotencyConflict,
}

#[derive(Clone)]
pub struct Workspace {
    pool: SqlitePool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CreateResource {
    pub idempotency_key: String,
    pub kind: String,
    pub title: String,
    pub body: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ResourceRecord {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: serde_json::Value,
    pub readiness: String,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedSource {
    pub content_id: String,
    pub variant_id: String,
    pub occurrence_id: String,
    pub normalized_sha256: String,
    pub raw_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EntityRecord {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub revision: i64,
}

#[derive(Clone)]
pub(crate) struct ApiState {
    pub(crate) workspace: Workspace,
    pub(crate) token: String,
    pub(crate) port: u16,
}

#[derive(Deserialize)]
struct ResourceRequest {
    kind: String,
    title: String,
    body: serde_json::Value,
}

/// Build the sole local API used by both browser and native shells.
pub fn api(workspace: Workspace, token: String, port: u16) -> Router {
    let state = ApiState {
        workspace,
        token,
        port,
    };
    Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .route("/api/v1/health", get(health))
        .route(
            "/api/v1/resources",
            get(list_resources).post(create_resource),
        )
        .route("/api/v1/resources/{id}", get(get_resource))
        .route("/api/v1/captures", get(list_captures).post(save_capture))
        .route("/api/v1/entities", get(list_entities).post(create_entity))
        .route("/api/v1/graph/related", post(create_related_entity))
        .merge(materials::routes())
        .with_state(state)
        .layer(axum::extract::DefaultBodyLimit::max(2_100_000))
}

async fn index(State(state): State<ApiState>, headers: HeaderMap) -> axum::response::Response {
    if !host_allowed(&headers, &state) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if web_assets::INDEX.is_empty() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Ok(mut html) = String::from_utf8(web_assets::INDEX.to_vec()) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let token = serde_json::to_string(&state.token).expect("token is serializable");
    let bootstrap = format!("<script>window.__PARLEY_TOKEN__={token};</script>");
    if let Some(index) = html.find("</head>") {
        html.insert_str(index, &bootstrap);
    } else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let mut response = html.into_response();
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

async fn asset(AxumPath(path): AxumPath<String>) -> axum::response::Response {
    let request_path = format!("assets/{path}");
    match web_assets::ASSETS
        .iter()
        .find(|(name, _, _)| *name == request_path)
    {
        Some((_, mime, bytes)) => {
            let mut response = Body::from(*bytes).into_response();
            response
                .headers_mut()
                .insert("content-type", HeaderValue::from_static(mime));
            response.headers_mut().insert(
                "cache-control",
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            );
            response
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn serve(
    listener: tokio::net::TcpListener,
    workspace: Workspace,
    token: String,
    port: u16,
) -> Result<(), std::io::Error> {
    axum::serve(listener, api(workspace, token, port)).await
}

async fn health() -> &'static str {
    "ok"
}

pub(crate) fn authorized(
    headers: &HeaderMap,
    state: &ApiState,
) -> Result<(), (StatusCode, &'static str)> {
    if !host_allowed(headers, state) {
        return Err((StatusCode::BAD_REQUEST, "invalid Host"));
    }
    let token = headers.get("x-parley-token").and_then(|v| v.to_str().ok());
    if token != Some(state.token.as_str()) {
        return Err((StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    Ok(())
}

fn host_allowed(headers: &HeaderMap, state: &ApiState) -> bool {
    headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|h| {
            h == format!("127.0.0.1:{}", state.port) || h == format!("localhost:{}", state.port)
        })
}

async fn create_resource(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(input): Json<ResourceRequest>,
) -> impl IntoResponse {
    if let Err(err) = authorized(&headers, &state) {
        return (
            err.0,
            Json(serde_json::json!({"error":{"code":"unauthorized","message":err.1}})),
        )
            .into_response();
    }
    let key = match headers.get("idempotency-key").and_then(|v| v.to_str().ok()) {
        Some(k) => k.to_owned(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error":{"code":"missing_idempotency_key"}})),
            )
                .into_response();
        }
    };
    match state
        .workspace
        .create_resource(CreateResource {
            idempotency_key: key,
            kind: input.kind,
            title: input.title,
            body: input.body,
        })
        .await
    {
        Ok(record) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(WorkspaceError::IdempotencyConflict) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":{"code":"idempotency_conflict"}})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":{"code":"invalid_request","message":e.to_string()}})),
        )
            .into_response(),
    }
}

async fn get_resource(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = authorized(&headers, &state) {
        return (
            err.0,
            Json(serde_json::json!({"error":{"code":"unauthorized","message":err.1}})),
        )
            .into_response();
    }
    match state.workspace.get_resource(&id).await {
        Ok(Some(record)) => (StatusCode::OK, Json(record)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error":{"code":"not_found"}})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":{"code":"storage_error","message":e.to_string()}})),
        )
            .into_response(),
    }
}

async fn list_resources(State(state): State<ApiState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(err) = authorized(&headers, &state) {
        return (
            err.0,
            Json(serde_json::json!({"error":{"code":"unauthorized","message":err.1}})),
        )
            .into_response();
    }
    match state.workspace.list_resources().await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":{"code":"storage_error"}})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct CaptureRequest {
    text: String,
    title: Option<String>,
    url: Option<String>,
}

async fn save_capture(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(input): Json<CaptureRequest>,
) -> impl IntoResponse {
    if let Err(err) = authorized(&headers, &state) {
        return (
            err.0,
            Json(serde_json::json!({"error":{"code":"unauthorized","message":err.1}})),
        )
            .into_response();
    }
    match state
        .workspace
        .save_source(&input.text, input.title.as_deref(), input.url.as_deref())
        .await
    {
        Ok(source) => (StatusCode::CREATED, Json(source)).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":{"code":"invalid_capture","message":e.to_string()}})),
        )
            .into_response(),
    }
}

async fn list_captures(State(state): State<ApiState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(err) = authorized(&headers, &state) {
        return (
            err.0,
            Json(serde_json::json!({"error":{"code":"unauthorized","message":err.1}})),
        )
            .into_response();
    }
    match state.workspace.list_sources().await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":{"code":"storage_error"}})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct EntityRequest {
    kind: String,
    name: String,
}
async fn create_entity(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(input): Json<EntityRequest>,
) -> impl IntoResponse {
    if let Err(err) = authorized(&headers, &state) {
        return (
            err.0,
            Json(serde_json::json!({"error":{"code":"unauthorized","message":err.1}})),
        )
            .into_response();
    }
    match state
        .workspace
        .create_entity(&input.kind, &input.name)
        .await
    {
        Ok(entity) => (StatusCode::CREATED, Json(entity)).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({"error":{"code":"invalid_entity","message":error.to_string()}}),
            ),
        )
            .into_response(),
    }
}
async fn list_entities(State(state): State<ApiState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(err) = authorized(&headers, &state) {
        return (
            err.0,
            Json(serde_json::json!({"error":{"code":"unauthorized","message":err.1}})),
        )
            .into_response();
    }
    match sqlx::query_as::<_, (String, String, String, i64)>(
        "SELECT id,kind,name,revision FROM entity ORDER BY name COLLATE NOCASE,id",
    )
    .fetch_all(state.workspace.pool())
    .await
    {
        Ok(rows) => (
            StatusCode::OK,
            Json(
                rows.into_iter()
                    .map(|(id, kind, name, revision)| EntityRecord {
                        id,
                        kind,
                        name,
                        revision,
                    })
                    .collect::<Vec<_>>(),
            ),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":{"code":"storage_error"}})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct RelatedEntityRequest {
    kind: String,
    name: String,
    related_entity_id: String,
    verb: String,
    occurrence_id: String,
    quote: String,
    view_scope: String,
    view_id: String,
    x: f64,
    y: f64,
}
async fn create_related_entity(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(input): Json<RelatedEntityRequest>,
) -> impl IntoResponse {
    if let Err(err) = authorized(&headers, &state) {
        return (
            err.0,
            Json(serde_json::json!({"error":{"code":"unauthorized","message":err.1}})),
        )
            .into_response();
    }
    match state.workspace.create_related_entity(&input.kind,&input.name,&input.related_entity_id,&input.verb,&input.occurrence_id,&input.quote,&input.view_scope,&input.view_id,input.x,input.y).await{
        Ok(entity)=>(StatusCode::CREATED,Json(entity)).into_response(),
        Err(error)=>(StatusCode::UNPROCESSABLE_ENTITY,Json(serde_json::json!({"error":{"code":"relationship_rejected","message":error.to_string()}}))).into_response(),
    }
}

impl Workspace {
    /// Open and migrate a private SQLite workspace. Every connection enables
    /// foreign keys and uses FULL synchronous durability in WAL mode.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Full)
            .busy_timeout(std::time::Duration::from_secs(2));
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Idempotent first-launch resource write. Resource, revision, event and
    /// receipt are committed under one SQLite writer reservation.
    pub async fn create_resource(
        &self,
        request: CreateResource,
    ) -> Result<ResourceRecord, WorkspaceError> {
        if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 200 {
            return Err(WorkspaceError::Invalid(
                "idempotency key must contain 1–200 bytes".into(),
            ));
        }
        if !matches!(request.kind.as_str(), "gift" | "proof" | "artifact") {
            return Err(WorkspaceError::Invalid(
                "kind must be gift, proof, or artifact".into(),
            ));
        }
        if request.title.len() > 500 {
            return Err(WorkspaceError::Invalid("title exceeds 500 bytes".into()));
        }
        let body = serde_json::to_string(&request.body)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let canonical = digest(format!("{}\0{}\0{}", request.kind, request.title, body).as_bytes());
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>("SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch='workspace' AND key=?")
            .bind(&request.idempotency_key).fetch_optional(&mut *tx).await? {
            if old_digest != canonical { return Err(WorkspaceError::IdempotencyConflict); }
            let record = serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            tx.commit().await?;
            return Ok(record);
        }
        let now = now_utc();
        let id = new_id(&canonical);
        sqlx::query("INSERT INTO resource(id,kind,title,body_json,readiness,revision,created_at,updated_at) VALUES(?,?,?,?, 'draft',1,?,?)")
            .bind(&id).bind(&request.kind).bind(&request.title).bind(&body).bind(&now).bind(&now).execute(&mut *tx).await?;
        sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        let event_id = new_id(&id);
        let result = ResourceRecord {
            id: id.clone(),
            kind: request.kind,
            title: request.title,
            body: request.body,
            readiness: "draft".into(),
            revision: 1,
        };
        let result_json =
            serde_json::to_string(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let cursor: i64 = sqlx::query_scalar("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'resource',?,1,'resource.created',?) RETURNING cursor")
            .bind(event_id).bind(&now).bind(&id).bind(format!("{{\"kind\":{:?}}}", result.kind)).fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,'workspace',?,?,201,?,?,?)")
            .bind(&request.idempotency_key).bind(canonical).bind(&result_json).bind(cursor).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn get_resource(&self, id: &str) -> Result<Option<ResourceRecord>, WorkspaceError> {
        let row = sqlx::query_as::<_, (String,String,String,String,String,i64)>("SELECT id,kind,title,body_json,readiness,revision FROM resource WHERE id=? AND readiness <> 'retired'")
            .bind(id).fetch_optional(&self.pool).await?;
        row.map(|(id, kind, title, body, readiness, revision)| {
            Ok(ResourceRecord {
                id,
                kind,
                title,
                body: serde_json::from_str(&body)
                    .map_err(|e| WorkspaceError::Invalid(e.to_string()))?,
                readiness,
                revision,
            })
        })
        .transpose()
    }

    pub async fn list_resources(&self) -> Result<Vec<ResourceRecord>, WorkspaceError> {
        let rows=sqlx::query_as::<_,(String,String,String,String,String,i64)>("SELECT id,kind,title,body_json,readiness,revision FROM resource WHERE readiness <> 'retired' ORDER BY created_at,id").fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|(id, kind, title, body, readiness, revision)| {
                Ok(ResourceRecord {
                    id,
                    kind,
                    title,
                    body: serde_json::from_str(&body)
                        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?,
                    readiness,
                    revision,
                })
            })
            .collect()
    }

    pub async fn list_sources(&self) -> Result<Vec<SavedSource>, WorkspaceError> {
        let rows=sqlx::query_as::<_,(String,String,String,String,String)>("SELECT c.id,v.id,o.id,c.normalized_sha256,v.raw_sha256 FROM source_occurrence o JOIN source_variant v ON v.id=o.source_variant_id JOIN source_content c ON c.id=v.content_id ORDER BY o.captured_at DESC,o.id").fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(
                |(content_id, variant_id, occurrence_id, normalized_sha256, raw_sha256)| {
                    SavedSource {
                        content_id,
                        variant_id,
                        occurrence_id,
                        normalized_sha256,
                        raw_sha256,
                    }
                },
            )
            .collect())
    }

    /// Save evidence independently of identity resolution. Identical normalized
    /// content shares a group while each raw variant and capture remains distinct.
    pub async fn save_source(
        &self,
        text: &str,
        title: Option<&str>,
        url: Option<&str>,
    ) -> Result<SavedSource, WorkspaceError> {
        self.save_source_attributed(text, title, url, "unknown", None)
            .await
    }

    pub async fn save_source_attributed(
        &self,
        text: &str,
        title: Option<&str>,
        url: Option<&str>,
        actor_kind: &str,
        actor_entity_id: Option<&str>,
    ) -> Result<SavedSource, WorkspaceError> {
        if !matches!(actor_kind, "owner" | "party" | "unknown")
            || ((actor_kind == "party") != actor_entity_id.is_some())
        {
            return Err(WorkspaceError::Invalid(
                "source attribution must identify an owner, a party entity, or remain unknown"
                    .into(),
            ));
        }
        if text.trim().is_empty() {
            return Err(WorkspaceError::Invalid("source text is empty".into()));
        }
        if text.len() > 2_000_000 {
            return Err(WorkspaceError::Invalid("source exceeds 2 MB".into()));
        }
        let normalized = parley_domain::evidence::normalize(text);
        let normalized_hash = digest(normalized.as_bytes());
        let raw_hash = digest(text.as_bytes());
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(actor_id) = actor_entity_id {
            let exists: Option<String> = sqlx::query_scalar("SELECT id FROM entity WHERE id=?")
                .bind(actor_id)
                .fetch_optional(&mut *tx)
                .await?;
            if exists.is_none() {
                return Err(WorkspaceError::Invalid(
                    "attributed speaker does not exist".into(),
                ));
            }
        }
        let content_id = match sqlx::query_scalar::<_, String>("SELECT id FROM source_content WHERE normalized_sha256=? AND normalization_version=? ORDER BY id LIMIT 1")
            .bind(&normalized_hash).bind(parley_domain::evidence::NORMALIZATION_VERSION as i64).fetch_optional(&mut *tx).await? {
                Some(id) => id,
                None => {
                    let id = new_id(&normalized_hash);
                    sqlx::query("INSERT INTO source_content(id,normalized_sha256,normalization_version,normalized_text,created_at) VALUES(?,?,?,?,?)")
                        .bind(&id).bind(&normalized_hash).bind(parley_domain::evidence::NORMALIZATION_VERSION as i64).bind(&normalized).bind(now_utc()).execute(&mut *tx).await?;
                    id
                }
            };
        let variant_id = match sqlx::query_scalar::<_, String>(
            "SELECT id FROM source_variant WHERE content_id=? AND raw_sha256=? ORDER BY id LIMIT 1",
        )
        .bind(&content_id)
        .bind(&raw_hash)
        .fetch_optional(&mut *tx)
        .await?
        {
            Some(id) => id,
            None => {
                let id = new_id(&raw_hash);
                sqlx::query("INSERT INTO source_variant(id,content_id,raw_sha256,raw_text,title,url,captured_at) VALUES(?,?,?,?,?,?,?)")
                        .bind(&id).bind(&content_id).bind(&raw_hash).bind(text).bind(title).bind(url).bind(now_utc()).execute(&mut *tx).await?;
                id
            }
        };
        let occurrence_id = new_id(&variant_id);
        sqlx::query("INSERT INTO source_occurrence(id,source_variant_id,actor_kind,actor_entity_id,captured_at) VALUES(?,?,?,?,?)")
            .bind(&occurrence_id).bind(&variant_id).bind(actor_kind).bind(actor_entity_id).bind(now_utc()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(SavedSource {
            content_id,
            variant_id,
            occurrence_id,
            normalized_sha256: normalized_hash,
            raw_sha256: raw_hash,
        })
    }

    /// Create a distinct graph identity. Names are deliberately not keys: two
    /// people with the same display name remain separate records.
    pub async fn create_entity(
        &self,
        kind: &str,
        name: &str,
    ) -> Result<EntityRecord, WorkspaceError> {
        if !matches!(kind, "person" | "org" | "project" | "resource") {
            return Err(WorkspaceError::Invalid("unsupported entity kind".into()));
        }
        if name.trim().is_empty() || name.len() > 500 {
            return Err(WorkspaceError::Invalid(
                "entity name must contain 1–500 bytes".into(),
            ));
        }
        let id = new_id(name);
        let now = now_utc();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES(?,?,?,1,?,?)",
        )
        .bind(&id)
        .bind(kind)
        .bind(name)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        if matches!(kind, "person" | "org") {
            sqlx::query(
                "INSERT INTO party_policy(party_id) VALUES(?) ON CONFLICT(party_id) DO NOTHING",
            )
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'entity',?,1,'entity.created','{}')")
            .bind(new_id(&id)).bind(&now).bind(&id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(EntityRecord {
            id,
            kind: kind.into(),
            name: name.into(),
            revision: 1,
        })
    }

    /// Atomically add a node, one source-backed edge and its initial layout.
    /// The quoted span is checked against the selected raw source occurrence;
    /// no name lookup or identity merge is performed.
    #[expect(
        clippy::too_many_arguments,
        reason = "the command has one revision-bound set of graph, evidence and layout inputs"
    )]
    pub async fn create_related_entity(
        &self,
        kind: &str,
        name: &str,
        from_id: &str,
        verb: &str,
        occurrence_id: &str,
        quote: &str,
        view_scope: &str,
        view_id: &str,
        x: f64,
        y: f64,
    ) -> Result<EntityRecord, WorkspaceError> {
        if !x.is_finite() || !y.is_finite() {
            return Err(WorkspaceError::Invalid(
                "layout coordinates must be finite".into(),
            ));
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let source=sqlx::query_as::<_,(String,String,String,Option<String>)>("SELECT v.raw_text,o.source_variant_id,o.actor_kind,o.actor_entity_id FROM source_occurrence o JOIN source_variant v ON v.id=o.source_variant_id WHERE o.id=?")
            .bind(occurrence_id).fetch_optional(&mut *tx).await?.ok_or_else(|| WorkspaceError::Invalid("source occurrence does not exist".into()))?;
        let matched =
            parley_domain::evidence::verify_quote(&source.0, quote, None).map_err(|_| {
                WorkspaceError::Invalid("quote is absent or ambiguous in the cited source".into())
            })?;
        let parent_kind: Option<String> = sqlx::query_scalar("SELECT kind FROM entity WHERE id=?")
            .bind(from_id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(parent_kind) = parent_kind else {
            return Err(WorkspaceError::Invalid(
                "relationship source entity does not exist".into(),
            ));
        };
        let target_is_source = matches!(
            (kind, parent_kind.as_str(), verb),
            ("person", "org", "member_of")
        );
        let legal = target_is_source
            || matches!(
                (kind, parent_kind.as_str(), verb),
                ("project", "org", "works_with") | ("resource", "person", "offers_to")
            );
        if !legal {
            return Err(WorkspaceError::Invalid(
                "relationship endpoint types and direction are not allowed".into(),
            ));
        }
        let id = new_id(name);
        let now = now_utc();
        sqlx::query(
            "INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES(?,?,?,1,?,?)",
        )
        .bind(&id)
        .bind(kind)
        .bind(name)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        if matches!(kind, "person" | "org") {
            sqlx::query(
                "INSERT INTO party_policy(party_id) VALUES(?) ON CONFLICT(party_id) DO NOTHING",
            )
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        }
        let claim_id = new_id(&id);
        sqlx::query("INSERT INTO claim(id,subject_id,predicate,current_revision,created_at) VALUES(?,?,?,1,?)").bind(&claim_id).bind(&id).bind(verb).bind(&now).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO claim_revision(claim_id,revision,wording,knowledge_kind,tier,speaker_kind,speaker_entity_id,occurrence_id,source_variant_id,source_start,source_end,normalized_start,normalized_end,normalization_version,created_at) VALUES(?,1,?,'observation',1,?,?,?,?,?,?,?,?,?,?)")
            .bind(&claim_id).bind(quote).bind(&source.2).bind(source.3.as_deref()).bind(occurrence_id).bind(&source.1).bind(matched.source_start as i64).bind(matched.source_end as i64).bind(matched.normalized_start as i64).bind(matched.normalized_end as i64).bind(parley_domain::evidence::NORMALIZATION_VERSION as i64).bind(&now).execute(&mut *tx).await?;
        let edge_id = new_id(&claim_id);
        let (edge_from, edge_to) = if target_is_source {
            (id.as_str(), from_id)
        } else {
            (from_id, id.as_str())
        };
        sqlx::query("INSERT INTO edge(id,from_id,verb,to_id,state,revision,created_at) VALUES(?,?,?,?,'active',1,?)").bind(&edge_id).bind(edge_from).bind(verb).bind(edge_to).bind(&now).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO edge_evidence(edge_id,claim_id,claim_revision) VALUES(?,?,1)")
            .bind(&edge_id)
            .bind(&claim_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO view_position(view_scope,view_id,entity_id,x,y,pinned,revision) VALUES(?,?,?,?,?,0,1)").bind(view_scope).bind(view_id).bind(&id).bind(x).bind(y).execute(&mut *tx).await?;
        for (event_type, entity_type, entity_id) in [
            ("entity.created", "entity", id.as_str()),
            ("edge.created", "edge", edge_id.as_str()),
        ] {
            sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,?, ?,1,?,'{}')").bind(new_id(entity_id)).bind(&now).bind(entity_type).bind(entity_id).bind(event_type).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(EntityRecord {
            id,
            kind: kind.into(),
            name: name.into(),
            revision: 1,
        })
    }
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(crate) fn new_id(seed: &str) -> String {
    let nonce = IDS.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    digest(format!("{seed}:{nanos}:{nonce}").as_bytes())[..32].to_owned()
}
pub(crate) fn now_utc() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let days = (now.as_secs() / 86_400) as i64;
    let seconds = now.as_secs() % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60,
        now.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::Arc};

    fn temp_db() -> std::path::PathBuf {
        let nonce = IDS.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("parley-s01-{}-{nonce}.sqlite", std::process::id()))
    }

    #[tokio::test]
    async fn same_key_race_commits_one_resource_event_and_receipt() {
        let path = temp_db();
        let workspace = Arc::new(Workspace::open(&path).await.unwrap());
        let mut tasks = Vec::new();
        for _ in 0..32 {
            let workspace = workspace.clone();
            tasks.push(tokio::spawn(async move {
                workspace
                    .create_resource(CreateResource {
                        idempotency_key: "first-resource".into(),
                        kind: "gift".into(),
                        title: "Incomplete offer".into(),
                        body: serde_json::json!({"what":"A guided walkthrough"}),
                    })
                    .await
                    .unwrap()
            }));
        }
        let mut ids = Vec::new();
        for task in tasks {
            ids.push(task.await.unwrap().id);
        }
        assert!(ids.iter().all(|id| id == &ids[0]));
        let resource_count: i64 = sqlx::query_scalar("SELECT count(*) FROM resource")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
        let event_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM event WHERE event_type='resource.created'")
                .fetch_one(workspace.pool())
                .await
                .unwrap();
        let receipt_count: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_receipt")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
        assert_eq!((resource_count, event_count, receipt_count), (1, 1, 1));
        assert!(matches!(
            workspace
                .create_resource(CreateResource {
                    idempotency_key: "first-resource".into(),
                    kind: "gift".into(),
                    title: "Different".into(),
                    body: serde_json::json!({})
                })
                .await,
            Err(WorkspaceError::IdempotencyConflict)
        ));
        workspace.pool().close().await;
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn source_only_save_deduplicates_content_without_inventing_identity() {
        let path = temp_db();
        let workspace = Workspace::open(&path).await.unwrap();
        let first = workspace
            .save_source("Dana will join the onboarding group.", Some("Update"), None)
            .await
            .unwrap();
        let second = workspace
            .save_source(
                "  DANA will join the onboarding group. ",
                Some("Copy"),
                None,
            )
            .await
            .unwrap();
        assert_eq!(first.content_id, second.content_id);
        assert_ne!(first.variant_id, second.variant_id);
        assert_ne!(first.occurrence_id, second.occurrence_id);
        let entities: i64 = sqlx::query_scalar("SELECT count(*) FROM entity")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
        let occurrences: i64 = sqlx::query_scalar("SELECT count(*) FROM source_occurrence")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
        assert_eq!((entities, occurrences), (0, 2));
        workspace.pool().close().await;
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn same_name_entities_keep_independent_ids() {
        let path = temp_db();
        let workspace = Workspace::open(&path).await.unwrap();
        let first = workspace.create_entity("person", "Dana Lee").await.unwrap();
        let second = workspace.create_entity("person", "Dana Lee").await.unwrap();
        assert_ne!(first.id, second.id);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM entity WHERE name='Dana Lee'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
        assert_eq!(count, 2);
        workspace.pool().close().await;
        let _ = fs::remove_file(path);
    }
}
