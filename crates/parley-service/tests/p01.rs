// P01: Party sheet rename, typed focus edits, entity notes and the inspector
// dossier (amendment 2026-11-22; RFC-0004 wire conventions).
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
        "parley-p01-http-{}-{}.sqlite",
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
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let payload: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    (
        cookie,
        payload["mutation_token"].as_str().unwrap().to_owned(),
        payload["epoch"].as_str().unwrap().to_owned(),
    )
}

#[derive(Debug)]
struct RawResponse {
    status: u16,
    body: Value,
}

async fn request(
    service: &RunningService,
    session: &Session,
    method: &str,
    path: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> RawResponse {
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
    let status: u16 = response
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let body: Value = if response.contains("\r\n\r\n") {
        serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    RawResponse { status, body }
}

fn next_key() -> String {
    format!("p01-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Create a person/org entity and return (id, etag).
async fn create_party(
    service: &RunningService,
    session: &Session,
    kind: &str,
    name: &str,
    focus: &str,
) -> (String, String) {
    let response = request(
        service,
        session,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind": kind, "name": name, "focus": focus})),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(response.status, 201, "{response:?}");
    let data = &response.body["data"];
    (
        data["id"].as_str().unwrap().to_owned(),
        data["etag"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn p01_session_advertises_party_capabilities() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let response = request(&service, &session, "GET", "/api/v1/session", None, &[]).await;
    assert_eq!(response.status, 200);
    let capabilities = response.body["capabilities"].as_array().unwrap();
    for expected in ["party.sheet", "party.notes", "party.dossier"] {
        assert!(
            capabilities.iter().any(|item| item.as_str() == Some(expected)),
            "missing capability {expected}: {capabilities:?}"
        );
    }
    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn p01_party_sheet_focus_edit_discards_stale_and_blank_writes() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let (id, first_etag) = create_party(&service, &session, "person", "Dana Lee", "Maintenance").await;

    // Directory rows carry the editable focus and the strong entity ETag.
    let list = request(
        &service,
        &session,
        "GET",
        "/api/v1/entities?kind=person&kind=org",
        None,
        &[],
    )
    .await;
    assert_eq!(list.status, 200);
    let rows = list.body["data"].as_array().unwrap();
    let row = rows.iter().find(|row| row["id"] == id).unwrap();
    assert_eq!(row["focus"], "Maintenance");
    assert_eq!(row["etag"], first_etag);

    // Typed focus PATCH under the current ETag.
    let updated = request(
        &service,
        &session,
        "PATCH",
        &format!("/api/v1/entities/{id}"),
        Some(json!({"focus": "CSV tools"})),
        &[
            ("If-Match", first_etag.as_str()),
            ("Idempotency-Key", next_key().as_str()),
        ],
    )
    .await;
    assert_eq!(updated.status, 200, "{updated:?}");
    let data = &updated.body["data"];
    assert_eq!(data["focus"], "CSV tools");
    assert_eq!(data["revision"], 2);
    let second_etag = data["etag"].as_str().unwrap().to_owned();

    // Stale ETag is rejected without consuming the write.
    let stale = request(
        &service,
        &session,
        "PATCH",
        &format!("/api/v1/entities/{id}"),
        Some(json!({"focus": "Outdated"})),
        &[
            ("If-Match", first_etag.as_str()),
            ("Idempotency-Key", next_key().as_str()),
        ],
    )
    .await;
    assert_eq!(stale.status, 412);
    assert_eq!(stale.body["error"]["code"], "STALE_REVISION");

    // Blank names are rejected; subtype transitions stay out of label edits.
    let blank = request(
        &service,
        &session,
        "PATCH",
        &format!("/api/v1/entities/{id}"),
        Some(json!({"name": "   "})),
        &[
            ("If-Match", second_etag.as_str()),
            ("Idempotency-Key", next_key().as_str()),
        ],
    )
    .await;
    assert_eq!(blank.status, 422, "{blank:?}");
    let confirm = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(confirm.status, 200);
    assert_eq!(confirm.body["data"]["name"], "Dana Lee");

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn p01_entity_notes_canonical_roundtrip_conflict_and_replay() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let (id, _) = create_party(&service, &session, "org", "Fieldwork", "CSV tools").await;

    let initial = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{id}/notes"),
        None,
        &[],
    )
    .await;
    assert_eq!(initial.status, 200);
    assert_eq!(initial.body["data"]["markdown"], "");
    assert_eq!(initial.body["data"]["revision"], 1);
    let initial_etag = initial.body["data"]["etag"].as_str().unwrap().to_owned();

    // CRLF input is canonicalized to LF and round-trips byte-exactly.
    let key = next_key();
    let written = request(
        &service,
        &session,
        "PUT",
        &format!("/api/v1/entities/{id}/notes"),
        Some(json!({"markdown": "Welcomes **small, reproducible fixtures**.\r\n- line one\r\n- line two\r\n", "expected_notes_rev": 1})),
        &[
            ("If-Match", initial_etag.as_str()),
            ("Idempotency-Key", key.as_str()),
        ],
    )
    .await;
    assert_eq!(written.status, 200, "{written:?}");
    assert_eq!(written.body["data"]["revision"], 2);
    assert_eq!(
        written.body["data"]["markdown"],
        "Welcomes **small, reproducible fixtures**.\n- line one\n- line two\n"
    );
    let second_etag = written.body["data"]["etag"].as_str().unwrap().to_owned();

    // A competing stale write is rejected and keeps the committed revision.
    let stale = request(
        &service,
        &session,
        "PUT",
        &format!("/api/v1/entities/{id}/notes"),
        Some(json!({"markdown": "stale", "expected_notes_rev": 1})),
        &[
            ("If-Match", initial_etag.as_str()),
            ("Idempotency-Key", next_key().as_str()),
        ],
    )
    .await;
    assert_eq!(stale.status, 412, "{stale:?}");

    // Replaying the exact committed command under the same key returns the
    // original success; reusing the key for a different body conflicts.
    let replay = request(
        &service,
        &session,
        "PUT",
        &format!("/api/v1/entities/{id}/notes"),
        Some(json!({"markdown": "Welcomes **small, reproducible fixtures**.\r\n- line one\r\n- line two\r\n", "expected_notes_rev": 1})),
        &[
            ("If-Match", initial_etag.as_str()),
            ("Idempotency-Key", key.as_str()),
        ],
    )
    .await;
    assert_eq!(replay.status, 200, "{replay:?}");
    assert_eq!(replay.body["data"]["revision"], 2);
    let reused = request(
        &service,
        &session,
        "PUT",
        &format!("/api/v1/entities/{id}/notes"),
        Some(json!({"markdown": "different body", "expected_notes_rev": 2})),
        &[
            ("If-Match", second_etag.as_str()),
            ("Idempotency-Key", key.as_str()),
        ],
    )
    .await;
    assert_eq!(reused.status, 409);
    assert_eq!(reused.body["error"]["code"], "KEY_REUSED");

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn p01_dossier_lists_direct_links_and_open_opportunities() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let (org_id, _) = create_party(&service, &session, "org", "Fieldwork", "CSV tools").await;

    // Capture a source occurrence and attach a source-backed related person.
    let captured = request(
        &service,
        &session,
        "POST",
        "/api/v1/captures",
        Some(json!({"text": "Fieldwork welcomes small fixtures. Mira maintains the CSV tools.", "title": "Guide", "url": "https://example.invalid/guide"})),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(captured.status, 201, "{captured:?}");
    let occurrence = captured.body["occurrence_id"].as_str().unwrap().to_owned();

    let related = request(
        &service,
        &session,
        "POST",
        "/api/v1/graph/related",
        Some(json!({
            "kind": "person", "name": "Mira Chen", "related_entity_id": org_id,
            "verb": "member_of", "occurrence_id": occurrence,
            "quote": "Mira maintains the CSV tools.",
            "view_scope": "leads", "view_id": org_id, "x": 120.0, "y": 40.0
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(related.status, 201, "{related:?}");

    // Open an opportunity with the organization as primary participant.
    let opportunity = request(
        &service,
        &session,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title": "Onboarding exchange", "primary_party_id": org_id})),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(opportunity.status, 201, "{opportunity:?}");

    let dossier = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{org_id}/dossier"),
        None,
        &[],
    )
    .await;
    assert_eq!(dossier.status, 200, "{dossier:?}");
    let data = &dossier.body["data"];
    assert_eq!(data["entity"]["id"], org_id);
    assert_eq!(data["entity"]["focus"], "CSV tools");
    let links = data["direct_links"].as_array().unwrap();
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0]["verb"], "member_of");
    assert!(links[0]["tier"].as_i64().unwrap() >= 1);
    let opportunities = data["opportunities"].as_array().unwrap();
    assert_eq!(opportunities.len(), 1);
    assert_eq!(opportunities[0]["title"], "Onboarding exchange");
    assert!(data["files"].as_array().unwrap().is_empty());
    assert!(data["associated_resources"].as_array().unwrap().is_empty());

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}
