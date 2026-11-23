//! Local Parley HTTP service and transactional workspace commands.
//!
//! The browser is a client of this service. Resource writes, revisions,
//! events, and idempotency receipts share one SQLite transaction. Sessions
//! are created by redeeming a one-time bootstrap secret issued by the local
//! launcher; every private command then presents its cookie, per-session
//! mutation token and workspace epoch (RFC-0004 IQ1/IQ2, RFC-0002 BQ1).

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path as AxumPath, Query, RawQuery, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use rand::random;
use serde::de::{DeserializeOwned, Error as DeError, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    Connection, Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::fmt;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

static IDS: AtomicU64 = AtomicU64::new(0);

/// Move a Draft-00 database out of the namespace used by current migrations.
/// Its rows remain intact as an audit source; conversion below only promotes
/// unambiguous party identities and records explicit source-key mappings.
async fn quarantine_draft00_schema(pool: &SqlitePool) -> Result<bool, WorkspaceError> {
    let has_party: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='party'",
    )
    .fetch_one(pool)
    .await?;
    let has_meta: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='workspace_meta'",
    )
    .fetch_one(pool)
    .await?;
    if has_party == 0 || has_meta != 0 {
        return Ok(false);
    }

    let mut connection = pool.acquire().await?;
    sqlx::query("PRAGMA foreign_keys=OFF")
        .execute(&mut *connection)
        .await?;
    let result = async {
        let mut tx = connection.begin_with("BEGIN IMMEDIATE").await?;
        let triggers: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='trigger' AND name NOT LIKE 'sqlite_%'",
        )
        .fetch_all(&mut *tx)
        .await?;
        for trigger in triggers {
            let quoted = trigger.replace('"', "\"\"");
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP TRIGGER IF EXISTS \"{quoted}\"")))
                .execute(&mut *tx)
                .await?;
        }
        let indexes: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='index' AND sql IS NOT NULL AND name NOT LIKE 'sqlite_%'",
        )
        .fetch_all(&mut *tx)
        .await?;
        for index in indexes {
            let quoted = index.replace('"', "\"\"");
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP INDEX IF EXISTS \"{quoted}\"")))
                .execute(&mut *tx)
                .await?;
        }
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(&mut *tx)
        .await?;
        for table in tables {
            if table.starts_with("draft00_") {
                return Err(WorkspaceError::Invalid(
                    "legacy Draft-00 conversion namespace already exists".into(),
                ));
            }
            let quoted = table.replace('"', "\"\"");
            let renamed = format!("draft00_{table}").replace('"', "\"\"");
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("ALTER TABLE \"{quoted}\" RENAME TO \"{renamed}\"")))
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok::<(), WorkspaceError>(())
    }
    .await;
    let enable = sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut *connection)
        .await;
    result?;
    enable?;
    Ok(true)
}

/// Deterministic Draft-00 party conversion. Legacy bytes are retained in
/// draft00_* tables; incomplete/unrecognized rows remain visible as diagnostics.
async fn convert_draft00_parties(pool: &SqlitePool) -> Result<(), WorkspaceError> {
    let present: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='draft00_party'",
    )
    .fetch_one(pool)
    .await?;
    if present == 0 {
        return Ok(());
    }
    let columns: Vec<String> = sqlx::query("PRAGMA table_info(draft00_party)")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| r.get::<String, _>("name"))
        .collect();
    let rows = sqlx::query("SELECT * FROM draft00_party")
        .fetch_all(pool)
        .await?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    for row in rows {
        let source_key = legacy_value(&row, &columns, "id").unwrap_or_default();
        let name = legacy_value(&row, &columns, "name").unwrap_or_default();
        let kind = legacy_value(&row, &columns, "kind").unwrap_or_else(|| "person".into());
        if source_key.is_empty()
            || name.trim().is_empty()
            || !["person", "org"].contains(&kind.as_str())
        {
            sqlx::query("INSERT INTO legacy_conversion_diagnostic(source_format,source_table,source_key,code,detail,created_at) VALUES('draft-00','party',?,'AMBIGUOUS_PARTY','Missing stable key/name or unsupported party kind',?)")
                .bind(source_key).bind(now_utc()).execute(&mut *tx).await?;
            continue;
        }
        let identity = format!("draft-00\0party\0{source_key}");
        let entity_id = format!(
            "legacy-{}",
            hex::encode(Sha256::digest(identity.as_bytes()))
        );
        let opportunity_id = format!(
            "legacy-opportunity-{}",
            hex::encode(Sha256::digest(
                format!("draft-00\0opportunity\0{source_key}").as_bytes()
            ))
        );
        let stage_class =
            legacy_value(&row, &columns, "stage_class").unwrap_or_else(|| "identified".into());
        let state = match stage_class.as_str() {
            "resolved" => "resolved",
            "dormant" => "dormant",
            "closed" => "closed",
            _ => "open",
        };
        let dnc = legacy_value(&row, &columns, "do_not_contact")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
            .clamp(0, 1);
        let rung = legacy_value(&row, &columns, "highest_rung")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
            .max(0);
        let touches = legacy_value(&row, &columns, "touches")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
            .max(0);
        let stamp = now_utc();
        sqlx::query(
            "INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES(?,?,?,1,?,?)",
        )
        .bind(&entity_id)
        .bind(&kind)
        .bind(name.trim())
        .bind(&stamp)
        .bind(&stamp)
        .execute(&mut *tx)
        .await?;
        let policy_state = if state == "resolved" { "closed" } else { state };
        sqlx::query("INSERT INTO party_policy(party_id,state,do_not_contact,highest_rung,touch_count,revision) VALUES(?,?,?,?,?,1)")
            .bind(&entity_id).bind(policy_state).bind(dnc).bind(rung).bind(touches).execute(&mut *tx).await?;
        let title = format!("Legacy: {}", name.trim());
        sqlx::query("INSERT INTO opportunity(id,title,state,close_reason,revision,created_at,updated_at) VALUES(?,?,?, ?,1,?,?)")
            .bind(&opportunity_id).bind(title).bind(state).bind(if state == "open" { None } else { Some("draft00_migration") }).bind(&stamp).bind(&stamp).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES(?,?,'primary',?)")
            .bind(&opportunity_id).bind(&entity_id).bind(&stamp).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO legacy_key_map(source_format,source_table,source_key,destination_id,status) VALUES('draft-00','party',?,?,'converted')")
            .bind(&source_key).bind(&entity_id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO legacy_key_map(source_format,source_table,source_key,destination_id,status) VALUES('draft-00','opportunity',?,?,'converted')")
            .bind(source_key).bind(opportunity_id).execute(&mut *tx).await?;
    }
    // Mark untranslated legacy tables explicitly, without inventing facts,
    // provenance, recipient consent, or reply deadlines.
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'draft00_%'",
    )
    .fetch_all(&mut *tx)
    .await?;
    for table in tables.into_iter().filter(|t| t != "draft00_party") {
        sqlx::query("INSERT INTO legacy_conversion_diagnostic(source_format,source_table,code,detail,created_at) VALUES('draft-00',?,'RETAINED_UNMAPPED','Original rows retained; no safe automatic semantic conversion is defined for this record type',?)")
            .bind(table.trim_start_matches("draft00_")).bind(now_utc()).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

fn legacy_value(row: &sqlx::sqlite::SqliteRow, columns: &[String], field: &str) -> Option<String> {
    if !columns.iter().any(|c| c == field) {
        return None;
    }
    if let Ok(value) = row.try_get::<String, _>(field) {
        return Some(value);
    }
    if let Ok(value) = row.try_get::<i64, _>(field) {
        return Some(value.to_string());
    }
    None
}

mod agent_runtime;
mod application;
mod assistant;
mod blob;
mod board;
mod commands;
mod generation;
mod jobs;
mod maintenance;
mod parties;
pub mod relations;
pub use maintenance::{MaintenanceSettings, MaintenanceSettingsInput};
#[doc(hidden)]
pub use maintenance::{create_backup_for_test, prune_backups_for_test};
mod materials;
mod queue_sheet;
mod queueing;
mod release;
mod resource_board;
mod thread;
mod writing;
mod writing_jobs;
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
    #[error("record does not exist")]
    NotFound,
    #[error("the record changed in another session")]
    StaleRevision,
    #[error("command ledger capacity is exhausted")]
    LedgerFull,
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
    pub focus: String,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EntityNotes {
    pub entity_id: String,
    pub markdown: String,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, sqlx::FromRow)]
