use super::*;

// ---------------------------------------------------------------------------
// Owner approval surface (U08 / FR-8, §4.1)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(super) struct ApprovalListQuery {
    status: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DecisionInput {
    expected_revision: i64,
    preview_digest: String,
}

pub(super) async fn list_approvals(
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

pub(super) async fn get_approval(
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
pub(super) async fn decision(
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

pub(super) fn decision_error_response(
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

pub(super) fn busy_decision_job() -> crate::application::writer::JobResult {
    crate::application::writer::JobResult {
        commit: false,
        body: Value::Null,
        failure: Some(crate::application::writer::Failure::Unavailable),
    }
}

pub(super) async fn grant_approval(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    decision(&state, headers, id, body, DecisionAction::Grant).await
}

pub(super) async fn deny_approval(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    decision(&state, headers, id, body, DecisionAction::Deny).await
}
