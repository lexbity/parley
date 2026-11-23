// R01: Resource board — fixed columns, atomic card commits, moves,
// owner-authored edges, attachment bindings and the delivery projection
// (amendment 2026-11-22 §2–§6).
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
        "parley-r01-http-{}-{}.sqlite",
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
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let payload: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    (
        cookie,
        payload["mutation_token"].as_str().unwrap().to_owned(),
        payload["epoch"].as_str().unwrap().to_owned(),
    )
}

#[derive(Debug)]
struct RawResponse {
    status: u16,
    body: Value,
}

async fn request(
    service: &RunningService,
    session: &Session,
    method: &str,
    path: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> RawResponse {
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
    RawResponse { status, body }
}

fn next_key() -> String {
    format!("r01-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

async fn stage_upload(service: &RunningService, session: &Session, bytes: &[u8]) -> String {
    let address = service.address;
    let headers = format!(
        "Cookie: {}\r\nX-Parley-Token: {}\r\nX-Parley-Epoch: {}\r\nx-parley-filename: notes.txt\r\nContent-Type: text/plain",
        session.0, session.1, session.2
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

#[tokio::test]
async fn r01_board_seeds_columns_and_commits_a_card() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;

    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board",
        None,
        &[],
    )
    .await;
    assert_eq!(board.status, 200);
    let columns = board.body["columns"].as_array().unwrap();
    let titles: Vec<&str> = columns
        .iter()
        .map(|column| column["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Inbox", "Developing", "Available"]);
    assert!(board.body["cards"].as_array().unwrap().is_empty());

    let commit = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {
                "title": "Importer checklist",
                "subtype": "artifact",
                "body": {"scope": "csv"},
                "notes_md": "- try a multiline field\n- compare parsed rows",
                "placement": {"column_id": "developing"}
            }
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(commit.status, 201, "{commit:?}");
    let entity = &commit.body["entity"];
    let id = entity["id"].as_str().unwrap().to_owned();
    assert_eq!(entity["subtype"], "artifact");
    assert_eq!(entity["readiness"], "draft");
    assert_eq!(commit.body["placement"]["column_id"], "developing");
    assert!(commit.body["created_related"].is_null());

    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board",
        None,
        &[],
    )
    .await;
    let cards = board.body["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 1);
    let card = &cards[0];
    assert_eq!(card["resource_id"], id);
    assert_eq!(card["title"], "Importer checklist");
    assert_eq!(card["placement"]["column_id"], "developing");
    assert_eq!(card["counts"]["connections"], 0);
    assert_eq!(card["counts"]["files"], 0);

    let notes = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{id}/notes"),
        None,
        &[],
    )
    .await;
    assert_eq!(notes.status, 200);
    assert!(
        notes.body["data"]["markdown"]
            .as_str()
            .unwrap()
            .contains("compare parsed rows")
    );

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r01_card_edit_rename_guards_stale_and_rejects_blank() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let id = next_key();
    let commit = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {"title": "Draft card", "subtype": "gift", "body": {}}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    let entity = &commit.body["entity"];
    let card_id = entity["id"].as_str().unwrap().to_owned();
    let revision = entity["revision"].as_i64().unwrap();
    let notes_rev = entity["notes"]["revision"].as_i64().unwrap();

    let edit = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {
                "resource_id": card_id,
                "expected_revision": revision,
                "expected_notes_rev": notes_rev,
                "title": "Renamed card"
            }
        })),
        &[("Idempotency-Key", id.as_str())],
    )
    .await;
    assert_eq!(edit.status, 200, "{edit:?}");
    assert_eq!(edit.body["entity"]["title"], "Renamed card");
    assert_eq!(edit.body["entity"]["revision"], revision + 1);

    // Stale revision replay is rejected, not silently applied.
    let stale = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {
                "resource_id": card_id,
                "expected_revision": revision,
                "expected_notes_rev": notes_rev,
                "title": "Stale"
            }
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(stale.status, 412);

    // Blank titles and unknown subtypes never create anything.
    let blank = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {"title": "   ", "subtype": "gift", "body": {}}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(blank.status, 422);
    let bad_type = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {"title": "Ok", "subtype": "widget", "body": {}}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(bad_type.status, 422);
    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board?include_offboard=true",
        None,
        &[],
    )
    .await;
    let off = board.body["off_board"].as_array().unwrap();
    assert_eq!(off.len(), 1, "{off:?}");

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r01_move_reorder_and_remove_preserve_resource() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;

    let mut ids = Vec::new();
    for title in ["A", "B", "C"] {
        let commit = request(
            &service,
            &session,
            "POST",
            "/api/v1/resource-card-commits",
            Some(json!({
                "create": {"title": title, "subtype": "note", "body": {}, "placement": {"column_id": "inbox"}}
            })),
            &[("Idempotency-Key", next_key().as_str())],
        )
        .await;
        assert_eq!(commit.status, 201);
        ids.push(commit.body["entity"]["id"].as_str().unwrap().to_owned());
    }

    // Order within Inbox follows creation (append semantics).
    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board",
        None,
        &[],
    )
    .await;
    let inbox: Vec<&str> = board.body["cards"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|card| card["placement"]["column_id"] == "inbox")
        .map(|card| card["resource_id"].as_str().unwrap())
        .collect();
    assert_eq!(inbox, ids.iter().map(String::as_str).collect::<Vec<_>>());

    // Move B before A in the same column.
    let moved = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-board/moves",
        Some(json!({
            "resource_id": ids[1],
            "destination_column_id": "inbox",
            "before_resource_id": ids[0]
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(moved.status, 200, "{moved:?}");
    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board",
        None,
        &[],
    )
    .await;
    let inbox: Vec<&str> = board.body["cards"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|card| card["placement"]["column_id"] == "inbox")
        .map(|card| card["resource_id"].as_str().unwrap())
        .collect();
    assert_eq!(inbox, [ids[1].as_str(), ids[0].as_str(), ids[2].as_str()]);

    // Move A to Available.
    let moved = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-board/moves",
        Some(json!({
            "resource_id": ids[0],
            "destination_column_id": "available"
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(moved.status, 200);
    assert_eq!(moved.body["placement"]["column_id"], "available");

    // Remove C from the board; the resource still exists and is pickable.
    let removed = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-board/moves",
        Some(json!({
            "resource_id": ids[2],
            "destination_column_id": null
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(removed.status, 200);
    assert!(removed.body["placement"].is_null());
    let fetch = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{}", ids[2]),
        None,
        &[],
    )
    .await;
    assert_eq!(fetch.status, 200);
    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board?include_offboard=true",
        None,
        &[],
    )
    .await;
    let off: Vec<&str> = board.body["off_board"]
        .as_array()
        .unwrap()
        .iter()
        .map(|card| card["resource_id"].as_str().unwrap())
        .collect();
    assert!(off.contains(&ids[2].as_str()), "{off:?}");

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r01_related_card_offboard_duplicate_and_detach() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let root = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {"title": "Checklist", "subtype": "artifact", "body": {}, "placement": {"column_id": "developing"}}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    let root_id = root.body["entity"]["id"].as_str().unwrap().to_owned();

    // Create a related card from the graph editor: child is off board and its
    // edge is created atomically with the parent commit.
    let related = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {
                "resource_id": root_id,
                "expected_revision": 1,
                "expected_notes_rev": 1
            },
            "relation_add": {
                "verb": "includes",
                "create": {"title": "Fixture walkthrough", "subtype": "artifact", "body": {}},
                "basis": "Owner typo the checklist"
            }
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(related.status, 200, "{related:?}");
    let child_id = related.body["created_related"]["to_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let edge_id = related.body["created_related"]["edge_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board?include_offboard=true",
        None,
        &[],
    )
    .await;
    let off: Vec<&str> = board.body["off_board"]
        .as_array()
        .unwrap()
        .iter()
        .map(|card| card["resource_id"].as_str().unwrap())
        .collect();
    assert!(off.iter().any(|id| *id == child_id), "{off:?}");
    let cards = board.body["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0]["counts"]["connections"], 1);

    let graph = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/graph?center_id={root_id}&view_scope=resources"),
        None,
        &[],
    )
    .await;
    assert_eq!(graph.status, 200, "{graph:?}");
    assert_eq!(graph.body["edges"][0]["id"], edge_id);
    assert_eq!(graph.body["edges"][0]["owner_authored"], true);
    assert_eq!(graph.body["edges"][0]["quote"], "Owner typo the checklist");

    // Duplicate link is rejected; a second distinct edge is allowed.
    let dup = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": root_id, "expected_revision": 1, "expected_notes_rev": 1},
            "relation_add": {"verb": "includes", "target_id": child_id, "basis": "again"}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(dup.status, 422);

    // Detaching the edge keeps both endpoints.
    let detach = request(
        &service,
        &session,
        "DELETE",
        &format!("/api/v1/edges/{edge_id}"),
        Some(json!({"expected_revision": 1})),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(detach.status, 204);
    let child = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{child_id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(child.status, 200);

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r01_attachment_bind_unbind_content_roundtrip() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let commit = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {"title": "Card with file", "subtype": "proof", "body": {}, "placement": {"column_id": "inbox"}}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    let card_id = commit.body["entity"]["id"].as_str().unwrap().to_owned();
    let upload_id = stage_upload(&service, &session, b"verified bytes\n").await;

    // Bind on save, then read the bound content.
    let bind_commit = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {
                "resource_id": card_id,
                "expected_revision": 1,
                "expected_notes_rev": 1,
                "staged_uploads": [upload_id]
            }
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(bind_commit.status, 200, "{bind_commit:?}");
    let list = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{card_id}/attachments"),
        None,
        &[],
    )
    .await;
    assert_eq!(list.status, 200);
    let attachments = list.body["data"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    let ref_id = attachments[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(attachments[0]["filename"], "notes.txt");
    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board",
        None,
        &[],
    )
    .await;
    assert_eq!(board.body["cards"][0]["counts"]["files"], 1);

    // An upload cannot be bound twice.
    let double = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {"resource_id": card_id, "expected_revision": 2, "expected_notes_rev": 1, "staged_uploads": [upload_id]}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(double.status, 422);

    // Unbind removes the card binding but keeps the resource.
    let unbind = request(
        &service,
        &session,
        "DELETE",
        &format!("/api/v1/entities/{card_id}/attachments/{ref_id}"),
        None,
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(unbind.status, 204);
    let list = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{card_id}/attachments"),
        None,
        &[],
    )
    .await;
    assert!(list.body["data"].as_array().unwrap().is_empty());

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r01_delivery_projection_counts_only_sent_versions() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let upload_id = stage_upload(&service, &session, b"Included checklist").await;
    let commit = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {"title": "Checklist v1", "subtype": "artifact", "body": {}, "placement": {"column_id": "available"}, "staged_uploads": [upload_id]}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    let resource_id = commit.body["entity"]["id"].as_str().unwrap().to_owned();
    let party = request(
        &service,
        &session,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind": "person", "name": "Mira Chen", "focus": ""})),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    let party_id = party.body["data"]["id"].as_str().unwrap().to_owned();

    // Seed a delivered material version and a drafting-only version directly.
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
    sqlx::query("INSERT INTO material(id,opportunity_id,recipient_id,kind,head_version,created_at,updated_at) VALUES('m1','o1',?,'outreach',2,?,?)")
        .bind(&party_id)
        .bind(now)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
    for (version, body) in [(1, "v1 body"), (2, "v2 body")] {
        sqlx::query("INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at,blob_upload_id) VALUES('m1',?, 'o1',?, 't1',1,?, '{}','{}',?,?,?)")
            .bind(version)
            .bind(&party_id)
            .bind(body)
            .bind(format!("sha{version}{}", "0".repeat(62)))
            .bind(now)
            .bind((version == 1).then_some(&upload_id))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO version_dependency(material_id,version_no,kind,source_id,source_revision) VALUES('m1',?,'resource',?, '1')")
            .bind(version)
            .bind(&resource_id)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO send(id,thread_id,touch_no,primary_material_id,primary_version_no,recipient_id,gate_snapshot_json,overrides_json,content_sha256,sent_at,response_deadline,deadline_zone,capacity_charged) VALUES('s1','t1',1,'m1',1,?,'{}','{}','sha10','2026-11-18T09:00:00.000Z','2026-11-25T09:00:00.000Z','UTC',1)")
        .bind(&party_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO send_part(send_id,material_id,version_no,release_id,kind) VALUES('s1','m1',1,'r1','primary')")
        .execute(&pool)
        .await
        .unwrap();

    // v1 was sent (one delivery); v2 was only drafted (no send).
    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board",
        None,
        &[],
    )
    .await;
    let card = board.body["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["resource_id"] == resource_id)
        .unwrap();
    assert_eq!(card["counts"]["deliveries"], 1);
    assert_eq!(card["counts"]["files"], 1);

    let binding_id: String =
        sqlx::query_scalar("SELECT id FROM entity_attachment WHERE entity_id=?")
            .bind(&resource_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let edit = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit":{"resource_id":resource_id,"expected_revision":1,"expected_notes_rev":1,
                    "title":"Checklist v2", "unbind_ids":[binding_id]}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(edit.status, 200, "{edit:?}");

    let historical = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/resource-cards/{resource_id}/deliveries/s1/m1/1"),
        None,
        &[],
    )
    .await;
    assert_eq!(historical.status, 200, "{historical:?}");
    assert_eq!(historical.body["resource_title"], "Checklist v1");
    assert_eq!(historical.body["body"], "v1 body");
    let unsent = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/resource-cards/{resource_id}/deliveries/s1/m1/2"),
        None,
        &[],
    )
    .await;
    assert_eq!(unsent.status, 404);

    pool.close().await;
    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r01_pagination_and_failed_create_preserve_board_membership() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let upload_id = stage_upload(&service, &session, b"Retry this attachment").await;
    let failing = request(&service,&session,"POST","/api/v1/resource-card-commits",Some(json!({
        "create":{"title":"Rolled back","subtype":"note","staged_uploads":[upload_id],"placement":{"column_id":"missing"}}
    })),&[("Idempotency-Key",next_key().as_str())]).await;
    assert_ne!(failing.status, 201);
    for title in ["First", "Second", "Third"] {
        let created = request(&service,&session,"POST","/api/v1/resource-card-commits",Some(json!({
            "create":{"title":title,"subtype":"note","staged_uploads":if title == "First" { vec![&upload_id] } else { vec![] },"placement":{"column_id":"inbox"}}
        })),&[("Idempotency-Key",next_key().as_str())]).await;
        assert_eq!(created.status, 201, "{created:?}");
    }
    let mut url = "/api/v1/resource-board?limit=1&include_offboard=true".to_owned();
    for title in ["First", "Second", "Third"] {
        let page = request(&service, &session, "GET", &url, None, &[]).await;
        assert_eq!(page.status, 200, "{page:?}");
        assert_eq!(page.body["cards"][0]["title"], title);
        assert_eq!(page.body["off_board"], json!([]));
        if title != "Third" {
            let cursor = page.body["next_cursor"]
                .as_str()
                .unwrap()
                .replace('|', "%7C");
            url = format!("/api/v1/resource-board?limit=1&cursor={cursor}");
        } else {
            assert!(page.body["next_cursor"].is_null());
        }
    }
    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r01_legacy_resources_stay_offboard_and_replay_returns_same_card() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;

    let legacy = request(
        &service,
        &session,
        "POST",
        "/api/v1/entities",
        Some(json!({
            "kind": "gift",
            "title": "Legacy offer",
            "body": {"delivery": "on_request", "description": "help"}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(legacy.status, 201);
    let legacy_id = legacy.body["data"]["id"].as_str().unwrap().to_owned();

    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board?include_offboard=true",
        None,
        &[],
    )
    .await;
    let legacy_card = board.body["off_board"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["resource_id"] == legacy_id)
        .expect("legacy resource appears in the picker");
    // The merged model mirrors legacy resources into entity rows, so the
    // picker entry may be entity-kind; it must exist exactly once.
    assert!(legacy_card["resource_id"] == legacy_id);

    // Replaying the same operation key returns the same card, never a second.
    let key = next_key();
    let first = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {"title": "Stable card", "subtype": "skill", "body": {}}
        })),
        &[("Idempotency-Key", key.as_str())],
    )
    .await;
    let second = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {"title": "Stable card", "subtype": "skill", "body": {}}
        })),
        &[("Idempotency-Key", key.as_str())],
    )
    .await;
    assert_eq!(first.body["entity"]["id"], second.body["entity"]["id"]);
    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board?include_offboard=true",
        None,
        &[],
    )
    .await;
    let stable: Vec<&str> = board.body["off_board"]
        .as_array()
        .unwrap()
        .iter()
        .map(|card| card["resource_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        stable
            .iter()
            .filter(|id| **id == first.body["entity"]["id"].as_str().unwrap())
            .count(),
        1,
        "{stable:?}"
    );

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn resource_lists_replay_create_and_remove_with_current_revision() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let create_key = next_key();
    let create = [("Idempotency-Key", create_key.as_str())];
    let lists = "/api/v1/resource-board/columns";
    let input = json!({"title": " Workshop "});
    let list = request(
        &service,
        &session,
        "POST",
        lists,
        Some(input.clone()),
        &create,
    )
    .await;
    assert_eq!(list.status, 201);
    assert_eq!(list.body["title"], "Workshop");
    let id = list.body["id"].as_str().unwrap();
    let replay = request(&service, &session, "POST", lists, Some(input), &create).await;
    assert_eq!(replay.status, 201);
    assert_eq!(replay.body, list.body);

    let card = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({"create": {"title": "Handout", "subtype": "artifact", "placement": {"column_id": id}}})),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(card.status, 201);
    let resource_id = card.body["entity"]["id"].as_str().unwrap();
    let list_path = format!("{lists}/{id}");
    let stale = request(
        &service,
        &session,
        "DELETE",
        &list_path,
        Some(json!({"expected_revision": 1})),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(stale.status, 412);

    let remove_key = next_key();
    let remove = [("Idempotency-Key", remove_key.as_str())];
    let input = json!({"expected_revision": 2});
    let removed = request(
        &service,
        &session,
        "DELETE",
        &list_path,
        Some(input.clone()),
        &remove,
    )
    .await;
    assert_eq!(removed.status, 200);
    assert_eq!(removed.body, json!({"id": id, "off_board_count": 1}));
    let replay = request(
        &service,
        &session,
        "DELETE",
        &list_path,
        Some(input),
        &remove,
    )
    .await;
    assert_eq!(replay.status, 200);
    assert_eq!(replay.body, removed.body);

    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board?include_offboard=true",
        None,
        &[],
    )
    .await;
    assert_eq!(board.status, 200);
    assert!(
        !board.body["columns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|column| column["id"] == id)
    );
    assert!(
        board.body["off_board"]
            .as_array()
            .unwrap()
            .iter()
            .any(|card| card["resource_id"] == resource_id)
    );
    assert_eq!(board.body["cards"], json!([]));

    service.shutdown().await;
    let _ = fs::remove_file(db);
}

#[tokio::test]
async fn resource_lists_rename_updates_title_with_revision_guard() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;

    // Only the seeded Inbox remains a system list (amendment 0022).
    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board",
        None,
        &[],
    )
    .await;
    assert_eq!(board.status, 200);
    let kinds: Vec<&str> = board.body["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|column| column["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["system", "owner", "owner"]);

    let create_key = next_key();
    let create = [("Idempotency-Key", create_key.as_str())];
    let lists = "/api/v1/resource-board/columns";
    let input = json!({"title": " Workshop "});
    let list = request(
        &service,
        &session,
        "POST",
        lists,
        Some(input.clone()),
        &create,
    )
    .await;
    assert_eq!(list.status, 201);
    assert_eq!(list.body["kind"], "owner");
    let id = list.body["id"].as_str().unwrap().to_owned();

    let list_path = format!("{lists}/{id}");
    let rename_key = next_key();
    let rename = [("Idempotency-Key", rename_key.as_str())];
    let input = json!({"title": " Lab Notes ", "expected_revision": 1});
    let renamed = request(
        &service,
        &session,
        "PATCH",
        &list_path,
        Some(input.clone()),
        &rename,
    )
    .await;
    assert_eq!(renamed.status, 200, "{}", renamed.body);
    assert_eq!(renamed.body["title"], "Lab Notes");
    assert_eq!(renamed.body["revision"], 2);
    assert_eq!(renamed.body["kind"], "owner");
    let replay = request(
        &service,
        &session,
        "PATCH",
        &list_path,
        Some(input),
        &rename,
    )
    .await;
    assert_eq!(replay.status, 200);
    assert_eq!(replay.body, renamed.body);

    // Stale guard, validation and missing lists fail closed.
    let stale = request(
        &service,
        &session,
        "PATCH",
        &list_path,
        Some(json!({"title": "Other", "expected_revision": 1})),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(stale.status, 412);
    let blank = request(
        &service,
        &session,
        "PATCH",
        &list_path,
        Some(json!({"title": "   ", "expected_revision": 2})),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(blank.status, 422);
    let missing = request(
        &service,
        &session,
        "PATCH",
        &format!("{lists}/resource-column-absent"),
        Some(json!({"title": "Other", "expected_revision": 1})),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(missing.status, 404);

    // A placed card bumps the column revision; renaming still guards on the
    // live revision, and the board summary exposes the card revision.
    let card = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({"create": {"title": "Handout", "subtype": "artifact", "placement": {"column_id": id}}})),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(card.status, 201);
    let resource_id = card.body["entity"]["id"].as_str().unwrap().to_owned();
    let board = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board",
        None,
        &[],
    )
    .await;
    let summary = board.body["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["resource_id"] == resource_id.as_str())
        .expect("placed card on board")
        .clone();
    assert_eq!(summary["revision"], 1);
    let revision = board.body["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|column| column["id"] == id.as_str())
        .map(|column| column["revision"].as_i64().unwrap())
        .unwrap();
    let renamed = request(
        &service,
        &session,
        "PATCH",
        &list_path,
        Some(json!({"title": "Lab Notes v2", "expected_revision": revision})),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(renamed.status, 200, "{}", renamed.body);
    assert_eq!(renamed.body["title"], "Lab Notes v2");

    service.shutdown().await;
    let _ = fs::remove_file(db);
}
