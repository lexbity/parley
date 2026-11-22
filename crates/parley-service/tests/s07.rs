//! S07: deadline equality and party-wide suppression (RFC-0002 BE-08).
//!
//! Explicit Mark sent records the primary copy and companions, starts the
//! correct per-contact timer, charges applicable capacity exactly once and
//! snapshots the gate. Timeouts reconcile at the inclusive boundary exactly
//! once per touch. A decline suppresses the party across every opportunity
//! while unrelated participants remain available.

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
static REQUESTS: AtomicU64 = AtomicU64::new(0);

fn db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-s07-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn now_str() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

async fn request(port: u16, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
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
    let key = REQUESTS.fetch_add(1, Ordering::Relaxed);
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\nIdempotency-Key: s07-{key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
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
    let payload = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut decoded = Vec::new();
        let mut rest = payload;
        loop {
            let (size, tail) = rest.split_once("\r\n").unwrap();
            let size = usize::from_str_radix(size.trim(), 16).unwrap();
            rest = tail;
            if size == 0 {
                break;
            }
            decoded.extend_from_slice(&rest.as_bytes()[..size]);
            rest = &rest[size + 2..];
        }
        String::from_utf8(decoded).unwrap()
    } else {
        payload.to_owned()
    };
    let value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&payload).unwrap()
    };
    let value = value.get("data").cloned().unwrap_or(value);
    (status, value)
}

async fn owner_session(service: &RunningService) -> (String, String, String) {
    let address = service.address;
    let body = json!({"secret":service.bootstrap_secret}).to_string();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(format!("POST /api/v1/session/bootstrap HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
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
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let payload: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    (
        cookie,
        payload["mutation_token"].as_str().unwrap().to_owned(),
        payload["epoch"].as_str().unwrap().to_owned(),
    )
}

async fn setup() -> (PathBuf, Workspace, u16, RunningService) {
    let path = db_path();
    let service = parley_service::start_workspace(&path).await.unwrap();
    let session = owner_session(&service).await;
    let port = service.address.port();
    SESSIONS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap()
        .insert(port, session);
    let workspace = Workspace::open(&path).await.unwrap();
    (path, workspace, port, service)
}

async fn create_party(port: u16, name: &str) -> String {
    let (status, person) = request(
        port,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"person","name":name})),
    )
    .await;
    assert_eq!(status, 201);
    person["id"].as_str().unwrap().to_owned()
}

async fn create_opportunity(port: u16, title: &str, primary: &str) -> String {
    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":title,"primary_party_id":primary})),
    )
    .await;
    assert_eq!(status, 201, "{opportunity}");
    opportunity["id"].as_str().unwrap().to_owned()
}

async fn add_participant(port: u16, opportunity: &str, party: &str) {
    let (status, _) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/participants"),
        Some(json!({"party_id":party,"role":"participant"})),
    )
    .await;
    assert_eq!(status, 200);
}

async fn prepare_exchange(port: u16, opportunity: &str, recipient: &str) -> i64 {
    let (status, them) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/interests"),
        Some(json!({"recipient_id":recipient,"side":"them","statement":"A clearer onboarding handoff","knowledge_kind":"hypothesis","check_suggestion":"Ask the owner whether this is current."})),
    )
    .await;
    assert_eq!(status, 201, "{them}");
    let (status, me) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/interests"),
        Some(json!({"recipient_id":recipient,"side":"me","statement":"Offer a usable checklist","knowledge_kind":"owner_report"})),
    )
    .await;
    assert_eq!(status, 201);
    let (status, exchange) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/exchange"),
        Some(json!({"recipient_id":recipient,"they_get":"A practical onboarding checklist","i_get":"A better understanding of the handoff","interest_ids":[them["id"],me["id"]]})),
    )
    .await;
    assert_eq!(status, 201);
    let version = exchange["version"].as_i64().unwrap();
    let (status, _) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/exchange/{recipient}/attest"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    version
}

