//! AR-001 AR08: Composer and Linter run through the packaged structured
//! worker, every chat role shares one worker boundary, and no direct legacy
//! provider client remains.
//!
//! The Rust project drives the real packaged worker, the closed codec and a
//! scripted upstream speaking the real Ollama-compatible wire. Deterministic
//! gates, approval and release stay fully off the model path.

#[path = "support/mod.rs"]
mod support;

use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use support::model_server::{ModelServer, install_qualified_profile_with_embed};

static NEXT: AtomicU64 = AtomicU64::new(0);
static KEY: AtomicU64 = AtomicU64::new(0);

type Auth = (String, String, String);

fn fresh_key() -> String {
    format!("ar08-{}", KEY.fetch_add(1, Ordering::Relaxed))
}

async fn setup() -> (PathBuf, Workspace, RunningService, Auth) {
    let path = std::env::temp_dir().join(format!(
        "parley-ar08-{}-{}.sqlite",
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

async fn wait_job(port: u16, auth: &Auth, id: &str, states: &[&str]) -> Value {
    for _ in 0..300 {
        let (status, value) = request(
            port,
            auth,
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
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("job {id} never reached {states:?}");
}

fn now_str() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

async fn seed_writing_scenario(workspace: &Workspace) -> (String, String, String) {
    let now = now_str();
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
    sqlx::query("INSERT INTO template_family(id,name,created_at) VALUES('family-1','Family',?)")
        .bind(&now)
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO template_version(id,family_id,version_no,body,fallbacks_json,status,content_sha256,created_at) VALUES('template-1','family-1',1,'Hi {{their_name:name}}, I made {{gift:item}} for you. You can review {{proof:item}}.','{\"proof\":\"omit_sentence\"}','active','template-hash',?)")
        .bind(&now).execute(workspace.pool()).await.unwrap();
    (
        "opportunity-1".into(),
        "recipient-1".into(),
        "template-1".into(),
    )
}

async fn install_profile(workspace: &Workspace, path: &Path, provider: &ModelServer) {
    install_qualified_profile_with_embed(
        workspace.pool(),
        path,
        provider.endpoint(),
        8192,
        256,
        64,
        Some(&"e".repeat(64)),
        Some(8),
    )
    .await;
}

fn composer_output() -> Value {
    let content = json!({
        "body": "dear Dana, i prepared a book for you. you can review a report.",
        "sentences": [
            {
                "text": "dear Dana, i prepared a book for you.",
                "claims": [{"type":"offer","ref":{"kind":"gift","id":"gift-1"}}],
                "rules": ["R1"]
            },
            {
                "text": "you can review a report.",
                "claims": [{"type":"self_fact","ref":{"kind":"proof","id":"proof-1"}}],
                "rules": ["R3"]
            }
        ],
        "slot_fills": {"their_name:name":"Dana","gift:item":"a book","proof:item":"a report"}
    })
    .to_string();
    json!({
        "message": {"role":"assistant","content":content},
        "prompt_eval_count": 20, "eval_count": 15
    })
}

fn linter_output() -> Value {
    let content = json!({"findings": []}).to_string();
    json!({
        "message": {"role":"assistant","content":content},
        "prompt_eval_count": 20, "eval_count": 12
    })
}

fn interviewer_output() -> Value {
    let content = json!({
        "next_question": "Which context should I record?",
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

fn briefer_output() -> Value {
    let content = json!({"response": null, "items": []}).to_string();
    json!({
        "message": {"role":"assistant","content":content},
        "prompt_eval_count": 20, "eval_count": 10
    })
}

/// One scripted upstream whose scripted replies are consumed in chat order.
/// Each structured role job performs exactly one chat, so a per-role server
/// answers with that role's output as its first (only) reply.
async fn role_server(role: &str) -> ModelServer {
    let reply = match role {
        "composer" => composer_output(),
        "linter" => linter_output(),
        "interviewer" => interviewer_output(),
        "briefer" => briefer_output(),
        _ => {
            json!({"message":{"role":"assistant","content":"Done."},"prompt_eval_count":12,"eval_count":8})
        }
    };
    ModelServer::start(vec![support::model_server::Reply::Native(reply)]).await
}

#[tokio::test]
async fn ar08_composer_and_linter_keep_approved_bytes_and_gate_deterministic() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let (opportunity, recipient, template) = seed_writing_scenario(&workspace).await;
    let provider = role_server("composer").await;
    install_profile(&workspace, &path, &provider).await;

    let fills = json!([
        {"slot":"their_name:name","value":"Dana"},
        {"slot":"gift:item","value":"a book","resource_id":"gift-1"},
        {"slot":"proof:item","value":"a report","resource_id":"proof-1"}
    ]);
    let (status, started) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/materials/compose"),
        Some(json!({
            "recipient_id": recipient,
            "kind":"outreach",
            "template_version_id": template,
            "exchange_version": 1,
            "slot_fills": fills,
            "selected_match_ids": []
        })),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 202, "{started:?}");
    let job_id = started["job_id"].as_str().unwrap().to_owned();
    let terminal = wait_job(port, &auth, &job_id, &["succeeded", "failed"]).await;
    assert_eq!(terminal["status"], "succeeded", "{terminal:?}");
    assert_eq!(terminal["role"], "composer");
    let result = terminal["result"].clone();
    assert_eq!(result["status"], "draft");
    assert_eq!(result["owner_review_required"], true);
    assert_eq!(result["released"], false);

    let recorded_at_terminal = provider.recorded().len();
    let draft_body = "dear Dana, i prepared a book for you. you can review a report.";
    let (status, saved) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/materials/compose/{job_id}/save"),
        Some(json!({"body": draft_body})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 201, "{saved:?}");
    let material_id = saved["id"].as_str().unwrap().to_owned();

    // Mutate the selected template after generation: save must be blocked and
    // the deterministic gate path must not consult the model.
    sqlx::query("UPDATE template_version SET status='flagged' WHERE id='template-1'")
        .execute(workspace.pool())
        .await
        .unwrap();
    let (status, stale) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/materials/compose/{job_id}/save"),
        Some(json!({"body": draft_body})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 409, "{stale:?}");

    sqlx::query("UPDATE template_version SET status='active' WHERE id='template-1'")
        .execute(workspace.pool())
        .await
        .unwrap();
    let (status, approved) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/materials/{material_id}/approve"),
        Some(json!({"override_reasons":{}})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 200, "{approved:?}");
    let (status, released) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/materials/{material_id}/release"),
        None,
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 200, "{released:?}");
    assert!(released["release_id"].as_str().is_some());
    let body_row: String = sqlx::query_scalar(
        "SELECT body FROM material_version WHERE material_id=? ORDER BY version_no DESC LIMIT 1",
    )
    .bind(&material_id)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(body_row, draft_body, "exact approved bytes released");

    // Deterministic gate, release and save path never touch the model.
    assert_eq!(provider.recorded().len(), recorded_at_terminal);

    // Linter runs as a packaged structured role with deterministic findings.
    let lint_provider = role_server("linter").await;
    install_profile(&workspace, &path, &lint_provider).await;
    let hash = crate_sha256(draft_body);
    let (status, linted) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/materials/{material_id}/lint"),
        Some(json!({"version":1,"content_sha256":hash})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 202, "{linted:?}");
    let lint_job = linted["job_id"].as_str().unwrap().to_owned();
    let lint_terminal = wait_job(port, &auth, &lint_job, &["succeeded", "failed"]).await;
    assert_eq!(lint_terminal["status"], "succeeded", "{lint_terminal:?}");
    assert_eq!(lint_terminal["role"], "linter");
    let advisory: Value = sqlx::query_scalar(
        "SELECT advisory_json FROM material_lint WHERE material_id=? AND version_no=1",
    )
    .bind(&material_id)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(advisory, json!([]));

    provider.stop().await;
    lint_provider.stop().await;
    cleanup(&path, workspace, service).await;
}

#[tokio::test]
async fn ar08_model_cannot_approve_release_or_send() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let (opportunity, recipient, template) = seed_writing_scenario(&workspace).await;

    // A schema-valid but host-invalid claim (approval authority) must be
    // rejected by the host validator; no draft, approval or release exists.
    let content = json!({
        "body": "Dear Dana, I approved a release for you.",
        "sentences": [
            {
                "text": "Dear Dana, I approved a release for you.",
                "claims": [{"type":"self_fact","ref":{"kind":"approval","id":"approval-1"}}],
                "rules": ["R1"]
            }
        ],
        "slot_fills": {"their_name:name":"Dana","gift:item":"a book","proof:item":"a report"}
    })
    .to_string();
    let provider = ModelServer::start(vec![support::model_server::Reply::Native(json!({
        "message": {"role":"assistant","content":content},
        "prompt_eval_count": 20, "eval_count": 15
    }))])
    .await;
    install_profile(&workspace, &path, &provider).await;

    let (status, started) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/materials/compose"),
        Some(json!({
            "recipient_id": recipient,
            "kind":"outreach",
            "template_version_id": template,
            "exchange_version": 1,
            "slot_fills": [
                {"slot":"their_name:name","value":"Dana"},
                {"slot":"gift:item","value":"a book","resource_id":"gift-1"},
                {"slot":"proof:item","value":"a report","resource_id":"proof-1"}
            ],
            "selected_match_ids": []
        })),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 202, "{started:?}");
    let job_id = started["job_id"].as_str().unwrap().to_owned();
    let terminal = wait_job(port, &auth, &job_id, &["invalid", "failed"]).await;
    let code = terminal["last_error_code"].as_str().unwrap_or_default();
    assert!(
        matches!(code, "OUTPUT_INVALID" | "RESULT_INVALID"),
        "model-driven approval must be rejected by validation: {terminal:?}"
    );

    let materials: i64 = sqlx::query_scalar("SELECT count(*) FROM material")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(
        materials, 0,
        "no draft may be created from an invalid proposal"
    );
    let approvals: i64 = sqlx::query_scalar("SELECT count(*) FROM approval")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(approvals, 0, "model output cannot approve");
    let releases: i64 = sqlx::query_scalar("SELECT count(*) FROM release_record")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(releases, 0, "model output cannot release or send");

    provider.stop().await;
    cleanup(&path, workspace, service).await;
}

#[tokio::test]
async fn ar08_all_roles_use_one_worker_boundary() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let (opportunity, recipient, template) = seed_writing_scenario(&workspace).await;
    let mut providers: Vec<ModelServer> = Vec::new();

    // Interviewer job (owner turn).
    let interviewer_provider = role_server("interviewer").await;
    install_profile(&workspace, &path, &interviewer_provider).await;
    let (status, chat) = request(
        port,
        &auth,
        "POST",
        "/api/v1/agent/chats",
        Some(json!({"role":"interviewer","scope_kind":"owner","asset_kind":"proof"})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 201, "{chat:?}");
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let (status, turn) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/agent/chats/{chat_id}/turns"),
        Some(json!({"text":"I ran a survey in June."})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 202, "{turn:?}");
    let interviewer_job = turn["job_id"].as_str().unwrap().to_owned();
    let interviewer_terminal =
        wait_job(port, &auth, &interviewer_job, &["succeeded", "failed"]).await;
    assert_eq!(
        interviewer_terminal["status"], "succeeded",
        "{interviewer_terminal:?}"
    );
    providers.push(interviewer_provider);

    // Briefer job (owner brief).
    let briefer_provider = role_server("briefer").await;
    install_profile(&workspace, &path, &briefer_provider).await;
    let (status, brief) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/briefs/party/{recipient}/items"),
        Some(json!({})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 202, "{brief:?}");
    let brief_job = brief["job_id"].as_str().unwrap().to_owned();
    let brief_terminal = wait_job(port, &auth, &brief_job, &["succeeded", "failed"]).await;
    assert_eq!(brief_terminal["status"], "succeeded", "{brief_terminal:?}");
    providers.push(briefer_provider);

    // Composer job.
    let compose_provider = role_server("composer").await;
    install_profile(&workspace, &path, &compose_provider).await;
    let (status, started) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/materials/compose"),
        Some(json!({
            "recipient_id": recipient,
            "kind":"outreach",
            "template_version_id": template,
            "exchange_version": 1,
            "slot_fills": [
                {"slot":"their_name:name","value":"Dana"},
                {"slot":"gift:item","value":"a book","resource_id":"gift-1"},
                {"slot":"proof:item","value":"a report","resource_id":"proof-1"}
            ],
            "selected_match_ids": []
        })),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 202, "{started:?}");
    let compose_job = started["job_id"].as_str().unwrap().to_owned();
    let compose_terminal = wait_job(port, &auth, &compose_job, &["succeeded", "failed"]).await;
    assert_eq!(
        compose_terminal["status"], "succeeded",
        "{compose_terminal:?}"
    );
    providers.push(compose_provider);

    // Linter job needs a saved material: save the composer candidate first.
    let draft_body = "dear Dana, i prepared a book for you. you can review a report.";
    let (status, saved) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/materials/compose/{compose_job}/save"),
        Some(json!({"body": draft_body})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 201, "{saved:?}");
    let material_id = saved["id"].as_str().unwrap().to_owned();
    let lint_provider = role_server("linter").await;
    install_profile(&workspace, &path, &lint_provider).await;
    let hash = crate_sha256(draft_body);
    let (status, linted) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/materials/{material_id}/lint"),
        Some(json!({"version":1,"content_sha256":hash})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 202, "{linted:?}");
    let lint_job = linted["job_id"].as_str().unwrap().to_owned();
    let lint_terminal = wait_job(port, &auth, &lint_job, &["succeeded", "failed"]).await;
    assert_eq!(lint_terminal["status"], "succeeded", "{lint_terminal:?}");
    providers.push(lint_provider);

    // Embedding job: a health probe for the qualified embed capability.
    let embed_provider = ModelServer::with_embed_responder(
        Vec::new(),
        None,
        support::model_server::marker_embed_responder(),
    )
    .await;
    install_profile(&workspace, &path, &embed_provider).await;
    let (status, probe) = request(
        port,
        &auth,
        "POST",
        "/api/v1/agent/provider/health",
        Some(json!({"mode":"embed"})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 202, "{probe:?}");
    let mut embed_probe: Option<(String, Option<String>)> = None;
    for _ in 0..100 {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT status,diagnostic FROM inference_probe ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(workspace.pool())
        .await
        .unwrap();
        if let Some(row) = row
            && matches!(row.0.as_str(), "succeeded" | "failed")
        {
            embed_probe = Some(row);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let embed_probe = embed_probe.expect("embed probe reached a terminal state");
    providers.push(embed_provider);

    // Every recorded upstream request must be the worker's allowed surface;
    // no direct client path, no owner credential ever reaches the provider.
    for provider in &providers {
        let recorded = provider.recorded();
        assert!(!recorded.is_empty());
        for raw in &recorded {
            let head = raw.split("\r\n\r\n").next().unwrap_or_default();
            let path = head.lines().next().unwrap_or_default();
            if path.starts_with("GET ") {
                assert!(
                    path.contains("/api/version") || path.contains("/api/tags"),
                    "unexpected GET upstream path: {path}"
                );
            } else {
                assert!(
                    path.contains("POST /v1/chat/completions")
                        || path.contains("POST /v1/embeddings"),
                    "unexpected POST upstream path: {path}"
                );
            }
            assert!(
                !raw.contains("X-Parley-Token"),
                "credential leaked upstream"
            );
            assert!(!raw.contains("Cookie:"), "cookie leaked upstream");
            assert!(!raw.contains("/api/chat"), "legacy chat path used");
        }
    }
    match embed_probe.0.as_str() {
        "succeeded" => {}
        "failed" => {
            let diagnostic = embed_probe.1.unwrap_or_default();
            assert!(
                matches!(
                    diagnostic.as_str(),
                    "PROVIDER_FAILED"
                        | "MODEL_UNAVAILABLE"
                        | "WORKER_PROTOCOL"
                        | "PROFILE_CHANGED"
                        | "RESULT_INVALID"
                ),
                "embed probe failed with a typed diagnostic: {diagnostic}"
            );
        }
        other => panic!("unexpected embed probe state {other}"),
    }

    // Upstream refusal must fail closed with a typed code and not fall back to
    // an alternate transport.
    let dead_provider = role_server("interviewer").await;
    install_profile(&workspace, &path, &dead_provider).await;
    dead_provider.stop().await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let (status, turn2) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/agent/chats/{chat_id}/turns"),
        Some(json!({"text":"A later question."})),
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 202, "{turn2:?}");
    let late_job = turn2["job_id"].as_str().unwrap().to_owned();
    let late_terminal = wait_job(port, &auth, &late_job, &["failed", "invalid"]).await;
    let code = late_terminal["last_error_code"]
        .as_str()
        .unwrap_or_default();
    assert!(
        matches!(
            code,
            "PROVIDER_FAILED"
                | "MODEL_UNAVAILABLE"
                | "WORKER_PROTOCOL"
                | "PROFILE_CHANGED"
                | "ISOLATION_UNAVAILABLE"
                | "RUNNER_INSTALL_INVALID"
        ),
        "expected a closed inference failure, got {late_terminal:?}"
    );

    for provider in providers {
        provider.stop().await;
    }
    cleanup(&path, workspace, service).await;
}

fn crate_sha256(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(text.as_bytes()))
}
