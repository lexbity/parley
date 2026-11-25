use super::*;
use serde::Serialize;
use serde_json::json;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/resource-board", get(get_resource_board))
        .route(
            "/api/v1/resource-board/columns",
            post(create_resource_column),
        )
        .route(
            "/api/v1/resource-board/columns/{id}",
            axum::routing::delete(remove_resource_column).patch(rename_resource_column),
        )
        .route("/api/v1/resource-cards/{id}", get(get_resource_card))
        .route(
            "/api/v1/resource-cards/{id}/deliveries",
            get(get_resource_card_deliveries),
        )
        .route(
            "/api/v1/resource-cards/{id}/retire",
            post(retire_resource_card),
        )
        .route(
            "/api/v1/resource-cards/{id}/erase",
            post(erase_resource_card),
        )
        .route(
            "/api/v1/resource-cards/{id}/deliveries/{send_id}/{material_id}/{version}",
            get(get_delivered_resource_version),
        )
        .route("/api/v1/resource-card-commits", post(commit_resource_card))
        .route("/api/v1/resource-board/moves", post(move_resource_card))
        .route("/api/v1/entities/{id}/attachments", get(list_attachments))
}

// Handlers.

async fn get_resource_board(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<BoardQuery>,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return error.into_response();
    }
    match state
        .workspace
        .resource_board_snapshot(query.limit, query.cursor, query.include_offboard)
        .await
    {
        Ok(board) => (StatusCode::OK, Json(board)).into_response(),
        Err(error) => rb_error(error),
    }
}

async fn create_resource_column(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CreateResourceColumn>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.create_resource_column(&input, &key)).await
}

async fn remove_resource_column(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<RemoveResourceColumn>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.remove_resource_column(&id, &input, &key)).await
}

async fn rename_resource_column(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<RenameResourceColumn>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.rename_resource_column(&id, &input, &key)).await
}

async fn erase_resource_card(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    match state.workspace.erase_resource_card(&id, &key).await {
        Ok(EraseOutcome::Erased(result)) => (StatusCode::OK, Json(result)).into_response(),
        Ok(EraseOutcome::Refused(refusal)) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": {"code": refusal.code(), "message": refusal.message(), "details": []}
            })),
        )
            .into_response(),
        Err(error) => rb_error(error),
    }
}

async fn get_resource_card(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return error.into_response();
    }
    match state.workspace.resource_card_read(&id).await {
        Ok(entity) => (StatusCode::OK, Json(json!({"data": entity}))).into_response(),
        Err(WorkspaceError::NotFound) => {
            let lifecycle: Result<Option<String>, _> = sqlx::query_scalar(
                "SELECT CASE WHEN EXISTS(SELECT 1 FROM erasure_tombstone WHERE entity_id=?1) THEN 'erased'
                             WHEN EXISTS(SELECT 1 FROM entity WHERE id=?1 AND subtype<>'' AND (readiness='retired' OR retired_at IS NOT NULL)) THEN 'retired'
                        END",
            ).bind(&id).fetch_one(state.workspace.pool()).await;
            match lifecycle {
                Ok(Some(value)) if value == "erased" => error(
                    StatusCode::NOT_FOUND,
                    "RESOURCE_ERASED",
                    "This resource was erased.",
                ),
                Ok(Some(value)) if value == "retired" => error(
                    StatusCode::NOT_FOUND,
                    "RESOURCE_RETIRED",
                    "This resource was retired.",
                ),
                Ok(_) => rb_error(WorkspaceError::NotFound),
                Err(reason) => rb_error(reason.into()),
            }
        }
        Err(error) => rb_error(error),
    }
}

async fn get_resource_card_deliveries(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return error.into_response();
    }
    match state.workspace.resource_card_deliveries(&id).await {
        Ok(rows) => (StatusCode::OK, Json(json!({"data": rows}))).into_response(),
        Err(error) => rb_error(error),
    }
}

