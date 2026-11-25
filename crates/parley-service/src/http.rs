//! Owner HTTP handlers for the unified entity, event, capture and graph API.
//! Moved verbatim from the crate root so `lib.rs` keeps the crate surface,
//! workspace impls and shared helpers.

use crate::*;
use axum::{
    Json,
    extract::{Path as AxumPath, Query, RawQuery, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, time::Instant};

pub(super) async fn health() -> Json<Value> {
    Json(json!({"status":"ready","model":"not-probed"}))
}

pub(super) async fn index(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = check_authority(&headers, &state) {
        return response.into_response();
    }
    if web_assets::INDEX.is_empty() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "BUNDLE_UNAVAILABLE",
            "The owner interface bundle is not installed",
        );
    }
    let Ok(html) = std::str::from_utf8(web_assets::INDEX) else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "BUNDLE_INVALID",
            "The owner interface bundle is unreadable",
        );
    };
    let mut response = html.to_owned().into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

pub(super) async fn asset(AxumPath(path): AxumPath<String>) -> Response {
    let request_path = format!("assets/{path}");
    match web_assets::ASSETS
        .iter()
        .find(|(name, _, _)| *name == request_path)
    {
        Some((_, mime, bytes)) => {
            let mut response = Body::from(*bytes).into_response();
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            );
            response
        }
        None => error(StatusCode::NOT_FOUND, "NOT_FOUND", "Asset does not exist"),
    }
}

#[derive(Deserialize)]
pub(super) struct BootstrapInput {
    secret: String,
}

pub(super) async fn bootstrap(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<BootstrapInput>,
) -> Response {
    if let Err(response) = check_authority(&headers, &state) {
        return response.into_response();
    }
    let mut stored = state.bootstrap.lock().await;
    if !stored
        .remove(&input.secret)
        .is_some_and(|expires| Instant::now() < expires)
    {
        return error(
            StatusCode::UNAUTHORIZED,
            "BOOTSTRAP_INVALID",
            "Bootstrap secret is invalid or expired",
        );
    }
    let session_id = random_hex(32);
    let token = random_hex(32);
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), Session { token });
    let mut response = Json(json!({"ok": true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "parley_session={session_id}; HttpOnly; SameSite=Strict; Path=/api/v1; Max-Age=28800"
        ))
        .unwrap(),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

pub(super) async fn session(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session_id = match authenticate_session(&headers, &state).await {
        Ok(session_id) => session_id,
        Err(auth) => return auth.into_response(),
    };
    let sessions = state.sessions.lock().await;
    let Some(record) = sessions.get(&session_id) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "SESSION_EXPIRED",
            "The local session has expired",
        );
    };
    let cursor = state.workspace.current_cursor().await.unwrap_or(0);
    let mut response = Json(json!({
        "workspace_id": state.workspace.workspace_id(),
        "epoch": state.workspace.epoch(),
        "wire_major": 1,
        "wire_minor": 0,
        "capabilities": ["resources.read", "resources.write", "party.sheet", "party.notes", "party.dossier", "party.imports", "party.attachments", "party.relations", "resource.board", "resource.attachments", "resource.relationships"],
        "mutation_token": record.token,
        "cursor": cursor.to_string(),
    }))
    .into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

/// Validate the entity-kind query filter. `resource` selects owner resources;
/// person, organization and project kinds select directory entities.
pub(super) fn parse_kind_filter(kinds: &[String]) -> Result<(Vec<&str>, Vec<&str>), Rejection> {
    let mut resource_kinds = Vec::new();
    let mut graph_kinds = Vec::new();
    for kind in kinds {
        match kind.as_str() {
            "gift" | "proof" | "artifact" | "resource" => resource_kinds.push(kind.as_str()),
            "person" | "org" | "project" => graph_kinds.push(kind.as_str()),
            _ => {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "INVALID_KIND",
                    "Only workspace entity kinds can be listed in this slice",
                ));
            }
        }
    }
    Ok((resource_kinds, graph_kinds))
}

/// Repeated `kind` query parameters OR together (RFC-0004 §4.3); absent kinds
/// list both entity families.
pub(super) fn parse_entity_query(raw: Option<String>) -> Vec<String> {
    raw.map(|query| {
        form_urlencoded::parse(query.as_bytes())
            .filter_map(|(key, value)| (key == "kind").then(|| value.into_owned()))
            .collect()
    })
    .unwrap_or_default()
}

pub(super) fn resource_json(epoch: &str, record: &ResourceRecord) -> Value {
    json!({
        "id": record.id,
        "kind": record.kind,
        "title": record.title,
        "body": record.body,
        "readiness": record.readiness,
        "revision": record.revision,
        "etag": resource_etag(epoch, &record.id, record.revision),
    })
}

