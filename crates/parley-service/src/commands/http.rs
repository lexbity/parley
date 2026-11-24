//! Command HTTP transport (UCR-001 §4.1).
//!
//! Transport-only parsing, session/origin checks, DTO translation, status
//! mapping and response privacy live here; policy and operation integrity
//! live behind the registry and application layers.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Connection, Sqlite};
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::approval::{self, ApprovalStatus, DecisionAction, DecisionError};
use super::descriptor::CommandDescriptor;
use super::jobs::{self, RunError as JobRunError, run as run_jobs};
use super::ledger::{
    AdmitError, Mode, PolicyResult, Timing, admit_attempt, finalize_attempt, key_digest,
    lookup_receipt,
};
use super::opportunity::{self, RunError as CreateRunError};
use super::party::{PartyCommand, RunError as PartyRunError, run as run_party};
use super::phase::{PhaseCommand, RunError as PhaseRunError, run as run_phase};
use super::policy::Effect;
use super::resource::{ResourceCommand, RunError as ResourceRunError};
use super::task::{self, RunError as TaskRunError, run as run_task};
use super::writing::{RunError as WritingRunError, WritingCommand};
use super::{Actor, ActorKind, Registry, audit_args, validator_for};
use crate::application::brief::BriefError;
use crate::application::opportunity::CreateError;
use crate::application::opportunity_task::TaskError;
use crate::application::party_edit::PartyEditError;
use crate::application::phase::PhaseError;
use crate::application::resource_retirement::RetirementError;
use crate::application::writer::{Failure as WriterFailure, JobResult, WriteOutcome, Writer};
use crate::{AppState, CommandSignal, OwnerPrincipal, now_utc, private_headers};

pub(crate) const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_JSON_DEPTH: usize = 16;
pub(crate) const QUERY_MAX_COMMANDS: usize = 20;
pub(crate) const SEARCH_MAX_RESULTS: usize = 25;
pub(crate) const SEARCH_DEFAULT_LIMIT: usize = 10;
pub(crate) const MAX_SEARCH_BYTES: usize = 256;
pub(crate) const MAX_CONTEXT_KEYS: usize = 16;
pub(crate) const MAX_CONTEXT_BYTES: usize = 4096;
/// NFR-3: cap the retained result body at 64 KiB.
pub(crate) const MAX_RESULT_BYTES: i64 = 64 * 1024;
/// NFR-4: executed effects per delegated run (both write counters).
pub(crate) const MAX_EXECUTED_WRITES_PER_RUN: i64 = 10;
/// NFR-4: destructive effects per delegated run.
pub(crate) const MAX_DESTRUCTIVE_EFFECTS_PER_RUN: i64 = 3;
/// NFR-4: invocation window for a delegated run.
const BUDGET_WINDOW_SECONDS: i64 = 60;

