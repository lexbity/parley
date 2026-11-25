//! Owner-controlled backup policy and verified snapshot operations.
use super::{AppState, Workspace, WorkspaceError, authenticate, new_id, now_utc};
use axum::{
    Json,
    body::Body,
    extract::{Path as AxumPath, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{AssertSqlSafe, Connection, sqlite::SqlitePoolOptions};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// Policy for detached archives. Disposable tables stay empty, preserving
/// the forward SQLx schema and retained product lineage.
pub(crate) const COMMAND_TABLE_POLICY_VERSION: u32 = 5;
const ARCHIVE_FORMAT: u32 = 2;
const PRIVATE_ARCHIVE_TABLES: &[&str] = &[
    "command_audit_event",
    "command_receipt",
    "command_ledger_meta",
    "command_approval",
    "agent_run",
    "agent_run_target",
    "agent_run_turn",
    "agent_tool_intent",
    "agent_run_budget",
    "durable_job",
    "job_attempt",
    "coach_question_hold",
    "template_build_candidate",
    "inference_job_dependency",
    "inference_active_profile",
    "inference_probe",
    "inference_qualification",
    "inference_profile_revocation",
    "inference_profile",
    "embedding_cache",
    "idempotency_receipt",
    "maintenance_preview",
    "blob_snapshot_pin",
    "blob_upload_lease",
];
const PORTABLE_TABLES: &[&str] = &[
    "_sqlx_migrations",
    "accepted_hook",
    "agent_chat",
    "agent_prompt_active",
    "agent_prompt_version",
    "agent_turn",
    "approval",
    "blob",
    "blob_upload",
    "board_list",
    "board_placement",
    "brief_item",
    "capacity_ledger",
    "claim",
    "claim_revision",
    "coach_question",
    "coach_report",
    "contact_thread",
    "delivered_resource_version",
    "edge",
    "edge_evidence",
    "edge_history",
    "entity",
    "entity_attachment",
    "erasure_authorization",
    "erasure_tombstone",
    "event",
    "exchange_interest_link",
    "exchange_version",
    "fluency",
    "inference_record",
    "interviewer_candidate",
    "interviewer_question_bank_active",
    "interviewer_question_bank_version",
    "interviewer_question_state",
    "legacy_conversion_diagnostic",
    "legacy_key_map",
    "maintenance_operation",
    "maintenance_settings",
    "match_run",
    "match_run_mention",
    "matching_calibration",
    "material",
    "material_lint",
    "material_version",
    "opportunity",
    "opportunity_interest",
    "opportunity_party",
    "opportunity_tag",
    "outcome",
    "outcome_revision",
    "owner_identity_revision",
    "owner_identity_tag",
    "owner_profile",
    "pack_stage",
    "pack_document",
    "party_identity_tag",
    "party_import_batch",
    "party_import_origin",
    "party_import_stage",
    "party_policy",
    "party_stage_receipt",
    "proposal_batch",
    "proposal_decision",
    "proposal_item",
    "release_ack",
    "release_record",
    "resolution",
    "resource_column",
    "resource_placement",
    "resource_tag",
    "resource_use",
    "segment",
    "segment_party",
    "send",
    "send_part",
    "source_content",
    "source_occurrence",
    "source_opportunity_link",
    "source_variant",
    "template_family",
    "template_version",
    "unresolved_mention",
    "version_dependency",
    "view_position",
    "voice_sample",
    "workspace_meta",
];

async fn verify_portable_schema(conn: &mut sqlx::SqliteConnection) -> Result<(), sqlx::Error> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&mut *conn)
    .await?;
    if tables.iter().any(|name| {
        !PRIVATE_ARCHIVE_TABLES.contains(&name.as_str())
            && !PORTABLE_TABLES.contains(&name.as_str())
            && !name.starts_with("draft00_")
    }) {
        return Err(sqlx::Error::Protocol(
            "archive contains an unclassified table".into(),
        ));
    }
    Ok(())
}

async fn verify_database_integrity(conn: &mut sqlx::SqliteConnection) -> Result<(), sqlx::Error> {
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&mut *conn)
        .await?;
    let orphan: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pragma_foreign_key_check)")
        .fetch_one(&mut *conn)
        .await?;
    if integrity != "ok" || orphan {
        return Err(sqlx::Error::Protocol(
            "archive failed database integrity checks".into(),
        ));
    }
    Ok(())
}

async fn open_archive_database(
    db: &Path,
    read_only: bool,
) -> Result<sqlx::SqliteConnection, sqlx::Error> {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db)
        .read_only(read_only)
        .foreign_keys(true);
    sqlx::sqlite::SqliteConnection::connect_with(&options).await
}

async fn verify_portable_database(db: &Path) -> Result<(), sqlx::Error> {
    let mut conn = open_archive_database(db, true).await?;
    verify_portable_schema(&mut conn).await?;
    verify_database_integrity(&mut conn).await?;
    for table in PRIVATE_ARCHIVE_TABLES {
        let has_rows: bool = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT EXISTS(SELECT 1 FROM {table})"
        )))
        .fetch_one(&mut conn)
        .await?;
        if has_rows {
            return Err(sqlx::Error::Protocol(
                "archive contains private execution state".into(),
            ));
        }
    }
    Ok(())
}