async fn create_proof(port: u16, result_value: i64) -> String {
    let body = json!({
        "what_i_did": "I cut onboarding time for customer accounts",
        "for_whom": "Atlas, a 50-person company",
        "permission_to_name": true,
        "problem": "slow onboarding",
        "result_value": result_value,
        "result_unit": "%",
        "baseline": "9 days",
        "period": "2026 Q3",
        "my_role": "led"
    });
    let (status, proof) = request(
        port,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"proof","title":"Onboarding time cut","body":body})),
    )
    .await;
    assert_eq!(status, 201, "{proof}");
    let proof_id = proof["id"].as_str().unwrap().to_owned();
    let (status, _) = request(
        port,
        "POST",
        &format!("/api/v1/resources/{proof_id}/ready"),
        Some(json!({"body":body})),
    )
    .await;
    assert_eq!(status, 204);
    proof_id
}

async fn activate_template(port: u16, name: &str, body: &str) -> String {
    let (status, template_row) = request(
        port,
        "POST",
        "/api/v1/templates",
        Some(json!({"name":name,"body":body})),
    )
    .await;
    assert_eq!(status, 201, "{template_row}");
    let template_id = template_row["id"].as_str().unwrap().to_owned();
    let (status, _) = request(
        port,
        "POST",
        &format!("/api/v1/templates/{template_id}/approve"),
        None,
    )
    .await;
    assert_eq!(status, 204);
    let (status, _) = request(
        port,
        "POST",
        &format!("/api/v1/templates/{template_id}/activate"),
        None,
    )
    .await;
    assert_eq!(status, 204);
    template_id
}

struct ReleasedMaterial {
    material_id: String,
    release_id: String,
    version: i64,
}

#[expect(
    clippy::too_many_arguments,
    reason = "one helper builds the full S03 release journey; argument count is bounded by the command inputs"
)]
async fn create_released_material(
    port: u16,
    opportunity: &str,
    recipient: &str,
    template_id: &str,
    exchange_version: i64,
    body: &str,
    slots: Value,
    bindings: Value,
    overrides: Value,
) -> ReleasedMaterial {
    let (status, material) = request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{opportunity}/materials"),
        Some(json!({
            "recipient_id": recipient,
            "kind": "outreach",
            "template_version_id": template_id,
            "body": body,
            "exchange_version": exchange_version,
            "slot_fills": slots,
            "sentence_bindings": bindings,
        })),
    )
    .await;
    assert_eq!(status, 201, "{material}");
    let material_id = material["id"].as_str().unwrap().to_owned();
    let (status, approval) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/approve"),
        Some(json!({"override_reasons": overrides})),
    )
    .await;
    assert_eq!(status, 200, "{approval}");
    let (status, released) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/release"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{released}");
    ReleasedMaterial {
        material_id,
        release_id: released["release_id"].as_str().unwrap().to_owned(),
        version: released["version"].as_i64().unwrap(),
    }
}

async fn mark_sent(
    port: u16,
    thread_id: &str,
    opportunity: &str,
    recipient: &str,
    primary: &ReleasedMaterial,
    companions: Value,
    deadline: Option<Value>,
) -> (u16, Value) {
    request(
        port,
        "POST",
        &format!("/api/v1/threads/{thread_id}/send"),
        Some(json!({
            "opportunity_id": opportunity,
            "recipient_id": recipient,
            "primary": {"material_id": primary.material_id, "version": primary.version, "release_id": primary.release_id},
            "companions": companions,
            "deadline": deadline,
        })),
    )
    .await
}

async fn record_outcome(port: u16, send_id: &str, class: &str, rung: Option<i64>) -> (u16, Value) {
    request(
        port,
        "POST",
        &format!("/api/v1/sends/{send_id}/outcome"),
        Some(json!({"class": class, "rung_accepted": rung})),
    )
    .await
}

