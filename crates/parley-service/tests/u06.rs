//! UCR-001 U06: canonical resources — `entity` is the sole resource record.
//!
//! These tests drive the real service: migration of a pre-0025 workspace
//! (legacy `resource` rows + mirrors) into the canonical entity model, the
//! card/content revision split, material dependency binding, the shared
//! resource commands and the delegated summary read. The fake provider is a
//! real loopback endpoint; authorization, the registry pipeline and the
//! commit boundary are the service's own.
mod support;

use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use sqlx::{Connection, SqlSafeStr, SqliteConnection};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

type OwnerSession = (String, String, String);
static NEXT: AtomicU64 = AtomicU64::new(0);
static SESSIONS: OnceLock<Mutex<HashMap<u16, OwnerSession>>> = OnceLock::new();

fn db_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-u06-{tag}-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn raw(
    port: u16,
    method: &str,
    path: &str,
    body: &str,
    key: Option<&str>,
    extra: &[(&str, &str)],
) -> (u16, HashMap<String, String>, Value) {
    let (cookie, token, epoch) = SESSIONS
        .get()
        .unwrap()
        .lock()
        .unwrap()
        .get(&port)
        .unwrap()
        .clone();
    let key_header = key
        .map(|value| format!("Idempotency-Key: {value}\r\n"))
        .unwrap_or_default();
    let extra_header = extra
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\n{key_header}{extra_header}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, payload) = response.split_once("\r\n\r\n").unwrap();
    let status = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers: HashMap<String, String> = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let value: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
    (status, headers, value)
}

async fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let (status, _, value) = raw(port, method, path, &payload, key, &[]).await;
    (status, value)
}

async fn owner_session(service: &RunningService) -> OwnerSession {
    let address = service.address;
    let body = json!({"secret": service.bootstrap_secret}).to_string();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!("POST /api/v1/session/bootstrap HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let cookie = response
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|line| {
            line.split_once(':')
                .map(|(_, value)| value.trim().split(';').next().unwrap().to_owned())
        })
        .unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!("GET /api/v1/session HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nCookie: {cookie}\r\nContent-Length: 0\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let payload: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    (
        cookie,
        payload["mutation_token"].as_str().unwrap().into(),
        payload["epoch"].as_str().unwrap().into(),
    )
}

async fn setup(tag: &str) -> (PathBuf, Workspace, u16, RunningService) {
    // Debug-only authority-test fixture: keep the pre-cut transport for
    // these U01-U08 authority/fencing cases. AR05 proves the real worker.
    unsafe {
        std::env::set_var("PARLEY_TEST_NATIVE_RUN_FIXTURE", "1");
    }
    let path = db_path(tag);
    let _ = fs::remove_file(&path);
    let service = parley_service::start_workspace(&path).await.unwrap();
    let port = service.address.port();
    let owner = owner_session(&service).await;
    SESSIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(port, owner);
    let workspace = Workspace::open(&path).await.unwrap();
    (path, workspace, port, service)
}

/// Start the service over a pre-seeded database (legacy schema) so the
/// embedded migration 0025 runs against it; unlike `setup` it never deletes
/// the prepared file.
async fn setup_seeded(path: &PathBuf) -> (Workspace, u16, RunningService) {
    let service = parley_service::start_workspace(path).await.unwrap();
    let port = service.address.port();
    let owner = owner_session(&service).await;
    SESSIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(port, owner);
    let workspace = Workspace::open(path).await.unwrap();
    (workspace, port, service)
}

fn cleanup(path: &PathBuf, workspace: Workspace, service: RunningService) {
    drop(workspace);
    drop(service);
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(path.with_extension("sqlite-wal"));
    let _ = fs::remove_dir_all(path.with_extension("sqlite-shm"));
}

