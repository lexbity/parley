use std::{path::PathBuf, sync::Arc};

use parley_service::RunningService;
use serde_json::{Value, json};
use sqlx::sqlite::SqlitePoolOptions;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Barrier,
    time::{Duration, sleep},
};

fn workspace_path() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "parley-s01-{}-{}.sqlite",
        std::process::id(),
        rand::random::<u64>()
    ));
    let directory = path.with_extension("workspace");
    std::fs::create_dir_all(&directory).expect("create private test workspace directory");
    directory.join("workspace.sqlite")
}

async fn request(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
    drop_reply: bool,
) -> String {
    let mut stream = TcpStream::connect(address)
        .await
        .expect("connect to loopback service");
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    stream.flush().await.expect("flush request");
    if drop_reply {
        stream.shutdown().await.expect("drop the reply transport");
        return String::new();
    }
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read response");
    String::from_utf8(response).expect("HTTP response is UTF-8")
}

fn status(response: &str) -> u16 {
    response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .expect("HTTP status")
}

fn json_body(response: &str) -> Value {
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    serde_json::from_str(body).expect("JSON response body")
}

type Session = (String, String, String);

async fn owner_session(address: std::net::SocketAddr, bootstrap: &str) -> Session {
    let response = request(
        address,
        "POST",
        "/api/v1/session/bootstrap",
        &[],
        &json!({"secret":bootstrap}).to_string(),
        false,
    )
    .await;
    assert_eq!(status(&response), 200);
    let cookie = response
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|line| {
            line.split_once(':').map(|(_, value)| {
                value
                    .trim()
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
        })
        .expect("session cookie");
    let response = request(
        address,
        "GET",
        "/api/v1/session",
        &[("Cookie", &cookie)],
        "",
        false,
    )
    .await;
    assert_eq!(status(&response), 200);
    let value = json_body(&response);
    (
        cookie,
        value["mutation_token"].as_str().unwrap().to_owned(),
        value["epoch"].as_str().unwrap().to_owned(),
    )
}

fn session_headers(session: &Session) -> Vec<(&str, &str)> {
    vec![
        ("Cookie", session.0.as_str()),
        ("X-Parley-Token", session.1.as_str()),
        ("X-Parley-Epoch", session.2.as_str()),
    ]
}

fn borrowed_refs(headers: &Arc<(Vec<(String, String)>, String)>) -> Vec<(&str, &str)> {
    headers
        .0
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

fn frozen_headers(headers: &Arc<(Vec<(String, String)>, String)>) -> Vec<(&str, &str)> {
    let mut borrowed: Vec<(&str, &str)> = headers
        .0
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    borrowed.push(("Idempotency-Key", &headers.1));
    borrowed
}

async fn stop(service: RunningService, path: &std::path::Path) {
    service.shutdown().await;
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn s01_health_no_model_probe() {
    let path = workspace_path();
    let service = parley_service::start_workspace(&path)
        .await
        .expect("start service");
    let response = request(service.address, "GET", "/api/v1/health", &[], "", false).await;
    assert_eq!(status(&response), 200);
    assert_eq!(json_body(&response)["model"], "not-probed");
    stop(service, &path).await;
}

#[tokio::test]
async fn s01_bootstrap_is_single_use_and_session_is_private() {
    let path = workspace_path();
    let service = parley_service::start_workspace(&path)
        .await
        .expect("start service");
    let invalid = request(service.address, "GET", "/api/v1/session", &[], "", false).await;
    assert_eq!(status(&invalid), 401);
    let cross_origin = request(
        service.address,
        "GET",
        "/api/v1/session",
        &[("Origin", "http://attacker.invalid")],
        "",
        false,
    )
    .await;
    assert_eq!(status(&cross_origin), 403);
    let (cookie, token, epoch) = owner_session(service.address, &service.bootstrap_secret).await;
    let again = request(
        service.address,
        "POST",
        "/api/v1/session/bootstrap",
        &[],
        &json!({"secret":service.bootstrap_secret}).to_string(),
        false,
    )
    .await;
    assert_eq!(status(&again), 401);
    let bad = request(
        service.address,
        "GET",
        "/api/v1/entities?kind=resource",
        &[
            ("Cookie", &cookie),
            ("X-Parley-Token", "wrong"),
            ("X-Parley-Epoch", &epoch),
        ],
        "",
        false,
    )
    .await;
    assert_eq!(status(&bad), 403);
    assert!(!bad.contains(&token));
    stop(service, &path).await;
}

#[tokio::test]
async fn s01_resource_create_receipt_atomic() {
    let path = workspace_path();
    let service = parley_service::start_workspace(&path)
        .await
        .expect("start service");
    let session = owner_session(service.address, &service.bootstrap_secret).await;
    let headers = Arc::new((
        vec![
            ("Cookie".to_owned(), session.0.clone()),
            ("X-Parley-Token".to_owned(), session.1.clone()),
            ("X-Parley-Epoch".to_owned(), session.2.clone()),
        ],
        "01935f20-7c00-7000-8000-000000000001".to_owned(),
    ));
    let body = Arc::new(json!({"kind":"gift","title":"Synthetic gift","body":{"description":"canary-private-value","delivery":"on_request"}}).to_string());
    let duplicate_json = request(
        service.address,
        "POST",
        "/api/v1/entities",
        &[
            ("Cookie", &headers.0[0].1),
            ("X-Parley-Token", &headers.0[1].1),
            ("X-Parley-Epoch", &headers.0[2].1),
            ("Idempotency-Key", "01935f20-7c00-7000-8000-000000000099"),
        ],
        r#"{"kind":"gift","kind":"proof","title":"bad","body":{}}"#,
        false,
    )
    .await;
    assert_eq!(status(&duplicate_json), 400);
    // Commit the first operation while deliberately discarding its response,
    // then race 32 retries of the exact frozen command.
    let borrowed = frozen_headers(&headers);
    let _ = request(
        service.address,
        "POST",
        "/api/v1/entities",
        &borrowed,
        &body,
        true,
    )
    .await;
    let barrier = Arc::new(Barrier::new(32));
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let barrier = barrier.clone();
        let headers = headers.clone();
        let body = body.clone();
        let address = service.address;
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            let borrowed = frozen_headers(&headers);
            request(address, "POST", "/api/v1/entities", &borrowed, &body, false).await
        }));
    }
    let mut id = None;
    for task in tasks {
        let response = task.await.unwrap();
        assert_eq!(status(&response), 201);
        let current = json_body(&response)["data"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(id.get_or_insert(current.clone()), &current);
    }
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!(
            "sqlite://{}",
            parley_service::Workspace::resolve_database_path(&path)
                .unwrap()
                .display()
        ))
        .await
        .unwrap();
    let resources: i64 = sqlx::query_scalar("SELECT count(*) FROM resource")
        .fetch_one(&pool)
        .await
        .unwrap();
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event WHERE event_type='resource.created'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let receipts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM idempotency_receipt WHERE key='01935f20-7c00-7000-8000-000000000001'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((resources, events, receipts), (1, 1, 1));
    let read = request(
        service.address,
        "GET",
        "/api/v1/entities?kind=resource",
        &borrowed_refs(&headers),
        "",
        false,
    )
    .await;
    let resource = json_body(&read)["data"][0].clone();
    let detail = request(
        service.address,
        "GET",
        &format!("/api/v1/entities/{}", resource["id"].as_str().unwrap()),
        &borrowed_refs(&headers),
        "",
        false,
    )
    .await;
    assert_eq!(status(&detail), 200);
    assert_eq!(json_body(&detail)["data"]["id"], resource["id"]);
    let events = request(
        service.address,
        "GET",
        "/api/v1/events?after=0",
        &borrowed_refs(&headers),
        "",
        false,
    )
    .await;
    assert_eq!(status(&events), 200);
    assert_eq!(json_body(&events)["items"].as_array().unwrap().len(), 1);
    let etag = resource["etag"].as_str().unwrap().to_owned();
    let update = json!({"title":"Changed","body":{"description":"new text"}}).to_string();
    let patch_headers = [
        ("Cookie", headers.0[0].1.as_str()),
        ("X-Parley-Token", headers.0[1].1.as_str()),
        ("X-Parley-Epoch", headers.0[2].1.as_str()),
        ("Idempotency-Key", "01935f20-7c00-7000-8000-000000000002"),
        ("If-Match", etag.as_str()),
    ];
    let updated = request(
        service.address,
        "PATCH",
        &format!("/api/v1/entities/{}", resource["id"].as_str().unwrap()),
        &patch_headers,
        &update,
        false,
    )
    .await;
    assert_eq!(status(&updated), 200);
    assert_eq!(json_body(&updated)["data"]["revision"], 2);
    let stale_headers = [
        ("Cookie", headers.0[0].1.as_str()),
        ("X-Parley-Token", headers.0[1].1.as_str()),
        ("X-Parley-Epoch", headers.0[2].1.as_str()),
        ("Idempotency-Key", "01935f20-7c00-7000-8000-000000000003"),
        ("If-Match", etag.as_str()),
    ];
    let stale = request(
        service.address,
        "PATCH",
        &format!("/api/v1/entities/{}", resource["id"].as_str().unwrap()),
        &stale_headers,
        &update,
        false,
    )
    .await;
    assert_eq!(status(&stale), 412);
    let event_payload: String =
        sqlx::query_scalar("SELECT group_concat(payload_json,' ') FROM event")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!event_payload.contains("canary-private-value"));
    let conflict = request(
        service.address,
        "POST",
        "/api/v1/entities",
        &borrowed,
        &json!({"kind":"gift","title":"Different","body":{}}).to_string(),
        false,
    )
    .await;
    assert_eq!(status(&conflict), 409);
    assert!(etag.starts_with("\"p1."));
    pool.close().await;
    service.shutdown().await;
    let restarted = parley_service::start_workspace(&path)
        .await
        .expect("restart service on saved workspace");
    let session = owner_session(restarted.address, &restarted.bootstrap_secret).await;
    let after_restart = request(
        restarted.address,
        "GET",
        "/api/v1/entities?kind=resource",
        &session_headers(&session),
        "",
        false,
    )
    .await;
    assert_eq!(status(&after_restart), 200);
    assert_eq!(
        json_body(&after_restart)["data"].as_array().unwrap().len(),
        1
    );
    assert_eq!(json_body(&after_restart)["data"][0]["revision"], 2);
    stop(restarted, &path).await;
}

