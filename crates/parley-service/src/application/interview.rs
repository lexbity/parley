//! Typed owner-turn intake for the Interviewer (AR-001 AR04).
//!
//! One operation starts every Interviewer role job: an owner answer in an
//! existing chat or the Coach-answer flow that creates a fresh owner chat.
//! It freezes the reviewed prompt version (with explicit bundled fallback),
//! the active qualified profile and a canonical dependency vector at
//! admission, then enqueues exactly one durable role job inside the caller's
//! reserved transaction. The HTTP handlers and the Coach answer path are thin
//! adapters over this operation; they never duplicate scope resolution,
//! prompt lookup or job creation.

use crate::{WorkspaceError, digest, jobs, new_id, now_utc, now_utc_offset};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

pub(crate) const MAX_TURN_BYTES: usize = 16_000;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OwnerTurnInput {
    pub chat_id: String,
    pub text: String,
    pub epoch: String,
    /// Create the owner Interviewer chat when it does not exist yet. This is
    /// the Coach-answer flow; the chat id is then the derived Coach key.
    pub create_chat_if_missing: bool,
    /// Required when creating a chat; must match the chat's asset kind.
    pub asset_kind: Option<String>,
    /// Present when the turn answers a Coach question; survives into the job
    /// input so candidate confirmation can resolve the question disposition.
    pub coach_question_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OwnerTurnResult {
    pub chat_id: String,
    pub turn_id: String,
    pub job_id: String,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum InterviewError {
    NotFound,
    StaleRevision,
    InvalidTurn,
    InvalidKind,
    BankMissing,
    QueueAtCapacity,
    InferenceBusy,
    Database,
}

/// The active qualified structured-profile revision, if any. The same
/// admission rule gates every role that infers through the runtime boundary.
pub(crate) async fn active_profile_revision(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT a.profile_revision FROM inference_active_profile a
         JOIN inference_qualification q ON q.profile_revision=a.profile_revision AND q.mode='structured_role'
         WHERE a.singleton=1 AND q.status='ready' AND q.checked_at >= ?
         AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=a.profile_revision)
         ORDER BY q.checked_at DESC LIMIT 1",
    )
    .bind(now_utc_offset(-300))
    .fetch_optional(&mut **tx)
    .await
}

/// Row shape of an agent chat scope.
type ChatScopeRow = (String, String, Option<String>, Option<String>, i64);

