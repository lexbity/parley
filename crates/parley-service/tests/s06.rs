//! S06: verified file issuance across power loss (RFC-0002 BQ4/BE-11).
//!
//! The full journey uploads bytes, reviews the attachment for one recipient,
//! binds it to an approved material version, releases it, and downloads the
//! exact bytes. Denial and recovery paths cover unreviewed/private uploads,
//! corrupt store files (zero bytes issued), one-use release grants, duplicate
//! uploads sharing bytes with distinct review permission, and garbage
//! collection that rechecks leases and pins before unlink.

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
static REQUESTS: AtomicU64 = AtomicU64::new(0);

fn db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-s06-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn raw_request(
    port: u16,
    method: &str,
    path: &str,
    extra_headers: &str,
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let (cookie, token, epoch) = SESSIONS
        .get()
        .unwrap()
        .lock()
        .unwrap()
        .get(&port)
        .unwrap()
        .clone();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let key = REQUESTS.fetch_add(1, Ordering::Relaxed);
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\nIdempotency-Key: s06-{key}\r\n{extra_headers}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response has a header terminator");
    let head = std::str::from_utf8(&response[..header_end]).unwrap();
    let status = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let payload = &response[header_end + 4..];
    let payload = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut decoded = Vec::new();
        let mut rest = payload;
        loop {
            let (size, tail) = rest
                .split(|byte| *byte == b'\r' || *byte == b'\n')
                .next()
                .map(|size| {
                    let size = std::str::from_utf8(size).unwrap();
                    (
                        usize::from_str_radix(size.trim(), 16).unwrap(),
                        &rest[size.len() + 2..],
                    )
                })
                .unwrap();
            rest = tail;
            if size == 0 {
                break;
            }
            decoded.extend_from_slice(&rest[..size]);
            rest = &rest[size + 2..];
        }
        decoded
    } else {
        payload.to_vec()
    };
    (status, payload)
}

async fn request(port: u16, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
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
    let key = REQUESTS.fetch_add(1, Ordering::Relaxed);
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\nIdempotency-Key: s06-{key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
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
    let payload = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut decoded = Vec::new();
        let mut rest = payload;
        loop {
            let (size, tail) = rest.split_once("\r\n").unwrap();
            let size = usize::from_str_radix(size.trim(), 16).unwrap();
            rest = tail;
            if size == 0 {
                break;
            }
            decoded.extend_from_slice(&rest.as_bytes()[..size]);
            rest = &rest[size + 2..];
        }
        String::from_utf8(decoded).unwrap()
    } else {
        payload.to_owned()
    };
    let value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&payload).unwrap()
    };
    let value = value.get("data").cloned().unwrap_or(value);
    (status, value)
}

async fn owner_session(service: &RunningService) -> (String, String, String) {
    let address = service.address;
    let body = json!({"secret":service.bootstrap_secret}).to_string();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(format!("POST /api/v1/session/bootstrap HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
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
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let payload: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    (
        cookie,
        payload["mutation_token"].as_str().unwrap().to_owned(),
        payload["epoch"].as_str().unwrap().to_owned(),
    )
}

async fn setup() -> (PathBuf, Workspace, u16, RunningService) {
    let path = db_path();
    let service = parley_service::start_workspace(&path).await.unwrap();
    let session = owner_session(&service).await;
    let port = service.address.port();
    SESSIONS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap()
        .insert(port, session);
    let workspace = Workspace::open(&path).await.unwrap();
    (path, workspace, port, service)
}

async fn create_party(port: u16, name: &str) -> String {
    let (status, person) = request(
        port,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"person","name":name})),
    )
    .await;
    assert_eq!(status, 201);
    person["id"].as_str().unwrap().to_owned()
}

async fn prepare_exchange(port: u16, opportunity: &str, recipient: &str) -> i64 {
    let (status, them) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/interests"),
        Some(json!({"recipient_id":recipient,"side":"them","statement":"A clearer onboarding handoff","knowledge_kind":"hypothesis","check_suggestion":"Ask the owner whether this is current."})),
    )
    .await;
    assert_eq!(status, 201, "{them}");
    let (status, me) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/interests"),
        Some(json!({"recipient_id":recipient,"side":"me","statement":"Offer a usable checklist","knowledge_kind":"owner_report"})),
    )
    .await;
    assert_eq!(status, 201);
    let (status, exchange) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/exchange"),
        Some(json!({"recipient_id":recipient,"they_get":"A practical onboarding checklist","i_get":"A better understanding of the handoff","interest_ids":[them["id"],me["id"]]})),
    )
    .await;
    assert_eq!(status, 201);
    let version = exchange["version"].as_i64().unwrap();
    let (status, _) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/exchange/{recipient}/attest"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    version
}

