//! Typed resource operations (UCR-001 U06 / Q6).
//!
//! `entity` is the sole resource record; the legacy `resource` table is
//! gone. These operations own the resource rules shared by the REST adapters
//! and the registry commands: draft creation, readiness transition, safe
//! summary reads, and the card/content revision split. Card and notes edits
//! never touch `resource_content_revision`; subtype/title/body changes do,
//! so an approved material's content proof survives a board move or a notes
//! edit (U06).

use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};

use crate::new_id;
use crate::now_utc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ResourceDraftInput {
    pub title: String,
    pub subtype: String,
    /// Draft body. `None` starts with an empty `{}` body.
    pub body: Option<serde_json::Value>,
    /// Digest of the bounded operation key, stored on the emitted event
    /// instead of the raw key (Q1).
    pub operation_key_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ResourceDraftResult {
    pub id: String,
    pub title: String,
    pub subtype: String,
    pub readiness: String,
    pub revision: i64,
    pub content_revision: i64,
    pub event_id: String,
    pub event_cursor: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ResourceDraftError {
    InvalidTitle,
    InvalidSubtype,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ResourceReadyInput {
    pub id: String,
    pub expected_content_revision: i64,
    /// Optional body override applied with the readiness transition (REST
    /// adapter). `None` validates and persists the stored body.
    pub body_override: Option<serde_json::Value>,
    pub operation_key_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ResourceReadyResult {
    pub id: String,
    pub title: String,
    pub subtype: String,
    pub readiness: String,
    pub revision: i64,
    pub content_revision: i64,
    pub event_id: Option<String>,
    pub event_cursor: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ResourceReadyError {
    NotFound,
    StaleRevision,
    NotDraft,
    MissingFields(Vec<String>),
    UnsupportedSubtype,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ResourceReadyOutcome {
    Changed(ResourceReadyResult),
    NoOp(ResourceReadyResult),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ResourceSummary {
    pub id: String,
    pub subtype: String,
    pub title: String,
    pub readiness: String,
    pub content_revision: i64,
}

pub(crate) async fn create_draft(
    tx: &mut Transaction<'_, Sqlite>,
    input: &ResourceDraftInput,
) -> Result<ResourceDraftResult, ResourceDraftError> {
    let title = input.title.trim();
    if title.is_empty() || title.chars().count() > 240 {
        return Err(ResourceDraftError::InvalidTitle);
    }
    if !parley_domain::resources::ALL_SUBTYPES.contains(&input.subtype.as_str()) {
        return Err(ResourceDraftError::InvalidSubtype);
    }
    let body = input.body.clone().unwrap_or_else(|| serde_json::json!({}));
    if !body.is_object() {
        return Err(ResourceDraftError::InvalidSubtype);
    }
    let id = new_id(&crate::digest(
        format!("{title}\0{}\0", input.subtype).as_bytes(),
    ));
    let now = now_utc();
    sqlx::query(
        "INSERT INTO entity(id,kind,name,focus,notes_md,notes_rev,revision,created_at,updated_at,subtype,body_json,readiness,resource_content_revision,subtype_locked) \
         VALUES(?,?,?,'',?,1,1,?,?,?,?,'draft',1,0)",
    )
    .bind(&id)
    .bind("resource")
    .bind(title)
    .bind("")
    .bind(&now)
    .bind(&now)
    .bind(&input.subtype)
    .bind(serde_json::to_string(&body).map_err(|_| ResourceDraftError::InvalidSubtype)?)
    .execute(&mut **tx)
    .await
    .map_err(|_| ResourceDraftError::InvalidSubtype)?;
    let event_id = new_id(&id);
    let cursor: i64 = sqlx::query_scalar(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'resource',?,1,'resource.created',?) RETURNING cursor",
    )
    .bind(&event_id)
    .bind(&now)
    .bind(&id)
    .bind(
        serde_json::json!({
            "subtype": input.subtype,
            "title": title,
            "operation_key_digest": input.operation_key_digest,
        })
        .to_string(),
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| ResourceDraftError::InvalidSubtype)?;
    Ok(ResourceDraftResult {
        id,
        title: title.to_owned(),
        subtype: input.subtype.clone(),
        readiness: "draft".into(),
        revision: 1,
        content_revision: 1,
        event_id,
        event_cursor: cursor,
    })
}

type ReadyRow = (String, String, String, i64, i64);

pub(crate) async fn mark_ready(
    tx: &mut Transaction<'_, Sqlite>,
    input: &ResourceReadyInput,
) -> Result<ResourceReadyOutcome, ResourceReadyError> {
    let row: Option<ReadyRow> = sqlx::query_as(
        "SELECT name,subtype,readiness,revision,resource_content_revision FROM entity \
         WHERE id=? AND subtype<>'' AND retired_at IS NULL",
    )
    .bind(&input.id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| ResourceReadyError::NotFound)?;
    let Some((title, subtype, readiness, card_revision, content_revision)) = row else {
        return Err(ResourceReadyError::NotFound);
    };
    if content_revision != input.expected_content_revision {
        return Err(ResourceReadyError::StaleRevision);
    }
    if readiness == "ready" {
        return Ok(ResourceReadyOutcome::NoOp(ResourceReadyResult {
            id: input.id.clone(),
            title,
            subtype,
            readiness,
            revision: card_revision,
            content_revision,
            event_id: None,
            event_cursor: None,
        }));
    }
    if readiness != "draft" {
        return Err(ResourceReadyError::NotDraft);
    }
    let stored_body: String = sqlx::query_scalar("SELECT body_json FROM entity WHERE id=?")
        .bind(&input.id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| ResourceReadyError::NotFound)?;
    let body_value = match &input.body_override {
        Some(override_value) => override_value.clone(),
        None => serde_json::from_str(&stored_body).unwrap_or_else(|_| serde_json::json!({})),
    };
    let Some(missing) = parley_domain::resources::validate_resource(&subtype, &body_value) else {
        return Err(ResourceReadyError::UnsupportedSubtype);
    };
    if !missing.is_empty() {
        return Err(ResourceReadyError::MissingFields(missing));
    }
    let next_content = content_revision + 1;
    let next_card = card_revision + 1;
    let now = now_utc();
    let body_json = serde_json::to_string(&body_value)
        .map_err(|_| ResourceReadyError::MissingFields(Vec::new()))?;
    let updated = sqlx::query(
        "UPDATE entity SET readiness='ready',subtype_locked=1,body_json=?,revision=?,resource_content_revision=?,updated_at=? WHERE id=? AND revision=?",
    )
    .bind(body_json)
    .bind(next_card)
    .bind(next_content)
    .bind(&now)
    .bind(&input.id)
    .bind(card_revision)
    .execute(&mut **tx)
    .await
    .map_err(|_| ResourceReadyError::NotFound)?;
    if updated.rows_affected() != 1 {
        return Err(ResourceReadyError::StaleRevision);
    }
    let event_id = new_id(&input.id);
    let cursor: i64 = sqlx::query_scalar(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'resource',?,?,'resource.readied',?) RETURNING cursor",
    )
    .bind(&event_id)
    .bind(&now)
    .bind(&input.id)
    .bind(next_content)
    .bind(
        serde_json::json!({
            "subtype": subtype,
            "content_revision": next_content,
            "operation_key_digest": input.operation_key_digest,
        })
        .to_string(),
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| ResourceReadyError::NotFound)?;
    Ok(ResourceReadyOutcome::Changed(ResourceReadyResult {
        id: input.id.clone(),
        title,
        subtype,
        readiness: "ready".into(),
        revision: next_card,
        content_revision: next_content,
        event_id: Some(event_id),
        event_cursor: Some(cursor),
    }))
}

/// Safe summary for agent Tier 1 reads and owner palette reads: no notes,
/// no attachments, no private evidence (U02 rule applied to resources).
pub(crate) async fn summary(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<ResourceSummary>, sqlx::Error> {
    let row: Option<(String, String, String, i64)> = sqlx::query_as(
        "SELECT subtype,name,readiness,resource_content_revision FROM entity \
         WHERE id=? AND subtype<>'' AND retired_at IS NULL AND readiness<>'retired'",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(
        |(subtype, title, readiness, content_revision)| ResourceSummary {
            id: id.to_owned(),
            subtype,
            title,
            readiness,
            content_revision,
        },
    ))
}