/// Apply the portable policy with foreign keys enabled, including the
/// proposal decisions and minimized lineage permitted in a detached copy.
async fn scrub_archive_database(db: &Path) -> Result<(), sqlx::Error> {
    let mut conn = open_archive_database(db, false).await?;
    verify_portable_schema(&mut conn).await?;
    let mut tx = conn.begin().await?;
    for table in PRIVATE_ARCHIVE_TABLES {
        // Order handles restricted foreign keys before deleting their parent.
        sqlx::query(AssertSqlSafe(format!("DELETE FROM {table}")))
            .execute(&mut *tx)
            .await?;
    }
    sqlx::raw_sql(
        "DELETE FROM coach_question WHERE status = 'open';
         UPDATE coach_question SET text = '';
         UPDATE coach_report SET
           report_json = json_object(
             'period',period,
             'event_cursor',CAST(event_cursor AS TEXT),
             'evidence',json_object(
               'candidate_count',CASE WHEN json_valid(report_json)
                 THEN CAST(json_extract(report_json,'$.evidence.candidate_count') AS INTEGER) END
             )
           );
         DELETE FROM brief_item WHERE status = 'proposed';
         DELETE FROM interviewer_candidate WHERE state = 'proposed';
         UPDATE interviewer_candidate SET fields_json = '{}', owner_quotes_json = '{}',
           missing_fields_json = '[]' WHERE state = 'dismissed';
         DELETE FROM inference_record WHERE id NOT IN (
           SELECT job_id FROM agent_turn WHERE job_id IS NOT NULL
           UNION SELECT job_id FROM interviewer_candidate
           UNION SELECT job_id FROM brief_item WHERE job_id IS NOT NULL
           UNION SELECT job_id FROM coach_report
           UNION SELECT job_id FROM coach_question
           UNION SELECT job_id FROM material_version WHERE job_id IS NOT NULL
           UNION SELECT linter_job_id FROM material_lint WHERE linter_job_id IS NOT NULL
           UNION SELECT why_job_id FROM match_run WHERE why_job_id IS NOT NULL
           UNION SELECT job_id FROM proposal_batch WHERE job_id IS NOT NULL
         );",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    sqlx::query("VACUUM").execute(&mut conn).await?;
    verify_database_integrity(&mut conn).await
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaintenanceSettings {
    pub backup_directory: String,
    pub interval_hours: i64,
    pub retention_count: i64,
    pub last_successful_at: Option<String>,
    pub revision: i64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceSettingsInput {
    pub backup_directory: String,
    pub interval_hours: i64,
    pub retention_count: i64,
    pub expected_revision: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BlobRecord {
    sha256: String,
    size: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    format_version: u32,
    policy_version: u32,
    backup_id: String,
    created_at: String,
    workspace_id: String,
    db_sha256: String,
    db_size: u64,
    blobs: Vec<BlobRecord>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportManifest {
    format_version: u32,
    policy_version: u32,
    workspace_id: String,
    database_sha256: String,
    database_size: u64,
    blobs: Vec<BlobRecord>,
    state_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportCommitInput {
    preview_id: String,
    preview_digest: String,
    target_must_be_empty: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestorePreviewInput {
    backup_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestoreCommitInput {
    preview_id: String,
    preview_digest: String,
    acknowledge_erased_resurrection: bool,
}
#[derive(Serialize)]
pub(crate) struct RestorePreview {
    id: String,
    digest: String,
    backup: serde_json::Value,
    current_cursor: String,
    erasure_resurrections: u64,
    required_bytes: u64,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ErasurePreview {
    id: String,
    digest: String,
    domain_cursor: String,
    affected_counts: std::collections::BTreeMap<String, u64>,
    shared_sources: u64,
    owner_managed_backups: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ErasureCommitInput {
    preview_id: String,
    preview_digest: String,
}
#[derive(Serialize)]
pub(crate) struct MaintenanceOperation {
    id: String,
    kind: String,
    state: String,
    epoch: String,
    external_copies_requiring_disposal: u64,
}
pub(crate) async fn initialize_settings(w: &Workspace) -> Result<(), WorkspaceError> {
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM maintenance_settings WHERE singleton=1")
            .fetch_optional(w.pool())
            .await?;
    if exists.is_none() {
        let d = w.generation_root().default_backup_path();
        fs::create_dir_all(&d).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        secure(&d).map_err(WorkspaceError::Invalid)?;
        sqlx::query("INSERT INTO maintenance_settings(singleton,backup_directory,interval_hours,retention_count,revision,updated_at) VALUES(1,?,24,14,1,?)").bind(d.to_string_lossy().as_ref()).bind(now_utc()).execute(w.pool()).await?;
    }
    Ok(())
}

pub(crate) async fn complete_startup_operations(
    w: &Workspace,
    removed: u64,
) -> Result<(), WorkspaceError> {
    let mut tx = w.pool().begin_with("BEGIN IMMEDIATE").await?;
    let rows:Vec<(String,Option<String>,String)>=sqlx::query_as("SELECT id,subject_id,result_json FROM maintenance_operation WHERE status='pending_restart'").fetch_all(&mut *tx).await?;
    for (id, subject, result) in rows {
        if let Some(entity) = subject {
            let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entity WHERE id=?")
                .bind(entity)
                .fetch_one(&mut *tx)
                .await?;
            if exists != 0 {
                continue;
            }
        }
        let mut payload: serde_json::Value = serde_json::from_str(&result).unwrap_or(json!({}));
        payload["removed_generation_bytes"] = json!(removed);
        sqlx::query("UPDATE maintenance_operation SET status='complete',result_json=?,completed_at=?,subject_id=NULL WHERE id=?").bind(payload.to_string()).bind(now_utc()).bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub(crate) fn start_scheduler(
    workspace: std::sync::Arc<Workspace>,
    mut restart: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(900));
        loop {
            tokio::select! {_ = interval.tick()=>{let _=workspace.run_gc().await;let due=match workspace.maintenance_settings().await{Ok(s)=>match s.last_successful_at{None=>true,Some(v)=>chrono::DateTime::parse_from_rfc3339(&v).ok().is_none_or(|t|chrono::Utc::now().signed_duration_since(t).num_hours()>=s.interval_hours)},Err(_)=>false};if due{let _=make_backup(&workspace).await;}},r=restart.changed()=>{if r.is_ok(){break;}}}
        }
    })
}
pub(crate) async fn get_operation(
    State(s): State<AppState>,
    AxumPath(id): AxumPath<String>,
    h: HeaderMap,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    let r: Option<(String, String, String, String)> =
        sqlx::query_as("SELECT id,kind,status,epoch FROM maintenance_operation WHERE id=?")
            .bind(id)
            .fetch_optional(s.workspace.pool())
            .await
            .ok()
            .flatten();
    match r {
        Some((id, kind, state, epoch)) => {
            let details: Option<String> =
                sqlx::query_scalar("SELECT result_json FROM maintenance_operation WHERE id=?")
                    .bind(&id)
                    .fetch_optional(s.workspace.pool())
                    .await
                    .ok()
                    .flatten();
            let external = details
                .and_then(|x| serde_json::from_str::<serde_json::Value>(&x).ok())
                .and_then(|x| x["external_copies_requiring_disposal"].as_u64())
                .unwrap_or(0);
            private_json(
                &s,
                json!({"id":id,"kind":kind,"state":state,"epoch":epoch,"external_copies_requiring_disposal":external}),
            )
        }
        None => super::error(
            StatusCode::NOT_FOUND,
            "OPERATION_NOT_FOUND",
            "Maintenance operation not found",
        ),
    }
}
pub(crate) async fn preview_erasure(
    State(s): State<AppState>,
    AxumPath(id): AxumPath<String>,
    h: HeaderMap,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    };
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM entity WHERE id=?")
        .bind(&id)
        .fetch_optional(s.workspace.pool())
        .await
        .ok()
        .flatten();
    let Some(name) = name else {
        return super::error(
            StatusCode::NOT_FOUND,
            "ENTITY_NOT_FOUND",
            "Entity not found",
        );
    };
    let cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
        .fetch_one(s.workspace.pool())
        .await
        .unwrap_or(-1);
    let pid = new_id("erase-preview");
    let digest = hash(format!("{}\0{}\0{}", id, name, cursor).as_bytes());
    let backups: u64 = s
        .workspace
        .maintenance_settings()
        .await
        .ok()
        .map(|m| {
            fs::read_dir(m.backup_directory)
                .map(|d| d.count() as u64)
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let payload = json!({"entity_id":id,"name":name,"external_copies_requiring_disposal":backups});
    let now = now_utc();
    let inserted=sqlx::query("INSERT INTO maintenance_preview(id,kind,subject_id,digest,domain_cursor,payload_json,created_at,expires_at) VALUES(?,'erasure',?,?,?, ?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now','+15 minutes'))").bind(&pid).bind(&id).bind(&digest).bind(cursor).bind(payload.to_string()).bind(now).execute(s.workspace.pool()).await;
    if inserted.is_err() {
        return super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "ERASURE_PREVIEW_FAILED",
            "Could not persist erasure preview",
        );
    };
    private_json(
        &s,
        json!(ErasurePreview {
            id: pid,
            digest,
            domain_cursor: cursor.to_string(),
            affected_counts: std::collections::BTreeMap::from([("entity".into(), 1)]),
            shared_sources: 0,
            owner_managed_backups: backups
        }),
    )
}
pub(crate) async fn commit_erasure(
    State(s): State<AppState>,
    AxumPath(id): AxumPath<String>,
    h: HeaderMap,
    Json(i): Json<ErasureCommitInput>,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    };
    let _lock = s.workspace.maintenance_lock.lock().await;
    let row:Option<(String,i64,String)>=sqlx::query_as("SELECT digest,domain_cursor,payload_json FROM maintenance_preview WHERE id=? AND kind='erasure' AND subject_id=? AND consumed_at IS NULL AND expires_at>strftime('%Y-%m-%dT%H:%M:%fZ','now')").bind(&i.preview_id).bind(&id).fetch_optional(s.workspace.pool()).await.ok().flatten();
    let Some((digest, cursor, payload)) = row else {
        return super::error(
            StatusCode::CONFLICT,
            "ERASURE_PREVIEW_EXPIRED",
            "Erasure preview expired",
        );
    };
    if digest != i.preview_digest {
        return super::error(
            StatusCode::CONFLICT,
            "ERASURE_PREVIEW_MISMATCH",
            "Erasure digest mismatch",
        );
    };
    let current: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
        .fetch_one(s.workspace.pool())
        .await
        .unwrap_or(-1);
    if current != cursor {
        return super::error(
            StatusCode::CONFLICT,
            "ERASURE_PREVIEW_STALE",
            "Workspace changed after preview",
        );
    };
    let value: serde_json::Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(_) => {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "ERASURE_PREVIEW_INVALID",
                "Preview data invalid",
            );
        }
    };
    let name = value["name"].as_str().unwrap_or("").to_owned();
    let external_copies = value["external_copies_requiring_disposal"]
        .as_u64()
        .unwrap_or(0);
    let epoch = new_id("epoch");
    let operation_id = new_id("erase-op");
    let new_id_gen = new_id("generation");
    let target = match s
        .workspace
        .generation_root()
        .prepare_generation(&new_id_gen)
    {
        Ok(v) => v,
        Err(_) => {
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "ERASURE_STAGE_FAILED",
                "Could not stage clean workspace",
            );
        }
    };
    let dest_db = target.join("workspace.sqlite");
    let mut connection = match s.workspace.pool().acquire().await {
        Ok(c) => c,
        Err(_) => {
            let _ = fs::remove_dir_all(target);
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "ERASURE_STAGE_FAILED",
                "Could not read workspace",
            );
        }
    };
    let dst = dest_db.to_string_lossy().to_string();
    if sqlx::query("VACUUM INTO ?")
        .bind(dst)
        .execute(&mut *connection)
        .await
        .is_err()
    {
        let _ = fs::remove_dir_all(target);
        return super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "ERASURE_STAGE_FAILED",
            "Could not snapshot workspace",
        );
    };
    if let Err(e) = erase_from_database(
        &dest_db,
        &id,
        &name,
        s.workspace.workspace_id(),
        &new_id_gen,
        &epoch,
        &operation_id,
        external_copies,
    )
    .await
    {
        let _ = fs::remove_dir_all(target);
        return super::error(StatusCode::UNPROCESSABLE_ENTITY, "ERASURE_FAILED", &e);
    };
    let blobs = target.join("blobs");
    let _ = fs::create_dir(&blobs);
    let refs: Vec<String> = match blob_digests(&dest_db).await {
        Ok(v) => v,
        Err(e) => {
            let _ = fs::remove_dir_all(target);
            return super::error(StatusCode::UNPROCESSABLE_ENTITY, "ERASURE_FAILED", &e);
        }
    };
    for sha in refs {
        let _ = fs::copy(s.workspace.blob_root().join(&sha), blobs.join(&sha));
    }
    let mut tx = match s.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(v) => v,
        Err(_) => {
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "ERASURE_COMMIT_FAILED",
                "Could not reserve workspace",
            );
        }
    };
    let actual: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
        .fetch_one(&mut *tx)
        .await
        .unwrap_or(-1);
    if actual != cursor {
        return super::error(
            StatusCode::CONFLICT,
            "ERASURE_PREVIEW_STALE",
            "Workspace changed during erasure",
        );
    };
    if s.workspace.generation_root().publish(&new_id_gen).is_err() {
        return super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "ERASURE_PUBLISH_FAILED",
            "Could not publish clean generation",
        );
    };
    // The replacement rotates the generation key and advances the adopted
    // generation token: pending writer jobs fail closed (UCR-001 §5.1).
    s.workspace.rotate_command_secret();
    s.workspace.bump_generation().await;
    let now = now_utc();
    let _ = sqlx::query("UPDATE maintenance_preview SET consumed_at=? WHERE id=?")
        .bind(&now)
        .bind(&i.preview_id)
        .execute(&mut *tx)
        .await;
    let _ = tx.commit().await;
    let restart = s.restart.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        restart.send_replace(true);
    });
    let mut response = private_json(
        &s,
        json!(MaintenanceOperation {
            id: operation_id,
            kind: "erasure".into(),
            state: "pending_restart".into(),
            epoch,
            external_copies_requiring_disposal: external_copies
        }),
    );
    *response.status_mut() = StatusCode::ACCEPTED;
    response
}
impl Workspace {
    pub async fn maintenance_settings(&self) -> Result<MaintenanceSettings, WorkspaceError> {
        let r:(String,i64,i64,Option<String>,i64)=sqlx::query_as("SELECT backup_directory,interval_hours,retention_count,last_successful_at,revision FROM maintenance_settings WHERE singleton=1").fetch_one(self.pool()).await?;
        Ok(MaintenanceSettings {
            backup_directory: r.0,
            interval_hours: r.1,
            retention_count: r.2,
            last_successful_at: r.3,
            revision: r.4,
        })
    }
    pub async fn update_maintenance_settings(
        &self,
        i: MaintenanceSettingsInput,
    ) -> Result<MaintenanceSettings, WorkspaceError> {
        if !(24..=168).contains(&i.interval_hours) || !(1..=90).contains(&i.retention_count) {
            return Err(WorkspaceError::Invalid(
                "interval must be 24..168h and retention 1..90".into(),
            ));
        }
        let p = PathBuf::from(&i.backup_directory);
        if !p.is_absolute()
            || p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(WorkspaceError::Invalid(
                "backup directory must be absolute and contain no parent traversal".into(),
            ));
        }
        fs::create_dir_all(&p).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        secure(&p).map_err(WorkspaceError::Invalid)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let rev: i64 =
            sqlx::query_scalar("SELECT revision FROM maintenance_settings WHERE singleton=1")
                .fetch_one(&mut *tx)
                .await?;
        if rev != i.expected_revision {
            return Err(WorkspaceError::StaleRevision);
        }
        sqlx::query("UPDATE maintenance_settings SET backup_directory=?,interval_hours=?,retention_count=?,revision=revision+1,updated_at=? WHERE singleton=1").bind(p.to_string_lossy().as_ref()).bind(i.interval_hours).bind(i.retention_count).bind(now_utc()).execute(&mut *tx).await?;
        tx.commit().await?;
        self.maintenance_settings().await
    }
}

