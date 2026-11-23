use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use sqlx::sqlite::SqlitePoolOptions;
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
fn path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-s02-http-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

type Session = (String, String, String);

/// Redeem the launcher bootstrap and return the owner session headers used by
/// private commands.
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

async fn post(service: &RunningService, session: &Session, text: &str) -> String {
    let address = service.address;
    let body = serde_json::json!({"text":text,"title":"Hiring update","url":"https://example.invalid/update"}).to_string();
    let key = format!("s02-capture-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(format!("POST /api/v1/captures HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nCookie: {}\r\nX-Parley-Token: {}\r\nX-Parley-Epoch: {}\r\nIdempotency-Key: {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", session.0, session.1, session.2, body.len()).as_bytes()).await.unwrap();
    let mut result = String::new();
    stream.read_to_string(&mut result).await.unwrap();
    result
}

#[tokio::test]
async fn s02_source_only_capture_keeps_two_occurrences_without_identity_merge() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let first = post(
        &service,
        &session,
        "The next engineer will work with Dana on onboarding.",
    )
    .await;
    let second = post(
        &service,
        &session,
        "  THE NEXT ENGINEER WILL WORK WITH DANA ON ONBOARDING.  ",
    )
    .await;
    let first: Value = serde_json::from_str(first.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let second: Value = serde_json::from_str(second.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(first["content_id"], second["content_id"]);
    assert_ne!(first["occurrence_id"], second["occurrence_id"]);
    let independent = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!(
            "sqlite://{}",
            parley_service::Workspace::resolve_database_path(&db)
                .unwrap()
                .display()
        ))
        .await
        .unwrap();
    let entities: i64 = sqlx::query_scalar("SELECT count(*) FROM entity")
        .fetch_one(&independent)
        .await
        .unwrap();
    let sources: i64 = sqlx::query_scalar("SELECT count(*) FROM source_occurrence")
        .fetch_one(&independent)
        .await
        .unwrap();
    assert_eq!((entities, sources), (0, 2));
    independent.close().await;
    service.shutdown().await;
    let _ = fs::remove_file(db);
}

#[tokio::test]
async fn s02_same_name_entities_are_distinct() {
    let db = path();
    let workspace = Workspace::open(&db).await.unwrap();
    let dana_a = workspace
        .create_entity("dana-first", "person", "Dana Lee", "")
        .await
        .unwrap();
    let dana_b = workspace
        .create_entity("dana-second", "person", "Dana Lee", "")
        .await
        .unwrap();
    assert_ne!(dana_a.id, dana_b.id);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM entity WHERE name='Dana Lee'")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(count, 2);
    workspace.pool().close().await;
    let _ = fs::remove_file(db);
}

#[tokio::test]
async fn s02_related_node_edge_and_quote_commit_together() {
    let db = path();
    let workspace = Workspace::open(&db).await.unwrap();
    let raw = "Northline announced that Dana Lee will work with the onboarding team.";
    let source = workspace.save_source(raw, None, None).await.unwrap();
    let organization = workspace
        .create_entity("northline", "org", "Northline", "")
        .await
        .unwrap();
    let rejected = workspace
        .create_related_card(
            "Invented Dana",
            parley_service::relations::RelatedCard {
                kind: "person".into(),
                name: "Invented Dana".into(),
                related_entity_id: organization.id.clone(),
                verb: "member_of".into(),
                occurrence_id: source.occurrence_id.clone(),
                quote: "This sentence is not in the source".into(),
                view_scope: "party".into(),
                view_id: organization.id.clone(),
                x: 160.0,
                y: 80.0,
                expected_parent_revision: Some(1),
                direction: None,
                notes: String::new(),
                staged_upload_ids: vec![],
            },
        )
        .await;
    assert!(rejected.is_err());
    let untouched:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM entity),(SELECT count(*) FROM edge),(SELECT count(*) FROM view_position)").fetch_one(workspace.pool()).await.unwrap();
    assert_eq!(untouched, (1, 0, 0));
    let created = workspace
        .create_related_card(
            "Dana Lee",
            parley_service::relations::RelatedCard {
                kind: "person".into(),
                name: "Dana Lee".into(),
                related_entity_id: organization.id.clone(),
                verb: "member_of".into(),
                occurrence_id: source.occurrence_id.clone(),
                quote: "Dana Lee will work with the onboarding team".into(),
                view_scope: "party".into(),
                view_id: organization.id.clone(),
                x: 160.0,
                y: 80.0,
                expected_parent_revision: Some(1),
                direction: None,
                notes: String::new(),
                staged_upload_ids: vec![],
            },
        )
        .await
        .unwrap();
    assert_eq!(created.kind, "person");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!(
            "sqlite://{}",
            parley_service::Workspace::resolve_database_path(&db)
                .unwrap()
                .display()
        ))
        .await
        .unwrap();
    let counts: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM entity),(SELECT count(*) FROM edge WHERE state='active'),(SELECT count(*) FROM edge_evidence),(SELECT count(*) FROM view_position)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (2, 1, 1, 1));
    let span: (String, i64, i64) =
        sqlx::query_as("SELECT wording,source_start,source_end FROM claim_revision")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(&raw[span.1 as usize..span.2 as usize], span.0);
    pool.close().await;
    workspace.pool().close().await;
    let _ = fs::remove_file(db);
}

#[tokio::test]
async fn s02_two_speakers_and_raw_variants_survive_normalized_deduplication() {
    let db = path();
    let workspace = Workspace::open(&db).await.unwrap();
    let alice = workspace
        .create_entity("alice-lee", "person", "Alice Lee", "")
        .await
        .unwrap();
    let dana = workspace
        .create_entity("dana-lee", "person", "Dana Lee", "")
        .await
        .unwrap();
    let first = workspace
        .save_source_attributed("“Dana joined”", Some("A"), None, "party", Some(&alice.id))
        .await
        .unwrap();
    let second = workspace
        .save_source_attributed("\"DANA JOINED\"", Some("B"), None, "party", Some(&dana.id))
        .await
        .unwrap();
    assert_eq!(first.content_id, second.content_id);
    assert_ne!(first.variant_id, second.variant_id);
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT actor_kind,actor_entity_id FROM source_occurrence ORDER BY captured_at,id",
    )
    .fetch_all(workspace.pool())
    .await
    .unwrap();
    assert!(rows.contains(&("party".into(), Some(alice.id))));
    assert!(rows.contains(&("party".into(), Some(dana.id))));
    workspace.pool().close().await;
    let _ = fs::remove_file(db);
}
