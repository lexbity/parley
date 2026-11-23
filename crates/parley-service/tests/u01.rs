//! UCR-001 U01: owner changes and undoes an opportunity phase from the
//! palette. Every proof here exercises the packaged service over loopback
//! and inspects the durable facts afterwards. No test stands in for the
//! application authorization, ledger or commit boundary.
use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Barrier,
};

type OwnerSession = (String, String, String);
static NEXT: AtomicU64 = AtomicU64::new(0);
static SESSIONS: OnceLock<Mutex<HashMap<u16, OwnerSession>>> = OnceLock::new();

fn db_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-u01-{tag}-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Send an exact raw body so canonical member ordering and duplicate keys can
/// be exercised without serde_json normalizing the input.
async fn raw_with_headers(
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
    stream
        .write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\n{key_header}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
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
    raw(port, method, path, &body, Some(key)).await
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

async fn create_party(port: u16, name: &str) -> String {
    let (status, entity) = request(
        port,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"person","name":name})),
        &format!("party-{name}"),
    )
    .await;
    assert_eq!(status, 201, "{entity:?}");
    entity["id"].as_str().unwrap().to_string()
}

async fn create_opportunity(port: u16, title: &str, party: &str) -> String {
    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":title,"primary_party_id":party})),
        &format!("opp-{title}"),
    )
    .await;
    assert_eq!(status, 201, "{opportunity:?}");
    opportunity["id"].as_str().unwrap().to_string()
}

fn set_args(opp: &str, revision: i64, stage: &str) -> Value {
    json!({
        "opportunity_id": opp,
        "expected_phase_revision": revision,
        "expected_pack_id": "job_search",
        "expected_pack_version": "1.0.0",
        "stage_id": stage
    })
}

fn undo_args(opp: &str, revision: i64, event: &str) -> Value {
    json!({
        "opportunity_id": opp,
        "expected_phase_revision": revision,
        "expected_pack_id": "job_search",
        "expected_pack_version": "1.0.0",
        "original_event_id": event
    })
}

fn without_headers((status, _, value): (u16, HashMap<String, String>, Value)) -> (u16, Value) {
    (status, value)
}

async fn raw(port: u16, method: &str, path: &str, body: &str, key: Option<&str>) -> (u16, Value) {
    without_headers(raw_with_headers(port, method, path, body, key).await)
}

async fn cmd(port: u16, id: &str, key: &str, args: Value) -> (u16, Value) {
    let body = json!({"version":1,"args":args,"dry_run":false}).to_string();
    raw(
        port,
        "POST",
        &format!("/api/v1/commands/{id}"),
        &body,
        Some(key),
    )
    .await
}

async fn rest_phase(port: u16, opp: &str, target: &str, revision: i64, key: &str) -> (u16, Value) {
    request(
        port,
        "PATCH",
        &format!("/api/v1/opportunities/{opp}/phase"),
        Some(json!({
            "target_stage_id": target,
            "pack_version": "1.0.0",
            "expected_phase_revision": revision
        })),
        key,
    )
    .await
}

