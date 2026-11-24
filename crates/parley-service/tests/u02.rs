//! UCR-001 U02: an owner delegates a scoped read to a scripted local model.
//! The fake provider is a real loopback HTTP endpoint; authorization, the
//! registry pipeline, the ledger and the commit boundary are the service's
//! own. No test substitutes for them.
mod support;

use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
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
        "parley-u02-{tag}-{}-{}.sqlite",
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
    // Debug-only authority-test fixture: keep the pre-cut transport for
    // these U01-U08 authority/fencing cases. AR05 proves the real worker.
    unsafe {
        std::env::set_var("PARLEY_TEST_NATIVE_RUN_FIXTURE", "1");
    }
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

#[allow(dead_code)]
async fn create_opportunity(port: u16, title: &str, party: &str) -> String {
    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":title,"primary_party_id":party})),
        &format!("opp-{title}"),
    )
    .await;
    assert_eq!(status, 201, "{opportunity:?}");
    opportunity["id"].as_str().unwrap().to_string()
}

/// A scripted loopback model. Every request body is recorded so tests can
/// prove tools were offered and no owner credential reached the provider.
struct FakeProvider {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    task: Option<tokio::task::JoinHandle<()>>,
    release: Option<tokio::sync::oneshot::Sender<()>>,
}

impl FakeProvider {
    async fn start(bodies: Vec<Value>) -> Self {
        Self::with_gate(bodies, false).await
    }

