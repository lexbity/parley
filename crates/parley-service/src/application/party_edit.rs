//! Typed party edit operation (UCR-001 U04).
//!
//! Shared by the `party.rename` / `party.set_focus` registry commands and the
//! REST entity PATCH adapter. The transport parses the strong ETag into the
//! expected revision; this operation re-checks it under reservation and
//! performs the conditional update. Party edits only change the labeled sheet
//! fields — contact policy, claims, graph edges, provenance and source rows
//! are never touched here. Non-party card kinds (resources and other graph
//! kinds) are concealed as unavailable.

use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};

use crate::new_id;
use crate::now_utc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PartyEditInput {
    pub party_id: String,
    /// Expected card revision, parsed by the adapter from the strong ETag.
    pub expected_revision: i64,
    /// New name, when the intent is a rename. `None` keeps the current name.
    pub name: Option<String>,
    /// New focus, when the intent is a focus update. `None` keeps the
    /// current focus.
    pub focus: Option<String>,
    /// Digest of the bounded operation key, stored on the emitted event
    /// instead of the raw key.
    pub operation_key_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct PartyEditResult {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub focus: String,
    pub revision: i64,
    pub event_id: Option<String>,
    pub event_cursor: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PartyEditError {
    /// Concealed or absent target: hidden scopes and nonexistent IDs share
    /// the same public denial.
    NotFound,
    StaleRevision,
    InvalidName,
    InvalidFocus,
    NoFields,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PartyEditOutcome {
    Changed(PartyEditResult),
    /// True no-op: current permitted state, zero domain events.
    NoOp(PartyEditResult),
}

type PartyRowRecord = (String, String, String, i64);

pub(crate) async fn apply_party_edit(
    tx: &mut Transaction<'_, Sqlite>,
    input: &PartyEditInput,
) -> Result<PartyEditOutcome, PartyEditError> {
    let row: Option<PartyRowRecord> =
        sqlx::query_as("SELECT kind,name,focus,revision FROM graph_card WHERE id=?")
            .bind(&input.party_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| PartyEditError::NotFound)?;
    let Some((kind, current_name, current_focus, revision)) = row else {
        return Err(PartyEditError::NotFound);
    };
    // Guard the actual target kind: the typed party commands are for party
    // records only. Resources and other graph kinds are concealed.
    if !matches!(kind.as_str(), "person" | "org") {
        return Err(PartyEditError::NotFound);
    }
    if revision != input.expected_revision {
        return Err(PartyEditError::StaleRevision);
    }

    let mut next_name = current_name.clone();
    let mut next_focus = current_focus.clone();
    let mut touched = false;
    if let Some(name) = &input.name {
        let trimmed = name.trim();
        if trimmed.is_empty() || trimmed.len() > 500 {
            return Err(PartyEditError::InvalidName);
        }
        if trimmed != current_name {
            next_name = trimmed.to_owned();
            touched = true;
        }
    }
    if let Some(focus) = &input.focus {
        if focus.chars().count() > 250 {
            return Err(PartyEditError::InvalidFocus);
        }
        if *focus != current_focus {
            next_focus = focus.clone();
            touched = true;
        }
    }
    if input.name.is_none() && input.focus.is_none() {
        return Err(PartyEditError::NoFields);
    }

    if !touched {
        return Ok(PartyEditOutcome::NoOp(PartyEditResult {
            id: input.party_id.clone(),
            kind,
            name: next_name,
            focus: next_focus,
            revision,
            event_id: None,
            event_cursor: None,
        }));
    }

    let next_revision = revision + 1;
    let now = now_utc();
    let updated = sqlx::query(
        "UPDATE entity SET name=?,focus=?,revision=?,updated_at=? WHERE id=? AND revision=?",
    )
    .bind(&next_name)
    .bind(&next_focus)
    .bind(next_revision)
    .bind(&now)
    .bind(&input.party_id)
    .bind(revision)
    .execute(&mut **tx)
    .await
    .map_err(|_| PartyEditError::NotFound)?;
    if updated.rows_affected() != 1 {
        return Err(PartyEditError::StaleRevision);
    }
    sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
        .execute(&mut **tx)
        .await
        .map_err(|_| PartyEditError::NotFound)?;

    let event_id = new_id(&input.party_id);
    let cursor: i64 = sqlx::query_scalar(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'entity',?,?,'entity.updated',?) RETURNING cursor",
    )
    .bind(&event_id)
    .bind(&now)
    .bind(&input.party_id)
    .bind(next_revision)
    .bind(serde_json::json!({
        "operation_key_digest": input.operation_key_digest,
        "name_changed": next_name != current_name,
        "focus_changed": next_focus != current_focus,
    }).to_string())
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| PartyEditError::NotFound)?;

    Ok(PartyEditOutcome::Changed(PartyEditResult {
        id: input.party_id.clone(),
        kind,
        name: next_name,
        focus: next_focus,
        revision: next_revision,
        event_id: Some(event_id),
        event_cursor: Some(cursor),
    }))
}
