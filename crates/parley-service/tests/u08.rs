//! UCR-001 U08: destructive registry retirement with human approval.
//!
//! `resource.retire` is the first destructive command. Every execution for
//! both actors requires an exact human approval; the read-only planner binds
//! the impact into a fingerprint, execution recomputes it under reservation,
//! and the consumption + effect share one savepoint. The owner REST
//! retirement route keeps its direct entry but shares the application
//! operation. The scripts below prove the approval lifecycle, replays,
//! staleness, dry runs, expiry, budget/impact bounds, decision races,
//! crash rollback, owner inbox + notification stream, and the delegated
//! agent wait/resume path.
mod support;

use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

type OwnerSession = (String, String, String);
static NEXT: AtomicU64 = AtomicU64::new(0);
static SESSIONS: OnceLock<Mutex<HashMap<u16, OwnerSession>>> = OnceLock::new();

fn db_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-u08-{tag}-{}-{}.sqlite",
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
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\n{key_header}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
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
    let value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(payload).unwrap_or(Value::Null)
    };
    (status, headers, value.get("data").cloned().unwrap_or(value))
}

async fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: &str,
) -> (u16, Value) {
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let (status, _, value) = raw(port, method, path, &body, Some(key)).await;
    (status, value)
}

async fn owner_session(service: &RunningService) -> OwnerSession {
    let address = service.address;
    let body = json!({"secret":service.bootstrap_secret}).to_string();
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

fn cleanup(path: &PathBuf, workspace: Workspace, service: RunningService) {
    drop(workspace);
    drop(service);
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(path.with_extension("sqlite-wal"));
    let _ = fs::remove_dir_all(path.with_extension("sqlite-shm"));
}

async fn count(workspace: &Workspace, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(workspace.pool())
        .await
        .unwrap()
}

fn now_str() -> String {
    "2026-11-23T00:00:00.000Z".to_owned()
}

#[derive(Clone)]
struct SeedIds {
    resource: String,
    #[allow(dead_code)]
    opportunity: String,
    #[allow(dead_code)]
    recipient: String,
}

/// Seed a retirement scenario: a ready gift resource placed on the inbox
/// column, one material with two versions depending on it, and the minimal
/// opportunity/party/template rows the material foreign keys require.
async fn seed_retirement_scenario(workspace: &Workspace) -> SeedIds {
    let now = now_str();
    sqlx::query("INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES('recipient-1','person','Dana',2,?,?)")
        .bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES('primary-1','org','Primary Org',1,?,?)")
        .bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO party_policy(party_id) VALUES('recipient-1'),('primary-1')")
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES('opportunity-1','Follow up','open',2,?,?)")
        .bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES('opportunity-1','primary-1','primary',?),('opportunity-1','recipient-1','participant',?)")
        .bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO template_family(id,name,created_at) VALUES('family-1','Family',?)")
        .bind(&now)
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO template_version(id,family_id,version_no,body,fallbacks_json,status,content_sha256,created_at) VALUES('template-1','family-1',1,'x','{}','active','h',?)")
        .bind(&now).execute(workspace.pool()).await.unwrap();
    // The gift resource: card revision 3, content revision 5, ready.
    sqlx::query(
        "INSERT INTO entity(id,kind,name,focus,notes_md,notes_rev,revision,created_at,updated_at,subtype,body_json,readiness,resource_content_revision,subtype_locked) \
         VALUES('res-gift1','resource','Gift A','', '',1,3,?,?,'gift','{\"what\":\"a book\",\"tags\":[]}','ready',5,1)",
    )
    .bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO resource_placement(resource_id,column_id,position,revision) VALUES('res-gift1','inbox',1024,1)")
        .execute(workspace.pool()).await.unwrap();
    sqlx::query(
        "INSERT INTO material(id,opportunity_id,recipient_id,kind,head_version,created_at,updated_at) \
         VALUES('mat-1','opportunity-1','recipient-1','outreach',2,?,?)",
    )
    .bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    for (version, content_hash) in [(1i64, "h1".to_string()), (2, "h2".to_string())] {
        sqlx::query(
            "INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at) \
             VALUES('mat-1',?, 'opportunity-1','recipient-1','template-1',1,'body','{}','{}',?,?)",
        )
        .bind(version)
        .bind(content_hash)
        .bind(&now)
        .execute(workspace.pool())
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO version_dependency(material_id,version_no,kind,source_id,source_revision) \
         VALUES('mat-1',1,'resource','res-gift1','4'),('mat-1',2,'resource','res-gift1','5')",
    )
    .execute(workspace.pool())
    .await
    .unwrap();
    SeedIds {
        resource: "res-gift1".into(),
        opportunity: "opportunity-1".into(),
        recipient: "recipient-1".into(),
    }
}

fn retire_args(resource: &str, card_revision: i64, content_revision: i64) -> Value {
    json!({
        "resource_id": resource,
        "expected_card_revision": card_revision,
        "expected_content_revision": content_revision,
    })
}

/// POST the retire command with owner auth. Returns (status, body, location).
async fn retire_cmd(port: u16, key: &str, args: Value) -> (u16, Value, Option<String>) {
    let (status, headers, value) = raw(
        port,
        "POST",
        "/api/v1/commands/resource.retire",
        &json!({"version": 1, "args": args, "dry_run": false}).to_string(),
        Some(key),
    )
    .await;
    let location = headers.get("location").cloned();
    (status, value, location)
}

async fn grant(
    port: u16,
    approval_id: &str,
    revision: i64,
    preview_digest: &str,
    key: &str,
) -> (u16, Value) {
    request(
        port,
        "POST",
        &format!("/api/v1/approvals/{approval_id}/grant"),
        Some(json!({"expected_revision": revision, "preview_digest": preview_digest})),
        key,
    )
    .await
}

async fn deny(
    port: u16,
    approval_id: &str,
    revision: i64,
    preview_digest: &str,
    key: &str,
) -> (u16, Value) {
    request(
        port,
        "POST",
        &format!("/api/v1/approvals/{approval_id}/deny"),
        Some(json!({"expected_revision": revision, "preview_digest": preview_digest})),
        key,
    )
    .await
}

/// Find the pending approval for the retire command and return its
/// (id, revision, preview_digest).
async fn pending_approval(port: u16) -> (String, i64, String) {
    let (status, list) = request(
        port,
        "GET",
        "/api/v1/approvals?status=pending",
        None,
        "approval-list",
    )
    .await;
    assert_eq!(status, 200, "{list:?}");
    let items = list["items"].as_array().unwrap();
    let row = items
        .iter()
        .find(|item| item["command_id"] == "resource.retire")
        .expect("pending retire approval missing");
    (
        row["id"].as_str().unwrap().to_owned(),
        row["revision"].as_i64().unwrap(),
        row["preview_digest"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn u08_retire_requires_approval_and_location() {
    let (path, workspace, port, service) = setup("approval-required").await;
    let seed = seed_retirement_scenario(&workspace).await;
    let args = retire_args(&seed.resource, 3, 5);

    let (status, body, location) = retire_cmd(port, "retire-key-01", args.clone()).await;
    assert_eq!(status, 403, "{body:?}");
    assert_eq!(body["error"]["code"], "approval_required");
    let location = location.expect("approval_required must carry a Location header");
    assert!(location.starts_with("/api/v1/approvals/"), "{location}");

    // Repeated request for the same pending intent returns the same location.
    let (status2, body2, location2) = retire_cmd(port, "retire-key-01", args).await;
    assert_eq!(status2, 403, "{body2:?}");
    assert_eq!(location2.as_deref(), Some(location.as_str()));

    // No effect, no receipt, one approval_required outcome per admitted
    // attempt (both requests were admitted and finalized).
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM event WHERE event_type='resource.retired'"
        )
        .await,
        0
    );
    assert_eq!(count(&workspace, "SELECT count(*) FROM command_receipt WHERE key_digest IS NOT NULL AND command_id='resource.retire'").await, 0);
    assert_eq!(
        count(&workspace, "SELECT count(*) FROM command_audit_event WHERE kind='outcome' AND outcome_code='approval_required'").await,
        2
    );

    // Active detail includes exact normalized args and the preview.
    let approval_id = location.trim_start_matches("/api/v1/approvals/");
    let (status, detail) = request(
        port,
        "GET",
        &format!("/api/v1/approvals/{approval_id}"),
        None,
        "approval-read",
    )
    .await;
    assert_eq!(status, 200, "{detail:?}");
    assert_eq!(detail["command_id"], "resource.retire");
    assert_eq!(detail["args"]["resource_id"], seed.resource);
    assert_eq!(detail["preview"]["title"], "Gift A");
    assert_eq!(detail["preview"]["placement_removed"], true);
    assert_eq!(detail["preview"]["impacted_versions_count"], 2);

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u08_grant_then_same_key_executes_once() {
    let (path, workspace, port, service) = setup("grant-execute").await;
    let seed = seed_retirement_scenario(&workspace).await;
    let args = retire_args(&seed.resource, 3, 5);
    let key = "retire-key-11";

    let (status, _, location) = retire_cmd(port, key, args.clone()).await;
    assert_eq!(status, 403);
    let approval_id = location
        .unwrap()
        .trim_start_matches("/api/v1/approvals/")
        .to_owned();
    let (id, revision, preview_digest) = pending_approval(port).await;
    assert_eq!(approval_id, id);

    let (status, granted) = grant(
        port,
        &approval_id,
        revision,
        &preview_digest,
        "decision-key-11",
    )
    .await;
    assert_eq!(status, 200, "{granted:?}");
    assert_eq!(granted["status"], "granted");
    assert_eq!(granted["revision"], revision + 1);

    // Same-key retry now executes; 32 concurrent same-key callers produce one
    // mutation, one event and exactly one executed receipt.
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(32));
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let barrier = barrier.clone();
        let args = args.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            retire_cmd(port, key, args).await
        }));
    }
    let mut ok = 0;
    for task in tasks {
        let (status, reply, _) = task.await.unwrap();
        assert_eq!(status, 200, "{reply:?}");
        assert_eq!(reply["result"]["readiness"], "retired");
        ok += 1;
    }
    assert_eq!(ok, 32);

    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM event WHERE event_type='resource.retired'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM event WHERE event_type='material.release_recheck_required'"
        )
        .await,
        2
    );
    assert_eq!(count(&workspace, "SELECT count(*) FROM command_receipt WHERE command_id='resource.retire' AND response_status=200").await, 1);
    // Exactly one executed attempt; the rest replayed.
    assert_eq!(
        count(&workspace, "SELECT count(*) FROM command_audit_event WHERE kind='outcome' AND policy_result='allowed'").await,
        1
    );
    assert_eq!(
        count(&workspace, "SELECT count(*) FROM command_audit_event WHERE kind='outcome' AND policy_result='replayed'").await,
        31
    );
    // The approval row is consumed and its private payloads purged.
    let (status, detail) = request(
        port,
        "GET",
        &format!("/api/v1/approvals/{approval_id}"),
        None,
        "approval-read",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(detail["status"], "consumed");
    assert!(detail.get("args").is_none(), "{detail:?}");
    assert!(detail.get("preview").is_none(), "{detail:?}");
    // Revised entity state.
    let row: (i64, i64, String) = sqlx::query_as(
        "SELECT revision,resource_content_revision,readiness FROM entity WHERE id=?",
    )
    .bind(&seed.resource)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(row, (4, 6, "retired".to_string()));
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM resource_placement WHERE resource_id='res-gift1'"
        )
        .await,
        0
    );

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u08_denied_intent_fails_closed() {
    let (path, workspace, port, service) = setup("deny").await;
    let seed = seed_retirement_scenario(&workspace).await;
    let args = retire_args(&seed.resource, 3, 5);
    let key = "retire-key-21";

    let (status, _, location) = retire_cmd(port, key, args.clone()).await;
    assert_eq!(status, 403);
    let approval_id = location
        .unwrap()
        .trim_start_matches("/api/v1/approvals/")
        .to_owned();
    let (id, revision, preview_digest) = pending_approval(port).await;
    assert_eq!(approval_id, id);

    let (status, denied) = deny(
        port,
        &approval_id,
        revision,
        &preview_digest,
        "decision-key-21",
    )
    .await;
    assert_eq!(status, 200, "{denied:?}");
    assert_eq!(denied["status"], "denied");

    // Retry with the same key fails closed; zero effects.
    let (status, body, _) = retire_cmd(port, key, args).await;
    assert_eq!(status, 403, "{body:?}");
    assert_eq!(body["error"]["code"], "approval_invalid");
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM event WHERE event_type='resource.retired'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM entity WHERE id='res-gift1' AND readiness='retired'"
        )
        .await,
        0
    );

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u08_decisions_replay_exactly_and_reject_stale_changes() {
    let (path, workspace, port, service) = setup("decisions").await;
    let seed = seed_retirement_scenario(&workspace).await;
    let args = retire_args(&seed.resource, 3, 5);
    let key = "retire-key-31";

    let (status, _, location) = retire_cmd(port, key, args).await;
    assert_eq!(status, 403);
    let approval_id = location
        .unwrap()
        .trim_start_matches("/api/v1/approvals/")
        .to_owned();
    let (id, revision, preview_digest) = pending_approval(port).await;
    assert_eq!(approval_id, id);

    // Exact decision replay with the same key returns the original outcome.
    let (status, first) = grant(
        port,
        &approval_id,
        revision,
        &preview_digest,
        "decision-key-31",
    )
    .await;
    assert_eq!(status, 200);
    let (status, replay) = grant(
        port,
        &approval_id,
        revision,
        &preview_digest,
        "decision-key-31",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(replay, first);

    // Reusing the decision key with a different body is terminal.
    let (status, mismatched) = grant(
        port,
        &approval_id,
        revision,
        "0000000000000000000000000000000000000000000000000000000000000000",
        "decision-key-31",
    )
    .await;
    assert_eq!(status, 422, "{mismatched:?}");
    assert_eq!(mismatched["error"]["code"], "idempotency_mismatch");

    // A decision on a changed revision conflicts.
    let (status, stale) = grant(
        port,
        &approval_id,
        revision + 5,
        &preview_digest,
        "decision-key-32",
    )
    .await;
    assert_eq!(status, 409, "{stale:?}");
    assert_eq!(stale["error"]["code"], "approval_stale");

    // Deny at the current revision revokes the previously granted approval.
    let (status, revoked) = deny(
        port,
        &approval_id,
        revision + 1,
        &preview_digest,
        "decision-key-33",
    )
    .await;
    assert_eq!(status, 200, "{revoked:?}");
    assert_eq!(revoked["status"], "revoked");

    // A grant after revocation is not actionable; the intent fails closed.
    let (status, not_actionable) = grant(
        port,
        &approval_id,
        revision + 2,
        &preview_digest,
        "decision-key-34",
    )
    .await;
    assert_eq!(status, 409, "{not_actionable:?}");
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM event WHERE event_type='resource.retired'"
        )
        .await,
        0
    );

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u08_stale_dependency_blocks_approved_execution() {
    let (path, workspace, port, service) = setup("stale").await;
    let seed = seed_retirement_scenario(&workspace).await;
    let args = retire_args(&seed.resource, 3, 5);
    let key = "retire-key-41";

    let (status, _, location) = retire_cmd(port, key, args.clone()).await;
    assert_eq!(status, 403);
    let approval_id = location
        .unwrap()
        .trim_start_matches("/api/v1/approvals/")
        .to_owned();
    let (id, revision, preview_digest) = pending_approval(port).await;
    assert_eq!(approval_id, id);
    let (status, _) = grant(
        port,
        &approval_id,
        revision,
        &preview_digest,
        "decision-key-41",
    )
    .await;
    assert_eq!(status, 200);

    // The world changes: the resource's content revision advances.
    let now = now_str();
    sqlx::query(
        "UPDATE entity SET resource_content_revision=6, revision=4, updated_at=? WHERE id=?",
    )
    .bind(&now)
    .bind(&seed.resource)
    .execute(workspace.pool())
    .await
    .unwrap();

    // Execution recomputes the fingerprint and refuses to consume the grant.
    let (status, body, _) = retire_cmd(port, key, args).await;
    assert_eq!(status, 409, "{body:?}");
    assert_eq!(body["error"]["code"], "approval_stale");
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM event WHERE event_type='resource.retired'"
        )
        .await,
        0
    );
    let row: String = sqlx::query_scalar("SELECT status FROM command_approval WHERE id=?")
        .bind(&approval_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(row, "stale");

    // The approval is purged of its private payloads.
    let (status, detail) = request(
        port,
        "GET",
        &format!("/api/v1/approvals/{approval_id}"),
        None,
        "approval-read",
    )
    .await;
    assert_eq!(status, 200);
    assert!(detail.get("args").is_none());
    assert!(detail.get("preview").is_none());

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u08_dry_run_produces_preview_without_effect() {
    let (path, workspace, port, service) = setup("dry-run").await;
    let seed = seed_retirement_scenario(&workspace).await;
    let args = retire_args(&seed.resource, 3, 5);

    let (status, body) = request(
        port,
        "POST",
        "/api/v1/commands/resource.retire",
        Some(json!({"version": 1, "args": args, "dry_run": true})),
        "retire-dry-key",
    )
    .await;
    assert_eq!(status, 200, "{body:?}");
    assert_eq!(body["dry_run"], true);
    assert_eq!(body["result"]["title"], "Gift A");
    assert_eq!(body["result"]["impacted_versions_count"], 2);
    assert_eq!(body["result"]["placement_removed"], true);

    // Zero domain effects, no approval, one dry-run receipt.
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM event WHERE event_type='resource.retired'"
        )
        .await,
        0
    );
    assert_eq!(
        count(&workspace, "SELECT count(*) FROM command_approval").await,
        0
    );
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM command_receipt WHERE mode='dry_run'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM entity WHERE id='res-gift1' AND readiness='ready'"
        )
        .await,
        1
    );

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u08_expired_approval_fails_closed() {
    let (path, workspace, port, service) = setup("expired").await;
    let seed = seed_retirement_scenario(&workspace).await;
    let args = retire_args(&seed.resource, 3, 5);
    let key = "retire-key-51";

    let (status, _, location) = retire_cmd(port, key, args.clone()).await;
    assert_eq!(status, 403);
    let approval_id = location
        .unwrap()
        .trim_start_matches("/api/v1/approvals/")
        .to_owned();
    let (id, _, _) = pending_approval(port).await;
    assert_eq!(approval_id, id);

    // Force the TTL boundary by moving the expiry into the past.
    sqlx::query("UPDATE command_approval SET expires_at='2020-01-01T00:00:00.000Z' WHERE id=?")
        .bind(&approval_id)
        .execute(workspace.pool())
        .await
        .unwrap();

    let (status, body, _) = retire_cmd(port, key, args).await;
    assert_eq!(status, 403, "{body:?}");
    assert_eq!(body["error"]["code"], "approval_invalid");
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM event WHERE event_type='resource.retired'"
        )
        .await,
        0
    );
    let row: String = sqlx::query_scalar("SELECT status FROM command_approval WHERE id=?")
        .bind(&approval_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(row, "expired");

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u08_beyond_256_versions_uses_owner_workflow() {
    let (path, workspace, port, service) = setup("too-many").await;
    let seed = seed_retirement_scenario(&workspace).await;
    // Attach 257 more material versions to the resource.
    let now = now_str();
    for index in 10..267i64 {
        let material = format!("mat-extra-{index}");
        sqlx::query(
            "INSERT INTO material(id,opportunity_id,recipient_id,kind,head_version,created_at,updated_at) \
             VALUES(?,'opportunity-1','recipient-1','outreach',1,?,?)",
        )
        .bind(&material).bind(&now).bind(&now)
        .execute(workspace.pool()).await.unwrap();
        sqlx::query(
            "INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at) \
             VALUES(?,1, 'opportunity-1','recipient-1','template-1',1,'b','{}','{}','h',?)",
        )
        .bind(&material).bind(&now)
        .execute(workspace.pool()).await.unwrap();
        sqlx::query(
            "INSERT INTO version_dependency(material_id,version_no,kind,source_id,source_revision) \
             VALUES(?,1,'resource','res-gift1','5')",
        )
        .bind(&material)
        .execute(workspace.pool())
        .await
        .unwrap();
    }

    let args = retire_args(&seed.resource, 3, 5);
    let (status, body, _) = retire_cmd(port, "retire-key-61", args).await;
    assert_eq!(status, 409, "{body:?}");
    assert_eq!(body["error"]["code"], "requires_owner_workflow");
    assert_eq!(
        count(&workspace, "SELECT count(*) FROM command_approval").await,
        0
    );
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM event WHERE event_type='resource.retired'"
        )
        .await,
        0
    );

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u08_race_grant_vs_deny() {
    let (path, workspace, port, service) = setup("race-decision").await;
    let seed = seed_retirement_scenario(&workspace).await;
    let args = retire_args(&seed.resource, 3, 5);
    let key = "retire-key-71";

    let (status, _, location) = retire_cmd(port, key, args.clone()).await;
    assert_eq!(status, 403);
    let approval_id = location
        .unwrap()
        .trim_start_matches("/api/v1/approvals/")
        .to_owned();
    let (_, revision, preview_digest) = pending_approval(port).await;

    let grant_task = {
        let approval_id = approval_id.clone();
        let preview_digest = preview_digest.clone();
        tokio::spawn(async move {
            grant(
                port,
                &approval_id,
                revision,
                &preview_digest,
                "decision-key-71",
            )
            .await
        })
    };
    let deny_task = {
        let approval_id = approval_id.clone();
        let preview_digest = preview_digest.clone();
        tokio::spawn(async move {
            deny(
                port,
                &approval_id,
                revision,
                &preview_digest,
                "decision-key-72",
            )
            .await
        })
    };
    let (grant_status, grant_body) = grant_task.await.unwrap();
    let (deny_status, deny_body) = deny_task.await.unwrap();
    assert!(grant_status == 200 || grant_status == 409, "{grant_body:?}");
    assert!(deny_status == 200 || deny_status == 409, "{deny_body:?}");
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM command_approval WHERE status IN ('granted','revoked')"
        )
        .await,
        1
    );

    // The final disposition decides: a still-granted approval executes; a
    // revoked one denies.
    let row: String = sqlx::query_scalar("SELECT status FROM command_approval WHERE id=?")
        .bind(&approval_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    let (status, body, _) = retire_cmd(port, key, args).await;
    if row == "granted" {
        assert_eq!(status, 200, "{body:?}");
        assert_eq!(
            count(
                &workspace,
                "SELECT count(*) FROM event WHERE event_type='resource.retired'"
            )
            .await,
            1
        );
    } else {
        assert_eq!(status, 403, "{body:?}");
        assert_eq!(body["error"]["code"], "approval_invalid");
        assert_eq!(
            count(
                &workspace,
                "SELECT count(*) FROM event WHERE event_type='resource.retired'"
            )
            .await,
            0
        );
    }

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u08_claim_rollback_keeps_grant() {
    let (path, workspace, port, service) = setup("claim-rollback").await;
    let seed = seed_retirement_scenario(&workspace).await;
    let args = retire_args(&seed.resource, 3, 5);
    let key = "retire-key-81";

    let (status, _, location) = retire_cmd(port, key, args.clone()).await;
    assert_eq!(status, 403);
    let approval_id = location
        .unwrap()
        .trim_start_matches("/api/v1/approvals/")
        .to_owned();
    let (_, revision, preview_digest) = pending_approval(port).await;
    let (status, _) = grant(
        port,
        &approval_id,
        revision,
        &preview_digest,
        "decision-key-81",
    )
    .await;
    assert_eq!(status, 200);

    // A pre-commit crash after consumption aborts the whole transaction,
    // including the approval claim: the grant must remain usable on the
    // original key and the crash attempt leaves zero effects.
    let crash_key = "fault-precommit-u08-1";
    let (status, body, _) = retire_cmd(port, crash_key, args.clone()).await;
    assert_eq!(status, 503, "{body:?}");
    assert_eq!(body["error"]["code"], "unavailable");
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM event WHERE event_type='resource.retired'"
        )
        .await,
        0
    );
    let row: String = sqlx::query_scalar("SELECT status FROM command_approval WHERE id=?")
        .bind(&approval_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(row, "granted");

    // A clean retry with the original key succeeds and consumes the grant.
    let (status, body, _) = retire_cmd(port, "retire-key-81", args).await;
    assert_eq!(status, 200, "{body:?}");
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM event WHERE event_type='resource.retired'"
        )
        .await,
        1
    );
    let row: String = sqlx::query_scalar("SELECT status FROM command_approval WHERE id=?")
        .bind(&approval_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(row, "consumed");

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u08_owner_rest_retirement_still_works() {
    let (path, workspace, port, service) = setup("rest-retire").await;
    let seed = seed_retirement_scenario(&workspace).await;

    // The owner REST route keeps its direct entry, wire envelope and receipt
    // replay while sharing the application facts.
    let key = "rest-retire-key";
    for _ in 0..2 {
        let (status, body) = request(
            port,
            "POST",
            &format!("/api/v1/resource-cards/{}/retire", seed.resource),
            None,
            key,
        )
        .await;
        assert_eq!(status, 200, "{body:?}");
        assert_eq!(body["entity"]["readiness"], "retired");
        assert_eq!(body["entity"]["revision"], 4);
        assert_eq!(body["affected_versions"].as_array().unwrap().len(), 2);
    }
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM event WHERE event_type='resource.retired'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&workspace, "SELECT count(*) FROM command_approval").await,
        0
    );

    cleanup(&path, workspace, service);
}

