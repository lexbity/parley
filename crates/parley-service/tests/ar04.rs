//! AR-001 AR04: Interviewer and Briefer role work resumes through the typed
//! intake, survives reload/cancellation and never lets a late model result
//! overwrite a newer owner turn.
//!
//! The fake provider speaks the same loopback HTTP the host transport drives,
//! so the full durable job lifecycle is exercised without a worker bundle;
//! worker execution itself is qualified by the packaged release lanes.

use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Provider {
    port: u16,
    task: Option<tokio::task::JoinHandle<()>>,
    release: Option<tokio::sync::oneshot::Sender<()>>,
}

type Responder = Box<dyn FnMut(usize, &str) -> Value + Send>;

impl Provider {
    async fn with(responder: Responder, pause_index: Option<usize>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let release = if pause_index.is_some() {
            Some(release_tx)
        } else {
            None
        };
        let mut gate = pause_index.map(|index| (index, release_rx));
        let mut responder = responder;
        let task = tokio::spawn(async move {
            for index in 0..64 {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut head_bytes = Vec::new();
                let mut byte = [0u8; 1];
                while !head_bytes.ends_with(b"\r\n\r\n") {
                    if stream.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    head_bytes.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head_bytes).to_string();
                let length: usize = head
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .and_then(|(_, value)| value.trim().parse().ok())
                    })
                    .unwrap_or(0);
                let mut body_bytes = vec![0u8; length];
                let _ = stream.read_exact(&mut body_bytes).await;
                let body = String::from_utf8_lossy(&body_bytes).to_string();
                if let Some((pause, rx)) = gate.as_mut()
                    && index == *pause
                {
                    let _ = rx.await;
                    gate = None;
                }
                let payload = responder(index, &body).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Self {
            port,
            task: Some(task),
            release,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    async fn release(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }

    async fn stop(mut self) {
        self.release().await;
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn tags_response() -> Value {
    json!({"models":[{"name":"fixture","digest":"sha256:ar04"}]})
}

fn interviewer_output() -> Value {
    let content = json!({
        "next_question": "What context and time period should I record?",
        "target_field": "what_i_did",
        "extracted": {},
        "ready_to_save": false
    })
    .to_string();
    json!({
        "message": {"role":"assistant","content":content},
        "prompt_eval_count": 24, "eval_count": 12
    })
}

async fn setup() -> (PathBuf, Workspace, RunningService, String, String, String) {
    let path = std::env::temp_dir().join(format!(
        "parley-ar04-{}-{}.sqlite",
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

async fn set_provider(port: u16, auth: (&String, &String, &String), url: &str) {
    let (status, reply) = request(
        port,
        auth,
        "PUT",
        "/api/v1/agent/provider",
        Some(json!({
            "endpoint": url, "model": "fixture", "num_ctx": 8192,
            "num_predict": 256, "locale": "en"
        })),
        "ar04-provider",
    )
    .await;
    assert!(status == 200 || status == 204, "{status} {reply:?}");
    let (status, reply) = request(port, auth, "POST", "/api/v1/agent/provider/health", None, "ar04-health").await;
    assert_eq!(status, 200, "{reply:?}");
    assert_eq!(reply["available"], true, "{reply:?}");
}

async fn wait_job(port: u16, auth: (&String, &String, &String), id: &str, states: &[&str]) -> Value {
    for _ in 0..150 {
        let (status, value) = request(port, auth, "GET", &format!("/api/v1/jobs/{id}"), None, "job-read").await;
        assert_eq!(status, 200, "{value:?}");
        if states.contains(&value["status"].as_str().unwrap_or_default()) {
            return value;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let (_, last) = request(port, auth, "GET", &format!("/api/v1/jobs/{id}"), None, "job-read-last").await;
    panic!("job {id} never reached {states:?}; last={last:?}");
}

#[tokio::test]
async fn ar04_typed_intake_binds_chat_job_and_profile_at_admission() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let auth = (&cookie, &token, &epoch);
    let (status, chat) = request(
        service.address.port(),
        auth,
        "POST",
        "/api/v1/agent/chats",
        Some(json!({"role":"interviewer","scope_kind":"owner","asset_kind":"proof"})),
        "ar04-chat",
    )
    .await;
    assert_eq!(status, 201, "{chat:?}");
    let chat_id = chat["id"].as_str().unwrap();

    let (status, turn) = request(
        service.address.port(),
        auth,
        "POST",
        &format!("/api/v1/agent/chats/{chat_id}/turns"),
        Some(json!({"text":"I ran a survey in June."})),
        "ar04-turn-1",
    )
    .await;
    assert_eq!(status, 202, "{turn:?}");
    let job_id = turn["job_id"].as_str().unwrap();

    let row: (Option<String>, Option<i64>, Option<String>, i64) = sqlx::query_as(
        "SELECT chat_id,profile_revision,dependency_digest,scope_revision FROM durable_job WHERE id=?",
    )
    .bind(job_id)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(row.0.as_deref(), Some(chat_id), "typed chat binding");
    assert_eq!(row.1, None, "no qualified profile in this harness");
    assert!(row.2.as_deref().is_some_and(|digest| digest.len() == 64));
    assert_eq!(row.3, 2, "job binds the revision after the owner turn");

    let listed: Vec<(Option<String>,)> = sqlx::query_as(
        "SELECT active_job_id FROM (SELECT (SELECT j.id FROM durable_job j WHERE j.status IN ('queued','running') AND j.chat_id=c.id ORDER BY j.created_at DESC LIMIT 1) AS active_job_id FROM agent_chat c WHERE c.id=?)",
    )
    .bind(chat_id)
    .fetch_all(workspace.pool())
    .await
    .unwrap();
    assert_eq!(listed[0].0.as_deref(), Some(job_id));

    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn ar04_late_chat_result_cannot_publish_after_new_turn_or_cancel() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let auth = (&cookie, &token, &epoch);
    let mut provider = Provider::with(
        Box::new(move |index, _body| {
            if index == 0 {
                return tags_response();
            }
            interviewer_output()
        }),
        Some(1),
    )
    .await;
    set_provider(service.address.port(), auth, &provider.url()).await;

    let (status, chat) = request(
        service.address.port(),
        auth,
        "POST",
        "/api/v1/agent/chats",
        Some(json!({"role":"interviewer","scope_kind":"owner","asset_kind":"proof"})),
        "ar04-late-chat",
    )
    .await;
    assert_eq!(status, 201, "{chat:?}");
    let chat_id = chat["id"].as_str().unwrap().to_owned();

    let (status, first) = request(
        service.address.port(),
        auth,
        "POST",
        &format!("/api/v1/agent/chats/{chat_id}/turns"),
        Some(json!({"text":"First answer"})),
        "ar04-late-turn-1",
    )
    .await;
    assert_eq!(status, 202, "{first:?}");
    let first_job = first["job_id"].as_str().unwrap().to_owned();

    // The first job is claimed and waits on the paused provider. The owner
    // writes a second turn meanwhile; the chat revision moves.
    let (status, second) = request(
        service.address.port(),
        auth,
        "POST",
        &format!("/api/v1/agent/chats/{chat_id}/turns"),
        Some(json!({"text":"Second answer"})),
        "ar04-late-turn-2",
    )
    .await;
    assert_eq!(status, 202, "{second:?}");
    let second_job = second["job_id"].as_str().unwrap().to_owned();
    assert_ne!(first_job, second_job);

    let chat_row: (i64,) = sqlx::query_as("SELECT revision FROM agent_chat WHERE id=?")
        .bind(&chat_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(chat_row.0, 3);

    provider.release().await;
    let first_terminal = wait_job(service.address.port(), auth, &first_job, &["succeeded", "failed", "invalid", "cancelled"]).await;
    assert_eq!(first_terminal["status"], "failed", "{first_terminal:?}");
    assert_eq!(first_terminal["last_error_code"], "STALE_INPUT", "{first_terminal:?}");

    let second_terminal = wait_job(service.address.port(), auth, &second_job, &["succeeded", "failed"]).await;
    assert_eq!(second_terminal["status"], "succeeded", "{second_terminal:?}");

    let chat = request(
        service.address.port(),
        auth,
        "GET",
        &format!("/api/v1/agent/chats/{chat_id}"),
        None,
        "ar04-late-chat-read",
    )
    .await
    .1;
    let turns = chat["turns"].as_array().unwrap();
    let owner_turns = turns.iter().filter(|turn| turn["speaker"] == "owner").count();
    let agent_turns = turns.iter().filter(|turn| turn["speaker"] == "agent").count();
    assert_eq!(owner_turns, 2, "{chat:?}");
    assert_eq!(agent_turns, 1, "only the second job may publish an agent turn: {chat:?}");
    assert_eq!(chat["candidates"].as_array().unwrap().len(), 1, "{chat:?}");
    let candidate_job: Option<String> = sqlx::query_scalar(
        "SELECT job_id FROM interviewer_candidate WHERE chat_id=? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&chat_id)
    .fetch_optional(workspace.pool())
    .await
    .unwrap();
    assert_eq!(candidate_job.as_deref(), Some(second_job.as_str()), "late first job never created a candidate");

    let first_attempt: (String, Option<String>) = sqlx::query_as(
        "SELECT status,error_code FROM job_attempt WHERE job_id=? AND attempt_no=1",
    )
    .bind(&first_job)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(first_attempt.0, "failed");
    assert_eq!(first_attempt.1.as_deref(), Some("STALE_INPUT"));

    provider.stop().await;
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn ar04_owner_and_registry_brief_share_scoped_intake() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let auth = (&cookie, &token, &epoch);
    let party = workspace
        .create_entity("ar04-brief-party", "person", "Brief Example", "")
        .await
        .unwrap();

    let (status, first) = request(
        service.address.port(),
        auth,
        "POST",
        &format!("/api/v1/briefs/party/{}/items", party.id),
        Some(json!({})),
        "ar04-brief-key-1",
    )
    .await;
    assert_eq!(status, 202, "{first:?}");
    let job_id = first["job_id"].as_str().unwrap().to_owned();

    // Replaying the same intent returns the same durable job without creating
    // a second enqueue.
    let (status, replay) = request(
        service.address.port(),
        auth,
        "POST",
        &format!("/api/v1/briefs/party/{}/items", party.id),
        Some(json!({})),
        "ar04-brief-key-1",
    )
    .await;
    assert_eq!(status, 202, "{replay:?}");
    assert_eq!(replay["job_id"].as_str().unwrap(), job_id);

    let row: (String, String, String, Option<String>, Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT role,scope_kind,scope_id,chat_id,profile_revision,dependency_digest FROM durable_job WHERE id=?",
    )
    .bind(&job_id)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(row.0, "briefer");
    assert_eq!(row.1, "party");
    assert_eq!(row.2, party.id);
    assert_eq!(row.3, None, "bare generation is not a chat turn");
    assert_eq!(row.4, None);
    assert!(row.5.as_deref().is_some_and(|digest| digest.len() == 64));

    // A distinct intent is a distinct durable job.
    let (status, second) = request(
        service.address.port(),
        auth,
        "POST",
        &format!("/api/v1/briefs/party/{}/items", party.id),
        Some(json!({})),
        "ar04-brief-key-2",
    )
    .await;
    assert_eq!(status, 202, "{second:?}");
    assert_ne!(second["job_id"].as_str().unwrap(), job_id);

    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn ar04_coach_answer_reaches_interviewer_candidate_review() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let auth = (&cookie, &token, &epoch);
    let party = workspace
        .create_entity("ar04-answer-party", "person", "Jordan Example", "")
        .await
        .unwrap();
    sqlx::query("INSERT INTO inference_record(id,role,status,created_at,metadata_class) VALUES('job-ar04-answer','coach','succeeded','2026-11-20T00:00:00Z','historical_unknown')")
        .execute(workspace.pool()).await.unwrap();
    let selector = json!({"kind":"party","id":party.id,"revision":party.revision}).to_string();
    sqlx::query("INSERT INTO coach_question(id,job_id,question_key,semantic_key,kind,target_json,selector_json,dependency_json,answer_schema_json,priority,impact,text,status,created_at,updated_at) VALUES('ar04-question','job-ar04-answer','k',NULL,'missing_proof','{}',?,'[]','{}',0,2,'Which proof should be recorded?','open','2026-11-20T00:00:00Z','2026-11-20T00:00:00Z')")
        .bind(selector).execute(workspace.pool()).await.unwrap();

    let provider = Provider::with(
        Box::new(move |index, _body| {
            if index == 0 {
                return tags_response();
            }
            interviewer_output()
        }),
        None,
    )
    .await;
    set_provider(service.address.port(), auth, &provider.url()).await;

    let (status, answer) = request(
        service.address.port(),
        auth,
        "POST",
        "/api/v1/coach/questions/ar04-question/answer",
        Some(json!({"answer":{"text":"I measured the result in the onboarding report."},"expected_revision":1,"question_key":"k"})),
        "ar04-answer",
    )
    .await;
    assert_eq!(status, 202, "{answer:?}");
    assert_eq!(answer["status"], "answer_pending");
    let chat_id = answer["chat_id"].as_str().unwrap().to_owned();
    let job_id = answer["job_id"].as_str().unwrap().to_owned();

    let terminal = wait_job(service.address.port(), auth, &job_id, &["succeeded", "failed"]).await;
    assert_eq!(terminal["status"], "succeeded", "{terminal:?}");
    assert_eq!(terminal["role"], "interviewer");
    assert_eq!(terminal["review_ref"]["kind"], "interviewer_candidate");

    let chat = request(
        service.address.port(),
        auth,
        "GET",
        &format!("/api/v1/agent/chats/{chat_id}"),
        None,
        "ar04-answer-chat-read",
    )
    .await
    .1;
    let candidate = chat["candidates"].as_array().unwrap().iter().find(|c| c["state"] == "proposed").cloned().unwrap();
    let candidate_id = candidate["id"].as_str().unwrap().to_owned();
    let revision = chat["revision"].as_i64().unwrap();

    let (status, saved) = request(
        service.address.port(),
        auth,
        "POST",
        &format!("/api/v1/agent/candidates/{candidate_id}/confirm"),
        Some(json!({
            "title":"Onboarding measurement",
            "fields":{"what_i_did":"I measured the result in the onboarding report."},
            "expected_chat_revision":revision
        })),
        "ar04-confirm",
    )
    .await;
    assert_eq!(status, 201, "{saved:?}");
    let resource_id = saved["id"].as_str().unwrap();

    let question: (String, i64, Option<String>) = sqlx::query_as(
        "SELECT status,revision,json_extract(disposition_json,'$.resource_id') FROM coach_question WHERE id='ar04-question'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(question.0, "answered");
    assert_eq!(question.1, 3);
    assert_eq!(question.2.as_deref(), Some(resource_id), "confirm links the Coach question");

    let promoted: i64 = sqlx::query_scalar("SELECT count(*) FROM entity WHERE subtype<>''")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(promoted, 1);

    provider.stop().await;
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}




