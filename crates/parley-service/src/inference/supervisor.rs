use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read as _},
    net::SocketAddr,
    os::unix::fs::MetadataExt as _,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tokio::{io::BufReader, process::Command, sync::watch, time};

use super::{
    protocol::{
        self, Ack, AssistantTurn, Capability, Completion, Empty, Execute, Failure, HostFrame,
        Namespaces, ProtocolError, Suspend, ToolRequest, ToolResult, TurnAccepted, WorkerFrame,
    },
    relay::Relay,
};

const MANIFEST: &str = "runtime-manifest.json";
const BWRAP: &str = "/usr/bin/bwrap";
const NODE_VERSION: &str = "26.12.0";
const PI_VERSION: &str = "1.6.0";
const MAX_MANIFEST: u64 = 256 * 1024;
const MAX_BUNDLE: u64 = 256 * 1024 * 1024;
const MAX_TOKENIZER: u64 = 32 * 1024 * 1024;
const KERNEL_CONTROLS: [&str; 7] = [
    "user",
    "net",
    "pid",
    "ipc",
    "uts",
    "disable_userns",
    "die_with_parent",
];
const LIBRARIES: [&str; 7] = [
    "libatomic.so.1",
    "libdl.so.2",
    "libm.so.6",
    "libstdc++.so.6",
    "libgcc_s.so.1",
    "libpthread.so.0",
    "libc.so.6",
];

/// The host side of a delegated lease. Every decision is made by Rust under
/// the workspace writer before a frame reaches the worker; the worker never
/// sees a principal, receipt, approval identity or business result that the
/// host has not committed.
pub trait DelegatedHost {
    /// Journal the complete assistant turn and its ordered intents, then
    /// return the accepted ordinals or a durable wait/suspend.
    fn accept_turn(&self, turn: AssistantTurn) -> BoxFuture<'_, Result<HostDecision, RunError>>;
    /// Dispatch one accepted ordinal through the command registry, or park
    /// the run on its originating intent.
    fn resolve_tool(&self, request: ToolRequest) -> BoxFuture<'_, Result<ToolDecision, RunError>>;
}

#[derive(Debug)]
pub enum HostDecision {
    TurnAccepted { turn_no: u32, ordinals: Vec<u32> },
    Suspend(&'static str),
}

#[derive(Debug)]
pub enum ToolDecision {
    Result(Value),
    Suspend(&'static str),
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum RunError {
    #[error("runner installation invalid")]
    Install,
    #[error("worker protocol negotiation failed")]
    ProtocolMismatch,
    #[error("worker isolation unavailable")]
    Isolation,
    #[error("worker protocol failure")]
    Protocol,
    #[error("provider request failed")]
    Provider,
    #[error("inference was cancelled")]
    Cancelled,
    #[error("inference deadline expired")]
    Timeout,
    #[error("worker operation failed")]
    Worker(Failure),
}

impl RunError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Install => "RUNNER_INSTALL_INVALID",
            Self::ProtocolMismatch => "PROTOCOL_MISMATCH",
            Self::Isolation | Self::Worker(Failure::IsolationUnavailable) => {
                "ISOLATION_UNAVAILABLE"
            }
            Self::Protocol | Self::Worker(Failure::WorkerProtocol) => "WORKER_PROTOCOL",
            Self::Provider | Self::Worker(Failure::ProviderFailed) => "PROVIDER_FAILED",
            Self::Cancelled | Self::Worker(Failure::Cancelled) => "CANCELLED",
            Self::Timeout => "INFERENCE_TIMEOUT",
            Self::Worker(Failure::ModelUnavailable) => "MODEL_UNAVAILABLE",
            Self::Worker(Failure::ProfileChanged) => "PROFILE_CHANGED",
            Self::Worker(Failure::ContextBudget) => "CONTEXT_BUDGET",
            Self::Worker(Failure::ResultInvalid) => "RESULT_INVALID",
        }
    }
}