pub(super) async fn list_entities(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let kinds = parse_entity_query(query);
    let (resource_kinds, graph_kinds) = match parse_kind_filter(&kinds) {
        Ok(filter) => filter,
        Err((status, code, message)) => return error(status, code, message),
    };
    let mut items = Vec::new();
    if !resource_kinds.is_empty() || kinds.is_empty() {
        match state
            .workspace
            .list_resources_filtered(&resource_kinds)
            .await
        {
            Ok(records) => items.extend(
                records
                    .iter()
                    .map(|record| resource_json(state.workspace.epoch(), record)),
            ),
            Err(_) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "READ_FAILED",
                    "Could not read resources",
                );
            }
        }
    }
    let already_listed: std::collections::HashSet<String> = items
        .iter()
        .filter_map(|v| v["id"].as_str().map(str::to_owned))
        .collect();
    if !graph_kinds.is_empty() || kinds.is_empty() {
        match state.workspace.list_entities_filtered(&graph_kinds).await {
            Ok(entities) => items.extend(entities.into_iter().filter(|entity|!already_listed.contains(&entity.id)).map(|entity| {
                json!({"id": entity.id, "kind": entity.kind, "name": entity.name, "focus": entity.focus, "revision": entity.revision, "etag": entity_etag(state.workspace.epoch(), &entity.id, entity.revision)})
            })),
            Err(_) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "READ_FAILED",
                    "Could not read directory entities",
                );
            }
        }
    }
    let cursor = state.workspace.current_cursor().await.unwrap_or(0);
    let mut response = Json(json!({"data": items, "cursor": cursor.to_string()})).into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

/// The entity create command covers both entity families (RFC-0004 §3): owner
/// resources carry a title and typed body; directory entities carry a name.
#[derive(Deserialize)]
pub(super) struct ResourceCreateInput {
    kind: String,
    title: String,
    body: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GraphEntityCreateInput {
    #[serde(default)]
    notes: String,
    #[serde(default)]
    staged_upload_ids: Vec<String>,
    kind: String,
    name: String,
    focus: Option<String>,
}

pub(super) async fn create_entity(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    if body.len() > 2 * 1024 * 1024 {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "BODY_TOO_LARGE",
            "Request exceeds 2 MiB",
        );
    }
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err((status, code, message)) => return error(status, code, message),
    };
    let value: Value = match decode_unique(&body) {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "INVALID_JSON",
                "Request body is invalid",
            );
        }
    };
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(kind, "gift" | "proof" | "artifact") {
        let Ok(input) = serde_json::from_value::<ResourceCreateInput>(value) else {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "RESOURCE_INVALID",
                "Resource type or name is invalid",
            );
        };
        if input.title.chars().count() > 240 || !input.body.is_object() {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "RESOURCE_INVALID",
                "Resource type or name is invalid",
            );
        }
        match state
            .workspace
            .create_resource(CreateResource {
                idempotency_key: key,
                kind: input.kind,
                title: input.title,
                body: input.body,
            })
            .await
        {
            Ok(record) => {
                let etag = resource_etag(state.workspace.epoch(), &record.id, record.revision);
                let mut response = (
                    StatusCode::CREATED,
                    Json(json!({"data": resource_json(state.workspace.epoch(), &record)})),
                )
                    .into_response();
                if let Ok(tag) = HeaderValue::from_str(&etag) {
                    response.headers_mut().insert(header::ETAG, tag);
                }
                private_headers(&mut response, state.workspace.epoch());
                response
            }
            Err(err) => command_error(err),
        }
    } else if matches!(kind, "person" | "org" | "project" | "resource") {
        let Ok(input) = serde_json::from_value::<GraphEntityCreateInput>(value) else {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "ENTITY_INVALID",
                "Entity type or name is invalid",
            );
        };
        match state
            .workspace
            .create_card(
                &key,
                &input.kind,
                &input.name,
                input.focus.as_deref().unwrap_or(""),
                &input.notes,
                &input.staged_upload_ids,
            )
            .await
        {
            Ok(entity) => {
                let mut response = (
                    StatusCode::CREATED,
                    Json(json!({"data": {"id": entity.id, "kind": entity.kind, "name": entity.name, "focus": entity.focus, "revision": entity.revision, "etag": entity_etag(state.workspace.epoch(), &entity.id, entity.revision)}})),
                )
                    .into_response();
                private_headers(&mut response, state.workspace.epoch());
                response
            }
            Err(err) => command_error(err),
        }
    } else {
        error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "RESOURCE_INVALID",
            "Resource type or name is invalid",
        )
    }
}