    async fn with_gate(bodies: Vec<Value>, gate_chat: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let release = if gate_chat { Some(release_tx) } else { None };
        let mut gate = Some(release_rx);
        let task = tokio::spawn(async move {
            for (index, body) in bodies.into_iter().enumerate() {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                // Read exactly one request: headers, then the declared body.
                // Reading to EOF here would deadlock with a client that keeps
                // its write side open while awaiting the response.
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
                seen.lock()
                    .unwrap()
                    .push(format!("{head}{}", String::from_utf8_lossy(&body_bytes)));
                if gate_chat
                    && index >= 1
                    && let Some(rx) = gate.take()
                {
                    let _ = rx.await;
                }
                let payload = body.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Self {
            port,
            requests,
            task: Some(task),
            release,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn bodies(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    async fn stop(mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn tool_call(name: &str, args: Value, content: &str) -> Value {
    json!({
        "message": {
            "role": "assistant",
            "content": content,
            "tool_calls": [{"id": "call-1", "function": {"name": name, "arguments": args.to_string()}}]
        },
        "prompt_eval_count": 12,
        "eval_count": 8
    })
}

fn final_answer(content: &str) -> Value {
    json!({
        "message": {"role": "assistant", "content": content},
        "prompt_eval_count": 12,
        "eval_count": 8
    })
}

async fn set_provider(port: u16, url: &str) {
    let (status, reply) = request(
        port,
        "PUT",
        "/api/v1/agent/provider",
        Some(json!({
            "endpoint": url,
            "model": "fixture",
            "num_ctx": 8192,
            "num_predict": 256,
            "locale": "en"
        })),
        "provider-u02",
    )
    .await;
    assert!(status == 200 || status == 204, "{status} {reply:?}");
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
    for _ in 0..80 {
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
    panic!("run {id} never reached {states:?}; last={last:?}");
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

async fn count(workspace: &Workspace, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(workspace.pool())
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn u02_wire_fuzz_cases_and_malformed_provider_are_rejected() {
    for input in support::invalid_wire_cases() {
        assert!(
            parley_service::validate_command_wire(&input).is_err(),
            "invalid wire input was accepted: {}",
            String::from_utf8_lossy(&input)
        );
    }
    for input in support::valid_wire_cases() {
        assert!(
            parley_service::validate_command_wire(&input).is_ok(),
            "valid wire input was rejected: {}",
            String::from_utf8_lossy(&input)
        );
    }
    // A malformed provider response reaches the real loopback adapter and is
    // turned into a typed failure, never into a tool result.
    let (_path, workspace, port, service) = setup("malformed").await;
    let party = create_party(port, "Malformed Party").await;
    let provider = support::ScriptedProvider::start(vec![
        json!({"message":{"role":"assistant","content":"probe"}}),
        json!({"message":{"role":"assistant","content":"not-json-tool-calls","tool_calls":"unexpected"}}),
    ])
    .await;
    set_provider(port, &provider.url()).await;
    let (status, run) = start_run(
        port,
        "Malformed provider",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "malformed-run-key-01",
    )
    .await;
    assert_eq!(status, 201);
    let id = run["run_id"].as_str().unwrap();
    let done = wait_state(port, id, &["failed", "succeeded"]).await;
    assert_eq!(done["state"], "failed", "{done:?}");
    let turns = transcript(port, id).await;
    assert!(
        turns
            .iter()
            .any(|turn| turn["speaker"] == "system"
                && turn["text"].as_str() == Some("PROVIDER_FAILED")),
        "malformed response produced no typed failure: {turns:?}"
    );
    assert!(!provider.recorded().is_empty());
    cleanup(&_path, workspace, service);
}

#[tokio::test]
async fn u02_scoped_read_reaches_real_result_and_persists_run() {
    let (path, workspace, port, service) = setup("read").await;
    let party = create_party(port, "Scoped Party").await;
    let provider = FakeProvider::start(vec![
        final_answer("probe-ok"),
        tool_call(
            "party__get_summary",
            json!({"party_id": party}),
            "checking the scoped party",
        ),
        final_answer("Scoped Party is the granted record."),
    ])
    .await;
    set_provider(port, &provider.url()).await;
    let (status, run) = start_run(
        port,
        "Summarise the granted party",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "read-run-key-01",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    assert_eq!(run["granted_actions"][0], "party.read");
    let done = wait_state(port, id, &["succeeded", "failed"]).await;
    assert_eq!(done["state"], "succeeded", "{done:?}");
    assert!(done["turn_count"].as_i64().unwrap() >= 2);

    let turns = transcript(port, id).await;
    let tool_body = turns
        .iter()
        .find(|turn| turn["speaker"] == "tool")
        .map(|turn| turn["text"].as_str().unwrap())
        .expect("a tool result was recorded");
    assert!(tool_body.contains("Scoped Party"), "{tool_body}");
    assert!(!tool_body.contains("notes_markdown"), "{tool_body}");

    let bodies = provider.bodies();
    assert!(
        bodies.len() >= 2,
        "probe and chat must both reach the model"
    );
    let chat = bodies
        .iter()
        .find(|body| body.contains("party__get_summary"))
        .expect("the turn request offered the Tier 1 tool");
    assert!(
        chat.contains("party__get_summary"),
        "Tier 1 tool was offered"
    );
    assert!(chat.contains("commands__search"));
    assert!(chat.contains("commands__describe"));
    assert!(chat.contains("commands__run"));
    assert!(!chat.contains("parley_session"));
    assert!(!chat.contains("X-Parley-Token"));
    assert!(!chat.contains("X-Parley-Epoch"));
    assert_eq!(
        count(
            &workspace,
            "SELECT COUNT(*) FROM command_audit_event WHERE actor_kind='agent' AND kind='attempt'"
        )
        .await,
        1,
        "one read invocation was audited under the agent principal"
    );
    assert_eq!(
        count(
            &workspace,
            "SELECT COUNT(*) FROM opportunity WHERE title='Scoped Party'"
        )
        .await,
        0
    );
    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u02_unknown_and_concealed_tools_share_public_denial() {
    let (path, workspace, port, service) = setup("unknown").await;
    let party = create_party(port, "Unknown Tool Party").await;
    let provider = FakeProvider::start(vec![
        final_answer("probe-ok"),
        tool_call("commands__approve_material", json!({}), ""),
        final_answer("I could not run that tool."),
    ])
    .await;
    set_provider(port, &provider.url()).await;
    let (status, run) = start_run(
        port,
        "Try an unregistered tool",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "unknown-tool-key-01",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    let done = wait_state(port, id, &["succeeded", "failed"]).await;
    assert_eq!(done["state"], "succeeded");
    let turns = transcript(port, id).await;
    let text = turns
        .iter()
        .filter(|turn| turn["speaker"] == "tool")
        .map(|turn| turn["text"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("unknown_command"), "{text}");
    assert_eq!(
        count(&workspace, "SELECT COUNT(*) FROM command_receipt").await,
        0,
        "an unmapped tool never produced a receipt"
    );
    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u02_foreign_target_and_out_of_scope_command_are_denied() {
    let (path, workspace, port, service) = setup("foreign").await;
    let granted = create_party(port, "Granted Party").await;
    let foreign = create_party(port, "Foreign Party").await;
    let provider = FakeProvider::start(vec![
        final_answer("probe-ok"),
        tool_call(
            "party__get_summary",
            json!({"party_id": foreign}),
            "try a foreign record",
        ),
        tool_call(
            "opportunity__get_summary",
            json!({"opportunity_id": "opp_absent"}),
            "try an ungranted command",
        ),
        final_answer("Both requests were denied."),
    ])
    .await;
    set_provider(port, &provider.url()).await;
    let (status, run) = start_run(
        port,
        "Try records outside the grant",
        json!([{"kind":"party","id":granted}]),
        json!(["party.read"]),
        "foreign-target-key-01",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    let done = wait_state(port, id, &["succeeded", "failed"]).await;
    assert_eq!(done["state"], "succeeded");
    let turns = transcript(port, id).await;
    let text = turns
        .iter()
        .filter(|turn| turn["speaker"] == "tool")
        .map(|turn| turn["text"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("target_unavailable"), "{text}");
    assert!(text.contains("unknown_command"), "{text}");
    assert_eq!(
        count(&workspace, "SELECT COUNT(*) FROM command_receipt").await,
        0,
        "denied calls produce no receipts"
    );
    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u02_cancel_revokes_grant_while_provider_is_delayed() {
    let (path, workspace, port, service) = setup("cancel").await;
    let party = create_party(port, "Cancel Party").await;
    let provider = FakeProvider::with_gate(
        vec![
            final_answer("probe-ok"),
            tool_call("party__get_summary", json!({"party_id": party}), ""),
            final_answer("late answer"),
        ],
        true,
    )
    .await;
    set_provider(port, &provider.url()).await;
    let (status, run) = start_run(
        port,
        "Slow read",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "cancel-run-key-01",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    let _ = wait_state(port, id, &["running"]).await;
    let (status, cancelled) = request(
        port,
        "POST",
        &format!("/api/v1/agent/runs/{id}/cancel"),
        None,
        "cancel-key-01",
    )
    .await;
    assert_eq!(status, 200, "{cancelled:?}");
    assert_eq!(cancelled["state"], "cancelled");
    provider.stop().await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let after = wait_state(port, id, &["cancelled"]).await;
    assert_eq!(after["state"], "cancelled");
    assert_eq!(
        count(
            &workspace,
            "SELECT COUNT(*) FROM agent_tool_intent WHERE state='completed'"
        )
        .await,
        0,
        "cancellation won before a tool result could commit"
    );
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u02_expired_lease_recovery_keeps_one_tool_ordinal() {
    let (path, workspace, port, service) = setup("lease").await;
    let party = create_party(port, "Lease Party").await;
    let provider = FakeProvider::start(vec![
        final_answer("probe-ok"),
        tool_call("party__get_summary", json!({"party_id": party}), ""),
        final_answer("Recovered after lease expiry."),
    ])
    .await;
    set_provider(port, &provider.url()).await;
    let (status, run) = start_run(
        port,
        "Read after a stale lease",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "lease-run-key-01",
    )
    .await;
    assert_eq!(status, 201);
    let id = run["run_id"].as_str().unwrap();
    // Simulate a crashed worker: the lease is expired while the run is still
    // marked running, with a pending ordinal already persisted.
    sqlx::query("INSERT INTO agent_tool_intent(run_id,turn_no,ordinal,tool_name,key_digest,args_digest,state,created_at) VALUES(?,?,1,'party__get_summary','stale','stale','pending',?)")
        .bind(id)
        .bind(1i64)
        .bind(crate_time())
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE agent_run SET lease_deadline='2020-01-01T00:00:00.000Z' WHERE run_id=?")
        .bind(id)
        .execute(workspace.pool())
        .await
        .unwrap();
    parley_service::recover_agent_runs(&workspace).await;
    let done = wait_state(port, id, &["succeeded", "failed"]).await;
    assert_eq!(done["state"], "succeeded", "{done:?}");
    let intents: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_tool_intent WHERE run_id=? AND turn_no=1 AND ordinal=1",
    )
    .bind(id)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(
        intents, 1,
        "the same ordinal must be reconciled, not duplicated"
    );
    provider.stop().await;
    cleanup(&path, workspace, service);
}

fn crate_time() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[tokio::test]
async fn u02_budget_limit_and_plus_one_are_distinct() {
    let (path, workspace, port, service) = setup("budget").await;
    let party = create_party(port, "Budget Party").await;
    let provider = FakeProvider::start(vec![
        final_answer("probe-ok"),
        tool_call("party__get_summary", json!({"party_id": party}), ""),
        final_answer("one read"),
    ])
    .await;
    set_provider(port, &provider.url()).await;
    let (status, first) = start_run(
        port,
        "Budget read",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "budget-run-key-01",
    )
    .await;
    assert_eq!(status, 201);
    // Replaying the same key returns the same run, not a second invocation.
    let (status, replay) = start_run(
        port,
        "Budget read",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "budget-run-key-01",
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(replay["run_id"], first["run_id"]);
    assert_eq!(
        count(&workspace, "SELECT COUNT(*) FROM agent_run").await,
        1,
        "same-key replay is one run"
    );
    // A fresh key with the same task is a distinct, admitted intent.
    let (status, second) = start_run(
        port,
        "Budget read",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "budget-run-key-02",
    )
    .await;
    assert_eq!(status, 201);
    assert_ne!(second["run_id"], first["run_id"]);
    assert_eq!(count(&workspace, "SELECT COUNT(*) FROM agent_run").await, 2);
    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u02_discovery_respects_permission_filter_and_bounds() {
    let (path, workspace, port, service) = setup("discovery").await;
    let party = create_party(port, "Discovery Party").await;
    let provider = FakeProvider::start(vec![
        final_answer("probe-ok"),
        tool_call(
            "commands__search",
            json!({"query":"summary","limit":25}),
            "",
        ),
        final_answer("Only the granted read is visible."),
    ])
    .await;
    set_provider(port, &provider.url()).await;
    let (status, run) = start_run(
        port,
        "Discover available tools",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "discovery-run-key-01",
    )
    .await;
    assert_eq!(status, 201);
    let id = run["run_id"].as_str().unwrap();
    let done = wait_state(port, id, &["succeeded", "failed"]).await;
    assert_eq!(done["state"], "succeeded");
    let turns = transcript(port, id).await;
    let search = turns
        .iter()
        .find(|turn| turn["speaker"] == "tool")
        .map(|turn| turn["text"].as_str().unwrap())
        .expect("search result recorded");
    assert!(search.contains("party__get_summary"), "{search}");
    assert!(!search.contains("opportunity__get_summary"), "{search}");
    assert!(!search.contains("args_schema"), "{search}");
    provider.stop().await;
    cleanup(&path, workspace, service);
}
