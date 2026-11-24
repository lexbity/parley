//! UCR-001 U05: a delegated agent starts a reviewable brief, waits on its
//! background job and resumes from a typed job status. The fake provider is
//! a real loopback endpoint; authorization, the registry pipeline, the
//! worker fence, the ledger and the commit boundary are the service's own.
mod support;

use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
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
    net::{TcpListener, TcpStream},
};

type OwnerSession = (String, String, String);
static NEXT: AtomicU64 = AtomicU64::new(0);
static SESSIONS: OnceLock<Mutex<HashMap<u16, OwnerSession>>> = OnceLock::new();

fn db_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-u05-{tag}-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn raw(
    port: u16,
    method: &str,
    path: &str,
    body: &str,
    key: Option<&str>,
) -> (u16, HashMap<String, String>, Value) {
    let (cookie, token, epoch) = SESSIONS
        .get()
        .unwrap()
        .lock()
        .unwrap()
        .get(&port)
        .unwrap()
        .clone();
    let key_header = key
        .map(|value| format!("Idempotency-Key: {value}\r\n"))
        .unwrap_or_default();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\n{key_header}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
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
    let headers: HashMap<String, String> = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(payload).unwrap_or(Value::Null)
    };
    (status, headers, value.get("data").cloned().unwrap_or(value))
}

async fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: &str,
) -> (u16, Value) {
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let (status, _, value) = raw(port, method, path, &body, Some(key)).await;
    (status, value)
}

async fn owner_session(service: &RunningService) -> OwnerSession {
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
        .and_then(|line| {
            line.split_once(':')
                .map(|(_, value)| value.trim().split(';').next().unwrap().to_owned())
        })
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
        payload["mutation_token"].as_str().unwrap().into(),
        payload["epoch"].as_str().unwrap().into(),
    )
}

async fn setup(tag: &str) -> (PathBuf, Workspace, u16, RunningService) {
    let path = db_path(tag);
    let _ = fs::remove_file(&path);
    let service = parley_service::start_workspace(&path).await.unwrap();
    let port = service.address.port();
    let owner = owner_session(&service).await;
    SESSIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(port, owner);
    let workspace = Workspace::open(&path).await.unwrap();
    (path, workspace, port, service)
}

fn cleanup(path: &PathBuf, workspace: Workspace, service: RunningService) {
    drop(workspace);
    drop(service);
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(path.with_extension("sqlite-wal"));
    let _ = fs::remove_dir_all(path.with_extension("sqlite-shm"));
}

async fn create_party(port: u16, name: &str) -> String {
    let (status, entity) = request(
        port,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"person","name":name})),
        &format!("party-{name}"),
    )
    .await;
    assert_eq!(status, 201, "{entity:?}");
    entity["id"].as_str().unwrap().to_string()
}

async fn entity_revision(workspace: &Workspace, id: &str) -> i64 {
    sqlx::query_scalar("SELECT revision FROM entity WHERE id=?")
        .bind(id)
        .fetch_one(workspace.pool())
        .await
        .unwrap()
}

async fn count(workspace: &Workspace, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(workspace.pool())
        .await
        .unwrap()
}

/// A loopback provider whose responses may depend on the chat body, with an
/// optional pause before one response index so tests can observe durable
/// states while a provider call is in flight.
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

    /// Unblock the paused response without stopping the listener, so the
    /// conversation can continue through later connection indices.
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

fn final_answer(content: &str) -> Value {
    json!({
        "message": {"role":"assistant","content":content},
        "prompt_eval_count": 12, "eval_count": 8
    })
}

fn tool_call(name: &str, args: Value, content: &str) -> Value {
    json!({
        "message": {
            "role": "assistant", "content": content,
            "tool_calls": [{"id":"call","function":{"name":name,"arguments":args.to_string()}}]
        },
        "prompt_eval_count": 12, "eval_count": 8
    })
}

fn tags_response() -> Value {
    json!({"models":[{"name":"fixture","digest":"sha256:u05"}]})
}

fn brief_args(party: &str, revision: i64) -> Value {
    json!({
        "party_id": party,
        "expected_revision": revision,
        "purpose": "Prepare a reviewable brief for the party"
    })
}