#[doc(hidden)]
pub async fn create_backup_for_test(w: &Workspace) -> Result<String, String> {
    Ok(make_backup(w).await?.backup_id)
}
pub fn prune_backups_for_test(root: &Path, retain: usize) -> Result<(), String> {
    prune_verified_backups(root, retain)
}
pub(crate) async fn get_settings(State(s): State<AppState>, h: HeaderMap) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    match s.workspace.maintenance_settings().await {
        Ok(v) => private_json(&s, json!(v)),
        Err(_) => super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "MAINTENANCE_READ_FAILED",
            "Could not read maintenance settings",
        ),
    }
}
pub(crate) async fn update_settings(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(i): Json<MaintenanceSettingsInput>,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    match s.workspace.update_maintenance_settings(i).await {
        Ok(v) => private_json(&s, json!(v)),
        Err(WorkspaceError::StaleRevision) => super::error(
            StatusCode::CONFLICT,
            "STALE_REVISION",
            "Maintenance settings changed; reload before saving",
        ),
        Err(_) => super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "MAINTENANCE_SETTINGS_INVALID",
            "Maintenance settings could not be saved",
        ),
    }
}
pub(crate) async fn create_backup(State(s): State<AppState>, h: HeaderMap) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    match make_backup(&s.workspace).await {
        Ok(m) => private_json(
            &s,
            json!({"id":m.backup_id,"created_at":m.created_at,"status":"verified","manifest_sha256":hash(&serde_json::to_vec(&m).unwrap_or_default()),"workspace_id":m.workspace_id,"format_version":m.format_version}),
        ),
        Err(_) => super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "BACKUP_FAILED",
            "Could not create a verified backup",
        ),
    }
}
pub(crate) async fn create_export(State(s): State<AppState>, h: HeaderMap) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    let _guard = s.workspace.maintenance_lock.lock().await;
    let id = new_id("export");
    let control = s.workspace.generation_root().control_path();
    let db = control.join(format!(".{id}.sqlite"));
    let archive = control.join(format!(".{id}.zip"));
    let d = db.to_string_lossy().to_string();
    if sqlx::query("VACUUM INTO ?")
        .bind(d)
        .execute(s.workspace.pool())
        .await
        .is_err()
    {
        return super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "EXPORT_SNAPSHOT_FAILED",
            "Could not create a consistent database snapshot",
        );
    }
    if scrub_archive_database(&db).await.is_err() {
        let _ = fs::remove_file(&db);
        return super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "EXPORT_SCRUB_FAILED",
            "Could not scrub transient command state",
        );
    }
    let blobs = match archive_blob_records(&db).await {
        Ok(records) => records,
        Err(_) => {
            let _ = fs::remove_file(&db);
            return super::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "EXPORT_READ_FAILED",
                "Could not read attachment references",
            );
        }
    };
    for blob in &blobs {
        if file_hash(&s.workspace.blob_root().join(&blob.sha256)).ok()
            != Some((blob.size, blob.sha256.clone()))
        {
            let _ = fs::remove_file(&db);
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "EXPORT_BLOB_INVALID",
                "A retained attachment failed integrity verification",
            );
        }
    }
    let (database_size, database_sha256) = match file_hash(&db) {
        Ok(v) => v,
        Err(_) => {
            let _ = fs::remove_file(&db);
            return super::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "EXPORT_DATABASE_INVALID",
                "Could not verify database snapshot",
            );
        }
    };
    let canonical_db = match canonical_database_hash(&db).await {
        Ok(v) => v,
        Err(_) => {
            let _ = fs::remove_file(&db);
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "EXPORT_DATABASE_INVALID",
                "Could not compute canonical state hash",
            );
        }
    };
    let state_sha256 = hash(
        format!(
            "{}\n{}",
            canonical_db,
            blobs
                .iter()
                .map(|b| format!("{}:{}", b.sha256, b.size))
                .collect::<Vec<_>>()
                .join("\n")
        )
        .as_bytes(),
    );
    let manifest = ExportManifest {
        format_version: ARCHIVE_FORMAT,
        policy_version: COMMAND_TABLE_POLICY_VERSION,
        workspace_id: s.workspace.workspace_id().into(),
        database_sha256,
        database_size,
        blobs,
        state_sha256,
    };
    let bytes = serde_json::to_vec(&manifest).unwrap_or_default();
    let dbclone = db.clone();
    let blob_root = s.workspace.blob_root().to_path_buf();
    let archiveclone = archive.clone();
    let written = tokio::task::spawn_blocking(move || {
        write_export_zip(&archiveclone, &dbclone, &blob_root, &bytes)
    })
    .await;
    let _ = fs::remove_file(&db);
    if !matches!(written, Ok(Ok(()))) {
        let _ = fs::remove_file(&archive);
        return super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "EXPORT_WRITE_FAILED",
            "Could not write a complete export",
        );
    }
    let file = match tokio::fs::File::open(&archive).await {
        Ok(f) => f,
        Err(_) => {
            return super::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "EXPORT_READ_FAILED",
                "Could not open verified export",
            );
        }
    };
    let stream = futures_util::stream::unfold((file, archive), |(mut file, path)| async move {
        let mut b = vec![0u8; 65536];
        match tokio::io::AsyncReadExt::read(&mut file, &mut b).await {
            Ok(0) | Err(_) => {
                drop(file);
                let _ = tokio::fs::remove_file(path).await;
                None
            }
            Ok(n) => {
                b.truncate(n);
                Some((
                    Ok::<_, std::io::Error>(axum::body::Bytes::from(b)),
                    (file, path),
                ))
            }
        }
    });
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/zip"),
    );
    super::private_headers(&mut response, s.workspace.epoch());
    response
}

