//! Chat role runtime (AR-001 AR04/AR08): Interviewer, Briefer, Composer and
//! Linter.
//!
//! Every chat role is one packaged structured worker execution under a frozen
//! profile, the host owns admission, schema digests, conditional validation
//! and the durable result commit. No module in this file touches a model
//! transport; the worker executes against the private relay and the host
//! validates the structured result before any row is committed.

use super::{
    protocol::{Completion, Execute, Profile, Role, Usage},
    supervisor::Supervisor,
    template_builder::{installed_supervisor, tokenizer_path},
};
use crate::{
    AppState, Workspace, WorkspaceError, digest, jobs, new_id, now_utc, now_utc_offset, random_hex,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{net::SocketAddr, path::PathBuf, time::Duration};
use tokio::sync::watch;

#[derive(sqlx::FromRow)]
struct Frozen {
    status: String,
    current_attempt: i64,
    max_attempts: i64,
    queue_deadline_at: String,
    total_deadline_at: Option<String>,
    profile_revision: Option<i64>,
    input_sha256: String,
    input_json: String,
}

struct Lease {
    number: i64,
    token: String,
    input_sha256: String,
    input: Value,
    profile: Profile,
    runner_digest: String,
    endpoint: SocketAddr,
    remaining_ms: u32,
    role: String,
}

enum Claim {
    Missing,
    Failure,
    Ready(Box<Lease>),
}

fn role_enum(role: &str) -> Option<Role> {
    match role {
        "interviewer" => Some(Role::Interviewer),
        "briefer" => Some(Role::Briefer),
        "composer" => Some(Role::Composer),
        "linter" => Some(Role::Linter),
        _ => None,
    }
}

fn role_schema(role: Role) -> &'static [u8] {
    match role {
        Role::Interviewer => crate::inference::interviewer_schema(),
        Role::Briefer => crate::inference::briefer_schema(),
        Role::Composer => crate::inference::composer_schema(),
        Role::Linter => crate::inference::linter_schema(),
        _ => unreachable!("chat role runtime only hosts registered chat roles"),
    }
}

async fn claim(state: &AppState, id: &str) -> Result<Claim, WorkspaceError> {
    let workspace = &state.workspace;
    let mut tx = workspace.pool().begin_with("BEGIN IMMEDIATE").await?;
    let frozen: Option<Frozen> = sqlx::query_as(
        "SELECT status,current_attempt,max_attempts,queue_deadline_at,total_deadline_at,
                profile_revision,input_sha256,input_json
         FROM durable_job WHERE id=? AND epoch=?",
    )
    .bind(id)
    .bind(workspace.epoch())
    .fetch_optional(&mut *tx)
    .await?;
    let Some(frozen) = frozen.filter(|row| row.status == "queued") else {
        return Ok(Claim::Missing);
    };
    let role = {
        let role: Option<String> = sqlx::query_scalar("SELECT role FROM durable_job WHERE id=?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        role.unwrap_or_default()
    };
    let now = now_utc();
    let total_deadline = frozen
        .total_deadline_at
        .unwrap_or_else(|| now_utc_offset(360));
    let Some(revision) = frozen.profile_revision else {
        sqlx::query("UPDATE durable_job SET status='failed',last_error_code='CAPABILITY_UNQUALIFIED',completed_at=?,updated_at=?,lease_id=NULL WHERE id=? AND status='queued'")
            .bind(&now).bind(&now).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(Claim::Failure);
    };
    let row: Option<super::coach::ProfileRow> = sqlx::query_as(
        "SELECT p.endpoint,p.runner_build_digest,p.provider_version,p.chat_model,p.chat_model_digest,
                p.tokenizer_digest,p.recipe_json,p.context_tokens,p.output_tokens,
                p.embed_model,p.embed_model_digest
         FROM inference_profile p WHERE revision=?
           AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=p.revision)
           AND EXISTS(SELECT 1 FROM inference_qualification q WHERE q.profile_revision=p.revision
                      AND q.mode='structured_role' AND q.status='ready' AND q.checked_at >= ?)",
    )
    .bind(revision)
    .bind(now_utc_offset(-300))
    .fetch_optional(&mut *tx)
    .await?;
    let fields = row.and_then(|row| super::coach::profile_fields(revision, row));
    let failure = if frozen.current_attempt == 0 && frozen.queue_deadline_at < now {
        Some("QUEUE_EXPIRED")
    } else if total_deadline <= now || frozen.current_attempt >= frozen.max_attempts {
        Some("ATTEMPT_TIMEOUT")
    } else if role_enum(&role).is_none() {
        Some("ROLE_UNSUPPORTED")
    } else if fields.is_none() {
        Some("CAPABILITY_UNQUALIFIED")
    } else {
        None
    };
    if let Some(code) = failure {
        sqlx::query("UPDATE durable_job SET status='failed',last_error_code=?,completed_at=?,updated_at=?,lease_id=NULL WHERE id=?")
            .bind(code).bind(&now).bind(&now).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(Claim::Failure);
    }
    let input: Value = serde_json::from_str(&frozen.input_json).unwrap_or_default();
    if input.get("prompt").and_then(Value::as_str).is_none()
        || input
            .get("prompt_version")
            .and_then(Value::as_str)
            .is_none()
    {
        sqlx::query("UPDATE durable_job SET status='failed',last_error_code='RESULT_INVALID',completed_at=?,updated_at=?,lease_id=NULL WHERE id=?")
            .bind(&now).bind(&now).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(Claim::Failure);
    }
    let input_json = frozen.input_json;
    let (endpoint, profile, runner_digest) = fields.expect("validated frozen inference profile");
    let number = frozen.current_attempt + 1;
    let token = random_hex(32);
    let process_nonce = crate::inference::dispatch::process_nonce();
    sqlx::query("UPDATE durable_job SET status='running',current_attempt=?,lease_id=?,process_nonce=?,total_deadline_at=?,updated_at=? WHERE id=?")
        .bind(number).bind(&token).bind(process_nonce).bind(&total_deadline).bind(&now).bind(id)
        .execute(&mut *tx).await?;
    let prompt_digest = digest(input["prompt"].as_str().unwrap_or_default().as_bytes());
    let schema_digest = digest(role_schema(role_enum(&role).expect("validated role")));
    sqlx::query(
        "INSERT INTO job_attempt(job_id,attempt_no,status,model,model_digest,provider_snapshot_json,
             prompt_id,prompt_sha256,input_sha256,input_json,started_at)
         VALUES(?,?,'running',?,?,?,?,?,?,?,?)",
    )
    .bind(id).bind(number).bind(&profile.model_tag).bind(&profile.model_digest)
    .bind(serde_json::to_string(&profile).map_err(|_| WorkspaceError::Invalid("Invalid profile".into()))?)
    .bind(input["prompt_version"].as_str().unwrap_or_default())
    .bind(&prompt_digest).bind(&frozen.input_sha256)
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
    // The queued→running transition is durable now; tell subscribers the
    // exact job id so activity surfaces re-read GET /jobs/{id} while the
    // worker executes instead of polling (L1). Terminal states are covered
    // by the post-process hint in jobs::process.
    let _ = state
        .notify
        .send(crate::CommandSignal::JobChanged(id.to_owned()));
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
        input_sha256: frozen.input_sha256,
        input,
        profile,
        runner_digest,
        endpoint,
        remaining_ms,
        role,
    })))
}

