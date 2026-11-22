use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub struct Client {
    pub address: std::net::SocketAddr,
    pub cookie: String,
    pub token: String,
    pub epoch: String,
    next_key: u64,
}

impl Client {
    pub async fn new(service: &parley_service::RunningService) -> Self {
        let address = service.address;
        let response = request_raw(
            address,
            "POST",
            "/api/v1/session/bootstrap",
            &[],
            &serde_json::json!({"secret":service.bootstrap_secret}).to_string(),
        )
        .await;
        assert_eq!(response.0, 200, "{}", response.1);
        let cookie = response
            .1
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
            .unwrap()
            .split_once(':')
            .unwrap()
            .1
            .trim()
            .split(';')
            .next()
            .expect("session cookie value")
            .to_owned();
        let session = request_raw(
            address,
            "GET",
            "/api/v1/session",
            &[("Cookie", cookie.as_str())],
            "",
        )
        .await;
        assert_eq!(session.0, 200, "{}", session.1);
        Self {
            address,
            cookie,
            token: session.2["mutation_token"].as_str().unwrap().to_owned(),
            epoch: session.2["epoch"].as_str().unwrap().to_owned(),
            next_key: 0,
        }
    }

    pub async fn call(&mut self, method: &str, path: &str, body: Value) -> (u16, Value) {
        self.next_key += 1;
        let key = format!("integration-{}", self.next_key);
        let text = body.to_string();
        let headers = [
            ("Cookie", self.cookie.as_str()),
            ("X-Parley-Token", self.token.as_str()),
            ("X-Parley-Epoch", self.epoch.as_str()),
            ("Idempotency-Key", key.as_str()),
        ];
        let (status, _, body) = request_raw(self.address, method, path, &headers, &text).await;
        (status, body)
    }

    pub async fn get(&self, path: &str) -> (u16, Value) {
        let headers = [
            ("Cookie", self.cookie.as_str()),
            ("X-Parley-Token", self.token.as_str()),
            ("X-Parley-Epoch", self.epoch.as_str()),
        ];
        let (status, _, body) = request_raw(self.address, "GET", path, &headers, "").await;
        (status, body)
    }
}

pub async fn request_raw(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String, Value) {
    let mut stream = TcpStream::connect(address).await.expect("connect service");
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.expect("read response");
    let response = String::from_utf8(bytes).expect("UTF-8 HTTP response");
    let status = response
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let body = response.split_once("\r\n\r\n").unwrap().1;
    let payload = serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_owned()));
    (status, response, payload)
}