/// A validated operation output, or a normalized terminal response
/// `(status, code, message, details)`. The success tuple is
/// `(result, bytes, effected)` where `effected` is false for a true no-op.
type CheckedResult = Result<(Value, i64, bool), (i64, &'static str, String, Vec<Value>)>;

/// Registered write operations dispatched inside the savepoint.
enum Execution {
    Create,
    Phase(PhaseCommand),
    Party(PartyCommand),
    Task,
    Brief,
    Resource(ResourceCommand),
    Writing(WritingCommand),
}

enum OperationError {
    Phase(PhaseRunError),
    Create(CreateRunError),
    Party(PartyRunError),
    Task(TaskRunError),
    Jobs(JobRunError),
    Resource(ResourceRunError),
    Writing(WritingRunError),
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/commands/query", post(query))
        .route("/api/v1/commands/search", post(search))
        .route("/api/v1/commands/{id}", get(describe).post(run))
        .route("/api/v1/approvals", get(list_approvals))
        .route("/api/v1/approvals/{id}", get(get_approval))
        .route("/api/v1/approvals/{id}/grant", post(grant_approval))
        .route("/api/v1/approvals/{id}/deny", post(deny_approval))
        .route("/api/v1/command-notifications", get(notifications))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryInput {
    surface: String,
    #[serde(default)]
    context: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    query: String,
    limit: Option<usize>,
    #[serde(default)]
    context: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvocationInput {
    version: u32,
    args: Value,
    #[serde(default)]
    dry_run: bool,
    /// Opaque approval id presented when resuming a frozen human invocation
    /// that requires approval (U08 / §4.1). Only valid for destructive
    /// commands.
    #[serde(default)]
    approval_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunReply {
    status: i64,
    ok: bool,
    inner: Value,
    replayed: bool,
    original_audit_id: Option<String>,
    /// The approval id behind a `403 approval_required` outcome, used by the
    /// transport to emit the `Location` header. Never serialized into the
    /// public body.
    #[serde(skip)]
    approval_id: Option<String>,
}

fn json_depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(map) => 1 + map.values().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

fn context_allowed(key: &str) -> bool {
    if key == "route" {
        return true;
    }
    if let Some(rest) = key.strip_prefix("selection.") {
        return !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    }
    false
}

#[allow(clippy::result_large_err)] // axum Response is the transport error type
async fn auth_owner(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<(OwnerPrincipal, Arc<Registry>, Arc<Writer>), Response> {
    match crate::authenticate_owner(headers, state).await {
        Ok(principal) => Ok((principal, state.registry.clone(), state.writer.clone())),
        Err(error) => Err((error.0, error.1).into_response()),
    }
}

async fn query(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let (principal, registry, _writer) = match auth_owner(&headers, &state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let _ = principal;
    if body.len() > MAX_REQUEST_BYTES {
        return error403(
            &state,
            "request_too_large",
            "Request body exceeds 1 MiB",
            413,
        );
    }
    let parsed: Value = match crate::decode_unique(&body) {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                &state,
                400,
                "malformed_request",
                "Request body is not valid JSON",
            );
        }
    };
    if json_depth(&parsed) > MAX_JSON_DEPTH {
        return error_response(
            &state,
            400,
            "malformed_request",
            "Payload nesting exceeds 16 levels",
        );
    }
    let input: QueryInput = match serde_json::from_value(parsed) {
        Ok(value) => value,
        Err(_) => {
            return error_response(&state, 400, "malformed_request", "Malformed query request");
        }
    };
    if input.surface != "palette" {
        return error_response(
            &state,
            403,
            "forbidden",
            "Owner requests must use the palette surface",
        );
    }
    if input.context.len() > MAX_CONTEXT_KEYS {
        return error_response(&state, 400, "malformed_request", "Context exceeds 16 keys");
    }
    let context_bytes: usize = input
        .context
        .iter()
        .map(|(key, value)| key.len() + value.to_string().len())
        .sum();
    if context_bytes > MAX_CONTEXT_BYTES {
        return error_response(&state, 400, "malformed_request", "Context exceeds 4 KiB");
    }
    for key in input.context.keys() {
        if !context_allowed(key) {
            return error_response(
                &state,
                400,
                "malformed_request",
                "Context key is not allowlisted",
            );
        }
    }

    let mut commands: Vec<Value> = Vec::new();
    let mut truncated = false;
    for descriptor in registry.owner_tier1() {
        if let Some(when) = descriptor.when {
            let expr = super::descriptor::parse_expr(when).expect("boot-validated expression");
            if !super::descriptor::eval_expr(&expr, &input.context) {
                continue;
            }
        }
        if commands.len() >= super::descriptor::MANIFEST_MAX_COMMANDS.min(QUERY_MAX_COMMANDS) {
            truncated = true;
            break;
        }
        commands.push(serde_json::to_value(descriptor).unwrap_or_default());
    }
    let mut response = Json(json!({
        "contract_major": registry.contract_major(),
        "manifest_version": registry.tier1_manifest(),
        "truncated": truncated,
        "commands": commands,
    }))
    .into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

async fn search(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let (principal, registry, _writer) = match auth_owner(&headers, &state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let _ = principal;
    if body.len() > MAX_REQUEST_BYTES {
        return error403(
            &state,
            "request_too_large",
            "Request body exceeds 1 MiB",
            413,
        );
    }
    let parsed: Value = match crate::decode_unique(&body) {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                &state,
                400,
                "malformed_request",
                "Request body is not valid JSON",
            );
        }
    };
    if json_depth(&parsed) > MAX_JSON_DEPTH {
        return error_response(
            &state,
            400,
            "malformed_request",
            "Payload nesting exceeds 16 levels",
        );
    }
    let input: SearchInput = match serde_json::from_value(parsed) {
        Ok(value) => value,
        Err(_) => {
            return error_response(&state, 400, "malformed_request", "Malformed search request");
        }
    };
    if input.query.len() > MAX_SEARCH_BYTES {
        return error_response(&state, 400, "malformed_request", "Query exceeds 256 bytes");
    }
    let limit = match input.limit.unwrap_or(SEARCH_DEFAULT_LIMIT) {
        0 => {
            return error_response(
                &state,
                400,
                "malformed_request",
                "Limit must be between 1 and 25",
            );
        }
        value if value > SEARCH_MAX_RESULTS => {
            return error_response(
                &state,
                400,
                "malformed_request",
                "Limit must be between 1 and 25",
            );
        }
        value => value,
    };
    for key in input.context.keys() {
        if !context_allowed(key) {
            return error_response(
                &state,
                400,
                "malformed_request",
                "Context key is not allowlisted",
            );
        }
    }

    let query = input.query.trim().to_lowercase();
    let mut hits: Vec<&CommandDescriptor> = registry.owner_tier1();
    hits.sort_by(|a, b| {
        rank(a, &query)
            .cmp(&rank(b, &query))
            .then_with(|| a.id.cmp(b.id))
    });
    hits.truncate(limit);

    let items: Vec<Value> = hits
        .into_iter()
        .map(|descriptor| {
            json!({
                "id": descriptor.id,
                "version": descriptor.version,
                "title": descriptor.title,
                "summary": descriptor.summary,
                "effect": descriptor.effect,
            })
        })
        .collect();
    let mut response = Json(json!({
        "items": items,
        "total": items.len(),
        "next_cursor": null,
    }))
    .into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

/// §5.2 palette search order: exact title, title prefix, token prefix,
/// ordered subsequence with fewest skipped characters, then ID.
fn rank(descriptor: &CommandDescriptor, query: &str) -> (u8, usize) {
    if query.is_empty() {
        return (5, 0);
    }
    let title = descriptor.title.to_lowercase();
    if title == query {
        return (0, 0);
    }
    if title.starts_with(query) {
        return (1, 0);
    }
    if title
        .split_whitespace()
        .any(|token| token.starts_with(query))
    {
        return (2, 0);
    }
    if let Some(skipped) = subsequence_skips(&title, query) {
        return (3, skipped);
    }
    (4, 0)
}

fn subsequence_skips(text: &str, query: &str) -> Option<usize> {
    let text_chars: Vec<char> = text.chars().collect();
    let query_chars: Vec<char> = query.chars().collect();
    if query_chars.is_empty() {
        return Some(0);
    }
    let mut ti = 0;
    let mut skipped = 0;
    let mut first = true;
    for &target in &query_chars {
        while ti < text_chars.len() && text_chars[ti] != target {
            ti += 1;
            if !first {
                skipped += 1;
            }
        }
        if ti >= text_chars.len() {
            return None;
        }
        first = false;
        ti += 1;
    }
    Some(skipped)
}

async fn describe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let (principal, registry, _writer) = match auth_owner(&headers, &state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let _ = principal;
    let Some(descriptor) = registry.command(&id) else {
        return error403(&state, "unknown_command", "Command is not available", 404);
    };
    if !descriptor.is_owner_visible() {
        return error403(&state, "unknown_command", "Command is not available", 404);
    }
    let mut response = Json(serde_json::to_value(descriptor).unwrap_or_default()).into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

async fn run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let (principal, registry, _writer) = match auth_owner(&headers, &state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    if body.len() > MAX_REQUEST_BYTES {
        return error403(
            &state,
            "request_too_large",
            "Request body exceeds 1 MiB",
            413,
        );
    }
    let key = match crate::command_operation_key(&headers) {
        Ok(key) => key,
        Err((status, code, message)) => {
            return error_response(&state, status.as_u16(), code, message);
        }
    };
    let fault = crate::application::writer::fault_from_key(&key);
    let parsed: Value = match crate::decode_unique(&body) {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                &state,
                400,
                "malformed_request",
                "Request body is not valid JSON",
            );
        }
    };
    if json_depth(&parsed) > MAX_JSON_DEPTH {
        return error_response(
            &state,
            400,
            "malformed_request",
            "Payload nesting exceeds 16 levels",
        );
    }
    let input: InvocationInput = match serde_json::from_value(parsed) {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                &state,
                400,
                "malformed_request",
                "Malformed invocation request",
            );
        }
    };
    let command = match registry.command(&id) {
        Some(command) => command,
        None => return error403(&state, "unknown_command", "Command is not available", 404),
    };
    if !command.is_owner_visible() {
        return error403(&state, "unknown_command", "Command is not available", 404);
    }
    if input.version != command.version {
        return error403(
            &state,
            "version_mismatch",
            "Command version is stale; re-describe and collect fresh consent",
            409,
        );
    }
    if input.dry_run && !command.supports_dry_run {
        return error403(
            &state,
            "invalid_args",
            "This command does not support dry run",
            422,
        );
    }
    if !input.args.is_object() {
        return error403(&state, "invalid_args", "Args must be an object", 422);
    }
    if input.approval_id.is_some() && command.effect != Effect::Destructive {
        return error403(
            &state,
            "invalid_args",
            "approval_id is only valid for destructive commands",
            422,
        );
    }
    let actor = Actor::owner(principal.pseudonym.clone());
    submit(
        &state,
        command,
        &input,
        &key,
        actor,
        body.len() as i64,
        fault,
    )
    .await
}

/// Submit a prepared invocation through the shared registry pipeline. Both
/// the owner HTTP adapter and the in-process agent tool adapter call this;
/// neither bypasses admission, authorization, audit or the writer.
#[allow(clippy::needless_borrow)] // shared adapter: state may be owned or borrowed
async fn submit(
    state: &AppState,
    command: &CommandDescriptor,
    input: &InvocationInput,
    key: &str,
    actor: Actor,
    request_bytes: i64,
    fault: &'static str,
) -> Response {
    let args_value = input.args.clone();
    let wire_digest = match parley_domain::jcs::canonicalize_value(&args_value) {
        Ok(canonical) => crate::digest(canonical.as_bytes()),
        Err(_) => {
            return error_response(
                &state,
                400,
                "malformed_request",
                "Args are not canonicalizable",
            );
        }
    };
    // The canonical  binds normalized typed args with explicit defaults
    // (§4.2). For commands without defaults it equals the wire digest.
    let canonical_digest = if command.effect == Effect::Destructive {
        super::resource::canonical_args(&args_value)
            .and_then(|value| parley_domain::jcs::canonicalize_value(&value).ok())
            .map(|canonical| crate::digest(canonical.as_bytes()))
            .unwrap_or_else(|| wire_digest.clone())
    } else {
        wire_digest.clone()
    };
    let mode = if input.dry_run {
        Mode::DryRun
    } else {
        Mode::Execute
    };
    let epoch = state.workspace.epoch().to_string();
    let principal_pseudonym = actor.pseudonym.clone();
    let generation_secret = state.workspace.command_secret();
    let key_digest = key_digest(
        &generation_secret,
        &epoch,
        &principal_pseudonym,
        command.id,
        &key,
    );
    let args_json = audit_args(command.id, args_value.as_object().expect("checked object"));
    let command_id = command.id.to_string();
    let command_version = command.version;
    let args_schema = command.args_schema;
    let result_schema = command.result_schema;
    let effect = command.effect;
    let requires_self_created = opportunity::requires_self_created_grant(command.id);

    let audit_id = match admit_attempt(
        state.workspace.pool(),
        &epoch,
        &actor,
        &command_id,
        command_version,
        mode,
        PolicyResult::Allowed,
        Some(&key_digest),
        &args_json,
        request_bytes,
    )
    .await
    {
        Ok(audit_id) => audit_id,
        Err(error) => {
            return match error {
                AdmitError::RateLimited { limit } => error403(
                    &state,
                    "budget_exceeded",
                    &format!("Rate limit exceeded ({limit}/min)"),
                    429,
                ),
                AdmitError::BudgetExceeded => error403(
                    &state,
                    "budget_exceeded",
                    "Command ledger capacity is exhausted",
                    503,
                ),
                AdmitError::Unavailable => error403(
                    &state,
                    "unavailable",
                    "The command ledger is temporarily unavailable",
                    503,
                ),
            };
        }
    };

    let timing = Timing::start();
    let generation = state.workspace.generation_token().await;
    // Retained outside the `move` job closure so a rejected attempt still
    // receives its terminal audit outcome and its correlation ID.
    let rejection_epoch = epoch.clone();
    let rejection_command = command_id.clone();
    let audit_id_for_job = audit_id.clone();
    let job_actor = actor.clone();
    let permission = command.requires_permission.as_str();
    let approval_id = input.approval_id.clone();
    let job = crate::application::writer::WriterJob {
        deadline: Instant::now() + crate::application::writer::EXECUTION_DEADLINE,
        generation,
        run: Box::new(
            move |connection: &mut sqlx::sqlite::SqliteConnection, deadline: Instant| {
                let epoch = epoch.clone();
                let actor = job_actor.clone();
                let command_id = command_id.clone();
                let key_digest = key_digest.clone();
                let wire_digest = wire_digest.clone();
                let canonical_digest = canonical_digest.clone();
                let audit_id = audit_id_for_job.clone();
                let args = args_value.clone();
                let result_schema = result_schema;
                let approval_id = approval_id.clone();
                let fault = fault;
                Box::pin(async move {
                    let mut tx = match connection.begin_with("BEGIN IMMEDIATE").await {
                        Ok(tx) => tx,
                        Err(_) => {
                            return JobResult {
                                commit: false,
                                body: serde_json::Value::Null,
                                failure: Some(crate::application::writer::Failure::Unavailable),
                            };
                        }
                    };
                    let reply = match pipeline(
                        &mut tx,
                        &epoch,
                        &actor,
                        &command_id,
                        command_version,
                        mode,
                        &key_digest,
                        &wire_digest,
                        &canonical_digest,
                        &audit_id,
                        &args,
                        args_schema,
                        result_schema,
                        permission,
                        effect,
                        requires_self_created,
                        approval_id,
                    )
                    .await
                    {
                        Ok(reply) => reply,
                        Err(failure) => {
                            return JobResult {
                                commit: false,
                                body: serde_json::Value::Null,
                                failure: Some(failure),
                            };
                        }
                    };
                    match crate::application::writer::commit_tx(tx, deadline, fault).await {
                        Ok(()) => {
                            let mut body = serde_json::to_value(&reply).unwrap_or_default();
                            // The approval id is transport metadata, never part
                            // of the public command body; carry it out-of-band
                            // for the Location header.
                            if let Some(approval_id) = &reply.approval_id {
                                body["_approval_id"] =
                                    serde_json::Value::String(approval_id.clone());
                            }
                            JobResult {
                                commit: true,
                                body,
                                failure: None,
                            }
                        }
                        Err(failure) => JobResult {
                            commit: false,
                            body: serde_json::Value::Null,
                            failure: Some(failure),
                        },
                    }
                })
            },
        ),
        reply: tokio::sync::oneshot::channel().0,
    };
    match state.writer.run(job).await {
        WriteOutcome::Committed(body) => {
            let approval_id = body
                .get("_approval_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let mut body = body;
            if let Some(object) = body.as_object_mut() {
                object.remove("_approval_id");
            }
            let reply: RunReply = serde_json::from_value(body).unwrap_or(RunReply {
                status: 500,
                ok: false,
                inner: json!({"code": "internal", "message": "invariant failure", "details": []}),
                replayed: false,
                original_audit_id: None,
                approval_id: None,
            });
            let reply = RunReply {
                approval_id,
                ..reply
            };
            // Publish the approval notification only after the commit is
            // known (Q3). Owner inboxes poll on reconnect; the SSE stream
            // picks the signal up from the bounded broadcast channel.
            if command.effect == Effect::Destructive || reply.approval_id.is_some() {
                let _ = state.notify.send(CommandSignal::ApprovalsChanged);
            }
            let elapsed = timing.elapsed_ms();
            if reply.ok {
                // Publish the worker wake only after the commit is known
                // (UCR-001 Q3). A replay of the same job id wakes again;
                // an idempotent wake is harmless because the durable queue
                // and its 500 ms poll remain authoritative.
                if let Some(job_id) = reply.inner.get("job_id").and_then(|v| v.as_str()) {
                    crate::jobs::wake(&state.jobs, job_id);
                }
                let mut response = Json(json!({
                    "ok": true,
                    "command_version": command_version,
                    "audit_id": audit_id,
                    "original_audit_id": reply.original_audit_id,
                    "replayed": reply.replayed,
                    "dry_run": input.dry_run,
                    "result": reply.inner,
                }))
                .into_response();
                if let Some(approval_id) = &reply.approval_id
                    && let Ok(location) = axum::http::HeaderValue::from_str(&format!(
                        "/api/v1/approvals/{approval_id}"
                    ))
                {
                    response.headers_mut().insert(header::LOCATION, location);
                }
                private_headers(&mut response, state.workspace.epoch());
                let _ = elapsed;
                response
            } else {
                let code = reply
                    .inner
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("internal");
                let message = reply
                    .inner
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("command failed");
                let details = reply
                    .inner
                    .get("details")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                let status = StatusCode::from_u16(reply.status as u16)
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let mut response = Json(json!({
                    "ok": false,
                    "command_version": command_version,
                    "audit_id": audit_id,
                    "error": {"code": code, "message": message, "details": details}
                }))
                .into_response();
                *response.status_mut() = status;
                if let Some(approval_id) = &reply.approval_id
                    && let Ok(location) = axum::http::HeaderValue::from_str(&format!(
                        "/api/v1/approvals/{approval_id}"
                    ))
                {
                    response.headers_mut().insert(header::LOCATION, location);
                }
                private_headers(&mut response, state.workspace.epoch());
                let _ = elapsed;
                response
            }
        }
        WriteOutcome::Rejected(failure) => {
            let (status, code, message, outcome) = match failure {
                WriterFailure::Unavailable | WriterFailure::AdmissionTimeout => (
                    503,
                    "unavailable",
                    "Command execution is temporarily unavailable; retry with the same key",
                    "unavailable",
                ),
                WriterFailure::Timeout => {
                    (504, "timeout", "Command execution timed out", "timeout")
                }
                WriterFailure::GenerationChanged => (
                    409,
                    "epoch_changed",
                    "Workspace changed; reconnect before continuing",
                    "epoch_changed",
                ),
            };
            super::ledger::record_rejection(
                state.workspace.pool(),
                &rejection_epoch,
                &audit_id,
                &actor,
                &rejection_command,
                command_version,
                mode,
                outcome,
            )
            .await;
            let mut response = Json(json!({
                "ok": false,
                "command_version": command_version,
                "audit_id": audit_id,
                "error": {"code": code, "message": message, "details": []}
            }))
            .into_response();
            *response.status_mut() =
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            if status == 503 {
                response.headers_mut().insert(
                    header::RETRY_AFTER,
                    axum::http::HeaderValue::from_static("1"),
                );
            }
            private_headers(&mut response, state.workspace.epoch());
            response
        }
    }
}

/// In-process agent invocation. The actor is constructed only from a durable
/// run grant by `agent_runtime::grants`; no HTTP credential is accepted here.
/// Returns `(status, body, approval_id)` where `approval_id` is present for
/// a `403 approval_required` outcome so the runtime can park the run on the
/// durable approval (Q7). The approval id never reaches the model.
pub(crate) async fn invoke_agent(
    state: &AppState,
    actor: Actor,
    id: &str,
    version: u32,
    key: &str,
    args: Value,
) -> (u16, Value, Option<String>) {
    let registry = state.registry.clone();
    let command = match registry.agent_visible(id, &actor.permissions) {
        Some(command) => command,
        None => {
            return (
                404,
                json!({"ok": false, "error": {"code": "unknown_command", "message": "Command is not available", "details": []}}),
                None,
            );
        }
    };
    if version != command.version {
        return (
            409,
            json!({"ok": false, "error": {"code": "version_mismatch", "message": "Command version is stale", "details": []}}),
            None,
        );
    }
    let input = InvocationInput {
        version,
        args,
        dry_run: false,
        approval_id: None,
    };
    let response = submit(state, command, &input, key, actor, 0, "").await;
    let status = response.status().as_u16();
    let approval_id = response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|location| location.strip_prefix("/api/v1/approvals/"))
        .map(str::to_owned);
    let body = axum::body::to_bytes(response.into_body(), MAX_REQUEST_BYTES)
        .await
        .unwrap_or_default();
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (
        status,
        value.get("data").cloned().unwrap_or(value),
        approval_id,
    )
}

/// Re-check a delegated grant inside the reserved transaction, on both the
/// replay and fresh-execution paths. The grant rows are immutable while the
/// run is active and a cancellation revokes them under the same writer.
#[allow(clippy::type_complexity)] // bounded run/grant row
async fn agent_scope_denial(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    actor: &Actor,
    required_permission: &str,
    args: &Value,
    requires_self_created: bool,
) -> Result<Option<(i64, &'static str, String)>, crate::application::writer::Failure> {
    if actor.kind == ActorKind::Owner {
        return Ok(None);
    }
    let failure = crate::application::writer::Failure::Unavailable;
    let run_id = actor.run_id.as_deref().unwrap_or("");
    let row: Option<(String, String, Option<String>, Option<String>, String, String)> =
        sqlx::query_as(
            "SELECT state,epoch,lease_token,lease_deadline,grant_json,deadline_at FROM agent_run WHERE run_id=?",
        )
        .bind(run_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| failure)?;
    let Some((state, run_epoch, lease_token, lease_deadline, grant_json, deadline_at)) = row else {
        return Ok(Some((
            404,
            "target_unavailable",
            "Run is not available".into(),
        )));
    };
    let now = crate::now_utc();
    let expired = |value: Option<&String>| {
        value
            .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
            .map(|time| time < chrono::Utc::now())
            .unwrap_or(true)
    };
    if run_epoch != actor.epoch
        || state != "running"
        || lease_token.as_deref() != actor.lease_token.as_deref()
        || expired(lease_deadline.as_ref())
        || expired(Some(&deadline_at))
    {
        return Ok(Some((
            409,
            "epoch_changed",
            "The run grant is no longer active".into(),
        )));
    }
    let grant: Value = serde_json::from_str(&grant_json).map_err(|_| failure)?;
    let allowed = grant
        .get("permissions")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .any(|value| value.as_str() == Some(required_permission))
        })
        .unwrap_or(false);
    if !allowed
        || (requires_self_created
            && (!actor.allow_created_opportunities
                || !grant
                    .get("allow_created_opportunities")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)))
    {
        // A concealed command, a missing created-target grant and a concealed
        // target share one public denial so none can be probed via tools.
        return Ok(Some((
            404,
            "unknown_command",
            "Command is not available".into(),
        )));
    }
    if let Some((field, id)) = actor.target_field(args) {
        let kind = match field {
            "party_id" | "primary_party_id" => "party",
            "opportunity_id" => "opportunity",
            "resource_id" => "resource",
            "material_id" => "material",
            _ => "",
        };
        let granted: Option<String> = if kind.is_empty() {
            None
        } else {
            sqlx::query_scalar(
                "SELECT target_id FROM agent_run_target WHERE run_id=? AND kind=? AND target_id=?",
            )
            .bind(run_id)
            .bind(kind)
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| failure)?
        };
        if granted.is_none() {
            return Ok(Some((
                404,
                "target_unavailable",
                "Target record is not available".into(),
            )));
        }
    }
    refresh_budget_window(tx, run_id, &now).await?;
    let invocations: i64 =
        sqlx::query_scalar("SELECT invocations FROM agent_run_budget WHERE run_id=?")
            .bind(run_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| failure)?
            .unwrap_or(0);
    if invocations >= super::ledger::RUN_RATE_PER_MINUTE {
        return Ok(Some((
            429,
            "budget_exceeded",
            "Run invocation budget is exhausted".into(),
        )));
    }
    Ok(None)
}