#[tokio::test]
async fn s01_lost_ack_retry_returns_one_durable_resource() {
    let path = workspace_path();
    let service = parley_service::start_workspace(&path)
        .await
        .expect("start service");
    let session = owner_session(service.address, &service.bootstrap_secret).await;
    let key = "01935f20-7c00-7000-8000-000000000004";
    let mut headers = session_headers(&session);
    headers.push(("Idempotency-Key", key));
    let body = json!({"kind":"gift","title":"Incomplete offer","body":{"what":"A walkthrough"}})
        .to_string();
    // The commit happens while the response transport is already gone.
    request(
        service.address,
        "POST",
        "/api/v1/entities",
        &headers,
        &body,
        true,
    )
    .await;
    let independent = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!(
            "sqlite://{}",
            parley_service::Workspace::resolve_database_path(&path)
                .unwrap()
                .display()
        ))
        .await
        .unwrap();
    for _ in 0..200 {
        let committed: i64 = sqlx::query_scalar("SELECT count(*) FROM resource")
            .fetch_one(&independent)
            .await
            .unwrap();
        if committed == 1 {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    let retry = request(
        service.address,
        "POST",
        "/api/v1/entities",
        &headers,
        &body,
        false,
    )
    .await;
    assert_eq!(status(&retry), 201);
    let replay = json_body(&retry);
    assert_eq!(replay["data"]["title"], "Incomplete offer");
    let id = replay["data"]["id"].as_str().unwrap().to_owned();
    for (table, query) in [
        ("resource", "SELECT count(*) FROM resource"),
        ("event", "SELECT count(*) FROM event"),
        (
            "idempotency_receipt",
            "SELECT count(*) FROM idempotency_receipt",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(query)
            .fetch_one(&independent)
            .await
            .unwrap();
        assert_eq!(count, 1, "{table} must have one committed row");
    }
    let stored: String = sqlx::query_scalar("SELECT id FROM resource")
        .fetch_one(&independent)
        .await
        .unwrap();
    assert_eq!(
        stored, id,
        "the replayed receipt identifies the committed row"
    );
    independent.close().await;
    stop(service, &path).await;
}

#[tokio::test]
async fn s01_unauthenticated_read_discloses_no_resource_fields() {
    let path = workspace_path();
    let service = parley_service::start_workspace(&path)
        .await
        .expect("start service");
    let session = owner_session(service.address, &service.bootstrap_secret).await;
    let mut headers = session_headers(&session);
    headers.push(("Idempotency-Key", "01935f20-7c00-7000-8000-000000000005"));
    let body =
        json!({"kind":"gift","title":"Private draft","body":{"description":"secret detail"}})
            .to_string();
    let created = request(
        service.address,
        "POST",
        "/api/v1/entities",
        &headers,
        &body,
        false,
    )
    .await;
    assert_eq!(status(&created), 201);
    let id = json_body(&created)["data"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let anonymous = request(
        service.address,
        "GET",
        &format!("/api/v1/entities/{id}"),
        &[],
        "",
        false,
    )
    .await;
    assert_eq!(status(&anonymous), 401);
    assert!(!anonymous.contains("Private draft"));
    assert!(!anonymous.contains("secret detail"));
    assert!(!anonymous.contains("etag"));
    let list = request(
        service.address,
        "GET",
        "/api/v1/entities?kind=resource",
        &[],
        "",
        false,
    )
    .await;
    assert_eq!(status(&list), 401);
    assert!(!list.contains("Private draft"));
    stop(service, &path).await;
}

#[tokio::test]
async fn s01_capture_retry_is_one_occurrence() {
    let path = workspace_path();
    let service = parley_service::start_workspace(&path)
        .await
        .expect("start service");
    let session = owner_session(service.address, &service.bootstrap_secret).await;
    let mut headers = session_headers(&session);
    let key = "01935f20-7c00-7000-8000-0000000000ff";
    headers.push(("Idempotency-Key", key));
    let body = json!({"text":"The onboarding team is expanding."}).to_string();
    let first = request(
        service.address,
        "POST",
        "/api/v1/captures",
        &headers,
        &body,
        false,
    )
    .await;
    assert_eq!(status(&first), 201, "{first}");
    let second = request(
        service.address,
        "POST",
        "/api/v1/captures",
        &headers,
        &body,
        false,
    )
    .await;
    assert_eq!(status(&second), 201, "{second}");
    assert_eq!(json_body(&first), json_body(&second));

    let db = SqlitePoolOptions::new()
        .connect(&format!(
            "sqlite://{}",
            parley_service::Workspace::resolve_database_path(&path)
                .unwrap()
                .display()
        ))
        .await
        .unwrap();
    let occurrences: i64 = sqlx::query_scalar("SELECT count(*) FROM source_occurrence")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(occurrences, 1);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM durable_job")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event WHERE event_type='source.captured'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(events, 1);
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_receipt")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(receipts, 1);
    db.close().await;
    stop(service, &path).await;
}

#[tokio::test]
async fn s01_capture_concurrent_same_key_retry_returns_one_occurrence() {
    // BQ1/B-FR-1: two in-flight POSTs with the same idempotency key must
    // produce exactly one occurrence, one job, one event and one receipt —
    // both callers receive the identical original response (the loser
    // replays the winner's committed result, never an error).
    let path = workspace_path();
    let service = parley_service::start_workspace(&path)
        .await
        .expect("start service");
    let session = owner_session(service.address, &service.bootstrap_secret).await;
    let mut headers_owned: Vec<(String, String)> = session_headers(&session)
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    let key = "01935f20-7c00-7000-8000-000000000101";
    headers_owned.push(("Idempotency-Key".to_owned(), key.to_owned()));
    let body = json!({"text":"Concurrent onboarding capture."}).to_string();

    // Eight in-flight same-key requests: at least one pair will race the
    // replay/commit boundary. Every caller must receive the identical
    // committed response — never an error from a lost PK race.
    let mut responses = Vec::new();
    for _ in 0..8 {
        let address = service.address;
        let headers = headers_owned.clone();
        let body = body.clone();
        responses.push(tokio::spawn(async move {
            let refs: Vec<(&str, &str)> = headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            request(address, "POST", "/api/v1/captures", &refs, &body, false).await
        }));
    }
    let mut bodies = Vec::new();
    for handle in responses {
        let response = handle.await.unwrap();
        assert_eq!(status(&response), 201, "{response}");
        bodies.push(json_body(&response));
    }
    let first_body = &bodies[0];
    for body in &bodies[1..] {
        assert_eq!(
            body, first_body,
            "every concurrent same-key caller must receive the identical response"
        );
    }

    let db = SqlitePoolOptions::new()
        .connect(&format!(
            "sqlite://{}",
            parley_service::Workspace::resolve_database_path(&path)
                .unwrap()
                .display()
        ))
        .await
        .unwrap();
    for (label, sql) in [
        ("occurrences", "SELECT count(*) FROM source_occurrence"),
        ("jobs", "SELECT count(*) FROM durable_job"),
        (
            "events",
            "SELECT count(*) FROM event WHERE event_type='source.captured'",
        ),
        ("receipts", "SELECT count(*) FROM idempotency_receipt"),
    ] {
        let count: i64 = sqlx::query_scalar(sql).fetch_one(&db).await.unwrap();
        assert_eq!(
            count, 1,
            "{label} count must be exactly one after a concurrent same-key retry"
        );
    }
    db.close().await;
    stop(service, &path).await;
}
