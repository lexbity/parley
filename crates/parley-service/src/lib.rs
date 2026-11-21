//! Local Parley HTTP service and transactional workspace commands.
//!
//! The browser is a client of this service. Resource writes, revisions,
//! events, and idempotency receipts share one SQLite transaction.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{OriginalUri, Path as AxumPath, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rand::random;
use serde::de::{DeserializeOwned, Error as DeError, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use std::fmt;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[derive(Clone)]
struct Session {
    token: String,
}

#[derive(Clone)]
struct AppState {
    pool: SqlitePool,
    epoch: String,
    workspace_id: String,
    authority: String,
    static_root: std::path::PathBuf,
    bootstrap: Arc<Mutex<HashMap<String, Instant>>>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

/// A running local service. The bootstrap fragment is short lived at the
/// application boundary; callers must remove it from browser history at once.
pub struct RunningService {
    pub address: SocketAddr,
    pub bootstrap_secret: String,
    bootstrap: Arc<Mutex<HashMap<String, Instant>>>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
pub struct BootstrapIssuer {
    bootstrap: Arc<Mutex<HashMap<String, Instant>>>,
    pub address: SocketAddr,
}

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

impl RunningService {
    /// Issue a fresh single-use bootstrap secret to the authenticated local
    /// launcher control channel.
    pub async fn issue_bootstrap(&self) -> String {
        self.bootstrap_issuer().issue().await
    }

    pub fn bootstrap_issuer(&self) -> BootstrapIssuer {
        BootstrapIssuer {
            bootstrap: self.bootstrap.clone(),
            address: self.address,
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

impl BootstrapIssuer {
    pub async fn issue(&self) -> String {
        let secret = random_hex(32);
        let now = Instant::now();
        let mut bootstraps = self.bootstrap.lock().await;
        bootstraps.retain(|_, expires| *expires > now);
        if bootstraps.len() >= 8
            && let Some(oldest) = bootstraps
                .iter()
                .min_by_key(|(_, expires)| **expires)
                .map(|(value, _)| value.clone())
        {
            bootstraps.remove(&oldest);
        }
        bootstraps.insert(secret.clone(), now + Duration::from_secs(60));
        secret
    }
}

/// Initialize (or open) a private SQLite workspace and bind only to IPv4
/// loopback. The caller owns workspace locking and process lifetime.
pub async fn start_workspace(path: &Path) -> Result<RunningService, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("workspace directory: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("workspace permissions: {error}"))?;
        }
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .map_err(|error| format!("open workspace: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("workspace permissions: {error}"))?;
    }
    MIGRATOR
        .run(&pool)
        .await
        .map_err(|error| format!("migrate workspace: {error}"))?;
    let initialized: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workspace_meta WHERE singleton=1")
            .fetch_one(&pool)
            .await
            .map_err(|error| error.to_string())?;
    if initialized == 0 {
        let mut tx = pool.begin().await.map_err(|error| error.to_string())?;
        let epoch = random_hex(16);
        let workspace_id = random_hex(16);
        sqlx::query("INSERT INTO workspace_meta(singleton,schema_version,workspace_id,epoch,created_at) VALUES(1,1,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'))")
            .bind(workspace_id).bind(&epoch).execute(&mut *tx).await.map_err(|error| error.to_string())?;
        sqlx::query("INSERT INTO owner_profile(id,display_name,timezone,pack_id,pack_version,settings_json) VALUES(1,'Owner','UTC','job_search','1.0.0','{}')")
            .execute(&mut *tx).await.map_err(|error| error.to_string())?;
        sqlx::query("INSERT INTO board_list(id,title,position,archived,revision) VALUES('inbox','Inbox',0,0,1)")
            .execute(&mut *tx).await.map_err(|error| error.to_string())?;
        tx.commit()
            .await
            .map_err(|error| format!("seed workspace: {error}"))?;
        sqlx::query("PRAGMA user_version=1")
            .execute(&pool)
            .await
            .map_err(|error| error.to_string())?;
    }
    let meta: (String, String) =
        sqlx::query_as("SELECT epoch, workspace_id FROM workspace_meta WHERE singleton=1")
            .fetch_one(&pool)
            .await
            .map_err(|error| format!("read workspace metadata: {error}"))?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("bind loopback: {error}"))?;
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    let bootstrap_secret = random_hex(32);
    let state = AppState {
        pool,
        epoch: meta.0,
        workspace_id: meta.1,
        authority: address.to_string(),
        static_root: std::env::var_os("PARLEY_WEB_DIST")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().join("web/dist")),
        bootstrap: Arc::new(Mutex::new(HashMap::from([(
            bootstrap_secret.clone(),
            Instant::now() + Duration::from_secs(60),
        )]))),
        sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    let bootstrap_shared = state.bootstrap.clone();
    let app = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/session/bootstrap", post(bootstrap))
        .route("/api/v1/session", get(session))
        .route("/api/v1/entities", get(list_entities).post(create_entity))
        .route(
            "/api/v1/entities/{id}",
            get(get_entity).patch(update_entity),
        )
        .route("/api/v1/events", get(get_events))
        .fallback(serve_static)
        .with_state(state);
    let (shutdown, receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = receiver.await;
            })
            .await;
    });
    Ok(RunningService {
        address,
        bootstrap_secret,
        bootstrap: bootstrap_shared,
        shutdown,
        task,
    })
}

fn random_hex(size: usize) -> String {
    let bytes: Vec<u8> = (0..size).map(|_| random::<u8>()).collect();
    hex::encode(bytes)
}

struct UniqueJson(Value);

impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = Value;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON value without duplicate object keys")
            }
            fn visit_bool<E: DeError>(self, value: bool) -> Result<Value, E> {
                Ok(Value::Bool(value))
            }
            fn visit_i64<E: DeError>(self, value: i64) -> Result<Value, E> {
                Ok(Value::Number(value.into()))
            }
            fn visit_u64<E: DeError>(self, value: u64) -> Result<Value, E> {
                Ok(Value::Number(value.into()))
            }
            fn visit_f64<E: DeError>(self, value: f64) -> Result<Value, E> {
                serde_json::Number::from_f64(value)
                    .map(Value::Number)
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }
            fn visit_str<E: DeError>(self, value: &str) -> Result<Value, E> {
                Ok(Value::String(value.to_owned()))
            }
            fn visit_string<E: DeError>(self, value: String) -> Result<Value, E> {
                Ok(Value::String(value))
            }
            fn visit_unit<E: DeError>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_none<E: DeError>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<UniqueJson>()? {
                    values.push(value.0);
                }
                Ok(Value::Array(values))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(A::Error::custom("duplicate JSON object key"));
                    }
                    values.insert(key, map.next_value::<UniqueJson>()?.0);
                }
                Ok(Value::Object(values))
            }
        }
        deserializer.deserialize_any(UniqueVisitor).map(UniqueJson)
    }
}

