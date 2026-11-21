use parley_service::{Workspace, serve};
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
fn path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-s02-http-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}
async fn post(port: u16, text: &str) -> String {
    let body=serde_json::json!({"text":text,"title":"Hiring update","url":"https://example.invalid/update"}).to_string();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(format!("POST /api/v1/captures HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nX-Parley-Token: capture-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    let mut result = String::new();
    stream.read_to_string(&mut result).await.unwrap();
    result
}

#[tokio::test]
async fn s02_source_only_capture_keeps_two_occurrences_without_identity_merge() {
    let db = path();
    let workspace = Workspace::open(&db).await.unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve(
        listener,
        workspace.clone(),
        "capture-token".into(),
        port,
    ));
    let first = post(port, "The next engineer will work with Dana on onboarding.").await;
    let second = post(
        port,
        "  THE NEXT ENGINEER WILL WORK WITH DANA ON ONBOARDING.  ",
    )
    .await;
    let first: Value = serde_json::from_str(first.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let second: Value = serde_json::from_str(second.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(first["content_id"], second["content_id"]);
    assert_ne!(first["occurrence_id"], second["occurrence_id"]);
    let independent = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{}", db.display()))
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
    server.abort();
    let _ = fs::remove_file(db);
}

#[tokio::test]
async fn s02_same_name_entities_are_distinct() {
    let db = path();
    let workspace = Workspace::open(&db).await.unwrap();
    let dana_a = workspace.create_entity("person", "Dana Lee").await.unwrap();
    let dana_b = workspace.create_entity("person", "Dana Lee").await.unwrap();
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
    let organization = workspace.create_entity("org", "Northline").await.unwrap();
    let rejected = workspace
        .create_related_entity(
            "person",
            "Invented Dana",
            &organization.id,
            "member_of",
            &source.occurrence_id,
            "This sentence is not in the source",
            "party",
            &organization.id,
            160.0,
            80.0,
        )
        .await;
    assert!(rejected.is_err());
    let untouched:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM entity),(SELECT count(*) FROM edge),(SELECT count(*) FROM view_position)").fetch_one(workspace.pool()).await.unwrap();
    assert_eq!(untouched, (1, 0, 0));
    let created = workspace
        .create_related_entity(
            "person",
            "Dana Lee",
            &organization.id,
            "member_of",
            &source.occurrence_id,
            "Dana Lee will work with the onboarding team",
            "party",
            &organization.id,
            160.0,
            80.0,
        )
        .await
        .unwrap();
    assert_eq!(created.kind, "person");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{}", db.display()))
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
    let _ = fs::remove_file(db);
}

#[tokio::test]
async fn s02_two_speakers_and_raw_variants_survive_normalized_deduplication() {
    let db = path();
    let workspace = Workspace::open(&db).await.unwrap();
    let alice = workspace
        .create_entity("person", "Alice Lee")
        .await
        .unwrap();
    let dana = workspace.create_entity("person", "Dana Lee").await.unwrap();
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
