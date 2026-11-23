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
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Connection, Sqlite};

use super::descriptor::CommandDescriptor;
use super::ledger::{
    AdmitError, Mode, PolicyResult, Timing, admit_attempt, finalize_attempt, key_digest,
    lookup_receipt, redact_args,
};
use super::phase::{PhaseCommand, RunError, args_allowlist, args_sensitive, run as run_phase};
use super::{Registry, validator_for};
use crate::application::phase::PhaseError;
use crate::application::writer::{Failure as WriterFailure, JobResult, WriteOutcome, Writer};
use crate::{AppState, OwnerPrincipal, private_headers};

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

/// A validated operation output, or a normalized terminal response
/// `(status, code, message, details)`.
type CheckedResult = Result<(Value, i64), (i64, &'static str, String, Vec<Value>)>;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/commands/query", post(query))
        .route("/api/v1/commands/search", post(search))
        .route("/api/v1/commands/{id}", get(describe).post(run))
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunReply {
    status: i64,
    ok: bool,
    inner: Value,
    replayed: bool,
    original_audit_id: Option<String>,
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
            return error_response(400, "malformed_request", "Request body is not valid JSON");
        }
    };
    if json_depth(&parsed) > MAX_JSON_DEPTH {
        return error_response(
            400,
            "malformed_request",
            "Payload nesting exceeds 16 levels",
        );
    }
    let input: QueryInput = match serde_json::from_value(parsed) {
        Ok(value) => value,
        Err(_) => return error_response(400, "malformed_request", "Malformed query request"),
    };
    if input.surface != "palette" {
        return error_response(
            403,
            "forbidden",
            "Owner requests must use the palette surface",
        );
    }
    if input.context.len() > MAX_CONTEXT_KEYS {
        return error_response(400, "malformed_request", "Context exceeds 16 keys");
    }
    let context_bytes: usize = input
        .context
        .iter()
        .map(|(key, value)| key.len() + value.to_string().len())
        .sum();
    if context_bytes > MAX_CONTEXT_BYTES {
        return error_response(400, "malformed_request", "Context exceeds 4 KiB");
    }
    for key in input.context.keys() {
        if !context_allowed(key) {
            return error_response(400, "malformed_request", "Context key is not allowlisted");
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
            return error_response(400, "malformed_request", "Request body is not valid JSON");
        }
    };
    if json_depth(&parsed) > MAX_JSON_DEPTH {
        return error_response(
            400,
            "malformed_request",
            "Payload nesting exceeds 16 levels",
        );
    }
    let input: SearchInput = match serde_json::from_value(parsed) {
        Ok(value) => value,
        Err(_) => return error_response(400, "malformed_request", "Malformed search request"),
    };
    if input.query.len() > MAX_SEARCH_BYTES {
        return error_response(400, "malformed_request", "Query exceeds 256 bytes");
    }
    let limit = match input.limit.unwrap_or(SEARCH_DEFAULT_LIMIT) {
        0 => return error_response(400, "malformed_request", "Limit must be between 1 and 25"),
        value if value > SEARCH_MAX_RESULTS => {
            return error_response(400, "malformed_request", "Limit must be between 1 and 25");
        }
        value => value,
    };
    for key in input.context.keys() {
        if !context_allowed(key) {
            return error_response(400, "malformed_request", "Context key is not allowlisted");
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
    let (principal, registry, writer) = match auth_owner(&headers, &state).await {
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
        Err((status, code, message)) => return error_response(status.as_u16(), code, message),
    };
    let fault = crate::application::writer::fault_from_key(&key);
    let parsed: Value = match crate::decode_unique(&body) {
        Ok(value) => value,
        Err(_) => {
            return error_response(400, "malformed_request", "Request body is not valid JSON");
        }
    };
    if json_depth(&parsed) > MAX_JSON_DEPTH {
        return error_response(
            400,
            "malformed_request",
            "Payload nesting exceeds 16 levels",
        );
    }
    let input: InvocationInput = match serde_json::from_value(parsed) {
        Ok(value) => value,
        Err(_) => return error_response(400, "malformed_request", "Malformed invocation request"),
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
    let args_value = input.args.clone();
    let wire_digest = match parley_domain::jcs::canonicalize_value(&args_value) {
        Ok(canonical) => crate::digest(canonical.as_bytes()),
        Err(_) => return error_response(400, "malformed_request", "Args are not canonicalizable"),
    };
    let mode = if input.dry_run {
        Mode::DryRun
    } else {
        Mode::Execute
    };
    let epoch = state.workspace.epoch().to_string();
    let principal_pseudonym = principal.pseudonym.clone();
    let generation_secret = state.workspace.command_secret();
    let key_digest = key_digest(
        &generation_secret,
        &epoch,
        &principal_pseudonym,
        command.id,
        &key,
    );
    let args_json = redact_args(
        args_value.as_object().expect("checked object"),
        args_allowlist(),
        args_sensitive(),
    );
    let request_bytes = body.len() as i64;
    let command_id = command.id.to_string();
    let command_version = command.version;
    let result_schema = command.result_schema;

    let audit_id = match admit_attempt(
        state.workspace.pool(),
        &epoch,
        &principal_pseudonym,
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
    let rejection_principal = principal_pseudonym.clone();
    let rejection_command = command_id.clone();
    let audit_id_for_job = audit_id.clone();
    let job = crate::application::writer::WriterJob {
        deadline: Instant::now() + crate::application::writer::EXECUTION_DEADLINE,
        generation,
        run: Box::new(
            move |connection: &mut sqlx::sqlite::SqliteConnection, deadline: Instant| {
                let epoch = epoch.clone();
                let principal = principal_pseudonym.clone();
                let command_id = command_id.clone();
                let key_digest = key_digest.clone();
                let wire_digest = wire_digest.clone();
                let audit_id = audit_id_for_job.clone();
                let args = args_value.clone();
                let result_schema = result_schema;
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
                        &principal,
                        &command_id,
                        command_version,
                        mode,
                        &key_digest,
                        &wire_digest,
                        &audit_id,
                        &args,
                        result_schema,
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
                        Ok(()) => JobResult {
                            commit: true,
                            body: serde_json::to_value(&reply).unwrap_or_default(),
                            failure: None,
                        },
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
    match writer.run(job).await {
        WriteOutcome::Committed(body) => {
            let reply: RunReply = serde_json::from_value(body).unwrap_or(RunReply {
                status: 500,
                ok: false,
                inner: json!({"code": "internal", "message": "invariant failure", "details": []}),
                replayed: false,
                original_audit_id: None,
            });
            let elapsed = timing.elapsed_ms();
            if reply.ok {
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
                &rejection_principal,
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

#[allow(clippy::too_many_arguments)]
async fn pipeline(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    epoch: &str,
    principal: &str,
    command_id: &str,
    command_version: u32,
    mode: Mode,
    key_digest: &str,
    wire_digest: &str,
    audit_id: &str,
    args: &Value,
    result_schema: &'static Value,
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
        });
    }

    // Replay path: same key, same intent.
    if let Some(record) = lookup_receipt(tx, epoch, principal, command_id, key_digest)
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
                principal,
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
            });
        }
        // Same key, different intent: terminal mismatch for this key.
        finalize_attempt(
            tx,
            epoch,
            audit_id,
            principal,
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
        });
    }

    let phase_command = match PhaseCommand::for_id(command_id) {
        Some(command) => command,
        None => {
            finalize_attempt(
                tx,
                epoch,
                audit_id,
                principal,
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
            });
        }
    };

    // Fresh path: validate against the closed schema.
    let schema = phase_command.args_schema();
    // Validation-engine messages are never exposed to a caller (§4.1).
    let valid = match validator_for(schema) {
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
            principal,
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
        });
    }

    // The typed operation runs inside a savepoint. A domain denial, an
    // invalid output or an oversize result rolls the operation back while
    // the outer transaction still commits its audited terminal outcome
    // and receipt (UCR-001 Q3).
    sqlx::query("SAVEPOINT ucr_operation")
        .execute(&mut **tx)
        .await
        .map_err(|_| crate::application::writer::Failure::Unavailable)?;

    let executed = run_phase(phase_command, tx, args, key_digest).await;
    let checked: CheckedResult = match executed {
        Ok(value) => {
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
                    Ok((value, bytes))
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
                RunError::InvalidArgs { path, problem } => (
                    422,
                    "invalid_args",
                    "Arguments do not match the command rules".to_string(),
                    vec![json!({"path": path, "problem": problem})],
                ),
                RunError::Phase(PhaseError::NotFound) => (
                    404,
                    "target_unavailable",
                    "Target record is not available".to_string(),
                    vec![],
                ),
                RunError::Phase(PhaseError::InvalidStage) => (
                    422,
                    "invalid_args",
                    "Stage is not valid for the selected pack".to_string(),
                    vec![json!({"path": "stage_id", "problem": "unknown_stage"})],
                ),
                RunError::Phase(PhaseError::TerminalStage) => (
                    409,
                    "requires_owner_workflow",
                    "Terminal stages require the explicit lifecycle flow".to_string(),
                    vec![],
                ),
                RunError::Phase(PhaseError::LifecycleBlocked) => (
                    409,
                    "conflict",
                    "Lifecycle state blocks this command".to_string(),
                    vec![json!({"path": "", "problem": "lifecycle_blocked"})],
                ),
                RunError::Phase(PhaseError::StaleRevision) => (
                    409,
                    "version_mismatch",
                    "Expected revision is stale; reload and create a new intent".to_string(),
                    vec![],
                ),
                RunError::Phase(PhaseError::PackMismatch) => (
                    409,
                    "version_mismatch",
                    "Pack identity does not match the target".to_string(),
                    vec![],
                ),
                RunError::Phase(PhaseError::UndoInvalidReference) => (
                    422,
                    "invalid_args",
                    "Undo reference is not a valid manual phase transition".to_string(),
                    vec![json!({"path": "original_event_id", "problem": "invalid_undo_reference"})],
                ),
                RunError::Phase(PhaseError::UndoNotLatest) => (
                    409,
                    "conflict",
                    "The referenced transition is no longer the latest".to_string(),
                    vec![json!({"path": "", "problem": "undo_stale"})],
                ),
                RunError::Phase(PhaseError::UndoTargetMismatch) => (
                    422,
                    "invalid_args",
                    "Undo must target the stage recorded before the original transition"
                        .to_string(),
                    vec![json!({"path": "stage_id", "problem": "undo_target_mismatch"})],
                ),
            };
            Err(mapped)
        }
    };

    let (inner, body_bytes) = match checked {
        Ok(pair) => {
            sqlx::query("RELEASE SAVEPOINT ucr_operation")
                .execute(&mut **tx)
                .await
                .map_err(|_| crate::application::writer::Failure::Unavailable)?;
            pair
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
                principal,
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
            });
        }
    };

    finalize_attempt(
        tx,
        epoch,
        audit_id,
        principal,
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
    })
}

fn error_response(status: u16, code: &'static str, message: &str) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST);
    let mut response = Json(json!({
        "ok": false,
        "error": {"code": code, "message": message, "details": []}
    }))
    .into_response();
    *response.status_mut() = status;
    response
}

fn error403(state: &AppState, code: &'static str, message: &str, status: u16) -> Response {
    let mut response = error_response(status, code, message);
    private_headers(&mut response, state.workspace.epoch());
    response
}