fn decode_unique<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = UniqueJson::deserialize(&mut deserializer)?.0;
    deserializer.end()?;
    serde_json::from_value(value)
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}
#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
    details: Vec<Value>,
}
fn error(status: StatusCode, code: &'static str, message: impl Into<String>) -> Response {
    let mut response = (
        status,
        Json(ErrorBody {
            error: ErrorDetail {
                code,
                message: message.into(),
                details: vec![],
            },
        }),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn private_headers(response: &mut Response, epoch: &str) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(value) = HeaderValue::from_str(epoch) {
        response.headers_mut().insert("x-parley-epoch", value);
    }
}

type AuthError = (StatusCode, Json<ErrorBody>);
fn auth_error(status: StatusCode, code: &'static str, message: &'static str) -> AuthError {
    (
        status,
        Json(ErrorBody {
            error: ErrorDetail {
                code,
                message: message.to_owned(),
                details: vec![],
            },
        }),
    )
}

async fn check_authority(headers: &HeaderMap, state: &AppState) -> Result<(), AuthError> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if host != Some(&state.authority) {
        return Err(auth_error(
            StatusCode::FORBIDDEN,
            "ORIGIN_DENIED",
            "Request authority is not allowed",
        ));
    }
    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        && origin != format!("http://{}", state.authority)
    {
        return Err(auth_error(
            StatusCode::FORBIDDEN,
            "ORIGIN_DENIED",
            "Request origin is not allowed",
        ));
    }
    Ok(())
}

