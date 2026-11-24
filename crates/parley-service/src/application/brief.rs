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
use crate::{WorkspaceError, digest, new_id, now_utc, now_utc_offset};

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

/// The two owner-launched Briefer starts share one intake operation: a bare
/// scope generation or a scoped research-chat turn (AR-001 AR04).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OwnerBriefStart {
    Generate {
        scope_kind: String,
        scope_id: String,
    },
    ChatTurn {
        chat_id: String,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct OwnerBriefStartResult {
    pub job_id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub state: &'static str,
    pub event_cursor: String,
    pub chat_id: Option<String>,
    pub turn_id: Option<String>,
    pub revision: Option<i64>,
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
    let (scope_kind, scope_id, scope_revision, display_name) = match input.target_kind {
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
            let row: Option<(i64, String)> =
                sqlx::query_as("SELECT revision,state FROM opportunity WHERE id=?")
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
        && !input
            .granted_targets
            .iter()
            .any(|target| target.kind == scope_kind && target.id == scope_id)
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
    let job_id = enqueue_briefer_job(
        tx,
        &input.epoch,
        input.run_id.as_deref(),
        input.owner_pseudonym.as_deref(),
        scope_kind,
        &scope_id,
        scope_revision,
        &input_json,
        prompt_version_id.as_deref().unwrap_or("briefer@1"),
        None,
    )
    .await
    .map_err(|error| match error {
        WorkspaceError::Invalid(message) if message == "agent queue is at capacity" => {
            BriefError::QueueAtCapacity
        }
        WorkspaceError::Invalid(_) => BriefError::InvalidPurpose,
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

/// Start one owner-launched Briefer job. `Generate` produces a bare scoped
/// brief; `ChatTurn` records an owner turn in a scoped research chat and
/// asks the Briefer to answer from the same evidence. Both bind the reviewed
/// prompt version, the qualified profile and a canonical dependency vector at
/// admission, and both share the delegated `start` enqueue path.
pub(crate) async fn start_owner(
    tx: &mut Transaction<'_, Sqlite>,
    epoch: &str,
    action: &OwnerBriefStart,
) -> Result<OwnerBriefStartResult, BriefError> {
    match action {
        OwnerBriefStart::Generate {
            scope_kind,
            scope_id,
        } => {
            let target_kind: &'static str = match scope_kind.as_str() {
                "party" => "party",
                "segment" => "segment",
                _ => return Err(BriefError::NotFound),
            };
            let input = BriefInput {
                target_kind,
                target_id: scope_id.clone(),
                expected_revision: resolve_scope_revision(tx, scope_kind, scope_id).await?,
                purpose: "Owner requested a scoped briefing".into(),
                epoch: epoch.to_owned(),
                run_id: None,
                owner_pseudonym: None,
                granted_targets: Vec::new(),
            };
            let result = start(tx, &input).await?;
            Ok(OwnerBriefStartResult {
                job_id: result.job_id,
                scope_kind: result.scope_kind,
                scope_id: result.scope_id,
                state: result.state,
                event_cursor: result.event_cursor,
                chat_id: None,
                turn_id: None,
                revision: None,
            })
        }
        OwnerBriefStart::ChatTurn { chat_id, text } => {
            let text = text.trim();
            if text.is_empty() || text.len() > crate::application::interview::MAX_TURN_BYTES {
                return Err(BriefError::InvalidPurpose);
            }
            let chat: Option<(String, String, Option<String>, i64)> = sqlx::query_as(
                "SELECT role,scope_kind,scope_id,revision FROM agent_chat WHERE id=?",
            )
            .bind(chat_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| BriefError::Database)?;
            let Some((role, scope_kind, Some(scope_id), revision)) = chat else {
                return Err(BriefError::NotFound);
            };
            if role != "briefer" {
                return Err(BriefError::NotFound);
            }
            if !matches!(scope_kind.as_str(), "party" | "segment") {
                return Err(BriefError::NotFound);
            }
            let scope_revision = resolve_scope_revision(tx, &scope_kind, &scope_id).await?;
            let now = now_utc();
            let turn_no: i64 = sqlx::query_scalar(
                "SELECT coalesce(max(turn_no),0)+1 FROM agent_turn WHERE chat_id=?",
            )
            .bind(chat_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(|_| BriefError::Database)?;
            let turn_id = new_id(&format!("{chat_id}:{turn_no}"));
            sqlx::query(
                "INSERT INTO agent_turn(id,chat_id,turn_no,speaker,text,created_at) VALUES(?,?,?,'owner',?,?)",
            )
            .bind(&turn_id)
            .bind(chat_id)
            .bind(turn_no)
            .bind(text)
            .bind(&now)
            .execute(&mut **tx)
            .await
            .map_err(|_| BriefError::Database)?;
            let next_revision = revision + 1;
            let changed = sqlx::query(
                "UPDATE agent_chat SET revision=?,updated_at=? WHERE id=? AND revision=?",
            )
            .bind(next_revision)
            .bind(&now)
            .bind(chat_id)
            .bind(revision)
            .execute(&mut **tx)
            .await
            .map_err(|_| BriefError::Database)?
            .rows_affected();
            if changed != 1 {
                return Err(BriefError::StaleRevision);
            }
            let turns: Vec<serde_json::Value> = sqlx::query_as::<_, (String, String, String)>(
                "SELECT id,speaker,text FROM agent_turn WHERE chat_id=? ORDER BY turn_no",
            )
            .bind(chat_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(|_| BriefError::Database)?
            .into_iter()
            .map(|(id, speaker, text)| serde_json::json!({"id":id,"speaker":speaker,"text":text}))
            .collect();
            let evidence = collect_brief_evidence(tx, &scope_kind, &scope_id).await?;
            let prompt_version_id = active_prompt_version(tx).await?;
            let prompt_version_id = match prompt_version_id {
                Some(version_id) => version_id,
                None => "briefer@1".to_owned(),
            };
            let input_json = serde_json::json!({
                "chat_id": chat_id,
                "scope_kind": scope_kind,
                "scope_id": scope_id,
                "scope_revision": scope_revision,
                "chat_revision": next_revision,
                "turns": turns,
                "evidence": evidence,
                "prompt_version_id": prompt_version_id,
            });
            let job_id = enqueue_briefer_job(
                tx,
                epoch,
                None,
                None,
                &scope_kind,
                &scope_id,
                scope_revision,
                &input_json,
                &prompt_version_id,
                Some(chat_id),
            )
            .await
            .map_err(|error| match error {
                WorkspaceError::Invalid(message) if message == "agent queue is at capacity" => {
                    BriefError::QueueAtCapacity
                }
                WorkspaceError::Invalid(_) => BriefError::InvalidPurpose,
                _ => BriefError::Database,
            })?;
            let cursor = insert_event(
                tx,
                &scope_id,
                "briefer.owner_turn",
                &serde_json::json!({
                    "job_id": job_id,
                    "chat_id": chat_id,
                    "turn_id": turn_id,
                    "scope_kind": scope_kind,
                    "scope_id": scope_id,
                }),
            )
            .await?;
            Ok(OwnerBriefStartResult {
                job_id,
                scope_kind: scope_kind.clone(),
                scope_id: scope_id.clone(),
                state: "queued",
                event_cursor: cursor.to_string(),
                chat_id: Some(chat_id.clone()),
                turn_id: Some(turn_id),
                revision: Some(next_revision),
            })
        }
    }
}

async fn resolve_scope_revision(
    tx: &mut Transaction<'_, Sqlite>,
    scope_kind: &str,
    scope_id: &str,
) -> Result<i64, BriefError> {
    let revision: Option<i64> = if scope_kind == "party" {
        sqlx::query_scalar("SELECT revision FROM entity WHERE id=? AND kind IN ('person','org')")
            .bind(scope_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| BriefError::Database)?
    } else {
        sqlx::query_scalar("SELECT revision FROM segment WHERE id=?")
            .bind(scope_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| BriefError::Database)?
    };
    revision.ok_or(BriefError::NotFound)
}

async fn active_prompt_version(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<Option<String>, BriefError> {
    sqlx::query_scalar("SELECT version_id FROM agent_prompt_active WHERE role='briefer'")
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| BriefError::Database)
}

/// Shared Briefer enqueue with admission-time profile and prompt binding. The
/// profile binding is applied when a qualified structured profile is active;
/// otherwise the job is admitted unbound and fails closed at dispatch with a
/// typed capability error. `chat_id` binds the job to its research chat.
#[allow(clippy::too_many_arguments)]
async fn enqueue_briefer_job(
    tx: &mut Transaction<'_, Sqlite>,
    epoch: &str,
    run_id: Option<&str>,
    owner_pseudonym: Option<&str>,
    scope_kind: &str,
    scope_id: &str,
    scope_revision: i64,
    input_json: &serde_json::Value,
    prompt_version_id: &str,
    chat_id: Option<&str>,
) -> Result<String, WorkspaceError> {
    let job_id = crate::jobs::enqueue_role_in_tx(
        tx,
        "briefer",
        scope_kind,
        scope_id,
        scope_revision,
        input_json,
        3,
        epoch,
        run_id,
        owner_pseudonym,
    )
    .await?;
    let now = now_utc();
    let dependency_digest = digest(input_json.to_string().as_bytes());
    let profile: Option<i64> = crate::application::interview::active_profile_revision(tx)
        .await
        .unwrap_or(None);
    match (profile, chat_id) {
        (Some(profile), Some(chat_id)) => {
            sqlx::query(
                "UPDATE durable_job SET chat_id=?,profile_revision=?,dependency_digest=?,eligible_at=?,queue_deadline_at=? WHERE id=?",
            )
            .bind(chat_id)
            .bind(profile)
            .bind(&dependency_digest)
            .bind(&now)
            .bind(now_utc_offset(300))
            .bind(&job_id)
            .execute(&mut **tx)
            .await?;
        }
        (Some(profile), None) => {
            sqlx::query(
                "UPDATE durable_job SET profile_revision=?,dependency_digest=?,eligible_at=?,queue_deadline_at=? WHERE id=?",
            )
            .bind(profile)
            .bind(&dependency_digest)
            .bind(&now)
            .bind(now_utc_offset(300))
            .bind(&job_id)
            .execute(&mut **tx)
            .await?;
        }
        (None, Some(chat_id)) => {
            sqlx::query(
                "UPDATE durable_job SET chat_id=?,dependency_digest=?,eligible_at=?,queue_deadline_at=? WHERE id=?",
            )
            .bind(chat_id)
            .bind(&dependency_digest)
            .bind(&now)
            .bind(now_utc_offset(300))
            .bind(&job_id)
            .execute(&mut **tx)
            .await?;
        }
        (None, None) => {
            sqlx::query(
                "UPDATE durable_job SET dependency_digest=?,eligible_at=?,queue_deadline_at=? WHERE id=?",
            )
            .bind(&dependency_digest)
            .bind(&now)
            .bind(now_utc_offset(300))
            .bind(&job_id)
            .execute(&mut **tx)
            .await?;
        }
    }
    let _ = prompt_version_id;
    Ok(job_id)
}

/// The bounded evidence vector for a party (or a segment composed of granted
/// parties). Only saved sources the party is actor of, or that back an
/// existing claim about it, are eligible; nothing else is collectable.
pub(crate) async fn collect_brief_evidence(
    tx: &mut Transaction<'_, Sqlite>,
    scope_kind: &str,
    scope_id: &str,
) -> Result<Vec<serde_json::Value>, BriefError> {
    let rows: Vec<(String, String, String, Option<String>)> = if scope_kind == "party" {
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
