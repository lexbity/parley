//! Typed scoped compose-job start (UCR-001 U07).
//!
//! `material.compose_draft` sits behind the registry command of the same
//! name. It resolves the selected open opportunity, recipient, active
//! template version and attested exchange, validates the owner-selected slot
//! fills against the deterministic template and the server-computed match
//! shortlist, and enqueues a `composer` job bound to the initiating run and
//! epoch inside the caller's reserved transaction. The worker persists a
//! review candidate only; this operation never approves or releases anything.
//!
//! The owner REST route keeps its wire DTO and receipt semantics and shares
//! this operation's scope and validation code with the registry command.

use std::collections::{BTreeMap, BTreeSet};

use sqlx::{Sqlite, Transaction};

use crate::commands::ActorTarget;
use crate::{new_id, now_utc, now_utc_offset};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SlotFill {
    pub slot: String,
    pub value: String,
    #[serde(default)]
    pub resource_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ComposeStartInput {
    pub opportunity_id: String,
    pub recipient_id: String,
    pub expected_opportunity_revision: i64,
    pub expected_recipient_revision: i64,
    pub kind: String,
    pub template_version_id: String,
    pub exchange_version: i64,
    pub slot_fills: Vec<SlotFill>,
    pub selected_match_ids: Vec<String>,
    pub epoch: String,
    /// Present for delegated runs; the job is fenced to this run.
    pub run_id: Option<String>,
    pub owner_pseudonym: Option<String>,
    /// Immutable granted targets of the run (empty for owner callers).
    pub granted_targets: Vec<ActorTarget>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct ComposeStartResult {
    pub job_id: String,
    pub role: &'static str,
    pub scope_kind: String,
    pub scope_id: String,
    pub state: &'static str,
    pub event_cursor: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ComposeStartError {
    StaleRevision,
    RecipientInvalid,
    TemplateInvalid,
    ExchangeMissing,
    ExchangeNotReviewed,
    SlotInvalid,
    ResourceInvalid,
    MatchSelectionInvalid,
    MatchShortlistMissing,
    TargetNotGranted,
    QueueAtCapacity,
    Database,
}

/// Start one scoped composer job. Every write, event and binding shares the
/// caller's transaction; a domain denial leaves no effect for the caller to
/// roll back at the savepoint.
pub(crate) async fn start(
    tx: &mut Transaction<'_, Sqlite>,
    input: &ComposeStartInput,
) -> Result<ComposeStartResult, ComposeStartError> {
    if !matches!(input.kind.as_str(), "outreach" | "followup" | "handoff")
        || input.exchange_version <= 0
    {
        return Err(ComposeStartError::RecipientInvalid);
    }

    let context: Option<(i64, i64, String, String)> = sqlx::query_as(
        "SELECT o.revision,e.revision,o.state,e.kind FROM opportunity o JOIN opportunity_party p ON p.opportunity_id=o.id JOIN entity e ON e.id=p.party_id WHERE o.id=? AND p.party_id=?",
    )
    .bind(&input.opportunity_id)
    .bind(&input.recipient_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| ComposeStartError::Database)?;
    let Some((opp_revision, recipient_revision, state, kind)) = context else {
        return Err(ComposeStartError::RecipientInvalid);
    };
    if input.expected_opportunity_revision != opp_revision {
        return Err(ComposeStartError::StaleRevision);
    }
    if input.expected_recipient_revision != recipient_revision {
        return Err(ComposeStartError::StaleRevision);
    }
    if state != "open" || !matches!(kind.as_str(), "person" | "org") {
        return Err(ComposeStartError::RecipientInvalid);
    }

    if input.run_id.is_some()
        && !input
            .granted_targets
            .iter()
            .any(|target| target.kind == "opportunity" && target.id == input.opportunity_id)
    {
        return Err(ComposeStartError::TargetNotGranted);
    }
    if input.run_id.is_some()
        && !input
            .granted_targets
            .iter()
            .any(|target| target.kind == "party" && target.id == input.recipient_id)
    {
        return Err(ComposeStartError::TargetNotGranted);
    }

    let active: Option<String> =
        sqlx::query_scalar("SELECT id FROM template_version WHERE id=? AND status='active'")
            .bind(&input.template_version_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| ComposeStartError::Database)?;
    if active.is_none() {
        return Err(ComposeStartError::TemplateInvalid);
    }

    let exchange: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT they_get,i_get,fairness_attested FROM exchange_version WHERE opportunity_id=? AND recipient_id=? AND version_no=?",
    )
    .bind(&input.opportunity_id)
    .bind(&input.recipient_id)
    .bind(input.exchange_version)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| ComposeStartError::Database)?;
    let Some((they_get, i_get, attested)) = exchange else {
        return Err(ComposeStartError::ExchangeMissing);
    };
    if attested != 1 || they_get.trim().is_empty() || i_get.trim().is_empty() {
        return Err(ComposeStartError::ExchangeNotReviewed);
    }

    let slots: Option<(String, String)> = sqlx::query_as(
        "SELECT body,fallbacks_json FROM template_version WHERE id=? AND status='active'",
    )
    .bind(&input.template_version_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| ComposeStartError::Database)?;
    let Some((body, fallbacks_json)) = slots else {
        return Err(ComposeStartError::TemplateInvalid);
    };
    let fallback: serde_json::Value =
        serde_json::from_str(&fallbacks_json).unwrap_or(serde_json::Value::Null);
    let declared = parley_domain::template::validate(
        &body,
        fallback.get("proof").and_then(serde_json::Value::as_str),
    );
    if !declared.errors.is_empty() {
        return Err(ComposeStartError::TemplateInvalid);
    }
    let declared_slots = declared.slots.into_iter().collect::<BTreeSet<_>>();
    let provided = input
        .slot_fills
        .iter()
        .map(|fill| fill.slot.clone())
        .collect::<BTreeSet<_>>();
    if declared_slots != provided
        || input
            .slot_fills
            .iter()
            .any(|fill| fill.value.trim().is_empty())
    {
        return Err(ComposeStartError::SlotInvalid);
    }

    let mut resource_revisions = BTreeMap::new();
    for fill in &input.slot_fills {
        if let Some(resource_id) = fill.resource_id.as_deref() {
            let ready: Option<(String, i64)> = sqlx::query_as(
                "SELECT subtype,resource_content_revision FROM entity WHERE id=? AND readiness='ready' AND retired_at IS NULL",
            )
            .bind(resource_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| ComposeStartError::Database)?;
            let Some((resource_kind, revision)) = ready else {
                return Err(ComposeStartError::ResourceInvalid);
            };
            let slot_kind = fill.slot.split_once(':').map(|value| value.0).unwrap_or("");
            if slot_kind != resource_kind {
                return Err(ComposeStartError::ResourceInvalid);
            }
            // A delegated run may fill slots only with resources inside its
            // immutable grant (U07). Tool args cannot widen scope.
            if input.run_id.is_some()
                && !input
                    .granted_targets
                    .iter()
                    .any(|target| target.kind == "resource" && target.id == resource_id)
            {
                return Err(ComposeStartError::TargetNotGranted);
            }
            resource_revisions.insert(resource_id.to_owned(), revision);
        }
    }

    let selected = input.selected_match_ids.iter().collect::<BTreeSet<_>>();
    if selected.len() != input.selected_match_ids.len() {
        return Err(ComposeStartError::MatchSelectionInvalid);
    }
    for resource_id in &input.selected_match_ids {
        if !resource_revisions.contains_key(resource_id) {
            return Err(ComposeStartError::MatchSelectionInvalid);
        }
    }
    if !input.selected_match_ids.is_empty() {
        let latest: Option<String> = sqlx::query_scalar(
            "SELECT shortlist_json FROM match_run WHERE opportunity_id=? AND recipient_id=? ORDER BY created_at DESC,id DESC LIMIT 1",
        )
        .bind(&input.opportunity_id)
        .bind(&input.recipient_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| ComposeStartError::Database)?;
        let Some(latest) = latest else {
            return Err(ComposeStartError::MatchShortlistMissing);
        };
        let shortlist: serde_json::Value =
            serde_json::from_str(&latest).map_err(|_| ComposeStartError::Database)?;
        let allowed = shortlist
            .get("results")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|kind| {
                kind.get("candidates")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter_map(|candidate| candidate.get("id").and_then(serde_json::Value::as_str))
            .collect::<BTreeSet<_>>();
        if input
            .selected_match_ids
            .iter()
            .any(|id| !allowed.contains(id.as_str()))
        {
            return Err(ComposeStartError::MatchSelectionInvalid);
        }
    }

    let input_json = serde_json::json!({
        "opportunity_id": input.opportunity_id,
        "opportunity_revision": opp_revision,
        "recipient_id": input.recipient_id,
        "recipient_revision": recipient_revision,
        "kind": input.kind,
        "template_version_id": input.template_version_id,
        "exchange_version": input.exchange_version,
        "slot_fills": input.slot_fills,
        "selected_match_ids": input.selected_match_ids,
        "resource_revisions": resource_revisions,
        "prompt_version": "composer@1",
        "prompt": crate::jobs::bundled_role_prompt("composer").unwrap_or_default(),
    });
    let job_id = crate::jobs::enqueue_role_in_tx(
        tx,
        "composer",
        "opportunity",
        &input.opportunity_id,
        opp_revision,
        &input_json,
        3,
        &input.epoch,
        input.run_id.as_deref(),
        input.owner_pseudonym.as_deref(),
    )
    .await
    .map_err(|error| match error {
        crate::WorkspaceError::Invalid(message) if message == "agent queue is at capacity" => {
            ComposeStartError::QueueAtCapacity
        }
        crate::WorkspaceError::Invalid(_) => ComposeStartError::RecipientInvalid,
        _ => ComposeStartError::Database,
    })?;
    let profile_revision: Option<i64> = sqlx::query_scalar(
        "SELECT a.profile_revision FROM inference_active_profile a
         JOIN inference_qualification q ON q.profile_revision=a.profile_revision AND q.mode='structured_role'
         WHERE NOT EXISTS(SELECT 1 FROM inference_profile_revocation r
                         WHERE r.profile_revision=a.profile_revision)
         ORDER BY a.profile_revision DESC LIMIT 1",
    )
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| ComposeStartError::Database)?;
    if let Some(profile_revision) = profile_revision {
        let dependency_digest = crate::digest(input_json.to_string().as_bytes());
        let now = now_utc();
        sqlx::query(
            "UPDATE durable_job SET profile_revision=?,dependency_digest=?,eligible_at=?,queue_deadline_at=?,total_deadline_at=? WHERE id=?",
        )
        .bind(profile_revision)
        .bind(&dependency_digest)
        .bind(&now)
        .bind(now_utc_offset(300))
        .bind(now_utc_offset(360))
        .bind(&job_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| ComposeStartError::Database)?;
    }

    let cursor: i64 = sqlx::query_scalar(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'opportunity',?,1,'material.compose_generation_queued',?) RETURNING cursor",
    )
    .bind(new_id(&input.opportunity_id))
    .bind(now_utc())
    .bind(&input.opportunity_id)
    .bind(
        serde_json::json!({
            "job_id": job_id,
            "recipient_id": input.recipient_id,
            "run_id": input.run_id,
        })
        .to_string(),
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| ComposeStartError::Database)?;

    Ok(ComposeStartResult {
        job_id,
        role: "composer",
        scope_kind: "opportunity".into(),
        scope_id: input.opportunity_id.clone(),
        state: "queued",
        event_cursor: cursor.to_string(),
    })
}