async fn activate_minimal_template(port: u16) -> String {
    let (status, template_row) = request(
        port,
        "POST",
        "/api/v1/templates",
        Some(json!({"name":"Handoff note","body":"Hello."})),
    )
    .await;
    assert_eq!(status, 201, "{template_row}");
    let template_id = template_row["id"].as_str().unwrap().to_owned();
    let (status, _) = request(
        port,
        "POST",
        &format!("/api/v1/templates/{template_id}/approve"),
        None,
    )
    .await;
    assert_eq!(status, 204);
    let (status, _) = request(
        port,
        "POST",
        &format!("/api/v1/templates/{template_id}/activate"),
        None,
    )
    .await;
    assert_eq!(status, 204);
    template_id
}

async fn upload_file(port: u16, filename: &str, bytes: &[u8]) -> Value {
    let (status, payload) = raw_request(
        port,
        "POST",
        "/api/v1/uploads",
        &format!("Content-Type: application/octet-stream\r\nX-Parley-Filename: {filename}"),
        bytes.to_vec(),
    )
    .await;
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&payload));
    serde_json::from_slice(&payload).unwrap()
}

async fn review_upload(port: u16, upload_id: &str, share: bool) -> Value {
    let (status, value) = request(
        port,
        "POST",
        &format!("/api/v1/uploads/{upload_id}/review"),
        Some(json!({"share_permission":share})),
    )
    .await;
    assert_eq!(status, 200, "{value}");
    value
}

async fn create_handoff_material(
    port: u16,
    opportunity: &str,
    recipient: &str,
    template_id: &str,
    exchange_version: i64,
    upload_id: Option<&str>,
    body: &str,
) -> (u16, Value) {
    request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/materials"),
        Some(json!({
            "recipient_id": recipient,
            "kind": "handoff",
            "template_version_id": template_id,
            "body": body,
            "exchange_version": exchange_version,
            "slot_fills": [],
            "sentence_bindings": [{"sentence_index":0,"claim_type":"generic"}],
            "attachment": upload_id.map(|id| json!({"upload_id": id}))
        })),
    )
    .await
}

async fn approve_material(port: u16, material_id: &str, overrides: Value) -> (u16, Value) {
    request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/approve"),
        Some(json!({"override_reasons": overrides})),
    )
    .await
}

async fn download(port: u16, material_id: &str, release_id: &str) -> (u16, Vec<u8>) {
    raw_request(
        port,
        "GET",
        &format!("/api/v1/materials/{material_id}/download?release_id={release_id}"),
        "",
        Vec::new(),
    )
    .await
}

const HANDOFF_BYTES: &[u8] =
    b"S06 handoff payload \xE2\x80\x94 \xc3\xa6\xc3\xb8\xc3\xa5 \xe2\x9c\x93\n";

