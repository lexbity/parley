use std::{
    fs::DirBuilder,
    io,
    net::{IpAddr, SocketAddr},
    os::unix::fs::DirBuilderExt as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, Method, Request, Response, StatusCode, header, request::Parts},
};
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::{body::Incoming, client::conn, server, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::{
    net::{TcpStream, UnixListener},
    task::JoinHandle,
};

use super::protocol::parse_json;

pub const AUTHORITY: &str = "parley-provider.invalid";
const MAX_HEADERS: usize = 16 * 1024;
const MAX_HTTP_BUFFER: usize = 32 * 1024;
const MAX_REQUEST: usize = 512 * 1024;
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const RESPONSE_CHUNK: usize = 16 * 1024;

pub fn is_allowed_endpoint(endpoint: SocketAddr) -> bool {
    endpoint.port() != 0
        && matches!(
            endpoint.ip(),
            IpAddr::V4(ip) if ip == std::net::Ipv4Addr::LOCALHOST
        )
        || endpoint.port() != 0
            && matches!(
                endpoint,
                SocketAddr::V6(v6) if *v6.ip() == std::net::Ipv6Addr::LOCALHOST
                    && v6.scope_id() == 0 && v6.flowinfo() == 0
            )
}

struct Policy {
    endpoint: SocketAddr,
    model: String,
    max_output_tokens: u32,
    stage: AtomicU8,
}

pub struct Relay {
    directory: PathBuf,
    task: JoinHandle<()>,
}

impl Relay {
    pub fn bind(endpoint: SocketAddr, model: String, max_output_tokens: u32) -> io::Result<Self> {
        if !is_allowed_endpoint(endpoint) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid relay upstream",
            ));
        }

        let directory = PathBuf::from(format!("/tmp/parley-agent-{:032x}", rand::random::<u128>()));
        DirBuilder::new().mode(0o700).create(&directory)?;
        let listener = match UnixListener::bind(directory.join("provider.sock")) {
            Ok(listener) => listener,
            Err(error) => {
                let _ = std::fs::remove_dir(&directory);
                return Err(error);
            }
        };
        let policy = Arc::new(Policy {
            endpoint,
            model,
            max_output_tokens,
            stage: AtomicU8::new(0),
        });
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let policy = policy.clone();
                let service = service_fn(move |request| {
                    let policy = policy.clone();
                    async move {
                        let response = forward(request, policy).await.unwrap_or_else(|error| {
                            Response::builder()
                                .status(error)
                                .body(Body::empty())
                                .expect("relay error status")
                        });
                        Ok::<_, std::convert::Infallible>(response)
                    }
                });
                let _ = server::conn::http1::Builder::new()
                    .keep_alive(false)
                    .max_headers(48)
                    .max_buf_size(MAX_HTTP_BUFFER)
                    .timer(TokioTimer::new())
                    .header_read_timeout(Duration::from_secs(2))
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            }
        });
        Ok(Self { directory, task })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub async fn close(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(self.directory.join("provider.sock"));
        let _ = std::fs::remove_dir(&self.directory);
    }
}

fn headers_within_budget(headers: &HeaderMap) -> bool {
    headers.iter().fold(0, |len, (name, value)| {
        len + name.as_str().len() + value.as_bytes().len() + 4
    }) <= MAX_HEADERS
}

fn single_header(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(first)
}

fn allowed_framing(headers: &HeaderMap, allow_chunked: bool) -> bool {
    if headers.contains_key(header::CONTENT_ENCODING)
        && single_header(headers, header::CONTENT_ENCODING) != Some("identity")
    {
        return false;
    }
    if headers.contains_key(header::CONTENT_LENGTH)
        && single_header(headers, header::CONTENT_LENGTH)
            .and_then(|value| value.parse::<u64>().ok())
            .is_none()
    {
        return false;
    }
    !headers.contains_key(header::TRANSFER_ENCODING)
        || (allow_chunked
            && !headers.contains_key(header::CONTENT_LENGTH)
            && single_header(headers, header::TRANSFER_ENCODING) == Some("chunked"))
}

fn validate_request(parts: &Parts) -> Result<&str, StatusCode> {
    let headers = &parts.headers;
    if !headers_within_budget(headers)
        || single_header(headers, header::HOST) != Some(AUTHORITY)
        || headers.contains_key(header::UPGRADE)
        || headers.contains_key(header::EXPECT)
        || headers.contains_key(header::AUTHORIZATION)
        || headers.contains_key(header::COOKIE)
        || !allowed_framing(headers, false)
        || parts.uri.authority().is_some()
        || parts.uri.scheme().is_some()
        || parts.uri.query().is_some()
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let path = parts.uri.path();
    match (&parts.method, path) {
        (&Method::GET, "/api/tags" | "/api/version") => Ok(path),
        (&Method::POST, "/v1/chat/completions")
            if single_header(headers, header::CONTENT_TYPE) == Some("application/json") =>
        {
            Ok(path)
        }
        _ => Err(StatusCode::FORBIDDEN),
    }
}