async fn count(workspace: &Workspace, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(workspace.pool())
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn u01_set_phase_single_effect_under_32_concurrent_same_key() {
    let (path, workspace, port, service) = setup("race").await;
    let party = create_party(port, "Fieldwork Lead").await;
    let opp = create_opportunity(port, "Race a phase edit", &party).await;
    let args = set_args(&opp, 1, "exploring");
    let barrier = Arc::new(Barrier::new(32));
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let barrier = barrier.clone();
        let args = args.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            cmd(port, "opportunity.set_phase", "race-key-01", args).await
        }));
    }
    let mut ok = 0;
    for task in tasks {
        let (status, reply) = task.await.unwrap();
        assert_eq!(status, 200, "{reply:?}");
        assert_eq!(reply["result"]["phase_revision"], 2);
        ok += 1;
    }
    assert_eq!(ok, 32);
    let revision: i64 = sqlx::query_scalar("SELECT phase_revision FROM opportunity WHERE id=?")
        .bind(&opp)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(revision, 2);
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT COUNT(*) FROM event WHERE entity_id='{opp}' AND event_type='opportunity.phase_changed'")
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &workspace,
            "SELECT COUNT(*) FROM command_receipt WHERE key_digest IS NOT NULL"
        )
        .await,
        1
    );
    let allowed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM command_audit_event WHERE kind='outcome' AND policy_result='allowed'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    let replayed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM command_audit_event WHERE kind='outcome' AND policy_result='replayed'")
        .fetch_one(workspace.pool()).await.unwrap();
    assert_eq!(allowed, 1);
    assert_eq!(replayed, 31);
    let attempts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM command_audit_event WHERE kind='attempt'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(attempts, 32);
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u01_replay_after_lost_ack_restores_original_result() {
    let (path, workspace, port, service) = setup("lost-ack").await;
    let party = create_party(port, "Lost Ack Party").await;
    let opp = create_opportunity(port, "Lost ack phase", &party).await;
    let body = json!({"version":1,"args":set_args(&opp,1,"exploring"),"dry_run":false}).to_string();
    // Commit on the service, then drop the client before it reads the reply.
    {
        let (cookie, token, epoch) = SESSIONS
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .get(&port)
            .unwrap()
            .clone();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(format!("POST /api/v1/commands/opportunity.set_phase HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\nIdempotency-Key: lost-ack-key-01\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        drop(stream);
    }
    let revision: i64 = sqlx::query_scalar("SELECT phase_revision FROM opportunity WHERE id=?")
        .bind(&opp)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(revision, 2, "the effect must be durable before the reply");
    let event_id: String = sqlx::query_scalar(
        "SELECT event_id FROM event WHERE entity_id=? AND event_type='opportunity.phase_changed'",
    )
    .bind(&opp)
    .fetch_one(workspace.pool())
    .await
    .unwrap();

    let (status, replay) = cmd(
        port,
        "opportunity.set_phase",
        "lost-ack-key-01",
        set_args(&opp, 1, "exploring"),
    )
    .await;
    assert_eq!(status, 200, "{replay:?}");
    assert_eq!(replay["replayed"], true);
    assert!(replay["original_audit_id"].as_str().is_some());
    assert_eq!(replay["result"]["event_id"], event_id);
    assert_eq!(replay["result"]["phase_revision"], 2);
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{opp}' AND event_type='opportunity.phase_changed'")).await,
        1
    );
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u01_canonical_member_order_replays_same_intent() {
    let (path, workspace, port, service) = setup("jcs").await;
    let party = create_party(port, "JCS Party").await;
    let opp = create_opportunity(port, "Canonical ordering", &party).await;
    let first = format!(
        "{{\"version\":1,\"args\":{{\"opportunity_id\":\"{opp}\",\"expected_phase_revision\":1,\"expected_pack_id\":\"job_search\",\"expected_pack_version\":\"1.0.0\",\"stage_id\":\"exploring\"}},\"dry_run\":false}}"
    );
    let second = format!(
        "{{\"args\":{{\"stage_id\":\"exploring\",\"expected_pack_version\":\"1.0.0\",\"expected_pack_id\":\"job_search\",\"expected_phase_revision\":1,\"opportunity_id\":\"{opp}\"}},\"dry_run\":false,\"version\":1}}"
    );
    let (status, one) = raw(
        port,
        "POST",
        "/api/v1/commands/opportunity.set_phase",
        &first,
        Some("jcs-key-01"),
    )
    .await;
    assert_eq!(status, 200, "{one:?}");
    let (status, two) = raw(
        port,
        "POST",
        "/api/v1/commands/opportunity.set_phase",
        &second,
        Some("jcs-key-01"),
    )
    .await;
    assert_eq!(status, 200, "{two:?}");
    assert_eq!(two["replayed"], true);
    assert_eq!(one["result"]["event_id"], two["result"]["event_id"]);
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{opp}' AND event_type='opportunity.phase_changed'")).await,
        1
    );
    // A different intent under the same key is terminal for that key.
    let other = format!(
        "{{\"version\":1,\"args\":{{\"opportunity_id\":\"{opp}\",\"expected_phase_revision\":1,\"expected_pack_id\":\"job_search\",\"expected_pack_version\":\"1.0.0\",\"stage_id\":\"preparing\"}},\"dry_run\":false}}"
    );
    let (status, mismatch) = raw(
        port,
        "POST",
        "/api/v1/commands/opportunity.set_phase",
        &other,
        Some("jcs-key-01"),
    )
    .await;
    assert_eq!(status, 422, "{mismatch:?}");
    assert_eq!(mismatch["error"]["code"], "idempotency_mismatch");
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{opp}' AND event_type='opportunity.phase_changed'")).await,
        1
    );
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u01_boot_validation_rejects_invalid_registration() {
    parley_service::validate_command_catalogue().expect("active catalogue must boot");
    let object = json!({"type":"object","additionalProperties":false,"properties":{}});
    assert!(
        parley_service::validate_command_descriptor_shape(
            "opportunity.set_phase",
            &object,
            &object,
            Some("route == \"queue\"")
        )
        .is_ok()
    );
    // A single segment, a reserved prefix, a $ref schema and a malformed
    // expression must each be rejected before any descriptor is published.
    assert!(
        parley_service::validate_command_descriptor_shape("opportunity", &object, &object, None)
            .is_err()
    );
    assert!(
        parley_service::validate_command_descriptor_shape("ui.open_queue", &object, &object, None)
            .is_err()
    );
    assert!(
        parley_service::validate_command_descriptor_shape(
            "opportunity.set_phase",
            &json!({"type":"object","additionalProperties":false,"$ref":"#/remote"}),
            &object,
            None
        )
        .is_err()
    );
    assert!(
        parley_service::validate_command_descriptor_shape(
            "opportunity.set_phase",
            &object,
            &object,
            Some("route == &&")
        )
        .is_err()
    );
    assert!(parley_service::command_table_policy_version() >= 1);
}

#[tokio::test]
async fn u01_invalid_args_are_terminal_and_redacted() {
    let (path, workspace, port, service) = setup("invalid-args").await;
    let party = create_party(port, "Redaction Party").await;
    let opp = create_opportunity(port, "Redacted args", &party).await;
    let mut bad = set_args(&opp, 1, "exploring");
    bad.as_object_mut().unwrap().remove("stage_id");
    let (status, error) = cmd(port, "opportunity.set_phase", "bad-args-key-01", bad).await;
    assert_eq!(status, 422, "{error:?}");
    assert_eq!(error["error"]["code"], "invalid_args");
    assert_eq!(error["error"]["details"][0]["problem"], "schema_mismatch");
    let args_json: String = sqlx::query_scalar(
        "SELECT args_json FROM command_audit_event WHERE kind='attempt' AND key_digest IS NOT NULL LIMIT 1",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert!(args_json.contains("[redacted]"), "{args_json}");
    assert!(
        !args_json.contains(&opp),
        "private id leaked into audit: {args_json}"
    );
    let (status, mismatch) = cmd(
        port,
        "opportunity.set_phase",
        "bad-args-key-01",
        set_args(&opp, 1, "exploring"),
    )
    .await;
    assert_eq!(status, 422, "{mismatch:?}");
    assert_eq!(mismatch["error"]["code"], "idempotency_mismatch");
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{opp}' AND event_type='opportunity.phase_changed'")).await,
        0
    );
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u01_precommit_crash_leaves_no_effect() {
    let (path, workspace, port, service) = setup("precommit").await;
    let party = create_party(port, "Crash Party").await;
    let opp = create_opportunity(port, "Precommit crash", &party).await;
    let (status, reply) = cmd(
        port,
        "opportunity.set_phase",
        "fault-precommit-01",
        set_args(&opp, 1, "exploring"),
    )
    .await;
    assert_eq!(status, 503, "{reply:?}");
    let revision: i64 = sqlx::query_scalar("SELECT phase_revision FROM opportunity WHERE id=?")
        .bind(&opp)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(
        revision, 1,
        "a lost pre-commit operation must have zero effect"
    );
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{opp}' AND event_type='opportunity.phase_changed'")).await, 0
    );
    assert_eq!(
        count(&workspace, "SELECT COUNT(*) FROM command_receipt").await,
        0
    );
    let (status, retry) = cmd(
        port,
        "opportunity.set_phase",
        "fault-precommit-01",
        set_args(&opp, 1, "exploring"),
    )
    .await;
    assert_eq!(status, 200, "{retry:?}");
    assert_eq!(retry["replayed"], false);
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u01_postcommit_crash_replays_committed_effect() {
    let (path, workspace, port, service) = setup("postcommit").await;
    let party = create_party(port, "Ambiguous Party").await;
    let opp = create_opportunity(port, "Postcommit crash", &party).await;
    let (status, reply) = cmd(
        port,
        "opportunity.set_phase",
        "fault-postcommit-01",
        set_args(&opp, 1, "exploring"),
    )
    .await;
    assert_eq!(status, 503, "{reply:?}");
    assert_eq!(reply["error"]["code"], "unavailable");
    let revision: i64 = sqlx::query_scalar("SELECT phase_revision FROM opportunity WHERE id=?")
        .bind(&opp)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(
        revision, 2,
        "the effect committed before the ambiguous reply"
    );
    let event_id: String = sqlx::query_scalar(
        "SELECT event_id FROM event WHERE entity_id=? AND event_type='opportunity.phase_changed'",
    )
    .bind(&opp)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    let (status, replay) = cmd(
        port,
        "opportunity.set_phase",
        "fault-postcommit-01",
        set_args(&opp, 1, "exploring"),
    )
    .await;
    assert_eq!(status, 200, "{replay:?}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"]["event_id"], event_id);
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{opp}' AND event_type='opportunity.phase_changed'")).await, 1
    );
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u01_startup_recovery_finalizes_interrupted_attempt() {
    let (path, workspace, _port, service) = setup("recovery").await;
    let epoch: String = sqlx::query_scalar("SELECT epoch FROM workspace_meta WHERE singleton=1")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO command_audit_event(audit_id,kind,epoch,actor_kind,principal_pseudonym,command_id,command_version,mode,policy_result,occurred_at) VALUES('aud-u01-orphan','attempt',?,'owner','probe','opportunity.set_phase',1,'execute','allowed','2026-11-23T00:00:00.000Z')")
        .bind(&epoch)
        .execute(workspace.pool())
        .await
        .unwrap();
    drop(workspace);
    drop(service);
    let recovered = Workspace::open(&path).await.unwrap();
    let (policy_result, attempt_id): (String, String) = sqlx::query_as(
        "SELECT policy_result,attempt_audit_id FROM command_audit_event WHERE kind='outcome' AND attempt_audit_id='aud-u01-orphan'",
    )
    .fetch_one(recovered.pool())
    .await
    .unwrap();
    assert_eq!(policy_result, "interrupted");
    assert_eq!(attempt_id, "aud-u01-orphan");
    drop(recovered);
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn u01_audit_unavailable_fails_closed() {
    let (path, workspace, port, service) = setup("audit-down").await;
    let party = create_party(port, "Audit Party").await;
    let opp = create_opportunity(port, "Audit unavailable", &party).await;
    sqlx::query("DROP TABLE command_audit_event")
        .execute(workspace.pool())
        .await
        .unwrap();
    let (status, reply) = cmd(
        port,
        "opportunity.set_phase",
        "audit-down-key-01",
        set_args(&opp, 1, "exploring"),
    )
    .await;
    assert_eq!(status, 503, "{reply:?}");
    let revision: i64 = sqlx::query_scalar("SELECT phase_revision FROM opportunity WHERE id=?")
        .bind(&opp)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(revision, 1, "an unavailable audit store must block effects");
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u01_rest_and_command_share_one_fact_and_event() {
    let (path, workspace, port, service) = setup("adapters").await;
    let party = create_party(port, "Adapter Party").await;
    let opp = create_opportunity(port, "Shared fact", &party).await;
    let (_, before) = request(
        port,
        "GET",
        "/api/v1/commands/opportunity.set_phase",
        None,
        "describe-before",
    )
    .await;
    let (status, rest) = rest_phase(port, &opp, "exploring", 1, "rest-share-key").await;
    assert_eq!(status, 200, "{rest:?}");
    let rest_cursor: i64 = sqlx::query_scalar("SELECT MAX(cursor) FROM event WHERE entity_id=?")
        .bind(&opp)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    let (status, command) = cmd(
        port,
        "opportunity.set_phase",
        "cmd-share-key",
        set_args(&opp, 2, "preparing"),
    )
    .await;
    assert_eq!(status, 200, "{command:?}");
    assert_eq!(command["result"]["phase_revision"], 3);
    let command_cursor: i64 = command["result"]["event_cursor"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(command_cursor > rest_cursor);
    let revision: i64 = sqlx::query_scalar("SELECT phase_revision FROM opportunity WHERE id=?")
        .bind(&opp)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(revision, 3);
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{opp}' AND event_type='opportunity.phase_changed'")).await,
        2
    );
    // A REST adapter projection change must not alter the command contract:
    // descriptor schemas compare byte-for-byte before and after the REST call.
    let (_, after) = request(
        port,
        "GET",
        "/api/v1/commands/opportunity.set_phase",
        None,
        "describe-after",
    )
    .await;
    assert_eq!(before["args_schema"], after["args_schema"]);
    assert_eq!(before["result_schema"], after["result_schema"]);
    // The catalogue hash is deterministic across describe/query calls.
    let (_, first_manifest) = request(
        port,
        "POST",
        "/api/v1/commands/query",
        Some(json!({"surface":"palette","context":{"route":"queue","selection.opportunity":true}})),
        "manifest-key-1",
    )
    .await;
    let (_, second_manifest) = request(
        port,
        "POST",
        "/api/v1/commands/query",
        Some(json!({"surface":"palette","context":{"route":"queue","selection.opportunity":true}})),
        "manifest-key-2",
    )
    .await;
    assert_eq!(
        first_manifest["manifest_version"],
        second_manifest["manifest_version"]
    );
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u01_undo_restores_unmapped_predecessor() {
    let (path, workspace, port, service) = setup("unmapped").await;
    let party = create_party(port, "Undo Party").await;
    let opp = create_opportunity(port, "Restore Unmapped", &party).await;
    let (status, first) = cmd(
        port,
        "opportunity.set_phase",
        "unmapped-key-1",
        set_args(&opp, 1, "exploring"),
    )
    .await;
    assert_eq!(status, 200, "{first:?}");
    let event_id = first["result"]["event_id"].as_str().unwrap().to_string();
    let (status, undo) = cmd(
        port,
        "opportunity.undo_phase_change",
        "unmapped-key-2",
        undo_args(&opp, 2, &event_id),
    )
    .await;
    assert_eq!(status, 200, "{undo:?}");
    assert_eq!(undo["result"]["stage_id"], Value::Null);
    assert_eq!(undo["result"]["previous_stage_id"], "exploring");
    assert_eq!(undo["result"]["phase_revision"], 3);
    assert_eq!(undo["result"]["reverted"], true);
    let stage: Option<String> = sqlx::query_scalar("SELECT stage_id FROM opportunity WHERE id=?")
        .bind(&opp)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(
        stage, None,
        "Unmapped is a null predecessor, not an invented stage"
    );
    let payload: String = sqlx::query_scalar("SELECT payload_json FROM event WHERE event_id=?")
        .bind(undo["result"]["event_id"].as_str().unwrap())
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["from_stage_id"], "exploring");
    assert_eq!(payload["to_stage_id"], Value::Null);
    assert_eq!(payload["operation"], "undo");
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u01_backup_scrubs_command_canary() {
    let (path, workspace, port, service) = setup("canary").await;
    let party = create_party(port, "Canary Party").await;
    let opp = create_opportunity(port, "Backup canary", &party).await;
    let (status, reply) = cmd(
        port,
        "opportunity.set_phase",
        "canary-key-01",
        set_args(&opp, 1, "exploring"),
    )
    .await;
    assert_eq!(status, 200, "{reply:?}");
    let canary = "U01-CANARY-9f4c2a";
    sqlx::query(
        "UPDATE command_receipt SET response_bytes=? WHERE command_id='opportunity.set_phase'",
    )
    .bind(format!("{{\"canary\":\"{canary}\"}}"))
    .execute(workspace.pool())
    .await
    .unwrap();
    let backup = parley_service::create_backup_for_test(&workspace)
        .await
        .unwrap();
    let settings = workspace.maintenance_settings().await.unwrap();
    let backup_dir = PathBuf::from(settings.backup_directory).join(&backup);
    let snapshot = backup_dir.join("workspace.sqlite");
    assert!(snapshot.is_file());
    let snapshot_pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&snapshot)
            .read_only(true),
    )
    .await
    .unwrap();
    let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM command_receipt")
        .fetch_one(&snapshot_pool)
        .await
        .unwrap();
    drop(snapshot_pool);
    assert_eq!(
        receipts, 0,
        "receipts must be scrubbed from backup snapshots"
    );
    let mut found = false;
    for entry in fs::read_dir(&backup_dir).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_file() {
            let bytes = fs::read(entry.path()).unwrap();
            if bytes
                .windows(canary.len())
                .any(|window| window == canary.as_bytes())
            {
                found = true;
            }
        }
    }
    assert!(!found, "command canary leaked into the backup snapshot");
    assert!(
        !backup_dir.join("secret.key").exists(),
        "generation key leaked into backup"
    );
    let live: String = sqlx::query_scalar(
        "SELECT response_bytes FROM command_receipt WHERE command_id='opportunity.set_phase'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert!(
        live.contains(canary),
        "the live ledger must retain its receipt"
    );
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u01_manifest_search_describe_contract() {
    let (path, workspace, port, service) = setup("discovery").await;
    let context = json!({"route":"queue","selection.opportunity":true});
    let (status, manifest) = request(
        port,
        "POST",
        "/api/v1/commands/query",
        Some(json!({"surface":"palette","context":context})),
        "query-key-1",
    )
    .await;
    assert_eq!(status, 200, "{manifest:?}");
    assert_eq!(manifest["contract_major"], 1);
    assert_eq!(manifest["truncated"], false);
    assert!(manifest["manifest_version"].as_str().is_some());
    let ids: Vec<&str> = manifest["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![
            "opportunity.create",
            "opportunity.set_phase",
            "opportunity.set_task_done",
            "opportunity.undo_phase_change"
        ]
    );
    let (status, unconditioned) = request(
        port,
        "POST",
        "/api/v1/commands/query",
        Some(json!({"surface":"palette","context":{}})),
        "query-key-2",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(unconditioned["commands"].as_array().unwrap().len(), 0);
    let (status, denied) = request(
        port,
        "POST",
        "/api/v1/commands/query",
        Some(json!({"surface":"agent","context":{}})),
        "query-key-3",
    )
    .await;
    assert_eq!(status, 403, "{denied:?}");
    assert_eq!(denied["error"]["code"], "forbidden");

    let (status, search) = request(
        port,
        "POST",
        "/api/v1/commands/search",
        Some(json!({"query":"phase","limit":2})),
        "search-key-1",
    )
    .await;
    assert_eq!(status, 200, "{search:?}");
    assert!(
        search["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "opportunity.set_phase")
    );
    for item in search["items"].as_array().unwrap() {
        assert!(
            item.get("args_schema").is_none(),
            "search must not expose schemas"
        );
    }
    let (status, bad_limit) = request(
        port,
        "POST",
        "/api/v1/commands/search",
        Some(json!({"query":"phase","limit":0})),
        "search-key-2",
    )
    .await;
    assert_eq!(status, 400, "{bad_limit:?}");

    let (status, descriptor) = request(
        port,
        "GET",
        "/api/v1/commands/opportunity.set_phase",
        None,
        "describe-key-1",
    )
    .await;
    assert_eq!(status, 200, "{descriptor:?}");
    assert_eq!(descriptor["version"], 1);
    assert_eq!(descriptor["requires_permission"], "opportunity.phase.edit");
    assert_eq!(descriptor["args_schema"]["additionalProperties"], false);
    let (status, unknown) = request(
        port,
        "GET",
        "/api/v1/commands/nope.missing",
        None,
        "describe-key-2",
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(unknown["error"]["code"], "unknown_command");

    let (status, headers, _) = raw_with_headers(
        port,
        "POST",
        "/api/v1/commands/query",
        &json!({"surface":"palette","context":context}).to_string(),
        Some("query-key-4"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        headers.get("cache-control").map(String::as_str),
        Some("private, no-store")
    );
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u01_request_bounds_and_unique_json() {
    let (path, workspace, port, service) = setup("bounds").await;
    let party = create_party(port, "Bounds Party").await;
    let opp = create_opportunity(port, "Request bounds", &party).await;
    let oversized = "x".repeat(1024 * 1024 + 1);
    let (status, _) = raw(
        port,
        "POST",
        "/api/v1/commands/opportunity.set_phase",
        &oversized,
        Some("bounds-key-1"),
    )
    .await;
    assert_eq!(status, 413);
    let duplicated = format!(
        "{{\"version\":1,\"args\":{{\"opportunity_id\":\"{opp}\",\"opportunity_id\":\"other\",\"expected_phase_revision\":1,\"expected_pack_id\":\"job_search\",\"expected_pack_version\":\"1.0.0\",\"stage_id\":\"exploring\"}},\"dry_run\":false}}"
    );
    let (status, reply) = raw(
        port,
        "POST",
        "/api/v1/commands/opportunity.set_phase",
        &duplicated,
        Some("bounds-key-2"),
    )
    .await;
    assert_eq!(status, 400, "{reply:?}");
    assert_eq!(reply["error"]["code"], "malformed_request");
    let (status, _) = raw(
        port,
        "POST",
        "/api/v1/commands/opportunity.set_phase",
        &json!({"version":1,"args":set_args(&opp,1,"exploring"),"dry_run":false}).to_string(),
        Some("bad key!"),
    )
    .await;
    assert_eq!(status, 400);
    let (status, _) = raw(
        port,
        "POST",
        "/api/v1/commands/opportunity.set_phase",
        &json!({"version":1,"args":set_args(&opp,1,"exploring"),"dry_run":true}).to_string(),
        Some("bounds-key-3"),
    )
    .await;
    assert_eq!(status, 422, "phase commands do not support dry run");
    cleanup(&path, workspace, service);
}
