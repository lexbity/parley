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
    /// Structured executions keep the exact five-step path order. Delegated
    /// leases are bounded per path instead: one chat POST per planned step,
    /// at most two catalog observations, and never more than 16 chat calls.
    delegated: bool,
    stage: AtomicU8,
    versions: AtomicU8,
    tags: AtomicU8,
    chats: AtomicU8,
}

const MAX_DELEGATED_CHAT: u8 = 16;

pub struct Relay {
    directory: PathBuf,
    task: JoinHandle<()>,
}

impl Relay {
    pub fn bind(endpoint: SocketAddr, model: String, max_output_tokens: u32) -> io::Result<Self> {
        Self::bind_mode(endpoint, model, max_output_tokens, false)
    }

    pub fn bind_delegated(
        endpoint: SocketAddr,
        model: String,
        max_output_tokens: u32,
    ) -> io::Result<Self> {
        Self::bind_mode(endpoint, model, max_output_tokens, true)
    }

    fn bind_mode(
        endpoint: SocketAddr,
        model: String,
        max_output_tokens: u32,
        delegated: bool,
    ) -> io::Result<Self> {
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
            delegated,
            stage: AtomicU8::new(0),
            versions: AtomicU8::new(0),
            tags: AtomicU8::new(0),
            chats: AtomicU8::new(0),
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
    if request.get("model").and_then(serde_json::Value::as_str) != Some(&policy.model)
        || request.get("stream").and_then(serde_json::Value::as_bool) != Some(true)
        || request
            .get("temperature")
            .and_then(serde_json::Value::as_f64)
            != Some(0.0)
        || request
            .get("max_tokens")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(policy.max_output_tokens))
        || request.get("stream_options") != Some(&serde_json::json!({"include_usage":true}))
    {
        return false;
    }
    let structured_fields = [
        "model",
        "messages",
        "stream",
        "stream_options",
        "max_tokens",
        "temperature",
        "response_format",
    ];
    let delegated_fields = [
        "model",
        "messages",
        "stream",
        "stream_options",
        "max_tokens",
        "temperature",
        "tools",
    ];
    let messages = request
        .get("messages")
        .and_then(serde_json::Value::as_array);
    let Some(messages) = messages else {
        return false;
    };
    if policy.delegated {
        if request.len() != delegated_fields.len()
            || !delegated_fields
                .iter()
                .all(|field| request.contains_key(*field))
            || messages.is_empty()
            || messages.len() > 64
        {
            return false;
        }
        let Some(tools) = request["tools"].as_array() else {
            return false;
        };
        if tools.is_empty() || tools.len() > 20 || !tools.iter().all(|tool| {
            let Some(object) = tool.as_object() else { return false };
            object.len() == 2
                && tool["type"].as_str() == Some("function")
                && tool["function"].as_object().is_some_and(|function| {
                    function.len() == 3
                        && matches!(function.get("name"), Some(serde_json::Value::String(name)) if !name.is_empty() && name.len() <= 128)
                        && matches!(function.get("description"), Some(serde_json::Value::String(value)) if value.len() <= 512)
                        && matches!(function.get("parameters"), Some(serde_json::Value::Object(_)))
                })
        }) {
            return false;
        }
        return messages.iter().all(|message| {
            let Some(object) = message.as_object() else { return false };
            match message.get("role").and_then(serde_json::Value::as_str) {
                Some("system" | "user") => {
                    object.len() == 2 && message.get("content").is_some_and(serde_json::Value::is_string)
                }
                Some("tool") => {
                    (object.len() == 2
                        || (object.len() == 3
                            && message.get("tool_call_id").and_then(serde_json::Value::as_str).is_some_and(|id| {
                                !id.is_empty() && id.len() <= 128
                            })))
                        && message.get("content").is_some_and(serde_json::Value::is_string)
                }
                Some("assistant") => {
                    object.len() == 3
                        && message.get("content").is_some_and(serde_json::Value::is_string)
                        && message["tool_calls"].as_array().is_some_and(|calls| {
                            !calls.is_empty()
                                && calls.len() <= 4
                                && calls.iter().all(|call| {
                                    let Some(call) = call.as_object() else { return false };
                                    call.len() == 3
                                        && call.get("id").and_then(serde_json::Value::as_str).is_some_and(|id| !id.is_empty() && id.len() <= 128)
                                        && call.get("type").and_then(serde_json::Value::as_str) == Some("function")
                                        && call.get("function").and_then(serde_json::Value::as_object).is_some_and(|function| {
                                            function.len() == 2
                                                && matches!(function.get("name"), Some(serde_json::Value::String(name)) if !name.is_empty() && name.len() <= 128)
                                                && matches!(function.get("arguments"), Some(serde_json::Value::String(value)) if value.len() <= 16 * 1024)
                                        })
                                })
                        })
                }
                _ => false,
            }
        });
    }
    request.len() == structured_fields.len()
        && structured_fields
            .iter()
            .all(|field| request.contains_key(*field))
        && request
            .get("response_format")
            .and_then(|v| v.get("type"))
            .and_then(serde_json::Value::as_str)
            == Some("json_schema")
        && messages.len() == 2
        && ["system", "user"]
            .into_iter()
            .zip(messages)
            .all(|(role, message)| {
                message.as_object().is_some_and(|fields| fields.len() == 2)
                    && message.get("role").and_then(serde_json::Value::as_str) == Some(role)
                    && message
                        .get("content")
                        .is_some_and(serde_json::Value::is_string)
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
    if policy.delegated {
        let counter = match (&parts.method, path) {
            (&Method::GET, "/api/version") => &policy.versions,
            (&Method::GET, "/api/tags") => &policy.tags,
            (&Method::POST, "/v1/chat/completions") => &policy.chats,
            _ => return Err(StatusCode::FORBIDDEN),
        };
        let limit = if std::ptr::eq(counter, &policy.chats) {
            MAX_DELEGATED_CHAT
        } else {
            2
        };
        if counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                (used < limit).then_some(used + 1)
            })
            .is_err()
        {
            return Err(StatusCode::FORBIDDEN);
        }
    } else {
        let stage = policy.stage.load(Ordering::Relaxed);
        let expected_path = match stage {
            0 | 3 => "/api/version",
            1 | 4 => "/api/tags",
            2 => "/v1/chat/completions",
            _ => return Err(StatusCode::FORBIDDEN),
        };
        if path != expected_path {
            return Err(StatusCode::FORBIDDEN);
        }
        policy.stage.store(stage + 1, Ordering::Relaxed);
    }
    if (path == "/v1/chat/completions" && !valid_chat(&bytes, &policy))
        || (parts.method == Method::GET && !bytes.is_empty())
    {
        return Err(StatusCode::FORBIDDEN);
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            endpoint: "127.0.0.1:11434".parse().unwrap(),
            model: "local:approved".into(),
            max_output_tokens: 512,
            delegated: false,
            stage: AtomicU8::new(2),
            versions: AtomicU8::new(2),
            tags: AtomicU8::new(2),
            chats: AtomicU8::new(1),
        }
    }

    fn request(method: Method, uri: &str) -> Parts {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, AUTHORITY)
            .body(())
            .unwrap()
            .into_parts()
            .0
    }

    fn valid_chat_body() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "model":"local:approved",
            "messages":[{"role":"system","content":"instructions"},{"role":"user","content":"data"}],
            "stream":true,
            "stream_options":{"include_usage":true},
            "max_tokens":512,
            "temperature":0,
            "response_format":{"type":"json_schema","json_schema":{"name":"coach","schema":{}}}
        })).unwrap()
    }

    #[test]
    fn ar01_relay_limits_loopback_authority_headers_and_framing() {
        for allowed in [
            "127.0.0.1:11434".parse().unwrap(),
            "[::1]:11434".parse().unwrap(),
        ] {
            assert!(is_allowed_endpoint(allowed));
        }
        for denied in [
            "127.0.0.2:11434".parse().unwrap(),
            "0.0.0.0:11434".parse().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        ] {
            assert!(!is_allowed_endpoint(denied));
        }
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, "12".parse().unwrap());
        assert!(allowed_framing(&headers, false));
        headers.append(header::CONTENT_LENGTH, "12".parse().unwrap());
        assert!(!allowed_framing(&headers, false));
        let mut chunked = HeaderMap::new();
        chunked.insert(header::TRANSFER_ENCODING, "chunked".parse().unwrap());
        assert!(allowed_framing(&chunked, true));
        chunked.insert(header::CONTENT_LENGTH, "12".parse().unwrap());
        assert!(!allowed_framing(&chunked, true));
        let mut encoded = HeaderMap::new();
        encoded.insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
        assert!(!allowed_framing(&encoded, true));
        let mut huge = HeaderMap::new();
        huge.insert("x-large", "x".repeat(MAX_HEADERS).parse().unwrap());
        assert!(!headers_within_budget(&huge));
    }

    #[test]
    fn ar01_relay_routes_only_expected_methods_and_paths() {
        assert_eq!(
            validate_request(&request(Method::GET, "/api/tags")).unwrap(),
            "/api/tags"
        );
        assert_eq!(
            validate_request(&request(Method::GET, "/api/version")).unwrap(),
            "/api/version"
        );
        let mut chat = request(Method::POST, "/v1/chat/completions");
        chat.headers
            .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        assert_eq!(validate_request(&chat).unwrap(), "/v1/chat/completions");
        for (method, uri) in [
            (Method::POST, "/api/tags"),
            (Method::GET, "/v1/chat/completions"),
            (Method::GET, "/api/tags?x=1"),
            (Method::GET, "http://evil.test/api/tags"),
        ] {
            assert!(validate_request(&request(method, uri)).is_err());
        }
        let mut hostile = request(Method::GET, "/api/tags");
        hostile
            .headers
            .insert(header::AUTHORIZATION, "secret".parse().unwrap());
        assert!(validate_request(&hostile).is_err());
        let mut wrong_host = request(Method::GET, "/api/tags");
        wrong_host
            .headers
            .insert(header::HOST, "localhost".parse().unwrap());
        assert!(validate_request(&wrong_host).is_err());
    }

    #[test]
    fn ar01_relay_chat_body_is_closed_and_profile_bound() {
        let policy = policy();
        let body = valid_chat_body();
        assert!(valid_chat(&body, &policy));
        for body in [
            serde_json::to_vec(&serde_json::json!({"model":"foreign","messages":[]})).unwrap(),
            serde_json::to_vec(&serde_json::json!({"model":"local:approved","messages":[],"extra":true})).unwrap(),
            serde_json::to_vec(&serde_json::json!({"model":"local:approved","messages":[],"stream":false,"stream_options":{"include_usage":true},"max_tokens":512,"temperature":0,"response_format":{"type":"json_schema"}})).unwrap(),
            b"{\"model\":\"local:approved\",\"model\":\"foreign\"}".to_vec(),
        ] {
            assert!(!valid_chat(&body, &policy));
        }
    }
}