pub struct DossierLink {
    pub id: String,
    pub from_id: String,
    pub to_id: String,
    pub verb: String,
    pub revision: i64,
    pub tier: Option<i64>,
    pub wording: Option<String>,
    pub claim_id: Option<String>,
    pub claim_revision: Option<i64>,
    pub speaker_kind: Option<String>,
    pub speaker_entity_id: Option<String>,
    pub occurrence_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DossierFile {
    pub id: String,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
    pub sha256: String,
    pub share_permission: bool,
    pub reviewed_by_owner: bool,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DossierOpportunity {
    pub id: String,
    pub title: String,
    pub role: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DossierResource {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub opportunity_id: Option<String>,
    pub opportunity_title: Option<String>,
    pub paths: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EntityDossier {
    pub entity: EntityRecord,
    pub direct_links: Vec<DossierLink>,
    pub import_origin: Option<Value>,
    pub retired_at: Option<String>,
    pub files: Vec<DossierFile>,
    pub opportunities: Vec<DossierOpportunity>,
    pub associated_resources: Vec<DossierResource>,
}

#[derive(Debug, Serialize)]
struct GraphNode {
    id: String,
    kind: String,
    name: String,
    revision: i64,
    x: f64,
    y: f64,
    position_revision: i64,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct GraphEdge {
    id: String,
    from_id: String,
    to_id: String,
    verb: String,
    revision: i64,
    occurrence_id: Option<String>,
    quote: String,
    owner_authored: bool,
    tier: i64,
    speaker_kind: String,
    captured_at: String,
    source_title: Option<String>,
    source_url: Option<String>,
    claim_id: Option<String>,
    claim_revision: Option<i64>,
    speaker_entity_id: Option<String>,
    #[sqlx(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    record: Option<sqlx::types::Json<resource_board::ResourceConnectionRecord>>,
}

#[derive(Debug, Serialize)]
struct GraphSnapshot {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    omitted_nodes: i64,
    omitted_edges: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GraphLayoutInput {
    view_scope: String,
    view_id: String,
    positions: Vec<GraphPositionInput>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GraphPositionInput {
    entity_id: String,
    x: f64,
    y: f64,
    expected_revision: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct GraphLayoutResult {
    positions: Vec<GraphPositionResult>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GraphPositionResult {
    entity_id: String,
    x: f64,
    y: f64,
    revision: i64,
}

/// A running local service. The bootstrap fragment is short lived at the
/// application boundary; callers must remove it from browser history at once.
pub struct RunningService {
    pub address: SocketAddr,
    pub bootstrap_secret: String,
    bootstrap: Arc<Mutex<HashMap<String, Instant>>>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
    restart: tokio::sync::watch::Receiver<bool>,
    restart_sender: tokio::sync::watch::Sender<bool>,
    background: Vec<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
pub struct BootstrapIssuer {
    bootstrap: Arc<Mutex<HashMap<String, Instant>>>,
    pub address: SocketAddr,
}

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
        self.restart_sender.send_replace(true);
        let _ = self.shutdown.send(());
        let _ = self.task.await;
        for task in self.background {
            let _ = task.await;
        }
    }
    pub async fn wait_for_restart(&mut self) {
        let _ = self.restart.changed().await;
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

#[derive(Clone)]
pub struct Workspace {
    pool: SqlitePool,
    epoch: String,
    workspace_id: String,
    pub(crate) blob_root: PathBuf,
    pub(crate) maintenance_lock: Arc<Mutex<()>>,
    pub(crate) spool: blob::ReleaseSpool,
    root: generation::WorkspaceRoot,
    database_path: PathBuf,
    /// Private generation key for command ledger HMACs. Kept outside the
    /// database and excluded from backups/exports; rotated on replacement.
    command_secret: Arc<std::sync::RwLock<Vec<u8>>>,
    /// Adopted-operation generation token. A generation publisher holds the
    /// exclusive lock; writer jobs hold the shared lock.
    generation: Arc<tokio::sync::RwLock<u64>>,
    /// One active local inference across fixed roles and delegated runs.
    inference: Arc<tokio::sync::Semaphore>,
}

impl Workspace {
    pub fn resolve_database_path(identity: &Path) -> Result<PathBuf, WorkspaceError> {
        let root = generation::WorkspaceRoot::new(identity).map_err(WorkspaceError::Invalid)?;
        let id = root.read_selector().map_err(WorkspaceError::Invalid)?;
        root.database_path(&id).map_err(WorkspaceError::Invalid)
    }
    pub fn control_directory(identity: &Path) -> Result<PathBuf, WorkspaceError> {
        let r = generation::WorkspaceRoot::new(identity).map_err(WorkspaceError::Invalid)?;
        r.ensure_control().map_err(WorkspaceError::Invalid)?;
        Ok(r.control_path().to_path_buf())
    }
    /// Open and migrate a private SQLite workspace. Every connection enables
    /// foreign keys and uses FULL synchronous durability in WAL mode.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let identity = path.as_ref();
        let root = generation::WorkspaceRoot::new(identity).map_err(WorkspaceError::Invalid)?;
        let active = root
            .prepare_active()
            .await
            .map_err(WorkspaceError::Invalid)?;
        let path = active.database.as_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(2));
        // UCR-001 §5: at most four read/pool connections plus the dedicated
        // adopted writer connection created in start_workspace.
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // The launcher owns workspace-directory policy; opening a workspace
            // inside an existing shared directory (such as the system temporary
            // directory) protects the database file itself.
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        }
        let legacy_draft00 = quarantine_draft00_schema(&pool).await?;
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        if legacy_draft00 {
            convert_draft00_parties(&pool).await?;
        }
        assistant::initialize_defaults(&pool).await?;
        let (epoch, workspace_id): (String, String) =
            sqlx::query_as("SELECT epoch, workspace_id FROM workspace_meta WHERE singleton=1")
                .fetch_one(&pool)
                .await?;
        let blob_root = root
            .blob_path(&active.id)
            .map_err(WorkspaceError::Invalid)?;
        std::fs::create_dir_all(&blob_root)
            .map_err(|e| WorkspaceError::Invalid(format!("create blob store: {e}")))?;
        std::fs::create_dir_all(blob_root.join(".staging"))
            .map_err(|e| WorkspaceError::Invalid(format!("create blob staging: {e}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&blob_root, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            std::fs::set_permissions(
                blob_root.join(".staging"),
                std::fs::Permissions::from_mode(0o700),
            )
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        }
        if active.needs_publish {
            root.publish_initial(&active.id)
                .map_err(WorkspaceError::Invalid)?;
        }
        let removed_generation_bytes = root
            .reconcile_unselected()
            .map_err(WorkspaceError::Invalid)?;
        let command_secret = Arc::new(std::sync::RwLock::new(load_command_secret(&root)?));
        let generation = Arc::new(tokio::sync::RwLock::new(1u64));
        let workspace = Self {
            pool,
            epoch,
            workspace_id,
            blob_root,
            maintenance_lock: Arc::new(Mutex::new(())),
            spool: blob::ReleaseSpool::new(),
            root,
            database_path: path.to_path_buf(),
            command_secret,
            generation,
            inference: Arc::new(tokio::sync::Semaphore::new(1)),
        };
        maintenance::initialize_settings(&workspace).await?;
        maintenance::complete_startup_operations(&workspace, removed_generation_bytes).await?;
        // Finalize attempts from an earlier process that lack a final audit
        // record (UCR-001 Q3).
        let interrupted = commands::ledger::recover_interrupted_attempts(&workspace.pool)
            .await
            .map_err(|e| {
                WorkspaceError::Invalid(format!("recover interrupted command attempts: {e}"))
            })?;
        if interrupted > 0 {
            eprintln!("command ledger: finalized {interrupted} interrupted attempts");
        }
        Ok(workspace)
    }

    pub fn blob_root(&self) -> &Path {
        &self.blob_root
    }
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }
    pub(crate) fn generation_root(&self) -> &generation::WorkspaceRoot {
        &self.root
    }

    pub fn spool(&self) -> blob::ReleaseSpool {
        self.spool.clone()
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Snapshot of the current private generation key. Held only for the
    /// duration of one digest computation; replacement rotates it in memory
    /// as well as on disk.
    pub fn command_secret(&self) -> Vec<u8> {
        self.command_secret
            .read()
            .expect("generation key lock poisoned")
            .clone()
    }

    /// Rotate the private generation key on workspace replacement. New
    /// receipts are HMACed under the new key; old-epoch receipts are already
    /// unreachable because the epoch changed with the replacement.
    pub(crate) fn rotate_command_secret(&self) {
        match crate::rotate_command_secret(&self.root) {
            Ok(bytes) => {
                let mut guard = self
                    .command_secret
                    .write()
                    .expect("generation key lock poisoned");
                *guard = bytes;
            }
            Err(error) => eprintln!("command ledger: key rotation failed: {error}"),
        }
    }

    pub async fn generation_token(&self) -> u64 {
        *self.generation.read().await
    }

    /// The single local inference lane. Fixed-role jobs and delegated runs
    /// share this permit; waiting runs release it while awaiting owner input.
    pub(crate) fn inference(&self) -> Arc<tokio::sync::Semaphore> {
        self.inference.clone()
    }

    pub async fn bump_generation(&self) {
        let mut token = self.generation.write().await;
        *token += 1;
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
        if request.title.chars().count() > 240 {
            return Err(WorkspaceError::Invalid(
                "title exceeds 240 characters".into(),
            ));
        }
        if !request.body.is_object() {
            return Err(WorkspaceError::Invalid(
                "resource body must be an object".into(),
            ));
        }
        let body = serde_json::to_string(&request.body)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let canonical = digest(format!("{}\0{}\0{}", request.kind, request.title, body).as_bytes());
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        )
        .bind(&self.epoch)
        .bind(&request.idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            if old_digest != canonical {
                return Err(WorkspaceError::IdempotencyConflict);
            }
            let record =
                serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
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
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,201,?,?,?)")
            .bind(&self.epoch).bind(&request.idempotency_key).bind(canonical).bind(&result_json).bind(cursor).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Conditional draft edit. The receipt is bound to the addressed record,
    /// the presented ETag and the typed body; a stale precondition fails
    /// without consuming the key so the corrected command can reuse it.
    pub async fn update_resource(
        &self,
        idempotency_key: &str,
        id: &str,
        if_match: &str,
        title: &str,
        body: &serde_json::Value,
    ) -> Result<ResourceRecord, WorkspaceError> {
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 200 {
            return Err(WorkspaceError::Invalid(
                "idempotency key must contain 1–200 bytes".into(),
            ));
        }
        if title.chars().count() > 240 {
            return Err(WorkspaceError::Invalid(
                "title exceeds 240 characters".into(),
            ));
        }
        if !body.is_object() {
            return Err(WorkspaceError::Invalid(
                "resource body must be an object".into(),
            ));
        }
        let body_json =
            serde_json::to_string(body).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let canonical = digest(format!("{id}\0{if_match}\0{title}\0{body_json}").as_bytes());
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        )
        .bind(&self.epoch)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            if old_digest != canonical {
                return Err(WorkspaceError::IdempotencyConflict);
            }
            let record =
                serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            tx.commit().await?;
            return Ok(record);
        }
        let row = sqlx::query_as::<_, (String, String, String, String, i64)>(
            "SELECT kind,title,body_json,readiness,revision FROM resource WHERE id=? AND readiness <> 'retired'",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((kind, _title, _body_json, readiness, revision)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        if if_match != resource_etag(&self.epoch, id, revision) {
            return Err(WorkspaceError::StaleRevision);
        }
        let next_revision = revision + 1;
        let now = now_utc();
        sqlx::query("UPDATE resource SET title=?,body_json=?,revision=?,updated_at=? WHERE id=?")
            .bind(title)
            .bind(&body_json)
            .bind(next_revision)
            .bind(&now)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        let event_id = new_id(id);
        let updated = ResourceRecord {
            id: id.to_owned(),
            kind,
            title: title.to_owned(),
            body: body.clone(),
            readiness,
            revision: next_revision,
        };
        let result_json =
            serde_json::to_string(&updated).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,'resource',?,?,'resource.updated','{}')")
            .bind(event_id).bind(&now).bind(id).bind(next_revision).execute(&mut *tx).await?;
        let cursor: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,200,?,?,?)")
            .bind(&self.epoch).bind(idempotency_key).bind(canonical).bind(&result_json).bind(cursor).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Saved owner resources for directory reads, optionally narrowed to
    /// resource kinds. Ordered most recently updated first with a stable ID
    /// tiebreaker.
    pub async fn list_resources_filtered(
        &self,
        kinds: &[&str],
    ) -> Result<Vec<ResourceRecord>, WorkspaceError> {
        let mut builder = sqlx::QueryBuilder::new(
            "SELECT id,kind,title,body_json,readiness,revision FROM resource WHERE readiness <> 'retired'",
        );
        if !kinds.is_empty() {
            builder.push(" AND kind IN (");
            let mut separated = builder.separated(", ");
            for kind in kinds {
                match *kind {
                    "resource" => {
                        separated.push_bind("gift");
                        separated.push_bind("proof");
                        separated.push_bind("artifact");
                    }
                    other => {
                        separated.push_bind(other);
                    }
                };
            }
            builder.push(")");
        }
        builder.push(" ORDER BY updated_at DESC,id ASC");
        let rows = builder
            .build_query_as::<(String, String, String, String, String, i64)>()
            .fetch_all(&self.pool)
            .await?;
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

    /// Directory entities, optionally narrowed to entity kinds. Ordered by
    /// normalized name with a stable ID tiebreaker.
    pub async fn list_entities_filtered(
        &self,
        kinds: &[&str],
    ) -> Result<Vec<EntityRecord>, WorkspaceError> {
        let mut builder =
            sqlx::QueryBuilder::new("SELECT id,kind,name,focus,revision FROM graph_card");
        if !kinds.is_empty() {
            builder.push(" WHERE kind IN (");
            let mut separated = builder.separated(", ");
            for kind in kinds {
                separated.push_bind(*kind);
            }
            builder.push(")");
        }
        builder.push(" ORDER BY name COLLATE NOCASE,id");
        let rows = builder
            .build_query_as::<(String, String, String, String, i64)>()
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|(id, kind, name, focus, revision)| EntityRecord {
                id,
                kind,
                name,
                focus,
                revision,
            })
            .collect())
    }

    /// One directory entity by ID (person, organization, project or resource
    /// kind entity). None when the ID is unknown or belongs to a resource.
    pub async fn get_entity(&self, id: &str) -> Result<Option<EntityRecord>, WorkspaceError> {
        let row = sqlx::query_as::<_, (String, String, String, String, i64)>(
            "SELECT e.id,e.kind,coalesce(r.title,e.name),e.focus,e.revision FROM entity e LEFT JOIN resource r ON r.id=e.id WHERE e.id=?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id, kind, name, focus, revision)| EntityRecord {
            id,
            kind,
            name,
            focus,
            revision,
        }))
    }

    /// Conditional typed party edit. `name` and `focus` are the editable sheet
    /// fields; the strong entity ETag is the write scope. Blank names and
    /// missing update fields are rejected; subtype transitions stay out of
    /// ordinary label editing.
    pub async fn update_entity(
        &self,
        idempotency_key: &str,
        id: &str,
        if_match: &str,
        name: Option<String>,
        focus: Option<String>,
    ) -> Result<EntityRecord, WorkspaceError> {
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 200 {
            return Err(WorkspaceError::Invalid(
                "idempotency key must contain 1–200 bytes".into(),
            ));
        }
        if !is_strong_etag(if_match) {
            return Err(WorkspaceError::Invalid(
                "If-Match must be a strong entity ETag".into(),
            ));
        }
        let name = match name {
            Some(name) => {
                let trimmed = name.trim();
                if trimmed.is_empty() || trimmed.len() > 500 {
                    return Err(WorkspaceError::Invalid(
                        "entity name must contain 1–500 bytes".into(),
                    ));
                }
                Some(trimmed.to_owned())
            }
            None => None,
        };
        let focus = match focus {
            Some(focus) => {
                if focus.chars().count() > 250 {
                    return Err(WorkspaceError::Invalid(
                        "focus exceeds 250 characters".into(),
                    ));
                }
                Some(focus)
            }
            None => None,
        };
        if name.is_none() && focus.is_none() {
            return Err(WorkspaceError::Invalid("no entity fields to update".into()));
        }
        let canonical = digest(format!("{id}\0{if_match}\0{name:?}\0{focus:?}").as_bytes());
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        )
        .bind(&self.epoch)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            if old_digest != canonical {
                return Err(WorkspaceError::IdempotencyConflict);
            }
            let record =
                serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            tx.commit().await?;
            return Ok(record);
        }
        let row = sqlx::query_as::<_, (String, String, String, i64)>(
            "SELECT kind,name,focus,revision FROM graph_card WHERE id=?",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((kind, current_name, current_focus, revision)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        if if_match != entity_etag(&self.epoch, id, revision) {
            return Err(WorkspaceError::StaleRevision);
        }
        let next_name = name.unwrap_or(current_name);
        let next_focus = focus.unwrap_or(current_focus);
        let next_revision = revision + 1;
        let now = now_utc();
        let resource: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM resource WHERE id=?)")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if resource && next_name.chars().count() > 240 {
            return Err(WorkspaceError::Invalid(
                "Resource title exceeds 240 characters.".into(),
            ));
        }
        if resource {
            sqlx::query("UPDATE resource SET title=?,revision=revision+1,updated_at=? WHERE id=? AND title<>?").bind(&next_name).bind(&now).bind(id).bind(&next_name).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE entity SET name=CASE WHEN EXISTS(SELECT 1 FROM resource r WHERE r.id=entity.id) THEN '' ELSE ? END,focus=?,revision=?,updated_at=? WHERE id=?")
            .bind(&next_name)
            .bind(&next_focus)
            .bind(next_revision)
            .bind(&now)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        let updated = EntityRecord {
            id: id.to_owned(),
            kind,
            name: next_name,
            focus: next_focus,
            revision: next_revision,
        };
        let result_json =
            serde_json::to_string(&updated).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,'entity',?,?,'entity.updated','{}')")
            .bind(new_id(id)).bind(&now).bind(id).bind(next_revision).execute(&mut *tx).await?;
        let cursor: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,200,?,?,?)")
            .bind(&self.epoch).bind(idempotency_key).bind(canonical).bind(&result_json).bind(cursor).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Canonical entity notes (UTF-8, LF line endings) with their independent
    /// revision. Returns `{entity_id, markdown, revision}` when the entity
    /// exists (notes default to empty at revision 1).
    pub async fn get_entity_notes(&self, id: &str) -> Result<Option<EntityNotes>, WorkspaceError> {
        let row =
            sqlx::query_as::<_, (String, i64)>("SELECT notes_md,notes_rev FROM entity WHERE id=?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(markdown, revision)| EntityNotes {
            entity_id: id.to_owned(),
            markdown,
            revision,
        }))
    }