fn write_export_zip(
    archive: &Path,
    db: &Path,
    blob_root: &Path,
    manifest: &[u8],
) -> Result<(), String> {
    let file = fs::File::create(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(true);
    zip.start_file("workspace.sqlite", opts)
        .map_err(|e| e.to_string())?;
    let mut d = fs::File::open(db).map_err(|e| e.to_string())?;
    std::io::copy(&mut d, &mut zip).map_err(|e| e.to_string())?;
    let m: ExportManifest = serde_json::from_slice(manifest).map_err(|e| e.to_string())?;
    for b in m.blobs {
        zip.start_file(format!("blobs/{}", b.sha256), opts)
            .map_err(|e| e.to_string())?;
        let mut f = fs::File::open(blob_root.join(&b.sha256)).map_err(|e| e.to_string())?;
        std::io::copy(&mut f, &mut zip).map_err(|e| e.to_string())?;
    }
    zip.start_file("manifest.json", opts)
        .map_err(|e| e.to_string())?;
    use std::io::Write;
    zip.write_all(manifest).map_err(|e| e.to_string())?;
    let f = zip.finish().map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    Ok(())
}
pub(crate) async fn preview_import(
    State(s): State<AppState>,
    h: HeaderMap,
    request: Request,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    let empty:i64=sqlx::query_scalar("SELECT (SELECT COUNT(*) FROM entity)+(SELECT COUNT(*) FROM opportunity)+(SELECT COUNT(*) FROM event)").fetch_one(s.workspace.pool()).await.unwrap_or(1);
    if empty != 0 {
        return super::error(
            StatusCode::CONFLICT,
            "IMPORT_TARGET_NOT_EMPTY",
            "Full import requires an empty workspace",
        );
    }
    let id = new_id("import");
    let base = s
        .workspace
        .generation_root()
        .control_path()
        .join(format!(".{id}.staging"));
    if fs::create_dir(&base).is_err() {
        return super::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "IMPORT_STAGE_FAILED",
            "Could not prepare import staging",
        );
    };
    if secure(&base).is_err() {
        let _ = fs::remove_dir_all(&base);
        return super::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "IMPORT_STAGE_FAILED",
            "Could not secure import staging",
        );
    }
    let archive = base.join("upload.zip");
    let mut out = match fs::File::create(&archive) {
        Ok(f) => f,
        Err(_) => {
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "IMPORT_STAGE_FAILED",
                "Could not stage archive",
            );
        }
    };
    let mut stream = request.into_body().into_data_stream();
    let mut total = 0u64;
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            let _ = fs::remove_dir_all(&base);
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "IMPORT_STREAM_FAILED",
                "Archive upload was interrupted",
            );
        };
        total = total.saturating_add(chunk.len() as u64);
        if total > 16 * 1024 * 1024 * 1024 {
            let _ = fs::remove_dir_all(&base);
            return super::error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "IMPORT_TOO_LARGE",
                "Archive exceeds the 16 GiB upload limit",
            );
        };
        if std::io::Write::write_all(&mut out, &chunk).is_err() {
            let _ = fs::remove_dir_all(&base);
            return super::error(
                StatusCode::INSUFFICIENT_STORAGE,
                "IMPORT_STAGE_FAILED",
                "Could not write archive staging",
            );
        }
    }
    if std::io::Write::flush(&mut out).is_err() || out.sync_all().is_err() {
        let _ = fs::remove_dir_all(&base);
        return super::error(
            StatusCode::INSUFFICIENT_STORAGE,
            "IMPORT_STAGE_FAILED",
            "Could not sync archive staging",
        );
    }
    let extract = base.join("verified");
    let _ = fs::create_dir(&extract);
    if secure(&extract).is_err() {
        let _ = fs::remove_dir_all(&base);
        return super::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "IMPORT_STAGE_FAILED",
            "Could not secure extracted staging",
        );
    }
    let manifest = match extract_verified_export(&archive, &extract).await {
        Ok(m) => m,
        Err(_) => {
            let _ = fs::remove_dir_all(&base);
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "IMPORT_INVALID",
                "Archive manifest, paths, database, or attachment hashes failed validation",
            );
        }
    };
    let cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
        .fetch_one(s.workspace.pool())
        .await
        .unwrap_or(-1);
    let digest = hash(
        format!(
            "{}\0{}\0{}",
            manifest.state_sha256,
            s.workspace.workspace_id(),
            cursor
        )
        .as_bytes(),
    );
    let pid = new_id("import-preview");
    let now = now_utc();
    let payload = json!({"directory":base.to_string_lossy(),"manifest":manifest});
    if sqlx::query("INSERT INTO maintenance_preview(id,kind,subject_id,digest,domain_cursor,payload_json,created_at,expires_at) VALUES(?,'import',?,?,?, ?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now','+15 minutes'))").bind(&pid).bind(Option::<String>::None).bind(&digest).bind(cursor).bind(payload.to_string()).bind(now.clone()).execute(s.workspace.pool()).await.is_err(){let _=fs::remove_dir_all(base);return super::error(StatusCode::INTERNAL_SERVER_ERROR,"IMPORT_PREVIEW_FAILED","Could not save import preview")}
    private_json(
        &s,
        json!({"id":pid,"digest":digest,"backup":{"id":"full-import","created_at":now,"status":"verified","manifest_sha256":manifest.state_sha256,"workspace_id":manifest.workspace_id,"format_version":manifest.format_version},"current_cursor":cursor.to_string(),"erasure_resurrections":0,"required_bytes":total}),
    )
}
pub(crate) async fn commit_import(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(i): Json<ImportCommitInput>,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    };
    if !i.target_must_be_empty {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "IMPORT_CONFIRMATION_REQUIRED",
            "target_must_be_empty must be true",
        );
    }
    let _lock = s.workspace.maintenance_lock.lock().await;
    let preview:Option<(String,i64,String)>=sqlx::query_as("SELECT digest,domain_cursor,payload_json FROM maintenance_preview WHERE id=? AND kind='import' AND consumed_at IS NULL AND expires_at>strftime('%Y-%m-%dT%H:%M:%fZ','now')").bind(&i.preview_id).fetch_optional(s.workspace.pool()).await.ok().flatten();
    let Some((d, cursor, payload)) = preview else {
        return super::error(
            StatusCode::CONFLICT,
            "IMPORT_PREVIEW_EXPIRED",
            "Import preview unavailable",
        );
    };
    if d != i.preview_digest {
        return super::error(
            StatusCode::CONFLICT,
            "IMPORT_PREVIEW_MISMATCH",
            "Import digest differs from preview",
        );
    }
    let counts:i64=sqlx::query_scalar("SELECT (SELECT COUNT(*) FROM entity)+(SELECT COUNT(*) FROM opportunity)+(SELECT COUNT(*) FROM event)").fetch_one(s.workspace.pool()).await.unwrap_or(1);
    let current: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
        .fetch_one(s.workspace.pool())
        .await
        .unwrap_or(-1);
    if counts != 0 || current != cursor {
        return super::error(
            StatusCode::CONFLICT,
            "IMPORT_TARGET_CHANGED",
            "Target is no longer empty or revision changed",
        );
    }
    let value: serde_json::Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(_) => {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "IMPORT_PREVIEW_INVALID",
                "Preview payload invalid",
            );
        }
    };
    let dir = PathBuf::from(value["directory"].as_str().unwrap_or(""));
    let m: ExportManifest = match serde_json::from_value(value["manifest"].clone()) {
        Ok(v) => v,
        Err(_) => {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "IMPORT_PREVIEW_INVALID",
                "Manifest invalid",
            );
        }
    };
    let generation_id = new_id("generation");
    let target = match s
        .workspace
        .generation_root()
        .prepare_generation(&generation_id)
    {
        Ok(v) => v,
        Err(_) => {
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "IMPORT_STAGE_FAILED",
                "Could not allocate generation",
            );
        }
    };
    if fs::copy(
        dir.join("verified/workspace.sqlite"),
        target.join("workspace.sqlite"),
    )
    .is_err()
    {
        let _ = fs::remove_dir_all(target);
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "IMPORT_DATABASE_MISSING",
            "Verified database missing",
        );
    };
    let blobs = target.join("blobs");
    if fs::create_dir(&blobs).is_err() {
        let _ = fs::remove_dir_all(target);
        return super::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "IMPORT_STAGE_FAILED",
            "Could not create imported blob directory",
        );
    };
    for b in &m.blobs {
        if fs::copy(
            dir.join("verified/blobs").join(&b.sha256),
            blobs.join(&b.sha256),
        )
        .is_err()
        {
            let _ = fs::remove_dir_all(target);
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "IMPORT_BLOB_MISSING",
                "Verified blob missing",
            );
        }
    }
    let new_epoch = new_id("epoch");
    let operation_id = new_id("import-op");
    if let Err(e) = set_database_epoch(&target.join("workspace.sqlite"), &new_epoch).await {
        let _ = fs::remove_dir_all(target);
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "IMPORT_DATABASE_INVALID",
            &e,
        );
    }
    if let Err(e) = register_pending_operation(
        &target.join("workspace.sqlite"),
        &operation_id,
        "import",
        &new_epoch,
        0,
    )
    .await
    {
        let _ = fs::remove_dir_all(target);
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "IMPORT_DATABASE_INVALID",
            &e,
        );
    }
    let mut tx = match s.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(v) => v,
        Err(_) => {
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "IMPORT_COMMIT_FAILED",
                "Could not reserve target",
            );
        }
    };
    let now: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
        .fetch_one(&mut *tx)
        .await
        .unwrap_or(-1);
    if now != cursor {
        let _ = fs::remove_dir_all(target);
        return super::error(
            StatusCode::CONFLICT,
            "IMPORT_TARGET_CHANGED",
            "Target changed during commit",
        );
    };
    if s.workspace
        .generation_root()
        .publish(&generation_id)
        .is_err()
    {
        let _ = fs::remove_dir_all(target);
        return super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "IMPORT_PUBLISH_FAILED",
            "Could not atomically publish imported workspace",
        );
    };
    s.workspace.rotate_command_secret();
    s.workspace.bump_generation().await;
    let _ = sqlx::query("UPDATE maintenance_preview SET consumed_at=? WHERE id=?")
        .bind(now_utc())
        .bind(&i.preview_id)
        .execute(&mut *tx)
        .await;
    let _ = tx.commit().await;
    let restart = s.restart.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        restart.send_replace(true);
    });
    let mut response = private_json(
        &s,
        json!(MaintenanceOperation {
            id: operation_id,
            kind: "import".into(),
            state: "pending_restart".into(),
            epoch: new_epoch,
            external_copies_requiring_disposal: 0
        }),
    );
    *response.status_mut() = StatusCode::ACCEPTED;
    response
}

