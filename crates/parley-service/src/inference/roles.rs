//! Interviewer and Briefer role catalogue and execution (AR-001 AR04).
//!
//! This module owns the typed registration for both chat roles — schemas,
//! reviewed prompt versions, validation caps — and their durable execution
//! path. One dispatch runner handles both roles; the HTTP handlers only admit
//! work through the typed application intake and never resolve prompts,
//! scopes or model transport themselves.
//!
//! Until AR08 removes the last direct provider client, execution drives the
//! host-owned attempt transport in `jobs` (claim, bounded chat, verified
//! result commit). No helper module touches a model transport.

use crate::{Workspace, jobs, jobs::Attempt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) async fn run(workspace: &Workspace, id: &str, role: &str) {
    if !matches!(role, "interviewer" | "briefer") {
        jobs::finish_job_error(
            workspace,
            id,
            "ROLE_UNSUPPORTED",
            "This role is not registered by the Interviewer and Briefer runtime",
            "failed",
        )
        .await;
        return;
    }
    let attempt = match jobs::claim_attempt(workspace, id).await {
        Ok(Some(attempt)) => attempt,
        _ => return,
    };
    if attempt.role != role {
        jobs::finish_attempt_error(
            workspace,
            id,
            attempt.number,
            "ROLE_CHANGED",
            "Durable job role does not match the worker dispatch",
            "failed",
        )
        .await;
        return;
    }
    if attempt.provider_config.model.trim().is_empty() {
        jobs::finish_attempt_error(
            workspace,
            id,
            attempt.number,
            "MODEL_UNAVAILABLE",
            "Choose an installed local model in Settings",
            "failed",
        )
        .await;
        return;
    }
    if attempt.model_digest.is_none() {
        jobs::finish_attempt_error(
            workspace,
            id,
            attempt.number,
            "MODEL_HEALTH_REQUIRED",
            "Check the installed local model in Settings before running this role",
            "failed",
        )
        .await;
        return;
    }
    let payload = match serde_json::from_str::<Value>(&attempt.input_json) {
        Ok(value) => value,
        Err(_) => {
            jobs::finish_attempt_error(
                workspace,
                id,
                attempt.number,
                "INPUT_INVALID",
                "Durable role input is invalid",
                "failed",
            )
            .await;
            return;
        }
    };
    let prompt_input = serde_json::to_string(&payload).unwrap_or_default();
    if prompt_input
        .len()
        .saturating_add(attempt.prompt.len())
        .saturating_add(attempt.schema.len())
        .saturating_add(attempt.provider_config.num_predict as usize)
        > attempt.provider_config.num_ctx as usize
    {
        jobs::finish_attempt_error(
            workspace,
            id,
            attempt.number,
            "CONTEXT_BUDGET",
            "Role input exceeds the configured context budget",
            "failed",
        )
        .await;
        return;
    }
    let response = match tokio::time::timeout(
        std::time::Duration::from_secs(120),
        jobs::ollama_chat(
            &attempt.provider_config,
            &format!("<DATA>\n{prompt_input}\n</DATA>"),
            &attempt.prompt,
            &attempt.schema,
            0.2,
        ),
    )
    .await
    {
        Err(_) => {
            jobs::finish_attempt_error(
                workspace,
                id,
                attempt.number,
                "ATTEMPT_TIMEOUT",
                "Agent role exceeded 120 seconds",
                "failed",
            )
            .await;
            return;
        }
        Ok(Err(error)) => {
            jobs::finish_attempt_error(
                workspace,
                id,
                attempt.number,
                "AGENT_FAILED",
                &redact_error(&error),
                "failed",
            )
            .await;
            return;
        }
        Ok(Ok(value)) => value,
    };
    let tokens_in = response
        .prompt_eval_count
        .unwrap_or_else(|| token_upper_bound(&prompt_input)) as u32;
    let tokens_out = response.eval_count.unwrap_or(0) as u32;
    let result: Result<Value, String> = match role {
        "interviewer" => {
            process_interviewer_result(
                workspace,
                id,
                &attempt,
                &payload,
                &response.message.content,
                tokens_in,
                tokens_out,
            )
            .await
        }
        "briefer" => {
            process_briefer_result(
                workspace,
                id,
                &attempt,
                &payload,
                &response.message.content,
                tokens_in,
                tokens_out,
            )
            .await
        }
        _ => unreachable!(),
    };
    if let Err(error) = result {
        let (code, message, disposition) = if error.starts_with("SCHEMA:") {
            if attempt.number < 3 {
                ("OUTPUT_INVALID", error.as_str(), "retry")
            } else {
                ("OUTPUT_INVALID", error.as_str(), "invalid")
            }
        } else if error.starts_with("STALE:") {
            (
                "STALE_INPUT",
                "The conversation or brief scope changed while this role was running",
                "failed",
            )
        } else {
            ("ROLE_VALIDATION", error.as_str(), "failed")
        };
        jobs::finish_attempt_error(
            workspace,
            id,
            attempt.number,
            code,
            &redact_error(message),
            disposition,
        )
        .await;
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InterviewerOutput {
    next_question: String,
    target_field: String,
    extracted: BTreeMap<String, ExtractedValue>,
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

fn token_upper_bound(text: &str) -> u32 {
    text.len().div_ceil(4).min(u32::MAX as usize) as u32
}

fn redact_error(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || *ch == '\n')
        .take(500)
        .collect()
}

async fn process_interviewer_result(
    workspace: &Workspace,
    job_id: &str,
    attempt: &Attempt,
    input: &Value,
    output_text: &str,
    tokens_in: u32,
    tokens_out: u32,
) -> Result<Value, String> {
    let output: InterviewerOutput = serde_json::from_str(output_text)
        .map_err(|error| format!("SCHEMA: invalid Interviewer output: {error}"))?;
    if output.next_question.trim().is_empty()
        || output.next_question.chars().count() > 600
        || output.extracted.len() > 24
    {
        return Err("SCHEMA: Interviewer output exceeds field bounds".into());
    }
    let chat_id = input["chat_id"]
        .as_str()
        .ok_or("ROLE_INPUT: missing chat id")?;
    let kind = input["asset_kind"]
        .as_str()
        .ok_or("ROLE_INPUT: missing asset kind")?;
    let turns = input["turns"]
        .as_array()
        .ok_or("ROLE_INPUT: missing turns")?;
    let allowed: std::collections::BTreeSet<&str> = crate::assistant::allowed_fields(kind)
        .ok_or("ROLE_INPUT: unsupported asset kind")?;
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
            return Err(format!(
                "ROLE_VALIDATION: field {field:?} is not part of the {kind} schema"
            ));
        }
        let owner_turn = turns
            .iter()
            .find(|turn| {
                turn["id"].as_str() == Some(extracted.owner_turn_id.as_str())
                    && turn["speaker"].as_str() == Some("owner")
            })
            .ok_or_else(|| {
                format!("ROLE_VALIDATION: extracted field {field:?} cites no owner turn")
            })?;
        let owner_text = owner_turn["text"]
            .as_str()
            .ok_or("ROLE_VALIDATION: owner turn has no text")?;
        if parley_domain::evidence::verify_quote(owner_text, &extracted.owner_quote, None).is_err()
        {
            return Err(format!(
                "ROLE_VALIDATION: owner quote for {field:?} is not an exact verified span"
            ));
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
        .ok_or("ROLE_INPUT: missing chat revision")?;
    let now = crate::now_utc();
    let candidate_id = crate::new_id(&format!("{job_id}:candidate"));
    let fields_value = Value::Object(fields);
    let quotes_value = Value::Object(quotes);
    let missing_value = serde_json::to_value(&missing).map_err(|error| error.to_string())?;
    let result = json!({"prompt_id":attempt.prompt_id,"next_question":output.next_question,"target_field":output.target_field,"candidate_id":candidate_id,"fields":fields_value,"owner_quotes":quotes_value,"missing_fields":missing,"ready_to_save":ready});
    let mut tx = workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| e.to_string())?;
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT revision FROM agent_chat WHERE id=? AND role='interviewer' AND scope_kind='owner'",
    )
    .bind(chat_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    if current != Some(chat_revision) {
        tx.rollback().await.map_err(|e| e.to_string())?;
        return Err("STALE: interviewer chat changed".into());
    }
    let turn_no: i64 =
        sqlx::query_scalar("SELECT coalesce(max(turn_no),0)+1 FROM agent_turn WHERE chat_id=?")
            .bind(chat_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    let turn_id = crate::new_id(&format!("{chat_id}:{turn_no}"));
    sqlx::query("INSERT INTO agent_turn(id,chat_id,turn_no,speaker,text,job_id,created_at) VALUES(?,?,?,'agent',?,?,?)")
        .bind(&turn_id).bind(chat_id).bind(turn_no).bind(&output.next_question).bind(job_id).bind(&now).execute(&mut *tx).await.map_err(|e|e.to_string())?;
    sqlx::query("INSERT INTO interviewer_candidate(id,chat_id,job_id,asset_kind,fields_json,owner_quotes_json,missing_fields_json,state,created_at,updated_at) VALUES(?,?,?,?,?,?,?,'proposed',?,?)")
        .bind(&candidate_id).bind(chat_id).bind(job_id).bind(kind).bind(fields_value.to_string()).bind(quotes_value.to_string()).bind(missing_value.to_string()).bind(&now).bind(&now).execute(&mut *tx).await.map_err(|e|e.to_string())?;
    sqlx::query("UPDATE agent_chat SET revision=revision+1,updated_at=? WHERE id=? AND revision=?")
        .bind(&now)
        .bind(chat_id)
        .bind(chat_revision)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'agent_chat',?,?, 'interviewer.candidate_proposed',?)")
        .bind(crate::new_id(job_id)).bind(&now).bind(chat_id).bind(chat_revision+1).bind(json!({"candidate_id":candidate_id,"job_id":job_id,"ready":ready}).to_string()).execute(&mut *tx).await.map_err(|e|e.to_string())?;
    jobs::complete_role_result_in_tx(
        &mut tx,
        job_id,
        attempt.number,
        &attempt.input_sha256,
        &result,
        tokens_in,
        tokens_out,
    )
    .await
    .map_err(|e| format!("STALE: {e}"))?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(result)
}

async fn process_briefer_result(
    workspace: &Workspace,
    job_id: &str,
    attempt: &Attempt,
    input: &Value,
    output_text: &str,
    tokens_in: u32,
    tokens_out: u32,
) -> Result<Value, String> {
    let output: BrieferOutput = serde_json::from_str(output_text)
        .map_err(|error| format!("SCHEMA: invalid Briefer output: {error}"))?;
    if output.items.len() > 80 {
        return Err("SCHEMA: brief contains too many items".into());
    }
    let chat_response = output.response;
    let has_chat = input["chat_id"].as_str().is_some();
    if has_chat
        && chat_response
            .as_deref()
            .is_none_or(|value| value.trim().is_empty() || value.chars().count() > 1600)
    {
        return Err("ROLE_VALIDATION: scoped Briefer chat needs a bounded response".into());
    }
    if !has_chat
        && chat_response
            .as_deref()
            .is_some_and(|value| value.chars().count() > 1600)
    {
        return Err("ROLE_VALIDATION: brief response exceeds its bound".into());
    }
    let scope_kind = input["scope_kind"]
        .as_str()
        .ok_or("ROLE_INPUT: missing scope kind")?;
    let scope_id = input["scope_id"]
        .as_str()
        .ok_or("ROLE_INPUT: missing scope id")?;
    let evidence = input["evidence"].as_array().cloned().unwrap_or_default();
    let now = crate::now_utc();
    let mut validated = Vec::<(BriefCandidate, Option<(String, String, i64, i64)>)>::new();
    for item in output.items {
        if item.text.trim().is_empty()
            || item.text.chars().count() > 1000
            || !valid_brief_kind(&item.kind)
            || !valid_detail(&item.kind, &item.detail)
        {
            return Err("ROLE_VALIDATION: brief item failed deterministic field validation".into());
        }
        let evidence_span = if item.tier == 1 {
            let quote = item
                .quote
                .as_deref()
                .filter(|value| !value.is_empty())
                .ok_or("ROLE_VALIDATION: tier-1 item lacks a quote")?;
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
                return Err(
                    "ROLE_VALIDATION: tier-1 quote must resolve to one scoped source occurrence"
                        .into(),
                );
            }
            Some(matches.remove(0))
        } else if item.tier == 2 {
            if item.check_suggestion.as_deref().is_none_or(str::is_empty)
                || item.quote.is_some()
                || item.text.contains('@')
            {
                return Err("ROLE_VALIDATION: tier-2 brief item needs a verification suggestion and may not claim quoted or named evidence".into());
            }
            if mentions_sensitive_topic(&item.text) {
                return Err("ROLE_VALIDATION: sensitive topic is prohibited in a brief".into());
            }
            if matches!(item.kind.as_str(), "norm" | "value" | "landmine")
                && scope_kind != "segment"
            {
                return Err(
                    "ROLE_VALIDATION: unverified cultural tendencies must be scoped to a segment"
                        .into(),
                );
            }
            None
        } else {
            return Err("ROLE_VALIDATION: the model cannot assign tier 3".into());
        };
        if item.tier == 1 && mentions_sensitive_topic(&item.text) {
            return Err("ROLE_VALIDATION: sensitive topic is prohibited in a brief".into());
        }
        validated.push((item, evidence_span));
    }
    let result = json!({"prompt_id":attempt.prompt_id,"scope_kind":scope_kind,"scope_id":scope_id,"item_count":validated.len(),"response":chat_response});
    let mut tx = workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| e.to_string())?;
    let current:Option<(String,String,i64)>=sqlx::query_as("SELECT scope_kind,scope_id,scope_revision FROM durable_job WHERE id=? AND status='running' AND current_attempt=?").bind(job_id).bind(attempt.number).fetch_optional(&mut *tx).await.map_err(|e|e.to_string())?;
    let expected_revision = input["scope_revision"]
        .as_i64()
        .ok_or("ROLE_INPUT: missing scope revision")?;
    if current.as_ref().is_none_or(|(kind, id, revision)| {
        kind != scope_kind || id != scope_id || *revision != expected_revision
    }) {
        tx.rollback().await.map_err(|e| e.to_string())?;
        return Err("STALE: brief job is no longer current".into());
    }
    if jobs::scoped_job_fence(&mut tx, job_id)
        .await
        .is_err()
    {
        tx.rollback().await.map_err(|e| e.to_string())?;
        return Err("STALE: the initiating run revoked or the epoch changed".into());
    }
    if let Some(chat_id) = input["chat_id"].as_str() {
        let expected_chat_revision = input["chat_revision"]
            .as_i64()
            .ok_or("ROLE_INPUT: missing chat revision")?;
        let chat: Option<(i64, String, String, Option<String>)> =
            sqlx::query_as("SELECT revision,role,scope_kind,scope_id FROM agent_chat WHERE id=?")
                .bind(chat_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        if chat
            .as_ref()
            .is_none_or(|(revision, role, chat_scope, chat_scope_id)| {
                *revision != expected_chat_revision
                    || role != "briefer"
                    || chat_scope != scope_kind
                    || chat_scope_id.as_deref() != Some(scope_id)
            })
        {
            tx.rollback().await.map_err(|e| e.to_string())?;
            return Err("STALE: Briefer chat changed or its scope no longer matches".into());
        }
        let turn_no: i64 =
            sqlx::query_scalar("SELECT coalesce(max(turn_no),0)+1 FROM agent_turn WHERE chat_id=?")
                .bind(chat_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        let turn_id = crate::new_id(&format!("{chat_id}:{turn_no}"));
        sqlx::query("INSERT INTO agent_turn(id,chat_id,turn_no,speaker,text,job_id,created_at) VALUES(?,?,?,'agent',?,?,?)")
            .bind(&turn_id).bind(chat_id).bind(turn_no).bind(chat_response.as_deref().unwrap_or_default()).bind(job_id).bind(&now).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        sqlx::query(
            "UPDATE agent_chat SET revision=revision+1,updated_at=? WHERE id=? AND revision=?",
        )
        .bind(&now)
        .bind(chat_id)
        .bind(expected_chat_revision)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'agent_chat',?,?, 'briefer.reply',?)")
            .bind(crate::new_id(job_id)).bind(&now).bind(chat_id).bind(expected_chat_revision+1).bind(json!({"job_id":job_id,"scope_kind":scope_kind,"scope_id":scope_id}).to_string()).execute(&mut *tx).await.map_err(|e|e.to_string())?;
    }
    if scope_kind == "party" {
        let entity_revision: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM entity WHERE id=? AND kind IN ('person','org')",
        )
        .bind(scope_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        if entity_revision != Some(expected_revision) {
            tx.rollback().await.map_err(|e| e.to_string())?;
            return Err("STALE: party changed during brief generation".into());
        }
    } else {
        let segment_revision: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM segment WHERE id=?")
                .bind(scope_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        if segment_revision != Some(expected_revision) {
            tx.rollback().await.map_err(|e| e.to_string())?;
            return Err("STALE: segment changed during brief generation".into());
        }
    }
    let mut inserted_count = 0usize;
    for (item, span) in validated {
        let duplicate:Option<String>=sqlx::query_scalar("SELECT id FROM brief_item WHERE scope_kind=? AND scope_id=? AND kind=? AND lower(text)=lower(?) AND status='dismissed' LIMIT 1")
            .bind(scope_kind).bind(scope_id).bind(&item.kind).bind(&item.text).fetch_optional(&mut *tx).await.map_err(|e|e.to_string())?;
        if duplicate.is_some() {
            continue;
        }
        let id = crate::new_id(&format!("{job_id}:{}:{}", item.kind, item.text));
        let detail = serde_json::to_string(&item.detail).map_err(|e| e.to_string())?;
        let (occurrence, variant, start, end) = span
            .map(|(a, b, c, d)| (Some(a), Some(b), Some(c), Some(d)))
            .unwrap_or((None, None, None, None));
        if item.kind == "jargon" {
            let term = item.detail.term.as_deref().unwrap_or_default().trim();
            sqlx::query("INSERT OR IGNORE INTO fluency(term,status,revision,updated_at) VALUES(?,'unknown',1,?)")
                .bind(term).bind(&now).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        }
        sqlx::query("INSERT INTO brief_item(id,scope_kind,scope_id,kind,text,detail_json,tier,source_occurrence_id,source_variant_id,quote,source_start,source_end,check_suggestion,speaker_kind,status,job_id,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,? ,?,'proposed',?,?,?)")
            .bind(&id).bind(scope_kind).bind(scope_id).bind(&item.kind).bind(&item.text).bind(detail).bind(item.tier as i64)
            .bind(&occurrence).bind(&variant).bind(&item.quote).bind(start).bind(end).bind(&item.check_suggestion)
            .bind(if item.tier==1 {"party"} else {"model_prior"}).bind(job_id).bind(&now).bind(&now)
            .execute(&mut *tx).await.map_err(|e|e.to_string())?;
        inserted_count += 1;
    }
    let event_payload = json!({
        "job_id": job_id,
        "scope_kind": scope_kind,
        "scope_id": scope_id,
        "items_proposed": inserted_count,
    });
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'brief',?,?, 'brief.items_proposed',?)")
        .bind(crate::new_id(job_id)).bind(&now).bind(scope_id).bind(1).bind(event_payload.to_string()).execute(&mut *tx).await.map_err(|e|e.to_string())?;
    jobs::complete_role_result_in_tx(
        &mut tx,
        job_id,
        attempt.number,
        &attempt.input_sha256,
        &result,
        tokens_in,
        tokens_out,
    )
    .await
    .map_err(|e| format!("STALE: {e}"))?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(result)
}