pub(super) async fn get_entity(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    match state.workspace.get_resource(&id).await {
        Ok(Some(record)) => {
            let etag = resource_etag(state.workspace.epoch(), &record.id, record.revision);
            let mut response =
                Json(json!({"data": resource_json(state.workspace.epoch(), &record)}))
                    .into_response();
            if let Ok(tag) = HeaderValue::from_str(&etag) {
                response.headers_mut().insert(header::ETAG, tag);
            }
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Ok(None) => match state.workspace.get_entity(&id).await {
            Ok(Some(entity)) => {
                let etag = entity_etag(state.workspace.epoch(), &entity.id, entity.revision);
                let mut response = Json(json!({"data": {
                    "id": entity.id,
                    "kind": entity.kind,
                    "name": entity.name,
                    "focus": entity.focus,
                    "revision": entity.revision,
                    "etag": etag,
                }}))
                .into_response();
                if let Ok(tag) = HeaderValue::from_str(&etag) {
                    response.headers_mut().insert(header::ETAG, tag);
                }
                private_headers(&mut response, state.workspace.epoch());
                response
            }
            Ok(None) => error(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "Resource does not exist",
            ),
            Err(_) => error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "READ_FAILED",
                "Could not read resource",
            ),
        },
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "READ_FAILED",
            "Could not read resource",
        ),
    }
}

#[derive(Deserialize)]
pub(super) struct UpdateInput {
    title: String,
    body: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GraphEntityUpdateInput {
    name: Option<String>,
    focus: Option<String>,
}

pub(super) async fn update_entity(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err((status, code, message)) => return error(status, code, message),
    };
    let Some(if_match) = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
    else {
        return error(
            StatusCode::PRECONDITION_REQUIRED,
            "ETAG_REQUIRED",
            "If-Match is required",
        );
    };
    if !is_strong_etag(if_match) {
        return error(
            StatusCode::BAD_REQUEST,
            "ETAG_INVALID",
            "If-Match must be a strong resource ETag",
        );
    }
    if body.len() > 2 * 1024 * 1024 {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "BODY_TOO_LARGE",
            "Request exceeds 2 MiB",
        );
    }
    let value: Value = match decode_unique(&body) {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "INVALID_JSON",
                "Request body is invalid",
            );
        }
    };
    // Owner-resource edit (existing route semantics).
    if let Ok(input) = serde_json::from_value::<UpdateInput>(value.clone()) {
        if input.title.chars().count() > 240 || !input.body.is_object() {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "RESOURCE_INVALID",
                "Resource name or fields are invalid",
            );
        }
        match state
            .workspace
            .update_resource(&key, &id, if_match, &input.title, &input.body)
            .await
        {
            Ok(record) => {
                let etag = resource_etag(state.workspace.epoch(), &record.id, record.revision);
                let mut response =
                    Json(json!({"data": resource_json(state.workspace.epoch(), &record)}))
                        .into_response();
                if let Ok(tag) = HeaderValue::from_str(&etag) {
                    response.headers_mut().insert(header::ETAG, tag);
                }
                private_headers(&mut response, state.workspace.epoch());
                response
            }
            Err(WorkspaceError::NotFound) => {
                entity_update_response(
                    &state,
                    &key,
                    &id,
                    if_match,
                    serde_json::from_value::<GraphEntityUpdateInput>(value),
                )
                .await
            }
            Err(err) => command_error(err),
        }
    } else {
        entity_update_response(
            &state,
            &key,
            &id,
            if_match,
            serde_json::from_value::<GraphEntityUpdateInput>(value),
        )
        .await
    }
}

pub(super) async fn entity_update_response(
    state: &AppState,
    key: &str,
    id: &str,
    if_match: &str,
    input: Result<GraphEntityUpdateInput, serde_json::Error>,
) -> Response {
    let Ok(input) = input else {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "ENTITY_INVALID",
            "Entity fields are invalid",
        );
    };
    match state
        .workspace
        .update_entity(key, id, if_match, input.name, input.focus)
        .await
    {
        Ok(entity) => {
            let etag = entity_etag(state.workspace.epoch(), &entity.id, entity.revision);
            let mut response = Json(json!({"data": {
                "id": entity.id,
                "kind": entity.kind,
                "name": entity.name,
                "focus": entity.focus,
                "revision": entity.revision,
                "etag": etag,
            }}))
            .into_response();
            if let Ok(tag) = HeaderValue::from_str(&etag) {
                response.headers_mut().insert(header::ETAG, tag);
            }
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(WorkspaceError::NotFound) => error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Resource does not exist",
        ),
        Err(err) => command_error(err),
    }
}

#[derive(Deserialize)]
pub(super) struct EntityNotesUpdateInput {
    markdown: String,
    expected_notes_rev: i64,
}

