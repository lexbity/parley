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
async fn s09_template_family_version_receipts_and_voice_retirement_are_durable() {
    let (path, workspace, port, service) = setup().await;
    let (status, family) = request(
        port,
        "POST",
        "/api/v1/template-families",
        Some(json!({"name":"Review"})),
        "s09-family",
    )
    .await;
    assert_eq!(status, 201, "{family:?}");
    let (again_status, again) = request(
        port,
        "POST",
        "/api/v1/template-families",
        Some(json!({"name":"Review"})),
        "s09-family",
    )
    .await;
    assert_eq!(again_status, 201);
    assert_eq!(family["id"], again["id"]);
    let family_id = family["id"].as_str().unwrap();
    let (status, version) = request(port, "POST", &format!("/api/v1/templates/{family_id}/versions"), Some(json!({
        "name":"Simple", "body":"Hello {{their_name:name}}.", "stage_class":"identified", "channel":"email", "sentence_rules":[[]]
    })), "s09-version").await;
    assert_eq!(status, 201, "{version:?}");
    let version_id = version["id"].as_str().unwrap();
    let before: (String, String) =
        sqlx::query_as("SELECT body,content_sha256 FROM template_version WHERE id=?")
            .bind(version_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(before.1.len(), 64);
    assert!(
        sqlx::query("UPDATE template_version SET body='changed' WHERE id=?")
            .bind(version_id)
            .execute(workspace.pool())
            .await
            .is_err()
    );

    let (status, sample) = request(
        port,
        "POST",
        "/api/v1/voice-samples",
        Some(json!({"text":"A short owner voice example.","channel":"email"})),
        "s09-voice",
    )
    .await;
    assert_eq!(status, 201, "{sample:?}");
    let id = sample["id"].as_str().unwrap();
    let (status, retired) = request(
        port,
        "POST",
        &format!("/api/v1/voice-samples/{id}/retire"),
        None,
        "s09-retire",
    )
    .await;
    assert_eq!(status, 200, "{retired:?}");
    assert_eq!(retired["status"], "retired");
    let status: String = sqlx::query_scalar("SELECT status FROM voice_sample WHERE id=?")
        .bind(id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(status, "retired");

    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn s09_calibration_requires_local_model_digest_and_receipts_are_bound() {
    let (path, workspace, port, service) = setup().await;
    sqlx::query("UPDATE agent_provider SET embed_model='local-test-embed',embed_model_digest='sha256:test-model' WHERE singleton=1").execute(workspace.pool()).await.unwrap();
    let payload = json!({"asset_kind":"gift","fixture_id":"s09-gift-fixture","samples":[{"score":0.9,"relevant":true},{"score":0.8,"relevant":true},{"score":0.2,"relevant":false},{"score":0.1,"relevant":false}]});
    let (status, calibration) = request(
        port,
        "POST",
        "/api/v1/matching/calibrations",
        Some(payload.clone()),
        "s09-calibration",
    )
    .await;
    assert_eq!(status, 201, "{calibration:?}");
    assert_eq!(calibration["model_digest"], "sha256:test-model");
    assert_eq!(calibration["positives"], 2);
    assert_eq!(calibration["negatives"], 2);
    let (again_status, again) = request(
        port,
        "POST",
        "/api/v1/matching/calibrations",
        Some(payload),
        "s09-calibration",
    )
    .await;
    assert_eq!(again_status, 201);
    assert_eq!(calibration, again);
    let (conflict_status, _) = request(port, "POST", "/api/v1/matching/calibrations", Some(json!({"asset_kind":"gift","fixture_id":"different","samples":[{"score":0.9,"relevant":true},{"score":0.1,"relevant":false}]})), "s09-calibration").await;
    assert_eq!(conflict_status, 409);
    let receipts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM idempotency_receipt WHERE key='s09-calibration'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(receipts, 1);
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}