    /// Replace canonical entity notes under the notes-scoped strong ETag and
    /// the body's expected revision. A stale notes revision fails without
    /// consuming the idempotency key.
    pub async fn update_entity_notes(
        &self,
        idempotency_key: &str,
        id: &str,
        if_match: &str,
        expected_notes_rev: i64,
        markdown: &str,
    ) -> Result<EntityNotes, WorkspaceError> {
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 200 {
            return Err(WorkspaceError::Invalid(
                "idempotency key must contain 1–200 bytes".into(),
            ));
        }
        if !is_strong_etag(if_match) {
            return Err(WorkspaceError::Invalid(
                "If-Match must be a strong notes ETag".into(),
            ));
        }
        if markdown.len() > 512 * 1024 {
            return Err(WorkspaceError::Invalid("notes exceed 512 KiB".into()));
        }
        let canonical_markdown = canonical_markdown(markdown);
        let canonical = digest(
            format!("{id}\0{if_match}\0{expected_notes_rev}\0{canonical_markdown}").as_bytes(),
        );
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        )
        .bind(&self.epoch)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            if old_digest != canonical {
                return Err(WorkspaceError::IdempotencyConflict);
            }
            let notes =
                serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            tx.commit().await?;
            return Ok(notes);
        }
        let row = sqlx::query_as::<_, (i64,)>(
            "SELECT notes_rev FROM entity WHERE id=? AND retired_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((current_rev,)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        if expected_notes_rev != current_rev || if_match != notes_etag(&self.epoch, id, current_rev)
        {
            return Err(WorkspaceError::StaleRevision);
        }
        let next_rev = current_rev + 1;
        let now = now_utc();
        sqlx::query("UPDATE entity SET notes_md=?,notes_rev=?,updated_at=? WHERE id=?")
            .bind(&canonical_markdown)
            .bind(next_rev)
            .bind(&now)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        let notes = EntityNotes {
            entity_id: id.to_owned(),
            markdown: canonical_markdown,
            revision: next_rev,
        };
        let result_json =
            serde_json::to_string(&notes).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,'entity',?,?,'entity.notes_updated','{}')")
            .bind(new_id(id)).bind(&now).bind(id).bind(next_rev).execute(&mut *tx).await?;
        let cursor: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,200,?,?,?)")
            .bind(&self.epoch).bind(idempotency_key).bind(canonical).bind(&result_json).bind(cursor).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(notes)
    }

    /// Inspector dossier for one party: direct attributed links, files bound
    /// to the card, open opportunities involving the party, and resources used
    /// by visible opportunities, each bounded and deduplicated by ID.
    pub async fn entity_dossier(&self, id: &str) -> Result<Option<EntityDossier>, WorkspaceError> {
        let entity = match self.get_entity(id).await? {
            Some(entity) => entity,
            None => return Ok(None),
        };
        let direct_links=sqlx::query_as::<_,DossierLink>("SELECT eg.id,eg.from_id,eg.to_id,eg.verb,eg.revision,cr.tier,cr.wording,cr.claim_id,cr.revision claim_revision,cr.speaker_kind,cr.speaker_entity_id,cr.occurrence_id
            FROM edge eg LEFT JOIN edge_evidence ee ON ee.rowid=(SELECT latest.rowid FROM edge_evidence latest WHERE latest.edge_id=eg.id ORDER BY latest.rowid DESC LIMIT 1)
            LEFT JOIN claim c ON c.id=ee.claim_id LEFT JOIN claim_revision cr ON cr.claim_id=c.id AND cr.revision=c.current_revision
            WHERE eg.state='active' AND (eg.from_id=? OR eg.to_id=?) ORDER BY eg.created_at DESC,eg.id LIMIT 60")
            .bind(id).bind(id).fetch_all(&self.pool).await?;
        let files = sqlx::query_as::<_, (String, String, String, i64, String, i64, i64, String)>(
            "SELECT a.id,a.filename,a.content_type,a.size,a.sha256,u.share_permission,u.reviewed_by_owner,a.created_at \
             FROM entity_attachment a JOIN blob_upload u ON u.id=a.upload_id \
             WHERE a.entity_id=? ORDER BY a.created_at DESC LIMIT 60",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let files = files
            .into_iter()
            .map(
                |(
                    file_id,
                    filename,
                    content_type,
                    size,
                    sha256,
                    share_permission,
                    reviewed_by_owner,
                    created_at,
                )| {
                    DossierFile {
                        id: file_id,
                        filename,
                        content_type,
                        size,
                        sha256,
                        share_permission: share_permission != 0,
                        reviewed_by_owner: reviewed_by_owner != 0,
                        created_at,
                    }
                },
            )
            .collect();
        let opportunities = sqlx::query_as::<_, (String, String, String)>(
            "SELECT o.id,o.title,op.role FROM opportunity o \
             JOIN opportunity_party op ON op.opportunity_id=o.id \
             WHERE op.party_id=? AND o.state='open' \
             ORDER BY o.updated_at DESC LIMIT 60",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let opportunities = opportunities
            .into_iter()
            .map(|(opportunity_id, title, role)| DossierOpportunity {
                id: opportunity_id,
                title,
                role,
            })
            .collect();
        let paths=sqlx::query_as::<_,(String,String,String,String,String,String)>(
            "WITH paths AS (SELECT r.id,r.title,r.kind,'edge' AS via,eg.id AS hop,eg.verb AS label FROM edge eg JOIN resource r ON r.id=CASE WHEN eg.from_id=? THEN eg.to_id ELSE eg.from_id END WHERE (eg.from_id=? OR eg.to_id=?) AND eg.state='active' AND r.readiness<>'retired'
            UNION SELECT r.id,r.title,r.kind,'opportunity',o.id,o.title FROM opportunity_party op JOIN opportunity o ON o.id=op.opportunity_id JOIN material m ON m.opportunity_id=o.id JOIN resource_use ru ON ru.material_id=m.id AND ru.version_no=m.head_version JOIN resource r ON r.id=ru.resource_id WHERE op.party_id=? AND o.state='open' AND r.readiness<>'retired')
            SELECT * FROM paths WHERE id IN (SELECT id FROM paths GROUP BY id ORDER BY title COLLATE NOCASE,id LIMIT 60) ORDER BY title COLLATE NOCASE,id,via,hop")
            .bind(id).bind(id).bind(id).bind(id).fetch_all(&self.pool).await?;
        let mut associated_resources: Vec<DossierResource> = Vec::new();
        for (resource_id, title, kind, via, hop, label) in paths {
            if associated_resources
                .last()
                .is_none_or(|r| r.id != resource_id)
            {
                associated_resources.push(DossierResource {
                    id: resource_id,
                    title,
                    kind,
                    opportunity_id: None,
                    opportunity_title: None,
                    paths: vec![],
                });
            }
            let resource = associated_resources
                .last_mut()
                .expect("resource group was initialized");
            let path = if via == "edge" {
                json!({"via":"edge","edge_id":hop,"verb":label,"participant_id":id})
            } else {
                resource.opportunity_id.get_or_insert_with(|| hop.clone());
                resource
                    .opportunity_title
                    .get_or_insert_with(|| label.clone());
                json!({"via":"opportunity","opportunity_id":hop,"opportunity_title":label,"participant_id":id})
            };
            resource.paths.push(path);
        }
        let import_origin = sqlx::query_as::<_, (String, i64)>(
            "SELECT batch_id,source_record FROM party_import_origin WHERE party_id=?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .map(|(batch, record)| json!({"batch_id":batch,"source_record":record}));
        let retired_at = sqlx::query_scalar("SELECT retired_at FROM entity WHERE id=?")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        Ok(Some(EntityDossier {
            retired_at,
            entity,
            import_origin,
            direct_links,
            files,
            opportunities,
            associated_resources,
        }))
    }

    async fn graph_snapshot(
        &self,
        center_id: Option<&str>,
        view_scope: &str,
        view_id: &str,
    ) -> Result<GraphSnapshot, WorkspaceError> {
        let total_nodes = if let Some(center) = center_id {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM graph_card e WHERE e.id=? OR e.id IN (SELECT from_id FROM edge WHERE state='active' AND to_id=?) OR e.id IN (SELECT to_id FROM edge WHERE state='active' AND from_id=?)")
                .bind(center).bind(center).bind(center).fetch_one(&self.pool).await?
        } else {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM graph_card")
                .fetch_one(&self.pool)
                .await?
        };
        let mut rows = if let Some(center) = center_id {
            sqlx::query_as::<_, (String, String, String, i64, f64, f64, i64)>(
                "SELECT e.id,e.kind,e.name,e.revision,COALESCE(v.x,((ROW_NUMBER() OVER(ORDER BY e.name COLLATE NOCASE,e.id)-1)%8)*240.0),COALESCE(v.y,((ROW_NUMBER() OVER(ORDER BY e.name COLLATE NOCASE,e.id)-1)/8)*160.0),COALESCE(v.revision,0) FROM graph_card e LEFT JOIN view_position v ON v.entity_id=e.id AND v.view_scope=? AND v.view_id=? WHERE e.id=? OR e.id IN (SELECT from_id FROM edge WHERE state='active' AND to_id=?) OR e.id IN (SELECT to_id FROM edge WHERE state='active' AND from_id=?) ORDER BY (e.id=?) DESC,e.name COLLATE NOCASE,e.id LIMIT 201",
            )
            .bind(view_scope).bind(view_id).bind(center).bind(center).bind(center).bind(center).fetch_all(&self.pool).await?
        } else {
            sqlx::query_as::<_, (String, String, String, i64, f64, f64, i64)>(
                "SELECT e.id,e.kind,e.name,e.revision,COALESCE(v.x,((ROW_NUMBER() OVER(ORDER BY e.name COLLATE NOCASE,e.id)-1)%8)*240.0),COALESCE(v.y,((ROW_NUMBER() OVER(ORDER BY e.name COLLATE NOCASE,e.id)-1)/8)*160.0),COALESCE(v.revision,0) FROM graph_card e LEFT JOIN view_position v ON v.entity_id=e.id AND v.view_scope=? AND v.view_id=? ORDER BY e.name COLLATE NOCASE,e.id LIMIT 201",
            )
            .bind(view_scope).bind(view_id).fetch_all(&self.pool).await?
        };
        let mut omitted_nodes = (total_nodes - 200).max(0);
        rows.truncate(200);
        let visible: std::collections::HashSet<String> =
            rows.iter().map(|row| row.0.clone()).collect();
        let mut nodes = rows
            .into_iter()
            .map(
                |(id, kind, name, revision, x, y, position_revision)| GraphNode {
                    id,
                    kind,
                    name,
                    revision,
                    x,
                    y,
                    position_revision,
                },
            )
            .collect::<Vec<_>>();
        let total_edges = if visible.is_empty() {
            0
        } else {
            let mut query = sqlx::QueryBuilder::new(
                "SELECT COUNT(*) FROM edge WHERE state='active' AND from_id IN (",
            );
            {
                let mut separated = query.separated(",");
                for id in &visible {
                    separated.push_bind(id);
                }
            }
            query.push(") AND to_id IN (");
            {
                let mut separated = query.separated(",");
                for id in &visible {
                    separated.push_bind(id);
                }
            }
            query.push(")");
            query
                .build_query_scalar::<i64>()
                .fetch_one(&self.pool)
                .await?
        };
        let mut edges: Vec<GraphEdge> = if visible.is_empty() {
            Vec::new()
        } else {
            let mut query = sqlx::QueryBuilder::new(
                "SELECT e.id,e.from_id,e.to_id,e.verb,e.revision,cr.occurrence_id,COALESCE(cr.wording,e.basis) quote, (cr.claim_id IS NULL) owner_authored, COALESCE(cr.tier,1) tier, COALESCE(cr.speaker_kind,e.author_kind) speaker_kind,COALESCE(so.captured_at,e.created_at) captured_at,sv.title source_title,sv.url source_url,cr.claim_id,cr.revision claim_revision,cr.speaker_entity_id FROM edge e LEFT JOIN edge_evidence ee ON ee.rowid=(SELECT newest.rowid FROM edge_evidence newest WHERE newest.edge_id=e.id ORDER BY newest.rowid DESC LIMIT 1) LEFT JOIN claim_revision cr ON cr.claim_id=ee.claim_id AND cr.revision=ee.claim_revision LEFT JOIN source_occurrence so ON so.id=cr.occurrence_id LEFT JOIN source_variant sv ON sv.id=cr.source_variant_id WHERE e.state='active' AND e.from_id IN (",
            );
            {
                let mut separated = query.separated(",");
                for id in &visible {
                    separated.push_bind(id);
                }
            }
            query.push(") AND e.to_id IN (");
            {
                let mut separated = query.separated(",");
                for id in &visible {
                    separated.push_bind(id);
                }
            }
            query.push(") ORDER BY e.from_id,e.to_id,e.id LIMIT 401");
            query.build_query_as().fetch_all(&self.pool).await?
        };
        let mut omitted_edges = (total_edges - 400).max(0);
        edges.truncate(400);
        if view_scope == "resources"
            && let Some(center) = center_id
        {
            self.add_resource_graph_records(
                center,
                &mut nodes,
                &mut edges,
                &mut omitted_nodes,
                &mut omitted_edges,
            )
            .await?;
        }
        Ok(GraphSnapshot {
            nodes,
            edges,
            omitted_nodes,
            omitted_edges,
        })
    }

    async fn save_graph_layout(
        &self,
        idempotency_key: &str,
        input: GraphLayoutInput,
    ) -> Result<GraphLayoutResult, WorkspaceError> {
        if idempotency_key.is_empty()
            || idempotency_key.len() > 100
            || input.view_scope.is_empty()
            || input.view_scope.len() > 80
            || input.view_id.is_empty()
            || input.view_id.len() > 160
            || input.positions.is_empty()
            || input.positions.len() > 200
        {
            return Err(WorkspaceError::Invalid(
                "graph layout command is outside its bounds".into(),
            ));
        }
        let mut ids = std::collections::HashSet::new();
        if input.positions.iter().any(|position| {
            !position.x.is_finite()
                || !position.y.is_finite()
                || position.x.abs() > 1_000_000.0
                || position.y.abs() > 1_000_000.0
                || position.expected_revision < 0
                || !ids.insert(position.entity_id.as_str())
        }) {
            return Err(WorkspaceError::Invalid(
                "graph coordinates or entity set is invalid".into(),
            ));
        }
        let canonical = digest(
            &serde_json::to_vec(&input)
                .map_err(|error| WorkspaceError::Invalid(error.to_string()))?,
        );
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        ).bind(&self.epoch).bind(idempotency_key).fetch_optional(&mut *tx).await? {
            if old_digest != canonical { return Err(WorkspaceError::IdempotencyConflict); }
            let result = serde_json::from_str(&result).map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
            tx.commit().await?;
            return Ok(result);
        }
        let now = now_utc();
        let mut saved = Vec::with_capacity(input.positions.len());
        let mut last_cursor = 0_i64;
        for position in input.positions {
            let exists: Option<String> = sqlx::query_scalar("SELECT id FROM entity WHERE id=?")
                .bind(&position.entity_id)
                .fetch_optional(&mut *tx)
                .await?;
            if exists.is_none() {
                return Err(WorkspaceError::NotFound);
            }
            let current: Option<i64> = sqlx::query_scalar("SELECT revision FROM view_position WHERE view_scope=? AND view_id=? AND entity_id=?")
                .bind(&input.view_scope).bind(&input.view_id).bind(&position.entity_id).fetch_optional(&mut *tx).await?;
            let current_revision = current.unwrap_or(0);
            if current_revision != position.expected_revision {
                return Err(WorkspaceError::StaleRevision);
            }
            let revision = current_revision + 1;
            sqlx::query("INSERT INTO view_position(view_scope,view_id,entity_id,x,y,pinned,revision) VALUES(?,?,?,?,?,1,?) ON CONFLICT(view_scope,view_id,entity_id) DO UPDATE SET x=excluded.x,y=excluded.y,pinned=1,revision=excluded.revision")
                .bind(&input.view_scope).bind(&input.view_id).bind(&position.entity_id).bind(position.x).bind(position.y).bind(revision).execute(&mut *tx).await?;
            last_cursor = sqlx::query_scalar("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'entity',?,?, 'graph.positioned','{}') RETURNING cursor")
                .bind(new_id(&position.entity_id)).bind(&now).bind(&position.entity_id).bind(revision).fetch_one(&mut *tx).await?;
            saved.push(GraphPositionResult {
                entity_id: position.entity_id,
                x: position.x,
                y: position.y,
                revision,
            });
        }
        let result = GraphLayoutResult { positions: saved };
        let result_json = serde_json::to_string(&result)
            .map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,200,?,?,?)")
            .bind(&self.epoch).bind(idempotency_key).bind(canonical).bind(result_json).bind(last_cursor).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
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

    /// Replay lookup for a capture command. Returns the stored success result
    /// when the exact key+canonical pair was already committed (RFC-0004
    /// INT-03), `None` for a fresh command, and a conflict when the key was
    /// used for a different command.
    pub async fn save_capture_replay(
        &self,
        key: &str,
        canonical: &str,
    ) -> Result<Option<Value>, WorkspaceError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        )
        .bind(&self.epoch)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((stored, result)) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        if stored != canonical {
            return Err(WorkspaceError::IdempotencyConflict);
        }
        let value: Value =
            serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        tx.commit().await?;
        Ok(Some(value))
    }

    /// Commit a capture together with its extractor job, event rows and
    /// idempotency receipt in one writer transaction (B-FR-1, EVT-1, BQ1):
    /// a lost-ack retry must produce exactly one occurrence and one job, and
    /// a crash between "capture" and "enqueue" is impossible because the job
    /// row commits atomically with the source. Returns the job id (when the
    /// queue admitted it) and the final response payload, which is also what
    /// the receipt stores.
    pub async fn save_capture_commit(
        &self,
        key: &str,
        canonical: &str,
        text: &str,
        title: Option<&str>,
        url: Option<&str>,
    ) -> Result<(Option<String>, Value), WorkspaceError> {
        if key.trim().is_empty() || key.len() > 100 {
            return Err(WorkspaceError::Invalid(
                "idempotency key must contain 1–100 bytes".into(),
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
        sqlx::query("INSERT INTO source_occurrence(id,source_variant_id,actor_kind,actor_entity_id,captured_at) VALUES(?,?, 'unknown',NULL,?)")
            .bind(&occurrence_id).bind(&variant_id).bind(now_utc()).execute(&mut *tx).await?;
        let event_payload = json!({
            "content_id": content_id,
            "variant_id": variant_id,
            "occurrence_id": occurrence_id,
            "normalized_sha256": normalized_hash,
            "raw_sha256": raw_hash,
        });
        let captured_cursor: i64 = sqlx::query_scalar("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'source',?,1,'source.captured',?) RETURNING cursor")
            .bind(new_id(&occurrence_id)).bind(now_utc()).bind(&occurrence_id).bind(event_payload.to_string()).fetch_one(&mut *tx).await?;
        // The extraction job commits in the same transaction: a crash right
        // after this commit can never leave a capture without its job.
        let now = now_utc();
        let input = json!({"occurrence_id": occurrence_id, "source_variant_id": variant_id});
        let input_json =
            serde_json::to_string(&input).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let input_sha = digest(input_json.as_bytes());
        let in_flight: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM durable_job WHERE status IN ('queued','running')",
        )
        .fetch_one(&mut *tx)
        .await?;
        let (job_id, job_status, last_cursor) = if in_flight < 32 {
            let id = new_id(&input_sha);
            sqlx::query("INSERT INTO durable_job(id,role,status,input_json,input_sha256,source_occurrence_id,source_variant_id,scope_kind,scope_id,scope_revision,current_attempt,max_attempts,created_at,updated_at) VALUES(?,'extractor','queued',?,?,?,?, 'source',?,1,0,3,?,?)")
                .bind(&id).bind(&input_json).bind(&input_sha).bind(&occurrence_id).bind(&variant_id).bind(&occurrence_id).bind(&now).bind(&now).execute(&mut *tx).await?;
            let job_cursor: i64 = sqlx::query_scalar("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'job',?,1,'job.queued',?) RETURNING cursor")
                .bind(new_id(&id)).bind(&now).bind(&id).bind(json!({"role":"extractor","source_occurrence_id":occurrence_id}).to_string()).fetch_one(&mut *tx).await?;
            (Some(id), "queued", job_cursor)
        } else {
            (None, "not_queued", captured_cursor)
        };
        let full = json!({
            "content_id": content_id,
            "variant_id": variant_id,
            "occurrence_id": occurrence_id,
            "normalized_sha256": normalized_hash,
            "raw_sha256": raw_hash,
            "job_id": job_id,
            "job_status": job_status,
        });
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,201,?,?,?)")
            .bind(&self.epoch).bind(key).bind(canonical).bind(full.to_string()).bind(last_cursor).bind(now_utc()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok((job_id, full))
    }

    /// Create a distinct graph identity. Names are deliberately not keys: two
    /// people with the same display name remain separate records. The entity,
    /// its creation event and the receipt commit under one writer reservation.
    pub async fn create_entity(
        &self,
        idempotency_key: &str,
        kind: &str,
        name: &str,
        focus: &str,
    ) -> Result<EntityRecord, WorkspaceError> {
        self.create_card(idempotency_key, kind, name, focus, "", &[])
            .await
    }

    async fn create_card(
        &self,
        idempotency_key: &str,
        kind: &str,
        name: &str,
        focus: &str,
        notes: &str,
        uploads: &[String],
    ) -> Result<EntityRecord, WorkspaceError> {
        if uploads.len() > 100 || notes.len() > 512 * 1024 {
            return Err(WorkspaceError::Invalid(
                "Card notes or file count exceeds the permitted limit.".into(),
            ));
        }
        let notes = canonical_markdown(notes);
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 200 {
            return Err(WorkspaceError::Invalid(
                "idempotency key must contain 1–200 bytes".into(),
            ));
        }
        if !matches!(kind, "person" | "org" | "project" | "resource") {
            return Err(WorkspaceError::Invalid("unsupported entity kind".into()));
        }
        if name.trim().is_empty() || name.len() > 500 {
            return Err(WorkspaceError::Invalid(
                "entity name must contain 1–500 bytes".into(),
            ));
        }
        if focus.chars().count() > 250 {
            return Err(WorkspaceError::Invalid(
                "focus exceeds 250 characters".into(),
            ));
        }
        let canonical = digest(
            json!(["create-card", kind, name, focus, notes, uploads])
                .to_string()
                .as_bytes(),
        );
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        )
        .bind(&self.epoch)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            if old_digest != canonical {
                return Err(WorkspaceError::IdempotencyConflict);
            }
            let record =
                serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            tx.commit().await?;
            return Ok(record);
        }
        let id = new_id(name);
        let now = now_utc();
        sqlx::query(
            "INSERT INTO entity(id,kind,name,focus,notes_md,revision,created_at,updated_at) VALUES(?,?,?,?,?,1,?,?)",
        )
        .bind(&id)
        .bind(kind)
        .bind(name)
        .bind(focus)
        .bind(notes)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'entity',?,1,'entity.created','{}')")
            .bind(new_id(&id)).bind(&now).bind(&id).execute(&mut *tx).await?;
        for upload in uploads {
            parties::bind_upload(&mut tx, &id, upload).await?;
        }
        let record = EntityRecord {
            id: id.clone(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            focus: focus.to_owned(),
            revision: 1,
        };
        let result_json =
            serde_json::to_string(&record).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,201,?,(SELECT COALESCE(MAX(cursor),0) FROM event),?)")
            .bind(&self.epoch).bind(idempotency_key).bind(canonical).bind(&result_json).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(record)
    }

    /// Last committed workspace event sequence, used as the read cursor.
    pub async fn current_cursor(&self) -> Result<i64, WorkspaceError> {
        let cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
            .fetch_one(&self.pool)
            .await?;
        Ok(cursor)
    }
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(crate) fn new_id(seed: &str) -> String {
    let nonce = IDS.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    digest(format!("{seed}:{nanos}:{nonce}").as_bytes())[..32].to_owned()
}