async fn extract_verified_export(archive: &Path, output: &Path) -> Result<ExportManifest, String> {
    let file = fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let entry_count = zip.len();
    if entry_count > 100_002 {
        return Err("too many entries".into());
    }
    let mut names = BTreeSet::new();
    let mut manifest = None;
    let mut expanded = 0u64;
    for n in 0..entry_count {
        let mut f = zip.by_index(n).map_err(|e| e.to_string())?;
        let name = f.name().map_err(|e| e.to_string())?.into_owned();
        if f.is_dir()
            || name.starts_with('/')
            || name.contains("..")
            || name.contains('\\')
            || !names.insert(name.clone())
        {
            return Err("unsafe path".into());
        }
        if let Some(mode) = f.unix_mode()
            && mode & 0o170000 == 0o120000
        {
            return Err("symlink".into());
        }
        if name == "manifest.json" {
            if n + 1 != entry_count || f.size() > 16 * 1024 * 1024 {
                return Err("invalid manifest extent".into());
            }
            let mut b = Vec::new();
            f.read_to_end(&mut b).map_err(|e| e.to_string())?;
            manifest =
                Some(serde_json::from_slice::<ExportManifest>(&b).map_err(|e| e.to_string())?);
            continue;
        }
        let dest = if name == "workspace.sqlite" {
            output.join(name)
        } else if let Some(sha) = name.strip_prefix("blobs/") {
            if !valid_blob_digest(sha) || f.size() > 64 * 1024 * 1024 {
                return Err("invalid blob".into());
            }
            let d = output.join("blobs");
            fs::create_dir_all(&d).map_err(|e| e.to_string())?;
            d.join(sha)
        } else {
            return Err("unknown entry".into());
        };
        expanded = expanded.saturating_add(f.size());
        if expanded > 16 * 1024 * 1024 * 1024 {
            return Err("archive expansion cap".into());
        };
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)
            .map_err(|e| e.to_string())?;
        std::io::copy(&mut f, &mut out).map_err(|e| e.to_string())?;
        out.sync_all().map_err(|e| e.to_string())?;
    }
    let m = manifest.ok_or("manifest missing")?;
    if m.format_version != ARCHIVE_FORMAT || m.policy_version != COMMAND_TABLE_POLICY_VERSION {
        return Err("unsupported archive policy".into());
    }
    if file_hash(&output.join("workspace.sqlite"))? != (m.database_size, m.database_sha256.clone())
    {
        return Err("database mismatch".into());
    }
    let expected_names: BTreeSet<String> = m
        .blobs
        .iter()
        .map(|blob| format!("blobs/{}", blob.sha256))
        .chain(["workspace.sqlite".into(), "manifest.json".into()])
        .collect();
    if names != expected_names || names.len() != m.blobs.len() + 2 {
        return Err("archive membership mismatch".into());
    }
    if !verify_blob_members(&output.join("blobs"), &m.blobs) {
        return Err("blob mismatch".into());
    }
    let database = output.join("workspace.sqlite");
    verify_portable_database(&database)
        .await
        .map_err(|e| e.to_string())?;
    if archive_blob_records(&database).await? != m.blobs {
        return Err("database attachment manifest mismatch".into());
    }
    let canonical_db = canonical_database_hash(&output.join("workspace.sqlite")).await?;
    let expected = hash(
        format!(
            "{}\n{}",
            canonical_db,
            m.blobs
                .iter()
                .map(|b| format!("{}:{}", b.sha256, b.size))
                .collect::<Vec<_>>()
                .join("\n")
        )
        .as_bytes(),
    );
    if expected != m.state_sha256 {
        return Err("state hash mismatch".into());
    }
    Ok(m)
}
async fn set_database_epoch(path: &Path, epoch: &str) -> Result<(), String> {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .foreign_keys(true)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Full);
    let pool = sqlx::SqlitePool::connect_with(opts)
        .await
        .map_err(|e| e.to_string())?;
    verify_portable_database(path)
        .await
        .map_err(|e| e.to_string())?;
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("UPDATE workspace_meta SET epoch=? WHERE singleton=1")
        .bind(epoch)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    pool.close().await;
    Ok(())
}

async fn register_pending_operation(
    path: &Path,
    id: &str,
    kind: &str,
    epoch: &str,
    external: u64,
) -> Result<(), String> {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("INSERT INTO maintenance_operation(id,kind,status,subject_id,epoch,result_json,created_at) VALUES(?,?,'pending_restart',NULL,?,?,?)").bind(id).bind(kind).bind(epoch).bind(json!({"external_copies_requiring_disposal":external}).to_string()).bind(now_utc()).execute(&pool).await.map_err(|e|e.to_string())?;
    pool.close().await;
    Ok(())
}

async fn blob_digests(path: &Path) -> Result<Vec<String>, String> {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .map_err(|e| e.to_string())?;
    let refs = sqlx::query_scalar("SELECT sha256 FROM blob ORDER BY sha256")
        .fetch_all(&pool)
        .await
        .map_err(|e| e.to_string())?;
    pool.close().await;
    Ok(refs)
}