#[tokio::test]
async fn s06_verified_file_release_download_returns_exact_bytes_and_one_grant() {
    let (path, workspace, port, service) = setup().await;
    let blobs_dir = format!("{}.blobs", path.display());
    let recipient = create_party(port, "Dana Lee").await;
    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":"Handoff exchange","primary_party_id":recipient})),
    )
    .await;
    assert_eq!(status, 201);
    let opportunity_id = opportunity["id"].as_str().unwrap();
    let exchange_version = prepare_exchange(port, opportunity_id, &recipient).await;
    let template_id = activate_minimal_template(port).await;
    let upload = upload_file(port, "handoff.pdf", HANDOFF_BYTES).await;
    let upload_id = upload["id"].as_str().unwrap();
    let sha = upload["sha256"].as_str().unwrap();

    // An unreviewed attachment cannot bind to a material.
    let (status, blocked) = create_handoff_material(
        port,
        opportunity_id,
        &recipient,
        &template_id,
        exchange_version,
        Some(upload_id),
        "Please find the attached handoff.",
    )
    .await;
    assert_eq!(status, 422, "{blocked}");
    assert!(
        blocked["error"]["message"]
            .as_str()
            .unwrap()
            .contains("reviewed")
    );

    review_upload(port, upload_id, true).await;

    let (status, material) = create_handoff_material(
        port,
        opportunity_id,
        &recipient,
        &template_id,
        exchange_version,
        Some(upload_id),
        "Please find the attached handoff.",
    )
    .await;
    assert_eq!(status, 201, "{material}");
    let material_id = material["id"].as_str().unwrap();
    assert_eq!(material["attachment"]["sha256"], sha);
    assert_eq!(material["attachment"]["filename"], "handoff.pdf");
    assert_eq!(material["content_sha256"], sha);

    let overrides = json!({
        "S1 GIVE_FIRST": "This is a reviewed file handoff, not a gift message.",
        "S2 ANCHOR": "The exact approved attachment bytes are the anchor for this handoff."
    });
    let (status, approval) = approve_material(port, material_id, overrides).await;
    assert_eq!(status, 200, "{approval}");
    let (status, released) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/release"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{released}");
    let release_id = released["release_id"].as_str().unwrap();
    assert_eq!(released["content_sha256"], sha);
    assert_eq!(released["attachment"]["sha256"], sha);
    assert_eq!(released["attachment"]["filename"], "handoff.pdf");

    // Queue and Output address the same record.
    let (status, queue) = request(
        port,
        "GET",
        &format!("/api/v1/opportunities/{opportunity_id}/materials"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(queue[0]["id"], material_id);
    let (status, outputs) = request(port, "GET", "/api/v1/outputs", None).await;
    assert_eq!(status, 200);
    assert_eq!(outputs[0]["id"], material_id);
    assert_eq!(outputs[0]["attachment"]["sha256"], sha);

    // HEAD-as-GET must be refused without touching the one-use grant
    // (RFC-0004 IQ2: navigation, link preview and prefetch must not
    // trigger issuance). A 405 means no spool verification and no grant
    // consumption happen before the real GET.
    let (status, _head_bytes) = raw_request(
        port,
        "HEAD",
        &format!("/api/v1/materials/{material_id}/download?release_id={release_id}"),
        "",
        Vec::new(),
    )
    .await;
    assert_eq!(status, 405);

    let (status, bytes) = download(port, material_id, release_id).await;
    assert_eq!(status, 200);
    assert_eq!(bytes, HANDOFF_BYTES);

    // The grant is one-use.
    let (status, second) = download(port, material_id, release_id).await;
    assert_eq!(status, 422, "{}", String::from_utf8_lossy(&second));
    assert!(String::from_utf8_lossy(&second).contains("already consumed"));
    // Independent persisted observation matches the public response.
    let (blob_size, blob_mime): (i64, String) =
        sqlx::query_as("SELECT size,mime FROM blob WHERE sha256=?")
            .bind(sha)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(blob_size, HANDOFF_BYTES.len() as i64);
    assert_eq!(blob_mime, "application/octet-stream");
    let (stored_upload, stored_sha): (String, String) = sqlx::query_as(
        "SELECT blob_upload_id,blob_sha256 FROM material_version WHERE material_id=? AND version_no=1",
    )
    .bind(material_id)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(stored_upload, upload_id);
    assert_eq!(stored_sha, sha);
    let (issued, incomplete): (Option<String>, i64) =
        sqlx::query_as("SELECT bytes_issued_at,transfer_incomplete FROM release_record WHERE id=?")
            .bind(release_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert!(
        issued.is_some(),
        "issuance must be recorded before streaming"
    );
    assert_eq!(incomplete, 0);
    let download_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event WHERE event_type='material.download_issued'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(download_events, 1);
    let blob_dep: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM version_dependency WHERE material_id=? AND version_no=1 AND kind='blob' AND source_id=?",
    )
    .bind(material_id)
    .bind(upload_id)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(blob_dep, 1);

    service.shutdown().await;
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(blobs_dir);
}

#[tokio::test]
async fn s06_unreviewed_or_not_shareable_upload_cannot_bind() {
    let (path, _workspace, port, service) = setup().await;
    let blobs_dir = format!("{}.blobs", path.display());
    let recipient = create_party(port, "Dana Lee").await;
    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":"Handoff exchange","primary_party_id":recipient})),
    )
    .await;
    assert_eq!(status, 201);
    let opportunity_id = opportunity["id"].as_str().unwrap();
    let exchange_version = prepare_exchange(port, opportunity_id, &recipient).await;
    let template_id = activate_minimal_template(port).await;
    let upload = upload_file(port, "private.pdf", b"private bytes").await;
    let upload_id = upload["id"].as_str().unwrap();

    let (status, blocked) = create_handoff_material(
        port,
        opportunity_id,
        &recipient,
        &template_id,
        exchange_version,
        Some(upload_id),
        "Please find the attached handoff.",
    )
    .await;
    assert_eq!(status, 422, "{blocked}");
    assert!(
        blocked["error"]["message"]
            .as_str()
            .unwrap()
            .contains("reviewed")
    );

    review_upload(port, upload_id, false).await;
    let (status, blocked) = create_handoff_material(
        port,
        opportunity_id,
        &recipient,
        &template_id,
        exchange_version,
        Some(upload_id),
        "Please find the attached handoff.",
    )
    .await;
    assert_eq!(status, 422, "{blocked}");
    assert!(
        blocked["error"]["message"]
            .as_str()
            .unwrap()
            .contains("reviewed")
    );

    // A recorded review is immutable: flipping the share permission is denied,
    // while re-affirming the same permission replays the record.
    let (status, value) = request(
        port,
        "POST",
        &format!("/api/v1/uploads/{upload_id}/review"),
        Some(json!({"share_permission":true})),
    )
    .await;
    assert_eq!(status, 422, "{value}");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cannot be changed")
    );
    let (status, _) = request(
        port,
        "POST",
        &format!("/api/v1/uploads/{upload_id}/review"),
        Some(json!({"share_permission":false})),
    )
    .await;
    assert_eq!(status, 200);

    // A separate upload of the same bytes carries its own review permission.
    let second = upload_file(port, "private.pdf", b"private bytes").await;
    review_upload(port, second["id"].as_str().unwrap(), true).await;
    let (status, material) = create_handoff_material(
        port,
        opportunity_id,
        &recipient,
        &template_id,
        exchange_version,
        Some(second["id"].as_str().unwrap()),
        "Please find the attached handoff.",
    )
    .await;
    assert_eq!(status, 201, "{material}");
    assert_eq!(material["attachment"]["upload_id"], second["id"]);

    service.shutdown().await;
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(blobs_dir);
}