/// UTC RFC 3339 timestamp offset from trusted wall time, used for leases and
/// run deadlines.
fn now_utc_offset(seconds: i64) -> String {
    let value = chrono::Utc::now() + chrono::Duration::seconds(seconds);
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn now_utc() -> String {
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

fn resource_etag(epoch: &str, id: &str, revision: i64) -> String {
    format!("\"p1.{epoch}.resource.{id}.{revision}\"")
}

fn entity_etag(epoch: &str, id: &str, revision: i64) -> String {
    format!("\"p1.{epoch}.entity.{id}.{revision}\"")
}

fn notes_etag(epoch: &str, id: &str, revision: i64) -> String {
    format!("\"p1.{epoch}.notes.{id}.{revision}\"")
}

/// A strong quoted ETag: `"..."`, not weak, not wildcard. Used for the write
/// scopes that require `If-Match` (entities, notes, resources).
fn is_strong_etag(value: &str) -> bool {
    value.len() >= 2
        && value.starts_with('"')
        && value.ends_with('"')
        && !value.starts_with("W/")
        && value != "*"
}

/// Canonical Markdown: UTF-8 with LF line endings. CRLF input is normalized;
/// trailing bytes are preserved so literal punctuation round-trips.
fn canonical_markdown(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn random_hex(size: usize) -> String {
    let bytes: Vec<u8> = (0..size).map(|_| random::<u8>()).collect();
    hex::encode(bytes)
}

/// Load (or create) the private generation key used for command ledger HMACs
/// and cursors. Kept in the control directory, never in the database.
fn load_command_secret(root: &generation::WorkspaceRoot) -> Result<Vec<u8>, WorkspaceError> {
    let path = root.control_path().join("secret.key");
    if path.exists() {
        let bytes = std::fs::read(&path)
            .map_err(|e| WorkspaceError::Invalid(format!("read generation key: {e}")))?;
        if bytes.len() != 32 {
            return Err(WorkspaceError::Invalid(
                "generation key file has the wrong size".into(),
            ));
        }
        return Ok(bytes);
    }
    let bytes: Vec<u8> = (0..32).map(|_| random::<u8>()).collect();
    std::fs::write(&path, &bytes)
        .map_err(|e| WorkspaceError::Invalid(format!("write generation key: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(bytes)
}

/// Rotate the generation key on workspace replacement. Old-epoch receipts
/// and cursors are thereby invalidated.
fn rotate_command_secret(root: &generation::WorkspaceRoot) -> Result<Vec<u8>, WorkspaceError> {
    let path = root.control_path().join("secret.key");
    let bytes: Vec<u8> = (0..32).map(|_| random::<u8>()).collect();
    std::fs::write(&path, &bytes)
        .map_err(|e| WorkspaceError::Invalid(format!("rotate generation key: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Session {
    token: String,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) workspace: Arc<Workspace>,
    authority: String,
    bootstrap: Arc<Mutex<HashMap<String, Instant>>>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    pub(crate) jobs: jobs::Queue,
    pub(crate) restart: tokio::sync::watch::Sender<bool>,
    pub(crate) registry: Arc<commands::Registry>,
    pub(crate) writer: Arc<application::writer::Writer>,
}

/// Initialize (or open) a private SQLite workspace and bind only to IPv4
/// loopback. The caller owns workspace locking and process lifetime.
pub async fn start_workspace(path: &Path) -> Result<RunningService, String> {
    let workspace = Workspace::open(path)
        .await
        .map_err(|error| format!("open workspace: {error}"))?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("bind loopback: {error}"))?;
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    let bootstrap_secret = random_hex(32);
    let workspace = Arc::new(workspace);
    // Reconcile due threads before first read (RFC-0002 BE-08): a deadline
    // that expired while the service was down must be resolved exactly once,
    // and no deadline-sensitive read should return an expired thread as
    // actionable.
    let _ = workspace.reconcile_deadlines(&now_utc()).await;
    let (restart_sender, restart_receiver) = tokio::sync::watch::channel(false);
    let restart_wait = restart_receiver.clone();
    let (jobs, job_task) = jobs::start(workspace.clone(), 32);
    // One owner session polls the same writer; the delegated runtime starts
    // after AppState construction below so it can dispatch through the
    // registry with a server-derived actor.
    // Registry boot validation completes before the listener starts; zero
    // descriptors are published unless every registration passes (FR-4).
    let registry = Arc::new(
        commands::Registry::build()
            .map_err(|error| format!("command registry boot validation failed: {error}"))?,
    );
    // Dedicated adopted writer connection (UCR-001 §5).
    let writer_conn = {
        let options = SqliteConnectOptions::from_str(&format!(
            "sqlite://{}",
            workspace.database_path().display()
        ))
        .map_err(|e| format!("adopted writer options: {e}"))?
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .busy_timeout(Duration::from_secs(2));
        sqlx::SqliteConnection::connect_with(&options)
            .await
            .map_err(|e| format!("adopted writer connection: {e}"))?
    };
    let (writer, writer_task) =
        application::writer::start(writer_conn, workspace.generation.clone());
    let writer = Arc::new(writer);
    let reconcile_task = {
        let workspace = workspace.clone();
        let mut stop = restart_receiver.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            loop {
                tokio::select! {_ = interval.tick()=>{let now=now_utc();let _=workspace.reconcile_deadlines(&now).await;},r=stop.changed()=>{if r.is_ok(){break;}}}
            }
        })
    };
    let backup_task = maintenance::start_scheduler(workspace.clone(), restart_receiver.clone());
    let state = AppState {
        workspace,
        authority: address.to_string(),
        bootstrap: Arc::new(Mutex::new(HashMap::from([(
            bootstrap_secret.clone(),
            Instant::now() + Duration::from_secs(60),
        )]))),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        jobs,
        restart: restart_sender.clone(),
        registry,
        writer,
    };
    let agent_task = agent_runtime::start(Arc::new(state.clone()));
    let bootstrap_shared = state.bootstrap.clone();
    let mut shell = Router::new().route("/", get(index));
    for tab in [
        "resources",
        "parties",
        "leads",
        "queue",
        "output",
        "settings",
    ] {
        let root = format!("/{tab}");
        shell = shell
            .route(&root, get(index))
            .route(&format!("{root}/"), get(index))
            .route(&format!("{root}/{{record}}"), get(index))
            .route(&format!("{root}/{{record}}/"), get(index));
    }
    let app = shell
        .route("/assets/{*path}", get(asset))
        .route("/api/v1/health", get(health))
        .route("/api/v1/session/bootstrap", post(bootstrap))
        .route("/api/v1/session", get(session))
        .route("/api/v1/entities", get(list_entities).post(create_entity))
        .route(
            "/api/v1/entities/{id}",
            get(get_entity).patch(update_entity),
        )
        .route(
            "/api/v1/entities/{id}/notes",
            get(get_entity_notes).put(update_entity_notes),
        )
        .route("/api/v1/entities/{id}/dossier", get(get_entity_dossier))
        .route("/api/v1/events", get(get_events))
        .route("/api/v1/captures", get(list_captures).post(save_capture))
        .route("/api/v1/graph", get(get_graph))
        .route("/api/v1/graph/layout", put(update_graph_layout))
        .merge(relations::routes())
        .merge(board::routes())
        .merge(materials::routes())
        .merge(release::routes())
        .merge(blob::routes())
        .merge(parties::routes())
        .merge(thread::routes())
        .merge(jobs::routes())
        .merge(assistant::routes())
        .merge(writing::routes())
        .merge(queueing::routes())
        .merge(queue_sheet::routes())
        .merge(resource_board::routes())
        .merge(maintenance::routes())
        .merge(commands::http::routes())
        .merge(agent_runtime::routes())
        .with_state(state)
        .layer(axum::extract::DefaultBodyLimit::max(2_100_000));
    let (shutdown, receiver) = tokio::sync::oneshot::channel();
    let mut restart_rx = restart_receiver;
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                tokio::select! {_ = receiver=>{},_ = restart_rx.changed()=>{}}
            })
            .await;
    });
    Ok(RunningService {
        address,
        bootstrap_secret,
        bootstrap: bootstrap_shared,
        shutdown,
        task,
        restart: restart_wait,
        restart_sender,
        background: vec![
            job_task,
            writer_task,
            reconcile_task,
            backup_task,
            agent_task,
        ],
    })
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
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

pub(crate) fn private_headers(response: &mut Response, epoch: &str) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
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

fn check_authority(headers: &HeaderMap, state: &AppState) -> Result<(), AuthError> {
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
    check_authority(headers, state)?;
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
        != Some(state.workspace.epoch())
    {
        return Err(auth_error(
            StatusCode::CONFLICT,
            "WORKSPACE_REPLACED",
            "Workspace changed; reconnect before continuing",
        ));
    }
    Ok(())
}

/// A resolved, acting principal. U01 has only the owner principal; agent
/// principals arrive with the agent runtime slices.
#[derive(Clone, Debug)]
pub(crate) struct OwnerPrincipal {
    /// Opaque pseudonymous principal identity (hash of the session id).
    pub pseudonym: String,
}

/// One common auth boundary for adopted operations: session cookie, token
/// and epoch checks, returning the resolved owner principal.
pub(crate) async fn authenticate_owner(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<OwnerPrincipal, AuthError> {
    authenticate(headers, state).await?;
    let session_id = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .split(';')
        .filter_map(|part| part.trim().strip_prefix("parley_session="))
        .next()
        .ok_or_else(|| {
            auth_error(
                StatusCode::UNAUTHORIZED,
                "SESSION_REQUIRED",
                "Sign in to the local workspace",
            )
        })?
        .to_owned();
    Ok(OwnerPrincipal {
        pseudonym: digest(format!("parley-session\0{session_id}").as_bytes()),
    })
}

/// Verify one arbitrary wire payload against the real command catalogue.
/// Feature/test entry point used by the fuzz target: duplicate keys, nesting,
/// canonical digests, mapped tool names and schema/serde agreement all run
/// through the production code paths.
pub fn validate_command_wire(input: &[u8]) -> Result<(), String> {
    static REGISTRY: OnceLock<commands::Registry> = OnceLock::new();
    let registry = REGISTRY
        .get_or_init(|| commands::Registry::build().expect("boot-validated registry must build"));
    let value = decode_unique(input).map_err(|_| "duplicate keys or invalid JSON".to_owned())?;
    if value_depth(&value) > 16 {
        return Err("nesting exceeds 16".into());
    }
    let canonical = parley_domain::jcs::canonicalize_value(&value)
        .map_err(|_| "value is not canonicalizable".to_owned())?;
    let _digest = digest(canonical.as_bytes());
    let Some(object) = value.as_object() else {
        return Err("command wire input must be an object".into());
    };
    if let Some(id) = object.get("id").and_then(Value::as_str)
        && let Some(descriptor) = registry.command(id)
    {
        let validator = commands::validator_for(descriptor.args_schema)
            .map_err(|_| "schema failed to build".to_owned())?;
        let args = object.get("args").cloned().unwrap_or(Value::Null);
        if validator.validate(&args).is_err() {
            return Err("wire args do not match the closed schema".into());
        }
    }
    let mut mapped = std::collections::BTreeSet::new();
    for id in registry.descriptor_ids() {
        if !mapped.insert(id.replace('.', "__")) {
            return Err("mapped tool name collision".into());
        }
    }
    Ok(())
}

fn value_depth(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(items) => 1 + items.iter().map(value_depth).max().unwrap_or(0),
        serde_json::Value::Object(map) => 1 + map.values().map(value_depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// Finalize interrupted agent turns: an active lease whose deadline has
/// passed is returned to the queue for same-ordinal reconciliation. Called at
/// startup and by maintenance after a workspace replacement.
pub async fn recover_agent_runs(workspace: &Workspace) {
    agent_runtime::recover(workspace).await;
}

/// Version of the explicit table policy that scrubs command state from
/// snapshots and omits it from portable exports.
pub fn command_table_policy_version() -> u32 {
    maintenance::COMMAND_TABLE_POLICY_VERSION
}

/// Verify that the active catalogue passes every boot constraint. The service
/// calls this before binding its listener; the release gate calls it to prove
/// a valid catalogue is publishable.
pub fn validate_command_catalogue() -> Result<(), String> {
    commands::Registry::build().map(|_| ())
}

/// Reject a deliberately invalid descriptor shape. This runs the identical
/// identity, schema and context-expression checks used by boot registration,
/// so a negative catalogue case cannot pass by bypassing validation.
pub fn validate_command_descriptor_shape(
    id: &str,
    args_schema: &serde_json::Value,
    result_schema: &serde_json::Value,
    when: Option<&str>,
) -> Result<(), String> {
    commands::descriptor::validate_owned_descriptor(id, args_schema, result_schema, when)
}

/// Bounded operation-key syntax for registry invocation (UCR-001 §4.1).
pub(crate) fn command_operation_key(
    headers: &HeaderMap,
) -> Result<String, (StatusCode, &'static str, &'static str)> {
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
    else {
        return Err((
            StatusCode::BAD_REQUEST,
            "malformed_request",
            "Idempotency-Key is required",
        ));
    };
    let valid = !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.');
    if !valid {
        return Err((
            StatusCode::BAD_REQUEST,
            "malformed_request",
            "Idempotency-Key is invalid",
        ));
    }
    Ok(key.to_owned())
}

async fn authenticate_session(headers: &HeaderMap, state: &AppState) -> Result<String, AuthError> {
    check_authority(headers, state)?;
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

/// Small description of a rejected request predicate: status, stable code and
/// message. Handlers turn it into the shared error envelope.
type Rejection = (StatusCode, &'static str, &'static str);

/// Small description of a rejected operation key.
type KeyRejection = Rejection;

fn idempotency_key(headers: &HeaderMap) -> Result<String, KeyRejection> {
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
    else {
        return Err((
            StatusCode::BAD_REQUEST,
            "IDEMPOTENCY_KEY_REQUIRED",
            "Idempotency-Key is required",
        ));
    };
    if key.is_empty() || key.len() > 100 {
        return Err((
            StatusCode::BAD_REQUEST,
            "IDEMPOTENCY_KEY_INVALID",
            "Idempotency-Key is invalid",
        ));
    }
    Ok(key.to_owned())
}

pub(crate) fn command_error(err: WorkspaceError) -> Response {
    match err {
        WorkspaceError::IdempotencyConflict => error(
            StatusCode::CONFLICT,
            "KEY_REUSED",
            "Idempotency key was used for a different command",
        ),
        WorkspaceError::NotFound => error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Resource does not exist",
        ),
        WorkspaceError::StaleRevision => error(
            StatusCode::PRECONDITION_FAILED,
            "STALE_REVISION",
            "Resource changed in another session",
        ),
        WorkspaceError::Invalid(message) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "RESOURCE_INVALID",
            message,
        ),
        WorkspaceError::LedgerFull => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "LEDGER_CAPACITY",
            "The command ledger is at capacity",
        ),
        WorkspaceError::Database(sqlx::Error::Database(db))
            if db
                .code()
                .as_deref()
                .and_then(|c| c.parse::<i32>().ok())
                .is_some_and(|code| matches!(code & 255, 5 | 6)) =>
        {
            let mut response = error(
                StatusCode::SERVICE_UNAVAILABLE,
                "WRITER_BUSY",
                "The workspace is busy. Retry the same operation.",
            );
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            response
        }
        WorkspaceError::Database(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "WRITE_FAILED",
            "The workspace could not complete the command",
        ),
    }
}

async fn health() -> Json<Value> {
    Json(json!({"status":"ready","model":"not-probed"}))
}

async fn index(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = check_authority(&headers, &state) {
        return response.into_response();
    }
    if web_assets::INDEX.is_empty() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "BUNDLE_UNAVAILABLE",
            "The owner interface bundle is not installed",
        );
    }
    let Ok(html) = std::str::from_utf8(web_assets::INDEX) else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "BUNDLE_INVALID",
            "The owner interface bundle is unreadable",
        );
    };
    let mut response = html.to_owned().into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