async fn canonical_database_hash(path: &Path) -> Result<String, String> {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .map_err(|e| e.to_string())?;
    let tables:Vec<String>=sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(&pool).await.map_err(|e|e.to_string())?;
    let mut h = Sha256::new();
    for table in tables {
        if table == "idempotency_receipt" {
            continue;
        }
        let rawcols: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?) ORDER BY cid")
                .bind(&table)
                .fetch_all(&pool)
                .await
                .map_err(|e| e.to_string())?;
        let cols: Vec<String> = rawcols
            .into_iter()
            .filter(|c| !(table == "workspace_meta" && c == "epoch"))
            .collect();
        let expressions = cols
            .iter()
            .map(|c| format!("quote(\"{}\")", c.replace('"', "\"\"")))
            .collect::<Vec<_>>();
        let order = (1..=expressions.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let table_sql = table.replace('"', "\"\"");
        let query = format!(
            "SELECT {} FROM \"{}\" ORDER BY {}",
            expressions.join(","),
            table_sql,
            if order.is_empty() { "rowid" } else { &order }
        );
        let rows = sqlx::query(AssertSqlSafe(query))
            .fetch_all(&pool)
            .await
            .map_err(|e| format!("canonical query for {table}: {e}"))?;
        h.update(table.as_bytes());
        h.update([0]);
        for row in rows {
            for i in 0..cols.len() {
                let value: Option<String> =
                    sqlx::Row::try_get(&row, i).map_err(|e| e.to_string())?;
                h.update(value.unwrap_or_else(|| "NULL".into()).as_bytes());
                h.update([0]);
            }
            h.update([0xff]);
        }
    }
    pool.close().await;
    Ok(hex::encode(h.finalize()))
}