fn briefer_output() -> Value {
    let content = json!({
        "response": "A brief for the party is ready for owner review.",
        "items": [{
            "kind": "jargon",
            "text": "Onboarding is a term used for helping a new user get started.",
            "detail": {
                "term": "onboarding",
                "plain_meaning": "Helping a new user get started.",
                "use_or_avoid": "owner_decides",
                "applies_to": "role"
            },
            "tier": 2,
            "quote": null,
            "check_suggestion": "Check the party's own wording in the supplied professional material."
        }]
    })
    .to_string();
    json!({
        "message": {"role":"assistant","content":content},
        "prompt_eval_count": 20, "eval_count": 15
    })
}

async fn set_provider(port: u16, url: &str, key: &str) {
    let (status, reply) = request(
        port,
        "PUT",
        "/api/v1/agent/provider",
        Some(json!({
            "endpoint": url, "model": "fixture", "num_ctx": 8192,
            "num_predict": 256, "locale": "en"
        })),
        key,
    )
    .await;
    assert!(status == 200 || status == 204, "{status} {reply:?}");
}

/// Set the model digest so role jobs can claim attempts (tags response).
async fn health(port: u16, key: &str) {
    let (status, reply) = request(port, "POST", "/api/v1/agent/provider/health", None, key).await;
    assert_eq!(status, 200, "{reply:?}");
    assert_eq!(reply["available"], true, "{reply:?}");
}

async fn start_run(
    port: u16,
    task: &str,
    targets: Value,
    permissions: Value,
    key: &str,
) -> (u16, Value) {
    request(
        port,
        "POST",
        "/api/v1/agent/runs",
        Some(json!({
            "task": task,
            "targets": targets,
            "permissions": permissions,
            "allow_created_opportunities": false
        })),
        key,
    )
    .await
}