impl From<ProtocolError> for RunError {
    fn from(_: ProtocolError) -> Self {
        Self::Protocol
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    platform: String,
    node_version: String,
    pi_version: String,
    protocol_major: u8,
    lockfile_digest: String,
    build_revision: String,
    worker_build: String,
    schema_digest: String,
    kernel_controls: Vec<String>,
    members: BTreeMap<String, Member>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    bytes: u64,
    sha256: String,
}

pub struct Supervisor {
    bundle: PathBuf,
    manifest: Manifest,
    digest: String,
}

fn effective_uid() -> io::Result<u32> {
    Ok(fs::metadata("/proc/self")?.uid())
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hexadecimal(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn safe_member(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn secure_ancestry(path: &Path, owner: u32, allow_tmp: bool) -> Result<(), RunError> {
    if !path.is_absolute()
        || path
            .components()
            .skip(1)
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(RunError::Install);
    }
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).map_err(|_| RunError::Install)?;
        let trusted_tmp = allow_tmp
            && ancestor == Path::new("/tmp")
            && metadata.uid() == 0
            && metadata.mode() & 0o1000 != 0;
        if !metadata.file_type().is_dir()
            || (metadata.uid() != owner && metadata.uid() != 0)
            || (!trusted_tmp && metadata.mode() & 0o022 != 0)
        {
            return Err(RunError::Install);
        }
    }
    Ok(())
}

fn bounded_read(path: &Path, max: u64, owner: u32) -> Result<Vec<u8>, RunError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| RunError::Install)?;
    if !metadata.is_file()
        || metadata.len() > max
        || metadata.mode() & 0o022 != 0
        || (metadata.uid() != owner && metadata.uid() != 0)
    {
        return Err(RunError::Install);
    }
    let mut file = fs::File::open(path)
        .map_err(|_| RunError::Install)?
        .take(max + 1);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| RunError::Install)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(RunError::Install);
    }
    Ok(bytes)
}

fn list_members(
    root: &Path,
    directory: &Path,
    owner: u32,
    members: &mut BTreeSet<String>,
) -> Result<(), RunError> {
    for file in fs::read_dir(directory).map_err(|_| RunError::Install)? {
        let file = file.map_err(|_| RunError::Install)?;
        let metadata = fs::symlink_metadata(file.path()).map_err(|_| RunError::Install)?;
        if metadata.mode() & 0o022 != 0 || (metadata.uid() != 0 && metadata.uid() != owner) {
            return Err(RunError::Install);
        }
        let path = file.path();
        if metadata.is_dir() {
            list_members(root, &path, owner, members)?;
        } else if metadata.is_file() {
            let relative = path.strip_prefix(root).map_err(|_| RunError::Install)?;
            let name = relative.to_str().ok_or(RunError::Install)?;
            if !safe_member(relative) || members.len() > 1024 {
                return Err(RunError::Install);
            }
            members.insert(name.into());
        } else {
            return Err(RunError::Install);
        }
    }
    Ok(())
}

impl Supervisor {
    pub fn installed() -> Result<Self, RunError> {
        let launcher = std::env::current_exe().map_err(|_| RunError::Install)?;
        let root = launcher
            .parent()
            .and_then(Path::parent)
            .ok_or(RunError::Install)?;
        Self::verify(&root.join("libexec/parley-agent"), false)
    }

    pub fn from_bundle(bundle: &Path) -> Result<Self, RunError> {
        Self::verify(bundle, true)
    }

    pub fn manifest_digest(&self) -> &str {
        &self.digest
    }