async fn authenticate(headers: &HeaderMap, state: &AppState) -> Result<(), AuthError> {
    check_authority(headers, state).await?;
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let session_id = cookie
        .split(';')
        .filter_map(|part| part.trim().strip_prefix("parley_session="))
        .next();
    let Some(session_id) = session_id else {
        return Err(auth_error(
            StatusCode::UNAUTHORIZED,
            "SESSION_REQUIRED",
            "Sign in to the local workspace",
        ));
    };
    let sessions = state.sessions.lock().await;
    let Some(session) = sessions.get(session_id) else {
        return Err(auth_error(
            StatusCode::UNAUTHORIZED,
            "SESSION_EXPIRED",
            "The local session has expired",
        ));
    };
    if headers
        .get("x-parley-token")
        .and_then(|value| value.to_str().ok())
        != Some(session.token.as_str())
    {
        return Err(auth_error(
            StatusCode::FORBIDDEN,
            "TOKEN_INVALID",
            "Mutation token is invalid",
        ));
    }
    if headers
        .get("x-parley-epoch")
        .and_then(|value| value.to_str().ok())
        != Some(state.epoch.as_str())
    {
        return Err(auth_error(
            StatusCode::CONFLICT,
            "WORKSPACE_REPLACED",
            "Workspace changed; reconnect before continuing",
        ));
    }
    Ok(())
}

async fn authenticate_session(headers: &HeaderMap, state: &AppState) -> Result<String, AuthError> {
    check_authority(headers, state).await?;
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let Some(session_id) = cookie
        .split(';')
        .filter_map(|part| part.trim().strip_prefix("parley_session="))
        .next()
    else {
        return Err(auth_error(
            StatusCode::UNAUTHORIZED,
            "SESSION_REQUIRED",
            "Sign in to the local workspace",
        ));
    };
    if !state.sessions.lock().await.contains_key(session_id) {
        return Err(auth_error(
            StatusCode::UNAUTHORIZED,
            "SESSION_EXPIRED",
            "The local session has expired",
        ));
    }
    Ok(session_id.to_owned())
}

async fn health() -> Json<Value> {
    Json(json!({"status":"ready","model":"not-probed"}))
}

async fn serve_static(State(state): State<AppState>, OriginalUri(uri): OriginalUri) -> Response {
    let relative = uri.path().trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    if relative
        .split('/')
        .any(|part| part == ".." || part.is_empty())
    {
        return error(StatusCode::NOT_FOUND, "NOT_FOUND", "Page does not exist");
    }
    let path = state.static_root.join(relative);
    let Ok(metadata) = std::fs::metadata(&path) else {
        return error(StatusCode::NOT_FOUND, "NOT_FOUND", "Page does not exist");
    };
    if !metadata.is_file() {
        return error(StatusCode::NOT_FOUND, "NOT_FOUND", "Page does not exist");
    }
    let Ok(contents) = std::fs::read(path) else {
        return error(StatusCode::NOT_FOUND, "NOT_FOUND", "Page does not exist");
    };
    let content_type = match relative.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        "png" => "image/png",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    };
    let mut response = Response::new(axum::body::Body::from(contents));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"));
    response
}

