//! Test-only upstream and qualified worker bundle (AR-001 AR05).
//!
//! The Rust project never substitutes a browser fixture for the production
//! worker. This module builds the same verified bundle from the pinned Node and
//! the shipped worker source, installs a qualified profile into a disposable
//! workspace and speaks the real Ollama-compatible OpenAI wire to the relay.

#![allow(dead_code)]
#![allow(clippy::type_complexity)]

use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::SqlitePool;
use std::{
    collections::BTreeMap,
    fs,
    io::Write as _,
    net::SocketAddr,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

pub const MODEL_TAG: &str = "parley-fixture:latest";
pub const PROVIDER_VERSION: &str = "0.17.0";

static BUNDLE: OnceLock<PathBuf> = OnceLock::new();

pub fn model_digest() -> String {
    "b".repeat(64)
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn tokenizer_artifact() -> Value {
    json!({
        "tokenizer": {
            "version": "1.0", "truncation": null, "padding": null, "added_tokens": [],
            "normalizer": null, "pre_tokenizer": {"type": "Whitespace"},
            "post_processor": null, "decoder": null,
            "model": {"type": "WordLevel", "vocab": {"<unk>": 0}, "unk_token": "<unk>"}
        },
        "config": {}
    })
}

fn node_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("PARLEY_TEST_NODE") {
        return PathBuf::from(path);
    }
    let output = Command::new("bash")
        .args(["-lc", "command -v node"])
        .output()
        .expect("locate pinned node");
    let path = String::from_utf8(output.stdout).expect("node path");
    let path = PathBuf::from(path.trim());
    assert!(
        path.is_file(),
        "pinned node is required for the worker fixture"
    );
    path
}

fn copy_mode(from: &Path, to: &Path) {
    fs::copy(from, to).expect("copy worker member");
    let mut permissions = fs::metadata(to)
        .expect("worker member metadata")
        .permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(to, permissions).expect("worker member permissions");
}

/// Build the exact production layout once per test process and publish it via
/// the debug-only verified-bundle override. The manifest uses the same fields
/// and digests as `scripts/build-agent`.
pub fn verified_bundle() -> &'static Path {
    BUNDLE.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        let stamp = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(format!("parley-ar05-bundle-{stamp}"));
        fs::create_dir(&dir).expect("bundle directory");
        let licenses = dir.join("licenses");
        fs::create_dir(&licenses).expect("license directory");
        let node = node_binary();
        assert_eq!(
            String::from_utf8(Command::new(&node).arg("--version").output().unwrap().stdout)
                .unwrap()
                .trim(),
            "v26.12.0",
            "the pinned Node version is required"
        );
        copy_mode(&node, &dir.join("node"));
        let mut permissions = fs::metadata(dir.join("node")).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(dir.join("node"), permissions).unwrap();

        let metafile = dir.join("metafile.json");
        let status = Command::new(root.join("agent/node_modules/.bin/esbuild"))
            .args([
                "agent/src/main.mjs",
                "--bundle",
                "--platform=node",
                "--target=node26",
                "--format=esm",
                "--legal-comments=external",
                "--banner:js=import {createRequire as bundleCreateRequire} from 'node:module'; const require = bundleCreateRequire(import.meta.url);",
            ])
            .arg(format!("--outfile={}", dir.join("worker.mjs").display()))
            .arg(format!("--metafile={}", metafile.display()))
            .current_dir(root)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("build worker bundle");
        assert!(status.success(), "esbuild failed");

        let schema = fs::read(root.join("contract/agent/runtime-v1.schema.json")).unwrap();
        fs::write(dir.join("runtime-v1.schema.json"), &schema).unwrap();
        for (name, source) in [
            ("coach.schema.json", "crates/parley-service/roles/coach/schema.json"),
            ("template_builder.schema.json", "crates/parley-service/roles/template_builder/schema.json"),
            ("interviewer.schema.json", "crates/parley-service/roles/interviewer/schema.json"),
            ("briefer.schema.json", "crates/parley-service/roles/briefer/schema.json"),
            ("extractor.schema.json", "crates/parley-service/roles/extractor/schema.json"),
        ] {
            copy_mode(&root.join(source), &dir.join(name));
        }
        fs::write(dir.join("worker-build"), format!("{}\n", digest(&fs::read(dir.join("worker.mjs")).unwrap()))).unwrap();
        fs::write(licenses.join("node.txt"), b"Node.js is licensed under the MIT License.\n").unwrap();
        fs::write(licenses.join("index.json"), b"{\"node@26.12.0\":{\"license\":\"MIT\",\"files\":[\"licenses/node.txt\"]}}\n").unwrap();

        let mut members = BTreeMap::new();
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                for nested in fs::read_dir(entry.path()).unwrap() {
                    let nested = nested.unwrap();
                    if nested.file_type().unwrap().is_file() {
                        let name = format!("licenses/{}", nested.file_name().to_string_lossy());
                        let bytes = fs::read(nested.path()).unwrap();
                        members.insert(name, json!({"bytes": bytes.len(), "sha256": digest(&bytes)}));
                    }
                }
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let bytes = fs::read(entry.path()).unwrap();
            members.insert(name, json!({"bytes": bytes.len(), "sha256": digest(&bytes)}));
        }
        let manifest = json!({
            "platform": "linux-x86_64",
            "node_version": "26.12.0",
            "pi_version": "1.6.0",
            "protocol_major": 1,
            "lockfile_digest": digest(&fs::read(root.join("pnpm-lock.yaml")).unwrap()),
            "build_revision": "0123456789abcdef0123456789abcdef01234567",
            "worker_build": digest(&fs::read(dir.join("worker.mjs")).unwrap()),
            "schema_digest": digest(&schema),
            "kernel_controls": ["user","net","pid","ipc","uts","disable_userns","die_with_parent"],
            "members": members,
        });
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_digest = digest(&bytes);
        fs::write(dir.join("runtime-manifest.json"), bytes).unwrap();
        // Test-only, set once for the whole test process before any worker
        // starts; no other thread reads these variables concurrently.
        unsafe {
            std::env::set_var("PARLEY_RUNTIME_BUNDLE", &dir);
            std::env::set_var("PARLEY_TEST_MANIFEST_SHA256", &manifest_digest);
        }
        dir
    })
    .as_path()
}

