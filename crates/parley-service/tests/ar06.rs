//! AR-001 AR06: the Extractor runs as a typed structured role through the
//! packaged Pi worker, with a preplanned bounded chunk set, deterministic
//! host-side validation, cross-chunk deduplication and durable proposal
//! staging after every chunk succeeded. The Rust project exercises the real
//! packaged worker and a scripted upstream speaking the real Ollama-compatible
//! wire (support::model_server), never a browser fixture.

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

#[path = "support/mod.rs"]
mod support;
use support::model_server::{ModelServer, Reply, install_qualified_profile_with};

static NEXT: AtomicU64 = AtomicU64::new(0);
static KEY: AtomicU64 = AtomicU64::new(0);

async fn setup() -> (PathBuf, Workspace, RunningService, String, String, String) {
    let path = std::env::temp_dir().join(format!(
        "parley-ar06-{}-{}.sqlite",
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
    auth: (&String, &String, &String),
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

fn fresh_key() -> String {
    format!("ar06-{}", KEY.fetch_add(1, Ordering::Relaxed))
}

fn extractor_native(content: &str) -> Value {
    json!({"message":{"role":"assistant","content":content},"prompt_eval_count":40,"eval_count":8})
}

type Responder = Box<dyn FnMut(usize, &str) -> Value + Send>;

/// Scripted extractor responder: answers every chunk with candidates drawn
/// from the actual chunk text it receives, so the server-side plan length is
/// irrelevant to the test.
fn chunk_responder() -> Responder {
    Box::new(move |_index, body| {
        let payload: Value = serde_json::from_str(body).unwrap_or_default();
        let content = payload["messages"]
            .as_array()
            .and_then(|messages| messages.last())
            .and_then(|message| message["content"].as_str())
            .unwrap_or_default();
        let mut candidates = Vec::new();
        if content.contains("Alpha uses the new hiring pipeline") {
            candidates.push(json!({
                "kind":"signal","statement":"Alpha uses the new hiring pipeline.",
                "quote":"Alpha uses the new hiring pipeline.",
                "mention_name":"Dana Lee","mention_kind":"person"
            }));
        }
        if content.contains("Beta mentioned the quarterly planning review") {
            // The same unique quote twice in one chunk output must collapse to
            // one staged proposal through host-side deduplication.
            candidates.push(json!({
                "kind":"interest","statement":"Beta mentioned the quarterly planning review.",
                "quote":"Beta mentioned the quarterly planning review.",
                "mention_name":null,"mention_kind":null
            }));
            candidates.push(json!({
                "kind":"interest","statement":"Beta mentioned the quarterly planning review.",
                "quote":"Beta mentioned the quarterly planning review.",
                "mention_name":null,"mention_kind":null
            }));
        }
        if content.contains("Gamma coordinates the onboarding workbook") {
            candidates.push(json!({
                "kind":"hook","statement":"Gamma coordinates the onboarding workbook.",
                "quote":"Gamma coordinates the onboarding workbook.",
                "mention_name":null,"mention_kind":null
            }));
        }
        extractor_native(&json!({"candidates":candidates,"injection_suspected":false}).to_string())
    })
}

fn filler() -> String {
    "Filler data fills this line without any remarkable content or claim.\n".to_owned()
}

fn multi_chunk_source() -> String {
    let mut text = String::new();
    text.push_str("Alpha uses the new hiring pipeline.\n");
    for _ in 0..40 {
        text.push_str(&filler());
    }
    text.push_str("Beta mentioned the quarterly planning review.\n");
    for _ in 0..40 {
        text.push_str(&filler());
    }
    text.push_str("Gamma coordinates the onboarding workbook.\n");
    for _ in 0..40 {
        text.push_str(&filler());
    }
    text
}

async fn wait_terminal(port: u16, auth: (&String, &String, &String), job_id: &str) -> Value {
    let mut job = Value::Null;
    for _ in 0..400 {
        let (status, row) = request(
            port,
            auth,
            "GET",
            &format!("/api/v1/jobs/{job_id}"),
            None,
            &fresh_key(),
        )
        .await;
        assert_eq!(status, 200, "{row}");
        job = row;
        if job["status"] == "succeeded" || job["status"] == "failed" || job["status"] == "invalid" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    job
}

use std::time::Duration;

#[tokio::test]
async fn ar06_chunk_budget_and_original_span_round_trip() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let auth = (&cookie, &token, &epoch);
    let provider = ModelServer::with_responder(chunk_responder(), None).await;
    install_qualified_profile_with(workspace.pool(), &path, provider.endpoint(), 4096, 256, 64)
        .await;

    let text = multi_chunk_source();
    let (status, source) = request(
        service.address.port(),
        auth,
        "POST",
        "/api/v1/captures",
        Some(json!({"text":text})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 201, "{source}");
    let job_id = source["job_id"].as_str().unwrap().to_owned();
    let job = wait_terminal(service.address.port(), auth, &job_id).await;
    assert_eq!(job["status"], "succeeded", "{job}");

    // The staged plan really ran as bounded per-chunk worker executions.
    let recorded = provider.recorded();
    let chat_calls: usize = recorded
        .iter()
        .filter(|entry| entry.contains("POST /v1/chat/completions"))
        .count();
    assert!(
        chat_calls >= 2,
        "expected multiple chunks, got {chat_calls}"
    );
    let total = job["progress_total"].as_i64().unwrap();
    assert!(total >= 2, "{job}");
    assert_eq!(job["progress_current"], total);
    assert_eq!(job["proposals"].as_array().unwrap().len(), 3, "{job}");

    // Deterministic deduplication collapsed the duplicated Beta quote.
    let raw: String = sqlx::query_scalar("SELECT raw_text FROM source_variant WHERE id=?")
        .bind(source["variant_id"].as_str().unwrap())
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    let batch = job["proposal_batch_id"].as_str().unwrap();
    let items: Vec<(String, String)> = sqlx::query_as(
        "SELECT proposal_kind,candidate_json FROM proposal_item WHERE batch_id=? ORDER BY proposal_kind",
    )
    .bind(batch)
    .fetch_all(workspace.pool())
    .await
    .unwrap();
    assert_eq!(items.len(), 3);
    for (kind, candidate) in items {
        let candidate: Value = serde_json::from_str(&candidate).unwrap();
        let expected = match kind.as_str() {
            "signal" => ("Alpha uses the new hiring pipeline.", "Dana Lee"),
            "interest" => ("Beta mentioned the quarterly planning review.", ""),
            "hook" => ("Gamma coordinates the onboarding workbook.", ""),
            other => panic!("unexpected kind {other}"),
        };
        let start = candidate["source_start"].as_i64().unwrap() as usize;
        let end = candidate["source_end"].as_i64().unwrap() as usize;
        // The staged span is an exact slice of the ORIGINAL capture bytes.
        assert_eq!(&raw[start..end], expected.0);
        assert_eq!(
            candidate["mention_name"].as_str().unwrap_or_default(),
            expected.1
        );
        assert_eq!(
            candidate["normalization_version"].as_i64().unwrap(),
            parley_domain::evidence::NORMALIZATION_VERSION as i64
        );
    }
    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn ar06_cancel_or_bad_chunk_leaves_saved_source_only() {
    // Cancellation phase: hold the second chunk, cancel, and prove no
    // candidate batch can install and the source remains retrievable.
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let auth = (&cookie, &token, &epoch);
    let mut provider = ModelServer::with_responder(chunk_responder(), Some(1)).await;
    install_qualified_profile_with(workspace.pool(), &path, provider.endpoint(), 4096, 256, 64)
        .await;

    let (status, source) = request(
        service.address.port(),
        auth,
        "POST",
        "/api/v1/captures",
        Some(json!({"text":multi_chunk_source()})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 201, "{source}");
    let job_id = source["job_id"].as_str().unwrap().to_owned();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if provider.calls() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let (status, _) = request(
        service.address.port(),
        auth,
        "POST",
        &format!("/api/v1/jobs/{job_id}"),
        None,
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 204);
    provider.release().await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    let state: String = sqlx::query_scalar("SELECT status FROM durable_job WHERE id=?")
        .bind(&job_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(state, "cancelled");
    let batches: i64 = sqlx::query_scalar("SELECT count(*) FROM proposal_batch WHERE job_id=?")
        .bind(&job_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(batches, 0);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM source_occurrence WHERE id=?")
        .bind(source["occurrence_id"].as_str().unwrap())
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1);
    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn ar06_bad_chunk_invalidates_the_candidate_batch() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let auth = (&cookie, &token, &epoch);
    let provider = ModelServer::start(vec![
        Reply::Final("not-json".to_string()),
        Reply::Final("not-json".to_string()),
        Reply::Final("not-json".to_string()),
    ])
    .await;
    install_qualified_profile_with(workspace.pool(), &path, provider.endpoint(), 4096, 256, 64)
        .await;

    let (status, source) = request(
        service.address.port(),
        auth,
        "POST",
        "/api/v1/captures",
        Some(json!({"text":"A small capture with a deliberately broken model reply."})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 201, "{source}");
    let job_id = source["job_id"].as_str().unwrap().to_owned();
    let job = wait_terminal(service.address.port(), auth, &job_id).await;
    assert_eq!(job["status"], "invalid", "{job}");
    assert_eq!(job["last_error_code"], "RESULT_INVALID");
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM job_attempt WHERE job_id=?")
        .bind(&job_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(attempts, 3);
    let batches: i64 = sqlx::query_scalar("SELECT count(*) FROM proposal_batch WHERE job_id=?")
        .bind(&job_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(batches, 0);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM source_occurrence WHERE id=?")
        .bind(source["occurrence_id"].as_str().unwrap())
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1);
    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn ar06_identical_source_keeps_independent_scope_edges() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let auth = (&cookie, &token, &epoch);
    let text = "The team is expanding onboarding with Dana Lee.";
    let provider = ModelServer::start(vec![extractor_reply(text), extractor_reply(text)]).await;
    install_qualified_profile_with(workspace.pool(), &path, provider.endpoint(), 4096, 256, 64)
        .await;

    let (status, party) = request(
        service.address.port(),
        auth,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"person","name":"Dana Lee"})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 201);
    let party_id = party["id"].as_str().unwrap();

    let (status, first) = request(
        service.address.port(),
        auth,
        "POST",
        "/api/v1/captures",
        Some(json!({"text":text})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 201, "{first}");
    let (status, second) = request(
        service.address.port(),
        auth,
        "POST",
        "/api/v1/captures",
        Some(json!({"text":text})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 201, "{second}");
    assert_ne!(first["occurrence_id"], second["occurrence_id"]);
    assert_ne!(first["job_id"], second["job_id"]);

    let job_a = wait_terminal(
        service.address.port(),
        auth,
        first["job_id"].as_str().unwrap(),
    )
    .await;
    let job_b = wait_terminal(
        service.address.port(),
        auth,
        second["job_id"].as_str().unwrap(),
    )
    .await;
    assert_eq!(job_a["status"], "succeeded");
    assert_eq!(job_b["status"], "succeeded");
    let batch_a = job_a["proposal_batch_id"].as_str().unwrap();
    let batch_b = job_b["proposal_batch_id"].as_str().unwrap();
    assert_ne!(batch_a, batch_b);

    // Second capture's row stays untouched while the first is reviewed.
    let item_a: String =
        sqlx::query_scalar("SELECT id FROM proposal_item WHERE batch_id=? LIMIT 1")
            .bind(batch_a)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    let (status, result) = request(
        service.address.port(),
        auth,
        "POST",
        &format!("/api/v1/proposals/{batch_a}/resolve"),
        Some(json!({"decisions":[{"item_id":item_a,"action":"confirm","target_entity_id":party_id}]})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["status"], "applied");
    let claims: i64 =
        sqlx::query_scalar("SELECT count(*) FROM claim WHERE predicate LIKE 'proposal:%'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(claims, 1);
    let decisions: i64 = sqlx::query_scalar("SELECT count(*) FROM proposal_decision")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(decisions, 1);
    let item_b_status: String =
        sqlx::query_scalar("SELECT status FROM proposal_item WHERE batch_id=?")
            .bind(batch_b)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(item_b_status, "pending");
    let b_occurrence: String =
        sqlx::query_scalar("SELECT occurrence_id FROM proposal_item WHERE batch_id=? LIMIT 1")
            .bind(batch_b)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(b_occurrence, second["occurrence_id"].as_str().unwrap());
    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn ar06_capture_admission_is_atomic_and_typed() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let provider = ModelServer::start(vec![extractor_reply(
        "Concurrent same-key captures commit exactly one occurrence and one job.",
    )])
    .await;
    install_qualified_profile_with(workspace.pool(), &path, provider.endpoint(), 4096, 256, 64)
        .await;
    let key = fresh_key();
    let text = "Concurrent same-key captures commit exactly one occurrence and one job.";
    let body = json!({"text":text}).to_string();
    let (port, method, pathname) = (service.address.port(), "POST", "/api/v1/captures");
    let mut streams = Vec::new();
    for _ in 0..2 {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(format!("{method} {pathname} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {}\r\nX-Parley-Token: {}\r\nX-Parley-Epoch: {}\r\nIdempotency-Key: {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", cookie, token, epoch, body.len()).as_bytes()).await.unwrap();
        streams.push(stream);
    }
    let mut responses = Vec::new();
    for mut stream in streams {
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
            .to_owned();
        let payload: Value = serde_json::from_str(payload).unwrap();
        responses.push((status, payload.get("data").cloned().unwrap_or(payload)));
    }
    assert_eq!(responses[0].0, "201");
    assert_eq!(responses[1].0, "201");
    assert_eq!(responses[0].1["job_id"], responses[1].1["job_id"]);
    assert_eq!(
        responses[0].1["occurrence_id"],
        responses[1].1["occurrence_id"]
    );
    assert!(responses[0].1["job_id"].as_str().is_some());

    let occurrences: i64 = sqlx::query_scalar("SELECT count(*) FROM source_occurrence")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(occurrences, 1);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM durable_job WHERE role='extractor'")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    service.shutdown().await;
    let _ = fs::remove_file(path);
}

fn extractor_reply(quote: &str) -> Reply {
    Reply::Final(
        json!({"candidates":[{"kind":"signal","statement":quote,"quote":quote,"mention_name":"Dana Lee","mention_kind":"person"}],"injection_suspected":false})
            .to_string(),
    )
}
