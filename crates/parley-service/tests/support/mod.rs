//! Test-only helpers for the U01/U02 command wire boundary.
//!
//! These helpers call the production validation entry points directly. They
//! do not reimplement parsing, digests, schema checks or provider transport.

#![allow(dead_code)]

#[path = "model_server.rs"]
pub mod model_server;

use serde_json::{Value, json};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// A deterministic loopback provider that answers one scripted response per
/// accepted connection and records every request body.
pub struct ScriptedProvider {
    pub port: u16,
    pub requests: Arc<std::sync::Mutex<Vec<String>>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ScriptedProvider {
    pub async fn start(bodies: Vec<Value>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let task = tokio::spawn(async move {
            for body in bodies {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if stream.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    head.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&head).to_string();
                let length: usize = text
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .and_then(|(_, value)| value.trim().parse().ok())
                    })
                    .unwrap_or(0);
                let mut payload = vec![0u8; length];
                let _ = stream.read_exact(&mut payload).await;
                seen.lock()
                    .unwrap()
                    .push(format!("{text}{}", String::from_utf8_lossy(&payload)));
                let body = body.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Self {
            port,
            requests,
            task: Some(task),
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn endpoint(&self) -> std::net::SocketAddr {
        format!("127.0.0.1:{}", self.port).parse().unwrap()
    }

    pub fn recorded(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for ScriptedProvider {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Wire cases that must be rejected by production validation.
pub fn invalid_wire_cases() -> Vec<Vec<u8>> {
    vec![
        b"{\"version\":1,\"version\":2}".to_vec(),
        format!(
            "{{\"args\":{}}}",
            "{\"a\":".repeat(17) + "1" + &"}".repeat(17)
        )
        .into_bytes(),
        b"{\"id\":\"party.get_summary\",\"args\":{\"party_id\":123}}".to_vec(),
        b"[]".to_vec(),
        b"{\"args\":NaN}".to_vec(),
    ]
}

/// Valid wire cases: Unicode, alias-like names and a canonical object.
pub fn valid_wire_cases() -> Vec<Vec<u8>> {
    vec![
        json!({"id":"party.get_summary","args":{"party_id":"Ω-42"}}).to_string().into_bytes(),
        json!({"id":"commands__search","query":"阶段"}).to_string().into_bytes(),
        json!({"opportunity_id":"opp_1","expected_phase_revision":1,"expected_pack_id":"job_search","expected_pack_version":"1.0.0","stage_id":"exploring"}).to_string().into_bytes(),
    ]
}