pub(super) async fn get_entity_notes(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    match state.workspace.get_entity_notes(&id).await {
        Ok(Some(notes)) => {
            let etag = notes_etag(state.workspace.epoch(), &id, notes.revision);
            let mut response = Json(json!({"data": {
                "entity_id": notes.entity_id,
                "markdown": notes.markdown,
                "revision": notes.revision,
                "etag": etag,
            }}))
            .into_response();
            if let Ok(tag) = HeaderValue::from_str(&etag) {
                response.headers_mut().insert(header::ETAG, tag);
            }
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Resource does not exist",
        ),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "READ_FAILED",
            "Could not read entity notes",
        ),
    }
}

pub(super) async fn update_entity_notes(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err((status, code, message)) => return error(status, code, message),
    };
    let Some(if_match) = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
    else {
        return error(
            StatusCode::PRECONDITION_REQUIRED,
            "ETAG_REQUIRED",
            "If-Match is required",
        );
    };
    if !is_strong_etag(if_match) {
        return error(
            StatusCode::BAD_REQUEST,
            "ETAG_INVALID",
            "If-Match must be a strong notes ETag",
        );
    }
    if body.len() > 512 * 1024 + 64 {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "BODY_TOO_LARGE",
            "Notes exceed the size limit",
        );
    }
    let input: EntityNotesUpdateInput = match decode_unique(&body) {
        Ok(input) => input,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "INVALID_JSON",
                "Request body is invalid",
            );
        }
    };
    match state
        .workspace
        .update_entity_notes(
            &key,
            &id,
            if_match,
            input.expected_notes_rev,
            &input.markdown,
        )
        .await
    {
        Ok(notes) => {
            let etag = notes_etag(state.workspace.epoch(), &id, notes.revision);
            let mut response = Json(json!({"data": {
                "entity_id": notes.entity_id,
                "markdown": notes.markdown,
                "revision": notes.revision,
                "etag": etag,
            }}))
            .into_response();
            if let Ok(tag) = HeaderValue::from_str(&etag) {
                response.headers_mut().insert(header::ETAG, tag);
            }
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(WorkspaceError::NotFound) => error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Resource does not exist",
        ),
        Err(err) => command_error(err),
    }
}

pub(super) async fn get_entity_dossier(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    match state.workspace.entity_dossier(&id).await {
        Ok(Some(dossier)) => {
            let etag = entity_etag(
                state.workspace.epoch(),
                &dossier.entity.id,
                dossier.entity.revision,
            );
            let mut value = json!(dossier);
            value["entity"]["etag"] = Value::String(etag);
            let mut response = Json(json!({"data": value})).into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Resource does not exist",
        ),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "READ_FAILED",
            "Could not read the entity dossier",
        ),
    }
}

#[derive(Deserialize)]
pub(super) struct EventsQuery {
    after: Option<String>,
    /// Optional focused reads (Queue inspector history): restrict the feed to
    /// one entity without paging the whole workspace.
    entity_type: Option<String>,
    entity_id: Option<String>,
}

pub(super) async fn get_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let after = match query.after.as_deref().unwrap_or("0").parse::<i64>() {
        Ok(cursor) if cursor >= 0 => cursor,
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "CURSOR_INVALID",
                "Event cursor must be a non-negative decimal",
            );
        }
    };
    let observed = state.workspace.current_cursor().await.unwrap_or(0);
    if after > observed {
        return error(
            StatusCode::CONFLICT,
            "CURSOR_AHEAD",
            "Event cursor is ahead of the workspace",
        );
    }
    type EventRow = (i64, String, String, String, i64, String, String, String);
    let rows = match (query.entity_type.as_deref(), query.entity_id.as_deref()) {
        (None, None) => {
            sqlx::query_as::<_, EventRow>(
                "SELECT cursor,event_id,entity_type,entity_id,revision,event_type,payload_json,occurred_at FROM event WHERE cursor>? ORDER BY cursor LIMIT 200",
            )
            .bind(after)
            .fetch_all(state.workspace.pool())
            .await
        }
        (Some(entity_type), None) => {
            sqlx::query_as::<_, EventRow>(
                "SELECT cursor,event_id,entity_type,entity_id,revision,event_type,payload_json,occurred_at FROM event WHERE cursor>? AND entity_type=? ORDER BY cursor LIMIT 200",
            )
            .bind(after)
            .bind(entity_type)
            .fetch_all(state.workspace.pool())
            .await
        }
        (None, Some(entity_id)) => {
            sqlx::query_as::<_, EventRow>(
                "SELECT cursor,event_id,entity_type,entity_id,revision,event_type,payload_json,occurred_at FROM event WHERE cursor>? AND entity_id=? ORDER BY cursor LIMIT 200",
            )
            .bind(after)
            .bind(entity_id)
            .fetch_all(state.workspace.pool())
            .await
        }
        (Some(entity_type), Some(entity_id)) => {
            sqlx::query_as::<_, EventRow>(
                "SELECT cursor,event_id,entity_type,entity_id,revision,event_type,payload_json,occurred_at FROM event WHERE cursor>? AND entity_type=? AND entity_id=? ORDER BY cursor LIMIT 200",
            )
            .bind(after)
            .bind(entity_type)
            .bind(entity_id)
            .fetch_all(state.workspace.pool())
            .await
        }
    };
    let rows = match rows {
        Ok(rows) => rows,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "READ_FAILED",
                "Could not read workspace events",
            );
        }
    };
    let items: Vec<Value> = rows
        .into_iter()
        .map(
            |(
                cursor,
                event_id,
                entity_type,
                entity_id,
                revision,
                event_type,
                payload,
                occurred_at,
            )| {
                json!({
                    "cursor": cursor.to_string(),
                    "event_id": event_id,
                    "entity_type": entity_type,
                    "entity_id": entity_id,
                    "revision": revision,
                    "event_type": event_type,
                    "occurred_at": occurred_at,
                    "payload": serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null),
                })
            },
        )
        .collect();
    let next_cursor = items
        .last()
        .and_then(|item| item["cursor"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| after.to_string());
    let mut response = Json(json!({
        "items": items,
        "next_cursor": next_cursor,
        "observed_cursor": observed.to_string(),
    }))
    .into_response();
    private_headers(&mut response, state.workspace.epoch());
    response
}

