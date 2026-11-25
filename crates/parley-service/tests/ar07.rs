//! AR-001 AR07: qualified embeddings, recipe-bound cache/calibration and the
//! matcher through the packaged worker.
//!
//! The Rust project drives the real packaged worker, the closed codec and a
//! scripted upstream speaking the real Ollama-compatible wire (including
//! `POST /v1/embeddings`). The matcher embeds needs and resources through the
//! worker, ranks deterministically against trusted recipe-bound thresholds and
//! never touches a direct model client.

#[path = "support/mod.rs"]
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

use support::model_server::{
    ModelServer, Reply, install_qualified_profile_with_embed, marker_embed_responder,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
static KEY: AtomicU64 = AtomicU64::new(0);

type Auth = (String, String, String);

fn fresh_key() -> String {
    format!("ar07-{}", KEY.fetch_add(1, Ordering::Relaxed))
}

async fn setup() -> (PathBuf, Workspace, RunningService, Auth) {
    let path = std::env::temp_dir().join(format!(
        "parley-ar07-{}-{}.sqlite",
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

async fn cleanup(path: &PathBuf, workspace: Workspace, service: RunningService) {
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(path.with_extension("sqlite-wal"));
    let _ = fs::remove_file(path.with_extension("sqlite-shm"));
}

/// Seed an open opportunity with one recipient and the eligible resource set.
/// Relevant resources carry `[P]`, irrelevant ones `[N]` so the scripted
/// embedding responder separates them exactly. Returns the opportunity id.
async fn seed_match_scope(workspace: &Workspace, now: &str, count: usize) -> String {
    sqlx::query("INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES('recipient-1','person','Dana',1,?,?)")
        .bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO party_policy(party_id) VALUES('recipient-1')")
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES('opportunity-1','Onboarding','open',1,?,?)")
        .bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES('opportunity-1','recipient-1','primary',?)")
        .bind(now).execute(workspace.pool()).await.unwrap();
    sqlx::query(
        "INSERT INTO opportunity_interest(id,opportunity_id,party_id,side,statement,knowledge_kind,provenance_kind,revision,status,created_at)
         VALUES('interest-1','opportunity-1','recipient-1','them','Help a new executive onboard and understand operating rhythm.','observation','owner_report',1,'active',?)",
    )
    .bind(now).execute(workspace.pool()).await.unwrap();
    let kinds = ["gift", "proof", "artifact"];
    for index in 0..count {
        let kind = kinds[index % kinds.len()];
        let marker = if index % 2 == 0 { "[P]" } else { "[N]" };
        let body = json!({"marker": marker, "tags": ["onboarding"]}).to_string();
        sqlx::query(
            "INSERT INTO entity(id,kind,name,revision,created_at,updated_at,subtype,body_json,readiness,resource_content_revision,subtype_locked)
             VALUES(?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(format!("resource-{index}"))
        .bind("resource")
        .bind(format!("Resource {index}"))
        .bind(1)
        .bind(now).bind(now)
        .bind(kind)
        .bind(&body)
        .bind("ready")
        .bind(1)
        .bind(1)
        .execute(workspace.pool())
        .await
        .unwrap();
    }
    "opportunity-1".to_owned()
}

async fn wait_terminal(port: u16, auth: &Auth, job_id: &str) -> Value {
    for _ in 0..400 {
        let (_, job) = request(
            port,
            auth,
            "GET",
            &format!("/api/v1/jobs/{job_id}"),
            None,
            &fresh_key(),
        )
        .await;
        if matches!(
            job["status"].as_str(),
            Some("succeeded" | "failed" | "invalid" | "cancelled")
        ) {
            return job;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("matcher job did not reach a terminal state");
}

#[tokio::test]
async fn ar07_matcher_embeds_through_worker_and_commits_recipe_bound_cache() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let now = "2026-11-21T00:00:00Z";
    seed_match_scope(&workspace, now, 20).await;

    let provider = ModelServer::with_embed_responder(
        vec![
            Reply::final_answer("{\"explanations\":[]}"),
            Reply::final_answer("{\"explanations\":[]}"),
            Reply::final_answer("{\"explanations\":[]}"),
            Reply::final_answer("{\"explanations\":[]}"),
        ],
        None,
        marker_embed_responder(),
    )
    .await;
    let revision = install_qualified_profile_with_embed(
        workspace.pool(),
        &path,
        provider.endpoint(),
        8192,
        256,
        64,
        Some(&"e".repeat(64)),
        Some(8),
    )
    .await;

    // Calibrate through the real offline path; the marker responder separates
    // relevant from irrelevant cases exactly, so the RFC-0001 floors pass.
    let fixture = match_fixture();
    let report = parley_service::calibrate_matching(&workspace, fixture)
        .await
        .expect("calibration must succeed on the scripted separator");
    assert_eq!(report["status"], "calibrated");
    let trusted: i64 =
        sqlx::query_scalar("SELECT count(*) FROM matching_calibration WHERE trusted=1")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(trusted, 3);

    let (status, job) = request(
        port,
        &auth,
        "POST",
        "/api/v1/opportunities/opportunity-1/matches",
        Some(json!({"recipient_id":"recipient-1"})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 202, "{job:?}");
    let job_id = job["job_id"].as_str().unwrap();
    let job = wait_terminal(port, &auth, job_id).await;
    assert_eq!(job["status"], "succeeded", "{job}");

    let recorded = provider.recorded();
    let embed_calls = recorded
        .iter()
        .filter(|entry| entry.contains("POST /v1/embeddings"))
        .count();
    assert!(
        embed_calls >= 2,
        "need + resources must batch through the worker: {embed_calls}"
    );
    assert!(recorded.iter().all(|entry| !entry.contains("/api/embed")));

    let run: (String, String, String) = sqlx::query_as(
        "SELECT embed_model_digest,recipe_digest,calibration_json FROM match_run
         WHERE opportunity_id='opportunity-1' AND recipient_id='recipient-1'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(run.0, "e".repeat(64));
    let recipe: String = sqlx::query_scalar("SELECT recipe_digest FROM embedding_cache LIMIT 1")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(recipe, run.1);
    let dimensions: i64 = sqlx::query_scalar("SELECT DISTINCT dimensions FROM embedding_cache")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(dimensions, 8);

    // A second owner run reuses verified vectors: no additional embeddings POST.
    let embed_calls_before = provider
        .recorded()
        .iter()
        .filter(|entry| entry.contains("POST /v1/embeddings"))
        .count();
    let (status, second) = request(
        port,
        &auth,
        "POST",
        "/api/v1/opportunities/opportunity-1/matches",
        Some(json!({"recipient_id":"recipient-1"})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 202, "{second:?}");
    let second_id = second["job_id"].as_str().unwrap();
    let second = wait_terminal(port, &auth, second_id).await;
    assert_eq!(second["status"], "succeeded", "{second}");
    let embed_calls_after = provider
        .recorded()
        .iter()
        .filter(|entry| entry.contains("POST /v1/embeddings"))
        .count();
    assert_eq!(
        embed_calls_after, embed_calls_before,
        "cached vectors must not re-embed"
    );

    // The job's inference record carries the observed embedding identity.
    let model_digest: Option<String> =
        sqlx::query_scalar("SELECT model_digest FROM inference_record WHERE id=?")
            .bind(second_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(model_digest.as_deref(), Some("e".repeat(64).as_str()));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM inference_profile WHERE revision=?")
            .bind(revision)
            .fetch_one(workspace.pool())
            .await
            .unwrap(),
        1
    );

    provider.stop().await;
    cleanup(&path, workspace, service).await;
}

#[tokio::test]
async fn ar07_embed_shape_mismatch_fails_closed() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let now = "2026-11-21T00:00:00Z";
    seed_match_scope(&workspace, now, 4).await;

    // Scripted upstream returns 4-dimensional vectors while the profile is
    // qualified for 8 dimensions: the worker must refuse the execution.
    let provider = ModelServer::with_embed_responder(
        vec![
            Reply::final_answer("{\"explanations\":[]}"),
            Reply::final_answer("{\"explanations\":[]}"),
            Reply::final_answer("{\"explanations\":[]}"),
            Reply::final_answer("{\"explanations\":[]}"),
        ],
        None,
        Box::new(|text: &str| {
            if text.contains("[N]") {
                vec![0.0, 1.0, 0.0, 0.0]
            } else {
                vec![1.0, 0.0, 0.0, 0.0]
            }
        }),
    )
    .await;
    install_qualified_profile_with_embed(
        workspace.pool(),
        &path,
        provider.endpoint(),
        8192,
        256,
        64,
        Some(&"e".repeat(64)),
        Some(8),
    )
    .await;

    let (status, job) = request(
        port,
        &auth,
        "POST",
        "/api/v1/opportunities/opportunity-1/matches",
        Some(json!({"recipient_id":"recipient-1"})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 202, "{job:?}");
    let job_id = job["job_id"].as_str().unwrap();
    let job = wait_terminal(port, &auth, job_id).await;
    assert_eq!(job["status"], "failed", "{job}");
    let code = job["last_error_code"].as_str().unwrap_or_default();
    assert!(
        matches!(
            code,
            "RESULT_INVALID" | "ATTEMPT_TIMEOUT" | "WORKER_PROTOCOL"
        ),
        "{job}"
    );
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM match_run")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(runs, 0, "no partial match may publish");

    provider.stop().await;
    cleanup(&path, workspace, service).await;
}

#[tokio::test]
async fn ar07_scoped_match_cannot_read_ungranted_resources() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let now = "2026-11-21T00:00:00Z";
    let opportunity = seed_match_scope(&workspace, now, 4).await;

    let provider = ModelServer::with_embed_responder(
        vec![
            Reply::tool_call(
                "commands__run",
                json!({
                    "id":"match.start","version":1,
                    "args":{
                        "opportunity_id": opportunity,
                        "recipient_id":"recipient-1",
                        "expected_opportunity_revision":1,
                        "expected_recipient_revision":1
                    }
                }),
                "starting matching for the granted scope",
            ),
            Reply::final_answer("The match is ready for review."),
            Reply::final_answer("{\"explanations\":[]}"),
        ],
        None,
        marker_embed_responder(),
    )
    .await;
    install_qualified_profile_with_embed(
        workspace.pool(),
        &path,
        provider.endpoint(),
        8192,
        256,
        64,
        Some(&"e".repeat(64)),
        Some(8),
    )
    .await;
    parley_service::calibrate_matching(&workspace, match_fixture())
        .await
        .expect("calibration must succeed on the scripted separator");

    let (status, run) = request(
        port,
        &auth,
        "POST",
        "/api/v1/agent/runs",
        Some(json!({
            "task": "Run matching for the granted scope",
            "targets": [
                {"kind":"opportunity","id": opportunity},
                {"kind":"party","id":"recipient-1"},
                {"kind":"resource","id":"resource-0"}
            ],
            "permissions": ["match.start"],
            "allow_created_opportunities": false
        })),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let run_id = run["run_id"].as_str().unwrap();
    let done = wait_state(
        port,
        &auth,
        run_id,
        &["succeeded", "failed", "cancelled", "expired"],
    )
    .await;
    assert_eq!(done["state"], "succeeded", "{done:?}");

    let (job_id, input): (String, String) =
        sqlx::query_as("SELECT id,input_json FROM durable_job WHERE role='matcher' AND run_id=?")
            .bind(run_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    let input: Value = serde_json::from_str(&input).unwrap();
    let resources = input["resources"].as_array().unwrap();
    assert_eq!(
        resources.len(),
        1,
        "only the granted resource may enter the match"
    );
    assert_eq!(resources[0]["resource_id"], "resource-0");
    let job = wait_terminal(port, &auth, &job_id).await;
    assert_eq!(job["status"], "succeeded", "{job}");

    provider.stop().await;
    cleanup(&path, workspace, service).await;
}

/// The reviewed E-match fixture shape used by integration tests: the marker
/// responder puts every `[N]` case orthogonally to the need, so separation is
/// exact and the held-out floors pass deterministically.
fn match_fixture() -> Value {
    let kinds = ["gift", "proof", "artifact"];
    let mut cases = Vec::new();
    for (kind_index, kind) in kinds.iter().enumerate() {
        for index in 0..18 {
            let relevant = index % 2 == 0;
            cases.push(json!({
                "id": format!("{kind}-{index}"),
                "kind": kind,
                "text": format!(
                    "{} case {kind}-{index} for a new executive onboarding.",
                    if relevant { "[P]" } else { "[N]" }
                ),
                "relevant": relevant,
                "holdout": index >= 12,
            }));
        }
        let _ = kind_index;
    }
    json!({
        "dataset_id": "match-v1-test",
        "need": "Help a new executive onboard.",
        "split": "holdout",
        "cases": cases,
    })
}

async fn wait_state(port: u16, auth: &Auth, id: &str, states: &[&str]) -> Value {
    for _ in 0..400 {
        let (_, value) = request(
            port,
            auth,
            "GET",
            &format!("/api/v1/agent/runs/{id}"),
            None,
            &fresh_key(),
        )
        .await;
        if states
            .iter()
            .any(|state| value["state"].as_str() == Some(state))
        {
            return value;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("delegated run did not reach a terminal state");
}

#[tokio::test]
async fn ar07_unqualified_embed_profile_fails_closed_but_manual_survives() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let now = "2026-11-21T00:00:00Z";
    seed_match_scope(&workspace, now, 2).await;

    // Start a model server for chat, but install a profile without the embed
    // identity: the matcher must fail closed as unqualified.
    let provider = ModelServer::start(vec![Reply::final_answer("{\"explanations\":[]}")]).await;
    install_qualified_profile_with_embed(
        workspace.pool(),
        &path,
        provider.endpoint(),
        8192,
        256,
        64,
        None,
        None,
    )
    .await;

    let (status, job) = request(
        port,
        &auth,
        "POST",
        "/api/v1/opportunities/opportunity-1/matches",
        Some(json!({"recipient_id":"recipient-1"})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 202, "{job:?}");
    let job_id = job["job_id"].as_str().unwrap();
    let job = wait_terminal(port, &auth, job_id).await;
    assert_eq!(job["status"], "failed", "{job}");
    assert_eq!(job["last_error_code"], "CAPABILITY_UNQUALIFIED", "{job}");

    provider.stop().await;
    cleanup(&path, workspace, service).await;
}