    fn verify(bundle: &Path, explicit: bool) -> Result<Self, RunError> {
        if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            return Err(RunError::Isolation);
        }
        let expected = option_env!("PARLEY_RUNTIME_MANIFEST_SHA256")
            .map(str::to_owned)
            .or_else(|| {
                // Debug-only test affordance: an explicitly built bundle and
                // its digest. Release binaries keep the build-time pin only.
                if cfg!(debug_assertions) {
                    std::env::var("PARLEY_TEST_MANIFEST_SHA256").ok()
                } else {
                    None
                }
            })
            .ok_or(RunError::Install)?;
        let uid = effective_uid().map_err(|_| RunError::Install)?;
        secure_ancestry(bundle, uid, explicit)?;
        let manifest_bytes = bounded_read(&bundle.join(MANIFEST), MAX_MANIFEST, uid)?;
        if digest(&manifest_bytes) != expected {
            return Err(RunError::Install);
        }
        let manifest: Manifest = serde_json::from_value(protocol::parse_json(&manifest_bytes)?)
            .map_err(|_| RunError::Install)?;
        if manifest.platform != "linux-x86_64"
            || manifest.node_version != NODE_VERSION
            || manifest.pi_version != PI_VERSION
            || manifest.protocol_major != 1
            || manifest.kernel_controls != KERNEL_CONTROLS
            || manifest.schema_digest != digest(protocol::SCHEMA)
            || !hexadecimal(&manifest.lockfile_digest, 64)
            || !hexadecimal(&manifest.build_revision, 40)
            || !hexadecimal(&manifest.worker_build, 64)
            || !([
                "node",
                "worker.mjs",
                "worker-build",
                "coach.schema.json",
                "template_builder.schema.json",
                "interviewer.schema.json",
                "briefer.schema.json",
                "runtime-v1.schema.json",
                "licenses/node.txt",
                "licenses/index.json",
            ]
            .iter()
            .all(|name| manifest.members.contains_key(*name)))
        {
            return Err(RunError::Install);
        }
        let mut actual = BTreeSet::new();
        list_members(bundle, bundle, uid, &mut actual)?;
        actual.remove(MANIFEST);
        if !actual.iter().eq(manifest.members.keys()) {
            return Err(RunError::Install);
        }
        let mut total = 0u64;
        for (name, member) in &manifest.members {
            total = total.checked_add(member.bytes).ok_or(RunError::Install)?;
            if total > MAX_BUNDLE || !safe_member(Path::new(name)) {
                return Err(RunError::Install);
            }
            let file = bounded_read(&bundle.join(name), member.bytes, uid)?;
            if file.len() as u64 != member.bytes || digest(&file) != member.sha256 {
                return Err(RunError::Install);
            }
        }
        if manifest.members["worker.mjs"].sha256 != manifest.worker_build
            || manifest.members["runtime-v1.schema.json"].sha256 != manifest.schema_digest
        {
            return Err(RunError::Install);
        }
        Ok(Self {
            bundle: bundle.to_owned(),
            manifest,
            digest: expected,
        })
    }

    pub async fn execute(
        &self,
        endpoint: SocketAddr,
        execution_id: &str,
        lease_id: &str,
        tokenizer: &Path,
        request: Execute,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Completion, RunError> {
        if *cancel.borrow_and_update() {
            return Err(RunError::Cancelled);
        }
        let uid = effective_uid().map_err(|_| RunError::Isolation)?;
        secure_ancestry(tokenizer.parent().ok_or(RunError::Install)?, uid, true)?;
        if digest(&bounded_read(tokenizer, MAX_TOKENIZER, uid)?)
            != request.profile().tokenizer_digest
        {
            return Err(RunError::Worker(Failure::ProfileChanged));
        }
        let host_namespaces = read_host_namespaces().map_err(|_| RunError::Isolation)?;
        let relay = Relay::bind(
            endpoint,
            request.profile().model_tag.clone(),
            request.profile().max_output_tokens,
        )
        .map_err(|_| RunError::Provider)?;
        let mut command = self.command(relay.directory(), tokenizer)?;
        let mut child = command.spawn().map_err(|_| RunError::Isolation)?;
        let mut stdin = child.stdin.take().ok_or(RunError::Isolation)?;
        let mut stdout =
            BufReader::with_capacity(8192, child.stdout.take().ok_or(RunError::Isolation)?);
        let end = time::Instant::now() + Duration::from_millis(u64::from(request.deadline_ms()));
        let exchange = async {
            let hello = time::timeout(
                Duration::from_secs(2),
                protocol::read_worker_frame(&mut stdout),
            )
            .await
            .map_err(|_| RunError::Isolation)?
            .map_err(|_| RunError::ProtocolMismatch)?;
            let Some(WorkerFrame::Hello { seq: 1, body }) = hello else {
                return Err(RunError::Isolation);
            };
            if body.build != self.manifest.worker_build
                || body.schema_digest != self.manifest.schema_digest
                || body.capabilities != vec![Capability::StructuredRole, Capability::Delegated]
            {
                return Err(RunError::ProtocolMismatch);
            }
            if !namespace_separation(&host_namespaces, &body.namespaces) {
                return Err(RunError::Isolation);
            }
            protocol::write_host_frame(
                &mut stdin,
                HostFrame::HelloAck {
                    v: 1,
                    seq: 1,
                    body: Ack {
                        capabilities: body.capabilities,
                        max_frame_bytes: protocol::MAX_FRAME,
                    },
                },
            )
            .await?;
            protocol::write_host_frame(
                &mut stdin,
                HostFrame::Execute {
                    v: 1,
                    seq: 2,
                    execution_id,
                    lease_id,
                    body: &request,
                },
            )
            .await?;
            let completion = match protocol::read_worker_frame(&mut stdout).await? {
                Some(WorkerFrame::Complete {
                    seq: 2,
                    execution_id: execution,
                    lease_id: lease,
                    body,
                }) if execution_id == execution && lease_id == lease && request.matches(&body) => {
                    body
                }
                Some(WorkerFrame::Failed {
                    seq: 2,
                    execution_id: execution,
                    lease_id: lease,
                    body,
                }) if execution_id == execution && lease_id == lease => {
                    return Err(RunError::Worker(body.code));
                }
                _ => return Err(RunError::Protocol),
            };
            validate_completion(&request, &completion)?;
            if protocol::read_worker_frame(&mut stdout).await?.is_some() {
                return Err(RunError::Protocol);
            }
            Ok(completion)
        };

        let result = tokio::select! {
            biased;
            _ = cancel.wait_for(|requested| *requested) => Err(RunError::Cancelled),
            result = time::timeout_at(end, exchange) => result.unwrap_or(Err(RunError::Timeout)),
        };
        relay.close().await;
        if matches!(result, Err(RunError::Cancelled | RunError::Timeout)) {
            let _ = time::timeout(
                Duration::from_millis(30),
                protocol::write_host_frame(
                    &mut stdin,
                    HostFrame::Cancel {
                        v: 1,
                        seq: 3,
                        execution_id,
                        lease_id,
                        body: Empty {},
                    },
                ),
            )
            .await;
        }
        drop(stdin);
        drop(stdout);
        if result.is_ok()
            && let Ok(Ok(status)) = time::timeout_at(end, child.wait()).await
        {
            return if status.success() {
                result
            } else {
                Err(RunError::Protocol)
            };
        }
        let _ = child.start_kill();
        let _ = time::timeout(Duration::from_secs(1), child.wait()).await;
        result.and(Err(RunError::Timeout))
    }

    /// Run one bounded delegated step. Unlike a role execution, a delegated
    /// step is an interactive exchange: the host journals each assistant turn
    /// and each tool ordinal before the worker is allowed to continue. A
    /// durable wait ends the lease and reaps the process; a fresh claim starts
    /// a new execution with canonical reconstructed history.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_delegated(
        &self,
        endpoint: SocketAddr,
        execution_id: &str,
        lease_id: &str,
        tokenizer: &Path,
        request: Execute,
        mut cancel: watch::Receiver<bool>,
        host: &(impl DelegatedHost + Sync),
    ) -> Result<Completion, RunError> {
        if !matches!(request, Execute::Delegated { .. }) || *cancel.borrow_and_update() {
            return Err(RunError::Cancelled);
        }
        let uid = effective_uid().map_err(|_| RunError::Isolation)?;
        secure_ancestry(tokenizer.parent().ok_or(RunError::Install)?, uid, true)?;
        if digest(&bounded_read(tokenizer, MAX_TOKENIZER, uid)?)
            != request.profile().tokenizer_digest
        {
            return Err(RunError::Worker(Failure::ProfileChanged));
        }
        let host_namespaces = read_host_namespaces().map_err(|_| RunError::Isolation)?;
        let relay = Relay::bind_delegated(
            endpoint,
            request.profile().model_tag.clone(),
            request.profile().max_output_tokens,
        )
        .map_err(|_| RunError::Provider)?;
        let mut command = self.command(relay.directory(), tokenizer)?;
        let mut child = command.spawn().map_err(|_| RunError::Isolation)?;
        let mut stdin = child.stdin.take().ok_or(RunError::Isolation)?;
        let mut stdout =
            BufReader::with_capacity(8192, child.stdout.take().ok_or(RunError::Isolation)?);
        let end = time::Instant::now() + Duration::from_millis(u64::from(request.deadline_ms()));
        let host_seq = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(3));
        let cancel_seq = host_seq.clone();
        let exchange = async {
            let hello = time::timeout(
                Duration::from_secs(2),
                protocol::read_worker_frame(&mut stdout),
            )
            .await
            .map_err(|_| RunError::Isolation)?
            .map_err(|_| RunError::ProtocolMismatch)?;
            let Some(WorkerFrame::Hello { seq: 1, body }) = hello else {
                return Err(RunError::Isolation);
            };
            if body.build != self.manifest.worker_build
                || body.schema_digest != self.manifest.schema_digest
                || body.capabilities != vec![Capability::StructuredRole, Capability::Delegated]
            {
                return Err(RunError::ProtocolMismatch);
            }
            if !namespace_separation(&host_namespaces, &body.namespaces) {
                return Err(RunError::Isolation);
            }
            let capabilities = body.capabilities;
            protocol::write_host_frame(
                &mut stdin,
                HostFrame::HelloAck {
                    v: 1,
                    seq: 1,
                    body: Ack {
                        capabilities,
                        max_frame_bytes: protocol::MAX_FRAME,
                    },
                },
            )
            .await?;
            protocol::write_host_frame(
                &mut stdin,
                HostFrame::Execute {
                    v: 1,
                    seq: 2,
                    execution_id,
                    lease_id,
                    body: &request,
                },
            )
            .await?;

            let mut worker_seq = 3u32;
            let host_seq = host_seq.clone();
            let mut suspended = false;
            let completion = loop {
                let frame = protocol::read_worker_frame(&mut stdout).await?;
                let Some(frame) = frame else {
                    return Err(RunError::Protocol);
                };
                match frame {
                    WorkerFrame::AssistantTurn {
                        seq,
                        execution_id: execution,
                        lease_id: lease,
                        body,
                    } if seq == worker_seq && execution == execution_id && lease == lease_id => {
                        match host.accept_turn(body).await? {
                            HostDecision::TurnAccepted { turn_no, ordinals } => {
                                protocol::write_host_frame(
                                    &mut stdin,
                                    HostFrame::TurnAccepted {
                                        v: 1,
                                        seq: host_seq
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                                        execution_id,
                                        lease_id,
                                        body: &TurnAccepted { turn_no, ordinals },
                                    },
                                )
                                .await?;
                            }
                            HostDecision::Suspend(reason) => {
                                protocol::write_host_frame(
                                    &mut stdin,
                                    HostFrame::Suspend {
                                        v: 1,
                                        seq: host_seq
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                                        execution_id,
                                        lease_id,
                                        body: &Suspend { reason },
                                    },
                                )
                                .await?;
                                suspended = true;
                                break None;
                            }
                        }
                    }
                    WorkerFrame::ToolRequest {
                        seq,
                        execution_id: execution,
                        lease_id: lease,
                        body,
                    } if seq == worker_seq && execution == execution_id && lease == lease_id => {
                        let (turn_no, ordinal) = (body.turn_no, body.ordinal);
                        match host.resolve_tool(body).await? {
                            ToolDecision::Result(result) => {
                                let body = ToolResult {
                                    turn_no,
                                    ordinal,
                                    result,
                                };
                                protocol::write_host_frame(
                                    &mut stdin,
                                    HostFrame::ToolResult {
                                        v: 1,
                                        seq: host_seq
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                                        execution_id,
                                        lease_id,
                                        body: &body,
                                    },
                                )
                                .await?;
                            }
                            ToolDecision::Suspend(reason) => {
                                protocol::write_host_frame(
                                    &mut stdin,
                                    HostFrame::Suspend {
                                        v: 1,
                                        seq: host_seq
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                                        execution_id,
                                        lease_id,
                                        body: &Suspend { reason },
                                    },
                                )
                                .await?;
                                suspended = true;
                                break None;
                            }
                        }
                    }
                    WorkerFrame::Complete {
                        seq,
                        execution_id: execution,
                        lease_id: lease,
                        body,
                    } if seq == worker_seq
                        && execution == execution_id
                        && lease == lease_id
                        && request.matches(&body) =>
                    {
                        break Some(body);
                    }
                    WorkerFrame::Failed {
                        seq,
                        execution_id: execution,
                        lease_id: lease,
                        body,
                    } if seq == worker_seq && execution == execution_id && lease == lease_id => {
                        return Err(RunError::Worker(body.code));
                    }
                    _ => return Err(RunError::Protocol),
                }
                worker_seq += 1;
            };
            if protocol::read_worker_frame(&mut stdout).await?.is_some() {
                return Err(RunError::Protocol);
            }
            match completion {
                Some(completion) => {
                    validate_completion(&request, &completion)?;
                    Ok(completion)
                }
                None if suspended => Ok(Completion::Delegated {
                    observed: protocol::ObservedModel {
                        provider_version: request.profile().provider_version.clone(),
                        model_tag: request.profile().model_tag.clone(),
                        model_digest: request.profile().model_digest.clone(),
                    },
                    usage: protocol::Usage {
                        input_tokens: 0,
                        output_tokens: 0,
                    },
                    text: String::new(),
                }),
                None => Err(RunError::Protocol),
            }
        };

        let result = tokio::select! {
            biased;
            _ = cancel.wait_for(|requested| *requested) => Err(RunError::Cancelled),
            result = time::timeout_at(end, exchange) => result.unwrap_or(Err(RunError::Timeout)),
        };

        if matches!(result, Err(RunError::Cancelled | RunError::Timeout)) {
            let _ = time::timeout(
                Duration::from_millis(30),
                protocol::write_host_frame(
                    &mut stdin,
                    HostFrame::Cancel {
                        v: 1,
                        seq: cancel_seq.load(std::sync::atomic::Ordering::Relaxed),
                        execution_id,
                        lease_id,
                        body: Empty {},
                    },
                ),
            )
            .await;
        }
        drop(stdin);
        drop(stdout);
        if result.is_ok()
            && let Ok(Ok(status)) = time::timeout_at(end, child.wait()).await
        {
            return if status.success() {
                result
            } else {
                Err(RunError::Protocol)
            };
        }
        let _ = child.start_kill();
        let _ = time::timeout(Duration::from_secs(1), child.wait()).await;
        let outcome = result.and(Err(RunError::Timeout));
        relay.close().await;
        outcome
    }

    fn command(&self, relay: &Path, tokenizer: &Path) -> Result<Command, RunError> {
        let bwrap = fs::symlink_metadata(BWRAP).map_err(|_| RunError::Isolation)?;
        if !bwrap.is_file() || bwrap.uid() != 0 || bwrap.mode() & 0o6022 != 0 {
            return Err(RunError::Isolation);
        }
        let mut command = Command::new(BWRAP);
        command
            .env_clear()
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(
                if cfg!(debug_assertions)
                    && std::env::var_os("PARLEY_TEST_MANIFEST_SHA256").is_some()
                {
                    Stdio::inherit()
                } else {
                    Stdio::null()
                },
            );
        command.args([
            "--unshare-user",
            "--unshare-net",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--disable-userns",
            "--die-with-parent",
            "--cap-drop",
            "ALL",
            "--new-session",
            "--clearenv",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--size",
            "8388608",
            "--tmpfs",
            "/scratch",
            "--setenv",
            "HOME",
            "/scratch",
            "--setenv",
            "TMPDIR",
            "/scratch",
            "--chdir",
            "/scratch",
        ]);
        command.arg("--ro-bind").arg(&self.bundle).arg("/bundle");
        command.arg("--ro-bind").arg(relay).arg("/relay");
        command
            .arg("--ro-bind")
            .arg(tokenizer)
            .arg("/tokenizer/artifact.json");
        let loader = Path::new("/lib64/ld-linux-x86-64.so.2");
        mount_library(&mut command, loader, loader)?;
        let distro_paths = [
            Path::new("/lib/x86_64-linux-gnu"),
            Path::new("/usr/lib/x86_64-linux-gnu"),
            Path::new("/usr/lib"),
            Path::new("/lib64"),
        ];
        for library in LIBRARIES {
            let source = distro_paths
                .iter()
                .map(|path| path.join(library))
                .find(|path| path.is_file())
                .ok_or(RunError::Isolation)?;
            // Mount each loader dependency at its real path so the sandbox
            // works on the qualified Ubuntu host and on development hosts
            // whose libraries live in /usr/lib rather than the multiarch dir.
            let destination = source.parent().ok_or(RunError::Isolation)?.join(library);
            mount_library(&mut command, &source, &destination)?;
        }
        command.args([
            "--",
            "/bundle/node",
            "--permission",
            "--allow-net",
            "--allow-fs-read=/bundle",
            "--allow-fs-read=/tokenizer",
            "--allow-fs-read=/proc/self/ns",
            "--allow-fs-read=/scratch",
            "--allow-fs-write=/scratch",
            "--max-old-space-size=256",
            "/bundle/worker.mjs",
        ]);
        Ok(command)
    }
}