async fn asset(AxumPath(path): AxumPath<String>) -> Response {
    let request_path = format!("assets/{path}");
    match web_assets::ASSETS
        .iter()
        .find(|(name, _, _)| *name == request_path)
    {
        Some((_, mime, bytes)) => {
            let mut response = Body::from(*bytes).into_response();
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            );
            response
        }
        None => error(StatusCode::NOT_FOUND, "NOT_FOUND", "Asset does not exist"),
    }
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
    if let Err(response) = check_authority(&headers, &state) {
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
    let mut response = Json(json!({"ok": true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "parley_session={session_id}; HttpOnly; SameSite=Strict; Path=/api/v1; Max-Age=28800"
        ))
        .unwrap(),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

async fn session(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session_id = match authenticate_session(&headers, &state).await {
        Ok(session_id) => session_id,
        Err(auth) => return auth.into_response(),
    };
    let sessions = state.sessions.lock().await;
    let Some(record) = sessions.get(&session_id) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "SESSION_EXPIRED",
            "The local session has expired",
        );
    };
    let cursor = state.workspace.current_cursor().await.unwrap_or(0);
    let mut response = Json(json!({
        "workspace_id": state.workspace.workspace_id(),
        "epoch": state.workspace.epoch(),
        "wire_major": 1,
        "wire_minor": 0,
        "capabilities": ["resources.read", "resources.write", "party.sheet", "party.notes", "party.dossier", "party.imports", "party.attachments", "party.relations", "resource.board", "resource.attachments", "resource.relationships"],
        "mutation_token": record.token,
        "cursor": cursor.to_string(),
    }))
    .into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

/// Validate the entity-kind query filter. `resource` selects owner resources;
/// person, organization and project kinds select directory entities.
fn parse_kind_filter(kinds: &[String]) -> Result<(Vec<&str>, Vec<&str>), Rejection> {
    let mut resource_kinds = Vec::new();
    let mut graph_kinds = Vec::new();
    for kind in kinds {
        match kind.as_str() {
            "gift" | "proof" | "artifact" | "resource" => resource_kinds.push(kind.as_str()),
            "person" | "org" | "project" => graph_kinds.push(kind.as_str()),
            _ => {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "INVALID_KIND",
                    "Only workspace entity kinds can be listed in this slice",
                ));
            }
        }
    }
    Ok((resource_kinds, graph_kinds))
}

