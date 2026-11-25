// R03: retirement lifecycle, board-save edge preservation, retired-endpoint
// guards and opportunity-board segregation (amendment §5/§7).
use parley_service::RunningService;
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
fn path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-r03-http-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

type Session = (String, String, String);

async fn owner_session(service: &RunningService) -> Session {
    let address = service.address;
    let body = json!({"secret": service.bootstrap_secret}).to_string();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(format!("POST /api/v1/session/bootstrap HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let cookie = response
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|line| {
            line.split_once(':').map(|(_, value)| {
                value
                    .trim()
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
        })
        .expect("session cookie");
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(format!("GET /api/v1/session HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nCookie: {cookie}\r\nContent-Length: 0\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let payload: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    (
        cookie,
        payload["mutation_token"].as_str().unwrap().to_owned(),
        payload["epoch"].as_str().unwrap().to_owned(),
    )
}

async fn request(
    service: &RunningService,
    session: &Session,
    method: &str,
    path: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> (u16, Value) {
    let address = service.address;
    let body_text = body.map(|value| value.to_string()).unwrap_or_default();
    let mut headers = format!(
        "Cookie: {}\r\nX-Parley-Token: {}\r\nX-Parley-Epoch: {}",
        session.0, session.1, session.2
    );
    for (name, value) in extra {
        headers.push_str(&format!("\r\n{name}: {value}"));
    }
    if !body_text.is_empty() {
        headers.push_str("\r\nContent-Type: application/json");
    }
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{headers}\r\nContent-Length: {}\r\n\r\n{body_text}", body_text.len()).as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let status: u16 = response.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body: Value = if response.contains("\r\n\r\n") {
        serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    (status, body)
}

fn next_key() -> String {
    format!("r03-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

async fn stage_upload(service: &RunningService, session: &Session, bytes: &[u8]) -> String {
    let address = service.address;
    let headers = format!(
        "Cookie: {}\r\nX-Parley-Token: {}\r\nX-Parley-Epoch: {}\r\nIdempotency-Key: {}\r\nx-parley-filename: notes.txt\r\nContent-Type: text/plain",
        session.0, session.1, session.2, next_key()
    );
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(format!("POST /api/v1/uploads HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{headers}\r\nContent-Length: {}\r\n\r\n", bytes.len()).as_bytes())
        .await
        .unwrap();
    stream.write_all(bytes).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 201"), "{response}");
    let payload: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    payload["id"].as_str().unwrap().to_owned()
}

async fn create_card(
    service: &RunningService,
    session: &Session,
    title: &str,
    subtype: &str,
    column: &str,
) -> String {
    let (status, value) = request(
        service,
        session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {"title": title, "subtype": subtype, "body": {}, "placement": {"column_id": column}}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 201, "{value}");
    value["entity"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn r03_retire_removes_from_ordinary_use_and_guards_endpoints() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let card_id = create_card(&service, &session, "Retire me", "note", "inbox").await;

    let (status, result) = request(
        &service,
        &session,
        "POST",
        &format!("/api/v1/resource-cards/{card_id}/retire"),
        None,
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["entity"]["readiness"], "retired");

    // Gone from the board and the picker.
    let (_, board) = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board?include_offboard=true",
        None,
        &[],
    )
    .await;
    assert!(board["cards"].as_array().unwrap().is_empty());
    assert!(board["off_board"].as_array().unwrap().is_empty());

    // Reads and edits of a retired card fail closed.
    let (status, _) = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/resource-cards/{card_id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": card_id, "expected_revision": 2, "expected_notes_rev": 1, "title": "nope"}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 404);

    // A retired card cannot become an edge endpoint.
    let other = create_card(&service, &session, "Active", "note", "inbox").await;
    let (status, _) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": other, "expected_revision": 1, "expected_notes_rev": 1},
            "relation_add": {"verb": "refers to", "target_id": card_id, "basis": "basis"}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 404);

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r03_board_save_preserves_relationship_collection() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let root = create_card(&service, &session, "Checklist", "artifact", "developing").await;
    let (status, related) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": root, "expected_revision": 1, "expected_notes_rev": 1},
            "relation_add": {"verb": "includes", "create": {"title": "Fixture", "subtype": "artifact", "body": {}}, "basis": "owner link"}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 200, "{related}");

    // Board edit without a relationship intent must not serialize edges.
    let (status, _) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": root, "expected_revision": 1, "expected_notes_rev": 1, "title": "Checklist renamed"}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 200);
    let (_, board) = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board",
        None,
        &[],
    )
    .await;
    let card = board["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["resource_id"] == root)
        .unwrap();
    assert_eq!(card["counts"]["connections"], 1, "{board}");
    let (_, dossier) = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{root}/dossier"),
        None,
        &[],
    )
    .await;
    assert_eq!(
        dossier["data"]["direct_links"].as_array().unwrap().len(),
        1,
        "{dossier}"
    );

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r03_resource_moves_never_touch_the_opportunity_board() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let card_id = create_card(&service, &session, "Resource card", "note", "inbox").await;

    let (status, before) = request(&service, &session, "GET", "/api/v1/board", None, &[]).await;
    assert_eq!(status, 200);
    let (status, _) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-board/moves",
        Some(json!({"resource_id": card_id, "destination_column_id": "available"})),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 200);
    let (_, after) = request(&service, &session, "GET", "/api/v1/board", None, &[]).await;
    assert_eq!(
        before, after,
        "resource moves must not mutate the Queue board"
    );

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r03_erase_removes_clean_cards_and_refuses_provenance() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;

    // A clean card with an owner edge, a bound file and a related card
    // erases completely; the related card keeps its record.
    let root = create_card(&service, &session, "Scratch checklist", "artifact", "inbox").await;
    let upload_id = stage_upload(&service, &session, b"scratch bytes\n").await;
    let bind = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": root, "expected_revision": 1, "expected_notes_rev": 1, "staged_uploads": [upload_id]}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(bind.0, 200, "{bind:?}");
    let (status, related) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": root, "expected_revision": 2, "expected_notes_rev": 1},
            "relation_add": {"verb": "includes", "create": {"title": "Scratch fixture", "subtype": "artifact", "body": {}}, "basis": "scratch link"}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 200, "{related}");

    let erase_key = next_key();
    let erase = [("Idempotency-Key", erase_key.as_str())];
    let (status, result) = request(
        &service,
        &session,
        "POST",
        &format!("/api/v1/resource-cards/{root}/erase"),
        None,
        &erase,
    )
    .await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result, json!({"id": root, "erased": true}));
    let replay = request(
        &service,
        &session,
        "POST",
        &format!("/api/v1/resource-cards/{root}/erase"),
        None,
        &erase,
    )
    .await;
    assert_eq!(replay.0, 200);
    assert_eq!(replay.1, result);

    // Reads report the erasure; the board drops the card, its edge and its
    // placement, while the related card stays with no dangling connection.
    let (status, missing) = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/resource-cards/{root}"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(missing["error"]["code"], "RESOURCE_ERASED");
    let (status, _) = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{root}/attachments"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, 404);
    let (_, board) = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board?include_offboard=true",
        None,
        &[],
    )
    .await;
    assert!(
        !board["cards"]
            .as_array()
            .unwrap()
            .iter()
            .any(|card| card["resource_id"] == root.as_str()),
        "{}",
        board
    );
    // Related cards begin off board; the erasure must leave it discoverable
    // without a dangling connection count.
    let fixture = board["off_board"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["title"] == "Scratch fixture")
        .expect("related card stays discoverable");
    assert_eq!(fixture["counts"]["connections"], 0);

    // A delivered card refuses erasure and keeps the archive path open.
    let delivered = create_card(&service, &session, "Delivered handout", "artifact", "inbox").await;
    let handout = stage_upload(&service, &session, b"handout bytes\n").await;
    let bind = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": delivered, "expected_revision": 1, "expected_notes_rev": 1, "staged_uploads": [handout]}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(bind.0, 200, "{bind:?}");
    let (status, party) = request(
        &service,
        &session,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind": "person", "name": "Mira Chen", "focus": ""})),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 201);
    let party_id = party["data"]["id"].as_str().unwrap().to_owned();
    let database_path = parley_service::Workspace::resolve_database_path(&db).unwrap();
    let options = sqlx::sqlite::SqliteConnectOptions::from_str(&format!(
        "sqlite://{}",
        database_path.display()
    ))
    .unwrap()
    .foreign_keys(false);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .unwrap();
    let now = "2026-11-22T12:00:00.000Z";
    sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES('o1','Northline review','open',1,?,?)")
        .bind(now).bind(now).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO material(id,opportunity_id,recipient_id,kind,head_version,created_at,updated_at) VALUES('m1','o1',?,'outreach',2,?,?)")
        .bind(&party_id).bind(now).bind(now).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at,blob_upload_id) VALUES('m1',1,'o1',?,'t1',1,'sent body','{}','{}',?,? ,?)")
        .bind(&party_id).bind(format!("sha1{}", "0".repeat(62))).bind(now).bind(&handout)
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at) VALUES('m1',2,'o1',?,'t1',1,'draft body','{}','{}',?,?)")
        .bind(&party_id).bind(format!("sha2{}", "0".repeat(62))).bind(now)
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO version_dependency(material_id,version_no,kind,source_id,source_revision) VALUES('m1',2,'resource',?,'1')")
        .bind(&delivered).execute(&pool).await.unwrap();
    let (status, refused) = request(
        &service,
        &session,
        "POST",
        &format!("/api/v1/resource-cards/{delivered}/erase"),
        None,
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(refused["error"]["code"], "RESOURCE_DELIVERED");
    let (status, archived) = request(
        &service,
        &session,
        "POST",
        &format!("/api/v1/resource-cards/{delivered}/retire"),
        None,
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 200, "{archived}");

    // A drafted-only card refuses erasure as a drafting dependency.
    let drafted = create_card(&service, &session, "Draft source", "note", "inbox").await;
    sqlx::query("INSERT INTO version_dependency(material_id,version_no,kind,source_id,source_revision) VALUES('m1',2,'resource',?,'1')")
        .bind(&drafted)
        .execute(&pool)
        .await
        .unwrap();
    let (status, refused) = request(
        &service,
        &session,
        "POST",
        &format!("/api/v1/resource-cards/{drafted}/erase"),
        None,
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(refused["error"]["code"], "RESOURCE_IN_MATERIALS");

    // Claim evidence keeps citing its subject; erasure is refused.
    let cited = create_card(&service, &session, "Cited note", "note", "inbox").await;
    sqlx::query("INSERT INTO claim(id,subject_id,predicate,current_revision,created_at) VALUES('r03-erase-claim',?,'supports:note',1,?)")
        .bind(&cited).bind(now)
        .execute(&pool)
        .await
        .unwrap();
    let (status, refused) = request(
        &service,
        &session,
        "POST",
        &format!("/api/v1/resource-cards/{cited}/erase"),
        None,
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(refused["error"]["code"], "RESOURCE_IN_EVIDENCE");
    pool.close().await;

    // Erasing an unknown card fails closed.
    let (status, _) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-cards/resource-card-absent/erase",
        None,
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 404);

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}