fn mount_library(command: &mut Command, source: &Path, destination: &Path) -> Result<(), RunError> {
    let actual = fs::canonicalize(source).map_err(|_| RunError::Isolation)?;
    let metadata = fs::metadata(&actual).map_err(|_| RunError::Isolation)?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(RunError::Isolation);
    }
    command.arg("--ro-bind").arg(actual).arg(destination);
    Ok(())
}

fn read_host_namespaces() -> io::Result<Namespaces> {
    fn name(kind: &str) -> io::Result<String> {
        Ok(fs::read_link(Path::new("/proc/self/ns").join(kind))?
            .to_string_lossy()
            .into_owned())
    }
    Ok(Namespaces {
        user: name("user")?,
        net: name("net")?,
        pid: name("pid")?,
        ipc: name("ipc")?,
        uts: name("uts")?,
    })
}

fn namespace_separation(host: &Namespaces, child: &Namespaces) -> bool {
    [
        ("user", &host.user, &child.user),
        ("net", &host.net, &child.net),
        ("pid", &host.pid, &child.pid),
        ("ipc", &host.ipc, &child.ipc),
        ("uts", &host.uts, &child.uts),
    ]
    .into_iter()
    .all(|(kind, old, new)| {
        new != old
            && new
                .strip_prefix(kind)
                .and_then(|tail| tail.strip_prefix(":["))
                .and_then(|tail| tail.strip_suffix(']'))
                .and_then(|id| id.parse::<u64>().ok())
                .is_some()
    })
}