#[derive(Deserialize)]
pub(super) struct CaptureRequest {
    text: String,
    title: Option<String>,
    url: Option<String>,
}

pub(super) async fn save_capture(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CaptureRequest>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err(error) => {
            return (
                error.0,
                Json(json!({"error":{"code":error.1,"message":error.2,"details":[]}})),
            )
                .into_response();
        }
    };
    let canonical = digest(
        format!(
            "POST\n/api/v1/captures\n{}",
            serde_json::to_string(&json!({"text":input.text,"title":input.title,"url":input.url}))
                .unwrap_or_default()
        )
        .as_bytes(),
    );
    match state.workspace.save_capture_replay(&key, &canonical).await {
        Ok(Some(replay)) => {
            let mut response = (StatusCode::CREATED, Json(replay)).into_response();
            private_headers(&mut response, state.workspace.epoch());
            return response;
        }
        Ok(None) => {}
        Err(err) => return command_error(err),
    }
    match state
        .workspace
        .save_capture_commit(
            &key,
            &canonical,
            &input.text,
            input.title.as_deref(),
            input.url.as_deref(),
        )
        .await
    {
        Ok((job_id, full)) => {
            if job_id.is_some() {
                state.jobs.notify_one();
            }
            let mut response = (StatusCode::CREATED, Json(full)).into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(err) => {
            // A concurrent same-key commit may have won this race. Replay
            // its committed result instead of surfacing a lost PK write
            // (BQ1: one occurrence regardless of interleaving).
            match state.workspace.save_capture_replay(&key, &canonical).await {
                Ok(Some(replay)) => {
                    let mut response = (StatusCode::CREATED, Json(replay)).into_response();
                    private_headers(&mut response, state.workspace.epoch());
                    response
                }
                _ => command_error(err),
            }
        }
    }
}

pub(super) async fn list_captures(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    match state.workspace.list_sources().await {
        Ok(rows) => {
            let mut response = (StatusCode::OK, Json(rows)).into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "READ_FAILED",
            "Could not read saved sources",
        ),
    }
}

pub(super) async fn get_graph(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let values: HashMap<String, String> = query
        .as_deref()
        .map(|raw| {
            form_urlencoded::parse(raw.as_bytes())
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let center_id = values
        .get("center_id")
        .map(String::as_str)
        .filter(|id| !id.is_empty());
    let view_scope = values
        .get("view_scope")
        .map(String::as_str)
        .unwrap_or("leads");
    let view_id = values.get("view_id").map(String::as_str).unwrap_or("all");
    if view_scope.len() > 80 || view_id.len() > 160 {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "GRAPH_SCOPE_INVALID",
            "Graph view scope is too long",
        );
    }
    match state
        .workspace
        .graph_snapshot(center_id, view_scope, view_id)
        .await
    {
        Ok(snapshot) => {
            let mut response = Json(snapshot).into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "READ_FAILED",
            "Could not read the graph neighborhood",
        ),
    }
}

pub(super) async fn update_graph_layout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<GraphLayoutInput>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err((status, code, message)) => return error(status, code, message),
    };
    match state.workspace.save_graph_layout(&key, input).await {
        Ok(result) => {
            let mut response = Json(result).into_response();
            private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(error_value) => command_error(error_value),
    }
}