#[derive(Serialize, sqlx::FromRow)]
struct DeliveredResourceVersion {
    resource_title: String,
    resource_revision: i64,
    body: String,
    filename: Option<String>,
}

async fn get_delivered_resource_version(
    State(state): State<AppState>,
    Path((id, send_id, material_id, version)): Path<(String, String, String, i64)>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return error.into_response();
    }
    let row = sqlx::query_as::<_, DeliveredResourceVersion>(
        "SELECT d.resource_title,d.resource_revision,mv.body,mv.filename
         FROM delivered_resource_version d
         JOIN send_part sp ON sp.material_id=d.material_id AND sp.version_no=d.version_no
         JOIN material_version mv ON mv.material_id=sp.material_id AND mv.version_no=sp.version_no
         WHERE d.resource_id=? AND sp.send_id=? AND sp.material_id=? AND sp.version_no=?",
    )
    .bind(id)
    .bind(send_id)
    .bind(material_id)
    .bind(version)
    .fetch_optional(state.workspace.pool())
    .await;
    match row {
        Ok(Some(version)) => {
            let mut response = Json(version).into_response();
            crate::private_headers(&mut response, state.workspace.epoch());
            response
        }
        Ok(None) => rb_error(WorkspaceError::NotFound),
        Err(error) => rb_error(error.into()),
    }
}

async fn retire_resource_card(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.retire_resource_card(&id, &key)).await
}

async fn commit_resource_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<ResourceCardCommitInput>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.commit_resource_card(&input, &key)).await
}

async fn move_resource_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<MoveResourceCardInput>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.move_resource_card(&input, &key)).await
}

async fn list_attachments(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return error.into_response();
    }
    match state.workspace.list_attachments(&id).await {
        Ok(items) => (StatusCode::OK, Json(json!({"data": items}))).into_response(),
        Err(error) => rb_error(error),
    }
}

async fn command_key(state: &AppState, headers: &HeaderMap) -> Result<String, Box<Response>> {
    if let Err(error) = authenticate(headers, state).await {
        return Err(Box::new(error.into_response()));
    }
    idempotency_key(headers).map_err(|error| Box::new(key_error(error)))
}

async fn command_handler<F>(command: F) -> Response
where
    F: std::future::Future<Output = Result<Value, WorkspaceError>>,
{
    match command.await {
        Ok(value) => {
            let status = value
                .get("_replay_status")
                .and_then(Value::as_i64)
                .unwrap_or(StatusCode::OK.as_u16() as i64) as u16;
            let body = value.get("result").cloned().unwrap_or(value);
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            (status, Json(body)).into_response()
        }
        Err(error) => rb_error(error),
    }
}

fn key_error(error: (StatusCode, &'static str, &'static str)) -> Response {
    (
        error.0,
        Json(json!({"error": {"code": error.1, "message": error.2, "details": []}})),
    )
        .into_response()
}

fn rb_error(error: WorkspaceError) -> Response {
    let (status, code, message) = match error {
        WorkspaceError::Database(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "WRITE_FAILED",
            "The workspace could not complete the command".to_owned(),
        ),
        WorkspaceError::Invalid(message) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "INVALID_REQUEST", message)
        }
        WorkspaceError::IdempotencyConflict => (
            StatusCode::CONFLICT,
            "KEY_REUSED",
            "Idempotency key was used for a different command".to_owned(),
        ),
        WorkspaceError::NotFound => (
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Resource does not exist".to_owned(),
        ),
        WorkspaceError::StaleRevision => (
            StatusCode::PRECONDITION_FAILED,
            "STALE_REVISION",
            "The addressed resource, notes, placement, or column changed in another session"
                .to_owned(),
        ),
        WorkspaceError::LedgerFull => (
            StatusCode::SERVICE_UNAVAILABLE,
            "LEDGER_CAPACITY",
            "The command ledger is at capacity".to_owned(),
        ),
    };
    (
        status,
        Json(json!({"error": {"code": code, "message": message, "details": []}})),
    )
        .into_response()
}
