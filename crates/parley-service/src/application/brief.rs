//! Typed scoped brief-job start (UCR-001 U05).
//!
//! `brief.start` is the one application operation behind the registry command
//! of the same name. It resolves the selected party or opportunity against
//! the caller's observed revision, collects bounded brief evidence, and
//! enqueues a `briefer` job bound to the initiating run and epoch inside the
//! caller's reserved transaction. The worker later persists exactly one
//! reviewable proposal set; this operation never accepts a proposal.
//!
//! This operation is actor-independent: it receives a checked actor context
//! (run binding, epoch, granted targets) from the command adapter and never
//! sees an HTTP type, a key or a transport envelope.

use sqlx::{Sqlite, Transaction};

use crate::commands::ActorTarget;
use crate::{new_id, now_utc};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BriefInput {
    /// `"party"` or `"opportunity"` — the selected target kind.
    pub target_kind: &'static str,
    pub target_id: String,
    /// Revision of the selected record as observed by the caller.
    pub expected_revision: i64,
    /// Bounded purpose the owner or agent set for the brief.
    pub purpose: String,
    pub epoch: String,
    /// Present for delegated runs; the job is fenced to this run.
    pub run_id: Option<String>,
    pub owner_pseudonym: Option<String>,
    /// Immutable granted targets of the run (concealed for owner callers).
    pub granted_targets: Vec<ActorTarget>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct BriefResult {
    pub job_id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub state: &'static str,
    pub event_cursor: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BriefError {
    NotFound,
    StaleRevision,
    InvalidPurpose,
    TargetNotGranted,
    QueueAtCapacity,
    Database,
}

/// The maximum purpose length is bounded by the descriptor schema and the
/// input collection here so a hostile caller cannot inflate a job input.
pub(crate) const MAX_PURPOSE_SCALARS: usize = 2000;

/// Start one scoped brief job. Every write, event and binding shares the
/// caller's transaction; a domain denial leaves no effect for the caller to
/// roll back at the savepoint.
pub(crate) async fn start(
    tx: &mut Transaction<'_, Sqlite>,
    input: &BriefInput,
) -> Result<BriefResult, BriefError> {
    let purpose = input.purpose.trim();
    if purpose.is_empty() || purpose.chars().count() > MAX_PURPOSE_SCALARS {
        return Err(BriefError::InvalidPurpose);
    }

    // Resolve the party scope: a direct party or the primary party of the
    // selected opportunity. An opportunity is eligible only when it has a
    // primary party; the brief itself is scoped to that party so the owner
    // reviews it in the existing Brief workspace.
    let (scope_kind, scope_id, scope_revision, display_name) =
        match input.target_kind {
            "party" => {
                let row: Option<(i64, String)> = sqlx::query_as(
                    "SELECT revision,name FROM entity WHERE id=? AND kind IN ('person','org')",
                )
                .bind(&input.target_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|_| BriefError::Database)?;
                let Some((revision, name)) = row else {
                    return Err(BriefError::NotFound);
                };
                if revision != input.expected_revision {
                    return Err(BriefError::StaleRevision);
                }
                ("party", input.target_id.clone(), revision, name)
            }
            "opportunity" => {
                let row: Option<(i64, String)> = sqlx::query_as(
                    "SELECT revision,state FROM opportunity WHERE id=?",
                )
                .bind(&input.target_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|_| BriefError::Database)?;
                let Some((revision, state)) = row else {
                    return Err(BriefError::NotFound);
                };
                if revision != input.expected_revision {
                    return Err(BriefError::StaleRevision);
                }
                if state != "open" {
                    return Err(BriefError::NotFound);
                }
                let party_id: Option<String> = sqlx::query_scalar(
                    "SELECT party_id FROM opportunity_party WHERE opportunity_id=? AND role='primary'",
                )
                .bind(&input.target_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|_| BriefError::Database)?
                .flatten();
                let Some(party_id) = party_id else {
                    return Err(BriefError::NotFound);
                };
                let party: Option<(i64, String)> = sqlx::query_as(
                    "SELECT revision,name FROM entity WHERE id=? AND kind IN ('person','org')",
                )
                .bind(&party_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|_| BriefError::Database)?;
                let Some((party_revision, name)) = party else {
                    return Err(BriefError::NotFound);
                };
                ("party", party_id, party_revision, name)
            }
            _ => return Err(BriefError::NotFound),
        };

    // A delegated run may only collect evidence about a party inside its
    // immutable grant. Tool args cannot widen scope; an opportunity is
    // eligible only when its primary party is granted as well.
    if input.run_id.is_some()
        && !input.granted_targets.iter().any(|target| {
            target.kind == scope_kind && target.id == scope_id
        })
    {
        return Err(BriefError::TargetNotGranted);
    }

    let evidence = collect_brief_evidence(tx, scope_kind, &scope_id).await?;
    let prompt_version_id: Option<String> =
        sqlx::query_scalar("SELECT version_id FROM agent_prompt_active WHERE role='briefer'")
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| BriefError::Database)?
            .flatten();
    let input_json = serde_json::json!({
        "scope_kind": scope_kind,
        "scope_id": scope_id,
        "scope_revision": scope_revision,
        "display_name": display_name,
        "evidence": evidence,
        "prompt_version_id": prompt_version_id,
        "purpose": purpose,
    });
    let job_id = crate::jobs::enqueue_role_in_tx(
        tx,
        "briefer",
        scope_kind,
        &scope_id,
        scope_revision,
        &input_json,
        3,
        &input.epoch,
        input.run_id.as_deref(),
        input.owner_pseudonym.as_deref(),
    )
    .await
    .map_err(|error| match error {
        crate::WorkspaceError::Invalid(message) if message == "agent queue is at capacity" => {
            BriefError::QueueAtCapacity
        }
        crate::WorkspaceError::Invalid(_) => BriefError::InvalidPurpose,
        _ => BriefError::Database,
    })?;

    let cursor = insert_event(
        tx,
        &scope_id,
        "brief.generation_queued",
        &serde_json::json!({
            "job_id": job_id,
            "scope_kind": scope_kind,
            "scope_id": scope_id,
            "run_id": input.run_id,
        }),
    )
    .await?;

    Ok(BriefResult {
        job_id,
        scope_kind: scope_kind.to_string(),
        scope_id: scope_id.clone(),
        state: "queued",
        event_cursor: cursor.to_string(),
    })
}

/// The bounded evidence vector for a party (or a segment composed of granted
/// parties). Only saved sources the party is actor of, or that back an
/// existing claim about it, are eligible; nothing else is collectable.
pub(crate) async fn collect_brief_evidence(
    tx: &mut Transaction<'_, Sqlite>,
    scope_kind: &str,
    scope_id: &str,
) -> Result<Vec<serde_json::Value>, BriefError> {
    let rows: Vec<(String, String, String, Option<String>)> =
        if scope_kind == "party" {
            sqlx::query_as(
                "SELECT DISTINCT so.id,so.source_variant_id,sv.raw_text,sv.url FROM source_occurrence so JOIN source_variant sv ON sv.id=so.source_variant_id WHERE so.actor_entity_id=? OR EXISTS(SELECT 1 FROM claim_revision cr JOIN claim c ON c.id=cr.claim_id WHERE cr.occurrence_id=so.id AND c.subject_id=?) ORDER BY so.captured_at,so.id",
            )
            .bind(scope_id)
            .bind(scope_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(|_| BriefError::Database)?
        } else {
            sqlx::query_as(
                "SELECT DISTINCT so.id,so.source_variant_id,sv.raw_text,sv.url FROM segment_party sp JOIN source_occurrence so ON so.actor_entity_id=sp.party_id JOIN source_variant sv ON sv.id=so.source_variant_id WHERE sp.segment_id=? UNION SELECT DISTINCT so.id,so.source_variant_id,sv.raw_text,sv.url FROM segment_party sp JOIN claim c ON c.subject_id=sp.party_id JOIN claim_revision cr ON cr.claim_id=c.id JOIN source_occurrence so ON so.id=cr.occurrence_id JOIN source_variant sv ON sv.id=so.source_variant_id WHERE sp.segment_id=? ORDER BY 1",
            )
            .bind(scope_id)
            .bind(scope_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(|_| BriefError::Database)?
        };
    Ok(rows
        .into_iter()
        .map(|(occurrence_id, variant_id, text, url)| {
            serde_json::json!({
                "occurrence_id": occurrence_id,
                "variant_id": variant_id,
                "text": text,
                "url": url
            })
        })
        .collect())
}

async fn insert_event(
    tx: &mut Transaction<'_, Sqlite>,
    entity_id: &str,
    event_type: &str,
    payload: &serde_json::Value,
) -> Result<i64, BriefError> {
    sqlx::query_scalar(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'brief',?,1,?,?) RETURNING cursor",
    )
    .bind(new_id(entity_id))
    .bind(now_utc())
    .bind(entity_id)
    .bind(event_type)
    .bind(payload.to_string())
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| BriefError::Database)
}