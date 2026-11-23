// R04: cross-tab party linking and stale-order movement guards (amendment
// §5, §7).
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
        "parley-r04-http-{}-{}.sqlite",
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
    format!("r04-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

#[tokio::test]
async fn r04_link_existing_party_preserves_its_record() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;

    let (status, card) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {"title": "Checklist", "subtype": "artifact", "body": {}, "placement": {"column_id": "developing"}}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 201);
    let root_id = card["entity"]["id"].as_str().unwrap().to_owned();

    let (status, party) = request(
        &service,
        &session,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind": "person", "name": "Mira Chen", "focus": ""})),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 201);
    let party_id = party["data"]["id"].as_str().unwrap().to_owned();

    // Link the existing party by ID; no new person or resource tile appears.
    // Delivery-asserting verbs are reserved: the generic editor must not be
    // able to manufacture a "shared with" edge (Resources spec §5).
    let (status, link) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": root_id, "expected_revision": 1, "expected_notes_rev": 1},
            "relation_add": {"verb": "refers to", "target_id": party_id, "basis": "owner-recorded link to Mira"}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 200, "{link}");
    assert_eq!(link["created_related"]["to_id"], party_id);

    let (_, persons) = request(
        &service,
        &session,
        "GET",
        "/api/v1/entities?kind=person",
        None,
        &[],
    )
    .await;
    assert_eq!(persons["data"].as_array().unwrap().len(), 1);

    let (rejected_status, rejected) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": root_id, "expected_revision": 1, "expected_notes_rev": 1},
            "relation_add": {"verb": "shared with", "target_id": party_id, "basis": "owner-recorded send to Mira"}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(rejected_status, 422, "{rejected}");
    assert_eq!(
        rejected["error"]["code"].as_str().unwrap_or_default(),
        "INVALID_REQUEST",
        "{rejected}"
    );
    assert!(
        rejected["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("cannot assert a delivery"),
        "{rejected}"
    );

    let (_, dossier) = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{party_id}/dossier"),
        None,
        &[],
    )
    .await;
    let links = dossier["data"]["direct_links"].as_array().unwrap();
    assert!(
        links
            .iter()
            .any(|edge| edge["from_id"] == root_id && edge["verb"] == "refers to"),
        "{dossier}"
    );
    assert!(
        !links.iter().any(|edge| edge["verb"] == "shared with"),
        "{dossier}"
    );

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r04_stale_order_command_is_rejected_with_412() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;

    let (_, card) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {"title": "Ordered", "subtype": "note", "body": {}, "placement": {"column_id": "inbox"}}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    let card_id = card["entity"]["id"].as_str().unwrap().to_owned();

    // First move advances the placement revision.
    let (status, first) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-board/moves",
        Some(json!({"resource_id": card_id, "destination_column_id": "developing"})),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 200);
    let stale_placement_revision = first["placement"]["revision"].as_i64().unwrap() - 1;

    // A replay with the now-stale placement revision is rejected, not applied.
    let (status, _) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-board/moves",
        Some(json!({
            "resource_id": card_id,
            "destination_column_id": "available",
            "expected_placement_revision": stale_placement_revision
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 412);

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}