// ---------------------------------------------------------------------------
// TD-10 W2: legacy transport operation gate.
//
// Every non-exempt `/api/v1` mutation (POST/PUT/PATCH/DELETE) requires an
// explicit `Idempotency-Key` (400 `IDEMPOTENCY_KEY_REQUIRED` when missing).
// For the mutation families whose handlers do not already manage receipts,
// the gate additionally implements exactly-once: same key + same intent
// replays the stored receipt with zero second effect; same key + different
// intent fails 409 `KEY_REUSED`; a concurrently in-flight claim yields a
// transient 503 `WRITER_BUSY`. Handler-managed families (board, resource
// board, relations, party imports, registry commands, approvals, ...) keep
// their native receipts; the gate only enforces key presence there.
// ---------------------------------------------------------------------------

/// Body bytes the gate is willing to buffer for an intent digest. Uploads
/// (up to the 64 MiB blob cap) are content-addressed and pass through
/// key-presence-only; they are never buffered here.
const GATE_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Exempt transport classes: SSE, media bytes, session/bootstrap/health and
/// the embedded UI fall outside the mutation gate entirely. Preview routes
/// (import/restore/erasure/party-import) are read-only intent planners and
/// are not mutations, so they bypass the key policy too.
fn gate_exempt_path(path: &str) -> bool {
    path == "/api/v1/health"
        || path == "/api/v1/session/bootstrap"
        || path == "/api/v1/session"
        || path == "/api/v1/command-notifications"
        || path.starts_with("/api/v1/commands")
        || path.starts_with("/api/v1/approvals")
        || path.ends_with("/preview")
        || (path.starts_with("/api/v1/materials/") && path.ends_with("/download"))
}

/// Mutation families the gate fully manages (claim + replay + receipt).
/// These are the handlers that historically performed none of their own
/// idempotency bookkeeping; everything else uses native receipts.
fn gate_claimed_path(method: &Method, path: &str) -> bool {
    let post = method == Method::POST;
    let put = method == Method::PUT;
    let delete = method == Method::DELETE;
    // Blob store.
    (post && path == "/api/v1/uploads")
        || (post && path.starts_with("/api/v1/uploads/") && path.ends_with("/review"))
    // Maintenance.
        || (post && path == "/api/v1/backups")
        || (post && path == "/api/v1/exports")
        || (post && path == "/api/v1/imports/commit")
        || (post && path == "/api/v1/restore/commit")
        || (post && path.starts_with("/api/v1/parties/") && path.ends_with("/erase"))
        || (put && path == "/api/v1/settings/maintenance")
    // Threads / sends.
        || (post && path.starts_with("/api/v1/threads/") && path.ends_with("/send"))
        || (post && path.starts_with("/api/v1/sends/") && path.ends_with("/outcome"))
        || (post && path.starts_with("/api/v1/entities/") && path.ends_with("/clear-dnc"))
        || (post && path.starts_with("/api/v1/threads/") && path.ends_with("/reopen"))
        || (post && path.starts_with("/api/v1/opportunities/") && path.ends_with("/resolution"))
        || (post && path == "/api/v1/capacity")
    // Materials families without native receipts.
        || (post && path.starts_with("/api/v1/opportunities/") && path.ends_with("/participants"))
        || (post && path.starts_with("/api/v1/opportunities/") && path.contains("/sources/"))
        || (delete && path.starts_with("/api/v1/opportunities/") && path.contains("/sources/"))
        || (post && path.starts_with("/api/v1/opportunities/") && path.ends_with("/materials"))
        || (post && path.starts_with("/api/v1/materials/") && path.ends_with("/versions"))
        || (post && path.starts_with("/api/v1/materials/") && path.ends_with("/approve"))
        || (post && path.starts_with("/api/v1/materials/") && path.ends_with("/gate"))
        || (post && path.starts_with("/api/v1/materials/") && path.ends_with("/release"))
        || (post && path.starts_with("/api/v1/releases/") && path.ends_with("/ack"))
}

/// TD-10 W3: destructive legacy mutations are mapped to approval command ids
/// so the exact frozen intent stops at the owner approval inbox (FR-14).
/// These four families are the enumerated conservative destructive set that
/// executes through the legacy transport today; previews are excluded.
fn gate_destructive_command(path: &str) -> Option<&'static str> {
    if path == "/api/v1/imports/commit" {
        Some("import.commit")
    } else if path == "/api/v1/restore/commit" {
        Some("workspace.restore")
    } else if path.starts_with("/api/v1/parties/") && path.ends_with("/erase") {
        Some("party.erase")
    } else if path == "/api/v1/settings/maintenance" {
        Some("maintenance.settings")
    } else {
        None
    }
}

