use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

type OwnerSession = (String, String, String);
static NEXT: AtomicU64 = AtomicU64::new(0);
static SESSIONS: OnceLock<Mutex<std::collections::HashMap<u16, OwnerSession>>> = OnceLock::new();
fn db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-s09-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn request(
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

async fn owner_session(service: &RunningService) -> OwnerSession {
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

#[tokio::test]
async fn s09_template_family_version_receipts_and_voice_retirement_are_durable() {
    let (path, workspace, port, service) = setup().await;
    let (status, family) = request(
        port,
        "POST",
        "/api/v1/template-families",
        Some(json!({"name":"Review"})),
        "s09-family",
    )
    .await;
    assert_eq!(status, 201, "{family:?}");
    let (again_status, again) = request(
        port,
        "POST",
        "/api/v1/template-families",
        Some(json!({"name":"Review"})),
        "s09-family",
    )
    .await;
    assert_eq!(again_status, 201);
    assert_eq!(family["id"], again["id"]);
    let family_id = family["id"].as_str().unwrap();
    let (status, version) = request(port, "POST", &format!("/api/v1/templates/{family_id}/versions"), Some(json!({
        "name":"Simple", "body":"Hello {{their_name:name}}.", "stage_class":"identified", "channel":"email", "sentence_rules":[[]]
    })), "s09-version").await;
    assert_eq!(status, 201, "{version:?}");
    let version_id = version["id"].as_str().unwrap();
    let before: (String, String) =
        sqlx::query_as("SELECT body,content_sha256 FROM template_version WHERE id=?")
            .bind(version_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(before.1.len(), 64);
    assert!(
        sqlx::query("UPDATE template_version SET body='changed' WHERE id=?")
            .bind(version_id)
            .execute(workspace.pool())
            .await
            .is_err()
    );

    let (status, sample) = request(
        port,
        "POST",
        "/api/v1/voice-samples",
        Some(json!({"text":"A short owner voice example.","channel":"email"})),
        "s09-voice",
    )
    .await;
    assert_eq!(status, 201, "{sample:?}");
    let id = sample["id"].as_str().unwrap();
    let (status, retired) = request(
        port,
        "POST",
        &format!("/api/v1/voice-samples/{id}/retire"),
        None,
        "s09-retire",
    )
    .await;
    assert_eq!(status, 200, "{retired:?}");
    assert_eq!(retired["status"], "retired");
    let status: String = sqlx::query_scalar("SELECT status FROM voice_sample WHERE id=?")
        .bind(id)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(status, "retired");

    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(&path);
}

#[tokio::test]
async fn s09_arbitrary_client_scores_are_rejected_as_calibration_evidence() {
    let (path, workspace, port, service) = setup().await;
    let payload = json!({"asset_kind":"gift","fixture_id":"s09-gift-fixture","samples":[{"score":0.9,"relevant":true},{"score":0.8,"relevant":true},{"score":0.2,"relevant":false},{"score":0.1,"relevant":false}]});
    let (status, calibration) = request(
        port,
        "POST",
        "/api/v1/matching/calibrations",
        Some(payload.clone()),
        "s09-calibration",
    )
    .await;
    assert_eq!(status, 403, "{calibration:?}");
    assert_eq!(calibration["error"]["code"], "CALIBRATION_REQUIRES_EVAL");
    let (again_status, _again) = request(
        port,
        "POST",
        "/api/v1/matching/calibrations",
        Some(payload),
        "s09-calibration",
    )
    .await;
    assert_eq!(again_status, 403);
    let (conflict_status, _) = request(port, "POST", "/api/v1/matching/calibrations", Some(json!({"asset_kind":"gift","fixture_id":"different","samples":[{"score":0.9,"relevant":true},{"score":0.1,"relevant":false}]})), "s09-calibration").await;
    assert_eq!(conflict_status, 403);
    let receipts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM idempotency_receipt WHERE key='s09-calibration'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(receipts, 0);
    let trusted_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM matching_calibration WHERE trusted=1")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(trusted_rows, 0);
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s09_stale_composer_candidate_cannot_install_a_material_version() {
    let (path, workspace, port, service) = setup().await;
    let now = "2026-11-21T00:00:00Z";
    sqlx::query("INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES('recipient-1','person','Dana',2,?,?)")
        .bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO party_policy(party_id) VALUES('recipient-1')")
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES('opportunity-1','Follow up','open',2,?,?)")
        .bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES('opportunity-1','recipient-1','primary',?)")
        .bind(now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO template_family(id,name,created_at) VALUES('family-1','Family',?)")
        .bind(now)
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO template_version(id,family_id,version_no,body,fallbacks_json,status,content_sha256,created_at) VALUES('template-1','family-1',1,'Hello.','{}','active','template-hash',?)")
        .bind(now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO exchange_version(opportunity_id,recipient_id,version_no,they_get,i_get,fairness_attested,attestation_digest,attested_at,created_at) VALUES('opportunity-1','recipient-1',1,'A checklist','Feedback',1,'attested-hash',?,?)")
        .bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    let input = json!({"opportunity_id":"opportunity-1","opportunity_revision":1,"recipient_id":"recipient-1","recipient_revision":1,"template_version_id":"template-1","exchange_version":1,"kind":"outreach","slot_fills":[]}).to_string();
    sqlx::query("INSERT INTO durable_job(id,role,status,input_json,input_sha256,source_occurrence_id,source_variant_id,scope_kind,scope_id,scope_revision,current_attempt,max_attempts,progress_current,progress_total,created_at,updated_at,completed_at) VALUES('composer-job','composer','succeeded',?,'input-hash',NULL,NULL,'opportunity','opportunity-1',1,1,3,1,1,?,?,?)")
        .bind(&input).bind(now).bind(now).bind(now).execute(workspace.pool()).await.unwrap();
    sqlx::query("INSERT INTO job_attempt(job_id,attempt_no,status,model,model_digest,provider_snapshot_json,prompt_id,prompt_sha256,input_sha256,input_json,output_json,validation_errors_json,tokens_in,tokens_out,started_at,finished_at,duration_ms) VALUES('composer-job',1,'succeeded','local',NULL,'{}','composer@1','prompt-sha','input-hash',?,?, '[]',1,1,?,?,1)")
        .bind(&input).bind(json!({"body":"Hello.","sentences":[]}).to_string()).bind(now).bind(now).execute(workspace.pool()).await.unwrap();

    let (status, response) = request(
        port,
        "POST",
        "/api/v1/opportunities/opportunity-1/materials/compose/composer-job/save",
        Some(json!({"body":"Hello."})),
        "s09-stale-save",
    )
    .await;
    assert_eq!(status, 409, "{response:?}");
    assert_eq!(response["error"]["code"], "COMPOSER_INPUT_STALE");
    let material_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM material WHERE opportunity_id='opportunity-1'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(material_count, 0);
    let version_count: i64 = sqlx::query_scalar("SELECT count(*) FROM material_version")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(version_count, 0);
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s09_source_correction_after_mark_sent_preserves_sent_bytes() {
    let (path, workspace, port, service) = setup().await;

    // Source-backed graph entity (tier 1 claim).
    let (status, org) = request(
        port,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"org","name":"Northline"})),
        "s09-org",
    )
    .await;
    assert_eq!(status, 201);
    let (status, source) = request(
        port,
        "POST",
        "/api/v1/captures",
        Some(json!({"text":"Dana Lee will work with Northline.","title":"Team update"})),
        "s09-capture",
    )
    .await;
    assert_eq!(status, 201);
    let (status, relation) = request(
        port,
        "POST",
        "/api/v1/graph/related",
        Some(json!({
            "kind":"person","name":"Dana Lee",
            "related_entity_id": org["id"],"verb":"member_of",
            "occurrence_id": source["occurrence_id"],
            "quote":"Dana Lee will work with Northline.",
            "view_scope":"opportunity","view_id":"pending","x":120.0,"y":90.0
        })),
        "s09-relation",
    )
    .await;
    assert_eq!(status, 201, "{relation}");
    let recipient = relation["id"].as_str().unwrap().to_owned();
    let claim_id: String = sqlx::query_scalar("SELECT id FROM claim WHERE subject_id=?")
        .bind(&recipient)
        .fetch_one(workspace.pool())
        .await
        .unwrap();

    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":"Northline onboarding","primary_party_id":recipient})),
        "s09-opp",
    )
    .await;
    assert_eq!(status, 201);
    let opportunity_id = opportunity["id"].as_str().unwrap().to_owned();

    let (status, them) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity_id}/interests"),
        Some(json!({"recipient_id":recipient,"side":"them","statement":"A clearer onboarding handoff","knowledge_kind":"hypothesis","check_suggestion":"Ask the owner whether this is current."})),
        "s09-interest-them",
    )
    .await;
    assert_eq!(status, 201);
    let (status, me) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity_id}/interests"),
        Some(json!({"recipient_id":recipient,"side":"me","statement":"Offer a usable checklist","knowledge_kind":"owner_report"})),
        "s09-interest-me",
    )
    .await;
    assert_eq!(status, 201);
    let (status, exchange) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity_id}/exchange"),
        Some(json!({"recipient_id":recipient,"they_get":"A practical onboarding checklist","i_get":"A better understanding of the handoff","interest_ids":[them["id"],me["id"]]})),
        "s09-exchange",
    )
    .await;
    assert_eq!(status, 201);
    let exchange_version = exchange["version"].as_i64().unwrap();
    let (status, _) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity_id}/exchange/{recipient}/attest"),
        None,
        "s09-attest",
    )
    .await;
    assert_eq!(status, 200);

    let (status, template_row) = request(
        port,
        "POST",
        "/api/v1/templates",
        Some(json!({"name":"Research note","body":"Hello."})),
        "s09-template",
    )
    .await;
    assert_eq!(status, 201);
    let template_id = template_row["id"].as_str().unwrap().to_owned();
    assert_eq!(
        request(
            port,
            "POST",
            &format!("/api/v1/templates/{template_id}/approve"),
            None,
            "s09-tpl-approve"
        )
        .await
        .0,
        204
    );
    assert_eq!(
        request(
            port,
            "POST",
            &format!("/api/v1/templates/{template_id}/activate"),
            None,
            "s09-tpl-activate"
        )
        .await
        .0,
        204
    );

    let sentence = "Dana Lee will work with Northline.";
    let (status, material) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity_id}/materials"),
        Some(json!({
            "recipient_id": recipient,
            "kind":"outreach",
            "template_version_id": template_id,
            "body": sentence,
            "exchange_version": exchange_version,
            "slot_fills": [],
            "sentence_bindings": [{"sentence_index":0,"claim_type":"other_fact","ref_kind":"claim","ref_id":claim_id,"ref_revision":1,"approved_wording":sentence}]
        })),
        "s09-material",
    )
    .await;
    assert_eq!(status, 201, "{material}");
    let material_id = material["id"].as_str().unwrap().to_owned();
    let (status, approval) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/approve"),
        Some(json!({"override_reasons":{"S1 GIVE_FIRST":"This is a reviewed factual update, not a gift message.","S2 ANCHOR":"The approved claim is the anchor for this material."}})),
        "s09-approve",
    )
    .await;
    assert_eq!(status, 200, "{approval}");
    let (status, released) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/release"),
        None,
        "s09-release",
    )
    .await;
    assert_eq!(status, 200, "{released}");
    let release_id = released["release_id"].as_str().unwrap().to_owned();
    let sent_sha = released["content_sha256"].as_str().unwrap().to_owned();

    // Mark sent through the S07 thread flow: the immutable send snapshot now
    // exists and carries the exact released bytes.
    let (status, send) = request(
        port,
        "POST",
        "/api/v1/threads/t-s09/send",
        Some(json!({
            "opportunity_id": opportunity_id,
            "recipient_id": recipient,
            "primary": {"material_id": material_id, "version": 1, "release_id": release_id},
            "companions": []
        })),
        "s09-send",
    )
    .await;
    assert_eq!(status, 201, "{send}");
    let send_id = send["id"].as_str().unwrap().to_owned();
    assert_eq!(send["content_sha256"].as_str().unwrap(), sent_sha);

    let snapshot: (String, String, i64, String, String, String) = sqlx::query_as(
        "SELECT s.content_sha256, s.primary_material_id, s.primary_version_no, s.recipient_id, s.response_deadline, r.content_sha256 FROM send s JOIN send_part sp ON sp.send_id = s.id AND sp.kind='primary' JOIN release_record r ON r.id = sp.release_id"
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    let _ = &workspace;

    // Correct the source claim in a later transaction.
    let mut correction = workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    sqlx::query("INSERT INTO claim_revision(claim_id,revision,wording,knowledge_kind,tier,speaker_kind,confirmed_by_owner,created_at) VALUES(?,2,'Dana Lee is no longer associated with Northline.','owner_report',3,'owner',1,'2026-11-21T12:00:00Z')")
        .bind(&claim_id)
        .execute(&mut *correction)
        .await
        .unwrap();
    sqlx::query("UPDATE claim SET current_revision=2 WHERE id=?")
        .bind(&claim_id)
        .execute(&mut *correction)
        .await
        .unwrap();
    correction.commit().await.unwrap();

    // The sent snapshot is immutable: same bytes, same recipient, same
    // version, same deadline; the release still carries the original digest.
    let after: (String, String, i64, String, String, String) = sqlx::query_as(
        "SELECT s.content_sha256, s.primary_material_id, s.primary_version_no, s.recipient_id, s.response_deadline, r.content_sha256 FROM send s JOIN send_part sp ON sp.send_id = s.id AND sp.kind='primary' JOIN release_record r ON r.id = sp.release_id"
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(after, snapshot);
    let material_body: String = sqlx::query_scalar(
        "SELECT body FROM material_version WHERE material_id=? AND version_no=1",
    )
    .bind(&material_id)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(material_body, sentence);
    let thread_state: String =
        sqlx::query_scalar("SELECT state FROM contact_thread WHERE id='t-s09'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(thread_state, "waiting");
    let send_events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event WHERE event_type='send.marked_sent'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(send_events, 1);
    let timeout_events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event WHERE event_type='thread.timed_out'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(timeout_events, 0);

    // A later release of the same version is refused because the corrected
    // source invalidates current eligibility; historical bytes stay intact.
    let (status, blocked) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/release"),
        None,
        "s09-release-after-correction",
    )
    .await;
    assert_eq!(status, 422, "{blocked}");
    let message = blocked["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("dependencies changed") || message.contains("corrected"),
        "{message}"
    );
    let release_count: i64 = sqlx::query_scalar("SELECT count(*) FROM release_record")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(release_count, 1);
    let _ = send_id;

    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s09_interests_read_includes_linked_claim_revision() {
    let (path, workspace, port, service) = setup().await;

    // Source-backed graph entity (tier 1 claim), mirroring the correction test above.
    let (status, org) = request(
        port,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"org","name":"Northline"})),
        "s09-interests-org",
    )
    .await;
    assert_eq!(status, 201);
    let (status, source) = request(
        port,
        "POST",
        "/api/v1/captures",
        Some(json!({"text":"Dana Lee will work with Northline.","title":"Team update"})),
        "s09-interests-capture",
    )
    .await;
    assert_eq!(status, 201, "{source}");
    let (status, relation) = request(
        port,
        "POST",
        "/api/v1/graph/related",
        Some(json!({
            "kind":"person","name":"Dana Lee",
            "related_entity_id": org["id"],"verb":"member_of",
            "occurrence_id": source["occurrence_id"],
            "quote":"Dana Lee will work with Northline.",
            "view_scope":"opportunity","view_id":"pending","x":120.0,"y":90.0
        })),
        "s09-interests-relation",
    )
    .await;
    assert_eq!(status, 201, "{relation}");
    let recipient = relation["id"].as_str().unwrap().to_owned();
    let claim_id: String = sqlx::query_scalar("SELECT id FROM claim WHERE subject_id=?")
        .bind(&recipient)
        .fetch_one(workspace.pool())
        .await
        .unwrap();

    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":"Northline onboarding","primary_party_id":recipient})),
        "s09-interests-opp",
    )
    .await;
    assert_eq!(status, 201);
    let opportunity_id = opportunity["id"].as_str().unwrap().to_owned();

    // A source-backed interest links a claim revision; the read returns its
    // wording, tier and speaker so the inspector can show the passage.
    let (status, backed) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity_id}/interests"),
        Some(json!({"recipient_id":recipient,"side":"them","statement":"A clearer onboarding handoff","knowledge_kind":"observation","claim_id":claim_id,"claim_revision":1})),
        "s09-interests-backed",
    )
    .await;
    assert_eq!(status, 201, "{backed}");
    let (status, owner_only) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity_id}/interests"),
        Some(json!({"recipient_id":recipient,"side":"me","statement":"Offer a usable checklist","knowledge_kind":"owner_report"})),
        "s09-interests-owner",
    )
    .await;
    assert_eq!(status, 201);

    let (status, read) = request(
        port,
        "GET",
        &format!("/api/v1/opportunities/{opportunity_id}/interests?recipient_id={recipient}"),
        None,
        "s09-interests-read",
    )
    .await;
    assert_eq!(status, 200, "{read}");
    let items = read["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let backed_item = items
        .iter()
        .find(|item| item["id"] == backed["id"])
        .unwrap();
    assert_eq!(backed_item["claim"]["claim_id"].as_str().unwrap(), claim_id);
    assert_eq!(backed_item["claim"]["revision"].as_i64().unwrap(), 1);
    assert_eq!(
        backed_item["claim"]["wording"].as_str().unwrap(),
        "Dana Lee will work with Northline."
    );
    assert_eq!(backed_item["claim"]["tier"].as_i64().unwrap(), 1);
    assert!(
        !backed_item["claim"]["speaker_kind"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    let owner_item = items
        .iter()
        .find(|item| item["id"] == owner_only["id"])
        .unwrap();
    assert!(owner_item["claim"].is_null(), "{owner_item}");

    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
}
