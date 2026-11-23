//! Content-addressed attachment store and its maintenance policy.
//!
//! Files are immutable, keyed by SHA-256 and owned by the workspace directory.
//! Uploads install through a private staging copy: bytes are counted and
//! hashed while streaming, the file is fsynced and moved into the store
//! without replacing an existing digest, then blob/upload rows commit. A
//! committed lease row protects the window between atomic installation and the
//! commit that publishes the blob; garbage collection rechecks leases,
//! snapshot pins and live references under the maintenance lock immediately
//! before unlink (RFC-0002 BQ4, BE-11).

use super::{Workspace, WorkspaceError, new_id, now_utc};
use axum::{
    Json, Router,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
};

/// Hard byte ceiling for one attachment and for one release spool.
pub const MAX_BLOB_SIZE: u64 = 64 * 1024 * 1024;
/// Bounded concurrency for verified release spools.
pub const MAX_CONCURRENT_SPOOLS: usize = 2;

pub(super) fn routes() -> Router<super::AppState> {
    Router::new()
        .route(
            "/api/v1/uploads",
            post(upload)
                .get(list_uploads)
                .layer(axum::extract::DefaultBodyLimit::max(MAX_BLOB_SIZE as usize)),
        )
        .route("/api/v1/uploads/{id}/review", post(review_upload))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UploadRecord {
    pub id: String,
    pub sha256: String,
    pub size: u64,
    pub filename: String,
    pub mime: String,
    pub share_permission: bool,
    pub reviewed: bool,
    pub revision: i64,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
struct ReviewInput {
    share_permission: bool,
}

/// Verified handle into the bounded release spool. Holding the token keeps
/// the fully verified file open so the stream can start from the exact bytes
/// that were checked.
#[derive(Clone, Debug)]
pub struct SpoolToken(pub String);

#[derive(Clone)]
pub struct ReleaseSpool {
    inner: Arc<StdMutex<SpoolInner>>,
}

struct SpoolInner {
    entries: std::collections::HashMap<String, SpoolEntry>,
}

struct SpoolEntry {
    path: PathBuf,
    sha256: String,
    file: Option<tokio::fs::File>,
}

impl Default for ReleaseSpool {
    fn default() -> Self {
        Self::new()
    }
}

impl ReleaseSpool {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(StdMutex::new(SpoolInner {
                entries: std::collections::HashMap::new(),
            })),
        }
    }

    /// Open and fully verify a store file into a bounded spool. The digest and
    /// size must match the committed metadata; the file is kept open for the
    /// later stream. A second verify of the same digest returns the existing
    /// token (idempotent), not a second spool slot.
    pub async fn verify(
        &self,
        path: PathBuf,
        sha256: &str,
        expected_size: u64,
    ) -> Result<SpoolToken, WorkspaceError> {
        {
            let inner = self.inner.lock().unwrap();
            if let Some(entry) = inner.entries.get(sha256)
                && entry.sha256 == sha256
            {
                return Ok(SpoolToken(sha256.to_owned()));
            }
        }
        let mut file = tokio::fs::File::open(&path)
            .await
            .map_err(|e| WorkspaceError::Invalid(format!("blob file is not readable: {e}")))?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut buf = vec![0u8; 65_536];
        loop {
            let read = tokio::io::AsyncReadExt::read(&mut file, &mut buf)
                .await
                .map_err(|e| WorkspaceError::Invalid(format!("blob read failed: {e}")))?;
            if read == 0 {
                break;
            }
            size += read as u64;
            hasher.update(&buf[..read]);
            if size > expected_size {
                return Err(WorkspaceError::Invalid(
                    "blob file is larger than its committed size".into(),
                ));
            }
        }
        let actual = hex::encode(hasher.finalize());
        if actual != sha256 || size != expected_size {
            return Err(WorkspaceError::Invalid(
                "blob file failed digest or size verification; bytes are not issuable".into(),
            ));
        }
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(0))
            .await
            .map_err(|e| WorkspaceError::Invalid(format!("blob rewind failed: {e}")))?;
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.entries.len() >= MAX_CONCURRENT_SPOOLS {
                return Err(WorkspaceError::Invalid(
                    "release spools are busy; retry shortly".into(),
                ));
            }
            inner.entries.insert(
                sha256.to_owned(),
                SpoolEntry {
                    path,
                    sha256: sha256.to_owned(),
                    file: Some(file),
                },
            );
        }
        Ok(SpoolToken(sha256.to_owned()))
    }

    /// Take the verified open file for streaming, releasing the spool slot.
    pub async fn take_file(&self, token: &SpoolToken) -> Result<tokio::fs::File, WorkspaceError> {
        let (file, path) = {
            let mut inner = self.inner.lock().unwrap();
            let Some(entry) = inner.entries.remove(&token.0) else {
                return Err(WorkspaceError::Invalid(
                    "release spool entry is gone".into(),
                ));
            };
            (entry.file, entry.path)
        };
        if let Some(file) = file {
            return Ok(file);
        }
        tokio::fs::File::open(&path)
            .await
            .map_err(|e| WorkspaceError::Invalid(format!("blob file is not readable: {e}")))
    }
}

