#![allow(dead_code, reason = "party slice binaries exercise different requests")]
use parley_service::RunningService;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
pub fn path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-p01-http-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

pub type Session = (String, String, String);

pub async fn owner_session(service: &RunningService) -> Session {
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
pub struct RawResponse {
    pub status: u16,
    pub body: Value,
    pub bytes: Vec<u8>,
}

pub async fn request(
    service: &RunningService,
    session: &Session,
    method: &str,
    path: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> RawResponse {
    let bytes = body.map(|v| v.to_string().into_bytes()).unwrap_or_default();
    raw_request(
        service,
        session,
        method,
        path,
        &bytes,
        "application/json",
        extra,
    )
    .await
}

pub async fn raw_request(
    service: &RunningService,
    session: &Session,
    method: &str,
    path: &str,
    bytes: &[u8],
    mime: &str,
    extra: &[(&str, &str)],
) -> RawResponse {
    let address = service.address;
    let mut headers = format!(
        "Cookie: {}\r\nX-Parley-Token: {}\r\nX-Parley-Epoch: {}",
        session.0, session.1, session.2
    );
    for (name, value) in extra {
        headers.push_str(&format!("\r\n{name}: {value}"));
    }
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{headers}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n\r\n",bytes.len()).as_bytes()).await.unwrap();
    stream.write_all(bytes).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let split = response.windows(4).position(|v| v == b"\r\n\r\n").unwrap();
    let status = std::str::from_utf8(&response[..split])
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let bytes = response[split + 4..].to_vec();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    RawResponse {
        status,
        body,
        bytes,
    }
}

pub fn next_key() -> String {
    format!("p01-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Create a person/org entity and return (id, etag).
pub async fn create_party(
    service: &RunningService,
    session: &Session,
    kind: &str,
    name: &str,
    focus: &str,
) -> (String, String) {
    let response = request(
        service,
        session,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind": kind, "name": name, "focus": focus})),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(response.status, 201, "{response:?}");
    let data = &response.body["data"];
    (
        data["id"].as_str().unwrap().to_owned(),
        data["etag"].as_str().unwrap().to_owned(),
    )
}