#[derive(Deserialize)]
struct BootstrapInput {
    secret: String,
}
async fn bootstrap(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<BootstrapInput>,
) -> Response {
    if let Err(response) = check_authority(&headers, &state).await {
        return response.into_response();
    }
    let mut stored = state.bootstrap.lock().await;
    if !stored
        .remove(&input.secret)
        .is_some_and(|expires| Instant::now() < expires)
    {
        return error(
            StatusCode::UNAUTHORIZED,
            "BOOTSTRAP_INVALID",
            "Bootstrap secret is invalid or expired",
        );
    }
    let session_id = random_hex(32);
    let token = random_hex(32);
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), Session { token });
    let mut response = Json(json!({"ok":true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "parley_session={session_id}; HttpOnly; SameSite=Strict; Path=/api/v1; Max-Age=28800"
        ))
        .unwrap(),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn session(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session_id = match authenticate_session(&headers, &state).await {
        Ok(session_id) => session_id,
        Err(response) => return response.into_response(),
    };
    let sessions = state.sessions.lock().await;
    let Some(record) = sessions.get(&session_id) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "SESSION_EXPIRED",
            "The local session has expired",
        );
    };
    let cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
    let mut response = Json(json!({"workspace_id":state.workspace_id,"epoch":state.epoch,"wire_major":1,"wire_minor":0,"capabilities":["resources.read","resources.write"],"mutation_token":record.token,"cursor":cursor.to_string()})).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    private_headers(&mut response, &state.epoch);
    response
}

#[derive(Deserialize)]
struct EntityQuery {
    kind: Option<String>,
}
#[derive(Serialize, Deserialize, Clone)]
struct Resource {
    id: String,
    kind: String,
    title: String,
    body: Value,
    readiness: String,
    revision: i64,
    etag: String,
}
fn etag(epoch: &str, id: &str, revision: i64) -> String {
    format!("\"p1.{epoch}.resource.{id}.{revision}\"")
}
async fn list_entities(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<EntityQuery>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    if query.kind.as_deref().is_some_and(|kind| kind != "resource") {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_KIND",
            "Only owner resources are available in this slice",
        );
    }
    let rows = match sqlx::query_as::<_, (String,String,String,String,String,i64)>("SELECT id,kind,title,body_json,readiness,revision FROM resource WHERE readiness <> 'retired' ORDER BY updated_at DESC,id ASC")
        .fetch_all(&state.pool).await { Ok(rows) => rows, Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "READ_FAILED", "Could not read resources") };
    let items: Vec<Resource> = rows
        .into_iter()
        .map(|(id, kind, title, body, readiness, revision)| Resource {
            etag: etag(&state.epoch, &id, revision),
            id,
            kind,
            title,
            body: serde_json::from_str(&body).unwrap_or(Value::Null),
            readiness,
            revision,
        })
        .collect();
    let cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
    let mut response = Json(json!({"data":items,"cursor":cursor.to_string()})).into_response();
    response.headers_mut().insert(
        "x-parley-epoch",
        HeaderValue::from_str(&state.epoch).unwrap(),
    );
    response
}

async fn get_entity(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let row = sqlx::query_as::<_, (String, String, String, String, i64)>(
        "SELECT kind,title,body_json,readiness,revision FROM resource WHERE id=?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await;
    let (kind, title, body, readiness, revision) = match row {
        Ok(Some(row)) => row,
        Ok(None) => {
            return error(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "Resource does not exist",
            );
        }
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "READ_FAILED",
                "Could not read resource",
            );
        }
    };
    let resource = Resource {
        etag: etag(&state.epoch, &id, revision),
        id,
        kind,
        title,
        body: serde_json::from_str(&body).unwrap_or(Value::Null),
        readiness,
        revision,
    };
    let entity_etag = resource.etag.clone();
    let mut response = Json(json!({"data":resource})).into_response();
    response
        .headers_mut()
        .insert(header::ETAG, HeaderValue::from_str(&entity_etag).unwrap());
    private_headers(&mut response, &state.epoch);
    response
}