impl Workspace {
    /// Stream a raw upload into the store. Bytes are hashed while streaming,
    /// fsynced, installed atomically without replacing an existing digest, and
    /// only then are blob and upload rows committed. A committed lease covers
    /// the window between installation and commit.
    pub async fn save_upload(
        &self,
        body: axum::body::Body,
        filename: Option<String>,
        mime: Option<String>,
    ) -> Result<UploadRecord, WorkspaceError> {
        self.stage_upload(body, filename, mime, false).await
    }

    async fn stage_upload(
        &self,
        body: axum::body::Body,
        filename: Option<String>,
        mime: Option<String>,
        card_private: bool,
    ) -> Result<UploadRecord, WorkspaceError> {
        let filename = filename
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "attachment.bin".to_owned());
        if filename.len() > 500 {
            return Err(WorkspaceError::Invalid("filename exceeds 500 bytes".into()));
        }
        let mime = mime
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "application/octet-stream".to_owned());
        if mime.len() > 200 {
            return Err(WorkspaceError::Invalid(
                "MIME type exceeds 200 bytes".into(),
            ));
        }
        let staging = self.blob_root.join(".staging");
        std::fs::create_dir_all(&staging)
            .map_err(|e| WorkspaceError::Invalid(format!("staging directory: {e}")))?;
        let upload_id = new_id(&filename);
        let temp_path = staging.join(format!("{upload_id}.part"));
        let mut temp = tokio::fs::File::create(&temp_path)
            .await
            .map_err(|e| WorkspaceError::Invalid(format!("stage upload: {e}")))?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| WorkspaceError::Invalid(format!("upload read: {e}")))?;
            size += chunk.len() as u64;
            if size > MAX_BLOB_SIZE {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(WorkspaceError::Invalid(format!(
                    "attachment exceeds the {MAX_BLOB_SIZE}-byte limit"
                )));
            }
            hasher.update(&chunk);
            tokio::io::AsyncWriteExt::write_all(&mut temp, &chunk)
                .await
                .map_err(|e| WorkspaceError::Invalid(format!("stage write: {e}")))?;
        }
        if size == 0 {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(WorkspaceError::Invalid("attachment is empty".into()));
        }
        tokio::io::AsyncWriteExt::flush(&mut temp)
            .await
            .map_err(|e| WorkspaceError::Invalid(format!("stage flush: {e}")))?;
        temp.sync_all()
            .await
            .map_err(|e| WorkspaceError::Invalid(format!("stage fsync: {e}")))?;
        drop(temp);
        let sha256 = hex::encode(hasher.finalize());
        // Commit the lease before publishing the file so a crash between the
        // atomic install and the blob commit leaves a GC-visible pin.
        {
            let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
            sqlx::query("INSERT INTO blob_upload_lease(upload_id,blob_sha256,size,created_at) VALUES(?,?,?,?)")
                .bind(&upload_id)
                .bind(&sha256)
                .bind(size as i64)
                .bind(now_utc())
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        self.install_blob_file(&sha256, size, &temp_path).await?;
        let now = now_utc();
        {
            let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
            sqlx::query("INSERT INTO blob(sha256,size,mime,installed_at) VALUES(?,?,?,?) ON CONFLICT(sha256) DO NOTHING")
                .bind(&sha256)
                .bind(size as i64)
                .bind(&mime)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO blob_upload(id,blob_sha256,filename,mime,size,share_permission,reviewed_by_owner,revision,created_at,card_private) VALUES(?,?,?,?,?,0,0,1,?,?)")
                .bind(&upload_id)
                .bind(&sha256)
                .bind(&filename)
                .bind(&mime)
                .bind(size as i64)
                .bind(&now)
                .bind(card_private)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM blob_upload_lease WHERE upload_id=?")
                .bind(&upload_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'upload',?,1,'upload.created',?)")
                .bind(new_id(&upload_id))
                .bind(&now)
                .bind(&upload_id)
                .bind(format!(
                    "{{\"sha256\":{:?},\"size\":{size},\"filename\":{:?}}}",
                    sha256, filename
                ))
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        Ok(UploadRecord {
            id: upload_id,
            sha256,
            size,
            filename,
            mime,
            share_permission: false,
            reviewed: false,
            revision: 1,
            created_at: now,
        })
    }

    /// Atomically move a staged file into the store. If the digest already
    /// exists the existing file is verified instead of replaced. The install
    /// lock also serializes garbage collection's final recheck.
    async fn install_blob_file(
        &self,
        sha256: &str,
        size: u64,
        temp_path: &Path,
    ) -> Result<(), WorkspaceError> {
        let _guard = self.maintenance_lock.lock().await;
        let final_path = self.blob_root.join(sha256);
        if final_path.exists() {
            let (actual_size, actual_hash) = hash_file(&final_path).await?;
            if actual_size != size || actual_hash != sha256 {
                return Err(WorkspaceError::Invalid(
                    "existing blob failed integrity verification; refusing to reuse it".into(),
                ));
            }
            let _ = tokio::fs::remove_file(temp_path).await;
            return Ok(());
        }
        std::fs::rename(temp_path, &final_path)
            .map_err(|e| WorkspaceError::Invalid(format!("install blob atomically: {e}")))?;
        sync_directory(&self.blob_root)?;
        Ok(())
    }

    /// Owner review of an uploaded attachment: binds the digest, MIME, scope
    /// and share permission. The first review is recorded exactly once; a
    /// later request with the same permission replays the record, a request
    /// with a different permission is rejected.
    pub async fn review_upload(
        &self,
        upload_id: &str,
        share_permission: bool,
    ) -> Result<UploadRecord, WorkspaceError> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let row: Option<(String, String, String, i64, i64, i64, i64)> = sqlx::query_as(
            "SELECT blob_sha256,filename,mime,size,share_permission,revision,reviewed_by_owner FROM blob_upload WHERE id=?",
        )
        .bind(upload_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((sha256, filename, mime, size, existing_permission, revision, reviewed)) = row
        else {
            return Err(WorkspaceError::Invalid("upload does not exist".into()));
        };
        let requested = i64::from(share_permission);
        if reviewed == 1 {
            if existing_permission != requested {
                return Err(WorkspaceError::Invalid(
                    "an upload review cannot be changed after it is recorded".into(),
                ));
            }
            tx.commit().await?;
            return Ok(UploadRecord {
                id: upload_id.to_owned(),
                sha256,
                size: size as u64,
                filename,
                mime,
                share_permission: existing_permission == 1,
                reviewed: true,
                revision,
                created_at: String::new(),
            });
        }
        let now = now_utc();
        let updated = sqlx::query("UPDATE blob_upload SET reviewed_by_owner=1,card_private=0,share_permission=?,revision=revision+1 WHERE id=? AND reviewed_by_owner=0")
            .bind(requested)
            .bind(upload_id)
            .execute(&mut *tx)
            .await?;
        if updated.rows_affected() != 1 {
            return Err(WorkspaceError::Invalid(
                "an upload review cannot be changed after it is recorded".into(),
            ));
        }
        sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'upload',?,?,'upload.reviewed','{}')")
            .bind(new_id(upload_id))
            .bind(&now)
            .bind(upload_id)
            .bind(revision + 1)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(UploadRecord {
            id: upload_id.to_owned(),
            sha256,
            size: size as u64,
            filename,
            mime,
            share_permission,
            reviewed: true,
            revision: revision + 1,
            created_at: now,
        })
    }

    pub async fn list_uploads(&self) -> Result<Vec<UploadRecord>, WorkspaceError> {
        let rows = sqlx::query_as::<_, (String, String, String, String, i64, i64, i64, String)>(
            "SELECT id,blob_sha256,filename,mime,size,share_permission,revision,created_at FROM blob_upload WHERE card_private=0 ORDER BY created_at DESC,id",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, sha256, filename, mime, size, permission, revision, created_at)| {
                    UploadRecord {
                        id,
                        sha256,
                        size: size as u64,
                        filename,
                        mime,
                        share_permission: permission == 1,
                        reviewed: permission == 1,
                        revision,
                        created_at,
                    }
                },
            )
            .collect())
    }

    /// Garbage collection. Expires stale upload leases, then unlinks only
    /// store files whose digest is not referenced by a committed blob row, a
    /// live lease, or a snapshot pin. The maintenance lock is held across the
    /// reference snapshot and the unlink decisions.
    pub async fn run_gc(&self) -> Result<GcReport, WorkspaceError> {
        let _guard = self.maintenance_lock.lock().await;
        sqlx::query("DELETE FROM blob_upload_lease WHERE created_at < strftime('%Y-%m-%dT%H:%M:%fZ','now','-24 hours')")
            .execute(self.pool())
            .await?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM party_import_stage WHERE expires_at<=unixepoch() OR epoch<>?")
            .bind(self.epoch())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM blob_upload WHERE card_private=1 AND unixepoch(created_at)<=unixepoch()-1800 AND NOT EXISTS(SELECT 1 FROM entity_attachment a WHERE a.upload_id=blob_upload.id) AND NOT EXISTS(SELECT 1 FROM material_version v WHERE v.blob_upload_id=blob_upload.id)").execute(&mut *tx).await?;
        sqlx::query("DELETE FROM blob WHERE NOT EXISTS(SELECT 1 FROM blob_upload WHERE blob_sha256=blob.sha256) AND NOT EXISTS(SELECT 1 FROM material_version WHERE blob_sha256=blob.sha256) AND NOT EXISTS(SELECT 1 FROM blob_snapshot_pin WHERE blob_sha256=blob.sha256) AND NOT EXISTS(SELECT 1 FROM blob_upload_lease WHERE blob_sha256=blob.sha256)").execute(&mut *tx).await?;
        tx.commit().await?;
        let referenced: HashSet<String> = sqlx::query_scalar(
            "SELECT DISTINCT sha256 FROM blob UNION SELECT DISTINCT blob_sha256 FROM blob_upload_lease UNION SELECT DISTINCT blob_sha256 FROM blob_snapshot_pin",
        )
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .collect();
        let mut report = GcReport::default();
        let mut entries = tokio::fs::read_dir(&self.blob_root)
            .await
            .map_err(|e| WorkspaceError::Invalid(format!("read blob store: {e}")))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| WorkspaceError::Invalid(format!("scan blob store: {e}")))?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".staging"
                || name.len() != 64
                || !name.bytes().all(|b| b.is_ascii_hexdigit())
            {
                continue;
            }
            report.scanned += 1;
            if referenced.contains(&name) {
                report.kept += 1;
                continue;
            }
            match tokio::fs::remove_file(entry.path()).await {
                Ok(()) => report.removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(WorkspaceError::Invalid(format!("unlink orphan blob: {e}")));
                }
            }
        }
        Ok(report)
    }

    /// Pin blob digests held by a snapshot/backup. A pin survives GC until the
    /// snapshot pin is released.
    pub async fn pin_snapshot_blobs(
        &self,
        snapshot_id: &str,
        digests: &[String],
    ) -> Result<(), WorkspaceError> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let now = now_utc();
        for digest in digests {
            sqlx::query("INSERT INTO blob_snapshot_pin(snapshot_id,blob_sha256,created_at) VALUES(?,?,?) ON CONFLICT DO NOTHING")
                .bind(snapshot_id)
                .bind(digest)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn release_snapshot_pin(&self, snapshot_id: &str) -> Result<(), WorkspaceError> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM blob_snapshot_pin WHERE snapshot_id=?")
            .bind(snapshot_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[derive(Debug, Default, Serialize)]
pub struct GcReport {
    pub scanned: u64,
    pub kept: u64,
    pub removed: u64,
}

async fn hash_file(path: &Path) -> Result<(u64, String), WorkspaceError> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| WorkspaceError::Invalid(format!("blob file is not readable: {e}")))?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; 65_536];
    loop {
        let read = tokio::io::AsyncReadExt::read(&mut file, &mut buf)
            .await
            .map_err(|e| WorkspaceError::Invalid(format!("blob read failed: {e}")))?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buf[..read]);
    }
    Ok((size, hex::encode(hasher.finalize())))
}

