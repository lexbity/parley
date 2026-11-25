//! TD-10 W2 admission proof: operation keys are explicit on every mutation,
//! the pinned error codes are stable, and a same-key replay produces one
//! effect with the original receipt.

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

fn db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-td10-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn session(service: &RunningService) -> (String, String, String) {
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
        .and_then(|line| line.split_once(':'))
        .map(|(_, value)| value.trim().split(';').next().unwrap().to_owned())
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
        payload["mutation_token"].as_str().unwrap().to_owned(),
        payload["epoch"].as_str().unwrap().to_owned(),
    )
}

async fn request(
    port: u16,
    owner: &(String, String, String),
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (u16, Value) {
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let key_header = key
        .map(|value| format!("Idempotency-Key: {value}\r\n"))
        .unwrap_or_default();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(
            format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {}\r\nX-Parley-Token: {}\r\nX-Parley-Epoch: {}\r\n{key_header}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", owner.0, owner.1, owner.2, body.len()).as_bytes(),
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
    let value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(payload).unwrap()
    };
    (status, value.get("data").cloned().unwrap_or(value))
}

#[tokio::test]
async fn td10_missing_operation_key_fails_closed_on_both_transports() {
    let path = db_path();
    let service = parley_service::start_workspace(&path).await.unwrap();
    let port = service.address.port();
    let owner = session(&service).await;

    // Legacy transport (board): the pinned named code is the existing
    // IDEMPOTENCY_KEY_REQUIRED equivalent.
    let (status, body) = request(
        port,
        &owner,
        "POST",
        "/api/v1/board/lists",
        Some(json!({"title":"TD10 list"})),
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "IDEMPOTENCY_KEY_REQUIRED", "{body}");

    // Registry transport: the pinned code is malformed_request with the
    // named required-key message.
    let (status, body) = request(
        port,
        &owner,
        "POST",
        "/api/v1/commands/opportunity.set_phase",
        Some(json!({"args":{}})),
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "malformed_request", "{body}");
    assert_eq!(body["error"]["message"], "Idempotency-Key is required");

    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn td10_same_key_replay_has_one_effect_and_the_original_receipt() {
    let path = db_path();
    let service = parley_service::start_workspace(&path).await.unwrap();
    let port = service.address.port();
    let owner = session(&service).await;
    let body = Some(json!({"title":"TD10 replay list"}));
    let key = "td10-w2-replay-key";

    let (first_status, first) = request(
        port,
        &owner,
        "POST",
        "/api/v1/board/lists",
        body.clone(),
        Some(key),
    )
    .await;
    let (second_status, second) =
        request(port, &owner, "POST", "/api/v1/board/lists", body, Some(key)).await;
    assert_eq!(first_status, 201, "{first}");
    assert_eq!(second_status, first_status);
    assert_eq!(
        first, second,
        "replay must return the original receipt bytes"
    );

    let workspace = Workspace::open(&path).await.unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM board_list WHERE title='TD10 replay list'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(count, 1, "same-key replay must not create a second list");
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_receipt WHERE key=?")
        .bind(key)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(receipts, 1, "one receipt for the committed effect");
    drop(workspace);
    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn td10_opportunity_create_has_no_content_derived_fallback_key() {
    let path = db_path();
    let service = parley_service::start_workspace(&path).await.unwrap();
    let port = service.address.port();
    let owner = session(&service).await;

    let (status, body) = request(
        port,
        &owner,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"person","name":"TD10 owner","focus":""})),
        Some("td10-w2-party"),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let party = body["id"].as_str().unwrap().to_owned();
    let create_body = Some(json!({"title":"TD10 opportunity","primary_party_id":party}));

    let (status, body) = request(
        port,
        &owner,
        "POST",
        "/api/v1/opportunities",
        create_body.clone(),
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "IDEMPOTENCY_KEY_REQUIRED", "{body}");

    // Identical intents with distinct explicit keys are distinct effects; the
    // retired content-derived digest would have collapsed them.
    let (first_status, first) = request(
        port,
        &owner,
        "POST",
        "/api/v1/opportunities",
        create_body.clone(),
        Some("td10-w2-create-a"),
    )
    .await;
    let (second_status, _) = request(
        port,
        &owner,
        "POST",
        "/api/v1/opportunities",
        create_body.clone(),
        Some("td10-w2-create-b"),
    )
    .await;
    let (replay_status, replay) = request(
        port,
        &owner,
        "POST",
        "/api/v1/opportunities",
        create_body,
        Some("td10-w2-create-a"),
    )
    .await;
    assert_eq!(first_status, 201);
    assert_eq!(second_status, 201);
    assert_eq!(replay_status, first_status);
    assert_eq!(
        replay["id"], first["id"],
        "replay returns the original record"
    );

    service.shutdown().await;
    let _ = fs::remove_file(path);
}

/// The committed route inventory is the AC-11 oracle; this test consumes it
/// directly to prove every non-exempt legacy mutation fails closed on a
/// missing operation key (AC-12 first half).
const ROUTE_INVENTORY: &str = include_str!("../../../contract/fixtures/td10/route-inventory.json");

#[tokio::test]
async fn td10_inventory_missing_key_fails_closed_on_every_legacy_mutation() {
    let inventory: Value = serde_json::from_str(ROUTE_INVENTORY).unwrap();
    let routes = inventory["routes"].as_array().unwrap();
    let db = db_path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let port = service.address.port();
    let owner = session(&service).await;
    let mut checked = 0usize;
    for route in routes {
        let exempt = route["exempt"].as_str().unwrap();
        let dispatch = route["dispatch"].as_str().unwrap();
        let method = route["method"].as_str().unwrap();
        if exempt != "—" || dispatch != "legacy-axum" {
            continue;
        }
        if !matches!(method, "POST" | "PUT" | "PATCH" | "DELETE") {
            continue;
        }
        let path = route["path"].as_str().unwrap();
        if path.ends_with("/preview") {
            continue;
        }
        let (status, body) = request(port, &owner, method, path, Some(json!({})), None).await;
        assert_eq!(
            status, 400,
            "missing operation key on {method} {path} must fail 400, got {status} {body}"
        );
        assert_eq!(
            body["error"]["code"], "IDEMPOTENCY_KEY_REQUIRED",
            "{method} {path}: unexpected code {body}"
        );
        checked += 1;
    }
    assert!(
        checked >= 90,
        "inventory sweep must cover the legacy mutation surface (checked {checked})"
    );
    service.shutdown().await;
    let _ = fs::remove_file(db);
}

#[tokio::test]
async fn td10_gate_claimed_replay_returns_original_receipt_without_second_effect() {
    let path = db_path();
    let service = parley_service::start_workspace(&path).await.unwrap();
    let port = service.address.port();
    let owner = session(&service).await;
    let key = "td10-gate-backup";
    let body = Some(json!({}));

    let (first_status, first) = request(
        port,
        &owner,
        "POST",
        "/api/v1/backups",
        body.clone(),
        Some(key),
    )
    .await;
    let (second_status, second) =
        request(port, &owner, "POST", "/api/v1/backups", body, Some(key)).await;
    assert_eq!(first_status, 200, "{first}");
    assert_eq!(
        second_status, first_status,
        "replay must return the original status"
    );
    assert_eq!(
        first, second,
        "replay must return the original receipt bytes"
    );

    let workspace = Workspace::open(&path).await.unwrap();
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_receipt WHERE key=?")
        .bind(key)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(receipts, 1, "exactly one receipt for one effect");
    drop(workspace);
    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn td10_gate_same_key_different_intent_fails_409_key_reused() {
    let path = db_path();
    let service = parley_service::start_workspace(&path).await.unwrap();
    let port = service.address.port();
    let owner = session(&service).await;
    let key = "td10-gate-backup-reuse";

    // First intent succeeds and stores a receipt.
    let (status, body) = request(
        port,
        &owner,
        "POST",
        "/api/v1/backups",
        Some(json!({})),
        Some(key),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    // Same key, different intent: terminal 409, never a second effect.
    let (status, body) = request(
        port,
        &owner,
        "POST",
        "/api/v1/backups",
        Some(json!({"x":1})),
        Some(key),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "KEY_REUSED", "{body}");
    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn td10_legacy_destructive_stops_at_approval_inbox_and_resumes_on_grant() {
    let path = db_path();
    let service = parley_service::start_workspace(&path).await.unwrap();
    let port = service.address.port();
    let owner = session(&service).await;

    // Seed a party to erase.
    let (status, created) = request(
        port,
        &owner,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"person","name":"TD10 erase target","focus":""})),
        Some("td10-w3-party-key"),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    let party = created["id"].as_str().unwrap().to_owned();
    let erase_path = format!("/api/v1/parties/{party}/erase");
    let erase_body = Some(json!({"preview_id":"p","preview_digest":"d"}));
    let op_key = "td10-w3-erase-key";

    // First intent: frozen, parked at the approval inbox.
    let (status, body) = request(
        port,
        &owner,
        "POST",
        &erase_path,
        erase_body.clone(),
        Some(op_key),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["code"], "approval_required", "{body}");

    let pending = request(
        port,
        &owner,
        "GET",
        "/api/v1/approvals?status=pending",
        None,
        None,
    )
    .await;
    let items = pending.1["items"].as_array().unwrap();
    let approval = items
        .iter()
        .find(|item| item["command_id"] == "party.erase")
        .unwrap_or_else(|| panic!("pending party.erase approval missing: {items:?}"));
    let approval_id = approval["id"].as_str().unwrap().to_owned();
    let revision = approval["revision"].as_i64().unwrap();
    let preview_digest = approval["preview_digest"].as_str().unwrap().to_owned();

    // Still pending: a repeat stays parked.
    let (status, body) = request(
        port,
        &owner,
        "POST",
        &erase_path,
        erase_body.clone(),
        Some(op_key),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["code"], "approval_required", "{body}");

    // Owner grants the frozen intent.
    let (status, body) = request(
        port,
        &owner,
        "POST",
        &format!("/api/v1/approvals/{approval_id}/grant"),
        Some(json!({"expected_revision": revision, "preview_digest": preview_digest})),
        Some("td10-w3-grant-key"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "granted", "{body}");

    // The same intent resumes on the same operation key: the gate consumes
    // the grant and forwards to the handler (validation now runs, so the
    // frozen preview body fails handler-side, never the approval gate).
    let (status, body) = request(port, &owner, "POST", &erase_path, erase_body, Some(op_key)).await;
    assert_ne!(status, 403, "{body}");
    assert_ne!(body["error"]["code"], "approval_required", "{body}");
    assert!(
        matches!(status, 409 | 422 | 404 | 400 | 500),
        "post-grant execute should reach the handler, got {status} {body}"
    );

    let workspace = Workspace::open(&path).await.unwrap();
    let consumed: Option<String> =
        sqlx::query_scalar("SELECT status FROM command_approval WHERE id=?")
            .bind(&approval_id)
            .fetch_optional(workspace.pool())
            .await
            .unwrap();
    assert_eq!(
        consumed.as_deref(),
        Some("consumed"),
        "grant must be consumed exactly once"
    );
    drop(workspace);
    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn td10_stale_revision_fails_with_the_pinned_412() {
    let path = db_path();
    let service = parley_service::start_workspace(&path).await.unwrap();
    let port = service.address.port();
    let owner = session(&service).await;
    let (status, created) = request(
        port,
        &owner,
        "POST",
        "/api/v1/board/lists",
        Some(json!({"title":"TD10 stale list"})),
        Some("td10-w2-create"),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    let list_id = created["list"]["id"]
        .as_str()
        .expect("created list id")
        .to_owned();

    let (status, body) = request(
        port,
        &owner,
        "PATCH",
        &format!("/api/v1/board/lists/{list_id}"),
        Some(json!({"title":"Renamed","expected_revision":99})),
        Some("td10-w2-rename-stale"),
    )
    .await;
    assert_eq!(status, 412, "{body}");
    assert_eq!(body["error"]["code"], "STALE_REVISION", "{body}");
    service.shutdown().await;
    let _ = fs::remove_file(path);
}
