//! Owns the packaged structured-output execution path for the contextual
//! Template Builder (AR-001 AR03).

use super::{
    coach::{ProfileRow, profile_fields},
    protocol::{Completion, Execute, Profile, Role, Usage},
    supervisor::Supervisor,
};
use crate::{
    AppState, WorkspaceError, application::template_build, digest, jobs, new_id, now_utc,
    now_utc_offset, random_hex,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Connection;
use std::{net::SocketAddr, path::PathBuf, time::Duration};
use tokio::sync::watch;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // the envelope is re-serialized for dependency refresh
struct Input {
    context: Option<crate::application::inference_context::ContextRequest>,
    context_projection: Option<Value>,
    segment_scope: Option<Value>,
    dependency_json: Value,
    scope: Value,
    recipient_name: Option<Value>,
    family_head_id: Option<String>,
    stage_class: String,
    rungs: Vec<u32>,
    rules: Vec<String>,
    optional_rules: Vec<String>,
    channel: String,
    max_chars: i64,
    voice_samples: Vec<Value>,
    voice_sample_revisions: Value,
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

struct Lease {
    number: i64,
    token: String,
    dependencies: String,
    input_sha256: String,
    input: Input,
    input_json: String,
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

async fn claim(state: &AppState, id: &str) -> Result<Claim, WorkspaceError> {
    let workspace = &state.workspace;
    let mut tx = workspace.pool().begin_with("BEGIN IMMEDIATE").await?;
    let frozen: Option<Frozen> = sqlx::query_as(
        "SELECT status,current_attempt,max_attempts,queue_deadline_at,total_deadline_at,
                profile_revision,dependency_digest,input_sha256,input_json
         FROM durable_job WHERE id=? AND epoch=? AND role='template_builder' AND profile_revision IS NOT NULL",
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
                p.tokenizer_digest,p.recipe_json,p.context_tokens,p.output_tokens,
                p.embed_model,p.embed_model_digest
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
    let input = input.expect("validated frozen builder input");
    let input_json = frozen.input_json;
    let (endpoint, profile, runner_digest) = fields.expect("validated frozen inference profile");
    let number = frozen.current_attempt + 1;
    let token = random_hex(32);
    let process_nonce = crate::inference::dispatch::process_nonce();
    sqlx::query("UPDATE durable_job SET status='running',current_attempt=?,lease_id=?,process_nonce=?,total_deadline_at=?,updated_at=? WHERE id=?")
        .bind(number).bind(&token).bind(process_nonce).bind(&total_deadline).bind(&now).bind(id)
        .execute(&mut *tx).await?;
    let prompt_digest = digest(input.prompt.as_bytes());
    let schema_digest = digest(template_build::SCHEMA.as_bytes());
    sqlx::query(
        "INSERT INTO job_attempt(job_id,attempt_no,status,model,model_digest,provider_snapshot_json,
             prompt_id,prompt_sha256,input_sha256,input_json,started_at)
         VALUES(?,?,'running',?,?,?,?,?,?,?,?)",
    )
        .bind(id).bind(number).bind(&profile.model_tag).bind(&profile.model_digest)
        .bind(serde_json::to_string(&profile).map_err(|_| WorkspaceError::Invalid("Invalid profile".into()))?)
        .bind(&input.prompt_version).bind(&prompt_digest).bind(&frozen.input_sha256)
        .bind(&input_json).bind(&now).execute(&mut *tx).await?;
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
        input_json,
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
            if publish(state, id, &lease, candidate, usage).await.is_err() {
                fail(state, id, &lease, "PERSISTENCE_FAILED", "failed").await;
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
    Ok(state
        .workspace
        .control_path()
        .join("tokenizers")
        .join(format!("{}.json", profile.tokenizer_digest)))
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
    let execution = new_id("builder-worker");
    let input = json!({
        "context":lease.input.context_projection,
        "rules":lease.input.rules,
        "optional_rules":lease.input.optional_rules,
        "stage_class":lease.input.stage_class,
        "rungs":lease.input.rungs,
        "channel":lease.input.channel,
        "max_chars":lease.input.max_chars,
    });
    let request = Execute::StructuredRole {
        profile: lease.profile.clone(),
        deadline_ms: lease.remaining_ms,
        role: Role::TemplateBuilder,
        prompt_version: lease.input.prompt_version.clone(),
        schema_digest: digest(template_build::SCHEMA.as_bytes()),
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
        Completion::Delegated { .. } => Err("WORKER_PROTOCOL"),
        Completion::Embed { .. } => Err("WORKER_PROTOCOL"),
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
    candidate: Value,
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
    let dependencies = tokio::time::timeout(Duration::from_millis(500), async {
        let input: Value = serde_json::from_str(&lease.input_json).unwrap_or_default();
        template_build::dependency_digest_for(&mut tx, &input).await
    })
    .await
    .map_err(|_| WorkspaceError::StaleRevision)??;
    if dependencies != lease.dependencies {
        return Err(WorkspaceError::StaleRevision);
    }
    let input: Value = serde_json::from_str(&lease.input_json).unwrap_or_default();
    let context = template_build::BuildContext::from_projection(
        input.get("context_projection").unwrap_or(&Value::Null),
    );
    let requested: std::collections::BTreeSet<String> = lease.input.rules.iter().cloned().collect();
    let optional: std::collections::BTreeSet<String> =
        lease.input.optional_rules.iter().cloned().collect();
    let valid =
        template_build::validate_candidates(&mut tx, &candidate, &context, &requested, &optional)
            .await
            .map_err(|_| WorkspaceError::Invalid("RESULT_INVALID".into()))?;
    let now = now_utc();
    let expires = now_utc_offset(30 * 24 * 60 * 60);
    let mut candidate_rows = Vec::with_capacity(valid.len());
    for (index, candidate) in valid.into_iter().enumerate() {
        let variant_no = (index + 1) as i64;
        let candidate_id = new_id(format!("{id}:{variant_no}").as_str());
        let fallbacks = serde_json::to_string(&candidate.fallbacks)
            .map_err(|_| WorkspaceError::Invalid("CANDIDATE_INVALID".into()))?;
        let sentences = serde_json::to_string(&candidate.sentences)
            .map_err(|_| WorkspaceError::Invalid("CANDIDATE_INVALID".into()))?;
        let preview = serde_json::to_string(&candidate.preview)
            .map_err(|_| WorkspaceError::Invalid("CANDIDATE_INVALID".into()))?;
        let requested_rules = serde_json::to_string(&lease.input.rules)
            .map_err(|_| WorkspaceError::Invalid("CANDIDATE_INVALID".into()))?;
        let optional_rules = serde_json::to_string(&lease.input.optional_rules)
            .map_err(|_| WorkspaceError::Invalid("CANDIDATE_INVALID".into()))?;
        let dependency_json = serde_json::to_string(&input["dependency_json"])
            .map_err(|_| WorkspaceError::Invalid("CANDIDATE_INVALID".into()))?;
        sqlx::query(
            "INSERT INTO template_build_candidate(
                 id,job_id,variant_no,name,body,sentences_json,fallbacks_json,notes,
                 requested_rules_json,optional_rules_json,dependency_json,dependency_digest,
                 preview_json,status,revision,family_head_id,expires_at,created_at,updated_at)
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,'owner_review_required',1,?,?,?,?)",
        )
        .bind(&candidate_id)
        .bind(id)
        .bind(variant_no)
        .bind(&candidate.name)
        .bind(&candidate.body)
        .bind(sentences)
        .bind(fallbacks)
        .bind(&candidate.notes)
        .bind(requested_rules)
        .bind(optional_rules)
        .bind(dependency_json)
        .bind(&lease.dependencies)
        .bind(preview)
        .bind(&lease.input.family_head_id)
        .bind(&expires)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        candidate_rows
            .push(json!({"id":candidate_id,"variant_no":variant_no,"name":candidate.name}));
    }
    sqlx::query("UPDATE inference_record SET model_digest=?,metadata_class='observed' WHERE id=?")
        .bind(&lease.profile.model_digest)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let result = json!({
        "candidate_count":candidate_rows.len(),
        "candidates":candidate_rows,
        "status":"owner_review_required",
    });
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
