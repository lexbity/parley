use parley_service::{MaintenanceSettingsInput, Workspace, WorkspaceError};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

async fn auth(address: std::net::SocketAddr, bootstrap: &str) -> (String, String, String) {
    let mut s = TcpStream::connect(address).await.unwrap();
    let b = json!({"secret":bootstrap}).to_string();
    s.write_all(format!("POST /api/v1/session/bootstrap HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}",b.len()).as_bytes()).await.unwrap();
    let mut r = String::new();
    s.read_to_string(&mut r).await.unwrap();
    let cookie = r
        .lines()
        .find(|x| x.to_ascii_lowercase().starts_with("set-cookie:"))
        .unwrap()
        .split_once(':')
        .unwrap()
        .1
        .trim()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let mut s = TcpStream::connect(address).await.unwrap();
    s.write_all(format!("GET /api/v1/session HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nCookie: {cookie}\r\n\r\n").as_bytes()).await.unwrap();
    let mut r = String::new();
    s.read_to_string(&mut r).await.unwrap();
    let v: Value = serde_json::from_str(r.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    (
        cookie,
        v["mutation_token"].as_str().unwrap().into(),
        v["epoch"].as_str().unwrap().into(),
    )
}
async fn post(
    address: std::net::SocketAddr,
    a: &(String, String, String),
    path: &str,
    body: Value,
    key: &str,
) -> (u16, Value) {
    let mut s = TcpStream::connect(address).await.unwrap();
    let b = body.to_string();
    s.write_all(format!("POST {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nCookie: {}\r\nX-Parley-Token: {}\r\nX-Parley-Epoch: {}\r\nIdempotency-Key: {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}",a.0,a.1,a.2,b.len()).as_bytes()).await.unwrap();
    let mut r = String::new();
    s.read_to_string(&mut r).await.unwrap();
    let (head, payload) = r.split_once("\r\n\r\n").unwrap();
    let status = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (
        status,
        serde_json::from_str(payload).unwrap_or_else(|e| panic!("{e}: {r}")),
    )
}
async fn get(address: std::net::SocketAddr, a: &(String, String, String), path: &str) -> Value {
    let mut s = TcpStream::connect(address).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nCookie: {}\r\nX-Parley-Token: {}\r\nX-Parley-Epoch: {}\r\n\r\n",a.0,a.1,a.2).as_bytes()).await.unwrap();
    let mut r = String::new();
    s.read_to_string(&mut r).await.unwrap();
    serde_json::from_str(r.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

async fn request_bytes(
    address: std::net::SocketAddr,
    a: &(String, String, String),
    method: &str,
    path: &str,
    body: &[u8],
    content_type: &str,
) -> (u16, Vec<u8>) {
    let mut s = TcpStream::connect(address).await.unwrap();
    s.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nCookie: {}\r\nX-Parley-Token: {}\r\nX-Parley-Epoch: {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",a.0,a.1,a.2,body.len()).as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
    let mut response = Vec::new();
    s.read_to_end(&mut response).await.unwrap();
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&response[..split]);
    let status: u16 = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let raw = &response[split + 4..];
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut decoded = Vec::new();
        let mut offset = 0;
        loop {
            let end = raw[offset..].windows(2).position(|w| w == b"\r\n").unwrap() + offset;
            let n =
                usize::from_str_radix(std::str::from_utf8(&raw[offset..end]).unwrap().trim(), 16)
                    .unwrap();
            offset = end + 2;
            if n == 0 {
                break;
            }
            decoded.extend_from_slice(&raw[offset..offset + n]);
            offset += n + 2;
        }
        (status, decoded)
    } else {
        (status, raw.to_vec())
    }
}

#[cfg(unix)]
#[tokio::test]
async fn s11_open_preserves_existing_parent_permissions() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let parent = std::env::temp_dir().join(format!("parley-s11-parent-{}", std::process::id()));
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let before = std::fs::metadata(&parent).unwrap().mode() & 0o777;
    let w = Workspace::open(parent.join("workspace.sqlite"))
        .await
        .unwrap();
    assert_eq!(std::fs::metadata(&parent).unwrap().mode() & 0o777, before);
    drop(w);
    let _ = std::fs::remove_dir_all(parent);
}

#[tokio::test]
async fn s11_draft00_fixture_converts_party_with_stable_source_key_mapping() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let root = std::env::temp_dir().join(format!("parley-s11-draft00-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let identity = root.join("legacy.sqlite");
    let opts = SqliteConnectOptions::new()
        .filename(&identity)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    for statement in include_str!("fixtures/draft00.sql")
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }
    pool.close().await;
    let workspace = Workspace::open(&identity).await.unwrap();
    let mapped: String = sqlx::query_scalar("SELECT destination_id FROM legacy_key_map WHERE source_format='draft-00' AND source_table='party' AND source_key='42'")
        .fetch_one(workspace.pool()).await.unwrap();
    let entity: (String, String) = sqlx::query_as("SELECT id,name FROM entity WHERE id=?")
        .bind(&mapped)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(entity.0, mapped);
    assert_eq!(entity.1, "Dana Lee");
    let policy: (i64, i64, i64) = sqlx::query_as(
        "SELECT do_not_contact,highest_rung,touch_count FROM party_policy WHERE party_id=?",
    )
    .bind(&mapped)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(policy, (1, 2, 1));
    let preserved: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM draft00_capture WHERE id=7 AND party_id=42")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(preserved, 1);
    let diagnostic: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM legacy_conversion_diagnostic WHERE source_format='draft-00' AND source_table='capture' AND code='RETAINED_UNMAPPED'")
        .fetch_one(workspace.pool()).await.unwrap();
    assert_eq!(diagnostic, 1);
    drop(workspace);
    let reopened = Workspace::open(&identity).await.unwrap();
    let map_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM legacy_key_map WHERE source_format='draft-00' AND source_table='party' AND source_key='42'")
        .fetch_one(reopened.pool()).await.unwrap();
    assert_eq!(map_count, 1);
    drop(reopened);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn s11_settings_compare_expected_revision_inside_write() {
    let root = std::env::temp_dir().join(format!("parley-s11-settings-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let w = Workspace::open(root.join("workspace.sqlite"))
        .await
        .unwrap();
    let current = w.maintenance_settings().await.unwrap();
    let input = MaintenanceSettingsInput {
        backup_directory: root.join("backups").to_string_lossy().into_owned(),
        interval_hours: 48,
        retention_count: 8,
        expected_revision: current.revision,
    };
    let saved = w.update_maintenance_settings(input).await.unwrap();
    assert_eq!(saved.revision, current.revision + 1);
    let stale = MaintenanceSettingsInput {
        backup_directory: root.join("other").to_string_lossy().into_owned(),
        interval_hours: 72,
        retention_count: 4,
        expected_revision: current.revision,
    };
    assert!(matches!(
        w.update_maintenance_settings(stale).await,
        Err(WorkspaceError::StaleRevision)
    ));
    drop(w);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn s11_backup_contains_verified_database_and_manifest_last() {
    let root = std::env::temp_dir().join(format!("parley-s11-backup-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let w = Workspace::open(root.join("workspace.sqlite"))
        .await
        .unwrap();
    let settings = w.maintenance_settings().await.unwrap();
    let response = parley_service::create_backup_for_test(&w).await.unwrap();
    let target = std::path::PathBuf::from(settings.backup_directory).join(&response);
    assert!(target.join("workspace.sqlite").is_file());
    assert!(target.join("manifest.json").is_file());
    assert!(target.join("blobs").is_dir());
    drop(w);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn s11_startup_backup_scheduler_catches_up_and_retention_keeps_verified_last_good() {
    let root = std::env::temp_dir().join(format!("parley-s11-scheduler-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let identity = root.join("workspace.sqlite");
    let w = Workspace::open(&identity).await.unwrap();
    // An unrun maintenance schedule is overdue at startup.
    let settings = w.maintenance_settings().await.unwrap();
    assert!(settings.last_successful_at.is_none());
    drop(w);
    let service = parley_service::start_workspace(&identity).await.unwrap();
    let a = auth(service.address, &service.bootstrap_secret).await;
    let mut verified = Vec::new();
    for _ in 0..50 {
        let response = get(service.address, &a, "/api/v1/backups").await;
        let items = response["items"].as_array().unwrap();
        verified = items
            .iter()
            .filter(|item| item["status"] == "verified")
            .cloned()
            .collect();
        if !verified.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        !verified.is_empty(),
        "startup did not catch up the due backup"
    );
    service.shutdown().await;

    // Retention counts verified snapshots only; a damaged newer directory cannot
    // cause the previous verified snapshot to be removed.
    let backups = root.join(".workspace.sqlite.backups");
    let mut snapshots = std::fs::read_dir(&backups)
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("manifest.json").is_file())
        .collect::<Vec<_>>();
    snapshots.sort_by_key(|e| e.file_name());
    assert!(!snapshots.is_empty());
    let good = snapshots[0].path();
    let preserved_manifest = std::fs::read(good.join("manifest.json")).unwrap();
    let damaged = backups.join("damaged-newer");
    std::fs::create_dir(&damaged).unwrap();
    std::fs::write(damaged.join("manifest.json"), b"not a manifest").unwrap();
    parley_service::prune_backups_for_test(&backups, 1).unwrap();
    assert!(good.join("manifest.json").is_file());
    assert_eq!(
        std::fs::read(good.join("manifest.json")).unwrap(),
        preserved_manifest
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn s11_erasure_removes_subject_canaries_before_operation_is_complete() {
    let root = std::env::temp_dir().join(format!("parley-s11-erase-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let identity = root.join("workspace.sqlite");
    let w = Workspace::open(&identity).await.unwrap();
    let subject = w
        .create_entity("create-subject", "person", "Unique Erasure Canary", "")
        .await
        .unwrap();
    let old_db = w.database_path().to_path_buf();
    drop(w);
    let mut service = parley_service::start_workspace(&identity).await.unwrap();
    let a = auth(service.address, &service.bootstrap_secret).await;
    let (status, preview) = post(
        service.address,
        &a,
        &format!("/api/v1/parties/{}/erase/preview", subject.id),
        json!({}),
        "erase-preview",
    )
    .await;
    assert_eq!(status, 200, "{preview}");
    let (status, operation) = post(
        service.address,
        &a,
        &format!("/api/v1/parties/{}/erase", subject.id),
        json!({"preview_id":preview["id"],"preview_digest":preview["digest"]}),
        "erase-commit",
    )
    .await;
    assert_eq!(status, 202, "{operation}");
    assert_eq!(operation["state"], "pending_restart");
    service.wait_for_restart().await;
    service.shutdown().await;
    let w = Workspace::open(&identity).await.unwrap();
    assert!(!old_db.exists());
    let remaining: Option<String> = sqlx::query_scalar("SELECT id FROM entity WHERE id=?")
        .bind(&subject.id)
        .fetch_optional(w.pool())
        .await
        .unwrap();
    assert!(remaining.is_none());
    drop(w);
    let service = parley_service::start_workspace(&identity).await.unwrap();
    let fresh = auth(service.address, &service.bootstrap_secret).await;
    let completed = get(
        service.address,
        &fresh,
        &format!(
            "/api/v1/maintenance/operations/{}",
            operation["id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(completed["state"], "complete");
    service.shutdown().await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn s11_erasure_does_not_scrub_unrelated_events() {
    let root = std::env::temp_dir().join(format!("parley-s11-erase-scope-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let identity = root.join("workspace.sqlite");
    let w = Workspace::open(&identity).await.unwrap();
    let subject = w
        .create_entity("erase-scope-subject", "person", "Unique Erasure Canary", "")
        .await
        .unwrap();
    let other = w
        .create_entity("erase-scope-other", "person", "Some Other Party", "")
        .await
        .unwrap();
    let unrelated_payload =
        json!({"title":"met Unique Erasure Canary at a meetup","party":other.id}).to_string();
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES('evt-unrelated',?,'entity',?,1,'entity.created',?)")
        .bind("2026-11-20T12:00:00.000Z")
        .bind(&other.id)
        .bind(&unrelated_payload)
        .execute(w.pool())
        .await
        .unwrap();
    let other_events_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event WHERE entity_id=?")
            .bind(&other.id)
            .fetch_one(w.pool())
            .await
            .unwrap();
    drop(w);
    let mut service = parley_service::start_workspace(&identity).await.unwrap();
    let a = auth(service.address, &service.bootstrap_secret).await;
    let (status, preview) = post(
        service.address,
        &a,
        &format!("/api/v1/parties/{}/erase/preview", subject.id),
        json!({}),
        "erase-scope-preview",
    )
    .await;
    assert_eq!(status, 200, "{preview}");
    let (status, operation) = post(
        service.address,
        &a,
        &format!("/api/v1/parties/{}/erase", subject.id),
        json!({"preview_id":preview["id"],"preview_digest":preview["digest"]}),
        "erase-scope-commit",
    )
    .await;
    assert_eq!(status, 202, "{operation}");
    service.wait_for_restart().await;
    service.shutdown().await;
    let w = Workspace::open(&identity).await.unwrap();
    let payload: String =
        sqlx::query_scalar("SELECT payload_json FROM event WHERE event_id='evt-unrelated'")
            .fetch_one(w.pool())
            .await
            .unwrap();
    assert_eq!(
        payload, unrelated_payload,
        "unrelated events must remain byte-identical"
    );
    // Positive control: the erased subject's own events are still scrubbed to
    // the tombstone payload (ERS-2 party-scoped rewrite).
    let subject_events_left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event WHERE entity_id=?")
            .bind(&subject.id)
            .fetch_one(w.pool())
            .await
            .unwrap();
    assert_eq!(
        subject_events_left, 0,
        "the subject's own events must be scrubbed"
    );
    let erased_tombstones: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event WHERE entity_id='erased' AND payload_json='{\"erased\":true}'",
    )
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert!(erased_tombstones >= 1);
    let other_events_left: i64 = sqlx::query_scalar("SELECT count(*) FROM event WHERE entity_id=?")
        .bind(&other.id)
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(
        other_events_left, other_events_before,
        "the unrelated party's events must survive"
    );
    let subject_gone: Option<String> = sqlx::query_scalar("SELECT id FROM entity WHERE id=?")
        .bind(&subject.id)
        .fetch_optional(w.pool())
        .await
        .unwrap();
    assert!(subject_gone.is_none());
    drop(w);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn s11_full_export_import_api_roundtrip_preserves_canonical_workspace_state() {
    let base = std::env::temp_dir().join(format!("parley-s11-roundtrip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let source = base.join("source.sqlite");
    let target = base.join("target.sqlite");
    let w = Workspace::open(&source).await.unwrap();
    let entity = w
        .create_entity("exported-entity", "person", "Round Trip Contact", "")
        .await
        .unwrap();
    let source_workspace_id = w.workspace_id().to_owned();
    let _ = parley_service::create_backup_for_test(&w).await.unwrap();
    drop(w);
    let service = parley_service::start_workspace(&source).await.unwrap();
    let a = auth(service.address, &service.bootstrap_secret).await;
    let (status, archive) = request_bytes(
        service.address,
        &a,
        "POST",
        "/api/v1/exports",
        b"",
        "application/json",
    )
    .await;
    assert_eq!(status, 200, "export returned {} bytes", archive.len());
    assert!(archive.starts_with(b"PK"));
    service.shutdown().await;
    let target_workspace = Workspace::open(&target).await.unwrap();
    drop(target_workspace);
    let mut service = parley_service::start_workspace(&target).await.unwrap();
    let b = auth(service.address, &service.bootstrap_secret).await;
    let (status, preview_bytes) = request_bytes(
        service.address,
        &b,
        "POST",
        "/api/v1/imports/preview",
        &archive,
        "application/zip",
    )
    .await;
    let preview: Value = serde_json::from_slice(&preview_bytes)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&preview_bytes)));
    assert_eq!(status, 200, "{preview}");
    let(status,result)=post(service.address,&b,"/api/v1/imports/commit",json!({"preview_id":preview["id"],"preview_digest":preview["digest"],"target_must_be_empty":true}),"import-commit").await;
    assert_eq!(status, 202, "{result}");
    assert_eq!(result["state"], "pending_restart");
    service.wait_for_restart().await;
    service.shutdown().await;
    let imported = Workspace::open(&target).await.unwrap();
    assert_eq!(imported.workspace_id(), source_workspace_id);
    let rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM entity WHERE id=? AND name='Round Trip Contact'")
            .bind(&entity.id)
            .fetch_one(imported.pool())
            .await
            .unwrap();
    assert_eq!(rows, 1);
    drop(imported);
    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test]
async fn s11_restore_publishes_generation_and_revokes_old_epoch() {
    let root = std::env::temp_dir().join(format!("parley-s11-restore-api-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let identity = root.join("workspace.sqlite");
    let w = Workspace::open(&identity).await.unwrap();
    let first = w
        .create_entity("first", "person", "Before Backup", "")
        .await
        .unwrap();
    let backup_id = parley_service::create_backup_for_test(&w).await.unwrap();
    let old_db = w.database_path().to_path_buf();
    let old_epoch = w.epoch().to_owned();
    let _second = w
        .create_entity("second", "person", "After Backup", "")
        .await
        .unwrap();
    drop(w);
    let mut service = parley_service::start_workspace(&identity).await.unwrap();
    let a = auth(service.address, &service.bootstrap_secret).await;
    let (status, preview) = post(
        service.address,
        &a,
        "/api/v1/restore/preview",
        json!({"backup_id":backup_id}),
        "restore-preview",
    )
    .await;
    assert_eq!(status, 200, "{preview}");
    let(status,result)=post(service.address,&a,"/api/v1/restore/commit",json!({"preview_id":preview["id"],"preview_digest":preview["digest"],"acknowledge_erased_resurrection":false}),"restore-commit").await;
    assert_eq!(status, 202, "{result}");
    assert_eq!(result["state"], "pending_restart");
    assert_ne!(result["epoch"], old_epoch);
    service.wait_for_restart().await;
    service.shutdown().await;
    let restored = Workspace::open(&identity).await.unwrap();
    assert!(!old_db.exists());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entity")
        .fetch_one(restored.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    let id: Option<String> = sqlx::query_scalar("SELECT id FROM entity WHERE id=?")
        .bind(first.id)
        .fetch_optional(restored.pool())
        .await
        .unwrap();
    assert!(id.is_some());
    assert_eq!(restored.epoch(), result["epoch"].as_str().unwrap());
    let op_state: String =
        sqlx::query_scalar("SELECT status FROM maintenance_operation WHERE id=?")
            .bind(result["id"].as_str().unwrap())
            .fetch_one(restored.pool())
            .await
            .unwrap();
    assert_eq!(op_state, "complete");
    drop(restored);
    let _ = std::fs::remove_dir_all(root);
}
