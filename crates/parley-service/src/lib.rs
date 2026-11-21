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
    path::Path,
    str::FromStr,
    sync::{
        Arc,
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
    routing::{get, post},
};
use rand::random;
use serde::de::{DeserializeOwned, Error as DeError, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::fmt;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

static IDS: AtomicU64 = AtomicU64::new(0);

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

#[derive(Clone)]
pub struct Workspace {
    pool: SqlitePool,
    epoch: String,
    workspace_id: String,
}

impl Workspace {
    /// Open and migrate a private SQLite workspace. Every connection enables
    /// foreign keys and uses FULL synchronous durability in WAL mode.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let path = path.as_ref();
        let mut created_parent = false;
        if let Some(parent) = path.parent() {
            created_parent = !parent.exists();
            std::fs::create_dir_all(parent).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(2));
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
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
            if created_parent && let Some(parent) = path.parent() {
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                    .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            }
        }
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let (epoch, workspace_id): (String, String) =
            sqlx::query_as("SELECT epoch, workspace_id FROM workspace_meta WHERE singleton=1")
                .fetch_one(&pool)
                .await?;
        Ok(Self {
            pool,
            epoch,
            workspace_id,
        })
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
        let mut builder = sqlx::QueryBuilder::new("SELECT id,kind,name,revision FROM entity");
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
            .build_query_as::<(String, String, String, i64)>()
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|(id, kind, name, revision)| EntityRecord {
                id,
                kind,
                name,
                revision,
            })
            .collect())
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
    /// people with the same display name remain separate records. The entity,
    /// its creation event and the receipt commit under one writer reservation.
    pub async fn create_entity(
        &self,
        idempotency_key: &str,
        kind: &str,
        name: &str,
    ) -> Result<EntityRecord, WorkspaceError> {
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
        let canonical = digest(format!("entity\0{kind}\0{name}").as_bytes());
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
            "INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES(?,?,?,1,?,?)",
        )
        .bind(&id)
        .bind(kind)
        .bind(name)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'entity',?,1,'entity.created','{}')")
            .bind(new_id(&id)).bind(&now).bind(&id).execute(&mut *tx).await?;
        let record = EntityRecord {
            id: id.clone(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            revision: 1,
        };
        let result_json =
            serde_json::to_string(&record).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,201,?,(SELECT COALESCE(MAX(cursor),0) FROM event),?)")
            .bind(&self.epoch).bind(idempotency_key).bind(canonical).bind(&result_json).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(record)
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

    /// Last committed workspace event sequence, used as the read cursor.
    pub async fn current_cursor(&self) -> Result<i64, WorkspaceError> {
        let cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
            .fetch_one(&self.pool)
            .await?;
        Ok(cursor)
    }
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn new_id(seed: &str) -> String {
    let nonce = IDS.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    digest(format!("{seed}:{nanos}:{nonce}").as_bytes())[..32].to_owned()
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

fn random_hex(size: usize) -> String {
    let bytes: Vec<u8> = (0..size).map(|_| random::<u8>()).collect();
    hex::encode(bytes)
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Session {
    token: String,
}

#[derive(Clone)]
struct AppState {
    workspace: Arc<Workspace>,
    authority: String,
    bootstrap: Arc<Mutex<HashMap<String, Instant>>>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
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
    let state = AppState {
        workspace: Arc::new(workspace),
        authority: address.to_string(),
        bootstrap: Arc::new(Mutex::new(HashMap::from([(
            bootstrap_secret.clone(),
            Instant::now() + Duration::from_secs(60),
        )]))),
        sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    let bootstrap_shared = state.bootstrap.clone();
    let app = Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .route("/api/v1/health", get(health))
        .route("/api/v1/session/bootstrap", post(bootstrap))
        .route("/api/v1/session", get(session))
        .route("/api/v1/entities", get(list_entities).post(create_entity))
        .route(
            "/api/v1/entities/{id}",
            get(get_entity).patch(update_entity),
        )
        .route("/api/v1/events", get(get_events))
        .route("/api/v1/captures", get(list_captures).post(save_capture))
        .route("/api/v1/graph/related", post(create_related_entity))
        .with_state(state)
        .layer(axum::extract::DefaultBodyLimit::max(2_100_000));
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

fn command_error(err: WorkspaceError) -> Response {
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
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
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
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
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
        "capabilities": ["resources.read", "resources.write"],
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
    if !graph_kinds.is_empty() || kinds.is_empty() {
        match state.workspace.list_entities_filtered(&graph_kinds).await {
            Ok(entities) => items.extend(entities.into_iter().map(|entity| {
                json!({"id": entity.id, "kind": entity.kind, "name": entity.name, "revision": entity.revision})
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
struct GraphEntityCreateInput {
    kind: String,
    name: String,
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
            .create_entity(&key, &input.kind, &input.name)
            .await
        {
            Ok(entity) => {
                let mut response = (
                    StatusCode::CREATED,
                    Json(json!({"data": {"id": entity.id, "kind": entity.kind, "name": entity.name, "revision": entity.revision}})),
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
    }
}

#[derive(Deserialize)]
struct UpdateInput {
    title: String,
    body: Value,
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
        Err(err) => command_error(err),
    }
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
    let observed = state.workspace.current_cursor().await.unwrap_or(0);
    if after > observed {
        return error(
            StatusCode::CONFLICT,
            "CURSOR_AHEAD",
            "Event cursor is ahead of the workspace",
        );
    }
    let rows = match sqlx::query_as::<_, (i64, String, String, String, i64, String, String)>(
        "SELECT cursor,event_id,entity_type,entity_id,revision,event_type,payload_json FROM event WHERE cursor>? ORDER BY cursor LIMIT 200",
    )
    .bind(after)
    .fetch_all(state.workspace.pool())
    .await
    {
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
            |(cursor, event_id, entity_type, entity_id, revision, event_type, payload)| {
                json!({
                    "cursor": cursor.to_string(),
                    "event_id": event_id,
                    "entity_type": entity_type,
                    "entity_id": entity_id,
                    "revision": revision,
                    "event_type": event_type,
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
    match state
        .workspace
        .save_source(&input.text, input.title.as_deref(), input.url.as_deref())
        .await
    {
        Ok(source) => {
            let mut response = (StatusCode::CREATED, Json(source)).into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(WorkspaceError::Invalid(message)) => {
            error(StatusCode::BAD_REQUEST, "INVALID_CAPTURE", message)
        }
        Err(err) => command_error(err),
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
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<RelatedEntityRequest>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    match state
        .workspace
        .create_related_entity(
            &input.kind,
            &input.name,
            &input.related_entity_id,
            &input.verb,
            &input.occurrence_id,
            &input.quote,
            &input.view_scope,
            &input.view_id,
            input.x,
            input.y,
        )
        .await
    {
        Ok(entity) => {
            let mut response = (
                StatusCode::CREATED,
                Json(json!({"id": entity.id, "kind": entity.kind, "name": entity.name, "revision": entity.revision})),
            )
                .into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(WorkspaceError::Invalid(message)) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "RELATIONSHIP_REJECTED",
            message,
        ),
        Err(err) => command_error(err),
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
            .create_entity("dana-first", "person", "Dana Lee")
            .await
            .unwrap();
        let second = workspace
            .create_entity("dana-second", "person", "Dana Lee")
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
