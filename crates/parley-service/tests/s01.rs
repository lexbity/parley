use std::{path::PathBuf, sync::Arc};

use serde_json::{Value, json};
use sqlx::sqlite::SqlitePoolOptions;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Barrier,
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

async fn owner_session(address: std::net::SocketAddr, bootstrap: &str) -> (String, String, String) {
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

#[tokio::test]
async fn s01_health_no_model_probe() {
    let path = workspace_path();
    let service = parley_service::start_workspace(&path)
        .await
        .expect("start service");
    let response = request(service.address, "GET", "/api/v1/health", &[], "", false).await;
    assert_eq!(status(&response), 200);
    assert_eq!(json_body(&response)["model"], "not-probed");
    service.shutdown().await;
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
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
    service.shutdown().await;
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn s01_resource_create_receipt_atomic() {
    let path = workspace_path();
    let service = parley_service::start_workspace(&path)
        .await
        .expect("start service");
    let (cookie, token, epoch) = owner_session(service.address, &service.bootstrap_secret).await;
    let headers = Arc::new(vec![
        ("Cookie".to_owned(), cookie),
        ("X-Parley-Token".to_owned(), token),
        ("X-Parley-Epoch".to_owned(), epoch),
        (
            "Idempotency-Key".to_owned(),
            "01935f20-7c00-7000-8000-000000000001".to_owned(),
        ),
    ]);
    let body = Arc::new(json!({"kind":"gift","title":"Synthetic gift","body":{"description":"canary-private-value","delivery":"on_request"}}).to_string());
    let duplicate_json = request(
        service.address,
        "POST",
        "/api/v1/entities",
        &[
            ("Cookie", headers[0].1.as_str()),
            ("X-Parley-Token", headers[1].1.as_str()),
            ("X-Parley-Epoch", headers[2].1.as_str()),
            ("Idempotency-Key", "01935f20-7c00-7000-8000-000000000099"),
        ],
        r#"{"kind":"gift","kind":"proof","title":"bad","body":{}}"#,
        false,
    )
    .await;
    assert_eq!(status(&duplicate_json), 400);
    // Commit the first operation while deliberately discarding its response,
    // then race 32 retries of the exact frozen command.
    let borrowed: Vec<(&str, &str)> = headers
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
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
            let borrowed: Vec<(&str, &str)> = headers
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
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
        .connect(&format!("sqlite://{}", path.display()))
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
        &[
            ("Cookie", headers[0].1.as_str()),
            ("X-Parley-Token", headers[1].1.as_str()),
            ("X-Parley-Epoch", headers[2].1.as_str()),
        ],
        "",
        false,
    )
    .await;
    let resource = json_body(&read)["data"][0].clone();
    let detail = request(
        service.address,
        "GET",
        &format!("/api/v1/entities/{}", resource["id"].as_str().unwrap()),
        &[
            ("Cookie", headers[0].1.as_str()),
            ("X-Parley-Token", headers[1].1.as_str()),
            ("X-Parley-Epoch", headers[2].1.as_str()),
        ],
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
        &[
            ("Cookie", headers[0].1.as_str()),
            ("X-Parley-Token", headers[1].1.as_str()),
            ("X-Parley-Epoch", headers[2].1.as_str()),
        ],
        "",
        false,
    )
    .await;
    assert_eq!(status(&events), 200);
    assert_eq!(json_body(&events)["items"].as_array().unwrap().len(), 1);
    let etag = resource["etag"].as_str().unwrap().to_owned();
    let update = json!({"title":"Changed","body":{"description":"new text"}}).to_string();
    let updated = request(
        service.address,
        "PATCH",
        &format!("/api/v1/entities/{}", resource["id"].as_str().unwrap()),
        &[
            ("Cookie", headers[0].1.as_str()),
            ("X-Parley-Token", headers[1].1.as_str()),
            ("X-Parley-Epoch", headers[2].1.as_str()),
            ("Idempotency-Key", "01935f20-7c00-7000-8000-000000000002"),
            ("If-Match", &etag),
        ],
        &update,
        false,
    )
    .await;
    assert_eq!(status(&updated), 200);
    assert_eq!(json_body(&updated)["data"]["revision"], 2);
    let stale = request(
        service.address,
        "PATCH",
        &format!("/api/v1/entities/{}", resource["id"].as_str().unwrap()),
        &[
            ("Cookie", headers[0].1.as_str()),
            ("X-Parley-Token", headers[1].1.as_str()),
            ("X-Parley-Epoch", headers[2].1.as_str()),
            ("Idempotency-Key", "01935f20-7c00-7000-8000-000000000003"),
            ("If-Match", &etag),
        ],
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
        &[
            ("Cookie", headers[0].1.as_str()),
            ("X-Parley-Token", headers[1].1.as_str()),
            ("X-Parley-Epoch", headers[2].1.as_str()),
            ("Idempotency-Key", "01935f20-7c00-7000-8000-000000000001"),
        ],
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
    let (cookie, token, epoch) =
        owner_session(restarted.address, &restarted.bootstrap_secret).await;
    let after_restart = request(
        restarted.address,
        "GET",
        "/api/v1/entities?kind=resource",
        &[
            ("Cookie", &cookie),
            ("X-Parley-Token", &token),
            ("X-Parley-Epoch", &epoch),
        ],
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
    restarted.shutdown().await;
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