/// Approval gate outcome for a destructive legacy mutation.
enum LegacyApproval {
    /// No approval exists yet; a pending row was created with a frozen intent
    /// digest. The caller must reply 403 with the Location header.
    Required(String),
    /// The grant was consumed; execution may proceed.
    Proceed,
}

/// TD-10 W3: check and advance the approval state for a destructive legacy
/// mutation. A first request binds a pending approval with the canonical
/// intent digest; a granted request consumes the grant before execution.
async fn legacy_approval_gate(
    pool: &sqlx::SqlitePool,
    epoch: &str,
    principal: &str,
    command_id: &str,
    key: &str,
    canonical: &str,
    secret: &[u8],
) -> Result<LegacyApproval, (i64, &'static str, String)> {
    let secret = crate::commands::ledger::key_digest(secret, epoch, principal, command_id, key);
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|_| (503, "WRITER_BUSY", "The workspace is busy".into()))?;
    let existing =
        crate::commands::approval::lookup_by_key(&mut tx, epoch, principal, command_id, &secret)
            .await
            .map_err(|_| {
                (
                    503,
                    "WRITER_BUSY",
                    "The approval store is unavailable".into(),
                )
            })?;
    let Some(approval) = existing else {
        // First destructive intent: freeze it as a pending approval.
        let id = crate::commands::approval::create_pending(
            &mut tx,
            &crate::commands::approval::NewApproval {
                epoch,
                owner_pseudonym: principal,
                principal_pseudonym: principal,
                run_pseudonym: None,
                command_id,
                command_version: 1,
                key_digest: &secret,
                wire_args_digest: canonical,
                canonical_args_digest: canonical,
                dependency_fingerprint: canonical,
                preview_digest: canonical,
                args_json: canonical.to_owned(),
                preview_json: canonical.to_owned(),
            },
        )
        .await
        .map_err(|_| (503, "WRITER_BUSY", "The approval store is full".into()))?;
        let _ = tx.commit().await;
        return Ok(LegacyApproval::Required(id));
    };
    let status = approval.status;
    let _ = tx.commit().await;
    match status {
        crate::commands::approval::ApprovalStatus::Granted => {
            // Consume the grant in its own CAS; a stale revision refuses.
            let mut tx = pool
                .begin_with("BEGIN IMMEDIATE")
                .await
                .map_err(|_| (503, "WRITER_BUSY", "The workspace is busy".into()))?;
            let consumed = crate::commands::approval::consume(
                &mut tx,
                &approval.id,
                approval.revision,
                &crate::new_id("audit"),
            )
            .await
            .map_err(|_| (503, "WRITER_BUSY", "The approval store is busy".into()))?;
            let _ = tx.commit().await;
            if !consumed {
                return Err((
                    409,
                    "approval_stale",
                    "The approval changed; reload before continuing".into(),
                ));
            }
            Ok(LegacyApproval::Proceed)
        }
        crate::commands::approval::ApprovalStatus::Pending => {
            Ok(LegacyApproval::Required(approval.id.clone()))
        }
        crate::commands::approval::ApprovalStatus::Denied
        | crate::commands::approval::ApprovalStatus::Revoked => Err((
            403,
            "approval_denied",
            "This intent was denied; start a new operation key".into(),
        )),
        crate::commands::approval::ApprovalStatus::Expired
        | crate::commands::approval::ApprovalStatus::Stale => Err((
            409,
            "approval_stale",
            "This approval expired; start a new operation key".into(),
        )),
        crate::commands::approval::ApprovalStatus::Consumed => Err((
            409,
            "approval_consumed",
            "This approval was already consumed; start a new operation key".into(),
        )),
    }
}