/// The executed-effect allowance is checked only on a fresh execution path,
/// after a same-key replay has been ruled out. An authorized replay returns its
/// original result without consuming another effect (UCR-001 Q2). Destructive
/// effects burn both executed counters (NFR-4).
async fn effect_allowance_denial(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    actor: &Actor,
    effect: Effect,
    mode: Mode,
) -> Result<Option<(i64, &'static str, String)>, crate::application::writer::Failure> {
    if actor.kind != ActorKind::Agent || effect == Effect::Read || mode != Mode::Execute {
        return Ok(None);
    }
    let run_id = actor.run_id.as_deref().unwrap_or("");
    let (executed_writes, destructive_effects): (i64, i64) = sqlx::query_as(
        "SELECT executed_writes, destructive_effects FROM agent_run_budget WHERE run_id=?",
    )
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| crate::application::writer::Failure::Unavailable)?
    .unwrap_or((0, 0));
    if executed_writes >= MAX_EXECUTED_WRITES_PER_RUN {
        return Ok(Some((
            429,
            "budget_exceeded",
            "Run executed-write allowance is exhausted".into(),
        )));
    }
    if effect == Effect::Destructive && destructive_effects >= MAX_DESTRUCTIVE_EFFECTS_PER_RUN {
        return Ok(Some((
            429,
            "budget_exceeded",
            "Run destructive-effect allowance is exhausted".into(),
        )));
    }
    Ok(None)
}

/// A run's invocation counters use a trusted UTC window. A backward clock jump
/// cannot extend a grant or a rate window; the deadline was already checked
/// against the persisted high-water deadline above.
async fn refresh_budget_window(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    run_id: &str,
    now: &str,
) -> Result<(), crate::application::writer::Failure> {
    let failure = crate::application::writer::Failure::Unavailable;
    let updated: Option<String> =
        sqlx::query_scalar("SELECT updated_at FROM agent_run_budget WHERE run_id=?")
            .bind(run_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| failure)?;
    let current = chrono::DateTime::parse_from_rfc3339(now)
        .map(|value| value.with_timezone(&chrono::Utc))
        .map_err(|_| failure)?;
    let stale = updated
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| {
            current.timestamp() - value.timestamp() >= BUDGET_WINDOW_SECONDS
                || value.timestamp() > current.timestamp()
        })
        .unwrap_or(true);
    if stale {
        sqlx::query(
            "UPDATE agent_run_budget SET invocations=0, distinct_intents=0, updated_at=? WHERE run_id=?",
        )
        .bind(now)
        .bind(run_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| failure)?;
    }
    Ok(())
}

/// Charge one admitted fresh invocation. Replays never reach this function;
/// they still count against the admission rate in `admit_attempt`.
async fn charge_invocation(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    actor: &Actor,
) -> Result<(), crate::application::writer::Failure> {
    if actor.kind != ActorKind::Agent {
        return Ok(());
    }
    sqlx::query(
        "UPDATE agent_run_budget SET invocations=invocations+1, distinct_intents=distinct_intents+1, updated_at=? WHERE run_id=?",
    )
    .bind(now_utc())
    .bind(actor.run_id.as_deref().unwrap_or(""))
    .execute(&mut **tx)
    .await
    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
    Ok(())
}

