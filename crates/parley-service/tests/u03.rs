//! UCR-001 U03: a granted agent creates an opportunity and advances its
//! planning phase. The fake provider is a real loopback endpoint; all
//! authorization, ledger, writer and commit behavior is the service's own.
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
        "parley-u03-{tag}-{}-{}.sqlite",
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

/// Invoke a registry command as the owner.
async fn cmd(port: u16, id: &str, key: &str, args: Value) -> (u16, Value) {
    request(
        port,
        "POST",
        &format!("/api/v1/commands/{id}"),
        Some(json!({"version":1,"args":args,"dry_run":false})),
        key,
    )
    .await
}

fn create_args(title: &str, party: &str) -> Value {
    json!({"title":title,"primary_party_id":party})
}

fn phase_args(opportunity: &str, revision: i64, stage: &str) -> Value {
    json!({
        "opportunity_id": opportunity,
        "expected_phase_revision": revision,
        "expected_pack_id": "job_search",
        "expected_pack_version": "1.0.0",
        "stage_id": stage
    })
}

async fn count(workspace: &Workspace, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(workspace.pool())
        .await
        .unwrap()
}

/// A loopback tool-capable provider whose responses may depend on the chat
/// body, so a later tool call can use the opportunity ID returned by an
/// earlier one.
struct FakeProvider {
    port: u16,
    task: Option<tokio::task::JoinHandle<()>>,
    release: Option<tokio::sync::oneshot::Sender<()>>,
}

type Responder = Box<dyn FnMut(usize, &str) -> Value + Send>;