#[allow(clippy::too_many_arguments)]
async fn erase_from_database(
    path: &Path,
    subject: &str,
    name: &str,
    workspace_id: &str,
    source_generation: &str,
    epoch: &str,
    operation_id: &str,
    external_copies: u64,
) -> Result<(), String> {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .foreign_keys(false)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Full);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .map_err(|e| e.to_string())?;
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("INSERT INTO erasure_authorization(entity_id,subject_name) VALUES(?,?)")
        .bind(subject)
        .bind(name)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("INSERT OR REPLACE INTO erasure_tombstone(entity_id,erased_at,source_generation) VALUES(?,?,?)").bind(subject).bind(now_utc()).bind(source_generation).execute(&mut *tx).await.map_err(|e|e.to_string())?;
    // The fresh generation should omit files owned only by the erased party.
    // Retain an independent upload when it shares the same digest.
    sqlx::query("DELETE FROM blob_upload WHERE card_private=1 AND id IN (SELECT upload_id FROM entity_attachment WHERE entity_id=?) AND id NOT IN (SELECT blob_upload_id FROM material_version WHERE blob_upload_id IS NOT NULL)").bind(subject).execute(&mut *tx).await.map_err(|e|e.to_string())?;
    sqlx::query("DELETE FROM blob WHERE sha256 NOT IN (SELECT blob_sha256 FROM blob_upload UNION SELECT blob_sha256 FROM material_version WHERE blob_sha256 IS NOT NULL UNION SELECT blob_sha256 FROM blob_snapshot_pin UNION SELECT blob_sha256 FROM blob_upload_lease)").execute(&mut *tx).await.map_err(|e|e.to_string())?;
    // Probe observations and readiness are scoped to the old generation.
    sqlx::raw_sql(
        "DELETE FROM inference_probe;
         DELETE FROM inference_qualification;
         DELETE FROM inference_active_profile;",
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    // Stages belong to the replaced epoch, including mapping/preview receipts.
    sqlx::query("DELETE FROM party_import_stage")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM party_stage_receipt")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM edge_evidence WHERE edge_id IN (SELECT id FROM edge WHERE from_id=? OR to_id=?) OR claim_id IN (SELECT id FROM claim WHERE subject_id=?)").bind(subject).bind(subject).bind(subject).execute(&mut *tx).await.map_err(|e|e.to_string())?;
    sqlx::query(
        "DELETE FROM claim_revision WHERE claim_id IN (SELECT id FROM claim WHERE subject_id=?)",
    )
    .bind(subject)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    let tables:Vec<String>=sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(&mut *tx).await.map_err(|e|e.to_string())?;
    for table in tables {
        if matches!(
            table.as_str(),
            "erasure_authorization"
                | "erasure_tombstone"
                | "maintenance_settings"
                | "workspace_meta"
                | "owner_profile"
                | "board_list"
        ) {
            continue;
        }
        let cols: Vec<(String, String)> =
            sqlx::query_as("SELECT name,type FROM pragma_table_info(?)")
                .bind(&table)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        let text_cols: Vec<String> = cols
            .into_iter()
            .filter(|(_, ty)| ty.to_ascii_uppercase().contains("TEXT") || ty.is_empty())
            .map(|(n, _)| n)
            .collect();
        if text_cols.is_empty() {
            continue;
        }
        let predicate = text_cols
            .iter()
            .map(|c| {
                format!(
                    "(instr(CAST(\"{}\" AS TEXT),?)>0 OR instr(CAST(\"{}\" AS TEXT),?)>0)",
                    c.replace('"', "\"\""),
                    c.replace('"', "\"\"")
                )
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        let table_sql = table.replace('"', "\"\"");
        if table == "event" {
            // ERS-2 permits rewriting only party-scoped event payloads. A
            // substring match over every TEXT column would rewrite unrelated
            // events that merely mention the subject's name; scope the scrub
            // to the erased subject's own events (entity_id = subject).
            let sql = format!(
                "UPDATE \"{table_sql}\" SET entity_id='erased',payload_json='{{\"erased\":true}}' WHERE {predicate} AND entity_id=?"
            );
            let mut q = sqlx::query(AssertSqlSafe(sql));
            for _ in &text_cols {
                q = q.bind(subject).bind(name);
            }
            q = q.bind(subject);
            q.execute(&mut *tx)
                .await
                .map_err(|e| format!("scrub event stream: {e}"))?;
        } else {
            let sql = format!("DELETE FROM \"{table_sql}\" WHERE {predicate}");
            let mut q = sqlx::query(AssertSqlSafe(sql));
            for _ in &text_cols {
                q = q.bind(subject).bind(name);
            }
            q.execute(&mut *tx)
                .await
                .map_err(|e| format!("erase rows from {table}: {e}"))?;
        }
    }
    sqlx::query("DELETE FROM source_content WHERE id IN (SELECT content_id FROM source_variant WHERE id NOT IN (SELECT source_variant_id FROM source_occurrence))").execute(&mut *tx).await.map_err(|e|e.to_string())?;
    sqlx::query("DELETE FROM source_variant WHERE id NOT IN (SELECT source_variant_id FROM source_occurrence)").execute(&mut *tx).await.map_err(|e|e.to_string())?;
    sqlx::query("DELETE FROM erasure_authorization WHERE entity_id=?")
        .bind(subject)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'entity','erased',1,'party.erased',?)").bind(new_id(subject)).bind(now_utc()).bind(json!({"workspace_id":workspace_id,"erased":true}).to_string()).execute(&mut *tx).await.map_err(|e|e.to_string())?;
    let now = now_utc();
    sqlx::query("UPDATE workspace_meta SET epoch=? WHERE singleton=1")
        .bind(epoch)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("INSERT INTO maintenance_operation(id,kind,status,subject_id,epoch,result_json,created_at) VALUES(?,'erasure','pending_restart',?,?,?,?)").bind(operation_id).bind(subject).bind(epoch).bind(json!({"external_copies_requiring_disposal":external_copies}).to_string()).bind(now).execute(&mut *tx).await.map_err(|e|e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let bad: Option<String> =
        sqlx::query_scalar("SELECT \"table\" FROM pragma_foreign_key_check LIMIT 1")
            .fetch_optional(&pool)
            .await
            .map_err(|e| e.to_string())?;
    if let Some(t) = bad {
        return Err(format!("foreign key violation after erasure in {t}"));
    }
    sqlx::query("VACUUM")
        .execute(&pool)
        .await
        .map_err(|e| e.to_string())?;
    pool.close().await;
    let final_hash = canonical_database_hash(path).await?;
    let _ = final_hash;
    // A second verified scan ensures no subject identifier or name survives in any text column.
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true);
    let check = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .map_err(|e| e.to_string())?;
    let left:i64=sqlx::query_scalar("SELECT (SELECT COUNT(*) FROM entity WHERE id=? OR name=?) + (SELECT COUNT(*) FROM event WHERE instr(payload_json,?)>0 OR instr(entity_id,?)>0)").bind(subject).bind(name).bind(subject).bind(name).fetch_one(&check).await.map_err(|e|e.to_string())?;
    check.close().await;
    if left != 0 {
        return Err("subject canary remains after database erasure".into());
    }
    Ok(())
}
pub(crate) async fn list_backups(State(s): State<AppState>, h: HeaderMap) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    let settings = match s.workspace.maintenance_settings().await {
        Ok(v) => v,
        Err(_) => {
            return super::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "BACKUP_READ_FAILED",
                "Could not list backups",
            );
        }
    };
    let mut items = Vec::new();
    if let Ok(ds) = fs::read_dir(settings.backup_directory) {
        for e in ds.flatten() {
            let p = e.path();
            if p.is_dir() {
                let m = p.join("manifest.json");
                let valid = fs::read(&m)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Manifest>(&b).ok())
                    .is_some_and(|m| verify(&p, &m));
                items.push(json!({"id":e.file_name().to_string_lossy(),"status":if valid{"verified"}else{"damaged"}}));
            }
        }
    }
    private_json(
        &s,
        json!({"items":items,"last_successful_at":settings.last_successful_at}),
    )
}
pub(crate) async fn preview_restore(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(i): Json<RestorePreviewInput>,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    };
    let settings = match s.workspace.maintenance_settings().await {
        Ok(v) => v,
        Err(_) => {
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "BACKUP_SETTINGS_UNAVAILABLE",
                "Could not read backup settings",
            );
        }
    };
    let dir = PathBuf::from(settings.backup_directory).join(&i.backup_id);
    let manifest: Manifest = match fs::read(dir.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
    {
        Some(v) => v,
        None => {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "BACKUP_INVALID",
                "Backup manifest is invalid",
            );
        }
    };
    if manifest.backup_id != i.backup_id
        || manifest.workspace_id != s.workspace.workspace_id()
        || !verify_backup_snapshot(&dir, &manifest).await
    {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "BACKUP_INVALID",
            "Backup failed workspace or integrity verification",
        );
    }
    let cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
        .fetch_one(s.workspace.pool())
        .await
        .unwrap_or(-1);
    let id = new_id("restore-preview");
    let digest =
        hash(format!("{}\0{}\0{}", manifest.db_sha256, manifest.backup_id, cursor).as_bytes());
    let now = now_utc();
    let payload = json!({"directory":dir.to_string_lossy(),"backup_id":manifest.backup_id});
    if sqlx::query("INSERT INTO maintenance_preview(id,kind,subject_id,digest,domain_cursor,payload_json,created_at,expires_at) VALUES(?,'restore',?,?,?, ?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now','+15 minutes'))").bind(&id).bind(Option::<String>::None).bind(&digest).bind(cursor).bind(payload.to_string()).bind(now.clone()).execute(s.workspace.pool()).await.is_err(){return super::error(StatusCode::INTERNAL_SERVER_ERROR,"RESTORE_PREVIEW_FAILED","Could not persist preview")}
    private_json(
        &s,
        json!(RestorePreview {
            id,
            digest,
            backup: json!({"id":manifest.backup_id,"created_at":manifest.created_at,"status":"verified","manifest_sha256":hash(&serde_json::to_vec(&manifest).unwrap_or_default()),"workspace_id":manifest.workspace_id,"format_version":manifest.format_version}),
            current_cursor: cursor.to_string(),
            erasure_resurrections: 0,
            required_bytes: manifest.db_size + manifest.blobs.iter().map(|b| b.size).sum::<u64>()
        }),
    )
}
pub(crate) async fn commit_restore(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(i): Json<RestoreCommitInput>,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    };
    let _lock = s.workspace.maintenance_lock.lock().await;
    let row:Option<(String,i64,String)>=sqlx::query_as("SELECT digest,domain_cursor,payload_json FROM maintenance_preview WHERE id=? AND kind='restore' AND consumed_at IS NULL AND expires_at>strftime('%Y-%m-%dT%H:%M:%fZ','now')").bind(&i.preview_id).fetch_optional(s.workspace.pool()).await.ok().flatten();
    let Some((digest, cursor, payload)) = row else {
        return super::error(
            StatusCode::CONFLICT,
            "RESTORE_PREVIEW_EXPIRED",
            "Restore preview expired",
        );
    };
    if digest != i.preview_digest {
        return super::error(
            StatusCode::CONFLICT,
            "RESTORE_PREVIEW_MISMATCH",
            "Restore preview digest mismatch",
        );
    };
    let current: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
        .fetch_one(s.workspace.pool())
        .await
        .unwrap_or(-1);
    if current != cursor {
        return super::error(
            StatusCode::CONFLICT,
            "RESTORE_PREVIEW_STALE",
            "Workspace changed after preview",
        );
    }
    let value: serde_json::Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(_) => {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "RESTORE_PREVIEW_INVALID",
                "Preview payload invalid",
            );
        }
    };
    let dir = PathBuf::from(value["directory"].as_str().unwrap_or(""));
    let manifest: Manifest = match fs::read(dir.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
    {
        Some(v) => v,
        None => {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "BACKUP_INVALID",
                "Backup manifest missing",
            );
        }
    };
    if !verify_backup_snapshot(&dir, &manifest).await {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "BACKUP_INVALID",
            "Backup bytes changed after preview",
        );
    };
    if manifest.workspace_id != s.workspace.workspace_id() {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "WORKSPACE_MISMATCH",
            "Backup belongs to another workspace",
        );
    };
    if !i.acknowledge_erased_resurrection {
        let erased: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM erasure_tombstone")
            .fetch_one(s.workspace.pool())
            .await
            .unwrap_or(0);
        if erased > 0 {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "ERASURE_ACK_REQUIRED",
                "Restore must acknowledge historical erasure consequences",
            );
        }
    }
    let generation_id = new_id("generation");
    let target = match s
        .workspace
        .generation_root()
        .prepare_generation(&generation_id)
    {
        Ok(v) => v,
        Err(_) => {
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "RESTORE_STAGE_FAILED",
                "Could not allocate restore generation",
            );
        }
    };
    if fs::copy(
        dir.join("workspace.sqlite"),
        target.join("workspace.sqlite"),
    )
    .is_err()
    {
        let _ = fs::remove_dir_all(target);
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "BACKUP_INVALID",
            "Backup database is missing",
        );
    };
    let blobs = target.join("blobs");
    if fs::create_dir(&blobs).is_err() {
        let _ = fs::remove_dir_all(target);
        return super::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "RESTORE_STAGE_FAILED",
            "Could not allocate blob store",
        );
    };
    for b in &manifest.blobs {
        if fs::copy(dir.join("blobs").join(&b.sha256), blobs.join(&b.sha256)).is_err() {
            let _ = fs::remove_dir_all(target);
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "BACKUP_BLOB_INVALID",
                "Backup attachment missing",
            );
        }
    }
    let epoch = new_id("epoch");
    let operation_id = new_id("restore-op");
    if let Err(e) = set_database_epoch(&target.join("workspace.sqlite"), &epoch).await {
        let _ = fs::remove_dir_all(target);
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "RESTORE_DATABASE_INVALID",
            &e,
        );
    };
    if let Err(e) = register_pending_operation(
        &target.join("workspace.sqlite"),
        &operation_id,
        "restore",
        &epoch,
        0,
    )
    .await
    {
        let _ = fs::remove_dir_all(target);
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "RESTORE_DATABASE_INVALID",
            &e,
        );
    }
    let mut tx = match s.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(v) => v,
        Err(_) => {
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "RESTORE_COMMIT_FAILED",
                "Could not reserve workspace",
            );
        }
    };
    let actual: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
        .fetch_one(&mut *tx)
        .await
        .unwrap_or(-1);
    if actual != cursor {
        return super::error(
            StatusCode::CONFLICT,
            "RESTORE_PREVIEW_STALE",
            "Workspace changed during restore",
        );
    };
    if s.workspace
        .generation_root()
        .publish(&generation_id)
        .is_err()
    {
        return super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "RESTORE_PUBLISH_FAILED",
            "Could not publish restore generation",
        );
    };
    s.workspace.rotate_command_secret();
    s.workspace.bump_generation().await;
    let _ = sqlx::query("UPDATE maintenance_preview SET consumed_at=? WHERE id=?")
        .bind(now_utc())
        .bind(&i.preview_id)
        .execute(&mut *tx)
        .await;
    let _ = tx.commit().await;
    let restart = s.restart.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        restart.send_replace(true);
    });
    let mut response = private_json(
        &s,
        json!(MaintenanceOperation {
            id: operation_id,
            kind: "restore".into(),
            state: "pending_restart".into(),
            epoch,
            external_copies_requiring_disposal: 0
        }),
    );
    *response.status_mut() = StatusCode::ACCEPTED;
    response
}
async fn make_backup(w: &Workspace) -> Result<Manifest, String> {
    let _lock = w.maintenance_lock.lock().await;
    let settings = w.maintenance_settings().await.map_err(|e| e.to_string())?;
    let id = new_id("backup");
    let root = PathBuf::from(settings.backup_directory);
    let staging = root.join(format!(".{id}.staging"));
    fs::create_dir(&staging).map_err(|e| e.to_string())?;
    secure(&staging)?;
    fs::create_dir(staging.join("blobs")).map_err(|e| e.to_string())?;
    let db = staging.join("workspace.sqlite");
    let mut conn = w.pool().acquire().await.map_err(|e| e.to_string())?;
    let d = db.to_string_lossy().to_string();
    sqlx::query("VACUUM INTO ?")
        .bind(d)
        .execute(&mut *conn)
        .await
        .map_err(|e| e.to_string())?;
    drop(conn);
    scrub_archive_database(&db)
        .await
        .map_err(|e| e.to_string())?;
    let entries = archive_blob_records(&db).await?;
    for blob in &entries {
        let src = w.blob_root().join(&blob.sha256);
        let destination = staging.join("blobs").join(&blob.sha256);
        fs::copy(src, &destination).map_err(|e| e.to_string())?;
        FileSync::sync(&destination)?;
    }
    sync_dir(&staging.join("blobs"))?;
    let (db_size, db_sha256) = file_hash(&db)?;
    let m = Manifest {
        format_version: ARCHIVE_FORMAT,
        policy_version: COMMAND_TABLE_POLICY_VERSION,
        backup_id: id.clone(),
        created_at: now_utc(),
        workspace_id: w.workspace_id().into(),
        db_sha256,
        db_size,
        blobs: entries,
    };
    let bytes = serde_json::to_vec(&m).map_err(|e| e.to_string())?;
    fs::write(staging.join("manifest.json"), bytes).map_err(|e| e.to_string())?;
    FileSync::sync(&staging.join("manifest.json"))?;
    FileSync::sync(&db)?;
    if !verify(&staging, &m) {
        return Err("backup member verification failed".into());
    }
    sync_dir(&staging)?;
    fs::rename(&staging, root.join(&id)).map_err(|e| e.to_string())?;
    sync_dir(&root)?;
    let mut tx = w
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query(
        "UPDATE maintenance_settings SET last_successful_at=?,updated_at=? WHERE singleton=1",
    )
    .bind(&m.created_at)
    .bind(&m.created_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    prune_verified_backups(&root, settings.retention_count as usize)?;
    Ok(m)
}

fn prune_verified_backups(root: &Path, retain: usize) -> Result<(), String> {
    let mut verified = Vec::new();
    for e in fs::read_dir(root).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let m = fs::symlink_metadata(e.path()).map_err(|e| e.to_string())?;
        if m.file_type().is_symlink() || !m.is_dir() {
            continue;
        }
        let manifest_path = e.path().join("manifest.json");
        let Ok(bytes) = fs::read(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) else {
            continue;
        };
        if verify(&e.path(), &manifest) {
            verified.push((manifest.created_at, e.path()));
        }
    }
    verified.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in verified.into_iter().skip(retain) {
        fs::remove_dir_all(path).map_err(|e| e.to_string())?;
    }
    sync_dir(root)
}