/// Repeated `kind` query parameters OR together (RFC-0004 §4.3); absent kinds
/// list both entity families.
fn parse_entity_query(raw: Option<String>) -> Vec<String> {
    raw.map(|query| {
        form_urlencoded::parse(query.as_bytes())
            .filter_map(|(key, value)| (key == "kind").then(|| value.into_owned()))
            .collect()
    })
    .unwrap_or_default()
}

fn resource_json(epoch: &str, record: &ResourceRecord) -> Value {
    json!({
        "id": record.id,
        "kind": record.kind,
        "title": record.title,
        "body": record.body,
        "readiness": record.readiness,
        "revision": record.revision,
        "etag": resource_etag(epoch, &record.id, record.revision),
    })
}

async fn list_entities(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let kinds = parse_entity_query(query);
    let (resource_kinds, graph_kinds) = match parse_kind_filter(&kinds) {
        Ok(filter) => filter,
        Err((status, code, message)) => return error(status, code, message),
    };
    let mut items = Vec::new();
    if !resource_kinds.is_empty() || kinds.is_empty() {
        match state
            .workspace
            .list_resources_filtered(&resource_kinds)
            .await
        {
            Ok(records) => items.extend(
                records
                    .iter()
                    .map(|record| resource_json(state.workspace.epoch(), record)),
            ),
            Err(_) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "READ_FAILED",
                    "Could not read resources",
                );
            }
        }
    }
    let already_listed: std::collections::HashSet<String> = items
        .iter()
        .filter_map(|v| v["id"].as_str().map(str::to_owned))
        .collect();
    if !graph_kinds.is_empty() || kinds.is_empty() {
        match state.workspace.list_entities_filtered(&graph_kinds).await {
            Ok(entities) => items.extend(entities.into_iter().filter(|entity|!already_listed.contains(&entity.id)).map(|entity| {
                json!({"id": entity.id, "kind": entity.kind, "name": entity.name, "focus": entity.focus, "revision": entity.revision, "etag": entity_etag(state.workspace.epoch(), &entity.id, entity.revision)})
            })),
            Err(_) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "READ_FAILED",
                    "Could not read directory entities",
                );
            }
        }
    }
    let cursor = state.workspace.current_cursor().await.unwrap_or(0);
    let mut response = Json(json!({"data": items, "cursor": cursor.to_string()})).into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

