// R02: eligibility/gaps on board summaries, derived delivery records and
// release-recheck signaling after resource edits (amendment §7 lifecycle and
// delivery distinctions).
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
        "parley-r02-http-{}-{}.sqlite",
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
    format!("r02-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

async fn create_card(
    service: &RunningService,
    session: &Session,
    title: &str,
    subtype: &str,
    body: Value,
    column: &str,
) -> String {
    let (status, value) = request(
        service,
        session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "create": {"title": title, "subtype": subtype, "body": body, "placement": {"column_id": column}}
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 201, "{value}");
    value["entity"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn r02_board_summaries_report_typed_gaps_and_eligibility() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;

    // An unfinished proof placed in Available is still ineligible.
    let proof_id = create_card(
        &service,
        &session,
        "Unfinished proof",
        "proof",
        json!({}),
        "available",
    )
    .await;
    // A complete artifact has no gaps.
    let artifact_id = create_card(
        &service,
        &session,
        "Complete artifact",
        "artifact",
        json!({"reference": "https://example.test/importer", "share_permission": true}),
        "developing",
    )
    .await;

    let (status, board) = request(
        &service,
        &session,
        "GET",
        "/api/v1/resource-board",
        None,
        &[],
    )
    .await;
    assert_eq!(status, 200);
    let cards = board["cards"].as_array().unwrap();
    let proof = cards
        .iter()
        .find(|card| card["resource_id"] == proof_id)
        .unwrap();
    let gaps: Vec<&str> = proof["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g.as_str().unwrap())
        .collect();
    assert!(gaps.contains(&"Result"), "{gaps:?}");
    assert!(gaps.contains(&"Context"));
    assert_eq!(proof["readiness"], "draft");
    assert_eq!(proof["placement"]["column_id"], "available");
    let artifact = cards
        .iter()
        .find(|card| card["resource_id"] == artifact_id)
        .unwrap();
    assert!(
        artifact["gaps"].as_array().unwrap().is_empty(),
        "{artifact:?}"
    );

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r02_drafting_dependencies_do_not_establish_delivery() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let resource_id = create_card(
        &service,
        &session,
        "Checklist v1",
        "artifact",
        json!({}),
        "available",
    )
    .await;
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
    for (version, body) in [(1, "v1 body"), (2, "v2 body")] {
        sqlx::query("INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at) VALUES('m1',?, 'o1',?, 't1',1,?, '{}','{}',? ,?)")
            .bind(version).bind(&party_id).bind(body).bind(format!("sha{version}{}", "0".repeat(62))).bind(now)
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO version_dependency(material_id,version_no,kind,source_id,source_revision) VALUES('m1',?,'resource',?, '1')")
            .bind(version).bind(&resource_id).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO send(id,thread_id,touch_no,primary_material_id,primary_version_no,recipient_id,gate_snapshot_json,overrides_json,content_sha256,sent_at,response_deadline,deadline_zone,capacity_charged) VALUES('s1','t1',1,'m1',1,?,'{}','{}','sha10','2026-11-18T09:00:00.000Z','2026-11-25T09:00:00.000Z','UTC',1)")
        .bind(&party_id).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO send_part(send_id,material_id,version_no,release_id,kind) VALUES('s1','m1',1,'r1','primary')")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO contact_thread(id,opportunity_id,recipient_id,state,current_touch,created_at,updated_at) VALUES('t1','o1',?,'waiting',1,?,?)")
        .bind(&party_id).bind(now).bind(now).execute(&pool).await.unwrap();

    let (status, list) = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/resource-cards/{resource_id}/deliveries"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, 200, "{list}");
    let rows = list["data"].as_array().unwrap();
    assert!(
        rows.is_empty(),
        "Drafting references are not deliveries: {rows:?}"
    );
    let (status, graph) = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/graph?center_id={resource_id}&view_scope=resources"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, 200, "{graph}");
    let edges = graph["edges"].as_array().unwrap();
    assert!(edges.iter().any(|edge| edge["verb"] == "used to prepare"));
    assert!(edges.iter().all(|edge| edge["verb"] != "shared with"));

    pool.close().await;
    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn r02_resource_edit_signals_release_recheck_for_affected_versions() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let resource_id = create_card(
        &service,
        &session,
        "Checklist",
        "artifact",
        json!({}),
        "developing",
    )
    .await;

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
    sqlx::query("INSERT INTO material(id,opportunity_id,recipient_id,kind,head_version,created_at,updated_at) VALUES('m1','o1','p1','outreach',1,?,?)")
        .bind(now).bind(now).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at) VALUES('m1',1,'o1','p1','t1',1,'body','{}','{}',?,?)")
        .bind(format!("sha{}", "0".repeat(62))).bind(now).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO version_dependency(material_id,version_no,kind,source_id,source_revision) VALUES('m1',1,'resource',?, '1')")
        .bind(&resource_id).execute(&pool).await.unwrap();

    // Read the current revision for a correct edit precondition.
    let (_, card) = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/resource-cards/{resource_id}"),
        None,
        &[],
    )
    .await;
    let revision = card["data"]["revision"].as_i64().unwrap();
    let notes_rev = card["data"]["notes"]["revision"].as_i64().unwrap();

    let (status, result) = request(
        &service,
        &session,
        "POST",
        "/api/v1/resource-card-commits",
        Some(json!({
            "edit": {
                "resource_id": resource_id,
                "expected_revision": revision,
                "expected_notes_rev": notes_rev,
                "title": "Checklist v2"
            }
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(status, 200, "{result}");
    let affected = result["affected_versions"].as_array().unwrap();
    assert_eq!(affected.len(), 1, "{affected:?}");
    assert_eq!(affected[0]["material_id"], "m1");
    assert_eq!(affected[0]["version_no"], 1);

    // The recheck signal is an observable event.
    let (status, events) = request(
        &service,
        &session,
        "GET",
        "/api/v1/events?after=0",
        None,
        &[],
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        events["items"].as_array().unwrap().iter().any(|event| {
            event["event_type"] == "material.release_recheck_required" && event["entity_id"] == "m1"
        }),
        "{events}"
    );

    pool.close().await;
    service.shutdown().await;
    let _ = fs::remove_file(&db);
}