/// Install one immutable, qualified profile and write its tokenizer artifact.
/// The active pointer is set only after both independent capability rows are
/// recorded ready; a structured-only qualification cannot admit a run.
pub async fn install_qualified_profile(
    pool: &SqlitePool,
    workspace_path: &Path,
    endpoint: SocketAddr,
) -> i64 {
    install_qualified_profile_with(pool, workspace_path, endpoint, 8192, 256, 64).await
}

/// Same as [`install_qualified_profile`] with explicit context/output tokens
/// and template allowance so tests can shape the extractor chunk plan.
#[allow(clippy::too_many_arguments)]
pub async fn install_qualified_profile_with(
    pool: &SqlitePool,
    workspace_path: &Path,
    endpoint: SocketAddr,
    context_tokens: i64,
    output_tokens: i64,
    template_tokens: i64,
) -> i64 {
    verified_bundle();
    let artifact = tokenizer_artifact().to_string();
    let tokenizer_digest = digest(artifact.as_bytes());
    let control = parley_service::Workspace::control_directory(workspace_path).unwrap();
    fs::create_dir_all(control.join("tokenizers")).unwrap();
    fs::write(
        control
            .join("tokenizers")
            .join(format!("{tokenizer_digest}.json")),
        artifact,
    )
    .unwrap();
    let revision: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(revision),0)+1 FROM inference_profile")
            .fetch_one(pool)
            .await
            .unwrap();
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let runner = std::env::var("PARLEY_TEST_MANIFEST_SHA256").unwrap();
    sqlx::query(
        "INSERT INTO inference_profile(revision,runner_kind,runner_build_digest,protocol_major,
           provider_version,endpoint,chat_model,chat_model_digest,tokenizer_digest,recipe_json,
           context_tokens,output_tokens,locale,created_at)
         VALUES(?,'pi',?,1,?,?,?,?,?,?,?,?,'en',?)",
    )
    .bind(revision)
    .bind(&runner)
    .bind(PROVIDER_VERSION)
    .bind(format!("http://{endpoint}"))
    .bind(MODEL_TAG)
    .bind(model_digest())
    .bind(&tokenizer_digest)
    .bind(json!({"template_tokens": template_tokens}).to_string())
    .bind(context_tokens)
    .bind(output_tokens)
    .bind(&now)
    .execute(pool)
    .await
    .unwrap();
    for (mode, number) in [("structured_role", 1), ("delegated", 2), ("embed", 3)] {
        let tuple = digest(format!("{revision}:{mode}").as_bytes());
        sqlx::query(
            "INSERT INTO inference_qualification(tuple_digest,profile_revision,mode,evaluation_digest,checked_at,status)
             VALUES(?,?,?,?,?,'ready')",
        )
        .bind(tuple)
        .bind(revision)
        .bind(mode)
        .bind(format!("{number:064x}"))
        .bind(&now)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query("DELETE FROM inference_active_profile")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO inference_active_profile(singleton,profile_revision) VALUES(1,?)")
        .bind(revision)
        .execute(pool)
        .await
        .unwrap();
    revision
}