fn sync_dir(path: &Path) -> Result<(), String> {
    fs::File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
async fn archive_blob_records(db: &Path) -> Result<Vec<BlobRecord>, String> {
    let mut conn = open_archive_database(db, true)
        .await
        .map_err(|error| error.to_string())?;
    let rows: Vec<(String, i64)> = sqlx::query_as("SELECT sha256,size FROM blob ORDER BY sha256")
        .fetch_all(&mut conn)
        .await
        .map_err(|error| error.to_string())?;
    rows.into_iter()
        .map(|(sha256, size)| {
            if !valid_blob_digest(&sha256) || size < 0 {
                return Err("invalid database blob metadata".into());
            }
            Ok(BlobRecord {
                sha256,
                size: size as u64,
            })
        })
        .collect()
}

fn valid_blob_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn verify_blob_members(dir: &Path, blobs: &[BlobRecord]) -> bool {
    if blobs.is_empty() && !dir.exists() {
        return true;
    }
    let Ok(metadata) = fs::symlink_metadata(dir) else {
        return false;
    };
    if !metadata.is_dir() {
        return false;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    let names = entries
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<BTreeSet<_>, _>>();
    let expected: BTreeSet<_> = blobs.iter().map(|blob| blob.sha256.clone()).collect();
    names.is_ok_and(|names| names == expected)
        && expected.len() == blobs.len()
        && blobs.iter().all(|blob| {
            valid_blob_digest(&blob.sha256)
                && file_hash(&dir.join(&blob.sha256)).ok() == Some((blob.size, blob.sha256.clone()))
        })
}

async fn verify_backup_snapshot(path: &Path, manifest: &Manifest) -> bool {
    if !verify(path, manifest) {
        return false;
    }
    let database = path.join("workspace.sqlite");
    verify_portable_database(&database).await.is_ok()
        && archive_blob_records(&database)
            .await
            .is_ok_and(|records| records == manifest.blobs)
}

fn verify(p: &Path, m: &Manifest) -> bool {
    if !fs::symlink_metadata(p).is_ok_and(|metadata| metadata.is_dir())
        || !fs::symlink_metadata(p.join("manifest.json")).is_ok_and(|metadata| metadata.is_file())
    {
        return false;
    }
    let Ok(entries) = fs::read_dir(p) else {
        return false;
    };
    let names = entries
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<BTreeSet<_>, _>>();
    m.format_version == ARCHIVE_FORMAT
        && m.policy_version == COMMAND_TABLE_POLICY_VERSION
        && names.is_ok_and(|names| {
            names
                == [
                    "blobs".into(),
                    "manifest.json".into(),
                    "workspace.sqlite".into(),
                ]
                .into()
        })
        && file_hash(&p.join("workspace.sqlite")).ok() == Some((m.db_size, m.db_sha256.clone()))
        && verify_blob_members(&p.join("blobs"), &m.blobs)
}

fn hash(b: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(b))
}
fn file_hash(p: &Path) -> Result<(u64, String), String> {
    let metadata = fs::symlink_metadata(p).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("archive member is not a regular file".into());
    }
    let mut file = fs::File::open(p).map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    let size = std::io::copy(&mut file, &mut digest).map_err(|error| error.to_string())?;
    Ok((size, hex::encode(digest.finalize())))
}
pub(crate) struct FileSync;
impl FileSync {
    fn sync(p: &Path) -> Result<(), String> {
        std::fs::File::open(p)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    }
}
#[cfg(unix)]
fn secure(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}
#[cfg(not(unix))]
fn secure(_: &Path) -> Result<(), String> {
    Ok(())
}

fn private_json(state: &AppState, value: serde_json::Value) -> Response {
    let mut response = Json(value).into_response();
    super::private_headers(&mut response, state.workspace.epoch());
    response
}

#[cfg(test)]
mod archive_tests {
    use super::*;
    #[tokio::test]
    async fn s11_archive_canonical_state_roundtrip_preserves_database_and_blob_manifest() {
        let root = std::env::temp_dir().join(format!("parley-archive-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let workspace = Workspace::open(root.join("workspace.sqlite"))
            .await
            .unwrap();
        let snapshot = root.join("snapshot.sqlite");
        sqlx::query("VACUUM INTO ?")
            .bind(snapshot.to_string_lossy().to_string())
            .execute(workspace.pool())
            .await
            .unwrap();
        scrub_archive_database(&snapshot).await.unwrap();
        let dbhash = canonical_database_hash(&snapshot).await.unwrap();
        let (size, digest) = file_hash(&snapshot).unwrap();
        let manifest = ExportManifest {
            format_version: ARCHIVE_FORMAT,
            policy_version: COMMAND_TABLE_POLICY_VERSION,
            workspace_id: workspace.workspace_id().to_owned(),
            database_sha256: digest,
            database_size: size,
            blobs: vec![],
            state_sha256: hash(format!("{}\n", dbhash).as_bytes()),
        };
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let archive = root.join("export.zip");
        write_export_zip(&archive, &snapshot, workspace.blob_root(), &bytes).unwrap();
        let extracted = root.join("extract");
        fs::create_dir(&extracted).unwrap();
        let parsed = extract_verified_export(&archive, &extracted).await.unwrap();
        assert_eq!(parsed.state_sha256, manifest.state_sha256);
        assert_eq!(
            canonical_database_hash(&extracted.join("workspace.sqlite"))
                .await
                .unwrap(),
            dbhash
        );
        drop(workspace);
        let _ = fs::remove_dir_all(root);
    }
}