#[derive(Deserialize)]
struct EventsQuery {
    after: Option<String>,
}

async fn get_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let after = match query.after.as_deref().unwrap_or("0").parse::<i64>() {
        Ok(cursor) if cursor >= 0 => cursor,
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "CURSOR_INVALID",
                "Event cursor must be a non-negative decimal",
            );
        }
    };
    let observed: i64 = match sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
        .fetch_one(&state.pool)
        .await
    {
        Ok(cursor) => cursor,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "READ_FAILED",
                "Could not read workspace events",
            );
        }
    };
    if after > observed {
        return error(
            StatusCode::CONFLICT,
            "CURSOR_AHEAD",
            "Event cursor is ahead of the workspace",
        );
    }
    let rows = match sqlx::query_as::<_, (i64,String,String,String,i64,String,String)>(
        "SELECT cursor,event_id,entity_type,entity_id,revision,event_type,payload_json FROM event WHERE cursor>? ORDER BY cursor LIMIT 200",
    ).bind(after).fetch_all(&state.pool).await {
        Ok(rows) => rows,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "READ_FAILED", "Could not read workspace events"),
    };
    let items: Vec<Value> = rows.into_iter().map(|(cursor,event_id,entity_type,entity_id,revision,event_type,payload)| {
        json!({"cursor":cursor.to_string(),"event_id":event_id,"entity_type":entity_type,"entity_id":entity_id,"revision":revision,"event_type":event_type,"payload":serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null)})
    }).collect();
    let next_cursor = items
        .last()
        .and_then(|item| item["cursor"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| after.to_string());
    let mut response = Json(
        json!({"items":items,"next_cursor":next_cursor,"observed_cursor":observed.to_string()}),
    )
    .into_response();
    private_headers(&mut response, &state.epoch);
    response
}

#[derive(Deserialize, Serialize)]
struct CreateInput {
    kind: String,
    title: String,
    body: Value,
}
#[derive(Deserialize, Serialize)]
struct UpdateInput {
    title: String,
    body: Value,
}