// --- delegated agent wait/resume ----------------------------------------

struct Provider {
    port: u16,
    task: Option<tokio::task::JoinHandle<()>>,
}

type Responder = Box<dyn FnMut(usize, &str) -> Value + Send>;

impl Provider {
    async fn with(responder: Responder) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut responder = responder;
        let task = tokio::spawn(async move {
            for index in 0..128 {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut head_bytes = Vec::new();
                let mut byte = [0u8; 1];
                while !head_bytes.ends_with(b"\r\n\r\n") {
                    if stream.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    head_bytes.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head_bytes).to_string();
                let length: usize = head
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .and_then(|(_, value)| value.trim().parse().ok())
                    })
                    .unwrap_or(0);
                let mut body_bytes = vec![0u8; length];
                let _ = stream.read_exact(&mut body_bytes).await;
                let body = String::from_utf8_lossy(&body_bytes).to_string();
                let payload = responder(index, &body).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Self {
            port,
            task: Some(task),
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    async fn stop(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn final_answer(content: &str) -> Value {
    json!({
        "message": {"role":"assistant","content":content},
        "prompt_eval_count": 12, "eval_count": 8
    })
}

fn tool_call(name: &str, args: Value, content: &str) -> Value {
    json!({
        "message": {
            "role": "assistant", "content": content,
            "tool_calls": [{"id":"call","function":{"name":name,"arguments":args.to_string()}}]
        },
        "prompt_eval_count": 12, "eval_count": 8
    })
}

async fn set_provider(port: u16, url: &str, key: &str) {
    let (status, reply) = request(
        port,
        "PUT",
        "/api/v1/agent/provider",
        Some(json!({
            "endpoint": url, "model": "fixture", "num_ctx": 8192,
            "num_predict": 256, "locale": "en"
        })),
        key,
    )
    .await;
    assert!(status == 200 || status == 204, "{status} {reply:?}");
}

async fn start_run(
    port: u16,
    task: &str,
    targets: Value,
    permissions: Value,
    key: &str,
) -> (u16, Value) {
    request(
        port,
        "POST",
        "/api/v1/agent/runs",
        Some(json!({
            "task": task,
            "targets": targets,
            "permissions": permissions,
            "allow_created_opportunities": false,
        })),
        key,
    )
    .await
}

async fn wait_state(port: u16, id: &str, states: &[&str]) -> Value {
    for _ in 0..300 {
        let (status, value) = request(
            port,
            "GET",
            &format!("/api/v1/agent/runs/{id}"),
            None,
            "run-read",
        )
        .await;
        assert_eq!(status, 200, "{value:?}");
        if states.contains(&value["state"].as_str().unwrap_or_default()) {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (_, turns) = request(
        port,
        "GET",
        &format!("/api/v1/agent/runs/{id}/transcript?limit=20"),
        None,
        "transcript-read",
    )
    .await;
    panic!("run {id} never reached {states:?}; turns={turns:?}");
}

async fn transcript(port: u16, id: &str) -> Vec<Value> {
    let (status, value) = request(
        port,
        "GET",
        &format!("/api/v1/agent/runs/{id}/transcript?limit=20"),
        None,
        "transcript-read",
    )
    .await;
    assert_eq!(status, 200, "{value:?}");
    value["items"].as_array().unwrap().clone()
}

#[tokio::test]
async fn u08_agent_request_wait_and_resume() {
    let (path, workspace, port, service) = setup("agent-approval").await;
    let seed = seed_retirement_scenario(&workspace).await;

    let args = retire_args(&seed.resource, 3, 5);
    let tool_args = json!({
        "id": "resource.retire",
        "version": 1,
        "args": args,
    });
    let mut chat_calls = 0usize;
    let provider = Provider::with(Box::new(move |index, _body| {
        if index == 0 {
            // the tool-capability probe is itself a chat turn
            return final_answer("probe");
        }
        chat_calls += 1;
        if chat_calls == 1 {
            tool_call("commands__run", tool_args.clone(), "")
        } else {
            final_answer("Retired.")
        }
    }))
    .await;
    let url = provider.url();
    set_provider(port, &url, "provider-key-u08").await;

    let (status, run) = start_run(
        port,
        "Retire the selected resource after its impact is reviewed.",
        json!([{"kind": "resource", "id": seed.resource}]),
        json!(["resource.retire"]),
        "run-key-u08",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let run_id = run["run_id"].as_str().unwrap().to_owned();

    // The run parks on the approval; the transcript contains only the safe
    // waiting denial, never an approval id.
    wait_state(port, &run_id, &["waiting_owner"]).await;
    let turns = transcript(port, &run_id).await;
    let tool_text: Vec<&str> = turns
        .iter()
        .filter(|turn| turn["speaker"].as_str() == Some("tool"))
        .filter_map(|turn| turn["text"].as_str())
        .collect();
    assert!(tool_text.contains(&"waiting_for_owner"), "{turns:?}");
    let approval_id = {
        let (status, list) = request(
            port,
            "GET",
            "/api/v1/approvals?status=pending",
            None,
            "approval-list",
        )
        .await;
        assert_eq!(status, 200);
        let items = list["items"].as_array().unwrap();
        let row = items
            .iter()
            .find(|item| item["command_id"] == "resource.retire")
            .expect("agent pending approval missing");
        assert_eq!(row["run_id"].as_str(), Some(run_id.as_str()));
        (
            row["id"].as_str().unwrap().to_owned(),
            row["revision"].as_i64().unwrap(),
            row["preview_digest"].as_str().unwrap().to_owned(),
        )
    };
    for turn in &turns {
        let text = turn["text"].as_str().unwrap_or_default();
        assert!(
            !text.contains(&approval_id.0),
            "approval id leaked into transcript"
        );
    }

    // Owner grants; the runtime resumes, re-dispatches the durable intent and
    // the effect commits.
    let (status, _) = grant(
        port,
        &approval_id.0,
        approval_id.1,
        &approval_id.2,
        "decision-key-u08",
    )
    .await;
    assert_eq!(status, 200);
    wait_state(port, &run_id, &["succeeded", "failed"]).await;
    let row: (String, i64) = sqlx::query_as("SELECT readiness,revision FROM entity WHERE id=?")
        .bind(&seed.resource)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "retired");
    assert_eq!(row.1, 4);
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM event WHERE event_type='resource.retired'"
        )
        .await,
        1
    );

    // The model never saw the approval id or a credential.
    let turns = transcript(port, &run_id).await;
    for turn in &turns {
        let text = turn["text"].as_str().unwrap_or_default();
        assert!(
            !text.contains(&approval_id.0),
            "approval id leaked into transcript"
        );
        assert!(!text.contains("parley_session"));
    }

    provider.stop().await;
    cleanup(&path, workspace, service);
}

/// Open the fetch-based SSE notification stream with owner headers.
async fn open_sse(port: u16) -> TcpStream {
    let (cookie, token, epoch) = SESSIONS
        .get()
        .unwrap()
        .lock()
        .unwrap()
        .get(&port)
        .unwrap()
        .clone();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(
            format!(
                "GET /api/v1/command-notifications HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\nAccept: text/event-stream\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    stream
}

/// Read SSE chunks until `predicate` holds over the accumulated text, or the
/// timeout expires. Returns the accumulated text.
async fn drain_until(stream: &mut TcpStream, predicate: impl Fn(&str) -> bool) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let read = tokio::time::timeout(remaining, stream.read(&mut chunk))
            .await
            .expect("SSE stream timed out")
            .unwrap();
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&buffer).to_string();
        if predicate(&text) {
            return text;
        }
    }
    String::from_utf8_lossy(&buffer).to_string()
}

#[tokio::test]
async fn u08_approval_inbox_and_notification_stream() {
    let (path, workspace, port, service) = setup("notifications").await;
    let seed = seed_retirement_scenario(&workspace).await;
    let args = retire_args(&seed.resource, 3, 5);

    // Open the SSE stream with owner headers.
    let mut stream = open_sse(port).await;
    // The stream opens with an epoch_changed event.
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
        .await
        .expect("SSE stream timed out")
        .unwrap();
    buffer.extend_from_slice(&chunk[..read]);
    let text = String::from_utf8_lossy(&buffer).to_string();
    assert!(text.contains("event: epoch_changed"), "{text}");

    // A pending approval emits approvals_changed on the stream.
    let (status, _, _) = retire_cmd(port, "retire-key-91", args.clone()).await;
    assert_eq!(status, 403);

    // Granting the approval marks the intent granted; the same-key retry then
    // executes through the adopted writer, which appends the resource.retired
    // domain fact. The L1 writer fan-out must emit domain_changed carrying the
    // post-commit event-log high-water mark, alongside the decision's
    // approvals_changed.
    let (approval_id, revision, preview_digest) = pending_approval(port).await;
    let (status, body) = grant(
        port,
        &approval_id,
        revision,
        &preview_digest,
        "retire-decision-91",
    )
    .await;
    assert_eq!(status, 200, "{body:?}");
    let (status, executed, _) = retire_cmd(port, "retire-key-91", args).await;
    assert_eq!(status, 200, "{executed:?}");
    assert_eq!(executed["result"]["readiness"], "retired");

    let text = drain_until(&mut stream, |t| {
        t.contains("event: approvals_changed") && t.contains("event: domain_changed")
    })
    .await;
    assert!(text.contains("event: approvals_changed"), "{text}");
    assert!(text.contains("event: domain_changed"), "{text}");
    assert!(text.contains("\"cursor\":"), "{text}");

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u08_domain_and_job_change_hints() {
    let (path, workspace, port, service) = setup("hints").await;
    let mut stream = open_sse(port).await;

    // A durable role job inserted for the dispatcher is claimed by the real
    // worker: claim_attempt commits queued→running and the L1 job_changed
    // hint must carry that exact durable job id. The job never needs a
    // provider — the assertion only requires the post-commit hint.
    let job_id = format!("l1-hint-job-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let now = now_str();
    let changed = sqlx::query(
        "INSERT INTO durable_job(
             id,role,status,input_json,input_sha256,scope_kind,scope_id,
             scope_revision,current_attempt,max_attempts,created_at,updated_at)
         VALUES(?,?,'queued','{}',?,'owner',?,1,0,3,?,?)",
    )
    .bind(&job_id)
    .bind("interviewer")
    .bind("a".repeat(64))
    .bind("l1-hint-scope")
    .bind(&now)
    .bind(&now)
    .execute(workspace.pool())
    .await
    .unwrap();
    assert_eq!(changed.rows_affected(), 1);

    let text = drain_until(&mut stream, |t| {
        t.contains("event: job_changed") && t.contains(&format!("\"job_id\":\"{job_id}\""))
    })
    .await;
    assert!(text.contains("event: job_changed"), "{text}");
    assert!(text.contains(&format!("\"job_id\":\"{job_id}\"")), "{text}");

    cleanup(&path, workspace, service);
}