fn sync_directory(path: &Path) -> Result<(), WorkspaceError> {
    let dir = std::fs::File::open(path)
        .map_err(|e| WorkspaceError::Invalid(format!("open blob directory: {e}")))?;
    dir.sync_all()
        .map_err(|e| WorkspaceError::Invalid(format!("fsync blob directory: {e}")))
}

async fn upload(
    State(state): State<super::AppState>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    if let Err(error) = super::authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    let filename = headers
        .get("x-parley-filename")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            let decoded = form_urlencoded::parse(value.as_bytes()).next();
            match decoded {
                Some((_, decoded)) if !decoded.is_empty() => decoded.into_owned(),
                _ => value.to_owned(),
            }
        })
        .filter(|value| !value.trim().is_empty());
    let mime = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let card_private = headers
        .get("x-parley-purpose")
        .is_some_and(|v| v == "card-attachment");
    match state
        .workspace
        .stage_upload(body, filename, mime, card_private)
        .await
    {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(error) => crate::materials::workspace_error(error),
    }
}

async fn list_uploads(State(state): State<super::AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = super::authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.list_uploads().await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => crate::materials::workspace_error(error),
    }
}

async fn review_upload(
    State(state): State<super::AppState>,
    AxumPath(upload_id): AxumPath<String>,
    headers: HeaderMap,
    Json(input): Json<ReviewInput>,
) -> Response {
    if let Err(error) = super::authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state
        .workspace
        .review_upload(&upload_id, input.share_permission)
        .await
    {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => crate::materials::workspace_error(error),
    }
}

/// Shared store access for other modules (tests and release).
impl Workspace {
    pub(crate) fn blob_path(&self, sha256: &str) -> PathBuf {
        self.blob_root.join(sha256)
    }

    /// Full-file integrity verification without holding a spool slot. Used at
    /// release time to prove the issuable bytes still match the committed
    /// digest before any download grant can be consumed.
    pub(crate) async fn verify_blob_file(
        &self,
        sha256: &str,
        expected_size: u64,
    ) -> Result<(), WorkspaceError> {
        let (actual_size, actual_hash) = hash_file(&self.blob_root.join(sha256)).await?;
        if actual_size != expected_size || actual_hash != sha256 {
            return Err(WorkspaceError::Invalid(
                "blob file failed digest or size verification; bytes are not issuable".into(),
            ));
        }
        Ok(())
    }
}
