//! AR-001 AR03: contextual Template Builder candidate saves produce exactly
//! one immutable draft, revalidate the frozen dependency vector and never let
//! a rejected candidate or party-specific text enter the reusable library.

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

static NEXT: AtomicU64 = AtomicU64::new(0);

async fn setup() -> (PathBuf, Workspace, RunningService, String, String, String) {
    let path = std::env::temp_dir().join(format!(
        "parley-ar03-{}-{}.sqlite",
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

fn hex_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Seed a succeeded builder job whose frozen dependency vector is the empty
/// canonical vector. The candidate save re-derives the same digest.
async fn seed_job(
    workspace: &Workspace,
    job_id: &str,
    context_projection: Value,
    input_extra: Value,
    epoch: &str,
) {
    let input = json!({
        "dependency_json": [],
        "context_projection": context_projection,
        "prompt_version": "builder-v1",
    });
    let mut merged = input.as_object().unwrap().clone();
    if let Some(extra) = input_extra.as_object() {
        for (key, value) in extra {
            merged.insert(key.clone(), value.clone());
        }
    }
    let input = Value::Object(merged).to_string();
    sqlx::query("INSERT INTO durable_job(id,role,status,input_json,input_sha256,scope_kind,scope_id,scope_revision,max_attempts,epoch,created_at,updated_at,completed_at) VALUES(?,?,'succeeded',?,?,'owner','build',1,3,?,'2026-11-20T00:00:00Z','2026-11-20T00:00:00Z','2026-11-20T00:00:00Z')")
        .bind(job_id)
        .bind("template_builder")
        .bind(&input)
        .bind(hex_digest(&input))
        .bind(epoch)
        .execute(workspace.pool())
        .await
        .unwrap();
}

async fn seed_family(workspace: &Workspace) -> (String, String) {
    sqlx::query("INSERT INTO template_family(id,name,created_at) VALUES('fam-ar03','AR03 Family','2026-11-20T00:00:00Z')")
        .execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO template_version(id,family_id,version_no,body,fallbacks_json,status,content_sha256,created_at,rules_json,display_name,stage_class,channel,parent_version_id) VALUES('ver-head','fam-ar03',1,'Head body','{}','approved','hash','2026-11-20T00:00:00Z','[]','Head','prepared','email',NULL)")
        .execute(workspace.pool()).await.unwrap();
    ("fam-ar03".into(), "ver-head".into())
}

#[allow(clippy::too_many_arguments)]
async fn seed_candidate(
    workspace: &Workspace,
    candidate_id: &str,
    job_id: &str,
    family_head: &str,
    body: &str,
    sentences: Value,
    requested_rules: Value,
    variant_no: i64,
) {
    let empty_digest = hex_digest("[]");
    sqlx::query("INSERT INTO template_build_candidate(id,job_id,variant_no,name,body,sentences_json,fallbacks_json,notes,requested_rules_json,optional_rules_json,dependency_json,dependency_digest,preview_json,status,revision,saved_version_id,family_head_id,expires_at,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
        .bind(candidate_id)
        .bind(job_id)
        .bind(variant_no)
        .bind("Draft")
        .bind(body)
        .bind(sentences.to_string())
        .bind("{}")
        .bind("")
        .bind(requested_rules.to_string())
        .bind("[]")
        .bind("[]")
        .bind(empty_digest)
        .bind("{}")
        .bind("owner_review_required")
        .bind(1_i64)
        .bind(Option::<String>::None)
        .bind(family_head)
        .bind("2026-12-20T00:00:00Z")
        .bind("2026-11-20T00:00:00Z")
        .bind("2026-11-20T00:00:00Z")
        .execute(workspace.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn ar03_candidate_save_is_one_version_and_fences_changes() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let auth = (&cookie, &token, &epoch);
    let (family_id, head) = seed_family(&workspace).await;
    seed_job(
        &workspace,
        "job-ar03-save",
        Value::Null,
        json!({}),
        "epoch-ar03",
    )
    .await;
    let body = "I can share {{gift:guide}}. Would a look help {{ask:question}}?";
    let sentences = json!([{"rules":["R1"]},{"rules":["R2"]}]);
    seed_candidate(
        &workspace,
        "cand-ar03-save",
        "job-ar03-save",
        &head,
        body,
        sentences.clone(),
        json!(["R1", "R2"]),
        1,
    )
    .await;

    let (status, saved) = request(
        service.address.port(),
        auth,
        "POST",
        &format!("/api/v1/templates/{family_id}/versions"),
        Some(json!({
            "candidate_id":"cand-ar03-save",
            "expected_candidate_revision":1,
            "expected_family_head":head
        })),
        "ar03-save-key-1",
    )
    .await;
    assert_eq!(status, 201, "{saved:?}");
    let version_id = saved["id"].as_str().unwrap().to_owned();

    let versions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM template_version WHERE family_id=?")
            .bind(&family_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(versions, 2, "one immutable draft");

    let candidate: (String, Option<String>, i64) = sqlx::query_as(
        "SELECT status,saved_version_id,revision FROM template_build_candidate WHERE id='cand-ar03-save'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(candidate.0, "saved");
    assert_eq!(candidate.1.as_deref(), Some(version_id.as_str()));
    assert_eq!(candidate.2, 2);

    let saved_body: String = sqlx::query_scalar("SELECT body FROM template_version WHERE id=?")
        .bind(&version_id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(
        saved_body, body,
        "preview/party values are never stored in the skeleton"
    );

    // Replaying the same intent returns the same receipt; no second draft.
    let (status, replay) = request(
        service.address.port(),
        auth,
        "POST",
        &format!("/api/v1/templates/{family_id}/versions"),
        Some(json!({
            "candidate_id":"cand-ar03-save",
            "expected_candidate_revision":1,
            "expected_family_head":head
        })),
        "ar03-save-key-1",
    )
    .await;
    assert_eq!(status, 201, "{replay:?}");
    assert_eq!(replay["id"].as_str().unwrap(), version_id);
    let versions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM template_version WHERE family_id=?")
            .bind(&family_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(versions, 2);

    // A stale candidate revision is refused even with a fresh intent.
    let (status, stale) = request(
        service.address.port(),
        auth,
        "POST",
        &format!("/api/v1/templates/{family_id}/versions"),
        Some(json!({
            "candidate_id":"cand-ar03-save",
            "expected_candidate_revision":1,
            "expected_family_head":head
        })),
        "ar03-save-key-2",
    )
    .await;
    assert_eq!(status, 409, "{stale:?}");
    assert_eq!(stale["error"]["code"], "TEMPLATE_CANDIDATE_CLOSED");

    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn ar03_candidate_save_rejects_party_specific_text() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let auth = (&cookie, &token, &epoch);
    let (family_id, head) = seed_family(&workspace).await;
    let party = workspace
        .create_entity("ar03-party", "person", "Casey Canary", "")
        .await
        .unwrap();
    let context = json!({
        "scope":{"kind":"party","id":party.id,"revision":party.revision},
        "segment":null,
        "evidence":[],
        "resources":[],
        "voice_samples":[],
        "recipient_name":"Casey Canary"
    });
    seed_job(
        &workspace,
        "job-ar03-canary",
        context.clone(),
        json!({"scope":context["scope"]}),
        "epoch-ar03",
    )
    .await;
    let body = "Hi Casey Canary, see {{gift:guide}}.";
    let sentences = json!([{"rules":["R1"]}]);
    seed_candidate(
        &workspace,
        "cand-ar03-canary",
        "job-ar03-canary",
        &head,
        body,
        sentences,
        json!(["R1"]),
        1,
    )
    .await;

    let (status, saved) = request(
        service.address.port(),
        auth,
        "POST",
        &format!("/api/v1/templates/{family_id}/versions"),
        Some(json!({
            "candidate_id":"cand-ar03-canary",
            "expected_candidate_revision":1,
            "expected_family_head":head
        })),
        "ar03-canary-key",
    )
    .await;
    assert_eq!(status, 422, "{saved:?}");
    assert_eq!(saved["error"]["code"], "TEMPLATE_VALIDATION_FAILED");

    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn ar03_contextual_build_admission_fails_closed_without_profile() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let auth = (&cookie, &token, &epoch);
    let party = workspace
        .create_entity("ar03-admit-party", "person", "Admit Example", "")
        .await
        .unwrap();
    let (status, response) = request(
        service.address.port(),
        auth,
        "POST",
        "/api/v1/templates/build",
        Some(json!({
            "context":{
                "scope":{"kind":"party","id":party.id,"revision":party.revision}
            },
            "stage_class":"prepared",
            "rungs":[1],
            "rules":["R1"],
            "optional_rules":[],
            "channel":"email",
            "max_chars":1200,
            "voice_sample_ids":[]
        })),
        "ar03-build-key",
    )
    .await;
    assert_eq!(status, 503, "{response:?}");
    assert_eq!(response["error"]["code"], "CAPABILITY_UNQUALIFIED");

    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

/// GET /templates/build-candidates/{job_id} is the durable re-read surface for
/// a completed builder job: the owner reloads candidates after navigation even
/// when the job result has left the activity window. The listing is fenced to
/// the current epoch and the builder role; anything else is the same 404 as a
/// missing job (UCR-001 Q4 equal hiding).
#[tokio::test]
async fn ar03_candidates_listing_reports_job_and_variant_states() {
    let (path, workspace, service, cookie, token, epoch) = setup().await;
    let auth = (&cookie, &token, &epoch);
    // An empty epoch lets the admission trigger bind the live workspace epoch
    // and create the inference_record lineage exactly as a real admission
    // would; the listing is fenced to that epoch.
    seed_job(&workspace, "job-ar03-list", json!({}), json!({}), "").await;
    let (_family, head) = seed_family(&workspace).await;
    seed_candidate(
        &workspace,
        "cand-ar03-a",
        "job-ar03-list",
        &head,
        "Hello {{their_name}}",
        json!([{"text":"Hello","rules":["R1"]}]),
        json!(["R1"]),
        1,
    )
    .await;
    seed_candidate(
        &workspace,
        "cand-ar03-b",
        "job-ar03-list",
        &head,
        "Second skeleton body",
        json!([{"text":"Second skeleton body","rules":["R2"]}]),
        json!(["R1"]),
        2,
    )
    .await;
    sqlx::query("UPDATE template_build_candidate SET variant_no=2, revision=revision+1 WHERE id='cand-ar03-b'")
        .execute(workspace.pool())
        .await
        .unwrap();
    // One saved candidate exercises the saved branch of the review union; the
    // saved version is a real immutable draft in the same family.
    sqlx::query("INSERT INTO template_version(id,family_id,version_no,body,fallbacks_json,status,content_sha256,created_at,rules_json,display_name,stage_class,channel,parent_version_id) VALUES('ver-ar03-1','fam-ar03',2,'Saved candidate body','{}','draft','hash2','2026-11-20T00:00:00Z','[]','Candidate draft','prepared','email','ver-head')")
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE template_build_candidate SET status='saved', saved_version_id='ver-ar03-1', revision=revision+1 WHERE id='cand-ar03-a'")
        .execute(workspace.pool())
        .await
        .unwrap();

    let (status, listing) = request(
        service.address.port(),
        auth,
        "GET",
        "/api/v1/templates/build-candidates/job-ar03-list",
        None,
        "ar03-list-1",
    )
    .await;
    assert_eq!(status, 200, "{listing:?}");
    assert_eq!(listing["job_id"], "job-ar03-list");
    assert_eq!(listing["role"], "template_builder");
    assert_eq!(listing["status"], "succeeded");
    let items = listing["items"].as_array().expect("candidate items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["variant_no"], 1);
    assert_eq!(items[0]["status"], "saved");
    assert_eq!(items[0]["saved_version_id"], "ver-ar03-1");
    assert_eq!(items[1]["variant_no"], 2);
    assert_eq!(items[1]["status"], "owner_review_required");
    assert!(items[1]["saved_version_id"].is_null());

    // An unknown job and the equal-hiding guarantee: same body shape.
    let (status, missing) = request(
        service.address.port(),
        auth,
        "GET",
        "/api/v1/templates/build-candidates/job-ar03-none",
        None,
        "ar03-list-2",
    )
    .await;
    assert_eq!(status, 404, "{missing:?}");
    assert_eq!(missing["error"]["code"], "JOB_NOT_FOUND");

    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}
