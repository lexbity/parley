//! Owns the packaged structured-output execution path for the Extractor
//! (AR-001 AR06).
//!
//! One attempt executes the frozen chunk plan serially: each chunk runs as
//! its own bounded worker execution under the same durable lease. Candidate
//! staging happens only after every chunk succeeded, so a failed chunk
//! invalidates the whole candidate batch. All deterministic validation
//! (schema caps, quote fidelity against the original source, sensitive
//! content, cross-chunk deduplication) is host-implemented here; the model
//! output is data.

use super::{
    coach::{ProfileRow, profile_fields},
    protocol::{Completion, Execute, Profile, Role},
    template_builder::{installed_supervisor, tokenizer_path},
};
use crate::{
    AppState, WorkspaceError, application,
    application::extract::{Candidate, ExtractInput, ExtractorOutput, chunk_text, valid_candidate},
    digest, jobs, new_id, now_utc, now_utc_offset, random_hex,
};
use serde_json::json;
use sqlx::Connection;
use std::{net::SocketAddr, time::Duration};
use tokio::sync::watch;

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
    input: ExtractInput,
    profile: Profile,
    runner_digest: String,
    endpoint: SocketAddr,
    remaining_ms: u32,
    raw_text: String,
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
         FROM durable_job WHERE id=? AND epoch=? AND role='extractor' AND profile_revision IS NOT NULL",
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
    let input = serde_json::from_str::<ExtractInput>(&frozen.input_json).ok();
    let row: Option<ProfileRow> = sqlx::query_as(
        "SELECT p.endpoint,p.runner_build_digest,p.provider_version,p.chat_model,p.chat_model_digest,
                p.tokenizer_digest,p.recipe_json,p.context_tokens,p.output_tokens,
                p.embed_model,p.embed_model_digest
         FROM inference_profile p WHERE revision=?
           AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=p.revision)
           AND EXISTS(SELECT 1 FROM inference_qualification q WHERE q.profile_revision=p.revision
                      AND q.mode='structured_role' AND q.status='ready' AND q.checked_at >= ?)",
    )
    .bind(frozen.profile_revision)
    .bind(now_utc_offset(-300))
    .fetch_optional(&mut *tx)
    .await?;
    let fields = row.and_then(|row| profile_fields(frozen.profile_revision, row));
    let raw_text = if let Some(input) = input.as_ref() {
        sqlx::query_scalar("SELECT raw_text FROM source_variant WHERE id=?")
            .bind(&input.source_variant_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| WorkspaceError::Invalid("SOURCE_UNAVAILABLE".into()))?
            .flatten()
    } else {
        None
    };
    let failure = if frozen.current_attempt == 0 && frozen.queue_deadline_at < now {
        Some("QUEUE_EXPIRED")
    } else if total_deadline <= now || frozen.current_attempt >= frozen.max_attempts {
        Some("ATTEMPT_TIMEOUT")
    } else if input.is_none() {
        Some("RESULT_INVALID")
    } else if fields.is_none() {
        Some("CAPABILITY_UNQUALIFIED")
    } else if raw_text.is_none() {
        Some("SOURCE_UNAVAILABLE")
    } else {
        None
    };
    if let Some(code) = failure {
        sqlx::query("UPDATE durable_job SET status='failed',last_error_code=?,completed_at=?,updated_at=?,lease_id=NULL WHERE id=?")
            .bind(code)
            .bind(&now)
            .bind(&now)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(Claim::Failure(code));
    }
    let input = input.expect("validated frozen extractor input");
    let raw_text = raw_text.expect("validated source text");
    let (endpoint, profile, runner_digest) = fields.expect("validated frozen inference profile");
    let number = frozen.current_attempt + 1;
    let token = random_hex(32);
    let process_nonce = crate::inference::dispatch::process_nonce();
    sqlx::query("UPDATE durable_job SET status='running',current_attempt=?,lease_id=?,process_nonce=?,total_deadline_at=?,updated_at=? WHERE id=?")
        .bind(number)
        .bind(&token)
        .bind(process_nonce)
        .bind(&total_deadline)
        .bind(&now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let prompt_digest = digest(input.prompt.as_bytes());
    let schema_digest = digest(application::extract::SCHEMA.as_bytes());
    sqlx::query(
        "INSERT INTO job_attempt(job_id,attempt_no,status,model,model_digest,provider_snapshot_json,
             prompt_id,prompt_sha256,input_sha256,input_json,started_at)
         VALUES(?,?,'running',?,?,?,?,?,?,?,?)",
    )
    .bind(id)
    .bind(number)
    .bind(&profile.model_tag)
    .bind(&profile.model_digest)
    .bind(serde_json::to_string(&profile).map_err(|_| WorkspaceError::Invalid("Invalid profile".into()))?)
    .bind(&input.prompt_version)
    .bind(&prompt_digest)
    .bind(&frozen.input_sha256)
    .bind(frozen.input_json)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE inference_record SET runner_build_digest=?,prompt_digest=?,schema_digest=? WHERE id=?",
    )
    .bind(&runner_digest)
    .bind(&prompt_digest)
    .bind(&schema_digest)
    .bind(id)
    .execute(&mut *tx)
    .await?;
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
        raw_text,
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
        Ok(aggregate) => {
            if publish(state, id, &lease, aggregate).await.is_err() {
                fail(state, id, &lease, "PERSISTENCE_FAILED", "failed").await;
            }
        }
        Err(code) => {
            let disposition = if code == "RESULT_INVALID" {
                if lease.number < 3 { "retry" } else { "invalid" }
            } else {
                "failed"
            };
            fail(state, id, &lease, code, disposition).await;
        }
    }
}