/// The entity create command covers both entity families (RFC-0004 §3): owner
/// resources carry a title and typed body; directory entities carry a name.
#[derive(Deserialize)]
struct ResourceCreateInput {
    kind: String,
    title: String,
    body: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphEntityCreateInput {
    #[serde(default)]
    notes: String,
    #[serde(default)]
    staged_upload_ids: Vec<String>,
    kind: String,
    name: String,
    focus: Option<String>,
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
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err((status, code, message)) => return error(status, code, message),
    };
    let value: Value = match decode_unique(&body) {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "INVALID_JSON",
                "Request body is invalid",
            );
        }
    };
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(kind, "gift" | "proof" | "artifact") {
        let Ok(input) = serde_json::from_value::<ResourceCreateInput>(value) else {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "RESOURCE_INVALID",
                "Resource type or name is invalid",
            );
        };
        if input.title.chars().count() > 240 || !input.body.is_object() {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "RESOURCE_INVALID",
                "Resource type or name is invalid",
            );
        }
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
            Ok(record) => {
                let etag = resource_etag(state.workspace.epoch(), &record.id, record.revision);
                let mut response = (
                    StatusCode::CREATED,
                    Json(json!({"data": resource_json(state.workspace.epoch(), &record)})),
                )
                    .into_response();
                if let Ok(tag) = HeaderValue::from_str(&etag) {
                    response.headers_mut().insert(header::ETAG, tag);
                }
                private_headers(&mut response, state.workspace.epoch());
                response
            }
            Err(err) => command_error(err),
        }
    } else if matches!(kind, "person" | "org" | "project" | "resource") {
        let Ok(input) = serde_json::from_value::<GraphEntityCreateInput>(value) else {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "ENTITY_INVALID",
                "Entity type or name is invalid",
            );
        };
        match state
            .workspace
            .create_card(
                &key,
                &input.kind,
                &input.name,
                input.focus.as_deref().unwrap_or(""),
                &input.notes,
                &input.staged_upload_ids,
            )
            .await
        {
            Ok(entity) => {
                let mut response = (
                    StatusCode::CREATED,
                    Json(json!({"data": {"id": entity.id, "kind": entity.kind, "name": entity.name, "focus": entity.focus, "revision": entity.revision, "etag": entity_etag(state.workspace.epoch(), &entity.id, entity.revision)}})),
                )
                    .into_response();
                private_headers(&mut response, state.workspace.epoch());
                response
            }
            Err(err) => command_error(err),
        }
    } else {
        error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "RESOURCE_INVALID",
            "Resource type or name is invalid",
        )
    }
}

async fn get_entity(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    match state.workspace.get_resource(&id).await {
        Ok(Some(record)) => {
            let etag = resource_etag(state.workspace.epoch(), &record.id, record.revision);
            let mut response =
                Json(json!({"data": resource_json(state.workspace.epoch(), &record)}))
                    .into_response();
            if let Ok(tag) = HeaderValue::from_str(&etag) {
                response.headers_mut().insert(header::ETAG, tag);
            }
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Ok(None) => match state.workspace.get_entity(&id).await {
            Ok(Some(entity)) => {
                let etag = entity_etag(state.workspace.epoch(), &entity.id, entity.revision);
                let mut response = Json(json!({"data": {
                    "id": entity.id,
                    "kind": entity.kind,
                    "name": entity.name,
                    "focus": entity.focus,
                    "revision": entity.revision,
                    "etag": etag,
                }}))
                .into_response();
                if let Ok(tag) = HeaderValue::from_str(&etag) {
                    response.headers_mut().insert(header::ETAG, tag);
                }
                private_headers(&mut response, state.workspace.epoch());
                response
            }
            Ok(None) => error(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "Resource does not exist",
            ),
            Err(_) => error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "READ_FAILED",
                "Could not read resource",
            ),
        },
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "READ_FAILED",
            "Could not read resource",
        ),
    }
}

#[derive(Deserialize)]
struct UpdateInput {
    title: String,
    body: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphEntityUpdateInput {
    name: Option<String>,
    focus: Option<String>,
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
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err((status, code, message)) => return error(status, code, message),
    };
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
    if !is_strong_etag(if_match) {
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
    let value: Value = match decode_unique(&body) {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "INVALID_JSON",
                "Request body is invalid",
            );
        }
    };
    // Owner-resource edit (existing route semantics).
    if let Ok(input) = serde_json::from_value::<UpdateInput>(value.clone()) {
        if input.title.chars().count() > 240 || !input.body.is_object() {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "RESOURCE_INVALID",
                "Resource name or fields are invalid",
            );
        }
        match state
            .workspace
            .update_resource(&key, &id, if_match, &input.title, &input.body)
            .await
        {
            Ok(record) => {
                let etag = resource_etag(state.workspace.epoch(), &record.id, record.revision);
                let mut response =
                    Json(json!({"data": resource_json(state.workspace.epoch(), &record)}))
                        .into_response();
                if let Ok(tag) = HeaderValue::from_str(&etag) {
                    response.headers_mut().insert(header::ETAG, tag);
                }
                private_headers(&mut response, state.workspace.epoch());
                response
            }
            Err(WorkspaceError::NotFound) => {
                entity_update_response(
                    &state,
                    &key,
                    &id,
                    if_match,
                    serde_json::from_value::<GraphEntityUpdateInput>(value),
                )
                .await
            }
            Err(err) => command_error(err),
        }
    } else {
        entity_update_response(
            &state,
            &key,
            &id,
            if_match,
            serde_json::from_value::<GraphEntityUpdateInput>(value),
        )
        .await
    }
}

async fn entity_update_response(
    state: &AppState,
    key: &str,
    id: &str,
    if_match: &str,
    input: Result<GraphEntityUpdateInput, serde_json::Error>,
) -> Response {
    let Ok(input) = input else {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "ENTITY_INVALID",
            "Entity fields are invalid",
        );
    };
    match state
        .workspace
        .update_entity(key, id, if_match, input.name, input.focus)
        .await
    {
        Ok(entity) => {
            let etag = entity_etag(state.workspace.epoch(), &entity.id, entity.revision);
            let mut response = Json(json!({"data": {
                "id": entity.id,
                "kind": entity.kind,
                "name": entity.name,
                "focus": entity.focus,
                "revision": entity.revision,
                "etag": etag,
            }}))
            .into_response();
            if let Ok(tag) = HeaderValue::from_str(&etag) {
                response.headers_mut().insert(header::ETAG, tag);
            }
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(WorkspaceError::NotFound) => error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Resource does not exist",
        ),
        Err(err) => command_error(err),
    }
}

