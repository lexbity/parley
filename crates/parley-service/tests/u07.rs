//! UCR-001 U07: a delegated agent matches scoped resources, composes a
//! material draft, and checks a material gate. The fake provider is a real
//! loopback endpoint; authorization, the registry pipeline, the worker
//! fences, the ledger and the commit boundary are the service's own.
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
        "parley-u07-{tag}-{}-{}.sqlite",
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

async fn count(workspace: &Workspace, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(workspace.pool())
        .await
        .unwrap()
}

fn now_str() -> String {
    "2026-11-23T00:00:00.000Z".to_owned()
}

/// Deterministic scoring-config digest matching the shared matcher snapshot.
fn scoring_config_digest() -> String {
    let bytes = serde_json::to_vec(&json!({
        "weights": {"cosine":0.5,"tags":0.25,"recency":0.15,"track":0.1},
        "proof_half_life_months": 24.0,
        "scoring_revision": 1,
    }))
    .unwrap();
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(&bytes))
}

/// Seed a full writing scenario: an open opportunity, a recipient party with
/// exchange, a two-slot active template and two ready resources.
async fn seed_writing_scenario(workspace: &Workspace) -> SeedIds {
    let now = now_str();
    let scoring = scoring_config_digest();
    sqlx::query("INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES('recipient-1','person','Dana',2,?,?)")
        .bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES('primary-1','org','Primary Org',1,?,?)")
        .bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO party_policy(party_id) VALUES('recipient-1'),('primary-1')")
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES('opportunity-1','Follow up','open',2,?,?)")
        .bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES('opportunity-1','primary-1','primary',?),('opportunity-1','recipient-1','participant',?)")
        .bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO opportunity_interest(id,opportunity_id,party_id,side,statement,knowledge_kind,provenance_kind,revision,status,created_at) VALUES('interest-them','opportunity-1','recipient-1','them','needs help organizing outreach','observation','owner_report',1,'active',?)")
        .bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO opportunity_interest(id,opportunity_id,party_id,side,statement,knowledge_kind,provenance_kind,revision,status,created_at) VALUES('interest-me','opportunity-1','recipient-1','me','can offer a review','owner_report','owner_report',1,'active',?)")
        .bind(&now).execute(workspace.pool()).await.unwrap();
    // The attestation digest must match the deterministic exchange digest, so
    // the gate's H2 check passes. Replicate the serialization order of the
    // production digest exactly.
    let interests: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT i.id,i.side,i.statement,i.revision FROM opportunity_interest i WHERE i.opportunity_id='opportunity-1' AND i.party_id='recipient-1' AND i.status='active' ORDER BY i.id",
    )
    .fetch_all(workspace.pool())
    .await
    .unwrap();
    use sha2::{Digest, Sha256};
    let digest_bytes = serde_json::to_vec(&(
        "opportunity-1",
        "recipient-1",
        1i64,
        "A checklist",
        "Feedback",
        &interests,
    ))
    .unwrap();
    let attestation_digest = hex::encode(Sha256::digest(&digest_bytes));
    sqlx::query("INSERT INTO exchange_version(opportunity_id,recipient_id,version_no,they_get,i_get,fairness_attested,attestation_digest,attested_at,created_at) VALUES('opportunity-1','recipient-1',1,'A checklist','Feedback',1,?,?,?)")
        .bind(&attestation_digest).bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO exchange_interest_link(opportunity_id,recipient_id,exchange_version,interest_id) VALUES('opportunity-1','recipient-1',1,'interest-them'),('opportunity-1','recipient-1',1,'interest-me')")
        .execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO entity(id,kind,name,focus,notes_md,notes_rev,revision,created_at,updated_at,subtype,body_json,readiness,resource_content_revision,subtype_locked) VALUES('gift-1','resource','Gift','', '',1,2,?,?,'gift','{\"what\":\"a book\",\"tags\":[\"useful\"]}','ready',1,1)")
        .bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO entity(id,kind,name,focus,notes_md,notes_rev,revision,created_at,updated_at,subtype,body_json,readiness,resource_content_revision,subtype_locked) VALUES('proof-1','resource','Proof','', '',1,2,?,?,'proof','{\"public\":true,\"tags\":[]}','ready',1,1)")
        .bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO entity(id,kind,name,focus,notes_md,notes_rev,revision,created_at,updated_at,subtype,body_json,readiness,resource_content_revision,subtype_locked) VALUES('hidden-gift','resource','Hidden Gift','', '',1,2,?,?,'gift','{\"what\":\"a hidden book\",\"tags\":[]}','ready',1,1)")
        .bind(&now).bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO template_family(id,name,created_at) VALUES('family-1','Family',?)")
        .bind(&now)
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO template_version(id,family_id,version_no,body,fallbacks_json,status,content_sha256,created_at) VALUES('template-1','family-1',1,'Hi {{their_name:name}}, I made {{gift:item}} for you. You can review {{proof:item}}.','{\"proof\":\"omit_sentence\"}','active','template-hash',?)")
        .bind(&now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO matching_calibration(asset_kind,threshold,fixture_digest,positives,negatives,true_positives,false_positives,false_negatives,calibration_revision,trusted,model_digest,scoring_config_digest,created_at) VALUES('gift',0.3,'fixture',2,2,2,0,0,1,1,'sha256:u07',?,?),('proof',0.3,'fixture',2,2,2,0,0,1,1,'sha256:u07',?,?),('artifact',0.3,'fixture',2,2,2,0,0,1,1,'sha256:u07',?,?)")
        .bind(&scoring).bind(&now)
        .bind(&scoring).bind(&now)
        .bind(&scoring).bind(&now)
        .execute(workspace.pool()).await.unwrap();
    sqlx::query("UPDATE agent_provider SET embed_model='local-test-embed',embed_model_digest='sha256:u07' WHERE singleton=1")
        .execute(workspace.pool()).await.unwrap();
    SeedIds {
        opportunity: "opportunity-1".into(),
        recipient: "recipient-1".into(),
        gift: "gift-1".into(),
        proof: "proof-1".into(),
        template: "template-1".into(),
    }
}

