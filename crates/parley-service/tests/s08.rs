use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

#[path = "support/mod.rs"]
mod support;

type OwnerSession = (String, String, String);
static NEXT: AtomicU64 = AtomicU64::new(0);
static SESSIONS: OnceLock<Mutex<std::collections::HashMap<u16, OwnerSession>>> = OnceLock::new();
static REQUESTS: AtomicU64 = AtomicU64::new(0);
fn db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-s04-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
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
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\nIdempotency-Key: s04-{key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
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

/// Scripted upstream speaking the real provider wire through the packaged
/// worker's private relay (AR08). ModelServer answers the catalog
/// observations itself; responder index 0 is the first chat completion, and
/// its content is the role-schema payload.
async fn briefer_provider(
    workspace: &Workspace,
    path: &std::path::Path,
) -> support::model_server::ModelServer {
    let server = support::model_server::ModelServer::with_responder(
        Box::new(|_index, _body| {
            json!({
                "message":{"content":json!({
                    "response":"I found no direct source excerpt about that yet. What should I help you inspect next?",
                    "items":[{"kind":"jargon","text":"Onboarding is a term used for helping a new user get started.","detail":{"term":"onboarding","plain_meaning":"Helping a new user get started.","use_or_avoid":"owner_decides","applies_to":"role"},"tier":2,"quote":null,"check_suggestion":"Check the party's own wording in the supplied professional material."}]
                }).to_string()},
                "prompt_eval_count":20,"eval_count":15
            })
        }),
        None,
    )
    .await;
    support::model_server::install_qualified_profile(workspace.pool(), path, server.endpoint())
        .await;
    server
}

#[tokio::test]
async fn s08_seeds_versioned_defaults_and_keeps_prompt_and_question_versions_immutable() {
    let (path, workspace, port, service) = setup().await;
    let defaults: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM agent_prompt_active WHERE role IN ('interviewer','briefer')),(SELECT count(*) FROM interviewer_question_bank_active),(SELECT count(*) FROM interviewer_question_bank_version)",
    ).fetch_one(workspace.pool()).await.unwrap();
    assert_eq!(defaults, (2, 1, 1));

    let (status, created) = request(
        port,
        "POST",
        "/api/v1/agent/chats",
        Some(json!({
            "role":"interviewer","scope_kind":"owner","asset_kind":"proof"
        })),
    )
    .await;
    assert_eq!(status, 201, "{created:?}");
    let chat_id = created["id"].as_str().unwrap();
    let (status, chat) =
        request(port, "GET", &format!("/api/v1/agent/chats/{chat_id}"), None).await;
    assert_eq!(status, 200, "{chat:?}");
    assert_eq!(chat["role"], "interviewer");
    assert_eq!(chat["turns"], json!([]));
    let (status, listed_chats) = request(port, "GET", "/api/v1/agent/chats", None).await;
    assert_eq!(status, 200, "{listed_chats:?}");
    assert_eq!(listed_chats.as_array().unwrap().len(), 1);
    assert_eq!(listed_chats[0]["id"], chat_id);

    let (status, bank) = request(port, "GET", "/api/v1/agent/question-bank", None).await;
    assert_eq!(status, 200, "{bank:?}");
    assert!(
        bank["content"]["questions"]["discovery"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "third_party_interest")
    );
    let old_bank_id = bank["version_id"].as_str().unwrap().to_owned();
    let mut changed_bank = bank["content"].clone();
    changed_bank["questions"]["proof"][0]["text"] = json!("What measured result changed?");
    let (status, activated) = request(
        port,
        "PUT",
        "/api/v1/agent/question-bank",
        Some(json!({
            "expected_version_id":old_bank_id,"content":changed_bank
        })),
    )
    .await;
    assert_eq!(status, 200, "{activated:?}");
    assert_ne!(activated["version_id"], old_bank_id);
    let versions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM interviewer_question_bank_version")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(versions, 2);

    let (status, prompt) = request(port, "GET", "/api/v1/agent/prompts/interviewer", None).await;
    assert_eq!(status, 200, "{prompt:?}");
    let old_prompt_id = prompt["version_id"].as_str().unwrap().to_owned();
    let new_text = format!(
        "{}\nOwner override: ask one concise question.",
        prompt["content"].as_str().unwrap()
    );
    let (status, updated) = request(
        port,
        "PUT",
        "/api/v1/agent/prompts/interviewer",
        Some(json!({
            "expected_version_id":old_prompt_id,"content":new_text
        })),
    )
    .await;
    assert_eq!(status, 200, "{updated:?}");
    assert_ne!(updated["version_id"], old_prompt_id);
    let versions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM agent_prompt_version WHERE role='interviewer'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(versions, 2);
    let old_prompt: String =
        sqlx::query_scalar("SELECT prompt_text FROM agent_prompt_version WHERE version_id=?")
            .bind(old_prompt_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert!(!old_prompt.contains("Owner override"));

    let (status, fluency) = request(
        port,
        "PUT",
        "/api/v1/fluency/onboarding",
        Some(json!({"status":"learning","expected_revision":0})),
    )
    .await;
    assert_eq!(status, 200, "{fluency:?}");
    assert_eq!(fluency["status"], "learning");
    assert_eq!(fluency["revision"], 1);
    let (status, terms) = request(port, "GET", "/api/v1/fluency", None).await;
    assert_eq!(status, 200, "{terms:?}");
    assert_eq!(terms.as_array().unwrap().len(), 1);
    let (status, stale) = request(
        port,
        "PUT",
        "/api/v1/fluency/onboarding",
        Some(json!({"status":"fluent","expected_revision":0})),
    )
    .await;
    assert_eq!(status, 409, "{stale:?}");

    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s08_owner_chat_scope_and_segment_scope_are_distinct_and_revisioned() {
    let (path, workspace, port, service) = setup().await;
    let (status, owner_chat) = request(
        port,
        "POST",
        "/api/v1/agent/chats",
        Some(json!({
            "role":"interviewer","scope_kind":"owner","asset_kind":"gift"
        })),
    )
    .await;
    assert_eq!(status, 201, "{owner_chat:?}");
    let (status, rejected) = request(
        port,
        "POST",
        "/api/v1/agent/chats",
        Some(json!({
            "role":"interviewer","scope_kind":"party","scope_id":"missing","asset_kind":"gift"
        })),
    )
    .await;
    assert_eq!(status, 422, "{rejected:?}");

    let (status, segment) = request(
        port,
        "POST",
        "/api/v1/segments",
        Some(json!({
            "name":"SaaS onboarding","description":"Segment fixture"
        })),
    )
    .await;
    assert_eq!(status, 201, "{segment:?}");
    let segment_id = segment["id"].as_str().unwrap();
    let (status, briefer_chat) = request(
        port,
        "POST",
        "/api/v1/agent/chats",
        Some(json!({
            "role":"briefer","scope_kind":"segment","scope_id":segment_id
        })),
    )
    .await;
    assert_eq!(status, 201, "{briefer_chat:?}");
    assert_ne!(owner_chat["id"], briefer_chat["id"]);
    let (status, listed) = request(port, "GET", "/api/v1/segments", None).await;
    assert_eq!(status, 200, "{listed:?}");
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let (owner_chats, segment_chats): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM agent_chat WHERE role='interviewer' AND scope_kind='owner'),(SELECT count(*) FROM agent_chat WHERE role='briefer' AND scope_kind='segment')",
    ).fetch_one(workspace.pool()).await.unwrap();
    assert_eq!((owner_chats, segment_chats), (1, 1));

    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s08_owner_interviewer_and_party_briefer_histories_stay_separate() {
    let (path, workspace, port, service) = setup().await;
    let party = workspace
        .create_entity("s08-dana", "person", "Dana Lee", "")
        .await
        .unwrap();
    let provider = briefer_provider(&workspace, &path).await;

    let (status, owner_chat) = request(
        port,
        "POST",
        "/api/v1/agent/chats",
        Some(json!({"role":"interviewer","scope_kind":"owner","asset_kind":"proof"})),
    )
    .await;
    assert_eq!(status, 201, "{owner_chat:?}");
    let (status, lead_chat) = request(
        port,
        "POST",
        "/api/v1/agent/chats",
        Some(json!({"role":"briefer","scope_kind":"party","scope_id":party.id})),
    )
    .await;
    assert_eq!(status, 201, "{lead_chat:?}");
    let chat_id = lead_chat["id"].as_str().unwrap();
    let (status, submitted) = request(
        port,
        "POST",
        &format!("/api/v1/agent/chats/{chat_id}/turns"),
        Some(json!({"text":"What does the supplied material say about onboarding?"})),
    )
    .await;
    assert_eq!(status, 202, "{submitted:?}");
    let job_id = submitted["job_id"].as_str().unwrap();
    let mut job = Value::Null;
    for _ in 0..100 {
        let (_, current) = request(port, "GET", &format!("/api/v1/jobs/{job_id}"), None).await;
        job = current;
        if job["status"] == "succeeded" || job["status"] == "failed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(job["status"], "succeeded", "{job:?}");
    let (status, lead_history) =
        request(port, "GET", &format!("/api/v1/agent/chats/{chat_id}"), None).await;
    assert_eq!(status, 200, "{lead_history:?}");
    assert_eq!(lead_history["turns"].as_array().unwrap().len(), 2);
    assert_eq!(lead_history["turns"][0]["speaker"], "owner");
    assert_eq!(lead_history["turns"][1]["speaker"], "agent");
    let (status, items) = request(
        port,
        "GET",
        &format!("/api/v1/briefs/party/{}/items", party.id),
        None,
    )
    .await;
    assert_eq!(status, 200, "{items:?}");
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert_eq!(items[0]["tier"], 2);
    let fluency: Option<(String, String)> =
        sqlx::query_as("SELECT term,status FROM fluency WHERE term='onboarding'")
            .fetch_optional(workspace.pool())
            .await
            .unwrap();
    assert_eq!(fluency, Some(("onboarding".into(), "unknown".into())));
    let (status, owner_history) = request(
        port,
        "GET",
        &format!("/api/v1/agent/chats/{}", owner_chat["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, 200, "{owner_history:?}");
    assert!(owner_history["turns"].as_array().unwrap().is_empty());
    let stored_counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM agent_turn WHERE chat_id=?),(SELECT count(*) FROM agent_turn WHERE chat_id=?)")
        .bind(chat_id).bind(owner_chat["id"].as_str().unwrap()).fetch_one(workspace.pool()).await.unwrap();
    assert_eq!(stored_counts, (2, 0));

    provider.stop().await;
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s08_job_events_carry_no_model_prose() {
    let (path, workspace, port, service) = setup().await;
    let party = workspace
        .create_entity("s08-prose-party", "person", "Dana Lee", "")
        .await
        .unwrap();
    let provider = briefer_provider(&workspace, &path).await;
    let (status, chat) = request(
        port,
        "POST",
        "/api/v1/agent/chats",
        Some(json!({"role":"briefer","scope_kind":"party","scope_id":party.id})),
    )
    .await;
    assert_eq!(status, 201, "{chat:?}");
    let chat_id = chat["id"].as_str().unwrap();
    let (status, submitted) = request(
        port,
        "POST",
        &format!("/api/v1/agent/chats/{chat_id}/turns"),
        Some(json!({"text":"What does the supplied material say about onboarding?"})),
    )
    .await;
    assert_eq!(status, 202, "{submitted:?}");
    let job_id = submitted["job_id"].as_str().unwrap();
    let mut job = Value::Null;
    for _ in 0..100 {
        let (_, current) = request(port, "GET", &format!("/api/v1/jobs/{job_id}"), None).await;
        job = current;
        if job["status"] == "succeeded" || job["status"] == "failed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(job["status"], "succeeded", "{job:?}");
    let payloads: Vec<String> = sqlx::query_scalar(
        "SELECT payload_json FROM event WHERE event_type IN ('job.completed','brief.items_proposed')",
    )
    .fetch_all(workspace.pool())
    .await
    .unwrap();
    assert!(!payloads.is_empty());
    for payload in &payloads {
        let value: Value = serde_json::from_str(payload).unwrap_or_default();
        let prose_keys = [
            "next_question",
            "response",
            "extracted",
            "items",
            "candidates",
            "text",
            "check_suggestion",
        ];
        for key in prose_keys {
            assert!(
                value.get(key).is_none(),
                "immutable event log carries model prose ({key}): {payload}"
            );
        }
    }
    provider.stop().await;
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

async fn request_with_key(
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

#[tokio::test]
async fn s08_candidate_dismiss_is_idempotent_and_evented() {
    // Dismissing a proposed candidate is a private mutation and must follow
    // the full command discipline: one writer transaction, one event and one
    // idempotency receipt. A same-key retry replays the original result and
    // never touches the row twice.
    let (path, workspace, port, service) = setup().await;
    let now = "2026-11-22T10:00:00Z";
    let chat_id = "chat-dismiss-test";
    let job_id = "job-dismiss-test";
    let candidate_id = "candidate-dismiss-test";
    sqlx::query("INSERT INTO agent_chat(id,role,scope_kind,asset_kind,revision,created_at,updated_at) VALUES(?,'interviewer','owner','gift',1,?,?)")
        .bind(chat_id)
        .bind(now)
        .bind(now)
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO durable_job(id,role,status,input_json,input_sha256,scope_kind,scope_id,scope_revision,current_attempt,max_attempts,created_at,updated_at) VALUES(?, 'interviewer','succeeded','{}','seed', 'owner',?,1,0,3,?,?)")
        .bind(job_id)
        .bind(chat_id)
        .bind(now)
        .bind(now)
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO interviewer_candidate(id,chat_id,job_id,asset_kind,fields_json,owner_quotes_json,missing_fields_json,state,created_at,updated_at) VALUES(?,?,?,'gift','{}','{}','[]','proposed',?,?)")
        .bind(candidate_id)
        .bind(chat_id)
        .bind(job_id)
        .bind(now)
        .bind(now)
        .execute(workspace.pool())
        .await
        .unwrap();

    let key = "s08-candidate-dismiss";
    let dismiss_path = format!("/api/v1/agent/candidates/{candidate_id}/dismiss");
    let (status, first) = request_with_key(port, "POST", &dismiss_path, Some(json!({})), key).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["state"], "dismissed");
    let (status, second) =
        request_with_key(port, "POST", &dismiss_path, Some(json!({})), key).await;
    assert_eq!(
        status, 200,
        "{second}: same-key retry must replay, not error"
    );
    assert_eq!(first, second, "retry must return the identical response");

    let state: String = sqlx::query_scalar("SELECT state FROM interviewer_candidate WHERE id=?")
        .bind(candidate_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(state, "dismissed");
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event WHERE event_type='interviewer.candidate_dismissed'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(events, 1, "dismissal must emit exactly one event");
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_receipt WHERE key=?")
        .bind(key)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(receipts, 1, "dismissal must record exactly one receipt");

    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}
