//! Owns the packaged structured-output execution path for weekly Coach.

use super::{
    protocol::{Completion, Execute, Profile, Role, Usage},
    supervisor::Supervisor,
};
use crate::{
    AppState, WorkspaceError,
    application::coach::{self, Coverage, Seed, Week},
    digest, jobs, new_id, now_utc, now_utc_offset, random_hex,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Connection;
use std::{net::SocketAddr, path::PathBuf, time::Duration};
use tokio::sync::watch;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    week: Week,
    seeds: Vec<Seed>,
    coverage: Coverage,
    event_cursor: i64,
    prompt_version: String,
    prompt: String,
}

#[derive(sqlx::FromRow)]
struct Frozen {
    status: String,
    current_attempt: i64,
    max_attempts: i64,
    queue_deadline_at: String,
    total_deadline_at: Option<String>,
    profile_revision: i64,
    dependency_digest: String,
    input_sha256: String,
    input_json: String,
}

#[derive(sqlx::FromRow)]
pub(super) struct ProfileRow {
    endpoint: Option<String>,
    runner_build_digest: Option<String>,
    provider_version: Option<String>,
    chat_model: Option<String>,
    chat_model_digest: Option<String>,
    tokenizer_digest: Option<String>,
    recipe_json: String,
    context_tokens: i64,
    output_tokens: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipe {
    template_tokens: u32,
}

struct Lease {
    number: i64,
    token: String,
    dependencies: String,
    input_sha256: String,
    input: Input,
    profile: Profile,
    runner_digest: String,
    endpoint: SocketAddr,
    remaining_ms: u32,
}

enum Claim {
    Missing,
    Failure(&'static str),
    Ready(Box<Lease>),
}

pub(super) fn profile_fields(
    revision: i64,
    row: ProfileRow,
) -> Option<(SocketAddr, Profile, String)> {
    let address = super::profile::loopback_authority(&row.endpoint?).ok()?;
    let recipe: Recipe = serde_json::from_str(&row.recipe_json).ok()?;
    let profile = Profile {
        revision: u32::try_from(revision).ok()?,
        provider_version: row.provider_version?,
        model_tag: row.chat_model?,
        model_digest: row.chat_model_digest?,
        tokenizer_digest: row.tokenizer_digest?,
        context_tokens: u32::try_from(row.context_tokens).ok()?,
        max_output_tokens: u32::try_from(row.output_tokens).ok()?,
        template_tokens: recipe.template_tokens,
    };
    if ![&profile.tokenizer_digest, &profile.model_digest]
        .into_iter()
        .all(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
        || profile.model_tag.to_ascii_lowercase().contains("cloud")
    {
        return None;
    }
    Some((address, profile, row.runner_build_digest?))
}

async fn claim(state: &AppState, id: &str) -> Result<Claim, WorkspaceError> {
    let workspace = &state.workspace;
    let mut tx = workspace.pool().begin_with("BEGIN IMMEDIATE").await?;
    let frozen: Option<Frozen> = sqlx::query_as(
        "SELECT status,current_attempt,max_attempts,queue_deadline_at,total_deadline_at,
                profile_revision,dependency_digest,input_sha256,input_json
         FROM durable_job WHERE id=? AND epoch=? AND role='coach' AND profile_revision IS NOT NULL",
    )
    .bind(id)
    .bind(workspace.epoch())
    .fetch_optional(&mut *tx)
    .await?;
    let Some(frozen) = frozen.filter(|row| row.status == "queued") else {
        return Ok(Claim::Missing);
    };
    let now = now_utc();
    let total_deadline = frozen
        .total_deadline_at
        .unwrap_or_else(|| now_utc_offset(360));
    let input = serde_json::from_str::<Input>(&frozen.input_json).ok();
    let row: Option<ProfileRow> = sqlx::query_as(
        "SELECT p.endpoint,p.runner_build_digest,p.provider_version,p.chat_model,p.chat_model_digest,
                p.tokenizer_digest,p.recipe_json,p.context_tokens,p.output_tokens
         FROM inference_profile p WHERE revision=?
           AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=p.revision)
           AND EXISTS(SELECT 1 FROM inference_qualification q WHERE q.profile_revision=p.revision
                      AND q.mode='structured_role' AND q.status='ready' AND q.checked_at >= ?)",
    ).bind(frozen.profile_revision).bind(now_utc_offset(-300)).fetch_optional(&mut *tx).await?;
    let fields = row.and_then(|row| profile_fields(frozen.profile_revision, row));
    let failure = if frozen.current_attempt == 0 && frozen.queue_deadline_at < now {
        Some("QUEUE_EXPIRED")
    } else if total_deadline <= now || frozen.current_attempt >= frozen.max_attempts {
        Some("ATTEMPT_TIMEOUT")
    } else if input.is_none() {
        Some("RESULT_INVALID")
    } else if fields.is_none() {
        Some("CAPABILITY_UNQUALIFIED")
    } else {
        None
    };
    if let Some(code) = failure {
        sqlx::query("UPDATE durable_job SET status='failed',last_error_code=?,completed_at=?,updated_at=?,lease_id=NULL WHERE id=?")
            .bind(code).bind(&now).bind(&now).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(Claim::Failure(code));
    }
    let input = input.expect("validated frozen Coach input");
    let (endpoint, profile, runner_digest) = fields.expect("validated frozen inference profile");
    let number = frozen.current_attempt + 1;
    let token = random_hex(32);
    let process_nonce = crate::inference::dispatch::process_nonce();
    sqlx::query("UPDATE durable_job SET status='running',current_attempt=?,lease_id=?,process_nonce=?,total_deadline_at=?,updated_at=? WHERE id=?")
        .bind(number).bind(&token).bind(process_nonce).bind(&total_deadline).bind(&now).bind(id)
        .execute(&mut *tx).await?;
    let prompt_digest = digest(input.prompt.as_bytes());
    let schema_digest = digest(coach::SCHEMA.as_bytes());
    sqlx::query(
        "INSERT INTO job_attempt(job_id,attempt_no,status,model,model_digest,provider_snapshot_json,
             prompt_id,prompt_sha256,input_sha256,input_json,started_at)
         VALUES(?,?,'running',?,?,?,?,?,?,?,?)",
    )
        .bind(id).bind(number).bind(&profile.model_tag).bind(&profile.model_digest)
        .bind(serde_json::to_string(&profile).map_err(|_| WorkspaceError::Invalid("Invalid profile".into()))?)
        .bind(&input.prompt_version).bind(&prompt_digest).bind(&frozen.input_sha256)
        .bind(frozen.input_json).bind(&now).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE inference_record SET runner_build_digest=?,prompt_digest=?,schema_digest=? WHERE id=?",
    ).bind(&runner_digest).bind(&prompt_digest).bind(&schema_digest).bind(id)
        .execute(&mut *tx).await?;
    crate::assistant::append_event(
        &mut tx,
        id,
        "job.attempt_started",
        json!({"attempt":number}),
    )
    .await?;
    tx.commit().await?;
    let remaining_ms = chrono::DateTime::parse_from_rfc3339(&total_deadline)
        .ok()
        .map_or(0, |deadline| {
            (deadline.with_timezone(&chrono::Utc) - chrono::Utc::now())
                .num_milliseconds()
                .clamp(0, 120_000) as u32
        });
    Ok(Claim::Ready(Box::new(Lease {
        number,
        token,
        dependencies: frozen.dependency_digest,
        input_sha256: frozen.input_sha256,
        input,
        profile,
        runner_digest,
        endpoint,
        remaining_ms,
    })))
}