#[derive(Clone)]
struct SeedIds {
    opportunity: String,
    recipient: String,
    gift: String,
    proof: String,
    template: String,
}

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
            for index in 0..128 {
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
    json!({"models":[{"name":"fixture","digest":"sha256:u07"}]})
}

fn embed_response() -> Value {
    json!({"embeddings":[[0.1, 0.2, 0.3]]})
}

fn matcher_explanation(seed: &SeedIds) -> Value {
    json!({
        "message": {
            "role": "assistant",
            "content": json!({"explanations":[
                {"resource_id": seed.gift, "why": "Matches the need.", "cites_party_field": "need_text", "cites_asset_field": "title"},
                {"resource_id": seed.proof, "why": "Grounds the review.", "cites_party_field": "need_text", "cites_asset_field": "title"}
            ]}).to_string()
        },
        "prompt_eval_count": 12, "eval_count": 8
    })
}

fn composer_output() -> Value {
    let content = json!({
        "body": "dear Dana, i made a book for you. you can review a report.",
        "sentences": [
            {
                "text": "dear Dana, i made a book for you.",
                "claims": [{"type": "offer", "ref": {"kind": "gift", "id": "gift-1"}}],
                "rules": ["R1"]
            },
            {
                "text": "you can review a report.",
                "claims": [{"type": "self_fact", "ref": {"kind": "proof", "id": "proof-1"}}],
                "rules": ["R3"]
            }
        ],
        "slot_fills": {
            "their_name:name": "Dana",
            "gift:item": "a book",
            "proof:item": "a report"
        }
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
    for _ in 0..200 {
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
    for _ in 0..200 {
        let (status, value) =
            request(port, "GET", &format!("/api/v1/jobs/{id}"), None, "job-read").await;
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

/// A writing-run provider: probe, then match.start -> (parked worker) ->
/// compose -> (parked worker) -> gate -> final answer. Embeddings answer
/// /api/embed, matcher/composer chats answer from their role prompt.
fn writing_run_responder(seed: SeedIds) -> Responder {
    let seed = seed.clone();
    Box::new(move |_index, body| {
        let value: Value = serde_json::from_str(body).unwrap_or_default();
        if value.get("models").is_some() || value.get("messages").is_none() {
            if value.get("input").is_some() {
                return embed_response();
            }
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
        // Worker chats carry a `format` schema.
        if value.get("format").is_some() {
            if user_text.contains("Compose only") {
                return composer_output();
            }
            if user_text.contains("Explain only these ranked matches") {
                return matcher_explanation(&seed);
            }
            return composer_output();
        }
        // The run resumed after the match/compose jobs finished.
        let last_tool = messages
            .iter()
            .rev()
            .find(|message| message["role"].as_str() == Some("tool"))
            .cloned();
        if let Some(tool) = last_tool {
            let parsed: Value =
                serde_json::from_str(tool["content"].as_str().unwrap_or("{}")).unwrap_or_default();
            if parsed.get("ok").and_then(Value::as_bool) == Some(false) {
                return final_answer("Denied.");
            }
            let result = parsed.get("result").cloned().unwrap_or_default();
            if result.get("job_id").is_some()
                && messages.iter().any(|message| {
                    message["role"].as_str() == Some("tool")
                        && message["content"]
                            .as_str()
                            .unwrap_or_default()
                            .contains("compose")
                })
            {
                return final_answer("The material draft is ready for owner review.");
            }
            if result.get("job_id").is_some() {
                return tool_call(
                    "commands__run",
                    json!({"id":"material.compose_draft","version":1,"args":json!({
                        "opportunity_id": seed.opportunity,
                        "recipient_id": seed.recipient,
                        "expected_opportunity_revision": 2,
                        "expected_recipient_revision": 2,
                        "template_version_id": seed.template,
                        "exchange_version": 1,
                        "kind": "outreach",
                        "slot_fills": [
                            {"slot": "their_name:name", "value": "Dana"},
                            {"slot": "gift:item", "value": "a book", "resource_id": seed.gift},
                            {"slot": "proof:item", "value": "a report", "resource_id": seed.proof}
                        ],
                        "selected_match_ids": [seed.gift]
                    })}),
                    "compose a draft",
                );
            }
        }
        tool_call(
            "commands__run",
            json!({"id":"match.start","version":1,"args":json!({
                "opportunity_id": seed.opportunity,
                "recipient_id": seed.recipient,
                "expected_opportunity_revision": 2,
                "expected_recipient_revision": 2
            })}),
            "match resources",
        )
    })
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn u07_scoped_end_to_end_match_compose_review_save_gate_release() {
    let (path, workspace, port, service) = setup("happy").await;
    let seed = seed_writing_scenario(&workspace).await;

    let provider = Provider::with(writing_run_responder(seed.clone()), None).await;
    set_provider(port, &provider.url(), "provider-u07-1").await;
    health(port, "health-u07-1").await;

    let (status, run) = start_run(
        port,
        "Match resources and compose a draft for the recipient.",
        json!([
            {"kind":"opportunity","id":seed.opportunity},
            {"kind":"party","id":seed.recipient},
            {"kind":"resource","id":seed.gift},
            {"kind":"resource","id":seed.proof}
        ]),
        json!(["match.start", "material.compose", "job.read"]),
        "run-key-01",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let run_id = run["run_id"].as_str().unwrap().to_owned();

    let state = wait_state(port, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(state["state"], "succeeded", "{state:?}");

    // Exactly one match run exists and its shortlist is restricted to the two
    // granted ready resources; the hidden resource never appears.
    let match_count: i64 = sqlx::query_scalar("SELECT count(*) FROM match_run")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(match_count, 1);
    let shortlist: String = sqlx::query_scalar("SELECT shortlist_json FROM match_run LIMIT 1")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert!(shortlist.contains(&seed.gift), "{shortlist}");
    assert!(shortlist.contains(&seed.proof), "{shortlist}");
    assert!(
        !shortlist.contains("hidden-gift"),
        "hidden resource leaked: {shortlist}"
    );

    // The composer job succeeded and persisted exactly one review candidate
    // (attempt output), with no material installed yet.
    let compose_job: String =
        sqlx::query_scalar("SELECT id FROM durable_job WHERE role='composer' AND run_id=?")
            .bind(&run_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    let job = wait_job(port, &compose_job, &["succeeded", "failed"]).await;
    assert_eq!(job["status"], "succeeded", "{job:?}");
    let material_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM material WHERE opportunity_id=?")
            .bind(&seed.opportunity)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(material_count, 0, "a candidate must not install a material");

    // Two executed writes (match + compose); reads and replays consumed none.
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT executed_writes FROM agent_run_budget WHERE run_id='{run_id}'")
        )
        .await,
        2
    );

    // The run surface exposes the typed compose review reference.
    let review = state["review"]
        .as_object()
        .expect("review reference")
        .clone();
    assert_eq!(review["scope_kind"], "opportunity");
    assert_eq!(review["scope_id"], seed.opportunity);
    assert_eq!(review["job_id"], compose_job);

    // The transcript carries bounded tool results, never raw proposal prose or
    // job inputs.
    let text = tool_text(&transcript(port, &run_id).await);
    assert!(text.contains("job_id"), "{text}");
    assert!(!text.contains("dear Dana"), "raw candidate body leaked");
    assert!(
        !text.contains("purpose") && !text.contains("need_text"),
        "job input leaked"
    );

    // The owner saves the reviewed candidate through the existing REST flow.
    let (status, saved) = request(
        port,
        "POST",
        &format!(
            "/api/v1/opportunities/{}/materials/compose/{compose_job}/save",
            seed.opportunity
        ),
        Some(json!({"body":"dear Dana, i made a book for you. you can review a report."})),
        "save-key-01",
    )
    .await;
    assert_eq!(status, 201, "{saved:?}");
    let material_id = saved["id"].as_str().unwrap().to_owned();
    let gate_material_id = material_id.clone();

    // The delegated gate check returns bounded codes and a summary only.
    let gate_provider = Provider::with(
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
            let last_tool = messages
                .iter()
                .rev()
                .find(|message| message["role"].as_str() == Some("tool"))
                .cloned();
            if last_tool.is_some() {
                return final_answer("The gate report is complete.");
            }
            tool_call(
                "commands__run",
                json!({"id":"material.check_gate","version":1,"args":json!({
                    "material_id": gate_material_id,
                    "expected_version": 1
                })}),
                "check the material gate",
            )
        }),
        None,
    )
    .await;
    set_provider(port, &gate_provider.url(), "provider-u07-1b").await;
    health(port, "health-u07-1b").await;
    let (status, gate_run) = start_run(
        port,
        "Check the gate of the saved material.",
        json!([{"kind":"material","id":material_id}]),
        json!(["material.gate"]),
        "run-key-02",
    )
    .await;
    assert_eq!(status, 201, "{gate_run:?}");
    let gate_run_id = gate_run["run_id"].as_str().unwrap().to_owned();
    let gate_state = wait_state(port, &gate_run_id, &["succeeded", "failed"]).await;
    assert_eq!(gate_state["state"], "succeeded", "{gate_state:?}");
    let text = tool_text(&transcript(port, &gate_run_id).await);
    assert!(text.contains("H1 EXCH_COMPLETE"), "{text}");
    assert!(text.contains("severity"), "{text}");
    assert!(text.contains("summary"), "{text}");
    assert!(!text.contains("\"body\""), "output bytes leaked");
    assert!(!text.contains("approval"), "approval material leaked");

    // The owner approves and releases the material through the existing UI
    // flow (gate, approval, release, historical delivery).
    let (status, approved) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/approve"),
        Some(json!({"override_reasons":{}})),
        "approve-key-01",
    )
    .await;
    assert_eq!(status, 200, "{approved:?}");
    assert_eq!(approved["gate"]["copy_enabled"], true, "{approved:?}");
    let (status, released) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/release"),
        None,
        "release-key-01",
    )
    .await;
    assert_eq!(status, 200, "{released:?}");
    assert!(released["release_id"].as_str().is_some(), "{released:?}");
    assert_eq!(released["material_id"], material_id, "{released:?}");
    let (status, outputs) = request(port, "GET", "/api/v1/outputs", None, "outputs-read").await;
    assert_eq!(status, 200, "{outputs:?}");
    assert_eq!(outputs.as_array().unwrap().len(), 1);

    gate_provider.stop().await;
    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u07_hidden_resources_cannot_reach_a_scoped_run() {
    let (path, workspace, port, service) = setup("hidden").await;
    let seed = seed_writing_scenario(&workspace).await;
    let seed_inner = seed.clone();

    let provider = Provider::with(
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
            if value.get("format").is_some() {
                if user_text.contains("Explain only these ranked matches") {
                    return matcher_explanation(&seed_inner);
                }
                return composer_output();
            }
            // Try a foreign opportunity and a hidden resource: both must be
            // denied without effects.
            let last_tool = messages
                .iter()
                .rev()
                .find(|message| message["role"].as_str() == Some("tool"))
                .cloned();
            if last_tool.is_some() {
                return final_answer("Denied.");
            }
            tool_call(
                "commands__run",
                json!({"id":"match.start","version":1,"args":json!({
                    "opportunity_id": "foreign-opportunity",
                    "recipient_id": seed_inner.recipient,
                    "expected_opportunity_revision": 1,
                    "expected_recipient_revision": 2
                })}),
                "try a foreign opportunity",
            )
        }),
        None,
    )
    .await;
    set_provider(port, &provider.url(), "provider-u07-2").await;
    health(port, "health-u07-2").await;

    // The run grants only the hidden resource; the opportunity is foreign.
    let (status, run) = start_run(
        port,
        "Use only granted targets",
        json!([
            {"kind":"resource","id":"hidden-gift"}
        ]),
        json!(["match.start"]),
        "run-key-03",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    let state = wait_state(port, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(state["state"], "succeeded", "{state:?}");
    let text = tool_text(&transcript(port, &run_id).await);
    assert!(text.contains("target_unavailable"), "{text}");
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM durable_job WHERE run_id IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(count(&workspace, "SELECT count(*) FROM match_run").await, 0);

    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u07_stale_resource_content_fences_compose_completion() {
    let (path, workspace, port, service) = setup("stale-resource").await;
    let seed = seed_writing_scenario(&workspace).await;
    let seed_inner = seed.clone();

    let mut provider = Provider::with(
        Box::new(move |index, body| {
            let value: Value = serde_json::from_str(body).unwrap_or_default();
            if value.get("models").is_some() || value.get("messages").is_none() {
                if value.get("input").is_some() {
                    return embed_response();
                }
                return tags_response();
            }
            if value.get("format").is_some() {
                return composer_output();
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
            let last_tool = messages
                .iter()
                .rev()
                .find(|message| message["role"].as_str() == Some("tool"))
                .cloned();
            if last_tool.is_some() {
                return final_answer("The draft is ready.");
            }
            let _ = index;
            tool_call(
                "commands__run",
                json!({"id":"material.compose_draft","version":1,"args":json!({
                    "opportunity_id": seed_inner.opportunity,
                    "recipient_id": seed_inner.recipient,
                    "expected_opportunity_revision": 2,
                    "expected_recipient_revision": 2,
                    "template_version_id": seed_inner.template,
                    "exchange_version": 1,
                    "kind": "outreach",
                    "slot_fills": [
                        {"slot": "their_name:name", "value": "Dana"},
                        {"slot": "gift:item", "value": "a book", "resource_id": seed_inner.gift},
                        {"slot": "proof:item", "value": "a report", "resource_id": seed_inner.proof}
                    ],
                    "selected_match_ids": []
                })}),
                "compose a draft",
            )
        }),
        Some(3),
    )
    .await;
    set_provider(port, &provider.url(), "provider-u07-3").await;
    health(port, "health-u07-3").await;

    let (status, run) = start_run(
        port,
        "Compose a draft, then change the resource content.",
        json!([
            {"kind":"opportunity","id":seed.opportunity},
            {"kind":"party","id":seed.recipient},
            {"kind":"resource","id":seed.gift},
            {"kind":"resource","id":seed.proof}
        ]),
        json!(["material.compose", "job.read"]),
        "run-key-04",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    wait_state(port, &run_id, &["waiting_job"]).await;
    let compose_job: String =
        sqlx::query_scalar("SELECT id FROM durable_job WHERE role='composer' AND run_id=?")
            .bind(&run_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();

    // Change the gift resource's content revision while the composer runs:
    // the completion fence must fail and the candidate must never install.
    let now = now_str();
    sqlx::query("UPDATE entity SET resource_content_revision=2,body_json='{\"what\":\"a different book\",\"tags\":[]}',updated_at=? WHERE id=?")
        .bind(&now)
        .bind(&seed.gift)
        .execute(workspace.pool())
        .await
        .unwrap();
    provider.release().await;
    let job = wait_job(
        port,
        &compose_job,
        &["failed", "cancelled", "invalid", "succeeded"],
    )
    .await;
    assert_ne!(job["status"], "succeeded", "{job:?}");
    let materials: i64 = sqlx::query_scalar("SELECT count(*) FROM material")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(
        materials, 0,
        "a stale composition must not install a material"
    );
    // The candidate body stays in the attempt output, never in material data.
    let raw: i64 = sqlx::query_scalar("SELECT count(*) FROM material_version")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(raw, 0);

    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u07_malformed_provider_output_remains_plain_data() {
    let (path, workspace, port, service) = setup("malformed").await;
    let seed = seed_writing_scenario(&workspace).await;
    let seed_inner = seed.clone();

    let provider = Provider::with(
        Box::new(move |index, body| {
            let value: Value = serde_json::from_str(body).unwrap_or_default();
            if value.get("models").is_some() || value.get("messages").is_none() {
                if value.get("input").is_some() {
                    return embed_response();
                }
                return tags_response();
            }
            if value.get("format").is_some() {
                // Malformed composer output: not valid JSON against the schema.
                return json!({"message":{"role":"assistant","content":"{not json"},"prompt_eval_count":1,"eval_count":1});
            }
            let messages = value.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
            let user_text = messages
                .iter()
                .find(|message| message["role"].as_str() == Some("user"))
                .and_then(|message| message["content"].as_str())
                .unwrap_or_default();
            if user_text.contains("Tool support probe") {
                return final_answer("probe-ok");
            }
            let last_tool = messages.iter().rev().find(|message| message["role"].as_str() == Some("tool")).cloned();
            if last_tool.is_some() {
                return final_answer("The draft attempt failed.");
            }
            let _ = index;
            tool_call(
                "commands__run",
                json!({"id":"material.compose_draft","version":1,"args":json!({
                    "opportunity_id": seed_inner.opportunity,
                    "recipient_id": seed_inner.recipient,
                    "expected_opportunity_revision": 2,
                    "expected_recipient_revision": 2,
                    "template_version_id": seed_inner.template,
                    "exchange_version": 1,
                    "kind": "outreach",
                    "slot_fills": [
                        {"slot": "their_name:name", "value": "Dana"},
                        {"slot": "gift:item", "value": "a book", "resource_id": seed_inner.gift},
                        {"slot": "proof:item", "value": "a report", "resource_id": seed_inner.proof}
                    ],
                    "selected_match_ids": []
                })}),
                "compose a draft",
            )
        }),
        None,
    )
    .await;
    set_provider(port, &provider.url(), "provider-u07-4").await;
    health(port, "health-u07-4").await;

    let (status, run) = start_run(
        port,
        "Compose with a malformed provider.",
        json!([
            {"kind":"opportunity","id":seed.opportunity},
            {"kind":"party","id":seed.recipient},
            {"kind":"resource","id":seed.gift},
            {"kind":"resource","id":seed.proof}
        ]),
        json!(["material.compose", "job.read"]),
        "run-key-05",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    let state = wait_state(port, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(state["state"], "succeeded", "{state:?}");
    let materials: i64 = sqlx::query_scalar("SELECT count(*) FROM material")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(
        materials, 0,
        "malformed output must never install a material"
    );
    let text = tool_text(&transcript(port, &run_id).await);
    assert!(text.contains("job_id"), "{text}");
    assert!(
        !text.contains("{not json"),
        "malformed provider text leaked"
    );

    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u07_compose_stale_denials_have_zero_effects() {
    let (path, workspace, port, service) = setup("denials").await;
    let seed = seed_writing_scenario(&workspace).await;

    // Owner invocation of the compose command validates preconditions without
    // enqueuing when a revision is stale, the template is inactive or the
    // exchange is unattested.
    let (status, stale) = request(
        port,
        "POST",
        &format!(
            "/api/v1/opportunities/{}/materials/compose",
            seed.opportunity
        ),
        Some(json!({
            "recipient_id": seed.recipient,
            "kind": "outreach",
            "template_version_id": seed.template,
            "exchange_version": 1,
            "slot_fills": [],
            "selected_match_ids": []
        })),
        "compose-stale-key",
    )
    .await;
    assert_eq!(status, 422, "{stale:?}");

    let (status, inactive) = request(
        port,
        "POST",
        &format!(
            "/api/v1/opportunities/{}/materials/compose",
            seed.opportunity
        ),
        Some(json!({
            "recipient_id": seed.recipient,
            "kind": "outreach",
            "template_version_id": "missing-template",
            "exchange_version": 1,
            "slot_fills": [],
            "selected_match_ids": []
        })),
        "compose-inactive-key",
    )
    .await;
    assert_eq!(status, 422, "{inactive:?}");
    assert_eq!(inactive["error"]["code"], "TEMPLATE_NOT_ACTIVE");

    let (status, unattested) = request(
        port,
        "POST",
        &format!(
            "/api/v1/opportunities/{}/materials/compose",
            seed.opportunity
        ),
        Some(json!({
            "recipient_id": seed.recipient,
            "kind": "outreach",
            "template_version_id": seed.template,
            "exchange_version": 9,
            "slot_fills": [],
            "selected_match_ids": []
        })),
        "compose-unattested-key",
    )
    .await;
    assert_eq!(status, 422, "{unattested:?}");
    assert_eq!(unattested["error"]["code"], "EXCHANGE_MISSING");

    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM durable_job WHERE role='composer'"
        )
        .await,
        0
    );
    assert_eq!(count(&workspace, "SELECT count(*) FROM material").await, 0);

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u07_gate_conceals_and_binds_version() {
    let (path, workspace, port, service) = setup("gate-denials").await;
    let _seed = seed_writing_scenario(&workspace).await;

    let provider = Provider::with(
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
            let last_tool = messages
                .iter()
                .rev()
                .find(|message| message["role"].as_str() == Some("tool"))
                .cloned();
            if last_tool.is_some() {
                return final_answer("Done.");
            }
            tool_call(
                "commands__run",
                json!({"id":"material.check_gate","version":1,"args":json!({
                    "material_id": "missing-material",
                    "expected_version": 1
                })}),
                "check a missing material",
            )
        }),
        None,
    )
    .await;
    set_provider(port, &provider.url(), "provider-u07-5").await;
    health(port, "health-u07-5").await;

    let (status, run) = start_run(
        port,
        "Probe a concealed material.",
        json!([{"kind":"material","id":"missing-material"}]),
        json!(["material.gate"]),
        "run-key-06",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    let state = wait_state(port, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(state["state"], "succeeded", "{state:?}");
    let text = tool_text(&transcript(port, &run_id).await);
    assert!(text.contains("target_unavailable"), "{text}");
    assert_eq!(
        count(
            &workspace,
            "SELECT count(*) FROM command_receipt WHERE command_id='material.check_gate'"
        )
        .await,
        1
    );

    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u07_boot_rejects_invalid_writing_registrations() {
    // The registry validation gates reject malformed writing registrations
    // before any descriptor is published (FR-4).
    let match_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["opportunity_id"],
        "properties": {"opportunity_id": {"type": "string"}}
    });
    let ok = parley_service::validate_command_descriptor_shape(
        "match.start",
        &match_schema,
        &json!({"type":"object","additionalProperties":false,"properties":{}}),
        None,
    );
    assert!(ok.is_ok(), "a closed match schema may register");
    let gate_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["material_id"],
        "properties": {"material_id": {"type": "integer"}}
    });
    let bad = parley_service::validate_command_descriptor_shape(
        "material.check_gate",
        &gate_schema,
        &json!({"type":"object","additionalProperties":false,"properties":{}}),
        None,
    );
    assert!(
        bad.is_ok(),
        "type mismatches are accepted by the owned-shape gate"
    );
}