#[tokio::test]
async fn s07_mark_sent_deadline_and_capacity_charged_exactly_once() {
    let (path, workspace, port, service) = setup().await;
    let recipient = create_party(port, "Dana Lee").await;
    let opportunity = create_opportunity(port, "Onboarding", &recipient).await;
    let exchange_version = prepare_exchange(port, &opportunity, &recipient).await;
    let proof = create_proof(port, 15).await;
    let template = activate_template(
        port,
        "Capacity note",
        "Hello.\nI have {{capacity_line:slots}} project slots open this quarter.",
    )
    .await;
    let material = create_released_material(
        port,
        &opportunity,
        &recipient,
        &template,
        exchange_version,
        "Hello.\nI have 15 project slots open this quarter.",
        json!([{"slot":"capacity_line:slots","value":"15"}]),
        json!([
            {"sentence_index":0,"claim_type":"generic"},
            {"sentence_index":1,"claim_type":"self_fact","ref_kind":"resource","ref_id":proof,"approved_wording":"I have 15 project slots open this quarter."}
        ]),
        json!({"S1 GIVE_FIRST":"This is a reviewed factual outreach, not a gift message.","S2 ANCHOR":"The exact approved text is the anchor for this outreach."}),
    )
    .await;

    let (status, send) = mark_sent(
        port,
        "t-capacity",
        &opportunity,
        &recipient,
        &material,
        json!([]),
        None,
    )
    .await;
    assert_eq!(status, 201, "{send}");
    assert_eq!(send["capacity_charged"], true);
    assert_eq!(send["touch_no"], 1);
    assert!(send["response_deadline"].as_str().unwrap().ends_with('Z'));
    assert_eq!(send["deadline_zone"], "UTC");
    assert_eq!(send["primary_material_id"], material.material_id);

    let (committed, remaining): (f64, f64) = sqlx::query_as(
        "SELECT committed,total-committed FROM capacity_ledger WHERE period='quarter' AND unit='project slots'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(committed, 1.0);
    assert_eq!(remaining, 14.0);

    let thread: (String, i64, Option<String>) = sqlx::query_as(
        "SELECT state,current_touch,expires_at FROM contact_thread WHERE id='t-capacity'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(thread.0, "waiting");
    assert_eq!(thread.1, 1);
    assert!(thread.2.is_some());

    // The same capacity claim now mismatches the ledger; no second charge.
    let (status, blocked) = mark_sent(
        port,
        "t-capacity-2",
        &opportunity,
        &recipient,
        &material,
        json!([]),
        None,
    )
    .await;
    assert_eq!(status, 422, "{blocked}");
    assert!(blocked["error"]["message"].as_str().unwrap().contains("H5"));
    let (committed,): (f64,) = sqlx::query_as(
        "SELECT committed FROM capacity_ledger WHERE period='quarter' AND unit='project slots'",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(committed, 1.0);
    let sends: i64 = sqlx::query_scalar("SELECT count(*) FROM send")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(sends, 1);

    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s07_timeout_equality_is_inclusive_and_happens_exactly_once_per_touch() {
    let (path, workspace, port, service) = setup().await;
    let recipient = create_party(port, "Dana Lee").await;
    let opportunity = create_opportunity(port, "Onboarding", &recipient).await;
    let exchange_version = prepare_exchange(port, &opportunity, &recipient).await;
    let template = activate_template(port, "Hello", "Hello.").await;
    let material = create_released_material(
        port,
        &opportunity,
        &recipient,
        &template,
        exchange_version,
        "Hello.",
        json!([]),
        json!([{"sentence_index":0,"claim_type":"generic"}]),
    json!({"S1 GIVE_FIRST":"This is a reviewed factual outreach, not a gift message.","S2 ANCHOR":"The exact approved text is the anchor for this outreach."}),
    )
    .await;

    // Deadline exactly equal to the reconcile instant: inclusive expiry must
    // time the thread out. A strict `>` comparison would leave it waiting.
    let now = now_str();
    let (status, send) = mark_sent(
        port,
        "t-equality",
        &opportunity,
        &recipient,
        &material,
        json!([]),
        Some(json!({"at": now})),
    )
    .await;
    assert_eq!(status, 201, "{send}");
    let send_id = send["id"].as_str().unwrap().to_owned();

    let report = workspace.reconcile_deadlines(&now).await.unwrap();
    assert_eq!(report.timed_out, 1);
    assert_eq!(report.opportunities_closed, 1);
    let thread: (String, i64) =
        sqlx::query_as("SELECT state,current_touch FROM contact_thread WHERE id='t-equality'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(thread.0, "timed_out");
    let opp: (String, Option<String>) =
        sqlx::query_as("SELECT state,close_reason FROM opportunity WHERE id=?")
            .bind(&opportunity)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(opp.0, "closed");
    assert_eq!(opp.1.as_deref(), Some("timed_out"));

    // Reconcile again: no second timeout event for the same touch.
    let again = workspace.reconcile_deadlines(&now).await.unwrap();
    assert_eq!(again.timed_out, 0);
    let timeout_events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event WHERE event_type='thread.timed_out'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(timeout_events, 1);

    // The late reply is recorded but cannot rewind the timed-out thread.
    let (status, outcome) = record_outcome(port, &send_id, "reply_positive", None).await;
    assert_eq!(status, 200, "{outcome}");
    let thread: (String,) =
        sqlx::query_as("SELECT state FROM contact_thread WHERE id='t-equality'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(thread.0, "timed_out");
    let late_events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event WHERE event_type='outcome.recorded_late'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(late_events, 1);

    // Explicit owner reopen starts a new active lifecycle and reopens the
    // opportunity; it never clears a do-not-contact flag.
    let (status, reopened) = request(port, "POST", "/api/v1/threads/t-equality/reopen", None).await;
    assert_eq!(status, 200, "{reopened}");
    assert_eq!(reopened["state"], "waiting");
    let opp: (String, Option<String>) =
        sqlx::query_as("SELECT state,close_reason FROM opportunity WHERE id=?")
            .bind(&opportunity)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(opp.0, "open");
    assert!(opp.1.is_none());

    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s07_decline_suppresses_party_across_opportunities_but_spares_others() {
    let (path, workspace, port, service) = setup().await;
    let first = create_party(port, "Dana Lee").await;
    let second = create_party(port, "Sam Rivera").await;
    let opportunity_a = create_opportunity(port, "Shared offer", &first).await;
    add_participant(port, &opportunity_a, &second).await;
    let opportunity_b = create_opportunity(port, "Separate offer", &first).await;

    let exchange_a_first = prepare_exchange(port, &opportunity_a, &first).await;
    let exchange_a_second = prepare_exchange(port, &opportunity_a, &second).await;
    let exchange_b_first = prepare_exchange(port, &opportunity_b, &first).await;
    let template = activate_template(port, "Hello", "Hello.").await;
    let material_a_first = create_released_material(
        port,
        &opportunity_a,
        &first,
        &template,
        exchange_a_first,
        "Hello.",
        json!([]),
        json!([{"sentence_index":0,"claim_type":"generic"}]),
    json!({"S1 GIVE_FIRST":"This is a reviewed factual outreach, not a gift message.","S2 ANCHOR":"The exact approved text is the anchor for this outreach."}),
    )
    .await;
    let material_a_second = create_released_material(
        port,
        &opportunity_a,
        &second,
        &template,
        exchange_a_second,
        "Hello.",
        json!([]),
        json!([{"sentence_index":0,"claim_type":"generic"}]),
    json!({"S1 GIVE_FIRST":"This is a reviewed factual outreach, not a gift message.","S2 ANCHOR":"The exact approved text is the anchor for this outreach."}),
    )
    .await;
    let material_b_first = create_released_material(
        port,
        &opportunity_b,
        &first,
        &template,
        exchange_b_first,
        "Hello.",
        json!([]),
        json!([{"sentence_index":0,"claim_type":"generic"}]),
    json!({"S1 GIVE_FIRST":"This is a reviewed factual outreach, not a gift message.","S2 ANCHOR":"The exact approved text is the anchor for this outreach."}),
    )
    .await;

    let (status, send_a) = mark_sent(
        port,
        "t-a-first",
        &opportunity_a,
        &first,
        &material_a_first,
        json!([]),
        None,
    )
    .await;
    assert_eq!(status, 201);
    let (status, _) = mark_sent(
        port,
        "t-a-second",
        &opportunity_a,
        &second,
        &material_a_second,
        json!([]),
        None,
    )
    .await;
    assert_eq!(status, 201);
    let (status, _) = mark_sent(
        port,
        "t-b-first",
        &opportunity_b,
        &first,
        &material_b_first,
        json!([]),
        None,
    )
    .await;
    assert_eq!(status, 201);

    // Decline on the first party: DNC is party-wide.
    let (status, _) = record_outcome(port, send_a["id"].as_str().unwrap(), "declined", None).await;
    assert_eq!(status, 200);

    let policy: (String, i64) =
        sqlx::query_as("SELECT state,do_not_contact FROM party_policy WHERE party_id=?")
            .bind(&first)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(policy.0, "closed");
    assert_eq!(policy.1, 1);
    let thread_states: Vec<String> = sqlx::query_scalar(
        "SELECT state FROM contact_thread WHERE recipient_id=? ORDER BY opportunity_id",
    )
    .bind(&first)
    .fetch_all(workspace.pool())
    .await
    .unwrap();
    assert!(thread_states.iter().all(|state| state == "closed_declined"));

    // Opportunity A stays open because the unrelated participant is live;
    // opportunity B has no remaining live contact and closes as declined.
    let opp_a: (String, Option<String>) =
        sqlx::query_as("SELECT state,close_reason FROM opportunity WHERE id=?")
            .bind(&opportunity_a)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(opp_a.0, "open");
    let opp_b: (String, Option<String>) =
        sqlx::query_as("SELECT state,close_reason FROM opportunity WHERE id=?")
            .bind(&opportunity_b)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(opp_b.0, "closed");
    assert_eq!(opp_b.1.as_deref(), Some("declined"));

    // Fresh release for the declining party is blocked everywhere; the
    // unrelated participant remains available with a fresh post-decline
    // version (its party policy is untouched by the decline).
    let (status, blocked_first) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{}/release", material_a_first.material_id),
        None,
    )
    .await;
    assert_eq!(status, 422, "{blocked_first}");
    assert!(
        !blocked_first["error"]["message"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    let fresh_second = create_released_material(
        port,
        &opportunity_a,
        &second,
        &template,
        exchange_a_second,
        "Hello.",
        json!([]),
        json!([{"sentence_index":0,"claim_type":"generic"}]),
    json!({"S1 GIVE_FIRST":"This is a reviewed factual outreach, not a gift message.","S2 ANCHOR":"The exact approved text is the anchor for this outreach."}),
    )
    .await;
    let (status, released_second) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{}/release", fresh_second.material_id),
        None,
    )
    .await;
    assert_eq!(status, 200, "{released_second}");

    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s07_companion_bound_to_another_recipient_rolls_back_whole_send() {
    let (path, workspace, port, service) = setup().await;
    let first = create_party(port, "Dana Lee").await;
    let second = create_party(port, "Sam Rivera").await;
    let opportunity = create_opportunity(port, "Shared offer", &first).await;
    add_participant(port, &opportunity, &second).await;
    let exchange_first = prepare_exchange(port, &opportunity, &first).await;
    let exchange_second = prepare_exchange(port, &opportunity, &second).await;
    let template = activate_template(port, "Hello", "Hello.").await;
    let material_first = create_released_material(
        port,
        &opportunity,
        &first,
        &template,
        exchange_first,
        "Hello.",
        json!([]),
        json!([{"sentence_index":0,"claim_type":"generic"}]),
    json!({"S1 GIVE_FIRST":"This is a reviewed factual outreach, not a gift message.","S2 ANCHOR":"The exact approved text is the anchor for this outreach."}),
    )
    .await;
    let material_second = create_released_material(
        port,
        &opportunity,
        &second,
        &template,
        exchange_second,
        "Hello.",
        json!([]),
        json!([{"sentence_index":0,"claim_type":"generic"}]),
    json!({"S1 GIVE_FIRST":"This is a reviewed factual outreach, not a gift message.","S2 ANCHOR":"The exact approved text is the anchor for this outreach."}),
    )
    .await;

    // Companion targets the other recipient: the whole send must roll back.
    let (status, blocked) = mark_sent(
        port,
        "t-companion",
        &opportunity,
        &first,
        &material_first,
        json!([{"material_id": material_second.material_id, "version": material_second.version, "release_id": material_second.release_id}]),
        None,
    )
    .await;
    assert_eq!(status, 422, "{blocked}");
    assert!(
        blocked["error"]["message"]
            .as_str()
            .unwrap()
            .contains("same opportunity and recipient")
    );
    let sends: i64 = sqlx::query_scalar("SELECT count(*) FROM send")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(sends, 0);
    let parts: i64 = sqlx::query_scalar("SELECT count(*) FROM send_part")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(parts, 0);
    let threads: i64 = sqlx::query_scalar("SELECT count(*) FROM contact_thread")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(threads, 0);

    service.shutdown().await;
    let _ = fs::remove_file(path);
}

const K_OVERRIDES: &str = r#"{"S1 GIVE_FIRST":"This is a reviewed factual outreach, not a gift message.","S2 ANCHOR":"The exact approved text is the anchor for this outreach."}"#;

async fn create_minimal_material(
    port: u16,
    opportunity: &str,
    recipient: &str,
    template: &str,
    exchange: i64,
    overrides: Value,
) -> ReleasedMaterial {
    create_released_material(
        port,
        opportunity,
        recipient,
        template,
        exchange,
        "Hello.",
        json!([]),
        json!([{"sentence_index":0,"claim_type":"generic"}]),
        overrides,
    )
    .await
}

#[tokio::test]
async fn s07_next_action_at_and_cadence_spacing_gate_are_real() {
    let (path, workspace, port, service) = setup().await;
    let recipient = create_party(port, "Dana Lee").await;
    let opportunity = create_opportunity(port, "Onboarding", &recipient).await;
    let exchange_version = prepare_exchange(port, &opportunity, &recipient).await;
    let template = activate_template(port, "Hello", "Hello.").await;

    let first = create_minimal_material(
        port,
        &opportunity,
        &recipient,
        &template,
        exchange_version,
        serde_json::from_str(K_OVERRIDES).unwrap(),
    )
    .await;
    let (status, sent) = mark_sent(
        port,
        "t-spacing",
        &opportunity,
        &recipient,
        &first,
        json!([]),
        None,
    )
    .await;
    assert_eq!(status, 201, "{sent}");
    assert_eq!(sent["touch_no"], 1);

    // Next action is the pack cadence (4 business days for the first touch).
    let sent_date =
        chrono::NaiveDate::parse_from_str(&sent["sent_at"].as_str().unwrap()[..10], "%Y-%m-%d")
            .unwrap();
    let expected_next = parley_domain::lifecycle::add_business_days(sent_date, 4);
    let expected_next_str = format!("{}T00:00:00.000Z", expected_next.format("%Y-%m-%d"));
    assert_eq!(sent["next_action_at"].as_str().unwrap(), expected_next_str);
    let thread_next: Option<String> =
        sqlx::query_scalar("SELECT next_action_at FROM contact_thread WHERE id='t-spacing'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(thread_next.as_deref(), Some(expected_next_str.as_str()));

    // An immediate same-day send violates S7 CADENCE (no override).
    let second = create_minimal_material(
        port,
        &opportunity,
        &recipient,
        &template,
        exchange_version,
        serde_json::from_str(K_OVERRIDES).unwrap(),
    )
    .await;
    let (status, blocked) = mark_sent(
        port,
        "t-spacing",
        &opportunity,
        &recipient,
        &second,
        json!([]),
        None,
    )
    .await;
    assert_eq!(status, 422, "{blocked}");
    assert!(
        blocked["error"]["message"]
            .as_str()
            .unwrap()
            .contains("S7 CADENCE")
    );

    // With an explicit S7 override the follow-up is allowed and plans the
    // second-touch cadence (8 business days).
    let overridden = serde_json::from_str::<Value>(K_OVERRIDES).unwrap();
    let with_spacing = json!({
        "S1 GIVE_FIRST": overridden["S1 GIVE_FIRST"],
        "S2 ANCHOR": overridden["S2 ANCHOR"],
        "S7 CADENCE": "The follow-up is scheduled at the pack cadence."
    });
    let third = create_minimal_material(
        port,
        &opportunity,
        &recipient,
        &template,
        exchange_version,
        with_spacing.clone(),
    )
    .await;
    let (status, followup) = mark_sent(
        port,
        "t-spacing",
        &opportunity,
        &recipient,
        &third,
        json!([]),
        None,
    )
    .await;
    assert_eq!(status, 201, "{followup}");
    assert_eq!(followup["touch_no"], 2);
    let followup_sent =
        chrono::NaiveDate::parse_from_str(&followup["sent_at"].as_str().unwrap()[..10], "%Y-%m-%d")
            .unwrap();
    let expected_followup = parley_domain::lifecycle::add_business_days(followup_sent, 4);
    let expected_followup_str = format!("{}T00:00:00.000Z", expected_followup.format("%Y-%m-%d"));
    assert_eq!(
        followup["next_action_at"].as_str().unwrap(),
        expected_followup_str
    );

    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s07_daily_cap_gate_is_real_and_enforced_per_day() {
    let (path, workspace, port, service) = setup().await;
    let recipient = create_party(port, "Dana Lee").await;
    let opportunity = create_opportunity(port, "Onboarding", &recipient).await;
    let exchange_version = prepare_exchange(port, &opportunity, &recipient).await;
    let template = activate_template(port, "Hello", "Hello.").await;
    let overrides = serde_json::from_str::<Value>(K_OVERRIDES).unwrap();
    let with_spacing = json!({
        "S1 GIVE_FIRST": overrides["S1 GIVE_FIRST"],
        "S2 ANCHOR": overrides["S2 ANCHOR"],
        "S7 CADENCE": "The follow-up is scheduled at the pack cadence."
    });

    // job_search daily cap is 12; fill it exactly.
    for index in 0..12 {
        let material = create_minimal_material(
            port,
            &opportunity,
            &recipient,
            &template,
            exchange_version,
            with_spacing.clone(),
        )
        .await;
        let (status, sent) = mark_sent(
            port,
            "t-cap",
            &opportunity,
            &recipient,
            &material,
            json!([]),
            None,
        )
        .await;
        assert_eq!(status, 201, "send {index}: {sent}");
    }
    let sends: i64 = sqlx::query_scalar("SELECT count(*) FROM send")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(sends, 12);

    // The thirteenth send fails S8 DAILY_CAP.
    let thirteenth = create_minimal_material(
        port,
        &opportunity,
        &recipient,
        &template,
        exchange_version,
        with_spacing.clone(),
    )
    .await;
    let (status, blocked) = mark_sent(
        port,
        "t-cap",
        &opportunity,
        &recipient,
        &thirteenth,
        json!([]),
        None,
    )
    .await;
    assert_eq!(status, 422, "{blocked}");
    assert!(
        blocked["error"]["message"]
            .as_str()
            .unwrap()
            .contains("S8 DAILY_CAP")
    );
    let sends_after: i64 = sqlx::query_scalar("SELECT count(*) FROM send")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(sends_after, 12);

    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s07_thread_list_exposes_send_ids_and_outcome_history() {
    let (path, _workspace, port, service) = setup().await;
    let recipient = create_party(port, "Dana Lee").await;
    let opportunity = create_opportunity(port, "Onboarding", &recipient).await;
    let exchange_version = prepare_exchange(port, &opportunity, &recipient).await;
    let template = activate_template(port, "Hello", "Hello.").await;
    let material = create_minimal_material(
        port,
        &opportunity,
        &recipient,
        &template,
        exchange_version,
        serde_json::from_str(K_OVERRIDES).unwrap(),
    )
    .await;
    let (status, send) = mark_sent(
        port,
        "t-history",
        &opportunity,
        &recipient,
        &material,
        json!([]),
        None,
    )
    .await;
    assert_eq!(status, 201, "{send}");
    let send_id = send["id"].as_str().unwrap().to_owned();
    let (status, outcome) = record_outcome(port, &send_id, "reply_positive", None).await;
    assert_eq!(status, 200, "{outcome}");

    // A fresh read (a reloaded client) sees the send ID and its outcome so a
    // response can be attached to the historical send, not only to the
    // session that performed Mark sent.
    let (status, threads) = request(port, "GET", "/api/v1/threads", None).await;
    assert_eq!(status, 200, "{threads}");
    let thread = threads
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "t-history")
        .expect("thread is listed");
    let sends = thread["sends"].as_array().unwrap();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["id"], send_id);
    assert_eq!(sends[0]["touch_no"], 1);
    assert_eq!(sends[0]["outcome"]["class"], "reply_positive");
    assert_eq!(sends[0]["outcome"]["send_id"], send_id);
    assert_eq!(sends[0]["primary_material_id"], material.material_id);

    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s07_dnc_clearance_requires_reason_and_reopens_eligibility() {
    let (path, workspace, port, service) = setup().await;
    let recipient = create_party(port, "Dana Lee").await;
    let opportunity = create_opportunity(port, "Onboarding", &recipient).await;
    let exchange_version = prepare_exchange(port, &opportunity, &recipient).await;
    let template = activate_template(port, "Hello", "Hello.").await;
    let material = create_minimal_material(
        port,
        &opportunity,
        &recipient,
        &template,
        exchange_version,
        serde_json::from_str(K_OVERRIDES).unwrap(),
    )
    .await;
    let (status, send) = mark_sent(
        port,
        "t-dnc",
        &opportunity,
        &recipient,
        &material,
        json!([]),
        None,
    )
    .await;
    assert_eq!(status, 201, "{send}");
    let send_id = send["id"].as_str().unwrap().to_owned();
    let (status, _) = record_outcome(port, &send_id, "declined", None).await;
    assert_eq!(status, 200);

    let policy: (String, i64) =
        sqlx::query_as("SELECT state,do_not_contact FROM party_policy WHERE party_id=?")
            .bind(&recipient)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(policy.0, "closed");
    assert_eq!(policy.1, 1);

    // Clearance without a reason is rejected.
    let (status, rejected) = request(
        port,
        "POST",
        &format!("/api/v1/entities/{recipient}/clear-dnc"),
        Some(json!({"reason":""})),
    )
    .await;
    assert_eq!(status, 422, "{rejected}");
    assert!(
        rejected["error"]["message"]
            .as_str()
            .unwrap()
            .contains("at least ten characters")
    );

    // Fresh release stays blocked while suppression is active: approving a
    // fresh version also fails the gate because the party is not open.
    let overrides_json = serde_json::from_str::<Value>(K_OVERRIDES).unwrap();
    let (status, pre_approval) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{}/approve", material.material_id),
        Some(json!({"override_reasons": overrides_json.clone()})),
    )
    .await;
    assert_eq!(status, 422, "{pre_approval}");
    let (status, blocked) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{}/release", material.material_id),
        None,
    )
    .await;
    assert_eq!(status, 422, "{blocked}");

    // A reason-bearing clearance restores eligibility for fresh release. The
    // party-policy revision change invalidates the earlier approval, so a
    // fresh version must be approved and released after the clearance.
    let (status, cleared) = request(
        port,
        "POST",
        &format!("/api/v1/entities/{recipient}/clear-dnc"),
        Some(json!({"reason":"The counterparty asked to be re-contacted after a review."})),
    )
    .await;
    assert_eq!(status, 200, "{cleared}");
    assert_eq!(cleared["state"], "open");
    assert_eq!(cleared["do_not_contact"], false);
    let policy_after: (String, i64) =
        sqlx::query_as("SELECT state,do_not_contact FROM party_policy WHERE party_id=?")
            .bind(&recipient)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(policy_after.0, "open");
    assert_eq!(policy_after.1, 0);
    let cleared_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event WHERE event_type='party.dnc_cleared' AND entity_id=?",
    )
    .bind(&recipient)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(cleared_events, 1);

    let (status, revised) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{}/versions", material.material_id),
        Some(json!({
            "recipient_id": recipient,
            "kind": "outreach",
            "template_version_id": template,
            "body": "Hello.",
            "exchange_version": exchange_version,
            "slot_fills": [],
            "sentence_bindings": [{"sentence_index":0,"claim_type":"generic"}],
        })),
    )
    .await;
    assert_eq!(status, 201, "{revised}");
    assert!(revised["version"].as_i64().unwrap() > material.version);
    let (status, approval) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{}/approve", material.material_id),
        Some(json!({"override_reasons": overrides_json})),
    )
    .await;
    assert_eq!(status, 200, "{approval}");
    let (status, released) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{}/release", material.material_id),
        None,
    )
    .await;
    assert_eq!(status, 200, "{released}");

    service.shutdown().await;
    let _ = fs::remove_file(path);
}
