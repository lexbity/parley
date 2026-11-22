use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

type OwnerSession = (String, String, String);
static NEXT: AtomicU64 = AtomicU64::new(0);
static SESSIONS: OnceLock<Mutex<std::collections::HashMap<u16, OwnerSession>>> = OnceLock::new();
fn db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-s09-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: &str,
) -> (u16, Value) {
    let (cookie, token, epoch) = SESSIONS
        .get()
        .unwrap()
        .lock()
        .unwrap()
        .get(&port)
        .unwrap()
        .clone();
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\nIdempotency-Key: {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
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
    let value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(payload).unwrap()
    };
    (status, value.get("data").cloned().unwrap_or(value))
}

async fn owner_session(service: &RunningService) -> OwnerSession {
    let address = service.address;
    let body = json!({"secret":service.bootstrap_secret}).to_string();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(format!("POST /api/v1/session/bootstrap HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
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
    stream.write_all(format!("GET /api/v1/session HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nCookie: {cookie}\r\nContent-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let payload: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    (
        cookie,
        payload["mutation_token"].as_str().unwrap().into(),
        payload["epoch"].as_str().unwrap().into(),
    )
}

async fn setup() -> (PathBuf, Workspace, u16, RunningService) {
    let path = db_path();
    let service = parley_service::start_workspace(&path).await.unwrap();
    let port = service.address.port();
    let owner = owner_session(&service).await;
    SESSIONS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap()
        .insert(port, owner);
    let workspace = Workspace::open(&path).await.unwrap();
    (path, workspace, port, service)
}

#[tokio::test]
async fn s10_owner_identity_tag_is_revisioned_and_idempotent() {
    let (path, workspace, port, service) = setup().await;
    let body = json!({"scope":"owner","expected_revision":1,"tag":"systems design"});
    let (status, first) = request(
        port,
        "POST",
        "/api/v1/identity-tags",
        Some(body.clone()),
        "s10-tag",
    )
    .await;
    assert_eq!(status, 200, "{first:?}");
    assert_eq!(first["revision"], 2);
    let (status, replay) =
        request(port, "POST", "/api/v1/identity-tags", Some(body), "s10-tag").await;
    assert_eq!(status, 200, "{replay:?}");
    assert_eq!(first, replay);
    let (status, stale) = request(
        port,
        "POST",
        "/api/v1/identity-tags",
        Some(json!({"scope":"owner","expected_revision":1,"tag":"product ops"})),
        "s10-tag-stale",
    )
    .await;
    assert_eq!(status, 409, "{stale:?}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM owner_identity_tag")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s10_hook_metadata_requires_current_reviewed_claim_revision() {
    let (path, workspace, port, service) = setup().await;
    let party = workspace
        .create_entity("s10-hook-party", "person", "Dana Lee")
        .await
        .unwrap();
    sqlx::query("INSERT INTO claim(id,subject_id,predicate,current_revision,created_at) VALUES('s10-hook-claim',?,'hook:authored_content',1,'2026-11-20T00:00:00Z')").bind(&party.id).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO claim_revision(claim_id,revision,wording,knowledge_kind,tier,speaker_kind,speaker_entity_id,approved_wording,confirmed_by_owner,created_at) VALUES('s10-hook-claim',1,'Dana published an engineering update','owner_report',3,'owner',NULL,'Dana published an engineering update',1,'2026-11-20T00:00:00Z')").execute(workspace.pool()).await.unwrap();
    let body = json!({"expected_revision":1,"published_at":"2026-11-19T00:00:00Z","publicness":"professional_public"});
    let (status, accepted) = request(
        port,
        "POST",
        "/api/v1/claims/s10-hook-claim/hook",
        Some(body.clone()),
        "s10-hook",
    )
    .await;
    assert_eq!(status, 200, "{accepted:?}");
    assert_eq!(accepted["accepted"], true);
    let (status, replay) = request(
        port,
        "POST",
        "/api/v1/claims/s10-hook-claim/hook",
        Some(body),
        "s10-hook",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(accepted, replay);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accepted_hook WHERE claim_id='s10-hook-claim'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(count, 1);
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s10_queue_5000_parties_meets_render_budget() {
    let (path, workspace, port, service) = setup().await;
    let now = "2026-11-20T12:00:00.000Z";
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<5000) INSERT INTO entity(id,kind,name,revision,created_at,updated_at) SELECT printf('s10-perf-party-%05d',i),'person',printf('Person %05d',i),1,?,? FROM n").bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    sqlx::query(
        "INSERT INTO party_policy(party_id) SELECT id FROM entity WHERE id LIKE 's10-perf-party-%'",
    )
    .execute(workspace.pool())
    .await
    .unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<5000) INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) SELECT printf('s10-perf-op-%05d',i),printf('Opportunity %05d',i),'open',1,?,? FROM n").bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) SELECT o.id,e.id,'primary',? FROM opportunity o JOIN entity e ON e.id=replace(o.id,'s10-perf-op-','s10-perf-party-') WHERE o.id LIKE 's10-perf-op-%'").bind(now).execute(workspace.pool()).await.unwrap();
    let cold_start = std::time::Instant::now();
    let (cold_status, cold_body) =
        request(port, "GET", "/api/v1/queue", None, "s10-perf-cold").await;
    let cold_elapsed = cold_start.elapsed();
    assert_eq!(cold_status, 200, "{cold_body:?}");
    assert_eq!(
        cold_body["actionable"].as_array().unwrap().len()
            + cold_body["blocked"].as_array().unwrap().len(),
        5000
    );
    let mut samples = Vec::new();
    for i in 0..20 {
        let start = std::time::Instant::now();
        let (status, body) =
            request(port, "GET", "/api/v1/queue", None, &format!("s10-perf-{i}")).await;
        samples.push(start.elapsed());
        assert_eq!(status, 200, "{body:?}");
        assert_eq!(
            body["actionable"].as_array().unwrap().len()
                + body["blocked"].as_array().unwrap().len(),
            5000
        );
    }
    samples.sort();
    let p95 = samples[18];
    let build_profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    println!(
        "S10 queue benchmark profile: {build_profile} build, cold first read={cold_elapsed:?}; 20 warm loopback HTTP reads, 5,000 open opportunities/parties, response JSON parsed; warm p95={p95:?}, host {}",
        std::env::consts::OS
    );
    assert!(
        p95 < std::time::Duration::from_millis(200),
        "Queue p95 {p95:?}, samples={samples:?}"
    );
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s10_weekly_coach_enqueues_versioned_job_and_reports_event_fence() {
    let (path, workspace, port, service) = setup().await;
    let (status, job) = request(
        port,
        "POST",
        "/api/v1/coach/weekly",
        Some(json!({"period":"2026-W47"})),
        "s10-coach-weekly",
    )
    .await;
    assert_eq!(status, 202, "{job:?}");
    assert!(job["job_id"].as_str().is_some());
    assert!(job["event_cursor"].as_str().is_some());
    let role: String = sqlx::query_scalar("SELECT role FROM durable_job WHERE id=?")
        .bind(job["job_id"].as_str().unwrap())
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(role, "coach");
    let (status, report) =
        request(port, "GET", "/api/v1/coach/weekly", None, "s10-coach-get").await;
    assert_eq!(status, 200, "{report:?}");
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s10_answer_routes_through_interviewer_without_direct_promotion() {
    let (path, workspace, port, service) = setup().await;
    let now = "2026-11-20T12:00:00.000Z";
    sqlx::query("INSERT INTO durable_job(id,role,status,input_json,input_sha256,scope_kind,scope_id,scope_revision,current_attempt,max_attempts,created_at,updated_at) VALUES('coach-seed','coach','succeeded','{}','hash','owner','weekly',1,1,3,?,?)").bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO coach_question(id,job_id,question_key,kind,target_json,answer_schema_json,priority,impact,text,status,created_at,updated_at) VALUES('q-proof','coach-seed','missing_proof:segment','missing_proof','{}','{\"type\":\"object\"}',0,1,'What result did you achieve?','open',?,?)").bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO agent_chat(id,role,scope_kind,asset_kind,revision,created_at,updated_at) VALUES('chat-proof','interviewer','owner','proof',1,?,?)").bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    let (status,result)=request(port,"POST","/api/v1/coach/questions/q-proof/answer",Some(json!({"answer":"I reduced onboarding time from 9 days to 3.","interviewer_chat_id":"chat-proof"})),"s10-proof-answer").await;
    assert_eq!(status, 202, "{result:?}");
    assert!(result["job_id"].as_str().is_some());
    let turns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent_turn WHERE chat_id='chat-proof' AND speaker='owner'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(turns, 1);
    let promoted: i64 = sqlx::query_scalar("SELECT count(*) FROM resource")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(promoted, 0);
    let question: String =
        sqlx::query_scalar("SELECT status FROM coach_question WHERE id='q-proof'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(question, "answered");
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s10_coach_gap_candidates_use_per_segment_calibrated_coverage() {
    let (path, workspace, port, service) = setup().await;
    let now = "2026-11-20T12:00:00.000Z";
    for (sid, name, n) in [
        ("segment-high", "High impact", 3),
        ("segment-low", "Low impact", 1),
    ] {
        sqlx::query(
            "INSERT INTO segment(id,name,revision,created_at,updated_at) VALUES(?,?,1,?,?)",
        )
        .bind(sid)
        .bind(name)
        .bind(now)
        .bind(now)
        .execute(workspace.pool())
        .await
        .unwrap();
        for i in 0..n {
            let party = format!("{sid}-party-{i}");
            let op = format!("{sid}-op-{i}");
            let party_result = workspace
                .create_entity(&party, "person", &format!("Person {sid} {i}"))
                .await
                .unwrap();
            sqlx::query("INSERT INTO segment_party(segment_id,party_id,created_at) VALUES(?,?,?)")
                .bind(sid)
                .bind(&party_result.id)
                .bind(now)
                .execute(workspace.pool())
                .await
                .unwrap();
            sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES(?,?,'open',1,?,?)").bind(&op).bind(&op).bind(now).bind(now).execute(workspace.pool()).await.unwrap();
            sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES(?,?,'primary',?)").bind(&op).bind(&party_result.id).bind(now).execute(workspace.pool()).await.unwrap();
            sqlx::query("INSERT INTO board_placement(opportunity_id,list_id,position,revision) VALUES(?,'inbox',?,1)").bind(&op).bind((i+1+if sid == "segment-high" { 0 } else { 3 })*1024).execute(workspace.pool()).await.unwrap();
        }
    }
    let (status, job) = request(
        port,
        "POST",
        "/api/v1/coach/weekly",
        Some(json!({"period":"2026-W47"})),
        "s10-gap-order",
    )
    .await;
    assert_eq!(status, 202, "{job:?}");
    let input: String = sqlx::query_scalar("SELECT input_json FROM durable_job WHERE id=?")
        .bind(job["job_id"].as_str().unwrap())
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    let input: Value = serde_json::from_str(&input).unwrap();
    let candidates = input["candidates"].as_array().unwrap();
    assert!(!candidates.is_empty());
    assert_eq!(candidates[0]["impact"], 3);
    assert_eq!(candidates[0]["target_ref"]["segment_id"], "segment-high");
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s10_coach_question_cap_and_priority_are_code_owned() {
    let (path, workspace, port, service) = setup().await;
    let now = "2026-11-20T12:00:00.000Z";
    sqlx::query("INSERT INTO durable_job(id,role,status,input_json,input_sha256,scope_kind,scope_id,scope_revision,current_attempt,max_attempts,created_at,updated_at) VALUES('coach-cap','coach','succeeded','{}','hash','owner','weekly',1,1,3,?,?)").bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    for i in 0..5 {
        sqlx::query("INSERT INTO coach_question(id,job_id,question_key,kind,target_json,answer_schema_json,priority,impact,text,status,created_at,updated_at) VALUES(?,?,?,?,?,'{}',?,?,?,'open',?,?)").bind(format!("cap-{i}")).bind("coach-cap").bind(format!("key-{i}")).bind("insight").bind("{}").bind(i).bind(5-i).bind(format!("Question {i}")).bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    }
    let (status, list) = request(
        port,
        "GET",
        "/api/v1/coach/questions",
        None,
        "s10-coach-list",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(list["open_count"], 5);
    assert_eq!(list["max_open"], 5);
    assert_eq!(list["items"][0]["id"], "cap-0");
    let (status, error) = request(
        port,
        "POST",
        "/api/v1/coach/weekly",
        Some(json!({"period":"2026-W47"})),
        "s10-coach-cap-generate",
    )
    .await;
    assert_eq!(status, 409, "{error:?}");
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s10_question_disposition_is_idempotent_and_scoped() {
    let (path, workspace, port, service) = setup().await;
    let now = "2026-11-20T12:00:00.000Z";
    sqlx::query("INSERT INTO durable_job(id,role,status,input_json,input_sha256,scope_kind,scope_id,scope_revision,current_attempt,max_attempts,created_at,updated_at) VALUES('coach-disposition','coach','succeeded','{}','hash','owner','weekly',1,1,3,?,?)").bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    for id in ["q-disposition", "q-unrelated"] {
        sqlx::query("INSERT INTO coach_question(id,job_id,question_key,kind,target_json,answer_schema_json,priority,impact,text,status,created_at,updated_at) VALUES(?,? ,?,'insight','{}',?,0,1,'Question?','open',?,?)").bind(id).bind("coach-disposition").bind(id).bind("{\"type\":\"object\",\"required\":[\"note\"],\"properties\":{\"note\":{\"type\":\"string\"}}}").bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    }
    let body = json!({"answer":{"note":"The comparison remains inconclusive."}});
    let (status, first) = request(
        port,
        "POST",
        "/api/v1/coach/questions/q-disposition/answer",
        Some(body),
        "s10-question-answer",
    )
    .await;
    assert_eq!(status, 200, "{first:?}");
    let (status, replay) = request(
        port,
        "POST",
        "/api/v1/coach/questions/q-disposition/answer",
        Some(json!({"answer":{"note":"The comparison remains inconclusive."}})),
        "s10-question-answer",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(first, replay);
    let (status, dismissed) = request(
        port,
        "POST",
        "/api/v1/coach/questions/q-unrelated/dismiss",
        None,
        "s10-question-dismiss",
    )
    .await;
    assert_eq!(status, 200, "{dismissed:?}");
    let state1: String =
        sqlx::query_scalar("SELECT status FROM coach_question WHERE id='q-disposition'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    let state2: String =
        sqlx::query_scalar("SELECT status FROM coach_question WHERE id='q-unrelated'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(state1, "answered");
    assert_eq!(state2, "dismissed");
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}
