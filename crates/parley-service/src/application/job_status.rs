//! Typed scoped job status read (UCR-001 U05).
//!
//! `job.get_status` is the read counterpart of `brief.start`. It returns a
//! bounded projection of one durable job: progress, a safe error code and a
//! typed review reference when a reviewable proposal set exists. It never
//! returns raw provider output, job inputs or transcript bodies. A delegated
//! run can read only jobs it started; anything else is concealed as
//! unavailable.

use sqlx::{Sqlite, Transaction};

use crate::commands::Actor;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum JobStatusError {
    Database,
}

type JobRow = (
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    Option<String>,
    String,
    String,
    Option<String>,
);

/// The scope observation summarized for one job.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct ScopeRef {
    pub scope_kind: String,
    pub scope_id: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct JobStatusResult {
    pub job_id: String,
    pub role: String,
    pub state: String,
    pub current_attempt: i64,
    pub max_attempts: i64,
    pub progress_current: i64,
    pub progress_total: i64,
    pub last_error_code: Option<String>,
    /// Present only for a terminal job with reviewable proposals.
    pub review_reference: Option<ScopeRef>,
    pub event_cursor: String,
}

pub(crate) async fn status(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Actor,
    job_id: &str,
) -> Result<Option<JobStatusResult>, JobStatusError> {
    let row: Option<JobRow> = sqlx::query_as::<_, JobRow>(
        "SELECT role,status,current_attempt,max_attempts,progress_current,progress_total,last_error_code,epoch,scope_kind,run_id FROM durable_job WHERE id=?",
    )
        .bind(job_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| JobStatusError::Database)?;
    let Some((
        role,
        state,
        current,
        maximum,
        progress_current,
        progress_total,
        last_error,
        job_epoch,
        scope_kind,
        run_id,
    )) = row
    else {
        return Ok(None);
    };
    // A delegated run may read only its own jobs in its own epoch. The owner
    // uses the existing REST job surface; this read is agent-scoped.
    if let Some(run_id) = run_id.as_deref() {
        let my_run = actor.run_id.as_deref().unwrap_or("");
        if run_id != my_run || job_epoch != actor.epoch {
            return Ok(None);
        }
    } else if actor.kind == crate::commands::ActorKind::Agent {
        return Ok(None);
    }

    let review_reference = if state == "succeeded" {
        let proposed: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM brief_item WHERE job_id=? AND status='proposed'",
        )
        .bind(job_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| JobStatusError::Database)?;
        if proposed > 0 {
            Some(ScopeRef {
                scope_kind: scope_kind.clone(),
                scope_id: scope_id_of(tx, job_id).await?,
            })
        } else {
            None
        }
    } else {
        None
    };

    let cursor: i64 = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT max(cursor) FROM event WHERE entity_type='job' AND entity_id=?",
    )
    .bind(job_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| JobStatusError::Database)?
    .unwrap_or(0);

    Ok(Some(JobStatusResult {
        job_id: job_id.to_string(),
        role,
        state,
        current_attempt: current,
        max_attempts: maximum,
        progress_current,
        progress_total,
        last_error_code: last_error,
        review_reference,
        event_cursor: cursor.to_string(),
    }))
}

async fn scope_id_of(
    tx: &mut Transaction<'_, Sqlite>,
    job_id: &str,
) -> Result<String, JobStatusError> {
    sqlx::query_scalar("SELECT scope_id FROM durable_job WHERE id=?")
        .bind(job_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| JobStatusError::Database)
}