/// Apply migrations 0001..0024 only, leaving the workspace on the legacy
/// resource schema (mirror triggers + `resource` table) so migration 0025
/// runs when the service opens it.
async fn prepare_pre_0025(path: &Path) {
    use sqlx::migrate::{Migration, MigrationType, Migrator};
    let mut migrations: Vec<Migration> = Vec::new();
    let dir = fs::read_dir("../../migrations").unwrap();
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".sql") {
            continue;
        }
        let version: i64 = name[..4].parse().unwrap();
        if version >= 25 {
            continue;
        }
        let sql: &'static str =
            Box::leak(fs::read_to_string(entry.path()).unwrap().into_boxed_str());
        let no_tx = sql.contains("-- no-transaction");
        let description: &'static str =
            Box::leak(name[5..name.len() - 4].replace('_', " ").into_boxed_str());
        migrations.push(Migration::new(
            version,
            description.into(),
            MigrationType::ReversibleUp,
            sql.into_sql_str(),
            no_tx,
        ));
    }
    migrations.sort();
    let migrator = Migrator::with_migrations(migrations);
    let mut conn = SqliteConnection::connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .unwrap();
    migrator.run(&mut conn).await.unwrap();
}

/// Seed a realistic pre-0025 workspace: legacy resources with mirrors, a
/// placed card, a material version bound to one resource and a delivery.
async fn seed_legacy_resources(path: &Path) {
    let mut conn = SqliteConnection::connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .unwrap();
    let now = "2026-11-01T00:00:00Z";
    sqlx::query(
        "INSERT INTO resource(id,owner_id,kind,title,body_json,readiness,revision,created_at,updated_at) VALUES \
         ('res_gift1',1,'gift','Gift A','{\"what\":\"A\",\"format\":\"template\",\"delivery\":\"on_request\",\"time_to_produce_minutes\":30,\"cost_to_me\":\"low\"}','ready',3,?,?),
         ('res_proof1',1,'proof','Proof B','{}','draft',1,?,?)",
    )
    .bind(now)
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(&mut conn)
    .await
    .unwrap();
    // The legacy mirror trigger (pre-0025) creates the entity rows for the
    // resources above; only the entity-only card needs an explicit row.
    sqlx::query(
        "INSERT INTO entity(id,kind,name,revision,created_at,updated_at,focus,notes_md,notes_rev,subtype,body_json,readiness) VALUES \
         ('res_entity1','resource','Entity only',2,?,?,'','',1,'gift','{}','draft')",
    )
    .bind(now)
    .bind(now)
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO resource_placement(resource_id,column_id,position,revision) VALUES('res_gift1','available',1024,1)",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    // Parent rows the material version references (the legacy seed predates
    // the service's own creation flow, so it mints them directly).
    sqlx::query(
        "INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES('party_seed','person','Dana',1,?,?);",
    )
    .bind(now)
    .bind(now)
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query("INSERT INTO party_policy(party_id) VALUES('party_seed');")
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES('opp_seed','Seed', 'open',1,?,?);",
    )
    .bind(now)
    .bind(now)
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES('opp_seed','party_seed','primary',?);",
    )
    .bind(now)
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query("INSERT INTO template_family(id,name,created_at) VALUES('fam_seed','Seed',?);")
        .bind(now)
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO template_version(id,family_id,version_no,body,fallbacks_json,status,content_sha256,created_at) VALUES('tmpl_seed','fam_seed',1,'body','[]','active','t',?);",
    )
    .bind(now)
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO material(id,opportunity_id,recipient_id,kind,head_version,created_at,updated_at) VALUES('mat1','opp_seed','party_seed','outreach',1,?,?);",
    )
    .bind(now)
    .bind(now)
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at) \
         VALUES('mat1',1,'opp_seed','party_seed','tmpl_seed',1,'body','[{\"slot\":\"gift:g\",\"value\":\"A\",\"resource_id\":\"res_gift1\"}]','[]','h',?)",
    )
    .bind(now)
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO resource_use(material_id,version_no,resource_id,slot_name) VALUES('mat1',1,'res_gift1','gift:g')",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO version_dependency(material_id,version_no,kind,source_id,source_revision) VALUES \
         ('mat1',1,'resource','res_gift1','3'),('mat1',1,'opportunity','opp_seed','1'),('mat1',1,'party','party_seed','1'),('mat1',1,'template','tmpl_seed','t'),('mat1',1,'exchange','opp_seed:party_seed:1','1')",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO delivered_resource_version(resource_id,material_id,version_no,resource_revision,resource_title) VALUES('res_gift1','mat1',1,3,'Gift A')",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
        .execute(&mut conn)
        .await
        .unwrap();
}