/// Debit the executed-write allowance only after a committed effect.
/// Destructive effects also burn the destructive-effect counter (NFR-4).
async fn debit_effect(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    actor: &Actor,
    effect: Effect,
) -> Result<(), crate::application::writer::Failure> {
    if actor.kind != ActorKind::Agent {
        return Ok(());
    }
    if effect == Effect::Destructive {
        sqlx::query(
            "UPDATE agent_run_budget SET executed_writes=executed_writes+1, destructive_effects=destructive_effects+1, updated_at=? WHERE run_id=?",
        )
        .bind(now_utc())
        .bind(actor.run_id.as_deref().unwrap_or(""))
        .execute(&mut **tx)
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(());
    }
    sqlx::query(
        "UPDATE agent_run_budget SET executed_writes=executed_writes+1, updated_at=? WHERE run_id=?",
    )
    .bind(now_utc())
    .bind(actor.run_id.as_deref().unwrap_or(""))
    .execute(&mut **tx)
    .await
    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn pipeline(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    epoch: &str,
    actor: &Actor,
    command_id: &str,
    command_version: u32,
    mode: Mode,
    key_digest: &str,
    wire_digest: &str,
    canonical_digest: &str,
    audit_id: &str,
    args: &Value,
    args_schema: &'static Value,
    result_schema: &'static Value,
    required_permission: &'static str,
    effect: Effect,
    requires_self_created: bool,
    approval_id: Option<String>,
) -> Result<RunReply, crate::application::writer::Failure> {
    // Recheck the epoch under reservation.
    let current_epoch: Option<String> =
        sqlx::query_scalar("SELECT epoch FROM workspace_meta WHERE singleton=1")
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| crate::application::writer::Failure::Unavailable)?
            .flatten();
    if current_epoch.as_deref() != Some(epoch) {
        return Ok(RunReply {
            status: 409,
            ok: false,
            inner: json!({"code": "epoch_changed", "message": "Workspace changed; reconnect before continuing", "details": []}),
            replayed: false,
            original_audit_id: None,

            approval_id: None,
        });
    }

    // Authorize every retry against current authority *before* looking up a
    // receipt. A revoked run never replays (Q2).
    if let Some((status, code, message)) =
        agent_scope_denial(tx, actor, required_permission, args, requires_self_created).await?
    {
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            PolicyResult::DomainError,
            Some(code),
            Some(audit_id),
            Some(code),
            Some(&message),
            0,
            0,
            None,
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status,
            ok: false,
            inner: json!({
                "code": code,
                "message": message,
                "details": []
            }),
            replayed: false,
            original_audit_id: None,

            approval_id: None,
        });
    }

    // Replay path: same key, same intent.
    if let Some(record) = lookup_receipt(tx, epoch, &actor.pseudonym, command_id, key_digest)
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?
    {
        if record.command_version == command_version as i64
            && record.mode == mode.as_str()
            && record.wire_args_digest == wire_digest
        {
            finalize_attempt(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                PolicyResult::Replayed,
                Some("replayed"),
                Some(&record.original_audit_id),
                None,
                None,
                0,
                0,
                None,
            )
            .await
            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            return Ok(RunReply {
                status: record.status,
                ok: record.status == 200,
                inner: record.body,
                replayed: true,
                original_audit_id: Some(record.original_audit_id),
                approval_id: None,
            });
        }
        // Same key, different intent: terminal mismatch for this key.
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            PolicyResult::DomainError,
            Some("idempotency_mismatch"),
            Some(audit_id),
            Some("idempotency_mismatch"),
            Some("Idempotency key was used for a different command"),
            0,
            0,
            None,
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status: 422,
            ok: false,
            inner: json!({
                "code": "idempotency_mismatch",
                "message": "Idempotency key was used for a different command",
                "details": []
            }),
            replayed: false,
            original_audit_id: None,

            approval_id: None,
        });
    }

    if let Some((status, code, message)) = effect_allowance_denial(tx, actor, effect, mode).await? {
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            PolicyResult::DomainError,
            Some(code),
            Some(audit_id),
            Some(code),
            Some(&message),
            0,
            0,
            None,
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status,
            ok: false,
            inner: json!({"code": code, "message": message, "details": []}),
            replayed: false,
            original_audit_id: None,

            approval_id: None,
        });
    }

    // One closed-schema gate for every registered command, including reads.
    // Validation-engine messages are never exposed to a caller (§4.1).
    let valid = match validator_for(args_schema) {
        Ok(validator) => validator.validate(args).is_ok(),
        Err(_) => false,
    };
    if !valid {
        let error_body = json!({
            "code": "invalid_args",
            "message": "Arguments do not match the command schema",
            "details": [{"path": "", "problem": "schema_mismatch"}]
        });
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            PolicyResult::DomainError,
            Some("invalid_args"),
            Some(audit_id),
            Some("invalid_args"),
            Some("Arguments do not match the command schema"),
            0,
            0,
            Some(super::ledger::ReceiptWrite {
                key_digest,
                wire_args_digest: wire_digest,
                status: 422,
                body: &error_body,
                bytes: error_body.to_string().len() as i64,
            }),
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status: 422,
            ok: false,
            inner: error_body,
            replayed: false,
            original_audit_id: None,

            approval_id: None,
        });
    }
    charge_invocation(tx, actor).await?;

    // U08: destructive registry commands require human approval (Q5 / FR-8).
    // The planner, approval bindings and the atomic consume+effect handling
    // live in `destructive_pipeline`; dry runs and first requests produce an
    // observational preview instead of an effect.
    if effect == Effect::Destructive {
        return destructive_pipeline(
            tx,
            epoch,
            actor,
            command_id,
            command_version,
            mode,
            key_digest,
            wire_digest,
            canonical_digest,
            audit_id,
            args,
            result_schema,
            approval_id,
        )
        .await;
    }

    // U02/U05 read commands run on the same pipeline but never mutate facts.
    if let Some(read_command) = super::read::ReadCommand::for_id(command_id) {
        let reply = match super::read::run(read_command, tx, args, actor).await {
            Ok(value) => RunReply {
                status: 200,
                ok: true,
                inner: value,
                replayed: false,
                original_audit_id: None,

                approval_id: None,
            },
            Err(_) => RunReply {
                status: 404,
                ok: false,
                inner: json!({
                    "code": "target_unavailable",
                    "message": "Target record is not available",
                    "details": []
                }),
                replayed: false,
                original_audit_id: None,

                approval_id: None,
            },
        };
        let bytes = reply.inner.to_string().len() as i64;
        let (policy_result, outcome_code, error_code, error_message) = if reply.ok {
            (PolicyResult::Allowed, "ok", None, None)
        } else {
            (
                PolicyResult::DomainError,
                "target_unavailable",
                Some("target_unavailable"),
                Some("Target record is not available"),
            )
        };
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            policy_result,
            Some(outcome_code),
            Some(audit_id),
            error_code,
            error_message,
            0,
            bytes,
            Some(super::ledger::ReceiptWrite {
                key_digest,
                wire_args_digest: wire_digest,
                status: reply.status,
                body: &reply.inner,
                bytes,
            }),
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(reply);
    }

    let execution = if command_id == opportunity::CREATE_ID {
        Execution::Create
    } else if let Some(command) = PhaseCommand::for_id(command_id) {
        Execution::Phase(command)
    } else if let Some(command) = PartyCommand::for_id(command_id) {
        Execution::Party(command)
    } else if command_id == task::TASK_DONE_ID {
        Execution::Task
    } else if command_id == jobs::BRIEF_START_ID {
        Execution::Brief
    } else if let Some(command) = ResourceCommand::for_id(command_id) {
        Execution::Resource(command)
    } else if let Some(command) = WritingCommand::for_id(command_id) {
        Execution::Writing(command)
    } else {
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            PolicyResult::DomainError,
            Some("unknown_command"),
            Some(audit_id),
            Some("unknown_command"),
            Some("Command is not available"),
            0,
            0,
            None,
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status: 404,
            ok: false,
            inner: json!({
                "code": "unknown_command",
                "message": "Command is not available",
                "details": []
            }),
            replayed: false,
            original_audit_id: None,

            approval_id: None,
        });
    };

    // The typed operation runs inside a savepoint. A domain denial, an
    // invalid output or an oversize result rolls the operation back while
    // the outer transaction still commits its audited terminal outcome
    // and receipt (UCR-001 Q3).
    sqlx::query("SAVEPOINT ucr_operation")
        .execute(&mut **tx)
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;

    let created_by_run = if actor.kind == ActorKind::Agent {
        actor.run_id.clone()
    } else {
        None
    };
    let executed: Result<super::RunOutcome, OperationError> = match execution {
        Execution::Create => opportunity::run(tx, args, key_digest, created_by_run)
            .await
            .map_err(OperationError::Create),
        Execution::Phase(command) => run_phase(command, tx, args, key_digest)
            .await
            .map_err(OperationError::Phase),
        Execution::Party(command) => run_party(command, tx, args, key_digest)
            .await
            .map_err(OperationError::Party),
        Execution::Task => run_task(tx, args, key_digest)
            .await
            .map_err(OperationError::Task),
        Execution::Brief => run_jobs(tx, args, key_digest, actor)
            .await
            .map_err(OperationError::Jobs),
        Execution::Resource(command) => super::resource::run(tx, command, args, key_digest)
            .await
            .map_err(OperationError::Resource),
        Execution::Writing(command) => super::writing::run(command, tx, args, actor)
            .await
            .map_err(OperationError::Writing),
    };
    // A storage failure is not a domain conflict: abandon the transaction and
    // let admission record the terminal audit outcome with no effect.
    if matches!(
        executed,
        Err(OperationError::Create(CreateRunError::Create(
            CreateError::Database
        )))
    ) {
        return Err(crate::application::writer::Failure::Unavailable);
    }
    let checked: CheckedResult = match executed {
        Ok(outcome) => {
            let value = outcome.result;
            let bytes = serde_json::to_string(&value).unwrap_or_default().len() as i64;
            if bytes > MAX_RESULT_BYTES {
                Err((
                    413,
                    "result_too_large",
                    "Result exceeds the 64 KiB limit".to_string(),
                    vec![],
                ))
            } else {
                let valid = match validator_for(result_schema) {
                    Ok(validator) => validator.validate(&value).is_ok(),
                    Err(_) => false,
                };
                if valid {
                    Ok((value, bytes, outcome.effected))
                } else {
                    Err((
                        500,
                        "internal",
                        "Operation produced an invalid result".to_string(),
                        vec![],
                    ))
                }
            }
        }
        Err(error) => {
            let mapped: (i64, &'static str, String, Vec<Value>) = match error {
                OperationError::Phase(PhaseRunError::InvalidArgs { path, problem }) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": path, "problem": problem})],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::NotFound)) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::InvalidStage)) => (
                    422,
                    "invalid_args",
                    "Stage is not valid for the selected pack".to_string(),
                    vec![json!({"path": "stage_id", "problem": "unknown_stage"})],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::TerminalStage)) => (
                    409,
                    "requires_owner_workflow",
                    "Terminal stages require the explicit lifecycle flow".to_string(),
                    vec![],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::LifecycleBlocked)) => (
                    409,
                    "conflict",
                    "Lifecycle state blocks this command".to_string(),
                    vec![json!({"path": "", "problem": "lifecycle_blocked"})],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::StaleRevision)) => (
                    409,
                    "version_mismatch",
                    "Expected revision is stale; reload and create a new intent".to_string(),
                    vec![],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::PackMismatch)) => (
                    409,
                    "version_mismatch",
                    "Pack identity does not match the target".to_string(),
                    vec![],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::UndoInvalidReference)) => (
                    422,
                    "invalid_args",
                    "Undo reference is not a valid manual phase transition".to_string(),
                    vec![json!({"path": "original_event_id", "problem": "invalid_undo_reference"})],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::UndoNotLatest)) => (
                    409,
                    "conflict",
                    "The referenced transition is no longer the latest".to_string(),
                    vec![json!({"path": "", "problem": "undo_stale"})],
                ),
                OperationError::Phase(PhaseRunError::Phase(PhaseError::UndoTargetMismatch)) => (
                    422,
                    "invalid_args",
                    "Undo must target the stage recorded before the original transition"
                        .to_string(),
                    vec![json!({"path": "stage_id", "problem": "undo_target_mismatch"})],
                ),
                OperationError::Create(CreateRunError::InvalidArgs) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": "", "problem": "invalid_args"})],
                ),
                OperationError::Create(CreateRunError::Create(CreateError::InvalidTitle)) => (
                    422,
                    "invalid_args",
                    "Opportunity title must contain 1–500 bytes".to_string(),
                    vec![json!({"path": "/title", "problem": "invalid_title"})],
                ),
                OperationError::Create(CreateRunError::Create(CreateError::TargetUnavailable)) => (
                    404,
                    "target_unavailable",
                    "Primary party is not available".to_string(),
                    vec![json!({"path": "/primary_party_id", "problem": "unknown_party"})],
                ),
                OperationError::Create(CreateRunError::Create(CreateError::NoActiveList)) => (
                    409,
                    "conflict",
                    "Workspace must retain an active board list".to_string(),
                    vec![],
                ),
                OperationError::Party(PartyRunError::InvalidArgs) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": "", "problem": "invalid_args"})],
                ),
                OperationError::Party(PartyRunError::Party(PartyEditError::NotFound)) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Party(PartyRunError::Party(PartyEditError::StaleRevision)) => (
                    409,
                    "version_mismatch",
                    "Expected revision is stale; reload and create a new intent".to_string(),
                    vec![],
                ),
                OperationError::Party(PartyRunError::Party(PartyEditError::InvalidName)) => (
                    422,
                    "invalid_args",
                    "Party name must contain 1–500 bytes".to_string(),
                    vec![json!({"path": "/name", "problem": "invalid_name"})],
                ),
                OperationError::Party(PartyRunError::Party(PartyEditError::InvalidFocus)) => (
                    422,
                    "invalid_args",
                    "Focus exceeds 250 characters".to_string(),
                    vec![json!({"path": "/focus", "problem": "invalid_focus"})],
                ),
                OperationError::Party(PartyRunError::Party(PartyEditError::NoFields)) => (
                    422,
                    "invalid_args",
                    "No party fields to update".to_string(),
                    vec![json!({"path": "", "problem": "no_fields"})],
                ),
                OperationError::Task(TaskRunError::InvalidArgs) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": "", "problem": "invalid_args"})],
                ),
                OperationError::Task(TaskRunError::Task(TaskError::NotFound)) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Task(TaskRunError::Task(TaskError::StaleRevision)) => (
                    409,
                    "version_mismatch",
                    "Expected notes revision is stale; reload and create a new intent".to_string(),
                    vec![],
                ),
                OperationError::Task(TaskRunError::Task(TaskError::InvalidSpan(
                    "task line changed; reload notes before toggling",
                ))) => (
                    409,
                    "conflict",
                    "The addressed task line changed; reload notes before toggling".to_string(),
                    vec![json!({"path": "/line_sha256", "problem": "task_line_changed"})],
                ),
                OperationError::Task(TaskRunError::Task(TaskError::InvalidSpan(message))) => (
                    422,
                    "invalid_args",
                    message.to_string(),
                    vec![json!({"path": "/byte_start", "problem": "invalid_span"})],
                ),
                OperationError::Jobs(JobRunError::InvalidArgs) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": "", "problem": "invalid_args"})],
                ),
                OperationError::Jobs(JobRunError::Brief(BriefError::NotFound))
                | OperationError::Jobs(JobRunError::Brief(BriefError::TargetNotGranted)) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Jobs(JobRunError::Brief(BriefError::StaleRevision)) => (
                    409,
                    "version_mismatch",
                    "Expected revision is stale; reload and create a new intent".to_string(),
                    vec![],
                ),
                OperationError::Jobs(JobRunError::Brief(BriefError::InvalidPurpose)) => (
                    422,
                    "invalid_args",
                    "Purpose must be 1–2000 characters".to_string(),
                    vec![json!({"path": "/purpose", "problem": "invalid_purpose"})],
                ),
                OperationError::Jobs(JobRunError::Brief(BriefError::QueueAtCapacity)) => (
                    429,
                    "budget_exceeded",
                    "The job queue is at capacity".to_string(),
                    vec![],
                ),
                OperationError::Jobs(JobRunError::Brief(BriefError::Database)) => {
                    return Err(crate::application::writer::Failure::Unavailable);
                }
                OperationError::Resource(ResourceRunError::InvalidArgs) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": "", "problem": "invalid_args"})],
                ),
                OperationError::Resource(ResourceRunError::Draft(
                    crate::application::resources::ResourceDraftError::InvalidTitle,
                )) => (
                    422,
                    "invalid_args",
                    "Resource title must contain 1–240 characters".to_string(),
                    vec![json!({"path": "/title", "problem": "invalid_title"})],
                ),
                OperationError::Resource(ResourceRunError::Draft(
                    crate::application::resources::ResourceDraftError::InvalidSubtype,
                )) => (
                    422,
                    "invalid_args",
                    "Subtype must be one of the reviewed resource subtypes".to_string(),
                    vec![json!({"path": "/subtype", "problem": "unknown_subtype"})],
                ),
                OperationError::Resource(ResourceRunError::Ready(
                    crate::application::resources::ResourceReadyError::NotFound,
                )) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Resource(ResourceRunError::Ready(
                    crate::application::resources::ResourceReadyError::StaleRevision,
                )) => (
                    409,
                    "version_mismatch",
                    "Expected content revision is stale; reload and create a new intent"
                        .to_string(),
                    vec![],
                ),
                OperationError::Resource(ResourceRunError::Ready(
                    crate::application::resources::ResourceReadyError::NotDraft,
                )) => (
                    422,
                    "invalid_args",
                    "Only a draft resource can transition to ready".to_string(),
                    vec![json!({"path": "", "problem": "not_draft"})],
                ),
                OperationError::Resource(ResourceRunError::Ready(
                    crate::application::resources::ResourceReadyError::MissingFields(fields),
                )) => {
                    let details: Vec<Value> = fields
                        .iter()
                        .map(|field| json!({"path": format!("/{field}"), "problem": "missing_field"}))
                        .collect();
                    (
                        422,
                        "invalid_args",
                        format!("resource is missing required fields: {}", fields.join(", ")),
                        details,
                    )
                }
                OperationError::Resource(ResourceRunError::Ready(
                    crate::application::resources::ResourceReadyError::UnsupportedSubtype,
                )) => (
                    422,
                    "invalid_args",
                    "Subtype is not a material resource subtype".to_string(),
                    vec![json!({"path": "/subtype", "problem": "unknown_subtype"})],
                ),
                OperationError::Resource(ResourceRunError::Retire(retirement_error)) => {
                    match map_retire_error(retirement_error) {
                        Ok(mapped) => mapped,
                        Err(_) => return Err(crate::application::writer::Failure::Unavailable),
                    }
                }
                OperationError::Writing(WritingRunError::InvalidArgs) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": "", "problem": "invalid_args"})],
                ),
                OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::NotFound,
                ))
                | OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::TargetNotGranted,
                )) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::StaleRevision,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::StaleRevision,
                )) => (
                    409,
                    "version_mismatch",
                    "Expected revision is stale; reload and create a new intent".to_string(),
                    vec![],
                ),
                OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::RecipientInvalid,
                ))
                | OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::NeedMissing,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::RecipientInvalid,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::TemplateInvalid,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::ExchangeMissing,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::ExchangeNotReviewed,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::SlotInvalid,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::ResourceInvalid,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::MatchSelectionInvalid,
                )) => (
                    422,
                    "invalid_args",
                    "Arguments do not match the composition rules".to_string(),
                    vec![],
                ),
                OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::MatchShortlistMissing,
                )) => (
                    409,
                    "conflict",
                    "Run matching for this recipient before selecting matched resources"
                        .to_string(),
                    vec![],
                ),
                OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::TargetNotGranted,
                )) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::QueueAtCapacity,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::QueueAtCapacity,
                )) => (
                    429,
                    "budget_exceeded",
                    "The job queue is at capacity".to_string(),
                    vec![],
                ),
                OperationError::Writing(WritingRunError::Match(
                    crate::application::match_start::MatchStartError::Database,
                ))
                | OperationError::Writing(WritingRunError::Compose(
                    crate::application::compose_start::ComposeStartError::Database,
                )) => {
                    return Err(crate::application::writer::Failure::Unavailable);
                }
                OperationError::Create(CreateRunError::Create(CreateError::Database)) => {
                    return Err(crate::application::writer::Failure::Unavailable);
                }
            };
            Err(mapped)
        }
    };

    let (inner, body_bytes) = match checked {
        Ok((value, bytes, effected_flag)) => {
            sqlx::query("RELEASE SAVEPOINT ucr_operation")
                .execute(&mut **tx)
                .await
                .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            // A true no-op consumes zero executed-write allowance (FR-2).
            if effected_flag && effect != Effect::Read && mode == Mode::Execute {
                debit_effect(tx, actor, effect).await?;
            }
            (value, bytes)
        }
        Err((status, code, message, details)) => {
            sqlx::query("ROLLBACK TO SAVEPOINT ucr_operation")
                .execute(&mut **tx)
                .await
                .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            sqlx::query("RELEASE SAVEPOINT ucr_operation")
                .execute(&mut **tx)
                .await
                .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            let error_body = json!({
                "code": code,
                "message": message,
                "details": details
            });
            let bytes = error_body.to_string().len() as i64;
            finalize_attempt(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                PolicyResult::DomainError,
                Some(code),
                Some(audit_id),
                Some(code),
                Some(&message),
                0,
                bytes,
                Some(super::ledger::ReceiptWrite {
                    key_digest,
                    wire_args_digest: wire_digest,
                    status,
                    body: &error_body,
                    bytes,
                }),
            )
            .await
            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            return Ok(RunReply {
                status,
                ok: false,
                inner: error_body,
                replayed: false,
                original_audit_id: None,

                approval_id: None,
            });
        }
    };

    finalize_attempt(
        tx,
        epoch,
        audit_id,
        actor,
        command_id,
        command_version,
        mode,
        PolicyResult::Allowed,
        Some("ok"),
        Some(audit_id),
        None,
        None,
        0,
        body_bytes,
        Some(super::ledger::ReceiptWrite {
            key_digest,
            wire_args_digest: wire_digest,
            status: 200,
            body: &inner,
            bytes: body_bytes,
        }),
    )
    .await
    .map_err(|_| crate::application::writer::Failure::Unavailable)?;

    Ok(RunReply {
        status: 200,
        ok: true,
        inner,
        replayed: false,
        original_audit_id: None,

        approval_id: None,
    })
}