fn valid_chat(body: &[u8], policy: &Policy) -> bool {
    let Ok(serde_json::Value::Object(request)) = parse_json(body) else {
        return false;
    };
    let fields = [
        "model",
        "messages",
        "stream",
        "stream_options",
        "max_tokens",
        "temperature",
        "response_format",
    ];
    let messages = request
        .get("messages")
        .and_then(serde_json::Value::as_array);
    request.len() == fields.len()
        && fields.iter().all(|field| request.contains_key(*field))
        && request["model"].as_str() == Some(&policy.model)
        && request["stream"].as_bool() == Some(true)
        && request["temperature"].as_f64() == Some(0.0)
        && request["max_tokens"].as_u64() == Some(u64::from(policy.max_output_tokens))
        && request["stream_options"] == serde_json::json!({"include_usage":true})
        && request["response_format"]["type"].as_str() == Some("json_schema")
        && messages.is_some_and(|messages| {
            messages.len() == 2
                && ["system", "user"]
                    .into_iter()
                    .zip(messages)
                    .all(|(role, message)| {
                        message.as_object().is_some_and(|fields| fields.len() == 2)
                            && message["role"].as_str() == Some(role)
                            && message["content"].is_string()
                    })
        })
}

struct AbortConnection(JoinHandle<Result<(), hyper::Error>>);

impl Drop for AbortConnection {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn forward(
    request: Request<Incoming>,
    policy: Arc<Policy>,
) -> Result<Response<Body>, StatusCode> {
    let (parts, incoming) = request.into_parts();
    let path = validate_request(&parts)?;
    let body = Limited::new(incoming, MAX_REQUEST)
        .collect()
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    if body.trailers().is_some() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let bytes = body.to_bytes();
    let stage = policy.stage.load(Ordering::Relaxed);
    let expected_path = match stage {
        0 | 3 => "/api/version",
        1 | 4 => "/api/tags",
        2 => "/v1/chat/completions",
        _ => return Err(StatusCode::FORBIDDEN),
    };
    if path != expected_path
        || (path == "/v1/chat/completions" && !valid_chat(&bytes, &policy))
        || (parts.method == Method::GET && !bytes.is_empty())
    {
        return Err(StatusCode::FORBIDDEN);
    }
    policy.stage.store(stage + 1, Ordering::Relaxed);

    let stream = TcpStream::connect(policy.endpoint)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let (mut sender, connection) = conn::http1::Builder::new()
        .max_headers(48)
        .max_buf_size(MAX_HTTP_BUFFER)
        .handshake(TokioIo::new(stream))
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let connection = AbortConnection(tokio::spawn(connection));
    let mut outgoing = Request::builder()
        .uri(path)
        .method(parts.method.clone())
        .header(header::HOST, policy.endpoint.to_string())
        .header(header::CONNECTION, "close")
        .header(header::ACCEPT_ENCODING, "identity");
    if path == "/v1/chat/completions" {
        outgoing = outgoing.header(header::CONTENT_TYPE, "application/json");
    }
    let upstream = sender
        .send_request(outgoing.body(Full::new(bytes)).expect("validated request"))
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    if !upstream.status().is_success()
        || !headers_within_budget(upstream.headers())
        || !allowed_framing(upstream.headers(), true)
        || upstream.headers().contains_key(header::UPGRADE)
        || upstream.headers().contains_key(header::LOCATION)
    {
        return Err(StatusCode::BAD_GATEWAY);
    }
    let content_type = single_header(upstream.headers(), header::CONTENT_TYPE)
        .filter(|value| {
            let expected = if path == "/v1/chat/completions" {
                "text/event-stream"
            } else {
                "application/json"
            };
            value.split(';').next() == Some(expected)
        })
        .ok_or(StatusCode::BAD_GATEWAY)?
        .to_owned();
    let stream = futures_util::stream::unfold(
        (
            Limited::new(upstream.into_body(), MAX_RESPONSE),
            Bytes::new(),
            connection,
        ),
        |(mut incoming, mut remaining, connection)| async move {
            if remaining.is_empty() {
                let frame = incoming.frame().await?;
                remaining = match frame.ok().and_then(|frame| frame.into_data().ok()) {
                    Some(bytes) => bytes,
                    None => {
                        return Some((
                            Err(io::Error::other("invalid upstream stream")),
                            (incoming, remaining, connection),
                        ));
                    }
                };
            }
            let chunk = remaining.split_to(remaining.len().min(RESPONSE_CHUNK));
            Some((Ok(chunk), (incoming, remaining, connection)))
        },
    );
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from_stream(stream))
        .expect("validated provider response"))
}
