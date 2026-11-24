use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

async fn setup() -> (PathBuf, Workspace, RunningService, String, String, String) {
    let path = std::env::temp_dir().join(format!(
        "parley-ar02-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let service = parley_service::start_workspace(&path).await.unwrap();
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
    let token = payload["mutation_token"].as_str().unwrap().to_owned();
    let epoch = payload["epoch"].as_str().unwrap().to_owned();
    let workspace = Workspace::open(&path).await.unwrap();
    (path, workspace, service, cookie, token, epoch)
}

async fn request(
    port: u16,
    auth: (&str, &str, &str),
    method: &str,
    path: &str,
    body: Option<Value>,
    key: &str,
) -> (u16, Value) {
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let (cookie, token, epoch) = auth;
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
    let value: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(payload).unwrap()
    };
    (status, value.get("data").cloned().unwrap_or(value))
}

async fn seed_question(workspace: &Workspace, id: &str, scope: Value, key: &str, status: &str) {
    sqlx::query("INSERT INTO inference_record(id,role,status,created_at,metadata_class) VALUES(?,'coach','succeeded','2026-11-20T00:00:00Z','historical_unknown')")
        .bind(format!("job-{id}")).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO coach_question(id,job_id,question_key,semantic_key,kind,target_json,selector_json,dependency_json,answer_schema_json,priority,impact,text,status,created_at,updated_at) VALUES(?,?,?,NULL,'context_review','{}',?,'[]','{}',0,2,'Which part should be reviewed?',?,?,?)")
        .bind(id).bind(format!("job-{id}")).bind(key).bind(scope.to_string()).bind(status).bind("2026-11-20T00:00:00Z").bind("2026-11-20T00:00:00Z").execute(workspace.pool()).await.unwrap();
}

#[tokio::test]
async fn ar02_context_distinguishes_no_match_from_missing_and_stale() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let party = workspace
        .create_entity("ar02-party", "person", "Casey Example", "")
        .await
        .unwrap();
    let (status, stale) = request(
        service.address.port(),
        (&cookie, &token, &epoch),
        "POST",
        "/api/v1/coach/context",
        Some(json!({"scope":{"kind":"party","id":party.id,"revision":party.revision+1}})),
        "ar02-stale-context",
    )
    .await;
    assert_eq!(status, 409, "{stale:?}");
    assert_eq!(stale["error"]["code"], "STALE_INPUT");
    let (status, missing) = request(service.address.port(), (&cookie, &token, &epoch), "POST", "/api/v1/coach/context", Some(json!({"scope":{"kind":"opportunity","id":"foreign-opportunity","revision":1,"recipient_id":party.id,"recipient_revision":party.revision}})), "ar02-foreign-opportunity").await;
    assert_eq!(status, 404, "{missing:?}");
    assert_eq!(missing["error"]["code"], "TARGET_UNAVAILABLE");
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn ar02_concurrent_question_dispositions_keep_one_open_key() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let party = workspace
        .create_entity("ar02-question-party", "person", "Riley Example", "")
        .await
        .unwrap();
    let key = "a".repeat(64);
    seed_question(
        &workspace,
        "ar02-question",
        json!({"kind":"party","id":party.id,"revision":party.revision}),
        &key,
        "open",
    )
    .await;
    seed_question(
        &workspace,
        "ar02-defer-question",
        json!({"kind":"party","id":party.id,"revision":party.revision}),
        &"d".repeat(64),
        "open",
    )
    .await;
    let (status, list) = request(
        service.address.port(),
        (&cookie, &token, &epoch),
        "GET",
        &format!(
            "/api/v1/coach/questions?scope_kind=party&scope_id={}",
            party.id
        ),
        None,
        "ar02-list-question",
    )
    .await;
    assert_eq!(status, 200, "{list:?}");
    assert_eq!(list["open_count"], 2, "{list:?}");
    assert_eq!(list["items"][0]["revision"], 1);
    let (status, stale) = request(
        service.address.port(),
        (&cookie, &token, &epoch),
        "POST",
        "/api/v1/coach/questions/ar02-question/dismiss",
        Some(json!({"expected_revision":2,"question_key":key})),
        "ar02-stale-dismiss",
    )
    .await;
    assert_eq!(status, 409, "{stale:?}");
    assert_eq!(stale["error"]["code"], "QUESTION_STALE");
    let answer_key = key.clone();
    let (answer, dismiss) = tokio::join!(
        request(
            service.address.port(),
            (&cookie, &token, &epoch),
            "POST",
            "/api/v1/coach/questions/ar02-question/answer",
            Some(
                json!({"answer":{"note":"owner response"},"expected_revision":1,"question_key":key})
            ),
            "ar02-racing-answer"
        ),
        request(
            service.address.port(),
            (&cookie, &token, &epoch),
            "POST",
            "/api/v1/coach/questions/ar02-question/dismiss",
            Some(json!({"expected_revision":1,"question_key":answer_key})),
            "ar02-racing-dismiss"
        ),
    );
    assert_eq!(
        [answer.0, dismiss.0]
            .iter()
            .filter(|status| **status == 200)
            .count(),
        1,
        "answer={:?}; dismiss={:?}",
        answer.1,
        dismiss.1
    );
    assert_eq!(
        [answer.0, dismiss.0]
            .iter()
            .filter(|status| **status == 409)
            .count(),
        1
    );
    let disposition: (String, i64) =
        sqlx::query_as("SELECT status,revision FROM coach_question WHERE id='ar02-question'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert!(matches!(disposition.0.as_str(), "answered" | "dismissed"));
    assert_eq!(disposition.1, 2);
    let (status, invalid_defer) = request(
        service.address.port(),
        (&cookie, &token, &epoch),
        "POST",
        "/api/v1/coach/questions/ar02-defer-question/defer",
        Some(json!({"expected_revision":1,"question_key":"d".repeat(64),"defer_days":31})),
        "ar02-invalid-defer",
    )
    .await;
    assert_eq!(status, 422, "{invalid_defer:?}");
    let (status, deferred) = request(
        service.address.port(),
        (&cookie, &token, &epoch),
        "POST",
        "/api/v1/coach/questions/ar02-defer-question/defer",
        Some(json!({"expected_revision":1,"question_key":"d".repeat(64)})),
        "ar02-defer",
    )
    .await;
    assert_eq!(status, 200, "{deferred:?}");
    assert_eq!(deferred["status"], "deferred");
    let duplicates: i64 = sqlx::query_scalar("SELECT count(*) FROM coach_question WHERE status IN ('open','answer_pending') AND semantic_key='ar02-semantic'").fetch_one(workspace.pool()).await.unwrap();
    assert_eq!(duplicates, 0);
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn ar02_disposition_links_current_context() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let party = workspace
        .create_entity("ar02-disposition-party", "person", "Morgan Example", "")
        .await
        .unwrap();
    let key = "b".repeat(64);
    seed_question(
        &workspace,
        "ar02-disposition-question",
        json!({"kind":"party","id":party.id,"revision":party.revision}),
        &key,
        "open",
    )
    .await;
    let (status, response) = request(
        service.address.port(),
        (&cookie, &token, &epoch),
        "POST",
        "/api/v1/coach/questions/ar02-disposition-question/answer",
        Some(json!({"answer":{"note":"owner answer"},"expected_revision":1,"question_key":key})),
        "ar02-answer",
    )
    .await;
    assert_eq!(status, 200, "{response:?}");
    assert_eq!(response["status"], "answered");
    let (status, replay) = request(
        service.address.port(),
        (&cookie, &token, &epoch),
        "POST",
        "/api/v1/coach/questions/ar02-disposition-question/answer",
        Some(json!({"answer":{"note":"owner answer"},"expected_revision":1,"question_key":key})),
        "ar02-answer",
    )
    .await;
    assert_eq!(status, 200, "{replay:?}");
    assert_eq!(response, replay);
    let row: (String, i64) = sqlx::query_as(
        "SELECT status,revision FROM coach_question WHERE id='ar02-disposition-question'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(row, ("answered".into(), 2));
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn ar02_answer_creates_owner_interviewer_and_waits_for_review() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let party = workspace
        .create_entity("ar02-interview-party", "person", "Jordan Example", "")
        .await
        .unwrap();
    let key = "c".repeat(64);
    seed_question(
        &workspace,
        "ar02-interview-question",
        json!({"kind":"party","id":party.id,"revision":party.revision}),
        &key,
        "open",
    )
    .await;
    sqlx::query("UPDATE coach_question SET kind='missing_proof',answer_schema_json='{}' WHERE id='ar02-interview-question'").execute(workspace.pool()).await.unwrap();
    let (status, response) = request(service.address.port(), (&cookie, &token, &epoch), "POST", "/api/v1/coach/questions/ar02-interview-question/answer", Some(json!({"answer":{"text":"I measured the result in the onboarding report."},"expected_revision":1,"question_key":key})), "ar02-interview-answer").await;
    assert_eq!(status, 202, "{response:?}");
    assert_eq!(response["status"], "answer_pending");
    let chat_id = response["chat_id"].as_str().unwrap();
    let chat: (String, String, String) =
        sqlx::query_as("SELECT role,scope_kind,asset_kind FROM agent_chat WHERE id=?")
            .bind(chat_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(chat, ("interviewer".into(), "owner".into(), "proof".into()));
    let question: (String, i64) = sqlx::query_as(
        "SELECT status,revision FROM coach_question WHERE id='ar02-interview-question'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(question, ("answer_pending".into(), 2));
    let promoted: i64 = sqlx::query_scalar("SELECT count(*) FROM entity WHERE subtype<>''")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(promoted, 0);
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}
