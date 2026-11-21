use parley_service::{Workspace, serve};
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use std::{
    fs, io,
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
        "parley-s01-http-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn request(
    port: u16,
    method: &str,
    route: &str,
    headers: &str,
    body: &str,
) -> io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream.write_all(format!("{method} {route} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    Ok(response)
}

async fn disconnect_after_write(port: u16, headers: &str, body: &str) -> io::Result<()> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream.write_all(format!("POST /api/v1/resources HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn s01_lost_ack_retry_returns_one_durable_resource() {
    let db = path();
    let ws = Workspace::open(&db).await.unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve(
        listener,
        ws.clone(),
        "secret-test-token".into(),
        port,
    ));
    let body = r#"{"kind":"gift","title":"Incomplete offer","body":{"what":"A walkthrough"}}"#;
    disconnect_after_write(port,"X-Parley-Token: secret-test-token\r\nIdempotency-Key: request-1\r\nContent-Type: application/json\r\n",body).await.unwrap();
    for _ in 0..100 {
        let committed: i64 = sqlx::query_scalar("SELECT count(*) FROM resource")
            .fetch_one(ws.pool())
            .await
            .unwrap();
        if committed == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let retry=request(port,"POST","/api/v1/resources","X-Parley-Token: secret-test-token\r\nIdempotency-Key: request-1\r\nContent-Type: application/json\r\n",body).await.unwrap();
    let retry_json: Value = serde_json::from_str(retry.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert!(retry.starts_with("HTTP/1.1 201"));
    assert_eq!(retry_json["title"], "Incomplete offer");
    let independent = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{}", db.display()))
        .await
        .unwrap();
    for (table, query) in [
        ("resource", "SELECT count(*) FROM resource"),
        ("event", "SELECT count(*) FROM event"),
        (
            "idempotency_receipt",
            "SELECT count(*) FROM idempotency_receipt",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(query)
            .fetch_one(&independent)
            .await
            .unwrap();
        assert_eq!(count, 1, "{table} must have one committed row");
    }
    independent.close().await;
    server.abort();
    let _ = fs::remove_file(db);
}

#[tokio::test]
async fn s01_unauthenticated_read_discloses_no_resource_fields() {
    let db = path();
    let ws = Workspace::open(&db).await.unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve(listener, ws, "another-secret".into(), port));
    let response = request(port, "GET", "/api/v1/resources/any", "", "")
        .await
        .unwrap();
    assert!(response.starts_with("HTTP/1.1 401"));
    assert!(!response.contains("title"));
    assert!(!response.contains("body"));
    server.abort();
    let _ = fs::remove_file(db);
}
