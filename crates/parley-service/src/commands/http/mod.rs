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
    routing::{delete, get, patch, post, put},
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

mod legacy_routes;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/commands/query", post(query))
        .route("/api/v1/commands/search", post(search))
        .route("/api/v1/commands/{id}", get(describe).post(run))
        .route("/api/v1/approvals", get(approval_endpoints::list_approvals))
        .route(
            "/api/v1/approvals/{id}",
            get(approval_endpoints::get_approval),
        )
        .route(
            "/api/v1/approvals/{id}/grant",
            post(approval_endpoints::grant_approval),
        )
        .route(
            "/api/v1/approvals/{id}/deny",
            post(approval_endpoints::deny_approval),
        )
        .route(
            "/api/v1/command-notifications",
            get(notifications::notifications),
        )
        .merge(legacy_routes::legacy_routes())
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
                    let reply = match pipeline::pipeline(
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
                // and its 250 ms poll remain authoritative.
                if reply.inner.get("job_id").and_then(|v| v.as_str()).is_some() {
                    state.jobs.notify_one();
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

mod approval_endpoints;
mod destructive_pipeline;
mod notifications;
mod pipeline;

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

pub(super) fn error_response(
    state: &AppState,
    status: u16,
    code: &'static str,
    message: &str,
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

pub(super) fn error403(
    state: &AppState,
    code: &'static str,
    message: &str,
    status: u16,
) -> Response {
    error_response(state, status, code, message)
}
