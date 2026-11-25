//! Typed scoped match-job start (UCR-001 U07).
//!
//! `match.start` is the one application operation behind the registry command
//! of the same name. It resolves the selected open opportunity and recipient
//! against the caller's observed revisions, collects the bounded eligible
//! resource snapshot (filtered to the run's immutable grant for delegated
//! runs), and enqueues a `matcher` job bound to the initiating run and epoch
//! inside the caller's reserved transaction. The worker later persists a
//! server-computed shortlist; this operation never accepts model output.
//!
//! The owner REST route keeps its wire DTO and receipt semantics; it now
//! shares this operation's scope resolution and snapshot code with the
//! registry command so a release can change one without changing the other.

use sqlx::{Sqlite, Transaction};

use crate::commands::ActorTarget;
use crate::{new_id, now_utc};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MatchStartInput {
    pub opportunity_id: String,
    pub recipient_id: String,
    pub expected_opportunity_revision: i64,
    pub expected_recipient_revision: i64,
    pub epoch: String,
    /// Present for delegated runs; the job is fenced to this run.
    pub run_id: Option<String>,
    pub owner_pseudonym: Option<String>,
    /// Immutable granted targets of the run (empty for owner callers).
    pub granted_targets: Vec<ActorTarget>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct MatchStartResult {
    pub job_id: String,
    pub role: &'static str,
    pub scope_kind: String,
    pub scope_id: String,
    pub state: &'static str,
    pub event_cursor: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MatchStartError {
    NotFound,
    StaleRevision,
    RecipientInvalid,
    NeedMissing,
    TargetNotGranted,
    QueueAtCapacity,
    ScopeTooLarge,
    Database,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct ReadyResourceSnapshot {
    pub resource_id: String,
    pub kind: String,
    pub title: String,
    pub revision: i64,
    pub age_months: f64,
    pub body: serde_json::Value,
    pub tags: Vec<String>,
}

/// The typed ready-resource snapshot shared by the owner REST adapter and the
/// registry command. `granted_resource_ids` is empty for owner callers; a
/// delegated run sees only the resources enumerated in its grant (U07).
pub(crate) async fn ready_resource_snapshot(
    tx: &mut Transaction<'_, Sqlite>,
    granted_resource_ids: &[String],
) -> Result<Vec<ReadyResourceSnapshot>, MatchStartError> {
    let rows: Vec<(String, String, String, String, i64, f64)> = sqlx::query_as(
        "SELECT id,subtype,name,body_json,resource_content_revision,max(0.0,(julianday('now')-julianday(created_at))/30.4375) FROM entity WHERE subtype IN ('gift','proof','artifact') AND readiness='ready' AND retired_at IS NULL ORDER BY id LIMIT 4097",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| MatchStartError::Database)?;
    if rows.len() > 4096 {
        return Err(MatchStartError::ScopeTooLarge);
    }
    let mut snapshots = Vec::with_capacity(rows.len());
    for (id, kind, title, body_json, revision, age_months) in rows {
        if !granted_resource_ids.is_empty() && !granted_resource_ids.iter().any(|item| item == &id)
        {
            continue;
        }
        let body: serde_json::Value =
            serde_json::from_str(&body_json).map_err(|_| MatchStartError::Database)?;
        let tags = body
            .get("tags")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect();
        snapshots.push(ReadyResourceSnapshot {
            resource_id: id,
            kind,
            title,
            revision,
            age_months,
            body,
            tags,
        });
    }
    Ok(snapshots)
}

/// Start one scoped matcher job. Every write, event and binding shares the
/// caller's transaction; a domain denial leaves no effect for the caller to
/// roll back at the savepoint.
pub(crate) async fn start(
    tx: &mut Transaction<'_, Sqlite>,
    input: &MatchStartInput,
) -> Result<MatchStartResult, MatchStartError> {
    let opportunity: Option<(i64, String)> =
        sqlx::query_as("SELECT revision,state FROM opportunity WHERE id=?")
            .bind(&input.opportunity_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| MatchStartError::Database)?;
    let Some((opportunity_revision, state)) = opportunity else {
        return Err(MatchStartError::NotFound);
    };
    if input.expected_opportunity_revision != opportunity_revision {
        return Err(MatchStartError::StaleRevision);
    }
    if state != "open" {
        return Err(MatchStartError::NotFound);
    }

    let participant: Option<(String, i64)> = sqlx::query_as(
        "SELECT e.kind,e.revision FROM opportunity_party p JOIN entity e ON e.id=p.party_id WHERE p.opportunity_id=? AND p.party_id=?",
    )
    .bind(&input.opportunity_id)
    .bind(&input.recipient_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| MatchStartError::Database)?;
    let Some((kind, recipient_revision)) = participant else {
        return Err(MatchStartError::RecipientInvalid);
    };
    if !matches!(kind.as_str(), "person" | "org") {
        return Err(MatchStartError::RecipientInvalid);
    }
    if input.expected_recipient_revision != recipient_revision {
        return Err(MatchStartError::StaleRevision);
    }

    // A delegated run may only scope a match to granted opportunity and
    // recipient records. Tool args cannot widen scope (FR-3).
    if input.run_id.is_some()
        && !input
            .granted_targets
            .iter()
            .any(|target| target.kind == "opportunity" && target.id == input.opportunity_id)
    {
        return Err(MatchStartError::TargetNotGranted);
    }
    if input.run_id.is_some()
        && !input
            .granted_targets
            .iter()
            .any(|target| target.kind == "party" && target.id == input.recipient_id)
    {
        return Err(MatchStartError::TargetNotGranted);
    }

    let interests: Vec<(String, String)> = sqlx::query_as(
        "SELECT side,statement FROM opportunity_interest WHERE opportunity_id=? AND party_id=? AND status='active' ORDER BY created_at,id",
    )
    .bind(&input.opportunity_id)
    .bind(&input.recipient_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| MatchStartError::Database)?;
    let need_text = interests
        .iter()
        .filter(|(side, _)| side == "them")
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if need_text.trim().is_empty() {
        return Err(MatchStartError::NeedMissing);
    }

    let granted_resource_ids: Vec<String> = input
        .granted_targets
        .iter()
        .filter(|target| target.kind == "resource")
        .map(|target| target.id.clone())
        .collect();
    let resources = ready_resource_snapshot(tx, &granted_resource_ids).await?;
    let resource_inputs: Vec<serde_json::Value> = resources
        .into_iter()
        .map(|snapshot| {
            serde_json::json!({
                "resource_id": snapshot.resource_id,
                "kind": snapshot.kind,
                "title": snapshot.title,
                "revision": snapshot.revision,
                "age_months": snapshot.age_months,
                "body": snapshot.body,
                "tags": snapshot.tags,
                "advances": 0,
                "sends_using_asset": 0,
            })
        })
        .collect();

    let settings_json: String =
        sqlx::query_scalar("SELECT settings_json FROM owner_profile WHERE id=1")
            .fetch_one(&mut **tx)
            .await
            .map_err(|_| MatchStartError::Database)?;
    let settings: serde_json::Value =
        serde_json::from_str(&settings_json).map_err(|_| MatchStartError::Database)?;
    let proof_half_life_months = settings
        .pointer("/pack/limits/proof_half_life_months")
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(24.0);
    let weights = settings
        .pointer("/matcher/weights")
        .cloned()
        .unwrap_or_else(
            || serde_json::json!({"cosine":0.5,"tags":0.25,"recency":0.15,"track":0.1}),
        );
    let scoring_bytes = serde_json::to_vec(&serde_json::json!({
        "weights": weights,
        "proof_half_life_months": proof_half_life_months,
        "scoring_revision": 1,
    }))
    .map_err(|_| MatchStartError::Database)?;
    let scoring_config_digest = crate::digest(&scoring_bytes);

    let input_json = serde_json::json!({
        "opportunity_id": input.opportunity_id,
        "recipient_id": input.recipient_id,
        "opportunity_revision": opportunity_revision,
        "recipient_revision": recipient_revision,
        "need_text": need_text,
        "party_fields": {"need_text": need_text},
        "party_tags": [],
        "resources": resource_inputs,
        "proof_half_life_months": proof_half_life_months,
        "weights": weights,
        "scoring_config_digest": scoring_config_digest,
        "requested_at": now_utc(),
    });
    let job_id = crate::jobs::enqueue_role_in_tx(
        tx,
        "matcher",
        "opportunity",
        &input.opportunity_id,
        opportunity_revision,
        &input_json,
        2,
        &input.epoch,
        input.run_id.as_deref(),
        input.owner_pseudonym.as_deref(),
    )
    .await
    .map_err(|error| match error {
        crate::WorkspaceError::Invalid(message) if message == "agent queue is at capacity" => {
            MatchStartError::QueueAtCapacity
        }
        crate::WorkspaceError::Invalid(_) => MatchStartError::RecipientInvalid,
        _ => MatchStartError::Database,
    })?;
    // The matcher runs through the packaged worker and requires the profile
    // revision that will serve it. Without an active qualified profile the
    // job is admitted unbound and fails closed as unqualified at dispatch.
    let profile_revision: Option<i64> = sqlx::query_scalar(
        "SELECT a.profile_revision FROM inference_active_profile a
         WHERE a.singleton=1
           AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r
                          WHERE r.profile_revision=a.profile_revision)
         ORDER BY a.profile_revision DESC LIMIT 1",
    )
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| MatchStartError::Database)?
    .flatten();
    // The matcher runs through the packaged worker under fixed queue and
    // total deadlines (AR-001 NFR-2), independent of other role admissions.
    sqlx::query(
        "UPDATE durable_job SET profile_revision=?,queue_deadline_at=?,total_deadline_at=? WHERE id=?",
    )
    .bind(profile_revision)
    .bind(crate::now_utc_offset(300))
    .bind(crate::now_utc_offset(360))
    .bind(&job_id)
    .execute(&mut **tx)
    .await
    .map_err(|_| MatchStartError::Database)?;

    let cursor: i64 = sqlx::query_scalar(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'opportunity',?,1,'match.generation_queued',?) RETURNING cursor",
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
    .map_err(|_| MatchStartError::Database)?;

    Ok(MatchStartResult {
        job_id,
        role: "matcher",
        scope_kind: "opportunity".into(),
        scope_id: input.opportunity_id.clone(),
        state: "queued",
        event_cursor: cursor.to_string(),
    })
}