async fn create_entity(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    if body.len() > 2 * 1024 * 1024 {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "BODY_TOO_LARGE",
            "Request exceeds 2 MiB",
        );
    }
    let input: CreateInput = match decode_unique(&body) {
        Ok(input) => input,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "INVALID_JSON",
                "Request body is invalid",
            );
        }
    };
    if !["gift", "proof", "artifact"].contains(&input.kind.as_str())
        || input.title.chars().count() > 240
        || !input.body.is_object()
    {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "RESOURCE_INVALID",
            "Resource type or name is invalid",
        );
    }
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
    else {
        return error(
            StatusCode::BAD_REQUEST,
            "IDEMPOTENCY_KEY_REQUIRED",
            "Idempotency-Key is required",
        );
    };
    if key.is_empty() || key.len() > 100 {
        return error(
            StatusCode::BAD_REQUEST,
            "IDEMPOTENCY_KEY_INVALID",
            "Idempotency-Key is invalid",
        );
    }
    let canonical = serde_json::to_vec(&input).unwrap_or_default();
    let digest = hex::encode(Sha256::digest(&canonical));
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "WRITER_BUSY",
                "Workspace is busy",
            );
        }
    };
    if sqlx::query("UPDATE workspace_meta SET revision=revision WHERE singleton=1")
        .execute(&mut *tx)
        .await
        .is_err()
    {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "WRITER_BUSY",
            "Workspace is busy",
        );
    }
    let receipt = sqlx::query_as::<_, (String, String)>(
        "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE epoch=? AND key=?",
    )
    .bind(&state.epoch)
    .bind(key)
    .fetch_optional(&mut *tx)
    .await;
    let receipt = match receipt {
        Ok(receipt) => receipt,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "RECEIPT_READ_FAILED",
                "Could not resolve command status",
            );
        }
    };
    if let Some((old_digest, result)) = receipt {
        if old_digest != digest {
            return error(
                StatusCode::CONFLICT,
                "KEY_REUSED",
                "Idempotency key was used for a different command",
            );
        }
        let value: Value = serde_json::from_str(&result).unwrap_or(Value::Null);
        let _ = tx.rollback().await;
        let mut response = (StatusCode::CREATED, Json(json!({"data":value}))).into_response();
        if let Some(tag) = value
            .get("etag")
            .and_then(Value::as_str)
            .and_then(|value| HeaderValue::from_str(value).ok())
        {
            response.headers_mut().insert(header::ETAG, tag);
        }
        private_headers(&mut response, &state.epoch);
        return response.into_response();
    }
    let id = random_hex(16);
    let now = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";
    let _ = now;
    let body_json = serde_json::to_string(&input.body).unwrap_or_else(|_| "{}".to_owned());
    let result = sqlx::query("INSERT INTO resource(id,kind,title,body_json,readiness,revision,created_at,updated_at) VALUES(?,?,?,?,'draft',1,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))")
        .bind(&id).bind(&input.kind).bind(&input.title).bind(body_json).execute(&mut *tx).await;
    if result.is_err() {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "WRITE_FAILED",
            "Resource was not saved",
        );
    }
    let event_id = random_hex(16);
    if sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),'resource',?,1,'resource.created',?)")
        .bind(event_id).bind(&id).bind(json!({"kind":input.kind}).to_string()).execute(&mut *tx).await.is_err() { return error(StatusCode::INTERNAL_SERVER_ERROR,"WRITE_FAILED","Resource was not saved"); }
    let cursor: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
        .fetch_one(&mut *tx)
        .await
        .unwrap_or(0);
    let resource = Resource {
        etag: etag(&state.epoch, &id, 1),
        id,
        kind: input.kind,
        title: input.title,
        body: input.body,
        readiness: "draft".to_owned(),
        revision: 1,
    };
    let result_json = serde_json::to_string(&resource).unwrap_or_else(|_| "{}".to_owned());
    if sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,201,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'))")
        .bind(&state.epoch).bind(key).bind(digest).bind(&result_json).bind(cursor).execute(&mut *tx).await.is_err() { return error(StatusCode::INTERNAL_SERVER_ERROR,"WRITE_FAILED","Resource was not saved"); }
    if tx.commit().await.is_err() {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "WRITE_FAILED",
            "Resource was not saved",
        );
    }
    let mut response = (StatusCode::CREATED, Json(json!({"data":resource}))).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag(&state.epoch, &resource.id, 1)).unwrap(),
    );
    private_headers(&mut response, &state.epoch);
    response
}