pub(crate) async fn run(state: &AppState, id: &str) {
    let lease = match claim(state, id).await {
        Ok(Claim::Ready(lease)) => lease,
        Ok(Claim::Failure(code)) => {
            eprintln!("inference claim: {code}");
            return;
        }
        Ok(Claim::Missing) | Err(_) => return,
    };
    match execute(state, id, &lease).await {
        Ok((candidate, usage)) => {
            if let Some(questions) = coach::validate_output(candidate, &lease.input.seeds) {
                if let Err(error) = publish(state, id, &lease, questions, usage).await {
                    let code = if matches!(error, WorkspaceError::StaleRevision) {
                        "STALE_INPUT"
                    } else {
                        "PERSISTENCE_FAILED"
                    };
                    fail(state, id, &lease, code, "failed").await;
                }
            } else {
                fail(state, id, &lease, "RESULT_INVALID", "invalid").await;
            }
        }
        Err(code) => {
            let disposition = if code == "RESULT_INVALID" && lease.number < 3 {
                "retry"
            } else {
                "failed"
            };
            fail(state, id, &lease, code, disposition).await;
        }
    }
}

pub(super) fn installed_supervisor() -> Result<Supervisor, &'static str> {
    if cfg!(debug_assertions) {
        match std::env::var_os("PARLEY_RUNTIME_BUNDLE") {
            Some(path) => Supervisor::from_bundle(&PathBuf::from(path)),
            None => Supervisor::installed(),
        }
    } else {
        Supervisor::installed()
    }
    .map_err(|error| error.code())
}