/// One scripted upstream response. Native-style bodies produced by the older
/// UCR fixtures are converted to the Pi-compatible wire by `to_sse`.
#[derive(Clone, Debug)]
pub enum Reply {
    ToolCall {
        name: String,
        args: Value,
        text: String,
    },
    /// One assistant message with several accepted ordinals.
    ToolCalls {
        calls: Vec<(String, Value)>,
        text: String,
    },
    Final(String),
    Native(Value),
}

impl Reply {
    pub fn final_answer(text: &str) -> Self {
        Reply::Final(text.to_owned())
    }

    pub fn tool_call(name: &str, args: Value, text: &str) -> Self {
        Reply::ToolCall {
            name: name.to_owned(),
            args,
            text: text.to_owned(),
        }
    }

    fn from_native(value: &Value) -> Self {
        let text = value["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if let Some(calls) = value["message"]["tool_calls"].as_array()
            && let Some(call) = calls.first()
        {
            let name = call["function"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let args = call["function"]["arguments"]
                .as_str()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or_else(|| json!({}));
            Reply::ToolCall { name, args, text }
        } else {
            Reply::Final(text)
        }
    }
}

struct Inner {
    replies: Vec<Reply>,
    index: AtomicUsize,
    native: Mutex<Vec<Value>>,
    native_index: AtomicUsize,
    responder: Option<Mutex<Box<dyn FnMut(usize, &str) -> Value + Send>>>,
    responder_index: AtomicUsize,
    hold_native: bool,
    native_release: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    native_gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    release: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    gate_at: Option<usize>,
    seen: Arc<Mutex<Vec<String>>>,
}

pub struct ModelServer {
    port: u16,
    inner: Arc<Inner>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ModelServer {
    pub async fn start(replies: Vec<Reply>) -> Self {
        Self::with_gate(replies, None).await
    }

    /// Also answer the pre-cut native role path used by a child job, so a
    /// resumed run observes a genuinely successful durable completion.
    pub async fn with_native(replies: Vec<Reply>, native: Vec<Value>) -> Self {
        Self::with_native_and_gate(replies, native, None).await
    }

    /// Hold the child job's native response so the parent stays observably
    /// parked on its durable wait condition.
    pub async fn with_native_held(replies: Vec<Reply>, native: Vec<Value>) -> Self {
        Self::build(replies, native, None, true).await
    }

    pub async fn release_native(&mut self) {
        if let Some(release) = self.inner.native_release.lock().unwrap().take() {
            let _ = release.send(());
        }
    }

    pub async fn with_native_and_gate(
        replies: Vec<Reply>,
        native: Vec<Value>,
        pause_index: Option<usize>,
    ) -> Self {
        Self::build(replies, native, pause_index, false).await
    }

    /// Hold the response at `pause_index` so a test can race cancellation or
    /// exhaustion while a provider call is in flight.
    pub async fn with_gate(replies: Vec<Reply>, pause_index: Option<usize>) -> Self {
        Self::build(replies, Vec::new(), pause_index, false).await
    }

    async fn build(
        replies: Vec<Reply>,
        native: Vec<Value>,
        pause_index: Option<usize>,
        hold_native: bool,
    ) -> Self {
        Self::build_with_responder(replies, native, pause_index, hold_native, None).await
    }

    /// Drive scripted native-style responders through the real Pi wire.
    pub async fn with_responder(
        responder: Box<dyn FnMut(usize, &str) -> Value + Send>,
        pause_index: Option<usize>,
    ) -> Self {
        Self::build_with_responder(Vec::new(), Vec::new(), pause_index, false, Some(responder))
            .await
    }

    async fn build_with_responder(
        replies: Vec<Reply>,
        native: Vec<Value>,
        pause_index: Option<usize>,
        hold_native: bool,
        responder: Option<Box<dyn FnMut(usize, &str) -> Value + Send>>,
    ) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let (native_release, native_gate) = if hold_native {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let inner = Arc::new(Inner {
            replies,
            index: AtomicUsize::new(0),
            native: Mutex::new(native),
            native_index: AtomicUsize::new(0),
            responder: responder.map(Mutex::new),
            responder_index: AtomicUsize::new(0),
            hold_native,
            native_release: Mutex::new(native_release),
            native_gate: Mutex::new(native_gate),
            release: Mutex::new((pause_index.is_some()).then_some(release)),
            gate: Mutex::new(if pause_index.is_some() {
                Some(gate)
            } else {
                None
            }),
            gate_at: pause_index,
            seen: Arc::new(Mutex::new(Vec::new())),
        });
        let state = inner.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let state = state.clone();
                tokio::spawn(async move {
                    let _ = serve_one(&mut stream, state).await;
                });
            }
        });
        Self {
            port,
            inner,
            task: Some(task),
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn endpoint(&self) -> SocketAddr {
        format!("127.0.0.1:{}", self.port).parse().unwrap()
    }

    pub fn recorded(&self) -> Vec<String> {
        self.inner.seen.lock().unwrap().clone()
    }

    pub fn calls(&self) -> usize {
        self.inner.index.load(Ordering::Relaxed)
    }

    pub async fn release(&mut self) {
        if let Some(release) = self.inner.release.lock().unwrap().take() {
            let _ = release.send(());
        }
    }

    pub async fn stop(mut self) {
        self.release().await;
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for ModelServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read_exact(&mut byte).await.is_err() {
            return None;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let length: usize = head
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse().ok())
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    let _ = stream.read_exact(&mut body).await;
    Some((head, String::from_utf8_lossy(&body).into_owned()))
}

fn sse(reply: &Reply) -> String {
    let mut output = String::new();
    match reply {
        Reply::Final(text) => {
            output.push_str(&chunk(
                None,
                Some(json!({"role":"assistant","content":text})),
                None,
            ));
            output.push_str(&chunk(Some("stop"), None, None));
        }
        Reply::ToolCall { name, args, text } => {
            return sse(&Reply::ToolCalls {
                calls: vec![(name.clone(), args.clone())],
                text: text.clone(),
            });
        }
        Reply::ToolCalls { calls, text } => {
            if !text.is_empty() {
                output.push_str(&chunk(
                    None,
                    Some(json!({"role":"assistant","content":text})),
                    None,
                ));
            }
            let tools = calls
                .iter()
                .enumerate()
                .map(|(index, (name, args))| {
                    json!({
                        "index": index, "id": format!("sdk-{index}"), "type": "function",
                        "function": {"name": name, "arguments": args.to_string()}
                    })
                })
                .collect();
            output.push_str(&chunk(None, None, Some(tools)));
            output.push_str(&chunk(Some("tool_calls"), None, None));
        }
        Reply::Native(value) => {
            let converted = Reply::from_native(value);
            return sse(&converted);
        }
    }
    output.push_str(&chunk(None, None, None));
    output.push_str("data: [DONE]\n\n");
    output
}

fn chunk(finish: Option<&str>, delta: Option<Value>, tools: Option<Vec<Value>>) -> String {
    let mut choice =
        json!({"index": 0, "delta": delta.unwrap_or_else(|| json!({})), "finish_reason": finish});
    if let Some(tools) = tools {
        choice["delta"]["tool_calls"] = json!(tools);
    }
    let body = json!({
        "id": "chatcmpl-fixture", "object": "chat.completion.chunk", "created": 42,
        "model": MODEL_TAG, "choices": [choice],
        "usage": {"prompt_tokens": 40, "completion_tokens": 8, "total_tokens": 48},
    });
    format!("data: {body}\n\n")
}

async fn serve_one(stream: &mut TcpStream, state: Arc<Inner>) -> Option<()> {
    let (head, body) = read_request(stream).await?;
    state.seen.lock().unwrap().push(format!("{head}{body}"));
    let path = head.lines().next().unwrap_or_default();
    let response = if path.contains("GET /api/version") {
        (
            Some("application/json"),
            json!({"version": PROVIDER_VERSION}).to_string(),
        )
    } else if path.contains("GET /api/tags") {
        (
            Some("application/json"),
            json!({"models":[{"name":MODEL_TAG,"digest":model_digest()}]}).to_string(),
        )
    } else if path.contains("POST /api/chat") {
        let index = state.native_index.fetch_add(1, Ordering::Relaxed);
        let native_gate = if state.hold_native && index == 0 {
            state.native_gate.lock().unwrap().take()
        } else {
            None
        };
        if let Some(gate) = native_gate {
            let _ = gate.await;
        }
        let payload = state
            .native
            .lock()
            .unwrap()
            .get(index)
            .cloned()
            .unwrap_or_else(|| json!({"message":{"role":"assistant","content":"{\"response\":null,\"items\":[]}"},"prompt_eval_count":12,"eval_count":8}));
        (Some("application/json"), payload.to_string())
    } else if path.contains("POST /v1/chat/completions") {
        let index = state.index.fetch_add(1, Ordering::Relaxed);
        let gate = if state.gate_at == Some(index) {
            state.gate.lock().unwrap().take()
        } else {
            None
        };
        if let Some(gate) = gate {
            let _ = gate.await;
        }
        let reply = if let Some(responder) = &state.responder {
            // Legacy responders script a tool-capability probe as their first
            // turn. The worker does not issue that probe; skip it and present
            // the next scripted turn to the real delegated chat.
            let mut value = responder.lock().unwrap()(index, &body);
            for attempt in 1..4 {
                let text = value["message"]["content"].as_str().unwrap_or_default();
                if !text.contains("probe") {
                    break;
                }
                value = responder.lock().unwrap()(index + attempt, &body);
            }
            Reply::Native(value)
        } else {
            state
                .replies
                .get(index)
                .cloned()
                .unwrap_or(Reply::Final("Done.".into()))
        };
        (Some("text/event-stream"), sse(&reply))
    } else {
        (None, String::new())
    };
    let (content_type, payload) = response;
    let header = match content_type {
        Some(content_type) => format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        ),
        None => {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
        }
    };
    stream.write_all(header.as_bytes()).await.ok()?;
    stream.write_all(payload.as_bytes()).await.ok()?;
    let _ = std::io::stdout().flush();
    Some(())
}

/// Convert an older native-scripted response list into real Pi wire replies.
pub fn native_replies(bodies: Vec<Value>) -> Vec<Reply> {
    bodies
        .into_iter()
        .filter(|body| {
            !body["message"]["content"]
                .as_str()
                .is_some_and(|text| text.contains("probe"))
        })
        .map(Reply::Native)
        .collect()
}

pub async fn pause(millis: u64) {
    tokio::time::sleep(Duration::from_millis(millis)).await;
}

/// Drop-in provider with the API used by the pre-existing UCR authority tests.
pub enum PauseArg {
    Index(Option<usize>),
    Bool(bool),
}

impl From<Option<usize>> for PauseArg {
    fn from(value: Option<usize>) -> Self {
        Self::Index(value)
    }
}

impl From<bool> for PauseArg {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

pub struct ResponderProvider {
    inner: ModelServer,
}

impl ResponderProvider {
    pub async fn start(bodies: Vec<Value>) -> Self {
        Self::with_gate(bodies, false).await
    }

    pub async fn with_gate(bodies: Vec<Value>, gate_chat: bool) -> Self {
        let scripted: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(
            bodies
                .into_iter()
                .filter(|body| {
                    !body["message"]["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("probe"))
                })
                .collect(),
        ));
        let taken = Arc::new(AtomicUsize::new(0));
        let responder: Box<dyn FnMut(usize, &str) -> Value + Send> = Box::new(
            move |_index, _body| {
                let items = scripted.lock().unwrap();
                let position = taken.fetch_add(1, Ordering::Relaxed);
                items.get(position).cloned().unwrap_or_else(|| {
                json!({"message":{"role":"assistant","content":"Done."},"prompt_eval_count":12,"eval_count":8})
            })
            },
        );
        Self::with(responder, gate_chat).await
    }

    pub async fn with(
        responder: Box<dyn FnMut(usize, &str) -> Value + Send>,
        pause: impl Into<PauseArg>,
    ) -> Self {
        let pause = match pause.into() {
            PauseArg::Index(index) => index,
            PauseArg::Bool(true) => Some(0),
            PauseArg::Bool(false) => None,
        };
        Self {
            inner: ModelServer::with_responder(responder, pause).await,
        }
    }

    pub fn url(&self) -> String {
        self.inner.url()
    }
    pub fn endpoint(&self) -> SocketAddr {
        self.inner.endpoint()
    }
    pub fn bodies(&self) -> Vec<String> {
        self.inner.recorded()
    }
    pub fn recorded(&self) -> Vec<String> {
        self.inner.recorded()
    }
    pub async fn release(&mut self) {
        self.inner.release().await;
    }
    pub async fn stop(self) {
        self.inner.stop().await;
    }
}