/// Map a retirement operation failure to its transport tuple. Database
/// failures are not domain conflicts: they surface as writer `Unavailable`
/// so the whole transaction is abandoned with zero effects.
fn map_retire_error(
    error: RetirementError,
) -> Result<(i64, &'static str, String, Vec<Value>), crate::application::writer::Failure> {
    Ok(match error {
        RetirementError::NotFound => (
            404,
            "target_unavailable",
            "Target record is not available".to_string(),
            vec![],
        ),
        RetirementError::StaleRevision => (
            409,
            "version_mismatch",
            "Expected revision is stale; reload and create a new intent".to_string(),
            vec![],
        ),
        RetirementError::TooManyVersions => (
            409,
            "requires_owner_workflow",
            "Retirement impact exceeds 256 material versions; use the owner workflow".to_string(),
            vec![],
        ),
        RetirementError::Database => return Err(crate::application::writer::Failure::Unavailable),
    })
}

/// Finalize a terminal denial outcome for an approved intent (wrong, denied,
/// revoked, expired or stale grant) with its terminal receipt.
#[allow(clippy::too_many_arguments)]
async fn approval_deny_reply(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    epoch: &str,
    audit_id: &str,
    actor: &Actor,
    command_id: &str,
    command_version: u32,
    mode: Mode,
    key_digest: &str,
    wire_digest: &str,
    status: i64,
    code: &'static str,
    message: &str,
) -> Result<RunReply, crate::application::writer::Failure> {
    let error_body = json!({"code": code, "message": message, "details": []});
    let bytes = error_body.to_string().len() as i64;
    finalize_attempt(
        tx,
        epoch,
        audit_id,
        actor,
        command_id,
        command_version,
        mode,
        PolicyResult::DomainError,
        Some(code),
        Some(audit_id),
        Some(code),
        Some(message),
        0,
        bytes,
        Some(super::ledger::ReceiptWrite {
            key_digest,
            wire_args_digest: wire_digest,
            status,
            body: &error_body,
            bytes,
        }),
    )
    .await
    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
    Ok(RunReply {
        status,
        ok: false,
        inner: error_body,
        replayed: false,
        original_audit_id: None,
        approval_id: None,
    })
}

