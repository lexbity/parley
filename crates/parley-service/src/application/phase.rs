//! Typed opportunity phase operation (UCR-001 U01 / Q1).
//!
//! This is the single application operation behind both the REST PATCH
//! `/opportunities/{id}/phase` adapter and the `opportunity.set_phase` /
//! `opportunity.undo_phase_change` registry commands. It owns every phase
//! rule: stage resolution in the active pack, lifecycle and terminal
//! rejection, the revision guard (checked even for a reported no-op), exact
//! undo derivation from the latest manual transition, and restoration of an
//! Unmapped predecessor only when the accepted event captured one. It runs
//! inside the reserved transaction and never sees transport types.

use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};

use crate::new_id;
use crate::now_utc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PhaseInput {
    pub opportunity_id: String,
    pub expected_phase_revision: i64,
    pub expected_pack_id: String,
    pub expected_pack_version: String,
    pub action: PhaseAction,
    /// Digest of the bounded operation key, stored on the emitted event
    /// instead of the raw key (Q1).
    pub operation_key_digest: String,
    /// REST-only undo precondition: the caller's declared predecessor stage.
    /// The registry undo command derives its target from the stored event and
    /// leaves this unset. The REST adapter still publishes its historical
    /// `INVALID_UNDO_TARGET` status when the two disagree.
    pub expected_target_stage_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum PhaseAction {
    Set { target_stage_id: String },
    Undo { original_event_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct PhaseResult {
    pub opportunity_id: String,
    pub previous_stage_id: Option<String>,
    pub stage_id: Option<String>,
    pub phase_revision: i64,
    pub event_id: Option<String>,
    pub event_cursor: Option<i64>,
    pub reverted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PhaseError {
    NotFound,
    InvalidStage,
    TerminalStage,
    LifecycleBlocked,
    StaleRevision,
    PackMismatch,
    UndoInvalidReference,
    UndoNotLatest,
    UndoTargetMismatch,
}

/// Outcome of applying a phase input.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PhaseOutcome {
    /// A phase event was emitted and the opportunity advanced.
    Changed(PhaseResult),
    /// True no-op: current permitted state, zero domain events.
    NoOp(PhaseResult),
}

type PhaseStateRecord = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
);

pub(crate) async fn apply_phase(
    tx: &mut Transaction<'_, Sqlite>,
    input: &PhaseInput,
) -> Result<PhaseOutcome, PhaseError> {
    let row: Option<PhaseStateRecord> = sqlx::query_as(
        "SELECT state,stage_id,stage_pack_id,stage_pack_version,stage_class,phase_revision,last_phase_event_id FROM opportunity WHERE id=?",
    )
    .bind(&input.opportunity_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| PhaseError::NotFound)?;
    let Some((
        lifecycle,
        current_stage,
        _bound_pack,
        _bound_pack_version,
        current_class,
        phase_revision,
        last_phase_event_id,
    )) = row
    else {
        return Err(PhaseError::NotFound);
    };

    // The revision guard applies to every execution, including a no-op.
    if phase_revision != input.expected_phase_revision {
        return Err(PhaseError::StaleRevision);
    }

    let active_pack_id: String = sqlx::query_scalar("SELECT pack_id FROM owner_profile WHERE id=1")
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| PhaseError::NotFound)?;
    if input.expected_pack_id != active_pack_id {
        return Err(PhaseError::PackMismatch);
    }

    let from_stage_id = current_stage.clone();
    let from_class = current_class.clone();
    let mut reverted = false;
    let mut reversal_of: Option<String> = None;

    let (target_stage_id, target_class): (Option<String>, Option<String>) = match &input.action {
        PhaseAction::Set { target_stage_id } => {
            let stage: Option<(String, String, String, String)> = sqlx::query_as(
                    "SELECT stage_id,label,class,kind FROM pack_stage WHERE pack_id=? AND pack_version=? AND stage_id=?",
                )
                .bind(&input.expected_pack_id)
                .bind(&input.expected_pack_version)
                .bind(target_stage_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|_| PhaseError::NotFound)?;
            let Some((_id, _label, class, kind)) = stage else {
                return Err(PhaseError::InvalidStage);
            };
            if kind == "terminal" {
                return Err(PhaseError::TerminalStage);
            }
            (Some(target_stage_id.clone()), Some(class))
        }
        PhaseAction::Undo { original_event_id } => {
            reverted = true;
            reversal_of = Some(original_event_id.clone());
            if last_phase_event_id.as_deref() != Some(original_event_id.as_str()) {
                return Err(PhaseError::UndoNotLatest);
            }
            let event: Option<(String, String, String)> = sqlx::query_as(
                "SELECT entity_type,event_type,payload_json FROM event WHERE event_id=?",
            )
            .bind(original_event_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| PhaseError::NotFound)?;
            let Some((entity_type, event_type, payload_json)) = event else {
                return Err(PhaseError::UndoInvalidReference);
            };
            if entity_type != "opportunity" || event_type != "opportunity.phase_changed" {
                return Err(PhaseError::UndoInvalidReference);
            }
            let payload: serde_json::Value =
                serde_json::from_str(&payload_json).unwrap_or_default();
            if payload.get("operation").and_then(|v| v.as_str()) != Some("manual") {
                return Err(PhaseError::UndoInvalidReference);
            }
            let event_from = match payload.get("from_stage_id") {
                Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(value)) => Some(value.clone()),
                _ => return Err(PhaseError::UndoInvalidReference),
            };
            let event_to = payload.get("to_stage_id").and_then(|v| v.as_str());
            if event_to != current_stage.as_deref() {
                return Err(PhaseError::UndoNotLatest);
            }
            // The target is the recorded predecessor, including Unmapped.
            // Resolve its class so the projected row remains coherent.
            let predecessor_class: Option<String> = match &event_from {
                    Some(stage) => sqlx::query_scalar(
                        "SELECT class FROM pack_stage WHERE pack_id=? AND pack_version=? AND stage_id=?",
                    )
                    .bind(&input.expected_pack_id)
                    .bind(&input.expected_pack_version)
                    .bind(stage)
                    .fetch_optional(&mut **tx)
                    .await
                    .ok()
                    .flatten(),
                    None => None,
                };
            if input.expected_target_stage_id.as_deref().is_some()
                && input.expected_target_stage_id.as_deref() != event_from.as_deref()
            {
                return Err(PhaseError::UndoTargetMismatch);
            }
            // The undo transition goes *from* the current stage back to
            // the recorded predecessor; `from_stage_id`/`from_class`
            // therefore stay at the current state for the event payload.
            (event_from, predecessor_class)
        }
    };

    // True no-op (set path only): permitted state, zero events.
    if !reverted && current_stage.as_deref() == target_stage_id.as_deref() {
        return Ok(PhaseOutcome::NoOp(PhaseResult {
            opportunity_id: input.opportunity_id.clone(),
            previous_stage_id: current_stage,
            stage_id: target_stage_id,
            phase_revision,
            event_id: None,
            event_cursor: None,
            reverted,
        }));
    }

    if lifecycle != "open" {
        return Err(PhaseError::LifecycleBlocked);
    }

    let next_revision = phase_revision + 1;
    let event_payload = serde_json::json!({
        "from_stage_id": from_stage_id,
        "from_class": from_class,
        "to_stage_id": target_stage_id,
        "to_class": target_class,
        "pack_id": input.expected_pack_id,
        "pack_version": input.expected_pack_version,
        "operation": if reverted { "undo" } else { "manual" },
        "operation_key_digest": input.operation_key_digest,
        "reversal_of": reversal_of,
    });
    let event_id = new_id(&input.opportunity_id);
    let cursor: i64 = sqlx::query_scalar(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'opportunity',?,?,'opportunity.phase_changed',?) RETURNING cursor",
    )
    .bind(&event_id)
    .bind(now_utc())
    .bind(&input.opportunity_id)
    .bind(next_revision)
    .bind(event_payload.to_string())
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| PhaseError::NotFound)?;

    let updated = sqlx::query(
        "UPDATE opportunity SET stage_id=?,stage_pack_id=?,stage_pack_version=?,stage_class=?,phase_revision=?,last_phase_event_id=?,updated_at=? WHERE id=? AND phase_revision=?",
    )
    .bind(&target_stage_id)
    .bind(&input.expected_pack_id)
    .bind(&input.expected_pack_version)
    .bind(&target_class)
    .bind(next_revision)
    .bind(&event_id)
    .bind(now_utc())
    .bind(&input.opportunity_id)
    .bind(phase_revision)
    .execute(&mut **tx)
    .await
    .map_err(|_| PhaseError::NotFound)?;
    if updated.rows_affected() != 1 {
        return Err(PhaseError::StaleRevision);
    }

    Ok(PhaseOutcome::Changed(PhaseResult {
        opportunity_id: input.opportunity_id.clone(),
        previous_stage_id: from_stage_id,
        stage_id: target_stage_id,
        phase_revision: next_revision,
        event_id: Some(event_id),
        event_cursor: Some(cursor),
        reverted,
    }))
}