#[tokio::test]
async fn s06_corrupt_blob_file_issues_zero_bytes_until_restored() {
    let (path, workspace, port, service) = setup().await;
    let blobs_dir = format!("{}.blobs", path.display());
    let recipient = create_party(port, "Dana Lee").await;
    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":"Handoff exchange","primary_party_id":recipient})),
    )
    .await;
    assert_eq!(status, 201);
    let opportunity_id = opportunity["id"].as_str().unwrap();
    let exchange_version = prepare_exchange(port, opportunity_id, &recipient).await;
    let template_id = activate_minimal_template(port).await;
    let upload = upload_file(port, "handoff.pdf", HANDOFF_BYTES).await;
    let upload_id = upload["id"].as_str().unwrap();
    let sha = upload["sha256"].as_str().unwrap();
    review_upload(port, upload_id, true).await;
    let (status, material) = create_handoff_material(
        port,
        opportunity_id,
        &recipient,
        &template_id,
        exchange_version,
        Some(upload_id),
        "Please find the attached handoff.",
    )
    .await;
    assert_eq!(status, 201);
    let material_id = material["id"].as_str().unwrap();
    let overrides = json!({
        "S1 GIVE_FIRST": "This is a reviewed file handoff, not a gift message.",
        "S2 ANCHOR": "The exact approved attachment bytes are the anchor for this handoff."
    });
    let (status, _) = approve_material(port, material_id, overrides).await;
    assert_eq!(status, 200);
    let (status, released) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/release"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let release_id = released["release_id"].as_str().unwrap();

    // Flip the last byte of the stored file. No byte may be issued.
    let blob_path = workspace.blob_root().join(sha);
    assert!(blob_path.exists());
    let mut bytes = fs::read(&blob_path).unwrap();
    let last = bytes.len() - 1;
    let original = bytes[last];
    bytes[last] ^= 0xff;
    fs::write(&blob_path, &bytes).unwrap();

    let (status, payload) = download(port, material_id, release_id).await;
    assert_eq!(status, 422, "{}", String::from_utf8_lossy(&payload));
    assert_ne!(payload, HANDOFF_BYTES, "corrupt bytes must never be issued");
    assert!(String::from_utf8_lossy(&payload).contains("verification"));
    let issued: Option<String> =
        sqlx::query_scalar("SELECT bytes_issued_at FROM release_record WHERE id=?")
            .bind(release_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert!(
        issued.is_none(),
        "no issuance may be recorded for corrupt bytes"
    );
    let download_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event WHERE event_type='material.download_issued'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(download_events, 0);

    // Restoring the exact byte makes the same grant issuable again.
    bytes[last] = original;
    fs::write(&blob_path, &bytes).unwrap();
    let (status, restored) = download(port, material_id, release_id).await;
    assert_eq!(status, 200);
    assert_eq!(restored, HANDOFF_BYTES);

    service.shutdown().await;
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(blobs_dir);
}