/// The owner pseudonym whose inbox owns an approval for this actor.
async fn approval_owner(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    actor: &Actor,
) -> Result<String, crate::application::writer::Failure> {
    match actor.kind {
        ActorKind::Owner => Ok(actor.pseudonym.clone()),
        ActorKind::Agent => {
            let owner: Option<String> =
                sqlx::query_scalar("SELECT owner_pseudonym FROM agent_run WHERE run_id=?")
                    .bind(actor.run_id.as_deref().unwrap_or(""))
                    .fetch_optional(&mut **tx)
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?
                    .flatten();
            owner.ok_or(crate::application::writer::Failure::Unavailable)
        }
    }
}

fn approval_expired(expires_at: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(expires_at)
        .map(|time| time < chrono::Utc::now())
        .unwrap_or(true)
}

/// U08 destructive pipeline: planner preview, approval record, reservation-
/// time binding checks, and the atomic consume+effect inside one savepoint.
///
/// The effect and the approval consumption share the savepoint (Q5): any
/// failed effect rolls both back, so a grant survives a failed attempt and
/// remains usable on the original key. A dry run returns the observational
/// preview with zero domain effects and records its own dry-run receipt.
#[allow(clippy::too_many_arguments)]
async fn destructive_pipeline(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    epoch: &str,
    actor: &Actor,
    command_id: &str,
    command_version: u32,
    mode: Mode,
    key_digest: &str,
    wire_digest: &str,
    canonical_digest: &str,
    audit_id: &str,
    args: &Value,
    result_schema: &'static Value,
    approval_id: Option<String>,
) -> Result<RunReply, crate::application::writer::Failure> {
    let execution = if command_id == super::resource::RETIRE_ID {
        Execution::Resource(ResourceCommand::Retire)
    } else {
        return Err(crate::application::writer::Failure::Unavailable);
    };
    let preview_schema: &'static Value = super::resource::retire_preview_schema();

    // Plan the current impact with SQL reads only (§5.3). Planner denials
    // are terminal, receipted rejections (they do not create an approval).
    let plan = match plan_for_execution(tx, &execution, args).await {
        Ok(plan) => plan,
        Err(Err(failure)) => return Err(failure),
        Err(Ok((status, code, message, details))) => {
            let error_body = json!({"code": code, "message": message, "details": details});
            let bytes = error_body.to_string().len() as i64;
            finalize_attempt(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                PolicyResult::DomainError,
                Some(code),
                Some(audit_id),
                Some(code),
                Some(&message),
                0,
                bytes,
                Some(super::ledger::ReceiptWrite {
                    key_digest,
                    wire_args_digest: wire_digest,
                    status,
                    body: &error_body,
                    bytes,
                }),
            )
            .await
            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            return Ok(RunReply {
                status,
                ok: false,
                inner: error_body,
                replayed: false,
                original_audit_id: None,
                approval_id: None,
            });
        }
    };
    let preview = plan.preview();
    let preview_digest = match parley_domain::jcs::canonicalize_value(&preview) {
        Ok(canonical) => crate::digest(canonical.as_bytes()),
        Err(_) => return Err(crate::application::writer::Failure::Unavailable),
    };
    let fingerprint = plan.fingerprint();

    // Dry run: same access checks and planner, zero domain effects, and its
    // own dry-run receipt (§5 "Approval previews also pass admission...").
    if mode == Mode::DryRun {
        let bytes = preview.to_string().len() as i64;
        if bytes > MAX_RESULT_BYTES {
            let error_body = json!({
                "code": "result_too_large",
                "message": "Preview exceeds the 64 KiB limit",
                "details": []
            });
            let bytes = error_body.to_string().len() as i64;
            finalize_attempt(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                PolicyResult::DomainError,
                Some("result_too_large"),
                Some(audit_id),
                Some("result_too_large"),
                Some("Preview exceeds the 64 KiB limit"),
                0,
                bytes,
                Some(super::ledger::ReceiptWrite {
                    key_digest,
                    wire_args_digest: wire_digest,
                    status: 413,
                    body: &error_body,
                    bytes,
                }),
            )
            .await
            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            return Ok(RunReply {
                status: 413,
                ok: false,
                inner: error_body,
                replayed: false,
                original_audit_id: None,
                approval_id: None,
            });
        }
        let valid = match validator_for(preview_schema) {
            Ok(validator) => validator.validate(&preview).is_ok(),
            Err(_) => false,
        };
        if !valid {
            return Err(crate::application::writer::Failure::Unavailable);
        }
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            PolicyResult::Allowed,
            Some("ok"),
            Some(audit_id),
            None,
            None,
            0,
            bytes,
            Some(super::ledger::ReceiptWrite {
                key_digest,
                wire_args_digest: wire_digest,
                status: 200,
                body: &preview,
                bytes,
            }),
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status: 200,
            ok: true,
            inner: preview,
            replayed: false,
            original_audit_id: None,
            approval_id: None,
        });
    }

    // Execute path: resolve the durable approval for this key.
    let approval = approval::lookup_by_key(tx, epoch, &actor.pseudonym, command_id, key_digest)
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
    let Some(approval) = approval else {
        if approval_id.is_some() {
            let error_body = json!({
                "code": "approval_invalid",
                "message": "The presented approval does not match this invocation",
                "details": []
            });
            let bytes = error_body.to_string().len() as i64;
            finalize_attempt(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                PolicyResult::DomainError,
                Some("approval_invalid"),
                Some(audit_id),
                Some("approval_invalid"),
                Some("The presented approval does not match this invocation"),
                0,
                bytes,
                Some(super::ledger::ReceiptWrite {
                    key_digest,
                    wire_args_digest: wire_digest,
                    status: 403,
                    body: &error_body,
                    bytes,
                }),
            )
            .await
            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            return Ok(RunReply {
                status: 403,
                ok: false,
                inner: error_body,
                replayed: false,
                original_audit_id: None,
                approval_id: None,
            });
        }
        // First destructive execution request without a grant: record the
        // pending approval with the observational preview and dependency
        // fingerprint, then return the waiting outcome (FR-8).
        let owner = approval_owner(tx, actor).await?;
        let normalized_args = super::resource::canonical_args(args).unwrap_or_else(|| args.clone());
        let args_json = serde_json::to_string(&normalized_args)
            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        let id = approval::create_pending(
            tx,
            &approval::NewApproval {
                epoch,
                owner_pseudonym: &owner,
                principal_pseudonym: &actor.pseudonym,
                run_pseudonym: actor.run_id.as_deref(),
                command_id,
                command_version: command_version as i64,
                key_digest,
                wire_args_digest: wire_digest,
                canonical_args_digest: canonical_digest,
                dependency_fingerprint: &fingerprint,
                preview_digest: &preview_digest,
                args_json,
                preview_json: preview.to_string(),
            },
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        let error_body = json!({
            "code": "approval_required",
            "message": "Review the requested retirement in Approvals.",
            "details": [{"path": "", "problem": "approval_pending"}]
        });
        let bytes = error_body.to_string().len() as i64;
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            PolicyResult::DomainError,
            Some("approval_required"),
            Some(audit_id),
            Some("approval_required"),
            Some("Review the requested retirement in Approvals."),
            0,
            bytes,
            None,
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status: 403,
            ok: false,
            inner: error_body,
            replayed: false,
            original_audit_id: None,
            approval_id: Some(id),
        });
    };

    // Same key, different intent is terminal for that key (Q2).
    if approval.wire_args_digest != wire_digest
        || approval.canonical_args_digest != canonical_digest
    {
        let error_body = json!({
            "code": "idempotency_mismatch",
            "message": "Idempotency key was used for a different command",
            "details": []
        });
        let bytes = error_body.to_string().len() as i64;
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            actor,
            command_id,
            command_version,
            mode,
            PolicyResult::DomainError,
            Some("idempotency_mismatch"),
            Some(audit_id),
            Some("idempotency_mismatch"),
            Some("Idempotency key was used for a different command"),
            0,
            bytes,
            Some(super::ledger::ReceiptWrite {
                key_digest,
                wire_args_digest: wire_digest,
                status: 422,
                body: &error_body,
                bytes,
            }),
        )
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
        return Ok(RunReply {
            status: 422,
            ok: false,
            inner: error_body,
            replayed: false,
            original_audit_id: None,
            approval_id: None,
        });
    }

    match approval.status {
        ApprovalStatus::Pending => {
            // Pending approvals also carry a TTL; an expired pending intent
            // fails closed instead of returning the same waiting location.
            if approval_expired(&approval.expires_at) {
                approval::expire_one(tx, &approval.id)
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                return approval_deny_reply(
                    tx,
                    epoch,
                    audit_id,
                    actor,
                    command_id,
                    command_version,
                    mode,
                    key_digest,
                    wire_digest,
                    403,
                    "approval_invalid",
                    "The approval has expired",
                )
                .await;
            }
            // Repeated requests for the same pending intent return the same
            // location (§4.1).
            let error_body = json!({
                "code": "approval_required",
                "message": "Review the requested retirement in Approvals.",
                "details": [{"path": "", "problem": "approval_pending"}]
            });
            let bytes = error_body.to_string().len() as i64;
            finalize_attempt(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                PolicyResult::DomainError,
                Some("approval_required"),
                Some(audit_id),
                Some("approval_required"),
                Some("Review the requested retirement in Approvals."),
                0,
                bytes,
                None,
            )
            .await
            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            Ok(RunReply {
                status: 403,
                ok: false,
                inner: error_body,
                replayed: false,
                original_audit_id: None,
                approval_id: Some(approval.id),
            })
        }
        ApprovalStatus::Granted => {
            if approval_expired(&approval.expires_at) {
                approval::expire_one(tx, &approval.id)
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                return approval_deny_reply(
                    tx,
                    epoch,
                    audit_id,
                    actor,
                    command_id,
                    command_version,
                    mode,
                    key_digest,
                    wire_digest,
                    403,
                    "approval_invalid",
                    "The approval has expired",
                )
                .await;
            }
            // Execution recomputes the fingerprint under reservation before
            // consuming the grant (Q5): a changed dependency is a stale
            // denial, never a silently broadened authorization.
            if approval.dependency_fingerprint != fingerprint {
                approval::mark_stale(tx, &approval.id)
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                return approval_deny_reply(
                    tx,
                    epoch,
                    audit_id,
                    actor,
                    command_id,
                    command_version,
                    mode,
                    key_digest,
                    wire_digest,
                    409,
                    "approval_stale",
                    "The intent dependencies changed; request a fresh preview with a new key",
                )
                .await;
            }
            if let Some(presented) = approval_id.as_deref()
                && presented != approval.id
            {
                return approval_deny_reply(
                    tx,
                    epoch,
                    audit_id,
                    actor,
                    command_id,
                    command_version,
                    mode,
                    key_digest,
                    wire_digest,
                    403,
                    "approval_invalid",
                    "The presented approval does not match this invocation",
                )
                .await;
            }
            let _ = approval;
            // Atomic consume + effect inside one savepoint. A domain denial, an
            // invalid output or an oversize result rolls the consumption back
            // with the operation, so the grant stays usable on the original
            // key (Q5 / §5.3).
            sqlx::query("SAVEPOINT ucr_operation")
                .execute(&mut **tx)
                .await
                .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            let consumed = approval::consume(tx, &approval.id, approval.revision, audit_id)
                .await
                .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            if !consumed {
                sqlx::query("ROLLBACK TO SAVEPOINT ucr_operation")
                    .execute(&mut **tx)
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                sqlx::query("RELEASE SAVEPOINT ucr_operation")
                    .execute(&mut **tx)
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                return approval_deny_reply(
                    tx,
                    epoch,
                    audit_id,
                    actor,
                    command_id,
                    command_version,
                    mode,
                    key_digest,
                    wire_digest,
                    403,
                    "approval_invalid",
                    "The approval is no longer actionable",
                )
                .await;
            }
            let executed = match execution {
                Execution::Resource(ResourceCommand::Retire) => {
                    super::resource::run(tx, ResourceCommand::Retire, args, key_digest)
                        .await
                        .map_err(OperationError::Resource)
                }
                _ => unreachable!("destructive command validated above"),
            };
            match executed {
                Ok(outcome) => {
                    let value = outcome.result;
                    let body_bytes = serde_json::to_string(&value).unwrap_or_default().len() as i64;
                    if body_bytes > MAX_RESULT_BYTES {
                        sqlx::query("ROLLBACK TO SAVEPOINT ucr_operation")
                            .execute(&mut **tx)
                            .await
                            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                        sqlx::query("RELEASE SAVEPOINT ucr_operation")
                            .execute(&mut **tx)
                            .await
                            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                        return approval_deny_reply(
                            tx,
                            epoch,
                            audit_id,
                            actor,
                            command_id,
                            command_version,
                            mode,
                            key_digest,
                            wire_digest,
                            413,
                            "result_too_large",
                            "Result exceeds the 64 KiB limit",
                        )
                        .await;
                    }
                    let valid = match validator_for(result_schema) {
                        Ok(validator) => validator.validate(&value).is_ok(),
                        Err(_) => false,
                    };
                    if !valid {
                        sqlx::query("ROLLBACK TO SAVEPOINT ucr_operation")
                            .execute(&mut **tx)
                            .await
                            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                        sqlx::query("RELEASE SAVEPOINT ucr_operation")
                            .execute(&mut **tx)
                            .await
                            .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                        return approval_deny_reply(
                            tx,
                            epoch,
                            audit_id,
                            actor,
                            command_id,
                            command_version,
                            mode,
                            key_digest,
                            wire_digest,
                            500,
                            "internal",
                            "Operation produced an invalid result",
                        )
                        .await;
                    }
                    sqlx::query("RELEASE SAVEPOINT ucr_operation")
                        .execute(&mut **tx)
                        .await
                        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                    if outcome.effected {
                        debit_effect(tx, actor, Effect::Destructive).await?;
                    }
                    finalize_attempt(
                        tx,
                        epoch,
                        audit_id,
                        actor,
                        command_id,
                        command_version,
                        mode,
                        PolicyResult::Allowed,
                        Some("ok"),
                        Some(audit_id),
                        None,
                        None,
                        0,
                        body_bytes,
                        Some(super::ledger::ReceiptWrite {
                            key_digest,
                            wire_args_digest: wire_digest,
                            status: 200,
                            body: &value,
                            bytes: body_bytes,
                        }),
                    )
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                    Ok(RunReply {
                        status: 200,
                        ok: true,
                        inner: value,
                        replayed: false,
                        original_audit_id: None,
                        approval_id: None,
                    })
                }
                Err(error) => {
                    sqlx::query("ROLLBACK TO SAVEPOINT ucr_operation")
                        .execute(&mut **tx)
                        .await
                        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                    sqlx::query("RELEASE SAVEPOINT ucr_operation")
                        .execute(&mut **tx)
                        .await
                        .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                    let mapped: (i64, &'static str, String, Vec<Value>) = match error {
                        OperationError::Resource(ResourceRunError::Retire(retirement_error)) => {
                            map_retire_error(retirement_error)?
                        }
                        _ => unreachable!("destructive command validated above"),
                    };
                    let (status, code, message, details) = mapped;
                    let error_body = json!({"code": code, "message": message, "details": details});
                    let bytes = error_body.to_string().len() as i64;
                    finalize_attempt(
                        tx,
                        epoch,
                        audit_id,
                        actor,
                        command_id,
                        command_version,
                        mode,
                        PolicyResult::DomainError,
                        Some(code),
                        Some(audit_id),
                        Some(code),
                        Some(&message),
                        0,
                        bytes,
                        Some(super::ledger::ReceiptWrite {
                            key_digest,
                            wire_args_digest: wire_digest,
                            status,
                            body: &error_body,
                            bytes,
                        }),
                    )
                    .await
                    .map_err(|_| crate::application::writer::Failure::Unavailable)?;
                    Ok(RunReply {
                        status,
                        ok: false,
                        inner: error_body,
                        replayed: false,
                        original_audit_id: None,
                        approval_id: None,
                    })
                }
            }
        }
        ApprovalStatus::Consumed => {
            approval_deny_reply(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                key_digest,
                wire_digest,
                403,
                "approval_invalid",
                "The approval was already consumed",
            )
            .await
        }
        ApprovalStatus::Denied => {
            approval_deny_reply(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                key_digest,
                wire_digest,
                403,
                "approval_invalid",
                "The approval was denied",
            )
            .await
        }
        ApprovalStatus::Revoked => {
            approval_deny_reply(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                key_digest,
                wire_digest,
                403,
                "approval_invalid",
                "The approval was revoked",
            )
            .await
        }
        ApprovalStatus::Expired => {
            approval_deny_reply(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                key_digest,
                wire_digest,
                403,
                "approval_invalid",
                "The approval has expired",
            )
            .await
        }
        ApprovalStatus::Stale => {
            approval_deny_reply(
                tx,
                epoch,
                audit_id,
                actor,
                command_id,
                command_version,
                mode,
                key_digest,
                wire_digest,
                409,
                "approval_stale",
                "The intent dependencies changed; request a fresh preview with a new key",
            )
            .await
        }
    }
}

