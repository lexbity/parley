//! AR-001 AR05: a delegated run is executed by the packaged, contained Pi
//! worker, journals every ordered tool intent before any effect, releases the
//! inference lane while it waits on a durable child, and resumes from the
//! host's canonical transcript. Authority, receipts and fences stay in Rust.
//!
//! Both browser projects are separate; this Rust project uses the real
//! production worker, the real closed codec, the real sandbox and a scripted
//! upstream speaking the same Ollama-compatible protocol as the provider.

mod support;

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

use support::model_server::{ModelServer, Reply, install_qualified_profile};

static NEXT: AtomicU64 = AtomicU64::new(0);

type Auth = (String, String, String);

async fn setup() -> (PathBuf, Workspace, RunningService, Auth) {
    let path = std::env::temp_dir().join(format!(
        "parley-ar05-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let service = parley_service::start_workspace(&path).await.unwrap();
    let address = service.address;
    let body = json!({"secret": service.bootstrap_secret}).to_string();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!("POST /api/v1/session/bootstrap HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len())
                .as_bytes(),
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
    let auth = (
        cookie,
        payload["mutation_token"].as_str().unwrap().to_owned(),
        payload["epoch"].as_str().unwrap().to_owned(),
    );
    let workspace = Workspace::open(&path).await.unwrap();
    (path, workspace, service, auth)
}

async fn request(
    port: u16,
    auth: &Auth,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: &str,
) -> (u16, Value) {
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(
            format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {}\r\nX-Parley-Token: {}\r\nX-Parley-Epoch: {}\r\nIdempotency-Key: {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", auth.0, auth.1, auth.2, body.len())
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
    let value: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(payload).unwrap()
    };
    (status, value.get("data").cloned().unwrap_or(value))
}

/// The pre-cut role path used by a child job still reads the legacy provider
/// row; point it at the same scripted upstream as the qualified worker.
async fn set_legacy_provider(port: u16, auth: &Auth, url: &str) {
    let (status, reply) = request(
        port,
        auth,
        "PUT",
        "/api/v1/agent/provider",
        Some(json!({
            "endpoint": url, "model": "parley-fixture:latest", "num_ctx": 8192,
            "num_predict": 256, "locale": "en"
        })),
        "ar05-legacy-provider",
    )
    .await;
    assert!(status == 200 || status == 204, "{status} {reply:?}");
    let (status, reply) = request(
        port,
        auth,
        "POST",
        "/api/v1/agent/provider/health",
        None,
        "ar05-legacy-health",
    )
    .await;
    assert_eq!(status, 200, "{status} {reply:?}");
}

async fn create_party(port: u16, auth: &Auth, name: &str) -> String {
    let (status, party) = request(
        port,
        auth,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"person","name":name})),
        &format!("ar05-party-{name}"),
    )
    .await;
    assert_eq!(status, 201, "{party:?}");
    party["id"].as_str().unwrap().to_owned()
}

async fn start_run(
    port: u16,
    auth: &Auth,
    task: &str,
    targets: Value,
    permissions: Value,
    key: &str,
) -> (u16, Value) {
    request(
        port,
        auth,
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

async fn wait_state(port: u16, auth: &Auth, id: &str, states: &[&str]) -> Value {
    for _ in 0..300 {
        let (_, value) = request(
            port,
            auth,
            "GET",
            &format!("/api/v1/agent/runs/{id}"),
            None,
            "ar05-status",
        )
        .await;
        if states
            .iter()
            .any(|state| value["state"].as_str() == Some(state))
        {
            return value;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("run {id} never reached {states:?}");
}

async fn cleanup(path: &PathBuf, workspace: Workspace, service: RunningService) {
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(path.with_extension("sqlite-wal"));
    let _ = fs::remove_file(path.with_extension("sqlite-shm"));
}

#[tokio::test]
async fn ar05_packaged_pi_run_commits_one_scoped_read_and_finishes() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let party = create_party(port, &auth, "AR05 Scoped Party").await;
    let provider = ModelServer::start(vec![
        Reply::tool_call(
            "party__get_summary",
            json!({"party_id": party}),
            "checking the granted party",
        ),
        Reply::final_answer("The granted party is ready for review."),
    ])
    .await;
    let revision = install_qualified_profile(workspace.pool(), &path, provider.endpoint()).await;
    let (status, run) = start_run(
        port,
        &auth,
        "Summarise the granted party",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "ar05-read-key-1",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    let done = wait_state(port, &auth, id, &["succeeded", "failed"]).await;
    assert_eq!(done["state"], "succeeded", "{done:?}");
    let pinned: (Option<i64>, i64) =
        sqlx::query_as("SELECT profile_revision,transcript_major FROM agent_run WHERE run_id=?")
            .bind(id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(pinned, (Some(revision), 1));
    let intents: Vec<(i64, Option<String>, Option<String>, String)> = sqlx::query_as(
        "SELECT ordinal,call_ref,accepted_at,state FROM agent_tool_intent WHERE run_id=? ORDER BY turn_no,ordinal",
    )
    .bind(id)
    .fetch_all(workspace.pool())
    .await
    .unwrap();
    assert_eq!(
        intents.len(),
        1,
        "the assistant turn is journaled before its effect"
    );
    assert!(intents[0].1.is_some() && intents[0].2.is_some());
    assert_eq!(intents[0].3, "completed");
    let requests = provider.recorded();
    let chats = requests
        .iter()
        .filter(|request| request.contains("POST /v1/chat/completions"))
        .count();
    assert!(
        chats >= 2,
        "one tool turn plus the final turn must reach the real provider: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("parley_session"))
    );
    provider.stop().await;
    cleanup(&path, workspace, service).await;
}

#[tokio::test]
async fn ar05_lost_ack_committed_tool_is_reconciled_once() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let party = create_party(port, &auth, "AR05 Lost Ack").await;
    let provider = ModelServer::start(vec![
        Reply::tool_call("party__get_summary", json!({"party_id": party}), "checking"),
        Reply::final_answer("finished"),
    ])
    .await;
    install_qualified_profile(workspace.pool(), &path, provider.endpoint()).await;
    let (status, run) = start_run(
        port,
        &auth,
        "Summarise the granted party",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "ar05-lost-ack-key",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    let done = wait_state(port, &auth, id, &["succeeded", "failed"]).await;
    assert_eq!(done["state"], "succeeded", "{done:?}");

    // A replay with the original operation key must recover the same run and
    // must not create a second tool effect or intent ordinal.
    let (replay_status, replay) = start_run(
        port,
        &auth,
        "Summarise the granted party",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "ar05-lost-ack-key",
    )
    .await;
    assert_eq!(replay_status, 201, "{replay:?}");
    assert_eq!(replay["run_id"], id);
    let (effects, intents): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM command_audit_event WHERE actor_kind='agent' AND command_id='party.get_summary' AND kind='outcome'),
                (SELECT count(*) FROM agent_tool_intent WHERE run_id=? AND ordinal=1)",
    )
    .bind(id)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(intents, 1);
    assert_eq!(effects, 1, "one tool ordinal must have exactly one effect");
    provider.stop().await;
    cleanup(&path, workspace, service).await;
}

#[tokio::test]
async fn ar05_foreign_tool_and_scope_never_reach_business_mutation() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let party = create_party(port, &auth, "AR05 Foreign Tool").await;
    let provider = ModelServer::start(vec![
        Reply::tool_call(
            "commands__run",
            json!({"id":"party.rename","version":1,"args":{"party_id":"foreign","name":"ignored"}}),
            "Attempting a command",
        ),
        Reply::final_answer("Nothing was changed."),
    ])
    .await;
    install_qualified_profile(workspace.pool(), &path, provider.endpoint()).await;
    let (status, run) = start_run(
        port,
        &auth,
        "Try an out-of-scope rename",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "ar05-foreign-key",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    let done = wait_state(port, &auth, id, &["succeeded", "failed"]).await;
    assert_eq!(done["state"], "succeeded", "{done:?}");
    let renamed: Option<String> = sqlx::query_scalar("SELECT name FROM entity WHERE id='foreign'")
        .fetch_optional(workspace.pool())
        .await
        .unwrap();
    assert_ne!(renamed.as_deref(), Some("ignored"));
    let transcripts = request(
        port,
        &auth,
        "GET",
        &format!("/api/v1/agent/runs/{id}/transcript"),
        None,
        "ar05-transcript",
    )
    .await
    .1;
    let serialized = transcripts.to_string();
    assert!(
        !serialized.contains("ignored"),
        "typed denial must not leak attempted mutations: {serialized}"
    );
    provider.stop().await;
    cleanup(&path, workspace, service).await;
}

#[tokio::test]
async fn ar05_cancel_and_completion_have_one_writer_order() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let party = create_party(port, &auth, "AR05 Fenced Cancel").await;
    let mut provider = ModelServer::with_gate(
        vec![
            Reply::tool_call("party__get_summary", json!({"party_id": party}), "checking"),
            Reply::final_answer("too late"),
        ],
        Some(0),
    )
    .await;
    install_qualified_profile(workspace.pool(), &path, provider.endpoint()).await;
    let (status, run) = start_run(
        port,
        &auth,
        "Summarise then cancel",
        json!([{"kind":"party","id":party}]),
        json!(["party.read"]),
        "ar05-cancel-key",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    for _ in 0..100 {
        let (_, current) = request(
            port,
            &auth,
            "GET",
            &format!("/api/v1/agent/runs/{id}"),
            None,
            "ar05-status",
        )
        .await;
        if current["state"] == "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let (cancel_status, cancelled) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/agent/runs/{id}/cancel"),
        None,
        "ar05-cancel-op",
    )
    .await;
    assert_eq!(cancel_status, 200, "{cancelled:?}");
    provider.release().await;
    let done = wait_state(port, &auth, id, &["cancelled", "succeeded", "failed"]).await;
    assert_eq!(done["state"], "cancelled", "{done:?}");
    let late: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent_run_turn WHERE run_id=? AND role='assistant' AND ordinal=0 AND body='too late'",
    )
    .bind(id)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(
        late, 0,
        "a cancelled lease cannot publish a late assistant turn"
    );
    provider.stop().await;
    cleanup(&path, workspace, service).await;
}

#[tokio::test]
async fn ar05_partial_batch_waits_releases_lane_and_resumes() {
    use support::model_server::Reply as R;

    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let party = create_party(port, &auth, "AR05 Partial Batch").await;
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM entity WHERE id=?")
        .bind(&party)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    let brief_args = json!({
        "party_id": party,
        "expected_revision": revision,
        "purpose": "Prepare a reviewable brief"
    });
    let mut provider = ModelServer::with_native_held(
        vec![
            R::ToolCalls {
                text: "Starting a brief and checking the party".into(),
                calls: vec![
                    (
                        "commands__run".into(),
                        json!({"id":"brief.start","version":1,"args":brief_args}),
                    ),
                    (
                        "commands__run".into(),
                        json!({"id":"party.get_summary","version":1,"args":{"party_id":party}}),
                    ),
                ],
            },
            R::final_answer("The brief was prepared and the party checked."),
        ],
        vec![json!({
            "message": {"role":"assistant","content":"{\"response\":null,\"items\":[]}"},
            "prompt_eval_count": 12,
            "eval_count": 8
        })],
    )
    .await;
    set_legacy_provider(port, &auth, &provider.url()).await;
    install_qualified_profile(workspace.pool(), &path, provider.endpoint()).await;
    let (status, run) = start_run(
        port,
        &auth,
        "Start a brief then check the party",
        json!([{"kind":"party","id":party}]),
        json!(["brief.start", "job.read", "party.read"]),
        "ar05-partial-key",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    let waiting = wait_state(port, &auth, id, &["waiting_job", "succeeded", "failed"]).await;
    assert_eq!(waiting["state"], "waiting_job", "{waiting:?}");
    let pinned: Option<String> =
        sqlx::query_scalar("SELECT lease_token FROM agent_run WHERE run_id=?")
            .bind(id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert!(pinned.is_none(), "a waiting run holds no inference lease");
    let mut wait_binding: Option<String> = None;
    // Coverage builds run the dispatcher noticeably slower than a plain test.
    for _ in 0..250 {
        wait_binding = sqlx::query_scalar("SELECT wait_intent FROM agent_run WHERE run_id=?")
            .bind(id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
        if wait_binding.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        wait_binding.is_some(),
        "the wait is bound to its originating intent"
    );
    let pending: Vec<(i64, String, Option<String>)> = sqlx::query_as(
        "SELECT ordinal,state,accepted_at FROM agent_tool_intent WHERE run_id=? ORDER BY ordinal",
    )
    .bind(id)
    .fetch_all(workspace.pool())
    .await
    .unwrap();
    assert_eq!(
        pending.len(),
        2,
        "the full ordered call list was journaled before dispatch"
    );
    assert_eq!(pending[0].0, 1);
    assert_eq!(pending[1].0, 2);
    assert_ne!(
        pending[1].1, "completed",
        "the later ordinal is durable and not yet executed"
    );
    assert!(
        pending[1].2.is_some(),
        "the whole batch was journaled on acceptance"
    );

    provider.release_native().await;
    // The lane is free while the run waits: a second run can be claimed and
    // reaches a terminal state against the same upstream.
    let second_party = create_party(port, &auth, "AR05 Lane Free").await;
    let (second_status, second) = start_run(
        port,
        &auth,
        "Check the other party",
        json!([{"kind":"party","id":second_party}]),
        json!(["party.read"]),
        "ar05-lane-key",
    )
    .await;
    assert_eq!(second_status, 201, "{second:?}");
    let second_id = second["run_id"].as_str().unwrap();
    let second_done = wait_state(port, &auth, second_id, &["succeeded", "failed"]).await;
    assert_eq!(second_done["state"], "succeeded", "{second_done:?}");

    let done = wait_state(port, &auth, id, &["succeeded", "failed"]).await;
    assert_eq!(done["state"], "succeeded", "{done:?}");
    let intents: Vec<(i64, String)> = sqlx::query_as(
        "SELECT ordinal,state FROM agent_tool_intent WHERE run_id=? ORDER BY ordinal",
    )
    .bind(id)
    .fetch_all(workspace.pool())
    .await
    .unwrap();
    assert_eq!(intents.len(), 2);
    assert!(intents.iter().all(|(_, state)| state == "completed"));
    let tool_turns: i64 =
        sqlx::query_scalar("SELECT count(*) FROM agent_run_turn WHERE run_id=? AND role='tool'")
            .bind(id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(
        tool_turns, 2,
        "each accepted ordinal has exactly one projected result"
    );
    provider.stop().await;
    cleanup(&path, workspace, service).await;
}