#[derive(Deserialize)]
struct EntityNotesUpdateInput {
    markdown: String,
    expected_notes_rev: i64,
}

async fn get_entity_notes(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    match state.workspace.get_entity_notes(&id).await {
        Ok(Some(notes)) => {
            let etag = notes_etag(state.workspace.epoch(), &id, notes.revision);
            let mut response = Json(json!({"data": {
                "entity_id": notes.entity_id,
                "markdown": notes.markdown,
                "revision": notes.revision,
                "etag": etag,
            }}))
            .into_response();
            if let Ok(tag) = HeaderValue::from_str(&etag) {
                response.headers_mut().insert(header::ETAG, tag);
            }
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Resource does not exist",
        ),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "READ_FAILED",
            "Could not read entity notes",
        ),
    }
}

async fn update_entity_notes(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err((status, code, message)) => return error(status, code, message),
    };
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
    if !is_strong_etag(if_match) {
        return error(
            StatusCode::BAD_REQUEST,
            "ETAG_INVALID",
            "If-Match must be a strong notes ETag",
        );
    }
    if body.len() > 512 * 1024 + 64 {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "BODY_TOO_LARGE",
            "Notes exceed the size limit",
        );
    }
    let input: EntityNotesUpdateInput = match decode_unique(&body) {
        Ok(input) => input,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "INVALID_JSON",
                "Request body is invalid",
            );
        }
    };
    match state
        .workspace
        .update_entity_notes(
            &key,
            &id,
            if_match,
            input.expected_notes_rev,
            &input.markdown,
        )
        .await
    {
        Ok(notes) => {
            let etag = notes_etag(state.workspace.epoch(), &id, notes.revision);
            let mut response = Json(json!({"data": {
                "entity_id": notes.entity_id,
                "markdown": notes.markdown,
                "revision": notes.revision,
                "etag": etag,
            }}))
            .into_response();
            if let Ok(tag) = HeaderValue::from_str(&etag) {
                response.headers_mut().insert(header::ETAG, tag);
            }
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(WorkspaceError::NotFound) => error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Resource does not exist",
        ),
        Err(err) => command_error(err),
    }
}

async fn get_entity_dossier(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    match state.workspace.entity_dossier(&id).await {
        Ok(Some(dossier)) => {
            let etag = entity_etag(
                state.workspace.epoch(),
                &dossier.entity.id,
                dossier.entity.revision,
            );
            let mut value = json!(dossier);
            value["entity"]["etag"] = Value::String(etag);
            let mut response = Json(json!({"data": value})).into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Resource does not exist",
        ),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "READ_FAILED",
            "Could not read the entity dossier",
        ),
    }
}

#[derive(Deserialize)]
struct EventsQuery {
    after: Option<String>,
    /// Optional focused reads (Queue inspector history): restrict the feed to
    /// one entity without paging the whole workspace.
    entity_type: Option<String>,
    entity_id: Option<String>,
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
    let observed = state.workspace.current_cursor().await.unwrap_or(0);
    if after > observed {
        return error(
            StatusCode::CONFLICT,
            "CURSOR_AHEAD",
            "Event cursor is ahead of the workspace",
        );
    }
    type EventRow = (i64, String, String, String, i64, String, String, String);
    let rows = match (query.entity_type.as_deref(), query.entity_id.as_deref()) {
        (None, None) => {
            sqlx::query_as::<_, EventRow>(
                "SELECT cursor,event_id,entity_type,entity_id,revision,event_type,payload_json,occurred_at FROM event WHERE cursor>? ORDER BY cursor LIMIT 200",
            )
            .bind(after)
            .fetch_all(state.workspace.pool())
            .await
        }
        (Some(entity_type), None) => {
            sqlx::query_as::<_, EventRow>(
                "SELECT cursor,event_id,entity_type,entity_id,revision,event_type,payload_json,occurred_at FROM event WHERE cursor>? AND entity_type=? ORDER BY cursor LIMIT 200",
            )
            .bind(after)
            .bind(entity_type)
            .fetch_all(state.workspace.pool())
            .await
        }
        (None, Some(entity_id)) => {
            sqlx::query_as::<_, EventRow>(
                "SELECT cursor,event_id,entity_type,entity_id,revision,event_type,payload_json,occurred_at FROM event WHERE cursor>? AND entity_id=? ORDER BY cursor LIMIT 200",
            )
            .bind(after)
            .bind(entity_id)
            .fetch_all(state.workspace.pool())
            .await
        }
        (Some(entity_type), Some(entity_id)) => {
            sqlx::query_as::<_, EventRow>(
                "SELECT cursor,event_id,entity_type,entity_id,revision,event_type,payload_json,occurred_at FROM event WHERE cursor>? AND entity_type=? AND entity_id=? ORDER BY cursor LIMIT 200",
            )
            .bind(after)
            .bind(entity_type)
            .bind(entity_id)
            .fetch_all(state.workspace.pool())
            .await
        }
    };
    let rows = match rows {
        Ok(rows) => rows,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "READ_FAILED",
                "Could not read workspace events",
            );
        }
    };
    let items: Vec<Value> = rows
        .into_iter()
        .map(
            |(
                cursor,
                event_id,
                entity_type,
                entity_id,
                revision,
                event_type,
                payload,
                occurred_at,
            )| {
                json!({
                    "cursor": cursor.to_string(),
                    "event_id": event_id,
                    "entity_type": entity_type,
                    "entity_id": entity_id,
                    "revision": revision,
                    "event_type": event_type,
                    "occurred_at": occurred_at,
                    "payload": serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null),
                })
            },
        )
        .collect();
    let next_cursor = items
        .last()
        .and_then(|item| item["cursor"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| after.to_string());
    let mut response = Json(json!({
        "items": items,
        "next_cursor": next_cursor,
        "observed_cursor": observed.to_string(),
    }))
    .into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

#[derive(Deserialize)]
struct CaptureRequest {
    text: String,
    title: Option<String>,
    url: Option<String>,
}

async fn save_capture(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CaptureRequest>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err(error) => {
            return (
                error.0,
                Json(json!({"error":{"code":error.1,"message":error.2,"details":[]}})),
            )
                .into_response();
        }
    };
    let canonical = digest(
        format!(
            "POST\n/api/v1/captures\n{}",
            serde_json::to_string(&json!({"text":input.text,"title":input.title,"url":input.url}))
                .unwrap_or_default()
        )
        .as_bytes(),
    );
    match state.workspace.save_capture_replay(&key, &canonical).await {
        Ok(Some(replay)) => {
            let mut response = (StatusCode::CREATED, Json(replay)).into_response();
            private_headers(&mut response, state.workspace.epoch());
            return response;
        }
        Ok(None) => {}
        Err(err) => return command_error(err),
    }
    match state
        .workspace
        .save_capture_commit(
            &key,
            &canonical,
            &input.text,
            input.title.as_deref(),
            input.url.as_deref(),
        )
        .await
    {
        Ok((job_id, full)) => {
            if let Some(job_id) = &job_id {
                jobs::wake(&state.jobs, job_id);
            }
            let mut response = (StatusCode::CREATED, Json(full)).into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(err) => {
            // A concurrent same-key commit may have won this race. Replay
            // its committed result instead of surfacing a lost PK write
            // (BQ1: one occurrence regardless of interleaving).
            match state.workspace.save_capture_replay(&key, &canonical).await {
                Ok(Some(replay)) => {
                    let mut response = (StatusCode::CREATED, Json(replay)).into_response();
                    private_headers(&mut response, state.workspace.epoch());
                    response
                }
                _ => command_error(err),
            }
        }
    }
}

async fn list_captures(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    match state.workspace.list_sources().await {
        Ok(rows) => {
            let mut response = (StatusCode::OK, Json(rows)).into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "READ_FAILED",
            "Could not read saved sources",
        ),
    }
}

async fn get_graph(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let values: HashMap<String, String> = query
        .as_deref()
        .map(|raw| {
            form_urlencoded::parse(raw.as_bytes())
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let center_id = values
        .get("center_id")
        .map(String::as_str)
        .filter(|id| !id.is_empty());
    let view_scope = values
        .get("view_scope")
        .map(String::as_str)
        .unwrap_or("leads");
    let view_id = values.get("view_id").map(String::as_str).unwrap_or("all");
    if view_scope.len() > 80 || view_id.len() > 160 {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "GRAPH_SCOPE_INVALID",
            "Graph view scope is too long",
        );
    }
    match state
        .workspace
        .graph_snapshot(center_id, view_scope, view_id)
        .await
    {
        Ok(snapshot) => {
            let mut response = Json(snapshot).into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "READ_FAILED",
            "Could not read the graph neighborhood",
        ),
    }
}

async fn update_graph_layout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<GraphLayoutInput>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err((status, code, message)) => return error(status, code, message),
    };
    match state.workspace.save_graph_layout(&key, input).await {
        Ok(result) => {
            let mut response = Json(result).into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(error_value) => command_error(error_value),
    }
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
        let first = workspace
            .create_entity("dana-first", "person", "Dana Lee", "")
            .await
            .unwrap();
        let second = workspace
            .create_entity("dana-second", "person", "Dana Lee", "")
            .await
            .unwrap();
        assert_ne!(first.id, second.id);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM entity WHERE name='Dana Lee'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
        assert_eq!(count, 2);
        workspace.pool().close().await;
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn stale_update_fails_without_consuming_the_operation_key() {
        let path = temp_db();
        let workspace = Workspace::open(&path).await.unwrap();
        let created = workspace
            .create_resource(CreateResource {
                idempotency_key: "stale-create".into(),
                kind: "gift".into(),
                title: "Checklist".into(),
                body: serde_json::json!({"description":"first"}),
            })
            .await
            .unwrap();
        let stale_etag = resource_etag(workspace.epoch(), &created.id, 1);
        // Another session moves the record to revision 2 first.
        workspace
            .update_resource(
                "other-session",
                &created.id,
                &stale_etag,
                "Checklist v1.5",
                &serde_json::json!({"description":"competing"}),
            )
            .await
            .unwrap();
        // The frozen stale command fails without consuming its operation key.
        let stale = workspace
            .update_resource(
                "stale-update",
                &created.id,
                &stale_etag,
                "Checklist v2",
                &serde_json::json!({"description":"second"}),
            )
            .await;
        assert!(matches!(stale, Err(WorkspaceError::StaleRevision)));
        let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_receipt")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
        assert_eq!(receipts, 2);
        // The corrected retry reuses the same key against the fresh revision.
        let current_etag = resource_etag(workspace.epoch(), &created.id, 2);
        let corrected = workspace
            .update_resource(
                "stale-update",
                &created.id,
                &current_etag,
                "Checklist v2",
                &serde_json::json!({"description":"second"}),
            )
            .await
            .unwrap();
        assert_eq!(corrected.revision, 3);
        workspace.pool().close().await;
        let _ = fs::remove_file(path);
    }
}
