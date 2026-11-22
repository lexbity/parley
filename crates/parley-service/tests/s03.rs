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
        "parley-s03-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
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
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\nIdempotency-Key: s03-{key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
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

async fn prepare_exchange(
    port: u16,
    opportunity: &str,
    recipient: &str,
    attest: bool,
) -> (String, String, i64) {
    let(status,them)=request(port,"POST",&format!("/api/v1/opportunities/{opportunity}/interests"),Some(json!({"recipient_id":recipient,"side":"them","statement":"A clearer onboarding handoff","knowledge_kind":"hypothesis","check_suggestion":"Ask the owner whether this is current."}))).await;
    assert_eq!(status, 201, "{them}");
    let(status,me)=request(port,"POST",&format!("/api/v1/opportunities/{opportunity}/interests"),Some(json!({"recipient_id":recipient,"side":"me","statement":"Offer a usable checklist","knowledge_kind":"owner_report"}))).await;
    assert_eq!(status, 201);
    let(status,exchange)=request(port,"POST",&format!("/api/v1/opportunities/{opportunity}/exchange"),Some(json!({"recipient_id":recipient,"they_get":"A practical onboarding checklist","i_get":"A better understanding of the handoff","interest_ids":[them["id"],me["id"]]}))).await;
    assert_eq!(status, 201);
    let version = exchange["version"].as_i64().unwrap();
    if attest {
        let (status, _) = request(
            port,
            "POST",
            &format!("/api/v1/opportunities/{opportunity}/exchange/{recipient}/attest"),
            None,
        )
        .await;
        assert_eq!(status, 200);
    }
    (
        them["id"].as_str().unwrap().to_owned(),
        me["id"].as_str().unwrap().to_owned(),
        version,
    )
}

#[tokio::test]
async fn s03_one_source_can_link_to_two_opportunities_and_unlink_independently() {
    let (path, _workspace, port, service) = setup().await;
    let party = create_party(port, "Dana Lee").await;
    let (status, source) = request(
        port,
        "POST",
        "/api/v1/captures",
        Some(json!({"text":"The onboarding team is expanding."})),
    )
    .await;
    assert_eq!(status, 201);
    let (status, first) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":"Onboarding","primary_party_id":party})),
    )
    .await;
    assert_eq!(status, 201);
    let (status, second) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":"Hiring","primary_party_id":party})),
    )
    .await;
    assert_eq!(status, 201);
    let source_id = source["variant_id"].as_str().unwrap();
    for opportunity in [&first, &second] {
        let (status, _) = request(
            port,
            "POST",
            &format!(
                "/api/v1/opportunities/{}/sources/{source_id}",
                opportunity["id"].as_str().unwrap()
            ),
            None,
        )
        .await;
        assert_eq!(status, 204);
    }
    let (status, first_read) = request(
        port,
        "GET",
        &format!("/api/v1/opportunities/{}", first["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let (status, second_read) = request(
        port,
        "GET",
        &format!("/api/v1/opportunities/{}", second["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(first_read["source_variant_ids"][0], source_id);
    assert_eq!(second_read["source_variant_ids"][0], source_id);
    let (status, _) = request(
        port,
        "DELETE",
        &format!(
            "/api/v1/opportunities/{}/sources/{source_id}",
            first["id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert_eq!(status, 204);
    let (status, first_after) = request(
        port,
        "GET",
        &format!("/api/v1/opportunities/{}", first["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        first_after["source_variant_ids"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (status, second_after) = request(
        port,
        "GET",
        &format!("/api/v1/opportunities/{}", second["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(second_after["source_variant_ids"][0], source_id);
    assert_eq!(source["variant_id"], source_id);
    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s03_approved_text_releases_exact_bytes_and_is_shared_by_queue_and_output() {
    let (path, _workspace, port, service) = setup().await;
    let recipient = create_party(port, "Dana Lee").await;
    let(status,gift)=request(port,"POST","/api/v1/entities",Some(json!({"kind":"gift","title":"Onboarding checklist","body":{"what":"Onboarding checklist","delivery":"on_request"}}))).await;
    assert_eq!(status, 201);
    let(status,ready)=request(port,"POST",&format!("/api/v1/resources/{}/ready",gift["id"].as_str().unwrap()),Some(json!({"body":{"what":"Onboarding checklist","format":"template","problem_tags":["onboarding"],"delivery":"on_request","time_to_produce_minutes":20,"cost_to_me":"low"}}))).await;
    assert_eq!(status, 204, "{ready}");
    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":"Onboarding exchange","primary_party_id":recipient})),
    )
    .await;
    assert_eq!(status, 201);
    let opportunity_id = opportunity["id"].as_str().unwrap();
    let (_, _, exchange_version) = prepare_exchange(port, opportunity_id, &recipient, true).await;
    let(status,template_row)=request(port,"POST","/api/v1/templates",Some(json!({"name":"One useful offer","body":"Hi {{their_name:name}}.\n{{gift:offer}}\n{{ask:question}}"}))).await;
    assert_eq!(status, 201, "{template_row}");
    let template_id = template_row["id"].as_str().unwrap();
    assert_eq!(
        request(
            port,
            "POST",
            &format!("/api/v1/templates/{template_id}/approve"),
            None
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
            None
        )
        .await
        .0,
        204
    );
    let body = "Hi Dana Lee.\nI can share an Onboarding checklist.\nWould you take a look at it?";
    let binding = json!([
        {"sentence_index":0,"claim_type":"generic"},
        {"sentence_index":1,"claim_type":"offer","ref_kind":"resource","ref_id":gift["id"]},
        {"sentence_index":2,"claim_type":"ask","ref_kind":"ladder","ref_id":"rung-1","ask_rung":1}
    ]);
    let(status,material)=request(port,"POST",&format!("/api/v1/opportunities/{opportunity_id}/materials"),Some(json!({"recipient_id":recipient,"kind":"outreach","template_version_id":template_id,"body":body,"exchange_version":exchange_version,"slot_fills":[{"slot":"their_name:name","value":"Dana Lee"},{"slot":"gift:offer","value":"Onboarding checklist","resource_id":gift["id"]},{"slot":"ask:question","value":"Would you take a look at it?"}],"sentence_bindings":binding}))).await;
    assert_eq!(status, 201, "{material}");
    let material_id = material["id"].as_str().unwrap();
    let (status, blocked) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/approve"),
        Some(json!({"override_reasons":{}})),
    )
    .await;
    assert_eq!(
        status, 422,
        "soft anchor check requires an individual override"
    );
    assert!(
        blocked["error"]["message"]
            .as_str()
            .unwrap()
            .contains("S2 ANCHOR")
    );
    let reasons = json!({"override_reasons":{"S2 ANCHOR":"The checklist is a direct practical offer for this contact."}});
    let (status, approval) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/approve"),
        Some(reasons),
    )
    .await;
    assert_eq!(status, 200, "{approval}");
    assert_eq!(approval["content_sha256"], material["content_sha256"]);
    let (status, queue) = request(
        port,
        "GET",
        &format!("/api/v1/opportunities/{opportunity_id}/materials"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(queue[0]["id"], material_id);
    let (status, outputs) = request(port, "GET", "/api/v1/outputs", None).await;
    assert_eq!(status, 200);
    assert_eq!(outputs[0]["id"], material_id);
    assert_eq!(outputs[0]["version"], 1);
    let (status, released) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/release"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{released}");
    assert_eq!(released["body"], body);
    assert_eq!(released["content_sha256"], material["content_sha256"]);
    let (status, _) = request(
        port,
        "POST",
        &format!(
            "/api/v1/releases/{}/ack",
            released["release_id"].as_str().unwrap()
        ),
        Some(json!({"succeeded":true})),
    )
    .await;
    assert_eq!(status, 204);
    let release_count: i64 = sqlx::query_scalar("SELECT count(*) FROM release_record")
        .fetch_one(_workspace.pool())
        .await
        .unwrap();
    assert_eq!(release_count, 1);
    let touch_count: i64 =
        sqlx::query_scalar("SELECT touch_count FROM party_policy WHERE party_id=?")
            .bind(&recipient)
            .fetch_one(_workspace.pool())
            .await
            .unwrap();
    assert_eq!(
        touch_count, 0,
        "release must not create a sent touch or reply timer"
    );
    let sent_events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event WHERE event_type LIKE 'send.%'")
            .fetch_one(_workspace.pool())
            .await
            .unwrap();
    assert_eq!(sent_events, 0);
    service.shutdown().await;
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s03_correction_after_approval_revokes_release_without_mutating_history() {
    let (path, workspace, port, service) = setup().await;
    let (status, org) = request(
        port,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"org","name":"Northline"})),
    )
    .await;
    assert_eq!(status, 201);
    let (status, source) = request(
        port,
        "POST",
        "/api/v1/captures",
        Some(json!({"text":"Dana Lee will work with Northline.","title":"Team update"})),
    )
    .await;
    assert_eq!(status, 201);
    let(status,relation)=request(port,"POST","/api/v1/graph/related",Some(json!({"kind":"person","name":"Dana Lee","related_entity_id":org["id"],"verb":"member_of","occurrence_id":source["occurrence_id"],"quote":"Dana Lee will work with Northline.","view_scope":"opportunity","view_id":"pending","x":120.0,"y":90.0}))).await;
    assert_eq!(status, 201, "{relation}");
    let recipient = relation["id"].as_str().unwrap();
    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":"Northline onboarding","primary_party_id":recipient})),
    )
    .await;
    assert_eq!(status, 201);
    let opportunity_id = opportunity["id"].as_str().unwrap();
    let (_, _, exchange_version) = prepare_exchange(port, opportunity_id, recipient, true).await;
    let claim_id: String = sqlx::query_scalar("SELECT id FROM claim WHERE subject_id=?")
        .bind(recipient)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    let (status, template_row) = request(
        port,
        "POST",
        "/api/v1/templates",
        Some(json!({"name":"Research note","body":"Hello."})),
    )
    .await;
    assert_eq!(status, 201);
    let template_id = template_row["id"].as_str().unwrap();
    assert_eq!(
        request(
            port,
            "POST",
            &format!("/api/v1/templates/{template_id}/approve"),
            None
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
            None
        )
        .await
        .0,
        204
    );
    let sentence = "Dana Lee will work with Northline.";
    let(status,material)=request(port,"POST",&format!("/api/v1/opportunities/{opportunity_id}/materials"),Some(json!({"recipient_id":recipient,"kind":"outreach","template_version_id":template_id,"body":sentence,"exchange_version":exchange_version,"slot_fills":[],"sentence_bindings":[{"sentence_index":0,"claim_type":"other_fact","ref_kind":"claim","ref_id":claim_id,"ref_revision":1,"approved_wording":sentence}]}))).await;
    assert_eq!(status, 201, "{material}");
    let material_id = material["id"].as_str().unwrap();
    let(status,approval)=request(port,"POST",&format!("/api/v1/materials/{material_id}/approve"),Some(json!({"override_reasons":{"S1 GIVE_FIRST":"This is a reviewed factual update, not a gift message."}}))).await;
    assert_eq!(status, 200, "{approval}");
    let mut correction = workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    sqlx::query("INSERT INTO claim_revision(claim_id,revision,wording,knowledge_kind,tier,speaker_kind,confirmed_by_owner,created_at) VALUES(?,2,'Dana Lee is no longer associated with Northline.','owner_report',3,'owner',1,'2026-11-21T12:00:00Z')").bind(&claim_id).execute(&mut *correction).await.unwrap();
    sqlx::query("UPDATE claim SET current_revision=2 WHERE id=?")
        .bind(&claim_id)
        .execute(&mut *correction)
        .await
        .unwrap();
    correction.commit().await.unwrap();
    let (status, blocked) = request(
        port,
        "POST",
        &format!("/api/v1/materials/{material_id}/release"),
        None,
    )
    .await;
    assert_eq!(status, 422, "{blocked}");
    assert!(
        blocked["error"]["message"]
            .as_str()
            .unwrap()
            .contains("corrected"),
        "{blocked}"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM release_record")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let historic: String = sqlx::query_scalar(
        "SELECT body FROM material_version WHERE material_id=? AND version_no=1",
    )
    .bind(material_id)
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(historic, sentence);
    service.shutdown().await;
    let _ = fs::remove_file(path);
}