async fn update_entity(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
    else {
        return error(
            StatusCode::BAD_REQUEST,
            "IDEMPOTENCY_KEY_REQUIRED",
            "Idempotency-Key is required",
        );
    };
    if key.is_empty() || key.len() > 100 {
        return error(
            StatusCode::BAD_REQUEST,
            "IDEMPOTENCY_KEY_INVALID",
            "Idempotency-Key is invalid",
        );
    }
    let Some(if_match) = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
    else {
        return error(
            StatusCode::PRECONDITION_REQUIRED,
            "ETAG_REQUIRED",
            "If-Match is required",
        );
    };
    if if_match.len() < 2
        || !if_match.starts_with('"')
        || !if_match.ends_with('"')
        || if_match.starts_with("W/")
        || if_match == "*"
    {
        return error(
            StatusCode::BAD_REQUEST,
            "ETAG_INVALID",
            "If-Match must be a strong resource ETag",
        );
    }
    if body.len() > 2 * 1024 * 1024 {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "BODY_TOO_LARGE",
            "Request exceeds 2 MiB",
        );
    }
    let input: UpdateInput = match decode_unique(&body) {
        Ok(input) => input,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "INVALID_JSON",
                "Request body is invalid",
            );
        }
    };
    if input.title.chars().count() > 240 || !input.body.is_object() {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "RESOURCE_INVALID",
            "Resource name or fields are invalid",
        );
    }
    let canonical = serde_json::to_vec(&(&id, &if_match, &input)).unwrap_or_default();
    let digest = hex::encode(Sha256::digest(canonical));
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "WRITER_BUSY",
                "Workspace is busy",
            );
        }
    };
    if sqlx::query("UPDATE workspace_meta SET revision=revision WHERE singleton=1")
        .execute(&mut *tx)
        .await
        .is_err()
    {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "WRITER_BUSY",
            "Workspace is busy",
        );
    }
    let receipt = sqlx::query_as::<_, (String, String)>(
        "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE epoch=? AND key=?",
    )
    .bind(&state.epoch)
    .bind(key)
    .fetch_optional(&mut *tx)
    .await;
    let receipt = match receipt {
        Ok(receipt) => receipt,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "RECEIPT_READ_FAILED",
                "Could not resolve command status",
            );
        }
    };
    if let Some((old_digest, result)) = receipt {
        if old_digest != digest {
            return error(
                StatusCode::CONFLICT,
                "KEY_REUSED",
                "Idempotency key was used for a different command",
            );
        }
        let value: Value = serde_json::from_str(&result).unwrap_or(Value::Null);
        let _ = tx.rollback().await;
        let mut response = (StatusCode::OK, Json(json!({"data":value}))).into_response();
        private_headers(&mut response, &state.epoch);
        return response;
    }
    let current = sqlx::query_as::<_, (String, String, String, i64)>(
        "SELECT kind,title,body_json,revision FROM resource WHERE id=?",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await;
    let Ok(Some((kind, _title, old_body, revision))) = current else {
        return error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Resource does not exist",
        );
    };
    if if_match != etag(&state.epoch, &id, revision) {
        return error(
            StatusCode::PRECONDITION_FAILED,
            "STALE_REVISION",
            "Resource changed in another session",
        );
    }
    let next_revision = revision + 1;
    let body_json = serde_json::to_string(&input.body).unwrap_or_else(|_| "{}".to_owned());
    if sqlx::query("UPDATE resource SET title=?,body_json=?,revision=?,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?").bind(&input.title).bind(&body_json).bind(next_revision).bind(&id).execute(&mut *tx).await.is_err(){return error(StatusCode::INTERNAL_SERVER_ERROR,"WRITE_FAILED","Resource was not saved");}
    let event_id = random_hex(16);
    if sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),'resource',?,?,'resource.updated','{}')").bind(event_id).bind(&id).bind(next_revision).execute(&mut *tx).await.is_err(){return error(StatusCode::INTERNAL_SERVER_ERROR,"WRITE_FAILED","Resource was not saved");}
    let cursor: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
        .fetch_one(&mut *tx)
        .await
        .unwrap_or(0);
    let resource = Resource {
        etag: etag(&state.epoch, &id, next_revision),
        id: id.clone(),
        kind,
        title: input.title,
        body: input.body,
        readiness: "draft".to_owned(),
        revision: next_revision,
    };
    let result_json = serde_json::to_string(&resource).unwrap_or_else(|_| "{}".to_owned());
    if sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,200,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'))").bind(&state.epoch).bind(key).bind(digest).bind(&result_json).bind(cursor).execute(&mut *tx).await.is_err(){return error(StatusCode::INTERNAL_SERVER_ERROR,"WRITE_FAILED","Resource was not saved");}
    let _ = old_body;
    if tx.commit().await.is_err() {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "WRITE_FAILED",
            "Resource was not saved",
        );
    }
    let mut response = (StatusCode::OK, Json(json!({"data":resource}))).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag(&state.epoch, &id, next_revision)).unwrap(),
    );
    private_headers(&mut response, &state.epoch);
    response
}