impl FakeProvider {
    async fn with(responder: Responder, gate_chat: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let release = if gate_chat { Some(release_tx) } else { None };
        let mut gate = Some(release_rx);
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
                if gate_chat
                    && index >= 1
                    && let Some(rx) = gate.take()
                {
                    let _ = rx.await;
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

    async fn release(mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    async fn stop(self) {
        self.release().await
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

fn tool_calls(calls: Vec<(&str, Value)>, content: &str) -> Value {
    json!({
        "message": {
            "role": "assistant", "content": content,
            "tool_calls": calls.into_iter().enumerate().map(|(index, (name, args))| json!({
                "id": format!("call-{index}"),
                "function": {"name": name, "arguments": args.to_string()}
            })).collect::<Vec<_>>()
        },
        "prompt_eval_count": 12, "eval_count": 8
    })
}

/// Read the most recent tool result from a chat request body.
fn last_tool_result(body: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(body).ok()?;
    let messages = value.get("messages")?.as_array()?;
    messages
        .iter()
        .rev()
        .find(|message| message["role"].as_str() == Some("tool"))
        .and_then(|message| message["content"].as_str())
        .and_then(|text| serde_json::from_str(text).ok())
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

async fn start_run(
    port: u16,
    task: &str,
    targets: Value,
    permissions: Value,
    allow_created: bool,
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
            "allow_created_opportunities": allow_created
        })),
        key,
    )
    .await
}

async fn wait_state(port: u16, id: &str, states: &[&str]) -> Value {
    for _ in 0..100 {
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
    panic!("run {id} never reached {states:?}");
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
async fn u03_rest_and_command_create_share_one_fact_and_events() {
    let (path, workspace, port, service) = setup("parity").await;
    let party = create_party(port, "Parity Party").await;
    let rest_id = create_opportunity(port, "REST created opportunity", &party).await;
    let (status, reply) = cmd(
        port,
        "opportunity.create",
        "create-parity-key-01",
        create_args("Command created opportunity", &party),
    )
    .await;
    assert_eq!(status, 200, "{reply:?}");
    let command_id = reply["result"]["opportunity_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(rest_id, command_id);
    assert_eq!(reply["result"]["revision"], 1);
    assert_eq!(reply["result"]["primary_party_id"], party);
    assert_eq!(reply["result"]["list_id"], "inbox");
    assert!(
        reply["result"]["event_cursor"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap()
            > 0
    );

    for id in [&rest_id, &command_id] {
        assert_eq!(
            count(&workspace, &format!("SELECT COUNT(*) FROM opportunity_party WHERE opportunity_id='{id}' AND role='primary'")).await,
            1
        );
        assert_eq!(
            count(
                &workspace,
                &format!("SELECT COUNT(*) FROM board_placement WHERE opportunity_id='{id}'")
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &workspace,
                &format!("SELECT COUNT(*) FROM party_policy WHERE party_id='{party}'")
            )
            .await,
            1
        );
        let types: Vec<(String,)> = sqlx::query_as(
            "SELECT event_type FROM event WHERE entity_id=? AND event_type IN ('opportunity.created','board.card_created') ORDER BY cursor",
        )
        .bind(id)
        .fetch_all(workspace.pool())
        .await
        .unwrap();
        assert_eq!(
            types.into_iter().map(|row| row.0).collect::<Vec<_>>(),
            vec!["board.card_created", "opportunity.created"]
        );
    }

    // A REST-only projection field cannot change the command contract.
    let (_, descriptor) = request(
        port,
        "GET",
        "/api/v1/commands/opportunity.create",
        None,
        "describe-create",
    )
    .await;
    let properties = descriptor["result_schema"]["properties"]
        .as_object()
        .unwrap();
    assert!(!properties.contains_key("participant_ids"));
    assert!(!properties.contains_key("source_variant_ids"));
    assert!(!properties.contains_key("material_ids"));
    assert!(
        !reply["result"]
            .as_object()
            .unwrap()
            .contains_key("participant_ids")
    );

    // Audit args come from a positive allowlist with both private fields
    // redacted, from lookup through serialization.
    let args_json: String = sqlx::query_scalar(
        "SELECT args_json FROM command_audit_event WHERE kind='attempt' AND command_id='opportunity.create' LIMIT 1",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert!(args_json.contains("[redacted]"), "{args_json}");
    assert!(
        !args_json.contains("Command created opportunity"),
        "{args_json}"
    );
    assert!(!args_json.contains(&party), "{args_json}");

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u03_create_crash_boundaries_and_key_discipline() {
    let (path, workspace, port, service) = setup("crash").await;
    let party = create_party(port, "Crash Party").await;
    let args = create_args("Crash create", &party);

    let (status, lost) = cmd(
        port,
        "opportunity.create",
        "fault-precommit-u03",
        args.clone(),
    )
    .await;
    assert_eq!(status, 503, "{lost:?}");
    assert_eq!(
        count(
            &workspace,
            "SELECT COUNT(*) FROM opportunity WHERE title='Crash create'"
        )
        .await,
        0
    );
    let (status, recovered) = cmd(
        port,
        "opportunity.create",
        "fault-precommit-u03",
        args.clone(),
    )
    .await;
    assert_eq!(status, 200, "{recovered:?}");

    let (status, ambiguous) = cmd(
        port,
        "opportunity.create",
        "fault-postcommit-u03",
        args.clone(),
    )
    .await;
    assert_eq!(status, 503, "{ambiguous:?}");
    let (status, replay) = cmd(
        port,
        "opportunity.create",
        "fault-postcommit-u03",
        args.clone(),
    )
    .await;
    assert_eq!(status, 200, "{replay:?}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        count(
            &workspace,
            "SELECT COUNT(*) FROM opportunity WHERE title='Crash create'"
        )
        .await,
        2,
        "precommit retry and postcommit replay are exactly two effects"
    );

    let (status, mismatch) = cmd(
        port,
        "opportunity.create",
        "fault-precommit-u03",
        create_args("A different intent", &party),
    )
    .await;
    assert_eq!(status, 422, "{mismatch:?}");
    assert_eq!(mismatch["error"]["code"], "idempotency_mismatch");

    let (status, invalid) = cmd(
        port,
        "opportunity.create",
        "invalid-party-u03",
        create_args("Invalid party", "party_absent"),
    )
    .await;
    assert_eq!(status, 404, "{invalid:?}");
    assert_eq!(invalid["error"]["code"], "target_unavailable");
    assert_eq!(
        count(
            &workspace,
            "SELECT COUNT(*) FROM opportunity WHERE title='Invalid party'"
        )
        .await,
        0
    );

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u03_agent_create_binds_self_created_target_and_advances_phase() {
    let (path, workspace, port, service) = setup("agent-create").await;
    let party = create_party(port, "Delegated Party").await;
    let tool_party = party.clone();
    let provider = FakeProvider::with(
        Box::new(move |index, body| {
            if index == 0 {
                return final_answer("probe-ok");
            }
            if index == 1 {
                return tool_call(
                    "commands__run",
                    json!({
                        "id": "opportunity.create",
                        "version": 1,
                        "args": create_args("Agent created opportunity", &tool_party)
                    }),
                    "create the granted opportunity",
                );
            }
            let created = last_tool_result(body)
                .and_then(|value| {
                    value["result"]["opportunity_id"]
                        .as_str()
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            if index == 2 {
                return tool_call(
                    "opportunity__set_phase",
                    phase_args(&created, 1, "preparing"),
                    "advance it",
                );
            }
            final_answer("Created and advanced the opportunity.")
        }),
        false,
    )
    .await;
    set_provider(port, &provider.url(), "provider-u03-1").await;
    let (status, run) = start_run(
        port,
        "Create and advance an opportunity",
        json!([{"kind":"party","id":party}]),
        json!(["opportunity.create", "opportunity.phase.edit"]),
        true,
        "agent-create-key-01",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    let done = wait_state(port, id, &["succeeded", "failed"]).await;
    assert_eq!(
        done["state"],
        "succeeded",
        "{}",
        tool_text(&transcript(port, id).await)
    );

    assert_eq!(
        count(
            &workspace,
            "SELECT COUNT(*) FROM opportunity WHERE title='Agent created opportunity'"
        )
        .await,
        1
    );
    let created: String =
        sqlx::query_scalar("SELECT id FROM opportunity WHERE title='Agent created opportunity'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM agent_run_target WHERE run_id='{id}' AND kind='opportunity' AND target_id='{created}'")).await,
        1,
        "the created record is bound into the run scope in the create transaction"
    );
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT executed_writes FROM agent_run_budget WHERE run_id='{id}'")
        )
        .await,
        2,
        "create and phase each debit one executed write"
    );
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT invocations FROM agent_run_budget WHERE run_id='{id}'")
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT phase_revision FROM opportunity WHERE id='{created}'")
        )
        .await,
        2
    );
    let agent_attempts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM command_audit_event WHERE actor_kind='agent' AND kind='attempt' AND command_id IN ('opportunity.create','opportunity.set_phase')",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(agent_attempts, 2);
    let leaks: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM command_audit_event WHERE actor_kind='agent' AND args_json LIKE '%Agent created opportunity%'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(leaks, 0, "private create args never enter the audit tail");

    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u03_revocation_fences_phase_tool_and_replay() {
    let (path, workspace, port, service) = setup("revoke").await;
    let party = create_party(port, "Revoke Party").await;
    let opportunity = create_opportunity(port, "Guarded opportunity", &party).await;
    let tool_opportunity = opportunity.clone();
    let provider = FakeProvider::with(
        Box::new(move |index, _body| {
            if index == 0 {
                return final_answer("probe-ok");
            }
            if index == 1 {
                return tool_call(
                    "opportunity__set_phase",
                    phase_args(&tool_opportunity, 1, "preparing"),
                    "delayed phase edit",
                );
            }
            final_answer("too late")
        }),
        true,
    )
    .await;
    set_provider(port, &provider.url(), "provider-u03-2").await;
    let (status, run) = start_run(
        port,
        "Delayed phase edit",
        json!([{"kind":"opportunity","id":opportunity}]),
        json!(["opportunity.phase.edit"]),
        false,
        "revoke-key-01",
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
    provider.release().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{opportunity}' AND event_type='opportunity.phase_changed'")).await,
        0,
        "revocation won before the delayed tool could commit an effect"
    );
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT phase_revision FROM opportunity WHERE id='{opportunity}'")
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &workspace,
            "SELECT COUNT(*) FROM command_receipt WHERE command_id='opportunity.set_phase'"
        )
        .await,
        0,
        "a fenced attempt produces no replayable receipt"
    );
    // A fresh owner intent on the same record still works; revocation is scoped
    // to the run, not to the shared fact.
    let (status, applied) = cmd(
        port,
        "opportunity.set_phase",
        "owner-after-revoke",
        phase_args(&opportunity, 1, "preparing"),
    )
    .await;
    assert_eq!(status, 200, "{applied:?}");
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u03_nested_roles_and_self_created_grant_are_not_authority() {
    let (path, workspace, port, service) = setup("smuggle").await;
    let granted = create_party(port, "Smuggle Granted").await;
    let foreign = create_party(port, "Smuggle Foreign").await;
    let tool_granted = granted.clone();
    let tool_foreign = foreign.clone();
    let provider = FakeProvider::with(
        Box::new(move |index, _body| {
            if index == 0 {
                return final_answer("probe-ok");
            }
            if index == 1 {
                return tool_call(
                    "commands__run",
                    json!({"id":"opportunity.create","version":1,"args":json!({
                        "title":"Foreign primary", "primary_party_id":tool_foreign,
                        "participants":[{"party_id":tool_foreign,"role":"primary"}]
                    })}),
                    "try a foreign party and nested role",
                );
            }
            if index == 2 {
                return tool_call(
                    "commands__run",
                    json!({"id":"opportunity.create","version":1,"args":json!({
                        "title":"Granted party nested", "primary_party_id":tool_granted,
                        "participants":[{"party_id":tool_foreign,"role":"primary"}]
                    })}),
                    "try nested role smuggling",
                );
            }
            final_answer("Both attempts were denied.")
        }),
        false,
    )
    .await;
    set_provider(port, &provider.url(), "provider-u03-3").await;
    // No created-target grant: even the in-scope primary party is concealed.
    let (status, run) = start_run(
        port,
        "Try to create without the created-target grant",
        json!([{"kind":"party","id":granted}]),
        json!(["opportunity.create"]),
        false,
        "smuggle-key-01",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    let done = wait_state(port, id, &["succeeded", "failed"]).await;
    assert_eq!(done["state"], "succeeded");
    let text = tool_text(&transcript(port, id).await);
    assert!(text.contains("unknown_command"), "{text}");
    assert_eq!(
        count(&workspace, "SELECT COUNT(*) FROM opportunity").await,
        0,
        "an ungranted create left zero facts"
    );
    provider.stop().await;

    // With the grant, a foreign target and a nested role are still refused.
    let tool_granted_b = granted.clone();
    let tool_foreign_b = foreign.clone();
    let provider = FakeProvider::with(
        Box::new(move |index, _body| {
            if index == 0 {
                return final_answer("probe-ok");
            }
            if index == 1 {
                return tool_call(
                    "commands__run",
                    json!({"id":"opportunity.create","version":1,"args":json!({
                        "title":"Foreign primary", "primary_party_id":tool_foreign_b
                    })}),
                    "try a foreign party",
                );
            }
            if index == 2 {
                return tool_call(
                    "commands__run",
                    json!({"id":"opportunity.create","version":1,"args":json!({
                        "title":"Nested role", "primary_party_id":tool_granted_b,
                        "participants":[{"party_id":tool_foreign_b,"role":"primary"}]
                    })}),
                    "smuggle a nested role",
                );
            }
            final_answer("Both attempts were denied.")
        }),
        false,
    )
    .await;
    set_provider(port, &provider.url(), "provider-u03-4").await;
    let (status, run) = start_run(
        port,
        "Try out-of-scope and malformed creates",
        json!([{"kind":"party","id":granted}]),
        json!(["opportunity.create"]),
        true,
        "smuggle-key-02",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    assert_eq!(
        wait_state(port, id, &["succeeded", "failed"]).await["state"],
        "succeeded"
    );
    let text = tool_text(&transcript(port, id).await);
    assert!(text.contains("target_unavailable"), "{text}");
    assert!(text.contains("invalid_args"), "{text}");
    assert_eq!(
        count(&workspace, "SELECT COUNT(*) FROM opportunity").await,
        0
    );

    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u03_executed_write_allowance_is_debited_once_per_effect() {
    let (path, workspace, port, service) = setup("budget").await;
    let party = create_party(port, "Budget Party").await;
    let budget_party = party.clone();
    let provider = FakeProvider::with(
        Box::new(move |index, _body| {
            if index == 0 {
                return final_answer("probe-ok");
            }
            if index <= 3 {
                let calls = (1..=4)
                    .map(|n| {
                        (
                            "commands__run",
                            json!({"id":"opportunity.create","version":1,"args":create_args(
                                &format!("Budget create {}", (index - 1) * 4 + n), &budget_party
                            )}),
                        )
                    })
                    .collect();
                return tool_calls(calls, "batch one");
            }
            if index == 4 {
                let calls = (1..=4)
                    .map(|n| {
                        (
                            "commands__run",
                            json!({"id":"opportunity.create","version":1,"args":create_args(
                                &format!("Budget create {}", 16 + n), &budget_party
                            )}),
                        )
                    })
                    .collect();
                return tool_calls(calls, "batch two");
            }
            if index == 5 {
                return tool_call(
                    "commands__run",
                    json!({"id":"opportunity.create","version":1,"args":create_args("Budget create 21", &budget_party)}),
                    "one over the allowance",
                );
            }
            final_answer("Stopped at the allowance.")
        }),
        false,
    )
    .await;
    set_provider(port, &provider.url(), "provider-u03-5").await;
    let (status, run) = start_run(
        port,
        "Create up to the executed-write allowance",
        json!([{"kind":"party","id":party}]),
        json!(["opportunity.create"]),
        true,
        "budget-key-01",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    assert_eq!(
        wait_state(port, id, &["succeeded", "failed"]).await["state"],
        "succeeded"
    );
    assert_eq!(
        count(
            &workspace,
            "SELECT COUNT(*) FROM opportunity WHERE title LIKE 'Budget create %'"
        )
        .await,
        10,
        "exactly ten executed writes committed"
    );
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT executed_writes FROM agent_run_budget WHERE run_id='{id}'")
        )
        .await,
        10
    );
    let text = tool_text(&transcript(port, id).await);
    assert!(text.contains("budget_exceeded"), "{text}");

    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u03_concurrent_same_key_creates_produce_one_effect() {
    let (path, workspace, port, service) = setup("race").await;
    let party = create_party(port, "Race Party").await;
    let mut handles = Vec::new();
    for _ in 0..32 {
        let args = create_args("Raced create", &party);
        handles.push(tokio::spawn(async move {
            cmd(port, "opportunity.create", "race-create-key-01", args).await
        }));
    }
    let mut statuses = Vec::new();
    for handle in handles {
        statuses.push(handle.await.unwrap());
    }
    assert_eq!(
        statuses.iter().filter(|(status, _)| *status == 200).count(),
        32
    );
    assert_eq!(
        count(
            &workspace,
            "SELECT COUNT(*) FROM opportunity WHERE title='Raced create'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&workspace, "SELECT COUNT(*) FROM event WHERE entity_id IN (SELECT id FROM opportunity WHERE title='Raced create') AND event_type='opportunity.created'").await,
        1
    );
    let receipts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM command_receipt WHERE command_id='opportunity.create'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(receipts, 1);
    let outcomes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM command_audit_event WHERE kind='outcome' AND command_id='opportunity.create'")
        .fetch_one(workspace.pool()).await.unwrap();
    assert_eq!(
        outcomes, 32,
        "every admitted attempt has its own terminal outcome"
    );
    cleanup(&path, workspace, service);
}

async fn seeded_batch(tag: &str, offset: usize, cases: usize) {
    let (path, workspace, port, service) = setup(tag).await;
    let parties = [
        create_party(port, &format!("Seed Party A {tag}")).await,
        create_party(port, &format!("Seed Party B {tag}")).await,
        create_party(port, &format!("Seed Party C {tag}")).await,
    ];
    let mut seed: u64 = 314_159 + offset as u64;
    let mut next = || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        seed >> 33
    };
    let mut created = 0usize;
    for index in offset..offset + cases {
        let party = &parties[(next() % 3) as usize];
        let opportunity =
            create_opportunity(port, &format!("Seed opportunity {index}"), party).await;
        created += 1;
        let variant = next() % 8;
        let key = format!("seed-key-{index}-1");
        let args = match variant {
            0 => phase_args(&opportunity, 1, "preparing"),
            1 => phase_args(&opportunity, 99, "preparing"),
            2 => json!({
                "opportunity_id": opportunity, "expected_phase_revision": 1,
                "expected_pack_id": "wrong_pack", "expected_pack_version": "1.0.0",
                "stage_id": "preparing"
            }),
            3 => phase_args(&opportunity, 1, "closed"),
            4 => phase_args(&opportunity, 1, "missing_stage"),
            5 => json!({
                "opportunity_id": opportunity, "expected_phase_revision": 1,
                "expected_pack_id": "job_search", "expected_pack_version": "1.0.0",
                "stage_id": 42
            }),
            6 => phase_args(&opportunity, 1, "exploring"),
            _ => phase_args(&opportunity, 1, "preparing"),
        };
        let (status, first) = cmd(port, "opportunity.set_phase", &key, args.clone()).await;
        let expected = match variant {
            0 | 6 | 7 => 200,
            1..=3 => 409,
            _ => 422,
        };
        assert_eq!(
            status, expected,
            "case {index} variant {variant}: {first:?}"
        );
        if variant == 3 {
            assert_eq!(first["error"]["code"], "requires_owner_workflow");
        }
        if matches!(variant, 0 | 6 | 7) {
            // Same key and intent always replays, regardless of the new phase
            // revision visible after the first execution.
            let (replay_status, replay) =
                cmd(port, "opportunity.set_phase", &key, args.clone()).await;
            assert_eq!(replay_status, 200, "case {index}: {replay:?}");
            assert_eq!(replay["replayed"], true);
            let (fresh_status, fresh) = cmd(
                port,
                "opportunity.set_phase",
                &format!("seed-key-{index}-2"),
                phase_args(&opportunity, 2, "in_conversation"),
            )
            .await;
            assert_eq!(fresh_status, 200, "case {index}: {fresh:?}");
        }
    }
    assert_eq!(created, cases);
    assert_eq!(
        count(
            &workspace,
            "SELECT COUNT(*) FROM opportunity WHERE title LIKE 'Seed opportunity %'"
        )
        .await,
        cases as i64
    );
    // A lifecycle-blocked case is still refused with the command contract.
    let opportunity =
        create_opportunity(port, &format!("Seed lifecycle blocked {tag}"), &parties[0]).await;
    sqlx::query("UPDATE opportunity SET state='closed',close_reason='seed' WHERE id=?")
        .bind(&opportunity)
        .execute(workspace.pool())
        .await
        .unwrap();
    let (status, blocked) = cmd(
        port,
        "opportunity.set_phase",
        &format!("seed-lifecycle-key-{tag}"),
        phase_args(&opportunity, 1, "preparing"),
    )
    .await;
    assert_eq!(status, 409, "{blocked:?}");
    assert_eq!(blocked["error"]["code"], "conflict");

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u03_seeded_combinations_of_grants_stages_and_revisions() {
    // Fresh workspaces keep each owner under the 120/min admission cap.
    seeded_batch("seeded-a", 0, 40).await;
    seeded_batch("seeded-b", 40, 40).await;
    seeded_batch("seeded-c", 80, 40).await;
    seeded_batch("seeded-d", 120, 40).await;
    seeded_batch("seeded-e", 160, 40).await;
}

#[tokio::test]
async fn u03_restart_preserves_created_facts_audit_and_budgets() {
    let (path, workspace, port, service) = setup("restart").await;
    let party = create_party(port, "Restart Party").await;
    let restart_party = party.clone();
    let provider = FakeProvider::with(
        Box::new(move |index, _body| {
            if index == 0 {
                return final_answer("probe-ok");
            }
            if index == 1 {
                return tool_call(
                    "commands__run",
                    json!({"id":"opportunity.create","version":1,"args":create_args("Restart created", &restart_party)}),
                    "create",
                );
            }
            final_answer("done")
        }),
        false,
    )
    .await;
    set_provider(port, &provider.url(), "provider-u03-6").await;
    let (status, run) = start_run(
        port,
        "Restart after create",
        json!([{"kind":"party","id":party}]),
        json!(["opportunity.create"]),
        true,
        "restart-key-01",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    assert_eq!(
        wait_state(port, id, &["succeeded", "failed"]).await["state"],
        "succeeded"
    );
    drop(workspace);
    drop(service);
    provider.stop().await;

    let reopened = Workspace::open(&path).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM opportunity WHERE title='Restart created'"
        )
        .fetch_one(reopened.pool())
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT executed_writes FROM agent_run_budget WHERE run_id=?")
            .bind(id)
            .fetch_one(reopened.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM command_audit_event WHERE command_id='opportunity.create' AND kind='outcome'")
            .fetch_one(reopened.pool()).await.unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM event WHERE event_type='opportunity.created' AND entity_id=(SELECT id FROM opportunity WHERE title='Restart created')")
            .fetch_one(reopened.pool()).await.unwrap(),
        1
    );
    cleanup(
        &path,
        reopened,
        parley_service::start_workspace(&path).await.unwrap(),
    );
}