struct Aggregate {
    candidates: Vec<Candidate>,
    injection_suspected: bool,
    dropped_items: u32,
    tokens_in: u32,
    tokens_out: u32,
}

async fn execute(state: &AppState, id: &str, lease: &Lease) -> Result<Aggregate, &'static str> {
    if lease.remaining_ms < 1 {
        return Err("ATTEMPT_TIMEOUT");
    }
    let supervisor = installed_supervisor()?;
    if supervisor.manifest_digest() != lease.runner_digest {
        return Err("PROFILE_CHANGED");
    }
    let tokenizer = tokenizer_path(state, &lease.profile)?;
    let total = lease.input.chunks.len() as i64;
    // One normalized-origin index for the whole attempt: candidates from every
    // chunk verify against the same source, so rebuilding `verify_quote`'s map
    // per candidate would rescan the full source once per item.
    let source_index = parley_domain::evidence::SourceIndex::new(&lease.raw_text);
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut injection_suspected = false;
    let mut dropped_items = 0u32;
    let mut tokens_in = 0u32;
    let mut tokens_out = 0u32;
    let started = tokio::time::Instant::now();
    for (index, chunk) in lease.input.chunks.iter().enumerate() {
        let elapsed_ms = started.elapsed().as_millis() as u32;
        let deadline_ms = lease
            .remaining_ms
            .min(120_000u32.saturating_sub(elapsed_ms).max(1));
        if deadline_ms <= 1 {
            return Err("ATTEMPT_TIMEOUT");
        }
        let _ = sqlx::query(
            "UPDATE durable_job SET progress_current=?,progress_total=? WHERE id=? AND status='running' AND current_attempt=?",
        )
        .bind(index as i64)
        .bind(total)
        .bind(id)
        .bind(lease.number)
        .execute(state.workspace.pool())
        .await;
        let execution = new_id(&format!("{id}:chunk-{}", chunk.index));
        let input = json!({
            "content": chunk_text(&lease.raw_text, chunk),
            "chunk_index": chunk.index,
            "chunk_total": total,
            "locale": lease.input.locale,
        });
        let request = Execute::StructuredRole {
            profile: lease.profile.clone(),
            deadline_ms,
            role: Role::Extractor,
            prompt_version: lease.input.prompt_version.clone(),
            schema_digest: digest(application::extract::SCHEMA.as_bytes()),
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
                         AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=j.profile_revision))",
                    )
                    .bind(id)
                    .bind(&lease.token)
                    .fetch_one(state.workspace.pool())
                    .await
                    .unwrap_or(false);
                    if !valid { cancel.send_replace(true); break result.await; }
                }
            }
        }
        .map_err(|error| error.code())?;
        let (candidate, usage) = match result {
            Completion::StructuredRole {
                candidate, usage, ..
            } => (candidate, usage),
            Completion::Probe { .. } | Completion::Delegated { .. } | Completion::Embed { .. } => {
                return Err("WORKER_PROTOCOL");
            }
        };
        let output: ExtractorOutput =
            serde_json::from_value(candidate).map_err(|_| "RESULT_INVALID")?;
        injection_suspected |=
            application::extract::injection_suspected(&chunk_text(&lease.raw_text, chunk));
        injection_suspected |= output.injection_suspected;
        for candidate in output.candidates {
            if !valid_candidate(&candidate) {
                dropped_items += 1;
                continue;
            }
            if source_index.verify(&candidate.quote, None).is_err() {
                dropped_items += 1;
                continue;
            }
            let key = (
                parley_domain::evidence::normalize(&candidate.quote),
                candidate.kind.clone(),
            );
            if !seen.insert(key) {
                continue;
            }
            candidates.push(candidate);
        }
        tokens_in = tokens_in.saturating_add(usage.input_tokens);
        tokens_out = tokens_out.saturating_add(usage.output_tokens);
    }
    let _ = sqlx::query(
        "UPDATE durable_job SET progress_current=?,progress_total=? WHERE id=? AND status='running' AND current_attempt=?",
    )
    .bind(total)
    .bind(total)
    .bind(id)
    .bind(lease.number)
    .execute(state.workspace.pool())
    .await;
    Ok(Aggregate {
        candidates,
        injection_suspected,
        dropped_items,
        tokens_in,
        tokens_out,
    })
}

