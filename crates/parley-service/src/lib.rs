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
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use rand::random;
use serde::de::{DeserializeOwned, Error as DeError, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    Connection, SqlitePool,
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
pub(crate) mod agent_runtime;
mod application;
pub(crate) mod assistant;
pub(crate) mod blob;
pub(crate) mod board;
mod commands;
mod entity;
mod generation;
mod graph;
pub(crate) mod http;
pub(crate) mod inference;
pub(crate) mod jobs;
pub(crate) mod maintenance;
mod packs;
pub(crate) mod parties;
mod quarantine;
pub mod relations;
#[cfg(debug_assertions)]
pub use maintenance::{MaintenanceSettings, MaintenanceSettingsInput};
#[doc(hidden)]
pub use maintenance::{create_backup_for_test, prune_backups_for_test};
pub(crate) mod materials;
pub(crate) mod queue_sheet;
pub(crate) mod queueing;
pub(crate) mod release;
pub(crate) mod resource_board;
mod source;
pub(crate) mod thread;
pub(crate) mod writing;
mod writing_jobs;

/// Run the offline, reviewed E-match calibration for the active qualified
/// embedding profile (AR-001 AR07). Returns the committed calibration report
/// or a typed failure. Only this path may write trusted calibration rows.
pub async fn calibrate_matching(
    workspace: &Workspace,
    fixture: serde_json::Value,
) -> Result<serde_json::Value, String> {
    crate::inference::calibrate::run(workspace, fixture).await
}
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
}

impl Workspace {
    pub fn resolve_database_path(identity: &Path) -> Result<PathBuf, WorkspaceError> {
        let root = generation::WorkspaceRoot::new(identity).map_err(WorkspaceError::Invalid)?;
        let id = root.read_selector().map_err(WorkspaceError::Invalid)?;
        root.database_path(&id).map_err(WorkspaceError::Invalid)
    }
    /// The workspace control directory that owns private runtime artifacts.
    /// This is the identity-level directory, not a nested directory derived
    /// from a generation database.
    pub fn control_path(&self) -> PathBuf {
        self.root.control_path().to_path_buf()
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
        let legacy_draft00 = quarantine::quarantine_draft00_schema(&pool).await?;
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        if legacy_draft00 {
            quarantine::convert_draft00_parties(&pool).await?;
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
        };
        maintenance::initialize_settings(&workspace).await?;
        maintenance::complete_startup_operations(&workspace, removed_generation_bytes).await?;
        // Finalize attempts from an earlier process that lack a final audit
        // record (UCR-001 Q3).
        commands::ledger::recover_interrupted_attempts(&workspace.pool)
            .await
            .map_err(|e| {
                WorkspaceError::Invalid(format!("recover interrupted command attempts: {e}"))
            })?;
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
        if let Ok(bytes) = crate::rotate_command_secret(&self.root) {
            let mut guard = self
                .command_secret
                .write()
                .expect("generation key lock poisoned");
            *guard = bytes;
        }
    }

    pub async fn generation_token(&self) -> u64 {
        *self.generation.read().await
    }