async fn command(port: u16, id: &str, args: Value, key: Option<&str>) -> (u16, Value) {
    request(
        port,
        "POST",
        &format!("/api/v1/commands/{id}"),
        Some(json!({"version": 1, "args": args, "dry_run": false})),
        key,
    )
    .await
}

#[tokio::test]
async fn u06_migration_preserves_ids_content_revisions_and_deliveries() {
    let path = db_path("migration");
    let _ = fs::remove_file(&path);
    prepare_pre_0025(&path).await;
    seed_legacy_resources(&path).await;
    let (workspace, port, service) = setup_seeded(&path).await;
    let _ = workspace;

    // The legacy table is gone and the content revisions were backfilled
    // from the legacy revisions.
    let legacy_table: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='resource'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(legacy_table, 0);
    let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT id,subtype,revision,resource_content_revision FROM entity WHERE subtype<>'' ORDER BY id",
    )
    .fetch_all(workspace.pool())
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("res_entity1".into(), "gift".into(), 2, 2),
            ("res_gift1".into(), "gift".into(), 3, 3),
            ("res_proof1".into(), "proof".into(), 1, 1),
        ]
    );
    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(workspace.pool())
        .await
        .unwrap();
    assert!(violations.is_empty(), "foreign_key_check: {violations:?}");

    // The board lists the migrated card with its content revision; the
    // delivery record keeps the frozen title and revision.
    let (status, board) = request(
        port,
        "GET",
        "/api/v1/resource-board?include_offboard=true",
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "{board:?}");
    let cards = board["cards"].as_array().unwrap();
    let card = cards
        .iter()
        .find(|row| row["resource_id"].as_str() == Some("res_gift1"))
        .expect("migrated card on the board");
    assert_eq!(card["content_revision"], 3);
    let delivered: (i64, String) = sqlx::query_as(
        "SELECT resource_revision,resource_title FROM delivered_resource_version WHERE resource_id='res_gift1' AND material_id='mat1'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(delivered, (3, "Gift A".into()));

    // The directory still lists legacy-backed resources by their IDs; the
    // entity-only card stays off the material-kind directory.
    let (status, list) = request(port, "GET", "/api/v1/entities?kind=resource", None, None).await;
    assert_eq!(status, 200, "{list:?}");
    let ids: Vec<&str> = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"res_gift1"));
    assert!(ids.contains(&"res_proof1"));
    assert!(ids.contains(&"res_entity1"));

    // The material dependency kept the legacy revision binding.
    let dep: String = sqlx::query_scalar(
        "SELECT source_revision FROM version_dependency WHERE kind='resource' AND source_id='res_gift1' AND material_id='mat1'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(dep, "3");

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u06_preflight_rejects_divergent_mirrors() {
    let path = db_path("divergent");
    let _ = fs::remove_file(&path);
    prepare_pre_0025(&path).await;
    seed_legacy_resources(&path).await;
    let mut conn = SqliteConnection::connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .unwrap();
    sqlx::query("UPDATE entity SET name='Divergent' WHERE id='res_gift1'")
        .execute(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let result = parley_service::start_workspace(&path).await;
    assert!(result.is_err(), "divergent mirror must abort migration");
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn u06_notes_and_placement_keep_content_proof_intact() {
    let (path, workspace, port, service) = setup("nop").await;
    let _ = workspace;

    let (status, created) = request(
        port,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"gift","title":"Checklist","body":{"what":"Checklist","format":"template","delivery":"on_request","time_to_produce_minutes":20,"cost_to_me":"low"}})),
        Some("u06-create-1"),
    )
    .await;
    assert_eq!(status, 201, "{created:?}");
    let id = created["data"]["id"].as_str().unwrap().to_owned();
    let etag = created["data"]["etag"].as_str().unwrap().to_owned();
    let (status, _) = request(
        port,
        "POST",
        &format!("/api/v1/resources/{id}/ready"),
        Some(json!({})),
        None,
    )
    .await;
    assert_eq!(status, 204);

    async fn content_revision(port: u16, id: &str) -> i64 {
        let (status, board) = request(
            port,
            "GET",
            "/api/v1/resource-board?include_offboard=true",
            None,
            None,
        )
        .await;
        assert_eq!(status, 200);
        let all: Vec<&Value> = board["cards"]
            .as_array()
            .unwrap()
            .iter()
            .chain(board["off_board"].as_array().unwrap().iter())
            .collect();
        let row = all
            .iter()
            .find(|row| row["resource_id"].as_str() == Some(id))
            .expect("resource visible on the board or off-board");
        row["content_revision"].as_i64().unwrap()
    }

    // Notes edits and board moves never touch the content revision.
    let before = content_revision(port, &id).await;
    let (status, notes) = request(
        port,
        "GET",
        &format!("/api/v1/entities/{id}/notes"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    let notes_etag = notes["data"]["etag"].as_str().unwrap();
    let (status, _, _) = raw(
        port,
        "PUT",
        &format!("/api/v1/entities/{id}/notes"),
        &json!({"markdown":"notes","expected_notes_rev":1}).to_string(),
        Some("u06-notes-1"),
        &[("If-Match", notes_etag)],
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = request(
        port,
        "POST",
        "/api/v1/resource-board/moves",
        Some(json!({"resource_id":id,"destination_column_id":"available","expected_placement_revision":0})),
        Some("u06-move-1"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(content_revision(port, &id).await, before);

    // The typed content edit bumps the content revision.
    let (status, live) = request(port, "GET", &format!("/api/v1/entities/{id}"), None, None).await;
    assert_eq!(status, 200);
    let live_etag = live["data"]["etag"].as_str().unwrap();
    let (status, _, updated) = raw(
        port,
        "PATCH",
        &format!("/api/v1/entities/{id}"),
        &json!({"title":"Checklist v2","body":{"what":"Checklist v2","format":"template","delivery":"on_request","time_to_produce_minutes":20,"cost_to_me":"low"}}).to_string(),
        Some("u06-edit-1"),
        &[("If-Match", live_etag)],
    )
    .await;
    assert_eq!(status, 200, "{updated:?}");
    assert_eq!(content_revision(port, &id).await, before + 1);
    assert!(!etag.is_empty());

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u06_retirement_bumps_content_revision_and_flags_dependents() {
    let path = db_path("retire");
    let _ = fs::remove_file(&path);
    prepare_pre_0025(&path).await;
    seed_legacy_resources(&path).await;
    let (workspace, port, service) = setup_seeded(&path).await;

    let (status, retired) = request(
        port,
        "POST",
        "/api/v1/resource-cards/res_gift1/retire",
        None,
        Some("u06-retire-1"),
    )
    .await;
    assert_eq!(status, 200, "{retired:?}");
    let content: i64 =
        sqlx::query_scalar("SELECT resource_content_revision FROM entity WHERE id='res_gift1'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(content, 4);
    let recheck: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event WHERE event_type='material.release_recheck_required' AND entity_id='mat1'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(recheck, 1);
    let (status, list) = request(port, "GET", "/api/v1/entities?kind=resource", None, None).await;
    assert_eq!(status, 200);
    assert!(
        !list["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == "res_gift1")
    );

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u06_erase_refused_while_materials_reference_the_entity() {
    let path = db_path("erase");
    let _ = fs::remove_file(&path);
    prepare_pre_0025(&path).await;
    seed_legacy_resources(&path).await;
    let (_workspace, port, service) = setup_seeded(&path).await;

    let (status, erased) = request(
        port,
        "POST",
        "/api/v1/resource-cards/res_gift1/erase",
        None,
        Some("u06-erase-1"),
    )
    .await;
    assert_eq!(status, 409, "{erased:?}");
    assert_eq!(erased["error"]["code"], "RESOURCE_DELIVERED");

    let (status, erased) = request(
        port,
        "POST",
        "/api/v1/resource-cards/res_proof1/erase",
        None,
        Some("u06-erase-2"),
    )
    .await;
    assert_eq!(status, 200, "{erased:?}");
    let erased_body = erased.get("data").cloned().unwrap_or(erased.clone());
    assert_eq!(erased_body["erased"], true);

    cleanup(&path, _workspace, service);
}

#[tokio::test]
async fn u06_commands_create_draft_mark_ready_and_replay() {
    let (path, workspace, port, service) = setup("commands").await;
    let _ = workspace;

    let (status, manifest) = request(
        port,
        "POST",
        "/api/v1/commands/query",
        Some(
            json!({"surface":"palette","context":{"route":"resources","selection.resource":true}}),
        ),
        Some("u06-query-1"),
    )
    .await;
    assert_eq!(status, 200, "{manifest:?}");
    let ids: Vec<&str> = manifest["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"resource.create_draft"));
    assert!(ids.contains(&"resource.mark_ready"));

    let (status, created) = command(
        port,
        "resource.create_draft",
        json!({"title":"Palette draft","subtype":"gift"}),
        Some("u06-create-cmd"),
    )
    .await;
    assert_eq!(status, 200, "{created:?}");
    assert_eq!(created["ok"], true);
    assert_eq!(created["result"]["readiness"], "draft");
    let id = created["result"]["id"].as_str().unwrap().to_owned();

    let (status, replay) = command(
        port,
        "resource.create_draft",
        json!({"title":"Palette draft","subtype":"gift"}),
        Some("u06-create-cmd"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"]["id"], id);

    let (status, conflict) = command(
        port,
        "resource.create_draft",
        json!({"title":"Different draft","subtype":"proof"}),
        Some("u06-create-cmd"),
    )
    .await;
    assert_eq!(status, 422);
    assert_eq!(conflict["error"]["code"], "idempotency_mismatch");

    let (status, bad) = command(
        port,
        "resource.create_draft",
        json!({"title":"Draft","subtype":"robot"}),
        Some("u06-create-bad"),
    )
    .await;
    assert_eq!(status, 422);
    assert_eq!(bad["error"]["code"], "invalid_args");

    // Mark ready without required persisted fields reports bounded gaps and
    // produces zero effect.
    let (status, denied) = command(
        port,
        "resource.mark_ready",
        json!({"resource_id": id, "expected_content_revision": 1}),
        Some("u06-ready-1"),
    )
    .await;
    assert_eq!(status, 422, "{denied:?}");
    assert_eq!(denied["error"]["code"], "invalid_args");
    assert!(!denied["error"]["details"].as_array().unwrap().is_empty());
    let readiness: String = sqlx::query_scalar("SELECT readiness FROM entity WHERE id=?")
        .bind(&id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(readiness, "draft");

    // Complete the typed fields through the shared REST editor, then mark
    // ready with the recorded content revision.
    let (status, card) = request(port, "GET", &format!("/api/v1/entities/{id}"), None, None).await;
    assert_eq!(status, 200);
    let etag = card["data"]["etag"].as_str().unwrap();
    let (status, _, _) = raw(
        port,
        "PATCH",
        &format!("/api/v1/entities/{id}"),
        &json!({"title":"Palette draft","body":{"what":"Checklist","format":"template","delivery":"on_request","time_to_produce_minutes":20,"cost_to_me":"low"}}).to_string(),
        Some("u06-edit-cmd"),
        &[("If-Match", etag)],
    )
    .await;
    assert_eq!(status, 200);

    let (status, ready) = command(
        port,
        "resource.mark_ready",
        json!({"resource_id": id, "expected_content_revision": 2}),
        Some("u06-ready-2"),
    )
    .await;
    assert_eq!(status, 200, "{ready:?}");
    assert_eq!(ready["result"]["readiness"], "ready");
    assert_eq!(ready["result"]["content_revision"], 3);

    let (status, replay) = command(
        port,
        "resource.mark_ready",
        json!({"resource_id": id, "expected_content_revision": 2}),
        Some("u06-ready-2"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(replay["replayed"], true);
    let (status, stale) = command(
        port,
        "resource.mark_ready",
        json!({"resource_id": id, "expected_content_revision": 1}),
        Some("u06-ready-3"),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(stale["error"]["code"], "version_mismatch");

    // The agent-only summary is concealed from owner palette invocation.
    let (status, summary) = command(
        port,
        "resource.get_summary",
        json!({"resource_id": id}),
        Some("u06-summary-1"),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(summary["error"]["code"], "unknown_command");

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u06_agent_read_grant_scopes_resource_summary() {
    let (path, workspace, port, service) = setup("agent").await;
    let _ = workspace;

    let provider = support::ScriptedProvider::start(vec![
        json!({"models":[{"name":"fixture","digest":"sha256:u06"}]}),
        json!({
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id":"call","function":{"name":"resource__get_summary","arguments":json!({"resource_id":"res_gift1"}).to_string()}}]
            },
            "prompt_eval_count": 12, "eval_count": 8
        }),
        json!({"message": {"role": "assistant", "content": "done", "tool_calls": []}, "prompt_eval_count": 12, "eval_count": 8}),
    ])
    .await;
    let (status, reply) = request(
        port,
        "PUT",
        "/api/v1/agent/provider",
        Some(json!({"endpoint": provider.url(), "model": "fixture", "num_ctx": 8192, "num_predict": 256, "locale": "en"})),
        Some("u06-provider-1"),
    )
    .await;
    assert!(status == 200 || status == 204, "{status} {reply:?}");
    let (status, reply) = request(
        port,
        "POST",
        "/api/v1/agent/provider/health",
        None,
        Some("u06-provider-2"),
    )
    .await;
    assert_eq!(status, 200, "{reply:?}");
    assert_eq!(reply["available"], true);

    let (status, run) = request(
        port,
        "POST",
        "/api/v1/agent/runs",
        Some(json!({
            "task": "Summarise the gift.",
            "targets": [{"kind": "resource", "id": "res_gift1"}],
            "permissions": ["resource.read"],
            "allow_created_opportunities": false
        })),
        Some("u06-run-1"),
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let run_id = run["run_id"].as_str().unwrap().to_owned();

    for _ in 0..100 {
        let (status, current) = request(
            port,
            "GET",
            &format!("/api/v1/agent/runs/{run_id}"),
            None,
            None,
        )
        .await;
        if status != 200 {
            panic!("run status {status} current={current}");
        }
        let Some(state) = current["state"].as_str() else {
            panic!("run missing state: {current}");
        };
        if state == "succeeded" || state == "failed" || state == "cancelled" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (status, current) = request(
        port,
        "GET",
        &format!("/api/v1/agent/runs/{run_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    if current["state"] != "succeeded" {
        let (_s, transcript) = request(
            port,
            "GET",
            &format!("/api/v1/agent/runs/{run_id}/transcript"),
            None,
            None,
        )
        .await;
        panic!(
            "run state={} current={current} transcript={transcript}",
            current["state"]
        );
    }

    // The run transcript contains the tool result but never raw private
    // fields; the summary result carries only safe projections.
    let (status, transcript) = request(
        port,
        "GET",
        &format!("/api/v1/agent/runs/{run_id}/transcript"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    // The run persisted with its grant and terminal state; the transcript
    // never carries private record fields.
    let (run_epoch, run_state): (String, String) =
        sqlx::query_as("SELECT epoch,state FROM agent_run WHERE run_id=?")
            .bind(&run_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(run_state, "succeeded");
    assert!(!run_epoch.is_empty());
    let target_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent_run_target WHERE run_id=? AND kind='resource' AND target_id='res_gift1'",
    )
    .bind(&run_id)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(target_count, 1);
    let text = serde_json::to_string(&transcript).unwrap();
    assert!(!text.contains("notes"), "summary must not expose notes");

    cleanup(&path, workspace, service);
}