pub(super) fn tokenizer_path(state: &AppState, profile: &Profile) -> Result<PathBuf, &'static str> {
    Ok(
        crate::Workspace::control_directory(state.workspace.database_path())
            .map_err(|_| "RUNNER_INSTALL_INVALID")?
            .join("tokenizers")
            .join(format!("{}.json", profile.tokenizer_digest)),
    )
}

async fn execute(
    state: &AppState,
    id: &str,
    lease: &Lease,
) -> Result<(Value, Usage), &'static str> {
    if lease.remaining_ms < 1 {
        return Err("ATTEMPT_TIMEOUT");
    }
    let supervisor = installed_supervisor()?;
    if supervisor.manifest_digest() != lease.runner_digest {
        return Err("PROFILE_CHANGED");
    }
    let tokenizer = tokenizer_path(state, &lease.profile)?;
    let execution = new_id("coach-worker");
    let input = json!({"seeds":lease.input.seeds.iter().map(|seed| json!({
        "question_key":seed.key,"reason":seed.reason
    })).collect::<Vec<_>>()});
    let request = Execute::StructuredRole {
        profile: lease.profile.clone(),
        deadline_ms: lease.remaining_ms,
        role: Role::Coach,
        prompt_version: lease.input.prompt_version.clone(),
        schema_digest: digest(coach::SCHEMA.as_bytes()),
        system: lease.input.prompt.clone(),
        input,
    };
    let (cancel, signal) = watch::channel(false);
    let mut stop = state.restart.subscribe();
    let result = supervisor.execute(
        lease.endpoint,
        &execution,
        &lease.token,
        &tokenizer,
        request,
        signal,
    );
    tokio::pin!(result);
    let mut poll = tokio::time::interval(Duration::from_millis(50));
    let result = loop {
        tokio::select! {
            result = &mut result => break result,
            _ = stop.changed() => { cancel.send_replace(true); break result.await; }
            _ = poll.tick() => {
                let valid = sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS(SELECT 1 FROM durable_job j JOIN workspace_meta w
                     ON w.singleton=1 AND w.epoch=j.epoch
                     WHERE j.id=? AND j.lease_id=? AND j.status='running'
                     AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=j.profile_revision))"
                ).bind(id).bind(&lease.token).fetch_one(state.workspace.pool()).await.unwrap_or(false);
                if !valid { cancel.send_replace(true); break result.await; }
            }
        }
    }.map_err(|error| error.code())?;
    match result {
        Completion::StructuredRole {
            candidate, usage, ..
        } => Ok((candidate, usage)),
        Completion::Probe { .. } => Err("WORKER_PROTOCOL"),
    }
}