    pub async fn bump_generation(&self) {
        let mut token = self.generation.write().await;
        *token += 1;
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

/// UTC RFC 3339 timestamp offset from trusted wall time, used for leases,
/// run deadlines and approval expiry.
pub(crate) fn now_utc_offset(seconds: i64) -> String {
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

/// How long to sleep before the next known thread-deadline transition, capped
/// at `cap`. Returns `cap` when no future waiting-thread deadline exists or
/// it cannot be parsed, so a bounded safety sweep always runs. This is the
/// wake target for the deadline reconciler (L1): a fixed tick is replaced by
/// sleep-until-next-due with a safety cap.
async fn next_thread_deadline_nap(workspace: &Workspace, now: &str, cap: Duration) -> Duration {
    let next: Option<String> = sqlx::query_scalar(
        "SELECT MIN(expires_at) FROM contact_thread
         WHERE state='waiting' AND expires_at IS NOT NULL AND expires_at > ?",
    )
    .bind(now)
    .fetch_optional(workspace.pool())
    .await
    .ok()
    .flatten();
    let Some(next) = next else {
        return cap;
    };
    let (Ok(when), Ok(then)) = (
        chrono::DateTime::parse_from_rfc3339(&next),
        chrono::DateTime::parse_from_rfc3339(now),
    ) else {
        return cap;
    };
    let until = (when - then).to_std().unwrap_or_default();
    until.max(Duration::from_millis(50)).min(cap)
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
    pub(crate) jobs: Arc<tokio::sync::Notify>,
    pub(crate) restart: tokio::sync::watch::Sender<bool>,
    pub(crate) registry: Arc<commands::Registry>,
    pub(crate) writer: Arc<application::writer::Writer>,
    /// Post-commit command signals for the fetch-based SSE notification
    /// stream (UCR-001 U08 / §4.1): approvals_changed, run_changed,
    /// domain_changed, job_changed and epoch_changed. Bounded; a slow
    /// client drops older signals and must poll authoritative state after
    /// reconnect.
    pub(crate) notify: CommandSignalSender,
    /// Bounded concurrent notification connections (U08 §4.1).
    pub(crate) sse_connections: Arc<tokio::sync::Semaphore>,
}

/// One post-commit command notification signal (U08 §4.1). The stream emits
/// only these signal kinds plus a 15 s heartbeat; clients re-read
/// authoritative state after each signal. `epoch_changed` is emitted at
/// stream open and after reconnect so clients rebase their page.
///
/// L1 (de-polling design) adds the two domain-scoped hints: `DomainChanged`
/// carries the event log's high-water mark so a lagged client can catch up
/// by cursor, and `JobChanged` carries the id of a durable role job whose
/// state transitioned.
#[derive(Clone, Debug, PartialEq, Eq)]
// Signal kinds are tied to the on-the-wire SSE event names
// (approvals_changed, run_changed, domain_changed, job_changed), so the
// shared postfix is intentional naming, not a naming smell.
#[allow(clippy::enum_variant_names)]
pub(crate) enum CommandSignal {
    ApprovalsChanged,
    RunChanged,
    /// The domain event log advanced to a new high-water mark (cursor).
    DomainChanged(i64),
    /// A durable role job changed state; carries the job id.
    JobChanged(String),
}

pub(crate) type CommandSignalSender = tokio::sync::broadcast::Sender<CommandSignal>;

/// Emit a domain-level change hint carrying the event log's current
/// high-water mark. Best-effort by design: a lagged or absent receiver
/// drops the signal, and the client re-reads from its own cursor, so the
/// event log stays the single authority (U08 §4.1). Skips the zero cursor
/// (no new domain fact) to avoid waking clients on irrelevant commits.
pub(crate) async fn publish_domain_changed(notify: &CommandSignalSender, pool: &SqlitePool) {
    if let Ok(cursor) = sqlx::query_scalar::<_, i64>("SELECT COALESCE(MAX(cursor),0) FROM event")
        .fetch_one(pool)
        .await
        && cursor > 0
    {
        let _ = notify.send(CommandSignal::DomainChanged(cursor));
    }
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
    let jobs = Arc::new(tokio::sync::Notify::new());
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
    let (notify, _notify_rx) = tokio::sync::broadcast::channel(64);
    let (writer, writer_task) =
        application::writer::start(writer_conn, workspace.generation.clone(), notify.clone());
    let writer = Arc::new(writer);
    let reconcile_task = {
        // L1: reconcile expiring threads when they come due instead of on a
        // blind fixed tick. The sleep is bounded by a safety cap so a missed
        // wake or a deadline inserted after the nap is still caught on a
        // fixed cadence; expiry commits flow through the same publish hook as
        // adopted writes (domain_changed with the event-log high-water mark).
        const RECONCILE_WAKE_CAP: Duration = Duration::from_secs(15);
        let workspace = workspace.clone();
        let notify = notify.clone();
        let mut stop = restart_receiver.clone();
        tokio::spawn(async move {
            loop {
                let now = now_utc();
                let report = match workspace.reconcile_deadlines(&now).await {
                    Ok(report) => report,
                    Err(_) => crate::thread::ReconcileReport {
                        scanned: 0,
                        timed_out: 0,
                        opportunities_closed: 0,
                    },
                };
                if report.timed_out > 0 || report.opportunities_closed > 0 {
                    crate::publish_domain_changed(&notify, workspace.pool()).await;
                }
                let nap = next_thread_deadline_nap(&workspace, &now, RECONCILE_WAKE_CAP).await;
                tokio::select! {
                    _ = tokio::time::sleep(nap) => {}
                    changed = stop.changed() => { if changed.is_ok() { break; } }
                }
            }
        })
    };
    let backup_task = maintenance::start_scheduler(workspace.clone(), restart_receiver.clone());
    let sse_connections = Arc::new(tokio::sync::Semaphore::new(8));
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
        notify,
        sse_connections,
    };
    let inference_task = inference::dispatch::start(Arc::new(state.clone()));
    let bootstrap_shared = state.bootstrap.clone();
    let mut shell = Router::new().route("/", get(http::index));
    for tab in [
        "resources",
        "parties",
        "leads",
        "queue",
        "output",
        "settings",
        "activity",
    ] {
        let root = format!("/{tab}");
        shell = shell
            .route(&root, get(http::index))
            .route(&format!("{root}/"), get(http::index))
            .route(&format!("{root}/{{record}}"), get(http::index))
            .route(&format!("{root}/{{record}}/"), get(http::index));
    }
    let app = shell
        .route("/assets/{*path}", get(http::asset))
        // TD-10 W4: every non-exempt `/api/v1` route — including health,
        // bootstrap, entities, events, captures, graph and the legacy module
        // surfaces — registers through the consolidated transport table in
        // `commands::http::legacy_routes`. The per-module axum routers are
        // gone; `ci/routes.py` enforces that no `/api/v1` registration
        // exists anywhere else in the tree.
        .merge(commands::http::routes())
        .with_state(state.clone())
        .layer(axum::middleware::from_fn_with_state(
            state,
            http::legacy_operation_gate,
        ))
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
        background: vec![inference_task, writer_task, reconcile_task, backup_task],
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

pub(crate) fn idempotency_key(headers: &HeaderMap) -> Result<String, KeyRejection> {
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

/// TD-10 W2: deterministic intent digest for a legacy mutation. The digest
/// binds method, route path and the canonicalized body, so the same key with
/// a different intent is a terminal 409 instead of a silent replay. The
/// epoch is bound by the receipt row itself (owner_id, epoch, key).
pub(crate) fn canonical_intent(method: &str, path: &str, input: &Value) -> String {
    let canonical =
        parley_domain::jcs::canonicalize_value(input).unwrap_or_else(|_| input.to_string());
    digest(format!("{method}\0{path}\0{canonical}").as_bytes())
}

/// TD-10 W2: replay lookup for a legacy mutation. A matching key+intent
/// returns the stored result; the same key with a different intent fails
/// 409 `KEY_REUSED`. Both outcomes run in a short `BEGIN IMMEDIATE`
/// transaction so concurrent same-key requests serialize on the receipt row.
pub(crate) async fn gate_replay(
    pool: &sqlx::SqlitePool,
    epoch: &str,
    key: &str,
    canonical: &str,
) -> Result<Option<Value>, WorkspaceError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let old: Option<(String, String)> = sqlx::query_as(
        "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
    )
    .bind(epoch)
    .bind(key)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((stored, result)) = old {
        if stored != canonical {
            return Err(WorkspaceError::IdempotencyConflict);
        }
        let value: Value =
            serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let _ = tx.commit().await;
        return Ok(Some(value));
    }
    let _ = tx.commit().await;
    Ok(None)
}

/// Rebuild a stored receipt as a response with the private epoch headers.
pub(crate) fn gate_replay_response(epoch: &str, value: Value) -> Response {
    let mut response = Json(value).into_response();
    private_headers(&mut response, epoch);
    response
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
        let resource_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM entity WHERE subtype<>''")
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