/// Start one owner Interviewer turn. Every write, event and binding shares
/// the caller's transaction; a denial leaves no effect to roll back.
pub(crate) async fn start_owner_turn(
    tx: &mut Transaction<'_, Sqlite>,
    input: &OwnerTurnInput,
) -> Result<OwnerTurnResult, InterviewError> {
    let text = input.text.trim();
    if text.is_empty() || text.len() > MAX_TURN_BYTES {
        return Err(InterviewError::InvalidTurn);
    }
    let chat: Option<ChatScopeRow> = sqlx::query_as(
        "SELECT role,scope_kind,scope_id,asset_kind,revision FROM agent_chat WHERE id=?",
    )
    .bind(&input.chat_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| InterviewError::Database)?;
    let (asset_kind, revision) = match chat {
        Some((role, scope_kind, None, asset_kind, revision))
            if role == "interviewer" && scope_kind == "owner" && asset_kind.is_some() =>
        {
            (asset_kind, revision)
        }
        None if input.create_chat_if_missing => {
            let asset_kind = input
                .asset_kind
                .clone()
                .ok_or(InterviewError::InvalidKind)?;
            if !matches!(asset_kind.as_str(), "gift" | "proof" | "artifact") {
                return Err(InterviewError::InvalidKind);
            }
            let now = now_utc();
            sqlx::query(
                "INSERT INTO agent_chat(id,role,scope_kind,asset_kind,revision,created_at,updated_at)
                 VALUES(?,'interviewer','owner',?,1,?,?)",
            )
            .bind(&input.chat_id)
            .bind(&asset_kind)
            .bind(&now)
            .bind(&now)
            .execute(&mut **tx)
            .await
            .map_err(|_| InterviewError::Database)?;
            (Some(asset_kind), 1)
        }
        _ => return Err(InterviewError::NotFound),
    };
    let asset_kind = asset_kind.ok_or(InterviewError::InvalidKind)?;
    if let Some(expected) = &input.asset_kind
        && expected != &asset_kind
    {
        return Err(InterviewError::InvalidKind);
    }

    let turn_no: i64 =
        sqlx::query_scalar("SELECT coalesce(max(turn_no),0)+1 FROM agent_turn WHERE chat_id=?")
            .bind(&input.chat_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(|_| InterviewError::Database)?;
    let now = now_utc();
    let turn_id = new_id(&format!("{}:{turn_no}", input.chat_id));
    sqlx::query(
        "INSERT INTO agent_turn(id,chat_id,turn_no,speaker,text,created_at) VALUES(?,?,?,'owner',?,?)",
    )
    .bind(&turn_id)
    .bind(&input.chat_id)
    .bind(turn_no)
    .bind(text)
    .bind(&now)
    .execute(&mut **tx)
    .await
    .map_err(|_| InterviewError::Database)?;
    let next_revision = revision + 1;
    let changed =
        sqlx::query("UPDATE agent_chat SET revision=?,updated_at=? WHERE id=? AND revision=?")
            .bind(next_revision)
            .bind(&now)
            .bind(&input.chat_id)
            .bind(revision)
            .execute(&mut **tx)
            .await
            .map_err(|_| InterviewError::Database)?
            .rows_affected();
    if changed != 1 {
        return Err(InterviewError::StaleRevision);
    }

    let turns: Vec<Value> = sqlx::query_as::<_, (String, String, String)>(
        "SELECT id,speaker,text FROM agent_turn WHERE chat_id=? ORDER BY turn_no",
    )
    .bind(&input.chat_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| InterviewError::Database)?
    .into_iter()
    .map(|(id, speaker, text)| json!({"id":id,"speaker":speaker,"text":text}))
    .collect();

    let existing: Option<(String, String)> = sqlx::query_as(
        "SELECT fields_json,owner_quotes_json FROM interviewer_candidate
         WHERE chat_id=? AND state='proposed' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&input.chat_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| InterviewError::Database)?;
    let current_fields = existing
        .as_ref()
        .and_then(|row| serde_json::from_str::<Value>(&row.0).ok())
        .unwrap_or_else(|| json!({}));
    let current_quotes = existing
        .as_ref()
        .and_then(|row| serde_json::from_str::<Value>(&row.1).ok())
        .unwrap_or_else(|| json!({}));

    let bank: Option<(String, String)> = sqlx::query_as(
        "SELECT b.version_id,b.content_json FROM interviewer_question_bank_active a
         JOIN interviewer_question_bank_version b ON b.version_id=a.version_id
         WHERE a.singleton=1",
    )
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| InterviewError::Database)?;
    let Some((bank_id, bank_content)) = bank else {
        return Err(InterviewError::BankMissing);
    };

    // Prompt selection rule at admission: the reviewed active prompt version,
    // with an explicit fallback to the bundled role prompt when no owner
    // revision has been recorded.
    let active_prompt: Option<(String, String)> = sqlx::query_as(
        "SELECT a.version_id,p.prompt_text FROM agent_prompt_active a
         JOIN agent_prompt_version p ON p.version_id=a.version_id WHERE a.role='interviewer'",
    )
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| InterviewError::Database)?;
    let (prompt_version_id, _prompt) = match active_prompt {
        Some((version_id, text)) => (version_id, text),
        None => (
            "interviewer@1".to_owned(),
            crate::jobs::interviewer_prompt().to_owned(),
        ),
    };

    let profile = active_profile_revision(tx)
        .await
        .map_err(|_| InterviewError::Database)?;

    let mut input_json = json!({
        "chat_id": input.chat_id,
        "asset_kind": asset_kind,
        "chat_revision": next_revision,
        "turns": turns,
        "current_fields": current_fields,
        "current_quotes": current_quotes,
        "question_bank_version_id": bank_id,
        "question_bank": serde_json::from_str::<Value>(&bank_content).unwrap_or(Value::Null),
        "prompt_version_id": prompt_version_id,
    });
    if let Some(question_id) = &input.coach_question_id {
        input_json["coach_question_id"] = json!(question_id);
    }
    let dependency = json!([
        {"kind":"chat","id":input.chat_id,"revision":next_revision},
        {"kind":"bank","id":bank_id},
        {"kind":"prompt","id":prompt_version_id},
        {"kind":"asset","id":asset_kind},
    ]);
    let dependency_digest = digest(dependency.to_string().as_bytes());

    let job_id = jobs::enqueue_role_in_tx(
        tx,
        "interviewer",
        "owner",
        &input.chat_id,
        next_revision,
        &input_json,
        3,
        &input.epoch,
        None,
        Some(&digest(
            format!("parley-owner:1:{}", input.epoch).as_bytes(),
        )),
    )
    .await
    .map_err(|error| match error {
        WorkspaceError::Invalid(message) if message == "agent queue is at capacity" => {
            InterviewError::QueueAtCapacity
        }
        WorkspaceError::Invalid(message) if message == "inference queue is full" => {
            InterviewError::InferenceBusy
        }
        _ => InterviewError::Database,
    })?;
    if let Some(profile) = profile {
        sqlx::query(
            "UPDATE durable_job SET chat_id=?,profile_revision=?,dependency_digest=?,eligible_at=?,queue_deadline_at=? WHERE id=?",
        )
        .bind(&input.chat_id)
        .bind(profile)
        .bind(&dependency_digest)
        .bind(&now)
        .bind(now_utc_offset(300))
        .bind(&job_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| InterviewError::Database)?;
    } else {
        sqlx::query(
            "UPDATE durable_job SET chat_id=?,dependency_digest=?,eligible_at=?,queue_deadline_at=? WHERE id=?",
        )
        .bind(&input.chat_id)
        .bind(&dependency_digest)
        .bind(&now)
        .bind(now_utc_offset(300))
        .bind(&job_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| InterviewError::Database)?;
    }

    Ok(OwnerTurnResult {
        chat_id: input.chat_id.clone(),
        turn_id,
        job_id,
        revision: next_revision,
    })
}