async fn plan_for_execution(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    execution: &Execution,
    args: &Value,
) -> Result<
    crate::application::resource_retirement::RetirementPlan,
    Result<(i64, &'static str, String, Vec<Value>), crate::application::writer::Failure>,
> {
    match execution {
        Execution::Resource(ResourceCommand::Retire) => {
            match super::resource::plan(tx, ResourceCommand::Retire, args).await {
                Ok(plan) => Ok(plan),
                Err(ResourceRunError::Retire(retirement_error)) => {
                    Err(map_retire_error(retirement_error))
                }
                Err(ResourceRunError::InvalidArgs) => Err(Ok((
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": "", "problem": "invalid_args"})],
                ))),
                Err(_) => Err(Err(crate::application::writer::Failure::Unavailable)),
            }
        }
        _ => Err(Err(crate::application::writer::Failure::Unavailable)),
    }
}

// ---------------------------------------------------------------------------
// Owner approval surface (U08 / FR-8, §4.1)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ApprovalListQuery {
    status: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionInput {
    expected_revision: i64,
    preview_digest: String,
}

async fn list_approvals(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ApprovalListQuery>,
) -> Response {
    let (principal, _registry, _writer) = match auth_owner(&headers, &state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(25).clamp(1, 100);
    if let Some(status) = &query.status
        && !status.is_empty()
        && ApprovalStatus::parse(status).is_none()
    {
        return error403(
            &state,
            "invalid_args",
            "Unknown approval status filter",
            400,
        );
    }
    let (items, next) = match approval::list_owner(
        state.workspace.pool(),
        state.workspace.epoch(),
        &principal.pseudonym,
        query.status.as_deref(),
        limit,
        query.cursor.as_deref().unwrap_or(""),
    )
    .await
    {
        Ok(value) => value,
        Err(_) => {
            return error403(
                &state,
                "unavailable",
                "The approval store is temporarily unavailable",
                503,
            );
        }
    };
    let mut response = Json(json!({"items": items, "next_cursor": next})).into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

async fn get_approval(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let (principal, _registry, _writer) = match auth_owner(&headers, &state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let row = {
        let mut conn = match state.workspace.pool().acquire().await {
            Ok(conn) => conn,
            Err(_) => {
                return error403(
                    &state,
                    "unavailable",
                    "The approval store is temporarily unavailable",
                    503,
                );
            }
        };
        let mut tx = match conn.begin().await {
            Ok(tx) => tx,
            Err(_) => {
                return error403(
                    &state,
                    "unavailable",
                    "The approval store is temporarily unavailable",
                    503,
                );
            }
        };
        let row = approval::lookup_by_id(&mut tx, &id).await.ok().flatten();
        let _ = tx.rollback().await;
        row
    };
    let Some(row) = row else {
        return error403(
            &state,
            "target_unavailable",
            "Approval is not available",
            404,
        );
    };
    if row.epoch != state.workspace.epoch() || row.owner_pseudonym != principal.pseudonym {
        return error403(
            &state,
            "target_unavailable",
            "Approval is not available",
            404,
        );
    }
    // Active records include the exact private args and preview; terminal
    // responses omit them (FR-8 / §5 table).
    let active = !row.status.is_terminal();
    let args = if active {
        row.args_json
            .as_deref()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
    } else {
        None
    };
    let preview = if active {
        row.preview_json
            .as_deref()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
    } else {
        None
    };
    let mut payload = json!({
        "id": row.id,
        "command_id": row.command_id,
        "command_version": row.command_version,
        "revision": row.revision,
        "status": row.status.as_str(),
        "expires_at": row.expires_at,
        "preview_digest": row.preview_digest,
    });
    if let Some(run) = row.run_pseudonym {
        payload["run_id"] = json!(run);
    }
    if let Some(args) = args {
        payload["args"] = args;
    }
    if let Some(preview) = preview {
        payload["preview"] = preview;
    }
    let mut response = Json(payload).into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

/// Run one owner approval decision under the writer reservation, so grants,
/// denials and effect commits serialize on the same single-writer slot
/// (Q5: cancel/revoke compete with effect commit).
async fn decision(
    state: &AppState,
    headers: HeaderMap,
    id: String,
    body: Bytes,
    action: DecisionAction,
) -> Response {
    let (principal, _registry, _writer) = match auth_owner(&headers, state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    if body.len() > MAX_REQUEST_BYTES {
        return error403(
            state,
            "request_too_large",
            "Request body exceeds 1 MiB",
            413,
        );
    }
    let parsed: Value = match crate::decode_unique(&body) {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                state,
                400,
                "malformed_request",
                "Request body is not valid JSON",
            );
        }
    };
    if json_depth(&parsed) > MAX_JSON_DEPTH {
        return error_response(
            state,
            400,
            "malformed_request",
            "Payload nesting exceeds 16 levels",
        );
    }
    let input: DecisionInput = match serde_json::from_value(parsed) {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                state,
                400,
                "malformed_request",
                "Malformed decision request",
            );
        }
    };
    let key = match crate::command_operation_key(&headers) {
        Ok(key) => key,
        Err((status, code, message)) => {
            return error_response(state, status.as_u16(), code, message);
        }
    };
    let epoch = state.workspace.epoch().to_owned();
    let owner = principal.pseudonym.clone();
    let secret = state.workspace.command_secret();
    let action_name = match action {
        DecisionAction::Grant => "grant",
        DecisionAction::Deny => "deny",
    };
    let decision_key_digest = parley_domain::hmac::sign_hex(
        &secret,
        b"command-decision",
        format!("{epoch}\0{owner}\0{action_name}\0{id}\0{key}").as_bytes(),
    );

    let job = crate::application::writer::WriterJob {
        deadline: Instant::now() + crate::application::writer::EXECUTION_DEADLINE,
        generation: state.workspace.generation_token().await,
        run: Box::new(move |connection, deadline| {
            let epoch = epoch.clone();
            let owner = owner.clone();
            let id = id.clone();
            let decision_key_digest = decision_key_digest.clone();
            Box::pin(async move {
                let mut tx = match connection.begin_with("BEGIN IMMEDIATE").await {
                    Ok(tx) => tx,
                    Err(_) => return busy_decision_job(),
                };
                let current_epoch: Option<String> =
                    match sqlx::query_scalar("SELECT epoch FROM workspace_meta WHERE singleton=1")
                        .fetch_optional(&mut *tx)
                        .await
                    {
                        Ok(value) => value.flatten(),
                        Err(_) => return busy_decision_job(),
                    };
                if current_epoch.as_deref() != Some(epoch.as_str()) {
                    return busy_decision_job();
                }
                let outcome = match approval::decide(
                    &mut tx,
                    &id,
                    &epoch,
                    &owner,
                    action,
                    input.expected_revision,
                    &input.preview_digest,
                    &decision_key_digest,
                )
                .await
                {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        let (status, code, message) = match error {
                            DecisionError::NotFound => {
                                (404, "target_unavailable", "Approval is not available")
                            }
                            DecisionError::KeyMismatch => (
                                422,
                                "idempotency_mismatch",
                                "Decision key was used for a different request",
                            ),
                            DecisionError::StaleRevision
                            | DecisionError::PreviewMismatch
                            | DecisionError::NotActionable => (
                                409,
                                "approval_stale",
                                "The approval changed; reload before deciding",
                            ),
                            DecisionError::Database => {
                                (503, "unavailable", "The approval store is unavailable")
                            }
                        };
                        let body = serde_json::json!({
                            "ok": false,
                            "status": status,
                            "code": code,
                            "message": message,
                        });
                        return crate::application::writer::JobResult {
                            commit: true,
                            body,
                            failure: None,
                        };
                    }
                };
                let body = serde_json::json!({
                    "ok": true,
                    "id": outcome.id,
                    "revision": outcome.revision,
                    "status": outcome.status.as_str(),
                });
                match crate::application::writer::commit_tx(tx, deadline, "").await {
                    Ok(()) => crate::application::writer::JobResult {
                        commit: true,
                        body,
                        failure: None,
                    },
                    Err(failure) => crate::application::writer::JobResult {
                        commit: false,
                        body: Value::Null,
                        failure: Some(failure),
                    },
                }
            })
        }),
        reply: tokio::sync::oneshot::channel().0,
    };
    match state.writer.run(job).await {
        crate::application::writer::WriteOutcome::Committed(body) => {
            let _ = state.notify.send(CommandSignal::ApprovalsChanged);
            if body.get("ok").and_then(Value::as_bool) == Some(false) {
                let status = body.get("status").and_then(Value::as_u64).unwrap_or(500) as u16;
                let code = body
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("unavailable")
                    .to_owned();
                let message = body
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("command failed")
                    .to_owned();
                return decision_error_response(state, status, code, message);
            }
            let mut response = Json(body).into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        crate::application::writer::WriteOutcome::Rejected(_) => error403(
            state,
            "unavailable",
            "The workspace writer is unavailable; retry with the same key",
            503,
        ),
    }
}

fn decision_error_response(
    state: &AppState,
    status: u16,
    code: String,
    message: String,
) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST);
    let mut response = Json(json!({
        "ok": false,
        "error": {"code": code, "message": message, "details": []}
    }))
    .into_response();
    *response.status_mut() = status;
    private_headers(&mut response, state.workspace.epoch());
    response
}