/// TD-10 W2 gate. Mounted as the outermost layer on the composed app router.
pub(crate) async fn legacy_operation_gate(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    if matches!(method, Method::GET | Method::HEAD) {
        return next.run(request).await;
    }
    let path = request.uri().path().to_owned();
    if !path.starts_with("/api/v1/") || gate_exempt_path(&path) {
        return next.run(request).await;
    }

    // Preserve the auth ordering: unauthenticated mutations fail 401 before
    // any key policy is evaluated.
    let principal = match authenticate_owner(request.headers(), &state).await {
        Ok(value) => value,
        Err(response) => return response.into_response(),
    };

    let key = match idempotency_key(request.headers()) {
        Ok(key) => key,
        Err((status, code, message)) => return error(status, code, message),
    };
    let epoch = state.workspace.epoch().to_owned();

    if !gate_claimed_path(&method, &path) {
        return next.run(request).await;
    }

    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, GATE_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => {
            // Oversized body (upload): key-presence-only, never buffered.
            return next.run(Request::from_parts(parts, Body::empty())).await;
        }
    };
    let canonical = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => crate::canonical_intent(method.as_str(), &path, &value),
        Err(_) => digest(format!("{}\0{}\0{}", method, path, digest(&bytes)).as_bytes()),
    };
    let pool = state.workspace.pool().clone();

    let replay = crate::gate_replay(&pool, &epoch, &key, &canonical).await;
    match replay {
        Ok(Some(value)) => return crate::gate_replay_response(&epoch, value),
        Err(error) => return crate::command_error(error),
        Ok(None) => {}
    }

    // TD-10 W3: destructive legacy mutations stop at the approval inbox with
    // a frozen intent digest (FR-14). A pending/granted approval is bound to
    // the operation key; execution proceeds only after the grant is consumed.
    if let Some(command_id) = gate_destructive_command(&path) {
        let secret = state.workspace.command_secret();
        match legacy_approval_gate(
            &pool,
            &epoch,
            &principal.pseudonym,
            command_id,
            &key,
            &canonical,
            &secret,
        )
        .await
        {
            Ok(LegacyApproval::Required(id)) => {
                let mut response = error(
                    StatusCode::FORBIDDEN,
                    "approval_required",
                    "This operation requires owner approval before it can execute",
                );
                response.headers_mut().insert(
                    header::LOCATION,
                    HeaderValue::from_str(&format!("/api/v1/approvals/{id}"))
                        .unwrap_or_else(|_| HeaderValue::from_static("/api/v1/approvals")),
                );
                return response;
            }
            Ok(LegacyApproval::Proceed) => {}
            Err((status, code, message)) => {
                return error(
                    StatusCode::from_u16(status as u16)
                        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    code,
                    message,
                );
            }
        }
    }

    // Claim the key before running the handler so concurrent same-key
    // requests serialize instead of double-executing.
    let claimed = {
        let mut tx = match pool.begin_with("BEGIN IMMEDIATE").await {
            Ok(tx) => tx,
            Err(_) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "WRITER_BUSY",
                    "The workspace is busy. Retry the same operation.",
                );
            }
        };
        let inserted = sqlx::query(
            "INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,0,'{}',0,?) ON CONFLICT(owner_id,epoch,key) DO NOTHING",
        )
        .bind(&epoch)
        .bind(&key)
        .bind(&canonical)
        .bind(crate::now_utc())
        .execute(&mut *tx)
        .await;
        let inserted = match inserted {
            Ok(result) => result.rows_affected(),
            Err(_) => 0,
        };
        let _ = tx.commit().await;
        inserted
    };
    if claimed == 0 {
        // A concurrent same-key request claimed first; re-check for its
        // result rather than executing twice.
        match crate::gate_replay(&pool, &epoch, &key, &canonical).await {
            Ok(Some(value)) => return crate::gate_replay_response(&epoch, value),
            Ok(None) | Err(_) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "WRITER_BUSY",
                    "The workspace is busy. Retry the same operation.",
                );
            }
        }
    }

    let response = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;

    if response.status().is_success() {
        let (resp_parts, resp_body) = response.into_parts();
        // Exports/archives can legitimately exceed small JSON responses;
        // capture up to the blob ceiling before refusing to store a receipt.
        let out = match axum::body::to_bytes(resp_body, 8 * 1024 * 1024).await {
            Ok(bytes) => bytes,
            Err(_) => return Response::from_parts(resp_parts, Body::empty()),
        };
        let value: Value = serde_json::from_slice(&out).unwrap_or(Value::Null);
        let result_json = serde_json::to_string(&value).unwrap_or_else(|_| "{}".into());
        let status = resp_parts.status.as_u16() as i64;
        let cursor: i64 = sqlx::query_scalar("SELECT coalesce(max(cursor),0) FROM event")
            .fetch_one(&pool)
            .await
            .unwrap_or(0);
        // Update the pending claim into a finalized receipt.
        let _ = sqlx::query(
            "UPDATE idempotency_receipt SET canonical_digest=?,status_code=?,result_json=?,last_event_cursor=? WHERE owner_id=1 AND epoch=? AND key=?",
        )
        .bind(&canonical)
        .bind(status)
        .bind(&result_json)
        .bind(cursor)
        .bind(&epoch)
        .bind(&key)
        .execute(&pool)
        .await;
        Response::from_parts(resp_parts, Body::from(out))
    } else {
        // Failure is not sticky: drop the claim so the client may retry the
        // same key for a fresh attempt.
        let _ = sqlx::query(
            "DELETE FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=? AND status_code=0",
        )
        .bind(&epoch)
        .bind(&key)
        .execute(&pool)
        .await;
        response
    }
}