pub(crate) async fn run(state: &AppState, id: &str) {
    let lease = match claim(state, id).await {
        Ok(Claim::Ready(lease)) => lease,
        Ok(Claim::Failure) => return,
        Ok(Claim::Missing) | Err(_) => return,
    };
    match execute(state, id, &lease).await {
        Ok((candidate, usage)) => match publish(state, id, &lease, candidate, usage).await {
            Ok(()) => {}
            Err(error) => {
                let message = error.to_string();
                let (code, disposition) = if message.contains("STALE:") {
                    ("STALE_INPUT", "failed")
                } else if message.contains("SCHEMA:") && lease.number < 3 {
                    ("OUTPUT_INVALID", "retry")
                } else if message.contains("SCHEMA:") {
                    ("OUTPUT_INVALID", "invalid")
                } else {
                    ("ROLE_VALIDATION", "failed")
                };
                fail(state, id, &lease, code, disposition).await;
            }
        },
        Err((code, disposition)) => fail(state, id, &lease, code, disposition).await,
    }
}

async fn execute(
    state: &AppState,
    id: &str,
    lease: &Lease,
) -> Result<(Value, Usage), (&'static str, &'static str)> {
    if lease.remaining_ms < 1 {
        return Err(("ATTEMPT_TIMEOUT", "failed"));
    }
    let supervisor: Supervisor = match installed_supervisor() {
        Ok(supervisor) => supervisor,
        Err(code) => return Err((code, "failed")),
    };
    if supervisor.manifest_digest() != lease.runner_digest {
        return Err(("PROFILE_CHANGED", "failed"));
    }
    let tokenizer: PathBuf = match tokenizer_path(state, &lease.profile) {
        Ok(path) => path,
        Err(code) => return Err((code, "failed")),
    };
    let execution = new_id("chat-role-worker");
    let mut input = lease.input.clone();
    input.as_object_mut().map(|map| map.remove("prompt"));
    let role = role_enum(&lease.role).expect("validated chat role");
    let request = Execute::StructuredRole {
        profile: lease.profile.clone(),
        deadline_ms: lease.remaining_ms,
        role,
        prompt_version: lease.input["prompt_version"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        schema_digest: digest(role_schema(role)),
        system: lease.input["prompt"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
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
    }.map_err(|error| (error.code(), "failed"))?;
    match result {
        Completion::StructuredRole {
            candidate, usage, ..
        } => Ok((candidate, usage)),
        Completion::Probe { .. } => Err(("WORKER_PROTOCOL", "failed")),
        Completion::Delegated { .. } => Err(("WORKER_PROTOCOL", "failed")),
        Completion::Embed { .. } => Err(("WORKER_PROTOCOL", "failed")),
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
    match lease.role.as_str() {
        "interviewer" => {
            process_interviewer_result(
                state,
                id,
                lease,
                &candidate,
                usage.input_tokens,
                usage.output_tokens,
            )
            .await
        }
        "briefer" => {
            process_briefer_result(
                state,
                id,
                lease,
                &candidate,
                usage.input_tokens,
                usage.output_tokens,
            )
            .await
        }
        "composer" => {
            process_composer_result(
                state,
                id,
                lease,
                &candidate,
                usage.input_tokens,
                usage.output_tokens,
            )
            .await
        }
        "linter" => {
            process_linter_result(
                state,
                id,
                lease,
                &candidate,
                usage.input_tokens,
                usage.output_tokens,
            )
            .await
        }
        _ => Err(WorkspaceError::Invalid("ROLE_UNSUPPORTED".into())),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InterviewerOutput {
    next_question: String,
    target_field: String,
    extracted: std::collections::BTreeMap<String, ExtractedValue>,
    #[allow(dead_code)]
    ready_to_save: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractedValue {
    value: Value,
    owner_quote: String,
    owner_turn_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrieferOutput {
    #[serde(default)]
    response: Option<String>,
    items: Vec<BriefCandidate>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BriefCandidate {
    kind: String,
    text: String,
    detail: BriefDetail,
    tier: u8,
    quote: Option<String>,
    check_suggestion: Option<String>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct BriefDetail {
    #[serde(default)]
    term: Option<String>,
    plain_meaning: String,
    use_or_avoid: String,
    applies_to: String,
}

fn valid_brief_kind(kind: &str) -> bool {
    matches!(
        kind,
        "jargon" | "norm" | "value" | "landmine" | "stakeholder" | "interest_hypothesis"
    )
}

fn valid_detail(kind: &str, detail: &BriefDetail) -> bool {
    matches!(
        detail.use_or_avoid.as_str(),
        "use" | "avoid" | "owner_decides"
    ) && matches!(detail.applies_to.as_str(), "org" | "segment" | "role")
        && (!detail.plain_meaning.trim().is_empty() || kind != "jargon")
        && (kind != "jargon"
            || detail
                .term
                .as_deref()
                .is_some_and(|term| !term.trim().is_empty() && term.chars().count() <= 120))
}

fn mentions_sensitive_topic(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "health",
        "disability",
        "family",
        "sexual orientation",
        "religion",
        "political",
        "race",
        "ethnicity",
        "immigration",
        "union membership",
        "financial hardship",
        "home address",
        "home location",
        "minor",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

async fn process_interviewer_result(
    state: &AppState,
    job_id: &str,
    lease: &Lease,
    candidate: &Value,
    tokens_in: u32,
    tokens_out: u32,
) -> Result<(), WorkspaceError> {
    let output: InterviewerOutput = serde_json::from_value(candidate.clone()).map_err(|error| {
        WorkspaceError::Invalid(format!("SCHEMA: invalid Interviewer output: {error}"))
    })?;
    if output.next_question.trim().is_empty()
        || output.next_question.chars().count() > 600
        || output.extracted.len() > 24
    {
        return Err(WorkspaceError::Invalid(
            "SCHEMA: Interviewer output exceeds field bounds".into(),
        ));
    }
    let input = &lease.input;
    let chat_id = input["chat_id"]
        .as_str()
        .ok_or_else(|| WorkspaceError::Invalid("ROLE_INPUT: missing chat id".into()))?;
    let kind = input["asset_kind"]
        .as_str()
        .ok_or_else(|| WorkspaceError::Invalid("ROLE_INPUT: missing asset kind".into()))?;
    let turns = input["turns"]
        .as_array()
        .ok_or_else(|| WorkspaceError::Invalid("ROLE_INPUT: missing turns".into()))?;
    let allowed: std::collections::BTreeSet<&str> = crate::assistant::allowed_fields(kind)
        .ok_or_else(|| WorkspaceError::Invalid("ROLE_INPUT: unsupported asset kind".into()))?;
    let mut fields = input["current_fields"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut quotes = input["current_quotes"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for (field, extracted) in output.extracted {
        if !allowed.contains(field.as_str()) {
            return Err(WorkspaceError::Invalid(format!(
                "ROLE_VALIDATION: field {field:?} is not part of the {kind} schema"
            )));
        }
        let owner_turn = turns
            .iter()
            .find(|turn| {
                turn["id"].as_str() == Some(extracted.owner_turn_id.as_str())
                    && turn["speaker"].as_str() == Some("owner")
            })
            .ok_or_else(|| {
                WorkspaceError::Invalid(format!(
                    "ROLE_VALIDATION: extracted field {field:?} cites no owner turn"
                ))
            })?;
        let owner_text = owner_turn["text"].as_str().ok_or_else(|| {
            WorkspaceError::Invalid("ROLE_VALIDATION: owner turn has no text".into())
        })?;
        if parley_domain::evidence::verify_quote(owner_text, &extracted.owner_quote, None).is_err()
        {
            return Err(WorkspaceError::Invalid(format!(
                "ROLE_VALIDATION: owner quote for {field:?} is not an exact verified span"
            )));
        }
        fields.insert(field.clone(), extracted.value);
        quotes.insert(
            field,
            json!({"turn_id":extracted.owner_turn_id,"quote":extracted.owner_quote}),
        );
    }
    let missing = crate::assistant::missing_fields(kind, &Value::Object(fields.clone()));
    let ready = missing.is_empty();
    let chat_revision = input["chat_revision"]
        .as_i64()
        .ok_or_else(|| WorkspaceError::Invalid("ROLE_INPUT: missing chat revision".into()))?;
    let now = crate::now_utc();
    let candidate_id = crate::new_id(&format!("{job_id}:candidate"));
    let fields_value = Value::Object(fields);
    let quotes_value = Value::Object(quotes);
    let missing_value = serde_json::to_value(&missing)
        .map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
    let result = json!({"prompt_id":lease.input["prompt_version"],"next_question":output.next_question,"target_field":output.target_field,"candidate_id":candidate_id,"fields":fields_value,"owner_quotes":quotes_value,"missing_fields":missing,"ready_to_save":ready});
    let workspace = &state.workspace;
    let mut tx = workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT revision FROM agent_chat WHERE id=? AND role='interviewer' AND scope_kind='owner'",
    )
    .bind(chat_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
    if current != Some(chat_revision) {
        tx.rollback()
            .await
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        return Err(WorkspaceError::Invalid(
            "STALE: interviewer chat changed".into(),
        ));
    }
    let turn_no: i64 =
        sqlx::query_scalar("SELECT coalesce(max(turn_no),0)+1 FROM agent_turn WHERE chat_id=?")
            .bind(chat_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
    let turn_id = crate::new_id(&format!("{chat_id}:{turn_no}"));
    sqlx::query("INSERT INTO agent_turn(id,chat_id,turn_no,speaker,text,job_id,created_at) VALUES(?,?,?,'agent',?,?,?)")
        .bind(&turn_id).bind(chat_id).bind(turn_no).bind(&output.next_question).bind(job_id).bind(&now).execute(&mut *tx).await.map_err(|e|WorkspaceError::Invalid(e.to_string()))?;
    sqlx::query("INSERT INTO interviewer_candidate(id,chat_id,job_id,asset_kind,fields_json,owner_quotes_json,missing_fields_json,state,created_at,updated_at) VALUES(?,?,?,?,?,?,?,'proposed',?,?)")
        .bind(&candidate_id).bind(chat_id).bind(job_id).bind(kind).bind(fields_value.to_string()).bind(quotes_value.to_string()).bind(missing_value.to_string()).bind(&now).bind(&now).execute(&mut *tx).await.map_err(|e|WorkspaceError::Invalid(e.to_string()))?;
    sqlx::query("UPDATE agent_chat SET revision=revision+1,updated_at=? WHERE id=? AND revision=?")
        .bind(&now)
        .bind(chat_id)
        .bind(chat_revision)
        .execute(&mut *tx)
        .await
        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'agent_chat',?,?, 'interviewer.candidate_proposed',?)")
        .bind(crate::new_id(job_id)).bind(&now).bind(chat_id).bind(chat_revision+1).bind(json!({"candidate_id":candidate_id,"job_id":job_id,"ready":ready}).to_string()).execute(&mut *tx).await.map_err(|e|WorkspaceError::Invalid(e.to_string()))?;
    jobs::complete_role_result_in_tx(
        &mut tx,
        job_id,
        lease.number,
        &lease.input_sha256,
        &result,
        tokens_in,
        tokens_out,
    )
    .await
    .map_err(|e| WorkspaceError::Invalid(format!("STALE: {e}")))?;
    tx.commit()
        .await
        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
    Ok(())
}

async fn process_briefer_result(
    state: &AppState,
    job_id: &str,
    lease: &Lease,
    candidate: &Value,
    tokens_in: u32,
    tokens_out: u32,
) -> Result<(), WorkspaceError> {
    let output: BrieferOutput = serde_json::from_value(candidate.clone()).map_err(|error| {
        WorkspaceError::Invalid(format!("SCHEMA: invalid Briefer output: {error}"))
    })?;
    if output.items.len() > 80 {
        return Err(WorkspaceError::Invalid(
            "SCHEMA: brief contains too many items".into(),
        ));
    }
    let chat_response = output.response;
    let input = &lease.input;
    let has_chat = input["chat_id"].as_str().is_some();
    if has_chat
        && chat_response
            .as_deref()
            .is_none_or(|value| value.trim().is_empty() || value.chars().count() > 1600)
    {
        return Err(WorkspaceError::Invalid(
            "ROLE_VALIDATION: scoped Briefer chat needs a bounded response".into(),
        ));
    }
    if !has_chat
        && chat_response
            .as_deref()
            .is_some_and(|value| value.chars().count() > 1600)
    {
        return Err(WorkspaceError::Invalid(
            "ROLE_VALIDATION: brief response exceeds its bound".into(),
        ));
    }
    let scope_kind = input["scope_kind"]
        .as_str()
        .ok_or_else(|| WorkspaceError::Invalid("ROLE_INPUT: missing scope kind".into()))?;
    let scope_id = input["scope_id"]
        .as_str()
        .ok_or_else(|| WorkspaceError::Invalid("ROLE_INPUT: missing scope id".into()))?;
    let evidence = input["evidence"].as_array().cloned().unwrap_or_default();
    let now = crate::now_utc();
    let mut validated = Vec::<(BriefCandidate, Option<(String, String, i64, i64)>)>::new();
    for item in output.items {
        if item.text.trim().is_empty()
            || item.text.chars().count() > 1000
            || !valid_brief_kind(&item.kind)
            || !valid_detail(&item.kind, &item.detail)
        {
            return Err(WorkspaceError::Invalid(
                "ROLE_VALIDATION: brief item failed deterministic field validation".into(),
            ));
        }
        let evidence_span = if item.tier == 1 {
            let quote = item
                .quote
                .as_deref()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    WorkspaceError::Invalid("ROLE_VALIDATION: tier-1 item lacks a quote".into())
                })?;
            let mut matches = Vec::new();
            for source in &evidence {
                let Some(text) = source["text"].as_str() else {
                    continue;
                };
                if let Ok(span) = parley_domain::evidence::verify_quote(text, quote, None) {
                    matches.push((
                        source["occurrence_id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                        source["variant_id"].as_str().unwrap_or_default().to_owned(),
                        span.source_start as i64,
                        span.source_end as i64,
                    ));
                }
            }
            if matches.len() != 1 {
                return Err(WorkspaceError::Invalid(
                    "ROLE_VALIDATION: tier-1 quote must resolve to one scoped source occurrence"
                        .into(),
                ));
            }
            Some(matches.remove(0))
        } else if item.tier == 2 {
            if item.check_suggestion.as_deref().is_none_or(str::is_empty)
                || item.quote.is_some()
                || item.text.contains('@')
            {
                return Err(WorkspaceError::Invalid("ROLE_VALIDATION: tier-2 brief item needs a verification suggestion and may not claim quoted or named evidence".into()));
            }
            if mentions_sensitive_topic(&item.text) {
                return Err(WorkspaceError::Invalid(
                    "ROLE_VALIDATION: sensitive topic is prohibited in a brief".into(),
                ));
            }
            if matches!(item.kind.as_str(), "norm" | "value" | "landmine")
                && scope_kind != "segment"
            {
                return Err(WorkspaceError::Invalid(
                    "ROLE_VALIDATION: unverified cultural tendencies must be scoped to a segment"
                        .into(),
                ));
            }
            None
        } else {
            return Err(WorkspaceError::Invalid(
                "ROLE_VALIDATION: the model cannot assign tier 3".into(),
            ));
        };
        if item.tier == 1 && mentions_sensitive_topic(&item.text) {
            return Err(WorkspaceError::Invalid(
                "ROLE_VALIDATION: sensitive topic is prohibited in a brief".into(),
            ));
        }
        validated.push((item, evidence_span));
    }
    let result = json!({"prompt_id":lease.input["prompt_version"],"scope_kind":scope_kind,"scope_id":scope_id,"item_count":validated.len(),"response":chat_response});
    let workspace = &state.workspace;
    let mut tx = workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
    let current:Option<(String,String,i64)>=sqlx::query_as("SELECT scope_kind,scope_id,scope_revision FROM durable_job WHERE id=? AND status='running' AND current_attempt=?").bind(job_id).bind(lease.number).fetch_optional(&mut *tx).await.map_err(|e|WorkspaceError::Invalid(e.to_string()))?;
    let expected_revision = input["scope_revision"]
        .as_i64()
        .ok_or_else(|| WorkspaceError::Invalid("ROLE_INPUT: missing scope revision".into()))?;
    if current.as_ref().is_none_or(|(kind, id, revision)| {
        kind != scope_kind || id != scope_id || *revision != expected_revision
    }) {
        tx.rollback()
            .await
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        return Err(WorkspaceError::Invalid(
            "STALE: brief job is no longer current".into(),
        ));
    }
    if jobs::scoped_job_fence(&mut tx, job_id).await.is_err() {
        tx.rollback()
            .await
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        return Err(WorkspaceError::Invalid(
            "STALE: the initiating run revoked or the epoch changed".into(),
        ));
    }
    if let Some(chat_id) = input["chat_id"].as_str() {
        let expected_chat_revision = input["chat_revision"]
            .as_i64()
            .ok_or_else(|| WorkspaceError::Invalid("ROLE_INPUT: missing chat revision".into()))?;
        let chat: Option<(i64, String, String, Option<String>)> =
            sqlx::query_as("SELECT revision,role,scope_kind,scope_id FROM agent_chat WHERE id=?")
                .bind(chat_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        if chat
            .as_ref()
            .is_none_or(|(revision, role, chat_scope, chat_scope_id)| {
                *revision != expected_chat_revision
                    || role != "briefer"
                    || chat_scope != scope_kind
                    || chat_scope_id.as_deref() != Some(scope_id)
            })
        {
            tx.rollback()
                .await
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            return Err(WorkspaceError::Invalid(
                "STALE: Briefer chat changed or its scope no longer matches".into(),
            ));
        }
        let turn_no: i64 =
            sqlx::query_scalar("SELECT coalesce(max(turn_no),0)+1 FROM agent_turn WHERE chat_id=?")
                .bind(chat_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let turn_id = crate::new_id(&format!("{chat_id}:{turn_no}"));
        sqlx::query("INSERT INTO agent_turn(id,chat_id,turn_no,speaker,text,job_id,created_at) VALUES(?,?,?,'agent',?,?,?)")
            .bind(&turn_id).bind(chat_id).bind(turn_no).bind(chat_response.as_deref().unwrap_or_default()).bind(job_id).bind(&now).execute(&mut *tx).await.map_err(|e|WorkspaceError::Invalid(e.to_string()))?;
        sqlx::query(
            "UPDATE agent_chat SET revision=revision+1,updated_at=? WHERE id=? AND revision=?",
        )
        .bind(&now)
        .bind(chat_id)
        .bind(expected_chat_revision)
        .execute(&mut *tx)
        .await
        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'agent_chat',?,?, 'briefer.reply',?)")
            .bind(crate::new_id(job_id)).bind(&now).bind(chat_id).bind(expected_chat_revision+1).bind(json!({"job_id":job_id,"scope_kind":scope_kind,"scope_id":scope_id}).to_string()).execute(&mut *tx).await.map_err(|e|WorkspaceError::Invalid(e.to_string()))?;
    }
    if scope_kind == "party" {
        let entity_revision: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM entity WHERE id=? AND kind IN ('person','org')",
        )
        .bind(scope_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        if entity_revision != Some(expected_revision) {
            tx.rollback()
                .await
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            return Err(WorkspaceError::Invalid(
                "STALE: party changed during brief generation".into(),
            ));
        }
    } else {
        let segment_revision: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM segment WHERE id=?")
                .bind(scope_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        if segment_revision != Some(expected_revision) {
            tx.rollback()
                .await
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            return Err(WorkspaceError::Invalid(
                "STALE: segment changed during brief generation".into(),
            ));
        }
    }
    let mut inserted_count = 0usize;
    for (item, span) in validated {
        let duplicate:Option<String>=sqlx::query_scalar("SELECT id FROM brief_item WHERE scope_kind=? AND scope_id=? AND kind=? AND lower(text)=lower(?) AND status='dismissed' LIMIT 1")
            .bind(scope_kind).bind(scope_id).bind(&item.kind).bind(&item.text).fetch_optional(&mut *tx).await.map_err(|e|WorkspaceError::Invalid(e.to_string()))?;
        if duplicate.is_some() {
            continue;
        }
        let id = crate::new_id(&format!("{job_id}:{}:{}", item.kind, item.text));
        let detail = serde_json::to_string(&item.detail)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let (occurrence, variant, start, end) = span
            .map(|(a, b, c, d)| (Some(a), Some(b), Some(c), Some(d)))
            .unwrap_or((None, None, None, None));
        if item.kind == "jargon" {
            let term = item.detail.term.as_deref().unwrap_or_default().trim();
            sqlx::query("INSERT OR IGNORE INTO fluency(term,status,revision,updated_at) VALUES(?,'unknown',1,?)")
                .bind(term).bind(&now).execute(&mut *tx).await.map_err(|e|WorkspaceError::Invalid(e.to_string()))?;
        }
        sqlx::query("INSERT INTO brief_item(id,scope_kind,scope_id,kind,text,detail_json,tier,source_occurrence_id,source_variant_id,quote,source_start,source_end,check_suggestion,speaker_kind,status,job_id,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,? ,?,'proposed',?,?,?)")
            .bind(&id).bind(scope_kind).bind(scope_id).bind(&item.kind).bind(&item.text).bind(detail).bind(item.tier as i64)
            .bind(&occurrence).bind(&variant).bind(&item.quote).bind(start).bind(end).bind(&item.check_suggestion)
            .bind(if item.tier==1 {"party"} else {"model_prior"}).bind(job_id).bind(&now).bind(&now)
            .execute(&mut *tx).await.map_err(|e|WorkspaceError::Invalid(e.to_string()))?;
        inserted_count += 1;
    }
    let event_payload = json!({
        "job_id": job_id,
        "scope_kind": scope_kind,
        "scope_id": scope_id,
        "items_proposed": inserted_count,
    });
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'brief',?,?, 'brief.items_proposed',?)")
        .bind(crate::new_id(job_id)).bind(&now).bind(scope_id).bind(1).bind(event_payload.to_string()).execute(&mut *tx).await.map_err(|e|WorkspaceError::Invalid(e.to_string()))?;
    jobs::complete_role_result_in_tx(
        &mut tx,
        job_id,
        lease.number,
        &lease.input_sha256,
        &result,
        tokens_in,
        tokens_out,
    )
    .await
    .map_err(|e| WorkspaceError::Invalid(format!("STALE: {e}")))?;
    tx.commit()
        .await
        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
    Ok(())
}

/// Shared fatal/retryable failure classification for the four chat roles. The
/// worker already enforces the closed schema; host validation failures carry a
/// stable code and a disposition that matches the pre-cut behavior.
enum RoleFailure {
    Code(&'static str),
    Stale,
    OutputInvalid,
}

impl RoleFailure {
    fn code(self) -> (&'static str, &'static str) {
        match self {
            Self::Code(code) => (code, "failed"),
            Self::Stale => ("STALE_INPUT", "failed"),
            Self::OutputInvalid => ("OUTPUT_INVALID", "retry"),
        }
    }
}

async fn process_composer_result(
    state: &AppState,
    job_id: &str,
    lease: &Lease,
    candidate: &Value,
    tokens_in: u32,
    tokens_out: u32,
) -> Result<(), WorkspaceError> {
    let workspace = &state.workspace;
    let result = match validate_composer_result(workspace, job_id, lease, candidate).await {
        Ok(result) => result,
        Err(failure) => {
            let (code, disposition) = failure.code();
            if code == "OUTPUT_INVALID" && lease.number < 3 {
                fail(state, job_id, lease, code, "retry").await;
            } else {
                fail(state, job_id, lease, code, disposition).await;
            }
            return Err(WorkspaceError::Invalid(code.into()));
        }
    };
    let mut tx = workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
    jobs::complete_role_result_in_tx(
        &mut tx,
        job_id,
        lease.number,
        &lease.input_sha256,
        &result,
        tokens_in,
        tokens_out,
    )
    .await
    .map_err(|e| WorkspaceError::Invalid(format!("STALE: {e}")))?;
    tx.commit()
        .await
        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
    Ok(())
}

async fn process_linter_result(
    state: &AppState,
    job_id: &str,
    lease: &Lease,
    candidate: &Value,
    tokens_in: u32,
    tokens_out: u32,
) -> Result<(), WorkspaceError> {
    let workspace = &state.workspace;
    let result = match validate_linter_result(workspace, job_id, lease, candidate).await {
        Ok(result) => result,
        Err(failure) => {
            let (code, disposition) = failure.code();
            if code == "OUTPUT_INVALID" && lease.number < 3 {
                fail(state, job_id, lease, code, "retry").await;
            } else {
                fail(state, job_id, lease, code, disposition).await;
            }
            return Err(WorkspaceError::Invalid(code.into()));
        }
    };
    let mut tx = workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
    jobs::complete_role_result_in_tx(
        &mut tx,
        job_id,
        lease.number,
        &lease.input_sha256,
        &result,
        tokens_in,
        tokens_out,
    )
    .await
    .map_err(|e| WorkspaceError::Invalid(format!("STALE: {e}")))?;
    tx.commit()
        .await
        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposerResult {
    body: String,
    sentences: Vec<ComposerSentence>,
    slot_fills: std::collections::BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposerSentence {
    text: String,
    claims: Vec<ComposerClaim>,
    rules: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposerClaim {
    #[serde(rename = "type")]
    kind: String,
    r#ref: ComposerReference,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposerReference {
    kind: String,
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinterResult {
    findings: Vec<LinterFinding>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinterFinding {
    start_byte: usize,
    end_byte: usize,
    issue: String,
    suggested_rewrite: String,
}

async fn validate_composer_result(
    workspace: &Workspace,
    job_id: &str,
    lease: &Lease,
    candidate: &Value,
) -> Result<Value, RoleFailure> {
    let input = &lease.input;
    let parsed: ComposerResult = serde_json::from_value(candidate.clone())
        .map_err(|_| RoleFailure::Code("OUTPUT_INVALID"))?;
    let fills = input
        .get("slot_fills")
        .and_then(Value::as_array)
        .ok_or(RoleFailure::Code("COMPOSER_INPUT_INVALID"))?;
    let mut expected = std::collections::BTreeMap::new();
    let mut reference_ids = std::collections::BTreeSet::new();
    for fill in fills {
        let slot = fill
            .get("slot")
            .and_then(Value::as_str)
            .ok_or(RoleFailure::Code("COMPOSER_INPUT_INVALID"))?;
        let value = fill
            .get("value")
            .and_then(Value::as_str)
            .ok_or(RoleFailure::Code("COMPOSER_INPUT_INVALID"))?;
        expected.insert(slot.to_owned(), value.to_owned());
        if let Some(id) = fill.get("resource_id").and_then(Value::as_str) {
            reference_ids.insert(id.to_owned());
        }
    }
    if parsed.body.trim().is_empty()
        || parsed.body.len() > 20_000
        || parsed.slot_fills != expected
        || expected.values().any(|value| !parsed.body.contains(value))
    {
        return Err(RoleFailure::OutputInvalid);
    }
    let sentence_text = parsed
        .sentences
        .iter()
        .map(|sentence| sentence.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if sentence_text.trim().is_empty() {
        return Err(RoleFailure::OutputInvalid);
    }
    for sentence in &parsed.sentences {
        if !parsed.body.contains(&sentence.text) {
            return Err(RoleFailure::OutputInvalid);
        }
        for claim in &sentence.claims {
            if !matches!(
                claim.kind.as_str(),
                "self_fact" | "other_fact" | "offer" | "question" | "ask" | "generic"
            ) {
                return Err(RoleFailure::OutputInvalid);
            }
            if !matches!(
                claim.r#ref.kind.as_str(),
                "proof"
                    | "gift"
                    | "artifact"
                    | "hook"
                    | "interest"
                    | "identity"
                    | "capacity"
                    | "ladder"
            ) {
                return Err(RoleFailure::OutputInvalid);
            }
            if !reference_ids.contains(&claim.r#ref.id)
                && claim.r#ref.kind != "interest"
                && claim.r#ref.kind != "ladder"
            {
                return Err(RoleFailure::OutputInvalid);
            }
            if claim.kind == "other_fact"
                && input
                    .get("tier2_interest_ids")
                    .and_then(Value::as_array)
                    .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(&claim.r#ref.id)))
            {
                return Err(RoleFailure::OutputInvalid);
            }
        }
    }
    let protected = expected.values().cloned().collect::<Vec<_>>();
    if protected.iter().any(|token| !parsed.body.contains(token)) {
        return Err(RoleFailure::OutputInvalid);
    }
    let sentences=parsed.sentences.into_iter().map(|sentence|json!({"text":sentence.text,"claims":sentence.claims.into_iter().map(|claim|json!({"type":claim.kind,"ref":{"kind":claim.r#ref.kind,"id":claim.r#ref.id}})).collect::<Vec<_>>(),"rules":sentence.rules})).collect::<Vec<_>>();
    // Recheck frozen scope and resource revisions under the writer order.
    let mut tx = workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|_| RoleFailure::Code("PERSISTENCE_FAILED"))?;
    if crate::writing_jobs::ensure_scope_current(&mut tx, job_id, input)
        .await
        .is_err()
    {
        tx.rollback()
            .await
            .map_err(|_| RoleFailure::Code("PERSISTENCE_FAILED"))?;
        return Err(RoleFailure::Stale);
    }
    if jobs::scoped_job_fence(&mut tx, job_id).await.is_err() {
        tx.rollback()
            .await
            .map_err(|_| RoleFailure::Code("PERSISTENCE_FAILED"))?;
        return Err(RoleFailure::Stale);
    }
    tx.rollback()
        .await
        .map_err(|_| RoleFailure::Code("PERSISTENCE_FAILED"))?;
    Ok(
        json!({"body":parsed.body,"sentences":sentences,"slot_fills":parsed.slot_fills,"status":"draft","owner_review_required":true,"released":false,"prompt_id":lease.input["prompt_version"]}),
    )
}

async fn validate_linter_result(
    workspace: &Workspace,
    job_id: &str,
    lease: &Lease,
    candidate: &Value,
) -> Result<Value, RoleFailure> {
    let input = &lease.input;
    let body = input
        .get("body")
        .and_then(Value::as_str)
        .ok_or(RoleFailure::Code("LINTER_INPUT_INVALID"))?;
    let parsed: LinterResult =
        serde_json::from_value(candidate.clone()).map_err(|_| RoleFailure::OutputInvalid)?;
    if parsed.findings.len() > 3 {
        return Err(RoleFailure::OutputInvalid);
    }
    let mut accepted = Vec::new();
    for finding in parsed.findings {
        if finding.start_byte > finding.end_byte
            || finding.end_byte > body.len()
            || !body.is_char_boundary(finding.start_byte)
            || !body.is_char_boundary(finding.end_byte)
            || finding.issue.trim().is_empty()
        {
            return Err(RoleFailure::OutputInvalid);
        }
        let original = &body[finding.start_byte..finding.end_byte];
        if !parley_domain::lint::preserves_protected_tokens(original, &finding.suggested_rewrite) {
            continue;
        }
        accepted.push(json!({"start_byte":finding.start_byte,"end_byte":finding.end_byte,"issue":finding.issue,"suggested_rewrite":finding.suggested_rewrite,"advisory":true}));
    }
    // An advisory result still must not be saved against a changed material.
    let mut tx = workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|_| RoleFailure::Code("PERSISTENCE_FAILED"))?;
    if crate::writing_jobs::ensure_scope_current(&mut tx, job_id, input)
        .await
        .is_err()
    {
        tx.rollback()
            .await
            .map_err(|_| RoleFailure::Code("PERSISTENCE_FAILED"))?;
        return Err(RoleFailure::Stale);
    }
    if jobs::scoped_job_fence(&mut tx, job_id).await.is_err() {
        tx.rollback()
            .await
            .map_err(|_| RoleFailure::Code("PERSISTENCE_FAILED"))?;
        return Err(RoleFailure::Stale);
    }
    tx.rollback()
        .await
        .map_err(|_| RoleFailure::Code("PERSISTENCE_FAILED"))?;
    Ok(
        json!({"findings":accepted,"deterministic_findings":input.get("deterministic_findings").cloned().unwrap_or(Value::Array(Vec::new())),"content_sha256":input.get("content_sha256"),"version":input.get("version"),"advisory_only":true}),
    )
}
