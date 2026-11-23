// R03: retirement lifecycle, board-save edge preservation, retired-endpoint
// guards and opportunity-board segregation (amendment §5/§7).
use parley_service::RunningService;
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
fn path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-r03-http-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

type Session = (String, String, String);

async fn owner_session(service: &RunningService) -> Session {
    let address = service.address;
    let body = json!({"secret": service.bootstrap_secret}).to_string();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(format!("POST /api/v1/session/bootstrap HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
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
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(format!("GET /api/v1/session HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nCookie: {cookie}\r\nContent-Length: 0\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let payload: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    (
        cookie,
        payload["mutation_token"].as_str().unwrap().to_owned(),
        payload["epoch"].as_str().unwrap().to_owned(),
    )
}

async fn request(
    service: &RunningService,
    session: &Session,
    method: &str,
    path: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> (u16, Value) {
    let address = service.address;
    let body_text = body.map(|value| value.to_string()).unwrap_or_default();
    let mut headers = format!(
        "Cookie: {}\r\nX-Parley-Token: {}\r\nX-Parley-Epoch: {}",
        session.0, session.1, session.2
    );
    for (name, value) in extra {
        headers.push_str(&format!("\r\n{name}: {value}"));
    }
    if !body_text.is_empty() {
        headers.push_str("\r\nContent-Type: application/json");
    }
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{headers}\r\nContent-Length: {}\r\n\r\n{body_text}", body_text.len()).as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let status: u16 = response.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body: Value = if response.contains("\r\n\r\n") {
        serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    (status, body)
}

fn next_key() -> String {
    format!("r03-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

async fn create_card(
    service: &RunningService,
    session: &Session,
    title: &str,
    subtype: &str,
    column: &str,
) -> String {
    let (status, value) = request(
        service,
        session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {"title": title, "subtype": subtype, "body": {}, "placement": {"column_id": column}}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 201, "{value}");
    value["entity"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn r03_retire_removes_from_ordinary_use_and_guards_endpoints() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let card_id = create_card(&service, &session, "Retire me", "note", "inbox").await;

    let (status, result) = request(
        &service,
        &session,
        "POST",
        &format!("/api/v1/resource-cards/{card_id}/retire"),
        None,
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["entity"]["readiness"], "retired");

    // Gone from the board and the picker.
    let (_, board) = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board?include_offboard=true",
        None,
        &[],
    )
    .await;
    assert!(board["cards"].as_array().unwrap().is_empty());
    assert!(board["off_board"].as_array().unwrap().is_empty());

    // Reads and edits of a retired card fail closed.
    let (status, _) = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/resource-cards/{card_id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": card_id, "expected_revision": 2, "expected_notes_rev": 1, "title": "nope"}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 404);

    // A retired card cannot become an edge endpoint.
    let other = create_card(&service, &session, "Active", "note", "inbox").await;
    let (status, _) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": other, "expected_revision": 1, "expected_notes_rev": 1},
            "relation_add": {"verb": "refers to", "target_id": card_id, "basis": "basis"}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 404);

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r03_board_save_preserves_relationship_collection() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let root = create_card(&service, &session, "Checklist", "artifact", "developing").await;
    let (status, related) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": root, "expected_revision": 1, "expected_notes_rev": 1},
            "relation_add": {"verb": "includes", "create": {"title": "Fixture", "subtype": "artifact", "body": {}}, "basis": "owner link"}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 200, "{related}");

    // Board edit without a relationship intent must not serialize edges.
    let (status, _) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": root, "expected_revision": 1, "expected_notes_rev": 1, "title": "Checklist renamed"}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 200);
    let (_, board) = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board",
        None,
        &[],
    )
    .await;
    let card = board["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["resource_id"] == root)
        .unwrap();
    assert_eq!(card["counts"]["connections"], 1, "{board}");
    let (_, dossier) = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{root}/dossier"),
        None,
        &[],
    )
    .await;
    assert_eq!(
        dossier["data"]["direct_links"].as_array().unwrap().len(),
        1,
        "{dossier}"
    );

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r03_resource_moves_never_touch_the_opportunity_board() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let card_id = create_card(&service, &session, "Resource card", "note", "inbox").await;

    let (status, before) = request(&service, &session, "GET", "/api/v1/board", None, &[]).await;
    assert_eq!(status, 200);
    let (status, _) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-board/moves",
        Some(json!({"resource_id": card_id, "destination_column_id": "available"})),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 200);
    let (_, after) = request(&service, &session, "GET", "/api/v1/board", None, &[]).await;
    assert_eq!(
        before, after,
        "resource moves must not mutate the Queue board"
    );

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}