fn busy_decision_job() -> crate::application::writer::JobResult {
    crate::application::writer::JobResult {
        commit: false,
        body: Value::Null,
        failure: Some(crate::application::writer::Failure::Unavailable),
    }
}

async fn grant_approval(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    decision(&state, headers, id, body, DecisionAction::Grant).await
}

async fn deny_approval(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    decision(&state, headers, id, body, DecisionAction::Deny).await
}

/// Same-origin fetch-based SSE notification stream (U08 §4.1). Owner auth
/// headers are required; the stream emits `approvals_changed`,
/// `run_changed` and `epoch_changed` signals plus a 15 s heartbeat, and
/// closes on service shutdown. Connections are bounded.
async fn notifications(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let (_principal, _registry, _writer) = match auth_owner(&headers, &state).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let permit = match state.sse_connections.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return error403(
                &state,
                "unavailable",
                "Too many notification connections",
                503,
            );
        }
    };
    #[derive(Clone)]
    enum SsePayload {
        Approvals,
        Run,
        Epoch,
        Heartbeat,
    }
    let (tx, rx) = mpsc::channel::<SsePayload>(64);
    let mut notify_rx = state.notify.subscribe();
    let mut restart_rx = state.restart.subscribe();
    let epoch = state.workspace.epoch().to_owned();
    let _ = tx.send(SsePayload::Epoch).await;
    tokio::spawn(async move {
        let _permit = permit;
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            tokio::select! {
                signal = notify_rx.recv() => {
                    match signal {
                        Ok(CommandSignal::ApprovalsChanged) => {
                            if tx.send(SsePayload::Approvals).await.is_err() { break; }
                        }
                        Ok(CommandSignal::RunChanged) => {
                            if tx.send(SsePayload::Run).await.is_err() { break; }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = heartbeat.tick() => {
                    if tx.send(SsePayload::Heartbeat).await.is_err() { break; }
                }
                _ = restart_rx.changed() => break,
            }
        }
    });
    let stream = ReceiverStream::new(rx).map(move |payload| {
        let event = match payload {
            SsePayload::Approvals => Event::default().event("approvals_changed").data("{}"),
            SsePayload::Run => Event::default().event("run_changed").data("{}"),
            SsePayload::Epoch => Event::default().event("epoch_changed").data(epoch.clone()),
            SsePayload::Heartbeat => Event::default().comment("heartbeat"),
        };
        Ok::<Event, Infallible>(event)
    });
    let mut response = Sse::new(stream).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        header::CONNECTION,
        axum::http::HeaderValue::from_static("keep-alive"),
    );
    response.headers_mut().insert(
        "x-accel-buffering",
        axum::http::HeaderValue::from_static("no"),
    );
    response
}

fn error_response(state: &AppState, status: u16, code: &'static str, message: &str) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST);
    let mut response = Json(json!({
        "ok": false,
        "error": {"code": code, "message": message, "details": []}
    }))
    .into_response();
    *response.status_mut() = status;
    private_headers(&mut response, state.workspace.epoch());
    response
}

fn error403(state: &AppState, code: &'static str, message: &str, status: u16) -> Response {
    error_response(state, status, code, message)
}