#[tokio::test]
async fn s06_duplicate_uploads_share_bytes_with_distinct_review_permission() {
    let (path, _workspace, port, service) = setup().await;
    let blobs_dir = format!("{}.blobs", path.display());
    let recipient = create_party(port, "Dana Lee").await;
    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":"Handoff exchange","primary_party_id":recipient})),
    )
    .await;
    assert_eq!(status, 201);
    let opportunity_id = opportunity["id"].as_str().unwrap();
    let exchange_version = prepare_exchange(port, opportunity_id, &recipient).await;
    let template_id = activate_minimal_template(port).await;

    let first = upload_file(port, "shared.pdf", HANDOFF_BYTES).await;
    let second = upload_file(port, "shared-copy.pdf", HANDOFF_BYTES).await;
    assert_eq!(first["sha256"], second["sha256"]);
    assert_ne!(first["id"], second["id"]);
    let first_id = first["id"].as_str().unwrap();
    let second_id = second["id"].as_str().unwrap();

    review_upload(port, first_id, true).await;
    let (status, blocked) = create_handoff_material(
        port,
        opportunity_id,
        &recipient,
        &template_id,
        exchange_version,
        Some(second_id),
        "Please find the attached handoff.",
    )
    .await;
    assert_eq!(status, 422, "{blocked}");
    let (status, material) = create_handoff_material(
        port,
        opportunity_id,
        &recipient,
        &template_id,
        exchange_version,
        Some(first_id),
        "Please find the attached handoff.",
    )
    .await;
    assert_eq!(status, 201, "{material}");
    assert_eq!(material["attachment"]["upload_id"], first_id);
    assert_eq!(material["attachment"]["sha256"], first["sha256"]);

    service.shutdown().await;
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(blobs_dir);
}

#[tokio::test]
async fn s06_gc_respects_leases_and_pins_and_reclaims_orphans() {
    let (path, workspace, port, service) = setup().await;
    let blobs_dir = format!("{}.blobs", path.display());
    let upload = upload_file(port, "kept.pdf", HANDOFF_BYTES).await;
    let sha = upload["sha256"].as_str().unwrap().to_owned();
    let store = workspace.blob_root().to_path_buf();
    let kept_bytes = fs::read(store.join(&sha)).unwrap();
    let orphan_digest = format!("{}f", &sha[..63]);
    let pinned_digest = format!("{}e", &sha[..63]);

    // A newly installed file with a live lease survives GC.
    fs::write(store.join(&orphan_digest), &kept_bytes).unwrap();
    sqlx::query("INSERT INTO blob_upload_lease(upload_id,blob_sha256,size,created_at) VALUES('lease-1',?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'))")
        .bind(&orphan_digest)
        .bind(kept_bytes.len() as i64)
        .execute(workspace.pool())
        .await
        .unwrap();
    let _report = workspace.run_gc().await.unwrap();
    assert!(
        store.join(&orphan_digest).exists(),
        "leased orphan must survive GC"
    );

    // Once the lease is released and no pin exists, the orphan is reclaimed.
    sqlx::query("DELETE FROM blob_upload_lease WHERE upload_id='lease-1'")
        .execute(workspace.pool())
        .await
        .unwrap();
    let _report = workspace.run_gc().await.unwrap();
    assert!(
        !store.join(&orphan_digest).exists(),
        "unreferenced orphan must be reclaimed"
    );

    // A pinned digest survives GC and becomes eligible after the pin release.
    fs::write(store.join(&pinned_digest), &kept_bytes).unwrap();
    workspace
        .pin_snapshot_blobs("snap-1", std::slice::from_ref(&pinned_digest))
        .await
        .unwrap();
    let _report = workspace.run_gc().await.unwrap();
    assert!(
        store.join(&pinned_digest).exists(),
        "pinned digest must survive GC"
    );
    workspace.release_snapshot_pin("snap-1").await.unwrap();
    let _report = workspace.run_gc().await.unwrap();
    assert!(
        !store.join(&pinned_digest).exists(),
        "released pin must allow reclamation"
    );

    // The committed blob referenced by the upload is never collected.
    assert!(store.join(&sha).exists(), "committed blob must survive GC");
    assert_eq!(fs::read(store.join(&sha)).unwrap(), kept_bytes);

    service.shutdown().await;
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(blobs_dir);
}