async fn fail(state: &AppState, id: &str, lease: &Lease, code: &str, disposition: &str) {
    jobs::finish_attempt_error(
        &state.workspace,
        id,
        lease.number,
        code,
        "Local inference did not publish a result",
        disposition,
    )
    .await;
}

async fn publish(
    state: &AppState,
    id: &str,
    lease: &Lease,
    questions: Vec<String>,
    usage: Usage,
) -> Result<(), WorkspaceError> {
    let workspace = &state.workspace;
    let mut conn = workspace.pool().acquire().await?;
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    let bound: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM durable_job j JOIN workspace_meta w ON w.singleton=1 AND w.epoch=j.epoch
          WHERE j.id=? AND j.status='running' AND j.current_attempt=? AND j.lease_id=?
          AND j.total_deadline_at >= ?
          AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=j.profile_revision))",
    ).bind(id).bind(lease.number).bind(&lease.token).bind(now_utc()).fetch_one(&mut *tx).await?;
    if !bound {
        return Err(WorkspaceError::StaleRevision);
    }
    let plan = tokio::time::timeout(
        Duration::from_millis(500),
        coach::plan(&mut tx, &lease.input.week),
    )
    .await
    .map_err(|_| WorkspaceError::StaleRevision)??;
    if plan.dependencies != lease.dependencies {
        return Err(WorkspaceError::StaleRevision);
    }
    let held: Vec<String> = sqlx::query_scalar(
        "SELECT question_key FROM coach_question_hold WHERE epoch=? AND job_id=? ORDER BY question_key",
    ).bind(workspace.epoch()).bind(id).fetch_all(&mut *tx).await?;
    let mut expected = lease
        .input
        .seeds
        .iter()
        .map(|seed| seed.key.clone())
        .collect::<Vec<_>>();
    expected.sort_unstable();
    if held != expected {
        return Err(WorkspaceError::StaleRevision);
    }
    let now = now_utc();
    let period = &lease.input.week.period;
    let cursor = lease.input.event_cursor;
    let report_id = new_id(id);
    let report = json!({"period":period,"event_cursor":cursor.to_string(),
        "summary":null,"evidence":{"coverage":lease.input.coverage,"candidate_count":questions.len()}});
    sqlx::query("INSERT INTO coach_report(id,period,input_digest,event_cursor,job_id,report_json,created_at) VALUES(?,?,?,?,?,?,?)")
        .bind(&report_id).bind(period).bind(&lease.input_sha256).bind(cursor).bind(id)
        .bind(report.to_string()).bind(&now).execute(&mut *tx).await?;
    for (rank, (seed, text)) in lease.input.seeds.iter().zip(questions).enumerate() {
        sqlx::query(
            "INSERT INTO coach_question(id,job_id,question_key,kind,target_json,answer_schema_json,priority,impact,
                 text,status,created_at,updated_at,semantic_key)
             VALUES(?,?,?,?,?,?,?,?,?,'open',?,?,?)",
        ).bind(new_id(id)).bind(id).bind(&seed.key).bind(seed.reason.kind()).bind(seed.target.wire().to_string())
            .bind(seed.answer_schema().to_string()).bind(rank as i64).bind(seed.impact).bind(text)
            .bind(&now).bind(&now).bind(&seed.key).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE inference_record SET model_digest=?,metadata_class='observed' WHERE id=?")
        .bind(&lease.profile.model_digest)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let result = json!({"report_id":report_id,"period":period,"event_cursor":cursor.to_string()});
    jobs::complete_role_result_in_tx(
        &mut tx,
        id,
        lease.number,
        &lease.input_sha256,
        &result,
        usage.input_tokens,
        usage.output_tokens,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