async fn fail(state: &AppState, id: &str, lease: &Lease, code: &str, disposition: &str) {
    jobs::finish_attempt_error(
        &state.workspace,
        id,
        lease.number,
        code,
        "Local inference did not publish extractor candidates",
        disposition,
    )
    .await;
}

async fn publish(
    state: &AppState,
    id: &str,
    lease: &Lease,
    aggregate: Aggregate,
) -> Result<(), WorkspaceError> {
    let workspace = &state.workspace;
    let mut conn = workspace.pool().acquire().await?;
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    let bound: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM durable_job j JOIN workspace_meta w ON w.singleton=1 AND w.epoch=j.epoch
          WHERE j.id=? AND j.status='running' AND j.current_attempt=? AND j.lease_id=?
          AND j.total_deadline_at >= ?
          AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=j.profile_revision))",
    )
    .bind(id)
    .bind(lease.number)
    .bind(&lease.token)
    .bind(now_utc())
    .fetch_one(&mut *tx)
    .await?;
    if !bound {
        return Err(WorkspaceError::StaleRevision);
    }
    let occurrence_binding: Option<String> =
        sqlx::query_scalar("SELECT source_variant_id FROM source_occurrence WHERE id=?")
            .bind(&lease.input.occurrence_id)
            .fetch_optional(&mut *tx)
            .await?;
    if occurrence_binding.as_deref() != Some(lease.input.source_variant_id.as_str()) {
        return Err(WorkspaceError::StaleRevision);
    }
    let raw_text: String = sqlx::query_scalar("SELECT raw_text FROM source_variant WHERE id=?")
        .bind(&lease.input.source_variant_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(WorkspaceError::NotFound)?;
    let raw_sha256: String = sqlx::query_scalar("SELECT raw_sha256 FROM source_variant WHERE id=?")
        .bind(&lease.input.source_variant_id)
        .fetch_one(&mut *tx)
        .await?;
    let current_dependencies = application::extract::dependency_digest(
        &lease.input.occurrence_id,
        &lease.input.source_variant_id,
        &raw_sha256,
    )?;
    if current_dependencies != lease.dependencies {
        return Err(WorkspaceError::StaleRevision);
    }
    let now = now_utc();
    let batch_id = new_id(id);
    sqlx::query(
        "INSERT INTO proposal_batch(id,source_variant_id,job_id,source_occurrence_id,status,created_at) VALUES(?,?,?,?,'pending',?)",
    )
    .bind(&batch_id)
    .bind(&lease.input.source_variant_id)
    .bind(id)
    .bind(&lease.input.occurrence_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    // One index over the re-read source for every staged candidate; the
    // re-read itself stays because it fences freshness under this writer.
    let source_index = parley_domain::evidence::SourceIndex::new(&raw_text);
    let mut staged_count = 0usize;
    let mut dropped = aggregate.dropped_items as usize;
    for candidate in &aggregate.candidates {
        let matched = match source_index.verify(&candidate.quote, None) {
            Ok(value) => value,
            Err(_) => {
                dropped += 1;
                continue;
            }
        };
        let item_id = new_id(&format!("{id}:{}:{}", candidate.kind, candidate.quote));
        let data = json!({
            "kind":candidate.kind,
            "statement":candidate.statement,
            "quote":candidate.quote,
            "mention_name":candidate.mention_name,
            "mention_kind":candidate.mention_kind,
            "source_start":matched.source_start,
            "source_end":matched.source_end,
            "normalized_start":matched.normalized_start,
            "normalized_end":matched.normalized_end,
            "normalization_version":parley_domain::evidence::NORMALIZATION_VERSION,
            "injection_suspected":aggregate.injection_suspected,
        });
        sqlx::query(
            "INSERT INTO proposal_item(id,batch_id,proposal_kind,candidate_json,occurrence_id,status) VALUES(?,?,?,?,?,'pending')",
        )
        .bind(&item_id)
        .bind(&batch_id)
        .bind(&candidate.kind)
        .bind(data.to_string())
        .bind(&lease.input.occurrence_id)
        .execute(&mut *tx)
        .await?;
        staged_count += 1;
    }
    if staged_count == 0 {
        sqlx::query("UPDATE proposal_batch SET status='applied' WHERE id=?")
            .bind(&batch_id)
            .execute(&mut *tx)
            .await?;
    }
    let result = json!({
        "proposal_batch_id":batch_id,
        "candidate_count":staged_count,
        "dropped_items":dropped,
        "injection_suspected":aggregate.injection_suspected,
        "status":"owner_review_required",
    });
    sqlx::query("UPDATE inference_record SET model_digest=?,metadata_class='observed' WHERE id=?")
        .bind(&lease.profile.model_digest)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    jobs::complete_role_result_in_tx(
        &mut tx,
        id,
        lease.number,
        &lease.input_sha256,
        &result,
        aggregate.tokens_in,
        aggregate.tokens_out,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