fn validate_completion(request: &Execute, result: &Completion) -> Result<(), RunError> {
    let (observed, usage) = result.observation();
    let profile = request.profile();
    if observed.provider_version != profile.provider_version
        || observed.model_digest != profile.model_digest
        || observed.model_tag != profile.model_tag
    {
        return Err(RunError::Worker(Failure::ProfileChanged));
    }
    if usage.input_tokens == 0
        || usage.input_tokens > profile.context_tokens
        || usage.output_tokens == 0
        || usage.output_tokens > profile.max_output_tokens
    {
        return Err(RunError::Worker(Failure::ResultInvalid));
    }
    if let (
        Execute::StructuredRole {
            role,
            schema_digest,
            ..
        },
        Completion::StructuredRole { candidate, .. },
    ) = (request, result)
    {
        let schema = match role {
            protocol::Role::Coach => crate::inference::coach_schema(),
            protocol::Role::TemplateBuilder => crate::inference::builder_schema(),
            protocol::Role::Interviewer => crate::inference::interviewer_schema(),
            protocol::Role::Briefer => crate::inference::briefer_schema(),
        };
        if digest(schema) != *schema_digest {
            return Err(RunError::ProtocolMismatch);
        }
        let json = serde_json::from_slice(schema).map_err(|_| RunError::ProtocolMismatch)?;
        let bytes =
            serde_json::to_vec(candidate).map_err(|_| RunError::Worker(Failure::ResultInvalid))?;
        if bytes.len() > 128 * 1024 || !jsonschema::is_valid(&json, candidate) {
            return Err(RunError::Worker(Failure::ResultInvalid));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use sha2::Sha256;

    fn profile() -> super::protocol::Profile {
        super::protocol::Profile {
            revision: 2,
            provider_version: "0.18.0".into(),
            model_tag: "local:approved".into(),
            model_digest: "a".repeat(64),
            tokenizer_digest: "b".repeat(64),
            context_tokens: 8192,
            template_tokens: 128,
            max_output_tokens: 512,
        }
    }

    fn observed() -> super::protocol::ObservedModel {
        super::protocol::ObservedModel {
            provider_version: "0.18.0".into(),
            model_tag: "local:approved".into(),
            model_digest: "a".repeat(64),
        }
    }

    fn usage() -> super::protocol::Usage {
        super::protocol::Usage {
            input_tokens: 1024,
            output_tokens: 128,
        }
    }

    fn coach_request() -> Execute {
        Execute::StructuredRole {
            profile: profile(),
            deadline_ms: 30_000,
            role: super::protocol::Role::Coach,
            prompt_version: "coach-v2".into(),
            schema_digest: hex::encode(Sha256::digest(crate::inference::coach_schema())),
            system: "Use only the supplied seeds.".into(),
            input: serde_json::json!({"seeds": []}),
        }
    }

    fn candidate() -> Value {
        serde_json::json!({"questions":[{
            "question_key":"c".repeat(64),
            "text":"Which measure should be reviewed?"
        }]})
    }

    #[test]
    fn ar01_completion_is_profile_and_role_schema_bound() {
        let request = coach_request();
        let result = Completion::StructuredRole {
            observed: observed(),
            usage: usage(),
            candidate: candidate(),
        };
        assert!(request.matches(&result));
        assert!(validate_completion(&request, &result).is_ok());

        let wrong_model = Completion::StructuredRole {
            observed: super::protocol::ObservedModel {
                model_tag: "other".into(),
                ..observed()
            },
            usage: usage(),
            candidate: candidate(),
        };
        assert!(matches!(
            validate_completion(&request, &wrong_model),
            Err(RunError::Worker(Failure::ProfileChanged))
        ));
        let zero_usage = Completion::StructuredRole {
            observed: observed(),
            usage: super::protocol::Usage {
                input_tokens: 0,
                output_tokens: 1,
            },
            candidate: candidate(),
        };
        assert!(matches!(
            validate_completion(&request, &zero_usage),
            Err(RunError::Worker(Failure::ResultInvalid))
        ));
        let invalid_candidate = Completion::StructuredRole {
            observed: observed(),
            usage: usage(),
            candidate: serde_json::json!({"questions":[{"question_key":"bad","text":"question"}]}),
        };
        assert!(matches!(
            validate_completion(&request, &invalid_candidate),
            Err(RunError::Worker(Failure::ResultInvalid))
        ));
        let bad_digest = match request.clone() {
            Execute::StructuredRole {
                profile,
                deadline_ms,
                role,
                prompt_version,
                system,
                input,
                ..
            } => Execute::StructuredRole {
                profile,
                deadline_ms,
                role,
                prompt_version,
                schema_digest: "0".repeat(64),
                system,
                input,
            },
            Execute::Delegated { .. } | Execute::Probe { .. } => unreachable!(),
        };
        assert!(matches!(
            validate_completion(&bad_digest, &result),
            Err(RunError::ProtocolMismatch)
        ));
        assert!(!request.matches(&Completion::Probe {
            observed: observed(),
            usage: usage()
        }));
    }

    #[test]
    fn ar01_sandbox_namespaces_and_closed_diagnostics_are_checked() {
        let host = Namespaces {
            user: "user:[1]".into(),
            net: "net:[2]".into(),
            pid: "pid:[3]".into(),
            ipc: "ipc:[4]".into(),
            uts: "uts:[5]".into(),
        };
        let child = Namespaces {
            user: "user:[11]".into(),
            net: "net:[12]".into(),
            pid: "pid:[13]".into(),
            ipc: "ipc:[14]".into(),
            uts: "uts:[15]".into(),
        };
        assert!(namespace_separation(&host, &child));
        let same = Namespaces {
            net: host.net.clone(),
            ..child
        };
        assert!(!namespace_separation(&host, &same));
        for (error, code) in [
            (RunError::Install, "RUNNER_INSTALL_INVALID"),
            (RunError::ProtocolMismatch, "PROTOCOL_MISMATCH"),
            (RunError::Isolation, "ISOLATION_UNAVAILABLE"),
            (RunError::Protocol, "WORKER_PROTOCOL"),
            (RunError::Provider, "PROVIDER_FAILED"),
            (RunError::Cancelled, "CANCELLED"),
            (RunError::Timeout, "INFERENCE_TIMEOUT"),
        ] {
            assert_eq!(error.code(), code);
        }
    }

    #[test]
    fn ar01_installation_path_helpers_reject_symlinks_and_unsafe_members() {
        assert!(hexadecimal(&"a".repeat(64), 64));
        assert!(!hexadecimal(&"A".repeat(64), 64));
        assert!(safe_member(Path::new("licenses/index.json")));
        assert!(!safe_member(Path::new("../worker.mjs")));
        assert!(!safe_member(Path::new("/worker.mjs")));
        let root = std::env::temp_dir().join(format!("parley-supervisor-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let file = root.join("tokenizer.json");
        fs::write(&file, b"tokenizer").unwrap();
        let uid = effective_uid().unwrap();
        assert_eq!(bounded_read(&file, 32, uid).unwrap(), b"tokenizer");
        assert!(bounded_read(&file, 4, uid).is_err());
        let link = root.join("tokenizer-link.json");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(bounded_read(&link, 32, uid).is_err());
        assert!(secure_ancestry(&root, uid, true).is_ok());
        assert!(list_members(&root, &root, uid, &mut BTreeSet::new()).is_err());
        fs::remove_file(&link).unwrap();
        assert!(list_members(&root, &root, uid, &mut BTreeSet::new()).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}