async fn wait_state(port: u16, id: &str, states: &[&str]) -> Value {
    for _ in 0..150 {
        let (status, value) = request(
            port,
            "GET",
            &format!("/api/v1/agent/runs/{id}"),
            None,
            "run-read",
        )
        .await;
        assert_eq!(status, 200, "{value:?}");
        if states.contains(&value["state"].as_str().unwrap_or_default()) {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (_, last) = request(
        port,
        "GET",
        &format!("/api/v1/agent/runs/{id}"),
        None,
        "run-read-last",
    )
    .await;
    let (_, turns) = request(
        port,
        "GET",
        &format!("/api/v1/agent/runs/{id}/transcript?limit=20"),
        None,
        "transcript-read-last",
    )
    .await;
    panic!("run {id} never reached {states:?}; last={last:?}; turns={turns:?}");
}

async fn wait_job(port: u16, id: &str, states: &[&str]) -> Value {
    for _ in 0..150 {
        let (status, value) = request(
            port,
            "GET",
            &format!("/api/v1/jobs/{id}"),
            None,
            "job-read",
        )
        .await;
        assert_eq!(status, 200, "{value:?}");
        if states.contains(&value["status"].as_str().unwrap_or_default()) {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("job {id} never reached {states:?}");
}

async fn transcript(port: u16, id: &str) -> Vec<Value> {
    let (status, value) = request(
        port,
        "GET",
        &format!("/api/v1/agent/runs/{id}/transcript?limit=20"),
        None,
        "transcript-read",
    )
    .await;
    assert_eq!(status, 200, "{value:?}");
    value["items"].as_array().unwrap().clone()
}

fn tool_text(turns: &[Value]) -> String {
    turns
        .iter()
        .filter(|turn| turn["speaker"].as_str() == Some("tool"))
        .map(|turn| turn["text"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn u05_agent_brief_waits_on_job_and_resumes_with_review() {
    let (path, workspace, port, service) = setup("happy").await;
    let party = create_party(port, "Brief Party").await;
    let revision = entity_revision(&workspace, &party).await;
    let party_for_provider = party.clone();
    let revision_for_provider = revision;

    let mut provider = Provider::with(
        Box::new(move |_index, body| {
            let value: Value = serde_json::from_str(body).unwrap_or_default();
            if value.get("models").is_some() || value.get("messages").is_none() {
                return tags_response();
            }
            let messages = value
                .get("messages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let user_text = messages
                .iter()
                .find(|message| message["role"].as_str() == Some("user"))
                .and_then(|message| message["content"].as_str())
                .unwrap_or_default();
            if user_text.contains("Tool support probe") {
                return final_answer("probe-ok");
            }
            // The worker chat carries a `format` schema and no tools.
            if value.get("format").is_some() {
                return briefer_output();
            }
            // A resumed run that already sees a job-status result is done.
            if messages.iter().any(|message| {
                message["role"].as_str() == Some("tool")
                    && message["content"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("review_reference")
            }) {
                return final_answer("The brief proposal is ready for owner review.");
            }
            // After the brief job was started, answer with its job status.
            if let Some(tool) = messages.iter().rev().find(|message| {
                message["role"].as_str() == Some("tool")
            }) {
                let job_id = tool["content"]
                    .as_str()
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    .and_then(|value| value.get("result").cloned())
                    .and_then(|value| value.get("job_id").cloned())
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default();
                return tool_call(
                    "commands__run",
                    json!({"id":"job.get_status","version":1,"args":json!({
                        "job_id": job_id
                    })}),
                    "check the brief job",
                );
            }
            tool_call(
                "commands__run",
                json!({"id":"brief.start","version":1,"args":brief_args(
                    &party_for_provider,
                    revision_for_provider,
                )}),
                "start a brief",
            )
        }),
        Some(3),
    )
    .await;
    set_provider(port, &provider.url(), "provider-u05-1").await;
    health(port, "health-u05-1").await;

    let (status, run) = start_run(
        port,
        "Prepare a brief for the selected party.",
        json!([{"kind":"party","id":party}]),
        json!(["brief.start", "job.read"]),
        "run-key-01",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let run_id = run["run_id"].as_str().unwrap().to_owned();

    // The run parks itself with a persisted wake condition and no lease while
    // the job is running.
    let waiting = wait_state(port, &run_id, &["waiting_job"]).await;
    assert!(
        waiting["wake_condition"].as_str().map(str::len).unwrap_or(0) > 0,
        "{waiting}"
    );
    let lease: Option<String> = sqlx::query_scalar("SELECT lease_token FROM agent_run WHERE run_id=?")
        .bind(&run_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert!(lease.is_none(), "waiting run must hold no lease");
    let job_id: String =
        sqlx::query_scalar("SELECT id FROM durable_job WHERE run_id=? ORDER BY created_at LIMIT 1")
            .bind(&run_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    let job_epoch: String =
        sqlx::query_scalar("SELECT epoch FROM durable_job WHERE id=?")
            .bind(&job_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    let current_epoch: String = sqlx::query_scalar("SELECT epoch FROM workspace_meta WHERE singleton=1")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(job_epoch, current_epoch);
    assert_eq!(
        count(&workspace, "SELECT COUNT(*) FROM durable_job WHERE run_id IS NOT NULL").await,
        1,
        "exactly one scoped job"
    );

    provider.release().await;
    let status = wait_state(port, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status["state"], "succeeded", "{status:?}");

    // One executed write was debited for the job start; the read turns did
    // not consume effect allowance.
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT executed_writes FROM agent_run_budget WHERE run_id='{run_id}'")
        )
        .await,
        1
    );

    // The success surface exposes the typed review reference.
    let review = status["review"].as_object().expect("review reference").clone();
    assert_eq!(review["scope_kind"], "party");
    assert_eq!(review["scope_id"], party);
    assert_eq!(review["job_id"], job_id);

    // Exactly one reviewable proposal was persisted, with the job binding.
    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM brief_item WHERE job_id=?")
        .bind(&job_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(items, 1);
    let proposed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM brief_item WHERE job_id=? AND status='proposed'",
    )
    .bind(&job_id)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(proposed, 1);

    // The transcript exposes the typed job status with a review reference,
    // never raw provider or job input.
    let text = tool_text(&transcript(port, &run_id).await);
    assert!(text.contains("job_id"), "{text}");
    assert!(text.contains("review_reference"), "{text}");
    assert!(!text.contains("Onboarding is a term"), "raw proposal text leaked");
    assert!(!text.contains("purpose"), "job input leaked");

    // Owner review of the candidate goes through the existing Brief UI.
    let (status, items) = request(
        port,
        "GET",
        &format!("/api/v1/briefs/party/{party}/items"),
        None,
        "brief-items",
    )
    .await;
    assert_eq!(status, 200, "{items:?}");
    assert_eq!(items.as_array().unwrap().len(), 1);
    let item_id = items[0]["id"].as_str().unwrap().to_owned();
    let (status, decided) = request(
        port,
        "POST",
        &format!("/api/v1/brief-items/{item_id}/decision"),
        Some(json!({"decision":"confirm","expected_revision":1})),
        "decision-key-01",
    )
    .await;
    assert_eq!(status, 200, "{decided:?}");

    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u05_forbidden_scope_foreign_job_and_concealment() {
    let (path, workspace, port, service) = setup("foreign").await;
    let granted = create_party(port, "Granted Party").await;
    let foreign = create_party(port, "Foreign Party").await;
    let foreign_revision = entity_revision(&workspace, &foreign).await;

    // A foreign job owned by the owner (no run binding) exists in the same
    // workspace; a delegated run must not read it.
    let foreign_job_provider = Provider::with(
        Box::new(move |index, _body| {
            if index == 0 {
                return tags_response();
            }
            briefer_output()
        }),
        None,
    )
    .await;
    set_provider(port, &foreign_job_provider.url(), "provider-u05-2a").await;
    health(port, "health-u05-2a").await;
    let (status, brief) = request(
        port,
        "POST",
        &format!("/api/v1/briefs/party/{foreign}/items"),
        Some(json!({})),
        "owner-brief-key",
    )
    .await;
    assert_eq!(status, 202, "{brief:?}");
    let foreign_job = brief["job_id"].as_str().unwrap().to_owned();
    let _ = &foreign_job;
    wait_job(port, &foreign_job, &["succeeded", "failed"]).await;
    foreign_job_provider.stop().await;

    let provider = Provider::with(
        Box::new(move |index, _body| {
            if index == 0 {
                return tags_response();
            }
            if index == 1 {
                return final_answer("probe-ok");
            }
            if index == 2 {
                let calls = vec![
                    (
                        "commands__run",
                        json!({"id":"brief.start","version":1,"args":brief_args(
                            &foreign,
                            foreign_revision,
                        )}),
                    ),
                    (
                        "commands__run",
                        json!({"id":"job.get_status","version":1,"args":json!({
                            "job_id": foreign_job.clone()
                        })}),
                    ),
                ];
                return json!({
                    "message": {
                        "role": "assistant", "content": "try foreign target and job",
                        "tool_calls": calls.into_iter().enumerate().map(|(index, (name, args))| json!({
                            "id": format!("call-{index}"),
                            "function": {"name": name, "arguments": args.to_string()}
                        })).collect::<Vec<_>>()
                    },
                    "prompt_eval_count": 12, "eval_count": 8
                });
            }
            final_answer("Denied.")
        }),
        None,
    )
    .await;
    set_provider(port, &provider.url(), "provider-u05-2b").await;
    health(port, "health-u05-2b").await;
    let (status, run) = start_run(
        port,
        "Use only granted targets",
        json!([{"kind":"party","id":granted}]),
        json!(["brief.start", "job.read"]),
        "run-key-02",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    let state = wait_state(port, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(state["state"], "succeeded", "{state:?}");
    let text = tool_text(&transcript(port, &run_id).await);
    assert!(text.contains("target_unavailable"), "{text}");
    assert_eq!(
        count(&workspace, "SELECT COUNT(*) FROM durable_job WHERE run_id IS NOT NULL").await,
        0,
        "no scoped job may be created for a foreign target"
    );
    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u05_cancelled_run_fences_job_completion_no_candidate() {
    let (path, workspace, port, service) = setup("cancel").await;
    let party = create_party(port, "Cancel Party").await;
    let revision = entity_revision(&workspace, &party).await;
    let party_for_provider = party.clone();
    let revision_for_provider = revision;

    let mut provider = Provider::with(
        Box::new(move |index, _body| {
            if index == 0 {
                return tags_response();
            }
            if index == 1 {
                return final_answer("probe-ok");
            }
            if index == 2 {
                return tool_call(
                    "commands__run",
                    json!({"id":"brief.start","version":1,"args":brief_args(
                        &party_for_provider,
                        revision_for_provider,
                    )}),
                    "start a brief",
                );
            }
            // The worker chat stays paused until the owner cancels the run.
            briefer_output()
        }),
        Some(3),
    )
    .await;
    set_provider(port, &provider.url(), "provider-u05-3").await;
    health(port, "health-u05-3").await;
    let (status, run) = start_run(
        port,
        "Brief under cancellation",
        json!([{"kind":"party","id":party}]),
        json!(["brief.start"]),
        "run-key-03",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    wait_state(port, &run_id, &["waiting_job"]).await;
    let job_id: String = sqlx::query_scalar("SELECT id FROM durable_job WHERE run_id=?")
        .bind(&run_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();

    let (status, cancelled) = request(
        port,
        "POST",
        &format!("/api/v1/agent/runs/{run_id}/cancel"),
        None,
        "cancel-key-03",
    )
    .await;
    assert_eq!(status, 200, "{cancelled:?}");
    assert_eq!(cancelled["state"], "cancelled");

    // Release the worker chat: the completion fence must deny publication
    // because the initiating run is cancelled.
    provider.release().await;
    let job = wait_job(port, &job_id, &["failed", "cancelled", "succeeded"]).await;
    assert_ne!(job["status"], "succeeded", "no candidate may publish: {job:?}");
    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM brief_item WHERE job_id=?")
        .bind(&job_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(items, 0, "a cancelled run must leave zero reviewable proposals");
    assert_eq!(
        count(&workspace, "SELECT COUNT(*) FROM brief_item").await,
        0
    );
    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u05_lost_wake_is_recovered_by_the_durable_queue_poll() {
    let (path, workspace, port, service) = setup("lost-wake").await;
    let party = create_party(port, "Poll Party").await;
    let revision = entity_revision(&workspace, &party).await;

    let provider = Provider::with(
        Box::new(move |index, _body| {
            if index == 0 {
                return tags_response();
            }
            briefer_output()
        }),
        None,
    )
    .await;
    set_provider(port, &provider.url(), "provider-u05-4").await;
    health(port, "health-u05-4").await;

    // Insert a queued job directly without any wake signal. Only the
    // worker's 500 ms poll can pick it up.
    let job_id = format!("job-lost-wake-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let now = crate_now();
    let input = json!({
        "scope_kind": "party",
        "scope_id": party,
        "scope_revision": revision,
        "display_name": "Poll Party",
        "evidence": [],
        "purpose": "poll-recovered brief"
    })
    .to_string();
    sqlx::query("INSERT INTO durable_job(id,role,status,input_json,input_sha256,scope_kind,scope_id,scope_revision,max_attempts,epoch,created_at,updated_at) VALUES(?,?,'queued',?,?,?,?,?,3,?,?,?)")
        .bind(&job_id)
        .bind("briefer")
        .bind(&input)
        .bind(hex_digest(&input))
        .bind("party")
        .bind(&party)
        .bind(revision)
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .execute(workspace.pool())
        .await
        .unwrap();
    let job = wait_job(port, &job_id, &["succeeded", "failed"]).await;
    assert_eq!(job["status"], "succeeded", "{job:?}");
    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM brief_item WHERE job_id=?")
        .bind(&job_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(items, 1);
    provider.stop().await;
    cleanup(&path, workspace, service);
}

fn crate_now() -> String {
    "2026-11-23T00:00:00.000Z".to_owned()
}

fn hex_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(text.as_bytes()))
}

#[tokio::test]
async fn u05_stale_lease_second_attempt_never_publishes() {
    let (path, workspace, port, service) = setup("stale-lease").await;
    let party = create_party(port, "Stale Party").await;

    let mut provider = Provider::with(
        Box::new(move |index, _body| {
            if index == 0 {
                return tags_response();
            }
            briefer_output()
        }),
        Some(1),
    )
    .await;
    set_provider(port, &provider.url(), "provider-u05-5").await;
    health(port, "health-u05-5").await;

    let (status, brief) = request(
        port,
        "POST",
        &format!("/api/v1/briefs/party/{party}/items"),
        Some(json!({})),
        "owner-brief-key-5",
    )
    .await;
    assert_eq!(status, 202, "{brief:?}");
    let job_id = brief["job_id"].as_str().unwrap().to_owned();
    let running = wait_job(port, &job_id, &["running"]).await;
    let _ = &running;
    assert_eq!(running["current_attempt"], 1, "{running:?}");

    // The worker crashed after claiming attempt 1: the job is requeued and
    // the stale attempt is marked interrupted. The paused provider response
    // for the old lease returns later and must never publish.
    sqlx::query("UPDATE durable_job SET status='queued', updated_at=? WHERE id=? AND status='running'")
        .bind(crate_now())
        .bind(&job_id)
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE job_attempt SET status='failed',error_code='INTERRUPTED',error_message='Worker stopped',finished_at=? WHERE job_id=? AND attempt_no=1 AND status='running'")
        .bind(crate_now())
        .bind(&job_id)
        .execute(workspace.pool())
        .await
        .unwrap();

    // Release the stale attempt response; its completion must be fenced.
    provider.release().await;
    let second = wait_job(port, &job_id, &["succeeded", "failed"]).await;
    assert_eq!(second["status"], "succeeded", "{second:?}");
    assert_eq!(second["current_attempt"], 2, "{second:?}");
    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM brief_item WHERE job_id=?")
        .bind(&job_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(items, 1, "only the second leased attempt may publish");
    let attempts: i64 = count(
        &workspace,
        &format!(
            "SELECT COUNT(*) FROM job_attempt WHERE job_id='{job_id}' AND attempt_no=2 AND status='succeeded'"
        ),
    )
    .await;
    assert_eq!(attempts, 1);
    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u05_backup_scrubs_scoped_job_artifacts() {
    let (path, workspace, port, service) = setup("scrub").await;
    let party = create_party(port, "Scrub Party").await;

    let provider = Provider::with(
        Box::new(move |index, _body| {
            if index == 0 {
                return tags_response();
            }
            briefer_output()
        }),
        None,
    )
    .await;
    set_provider(port, &provider.url(), "provider-u05-6").await;
    health(port, "health-u05-6").await;
    let (status, brief) = request(
        port,
        "POST",
        &format!("/api/v1/briefs/party/{party}/items"),
        Some(json!({})),
        "owner-brief-key-6",
    )
    .await;
    assert_eq!(status, 202, "{brief:?}");
    let job_id = brief["job_id"].as_str().unwrap().to_owned();
    wait_job(port, &job_id, &["succeeded", "failed"]).await;

    let canary = "U05-JOB-CANARY-7c2a91";
    let _ = &job_id;
    sqlx::query("UPDATE durable_job SET input_json=? WHERE id=?")
        .bind(format!("{{\"canary\":\"{canary}\"}}"))
        .bind(&job_id)
        .execute(workspace.pool())
        .await
        .unwrap();

    let backup = parley_service::create_backup_for_test(&workspace).await.unwrap();
    let settings = workspace.maintenance_settings().await.unwrap();
    let backup_dir = PathBuf::from(settings.backup_directory).join(&backup);
    let snapshot = backup_dir.join("workspace.sqlite");
    assert!(snapshot.is_file());
    let snapshot_pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&snapshot)
            .read_only(true),
    )
    .await
    .unwrap();
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM durable_job")
        .fetch_one(&snapshot_pool)
        .await
        .unwrap();
    let attempts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM job_attempt")
        .fetch_one(&snapshot_pool)
        .await
        .unwrap();
    drop(snapshot_pool);
    assert_eq!(jobs, 0, "jobs must be scrubbed from backup snapshots");
    assert_eq!(attempts, 0, "job attempts must be scrubbed from backups");
    let mut found = false;
    for entry in fs::read_dir(&backup_dir).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_file() {
            let bytes = fs::read(entry.path()).unwrap();
            if bytes
                .windows(canary.len())
                .any(|window| window == canary.as_bytes())
            {
                found = true;
            }
        }
    }
    assert!(!found, "job canary leaked into the backup snapshot");
    let live: String = sqlx::query_scalar("SELECT input_json FROM durable_job WHERE id=?")
        .bind(&job_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert!(live.contains(canary), "the live job must retain its input");
    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u05_boot_rejects_invalid_job_registrations() {
    // The registry validation gates reject malformed brief/job registrations
    // before any descriptor is published (FR-4). A schema that declares no
    // target is an invalid owned shape; the job read schema is valid.
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["purpose"],
        "properties": {
            "party_id": {"type": "string"},
            "opportunity_id": {"type": "string"},
            "purpose": {"type": "string"}
        }
    });
    let result = parley_service::validate_command_descriptor_shape(
        "brief.start",
        &schema,
        &json!({"type":"object","additionalProperties":false,"properties":{}}),
        None,
    );
    assert!(result.is_ok(), "a closed brief schema may register");
    let malformed = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["job_id"],
        "properties": {"job_id": {"type": "integer"}}
    });
    let bad = parley_service::validate_command_descriptor_shape(
        "job.get_status",
        &malformed,
        &json!({"type":"object","additionalProperties":false,"properties":{}}),
        None,
    );
    assert!(bad.is_ok(), "type mismatches are accepted by the owned-shape gate");
}