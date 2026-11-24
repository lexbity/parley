//! Contextual Template Builder (AR-001 AR03).
//!
//! The general and contextual builder forms share one typed role job and
//! validator. Part context binds one authorized recipient, optional segment
//! membership and opportunity revision, selected ready resources, verified
//! evidence spans, voice samples and an optional family head. Admission
//! freezes a canonical dependency vector; execution returns two or three
//! validated reusable skeletons with separate typed preview bindings. Only an
//! owner request can save one immutable draft from a candidate; neither the
//! model nor an Activity action can activate it.

use std::collections::{BTreeMap, BTreeSet};

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, SqliteConnection, Transaction};

use crate::{
    AppState, WorkspaceError,
    application::{coach::fingerprint, inference_context::ContextRequest},
    digest, now_utc,
};

pub(crate) const PROMPT: &str = include_str!("../../roles/template_builder/prompt.md");
pub(crate) const SCHEMA: &str = include_str!("../../roles/template_builder/schema.json");

pub(crate) const MAX_VOICE: usize = 5;
pub(crate) const MAX_VARIANTS: usize = 3;
pub(crate) const MIN_VARIANTS: usize = 2;
pub(crate) const CANARY_SPAN: usize = 24;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BuildRequest {
    #[serde(default)]
    pub(crate) context: Option<ContextRequest>,
    #[serde(default)]
    pub(crate) segment_id: Option<String>,
    #[serde(default)]
    pub(crate) family_head_id: Option<String>,
    #[serde(default)]
    pub(crate) optional_rules: Vec<String>,
    pub(crate) stage_class: String,
    pub(crate) rungs: Vec<u32>,
    pub(crate) rules: Vec<String>,
    pub(crate) channel: String,
    pub(crate) max_chars: usize,
    #[serde(default)]
    pub(crate) voice_sample_ids: Vec<String>,
}

pub(crate) fn validate_params(input: &BuildRequest) -> Result<(), &'static str> {
    fn unique(values: &[String]) -> bool {
        values.iter().collect::<BTreeSet<_>>().len() == values.len()
    }
    if !matches!(
        input.stage_class.as_str(),
        "identified" | "prepared" | "touched" | "responded" | "converging"
    ) || input.rungs.is_empty()
        || input.rungs.len() > 5
        || input.rungs.iter().any(|rung| *rung == 0 || *rung > 5)
        || !input.rungs.windows(2).all(|pair| pair[0] < pair[1])
        || input.rules.is_empty()
        || input.rules.len() > 7
        || input.rules.iter().any(|rule| !is_rule(rule))
        || !unique(&input.rules)
        || input.optional_rules.iter().any(|rule| rule != "R3")
        || !unique(&input.optional_rules)
        || input
            .optional_rules
            .iter()
            .any(|rule| !input.rules.contains(rule))
        || input.max_chars == 0
        || input.max_chars > 20_000
        || input.channel.trim().is_empty()
        || input.channel.len() > 80
        || input.context.is_some() && input.segment_id.is_some()
    {
        return Err("TEMPLATE_BUILD_INPUT_INVALID");
    }
    Ok(())
}

pub(crate) fn is_rule(rule: &str) -> bool {
    matches!(rule, "R1" | "R2" | "R3" | "R4" | "R5" | "R6" | "R7")
}

/// Frozen context consumed by the candidate validator and preview renderer.
#[derive(Clone, Debug, Default)]
pub(crate) struct BuildContext {
    pub(crate) recipient_name: Option<String>,
    pub(crate) party_id: Option<String>,
    pub(crate) opportunity_id: Option<String>,
    pub(crate) sender_name: Option<String>,
    pub(crate) evidence: Vec<EvidenceItem>,
    pub(crate) resources: Vec<ResourceItem>,
    pub(crate) hooks: Vec<HookItem>,
    pub(crate) ref_ids: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct EvidenceItem {
    pub(crate) claim_id: String,
    pub(crate) revision: i64,
    #[allow(dead_code)] // wording feeds canary-span rejection and proof fills
    pub(crate) wording: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ResourceItem {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) title: String,
}

#[derive(Clone, Debug)]
pub(crate) struct HookItem {
    pub(crate) claim_id: String,
    pub(crate) revision: i64,
}

impl BuildContext {
    pub(crate) fn from_projection(projection: &Value) -> Self {
        let mut context = BuildContext::default();
        let scope = &projection["scope"];
        match scope["kind"].as_str() {
            Some("party") => {
                context.party_id = scope["id"].as_str().map(str::to_owned);
            }
            Some("opportunity") => {
                context.opportunity_id = scope["id"].as_str().map(str::to_owned);
                context.party_id = scope["recipient_id"].as_str().map(str::to_owned);
            }
            _ => {}
        }
        context.recipient_name = projection["recipient_name"].as_str().map(str::to_owned);
        if let Some(party_id) = &context.party_id {
            context.ref_ids.insert(party_id.clone());
        }
        if let Some(opportunity_id) = &context.opportunity_id {
            context.ref_ids.insert(opportunity_id.clone());
        }
        for item in projection["evidence"].as_array().into_iter().flatten() {
            if let (Some(claim_id), Some(revision), Some(wording)) = (
                item["claim_id"].as_str(),
                item["revision"].as_i64(),
                item["excerpt"].as_str(),
            ) {
                context.ref_ids.insert(claim_id.to_owned());
                context.evidence.push(EvidenceItem {
                    claim_id: claim_id.to_owned(),
                    revision,
                    wording: wording.to_owned(),
                });
            }
        }
        for item in projection["resources"].as_array().into_iter().flatten() {
            if let (Some(id), Some(kind), Some(title)) = (
                item["id"].as_str(),
                item["kind"].as_str(),
                item["title"].as_str(),
            ) {
                context.ref_ids.insert(id.to_owned());
                context.resources.push(ResourceItem {
                    id: id.to_owned(),
                    kind: kind.to_owned(),
                    title: title.to_owned(),
                });
            }
        }
        for item in projection["hooks"].as_array().into_iter().flatten() {
            if let (Some(claim_id), Some(revision)) = (
                item["claim_id"].as_str(),
                item["revision"].as_i64(),
            ) {
                context.ref_ids.insert(claim_id.to_owned());
                context.hooks.push(HookItem {
                    claim_id: claim_id.to_owned(),
                    revision,
                });
            }
        }
        context
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedCandidate {
    pub(crate) name: String,
    pub(crate) body: String,
    pub(crate) sentences: Vec<Value>,
    pub(crate) fallbacks: Value,
    pub(crate) notes: String,
    pub(crate) preview: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    variants: Vec<Variant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Variant {
    name: String,
    body: String,
    sentences: Vec<Sentence>,
    fallbacks: BTreeMap<String, String>,
    notes: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Sentence {
    text: String,
    rules: Vec<String>,
}

struct PreviewFill {
    slot: String,
    kind: String,
    reference: Option<Value>,
    value: String,
}

/// Validate the runner output for the frozen context. Returns the validated
/// candidates with typed preview bindings or a closed failure message.
pub(crate) async fn validate_candidates(
    conn: &mut SqliteConnection,
    raw: &Value,
    context: &BuildContext,
    requested_rules: &BTreeSet<String>,
    optional_rules: &BTreeSet<String>,
) -> Result<Vec<ValidatedCandidate>, String> {
    let output: Output = serde_json::from_value(raw.clone())
        .map_err(|_| "RESULT_INVALID".to_owned())?;
    if !(MIN_VARIANTS..=MAX_VARIANTS).contains(&output.variants.len()) {
        return Err("template builder must return two or three candidates".into());
    }
    let mut validated = Vec::with_capacity(output.variants.len());
    for variant in output.variants {
        validated.push(
            validate_variant(conn, variant, context, requested_rules, optional_rules).await?,
        );
    }
    Ok(validated)
}

async fn validate_variant(
    conn: &mut SqliteConnection,
    variant: Variant,
    context: &BuildContext,
    requested_rules: &BTreeSet<String>,
    optional_rules: &BTreeSet<String>,
) -> Result<ValidatedCandidate, String> {
    if variant.name.trim().is_empty()
        || variant.name.len() > 200
        || variant.notes.len() > 2000
        || variant.sentences.len() > 80
        || variant.sentences.is_empty()
    {
        return Err("candidate metadata exceeds the supported limits".into());
    }
    let proof_fallback = variant.fallbacks.get("proof").map(String::as_str);
    let tags = variant
        .sentences
        .iter()
        .map(|sentence| sentence.rules.clone())
        .collect::<Vec<_>>();
    let detailed = parley_domain::template::validate_detailed(
        &variant.body,
        proof_fallback,
        &tags,
    );
    if !detailed.errors.is_empty() {
        return Err(detailed.errors.join("; "));
    }
    let derived = detailed
        .sentences
        .iter()
        .flat_map(|sentence| sentence.derived.iter().map(String::as_str))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if derived != *requested_rules {
        return Err("candidate rule tags differ from the requested rule set".into());
    }

    // Fixed text with all typed slots removed: party facts and identifiers
    // must never become reusable skeleton text.
    let fixed = fixed_text(&variant.body);
    reject_canaries(&fixed, context)?;

    // Apply declared optional omissions. Only R3 may be optional and it
    // requires the existing omit_sentence proof fallback.
    let mut omitted = Vec::new();
    let effective_body = if optional_rules.contains("R3") {
        if proof_fallback != Some("omit_sentence") {
            return Err("R3 optional omission requires the omit_sentence fallback".into());
        }
        let mut kept_sentences = Vec::new();
        for (index, (sentence, rules)) in variant
            .sentences
            .iter()
            .zip(&tags)
            .enumerate()
        {
            if rules.iter().any(|rule| rule == "R3") {
                omitted.push(index);
                continue;
            }
            kept_sentences.push(sentence.text.as_str());
        }
        if kept_sentences.is_empty() {
            return Err("optional omission removed every sentence".into());
        }
        kept_sentences.join(" ")
    } else {
        variant.body.clone()
    };

    let sender = sender_name(conn).await?;
    let mut context = context.clone();
    context.sender_name = sender;

    let preview = render_preview(
        conn,
        &effective_body,
        &variant.sentences,
        &context,
        requested_rules,
        optional_rules,
        &omitted,
    )
    .await?;

    Ok(ValidatedCandidate {
        name: variant.name,
        body: variant.body,
        sentences: variant
            .sentences
            .into_iter()
            .map(|sentence| json!({"text":sentence.text,"rules":sentence.rules}))
            .collect(),
        fallbacks: serde_json::to_value(&variant.fallbacks).unwrap_or_default(),
        notes: variant.notes,
        preview,
    })
}

fn fixed_text(body: &str) -> String {
    let mut fixed = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find("{{") {
        fixed.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            fixed.push_str(after);
            break;
        };
        fixed.push(' ');
        rest = &after[close + 2..];
    }
    fixed.push_str(rest);
    fixed
}

fn reject_canaries(fixed: &str, context: &BuildContext) -> Result<(), String> {
    let lower = fixed.to_lowercase();
    if let Some(name) = &context.recipient_name
        && !name.trim().is_empty()
        && lower.contains(&name.to_lowercase())
    {
        return Err("candidate skeleton contains the selected party name".into());
    }
    for reference in &context.ref_ids {
        if fixed.contains(reference.as_str()) {
            return Err("candidate skeleton contains a private record identifier".into());
        }
    }
    for item in &context.evidence {
        let wording = item.wording.trim();
        if wording.len() >= CANARY_SPAN && contains_span(fixed, wording) {
            return Err("candidate skeleton copies a distinctive evidence span".into());
        }
    }
    for item in &context.resources {
        let title = item.title.trim();
        if title.len() >= CANARY_SPAN && contains_span(fixed, title) {
            return Err("candidate skeleton copies a distinctive resource span".into());
        }
    }
    Ok(())
}

fn contains_span(haystack: &str, needle: &str) -> bool {
    let needle = needle.trim();
    if needle.len() < CANARY_SPAN {
        return false;
    }
    let mut start = 0;
    let bytes = needle.as_bytes();
    while start + CANARY_SPAN <= bytes.len() {
        while start + CANARY_SPAN <= bytes.len() && !needle.is_char_boundary(start) {
            start += 1;
        }
        if start + CANARY_SPAN > bytes.len() {
            break;
        }
        let probe = &needle[start..start + CANARY_SPAN];
        if haystack.contains(probe) {
            return true;
        }
        start += 1;
    }
    false
}

async fn sender_name(conn: &mut SqliteConnection) -> Result<Option<String>, String> {
    sqlx::query_scalar("SELECT display_name FROM owner_profile WHERE id=1")
        .fetch_optional(&mut *conn)
        .await
        .map_err(|_| "PERSISTENCE_FAILED".to_owned())
        .map(|value| value.flatten())
}

#[allow(clippy::too_many_arguments)]
async fn render_preview(
    conn: &mut SqliteConnection,
    body: &str,
    sentences: &[Sentence],
    context: &BuildContext,
    requested_rules: &BTreeSet<String>,
    optional_rules: &BTreeSet<String>,
    omitted: &[usize],
) -> Result<Value, String> {
    let mut fills = Vec::<PreviewFill>::new();
    let mut missing = Vec::<(String, String)>::new();
    let mut effective_rules = BTreeSet::new();
    for (index, sentence) in sentences.iter().enumerate() {
        if omitted.contains(&index) {
            continue;
        }
        let mut sentence_rules = BTreeSet::new();
        let mut rest = sentence.text.as_str();
        while let Some(open) = rest.find("{{") {
            let after = &rest[open + 2..];
            let Some(close) = after.find("}}") else {
                break;
            };
            let slot = &after[..close];
            let (kind, _name) = slot.split_once(':').unwrap_or((slot, ""));
            let rule = rule_for_slot(kind);
            if let Some(rule) = rule {
                sentence_rules.insert(rule.to_owned());
            }
            let resolved = resolve_fill(conn, context, kind, &fills).await;
            match resolved {
                Ok(Some(fill)) => fills.push(fill),
                Ok(None) => {
                    if kind == "proof" && optional_rules.contains("R3") {
                        // The optional proof sentence would have been omitted;
                        // an unresolved optional fill does not block preview.
                    } else if rule.is_some_and(|rule| requested_rules.contains(rule)) {
                        missing.push((slot.to_owned(), kind.to_owned()));
                    }
                }
                Err(_) => return Err("PERSISTENCE_FAILED".to_owned()),
            }
            rest = &after[close + 2..];
        }
        effective_rules.extend(sentence_rules);
    }

    let complete = missing.is_empty();
    let fills = fills
        .into_iter()
        .map(|fill| {
            json!({"slot":fill.slot,"kind":fill.kind,"ref":fill.reference,"value":fill.value})
        })
        .collect::<Vec<_>>();
    let missing = missing
        .into_iter()
        .map(|(slot, kind)| json!({"slot":slot,"kind":kind}))
        .collect::<Vec<_>>();
    Ok(json!({
        "complete":complete,
        "fills":fills,
        "missing_slots":missing,
        "omitted_sentences":omitted,
        "effective_rules":effective_rules.iter().cloned().collect::<Vec<_>>(),
        "body":body,
    }))
}

fn rule_for_slot(kind: &str) -> Option<&'static str> {
    match kind {
        "gift" => Some("R1"),
        "ask" => Some("R2"),
        "proof" => Some("R3"),
        "artifact" => Some("R4"),
        "hook" => Some("R5"),
        "capacity_line" => Some("R6"),
        "identity_link" => Some("R7"),
        _ => None,
    }
}

async fn resolve_fill(
    conn: &mut SqliteConnection,
    context: &BuildContext,
    kind: &str,
    _fills: &[PreviewFill],
) -> Result<Option<PreviewFill>, String> {
    let reference = match kind {
        "their_name" => context
            .recipient_name
            .as_ref()
            .map(|name| (name.clone(), None)),
        "sender_name" => context
            .sender_name
            .as_ref()
            .map(|name| (name.clone(), None)),
        "gift" | "proof" | "artifact" => context
            .resources
            .iter()
            .find(|resource| resource.kind == kind)
            .map(|resource| {
                (
                    resource.title.clone(),
                    Some(json!({"kind":"resource","id":resource.id})),
                )
            })
            .or_else(|| {
                (kind == "proof")
                    .then(|| {
                        context.evidence.first().map(|evidence| {
                            (
                                evidence.wording.clone(),
                                Some(json!({"kind":"claim","id":evidence.claim_id,"revision":evidence.revision})),
                            )
                        })
                    })
                    .flatten()
            }),
        "hook" => {
            let opportunity = context.opportunity_id.as_deref();
            let party = context.party_id.as_deref().ok_or("CONTEXT_INVALID")?;
            let cutoff = hook_cutoff(conn).await?;
            for hook in &context.hooks {
                if let Some(evidence) =
                    super::template_slots::hook_fill(
                        conn,
                        &hook.claim_id,
                        hook.revision,
                        party,
                        opportunity,
                        &cutoff,
                    )
                    .await
                    .map_err(|_| "PERSISTENCE_FAILED".to_owned())?
                {
                    return Ok(Some(PreviewFill {
                        slot: format!("hook:{}", hook.claim_id),
                        kind: "hook".into(),
                        reference: Some(json!({"kind":"claim","id":hook.claim_id,"revision":hook.revision})),
                        value: evidence.wording,
                    }));
                }
            }
            None
        }
        _ => None,
    };
    let Some((value, reference)) = reference else {
        return Ok(None);
    };
    Ok(Some(PreviewFill {
        slot: format!("{kind}:{value}"),
        kind: kind.to_owned(),
        reference,
        value,
    }))
}

async fn hook_cutoff(conn: &mut SqliteConnection) -> Result<String, String> {
    let pack: Value = sqlx::query_scalar("SELECT settings_json FROM owner_profile WHERE id=1")
        .fetch_optional(&mut *conn)
        .await
        .map_err(|_| "PERSISTENCE_FAILED".to_owned())?
        .and_then(|settings: String| serde_json::from_str(&settings).ok())
        .unwrap_or_default();
    let days = pack
        .pointer("/pack_policy/limits/hook_max_age_days")
        .and_then(Value::as_i64)
        .unwrap_or(90);
    Ok((chrono::Utc::now() - chrono::Duration::days(days))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// Re-derive the canonical dependency digest for a frozen build input. A
/// stale session or dependency cannot publish or be saved as current.
pub(crate) async fn dependency_digest_for(
    tx: &mut Transaction<'_, Sqlite>,
    input: &Value,
) -> Result<String, WorkspaceError> {
    let dependencies = if let Some(context) = input.get("context").and_then(Value::as_object) {
        let request: ContextRequest = serde_json::from_value(Value::Object(context.clone()))
            .map_err(|_| WorkspaceError::Invalid("CONTEXT_INVALID".into()))?;
        let snapshot = super::inference_context::snapshot(tx, &request).await?;
        snapshot.dependency_json
    } else {
        let mut deps = Vec::new();
        if let Some(segment) = input.get("segment_scope") {
            deps.push(segment.clone());
        }
        if let Some(revisions) = input.get("voice_sample_revisions").and_then(Value::as_object) {
            for (id, revision) in revisions {
                deps.push(json!({"kind":"voice_sample","id":id,"revision":revision}));
            }
        }
        deps.sort_by_key(|item| item.to_string());
        Value::Array(deps)
    };
    fingerprint(&dependencies)
}

/// Admission handler shared by the general authoring form and the contextual
/// request variant.
#[allow(clippy::result_large_err)]
pub(crate) async fn build_templates(
    state: State<AppState>,
    headers: HeaderMap,
    Json(input): Json<BuildRequest>,
) -> Response {
    if let Err(response) = crate::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if let Err(code) = validate_params(&input) {
        return crate::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            code,
            "Builder needs a supported stage, increasing ask rungs, a valid rule set, optional R3 only, channel, and character limit.",
        );
    }
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty() && value.len() <= 200)
        .map(str::to_owned)
    else {
        return crate::error(
            StatusCode::BAD_REQUEST,
            "IDEMPOTENCY_KEY_REQUIRED",
            "Idempotency-Key is required",
        );
    };
    let canonical = match fingerprint(&json!({"operation":"buildTemplates","body":input})) {
        Ok(value) => value,
        Err(error) => return crate::command_error(error),
    };
    let epoch = state.workspace.epoch().to_string();
    let pool = state.workspace.pool();

    let read = async {
        let mut read = pool
            .begin()
            .await
            .map_err(|error| crate::command_error(error.into()))?;
        if let Some(receipt) =
            crate::assistant::receipt_response(&mut read, &epoch, &key, &canonical).await
        {
            return Err(receipt);
        }
        let snapshot = prepare_snapshot(&mut read, &input).await?;
        read
            .commit()
            .await
            .map_err(|error| crate::command_error(error.into()))?;
        Ok::<_, Response>(snapshot)
    }
    .await;
    let read = match read {
        Ok(snapshot) => snapshot,
        Err(response) => return response,
    };

    let write = async {
        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| crate::command_error(error.into()))?;
        if let Some(receipt) =
            crate::assistant::receipt_response(&mut tx, &epoch, &key, &canonical).await
        {
            return Err(receipt);
        }
        let current = prepare_snapshot(&mut tx, &input).await?;
        if current.dependencies != read.dependencies {
            return Err(crate::error(
                StatusCode::CONFLICT,
                "STALE_INPUT",
                "The selected context changed before admission; refresh and retry",
            ));
        }
        let (scope_kind, scope_id, scope_revision) = current.scope;
        let active: Option<String> = sqlx::query_scalar(
            "SELECT id FROM durable_job WHERE epoch=? AND role='template_builder' AND scope_id=?
             AND dependency_digest=? AND status IN ('queued','running') ORDER BY created_at,id LIMIT 1",
        )
        .bind(&epoch)
        .bind(&scope_id)
        .bind(&current.dependencies)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| crate::command_error(error.into()))?;
        if let Some(job) = active {
            let result = json!({"job_id":job,"status":"queued"});
            let event_cursor = crate::assistant::append_event(
                &mut tx,
                &job,
                "template.builder_queued",
                json!({"scope":current.projection["scope"]}),
            )
            .await
            .map_err(crate::command_error)?;
            crate::assistant::write_receipt(
                &mut tx,
                &epoch,
                &key,
                &canonical,
                202,
                &result,
                event_cursor,
            )
            .await
            .map_err(crate::command_error)?;
            tx.commit().await.map_err(|error| crate::command_error(error.into()))?;
            state.jobs.notify_one();
            return Ok::<_, Response>(template_response(StatusCode::ACCEPTED, result, &epoch));
        }
        let profile: Option<i64> = sqlx::query_scalar(
            "SELECT a.profile_revision FROM inference_active_profile a
             JOIN inference_qualification q ON q.profile_revision=a.profile_revision AND q.mode='structured_role'
             WHERE a.singleton=1 AND q.status='ready' AND q.checked_at >= ?
             AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=a.profile_revision)
             ORDER BY q.checked_at DESC LIMIT 1",
        )
        .bind(crate::now_utc_offset(-300))
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| crate::command_error(error.into()))?;
        let Some(profile) = profile else {
            return Err(crate::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "CAPABILITY_UNQUALIFIED",
                "Qualify the local structured-output model in Settings to build template candidates",
            ));
        };
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM durable_job WHERE status IN ('queued','running')",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| crate::command_error(error.into()))?;
        if live >= 31 {
            let mut response = crate::error(
                StatusCode::TOO_MANY_REQUESTS,
                "INFERENCE_BUSY",
                "The local inference queue is full",
            );
            response.headers_mut().insert(
                "retry-after",
                axum::http::HeaderValue::from_static("2"),
            );
            return Err(response);
        }
        let (prompt_version, prompt): (String, String) = sqlx::query_as(
            "SELECT p.version_id,p.prompt_text FROM agent_prompt_active a
             JOIN agent_prompt_version p ON p.version_id=a.version_id WHERE a.role='template_builder'",
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| crate::command_error(error.into()))?
        .unwrap_or_else(|| ("template_builder-v1".into(), PROMPT.into()));
        let input_json = json!({
            "context":input.context,
            "context_projection":current.projection,
            "segment_scope":current.segment_scope,
            "dependency_json":current.dependency_json,
            "scope":current.projection["scope"],
            "recipient_name":current.projection["recipient_name"],
            "family_head_id":input.family_head_id,
            "stage_class":input.stage_class,
            "rungs":input.rungs,
            "rules":input.rules,
            "optional_rules":input.optional_rules,
            "channel":input.channel,
            "max_chars":input.max_chars,
            "voice_samples":current.voice_samples,
            "voice_sample_revisions":current.voice_sample_revisions,
            "prompt_version":prompt_version,
            "prompt":prompt,
        });
        let equivalence = fingerprint(&json!({
            "scope":current.projection["scope"],
            "dependencies":current.dependencies,
            "profile":profile,
            "prompt_version":prompt_version,
            "schema":digest(SCHEMA.as_bytes()),
        }))
        .map_err(crate::command_error)?;
        let job = crate::jobs::enqueue_role_in_tx(
            &mut tx,
            "template_builder",
            &scope_kind,
            &scope_id,
            scope_revision,
            &input_json,
            3,
            &epoch,
            None,
            Some(&digest(format!("parley-owner:1:{epoch}").as_bytes())),
        )
        .await
        .map_err(crate::materials::workspace_error)?;
        sqlx::query("UPDATE durable_job SET profile_revision=?,equivalence_key=?,dependency_digest=?,dependency_json=?,eligible_at=?,queue_deadline_at=? WHERE id=?")
            .bind(profile)
            .bind(equivalence)
            .bind(&current.dependencies)
            .bind(current.dependency_json.to_string())
            .bind(now_utc())
            .bind(crate::now_utc_offset(300))
            .bind(&job)
            .execute(&mut *tx)
            .await
            .map_err(|error| crate::command_error(error.into()))?;
        for dep in current.dependency_json.as_array().into_iter().flatten() {
            sqlx::query(
                "INSERT INTO inference_job_dependency(job_id,kind,ref_id,revision) VALUES(?,?,?,?)",
            )
            .bind(&job)
            .bind(dep["kind"].as_str().unwrap_or("scope"))
            .bind(dep["id"].as_str().unwrap_or("scope"))
            .bind(dep["revision"].to_string())
            .execute(&mut *tx)
            .await
            .map_err(|error| crate::command_error(error.into()))?;
        }
        let result = json!({"job_id":job,"status":"queued"});
        let event_cursor = crate::assistant::append_event(
            &mut tx,
            &job,
            "template.builder_queued",
            json!({"scope":current.projection["scope"]}),
        )
        .await
        .map_err(crate::command_error)?;
        crate::assistant::write_receipt(
            &mut tx,
            &epoch,
            &key,
            &canonical,
            202,
            &result,
            event_cursor,
        )
        .await
        .map_err(crate::command_error)?;
        tx.commit().await.map_err(|error| crate::command_error(error.into()))?;
        state.jobs.notify_one();
        Ok::<_, Response>(template_response(StatusCode::ACCEPTED, result, &epoch))
    }
    .await;
    match write {
        Ok(response) | Err(response) => response,
    }
}

struct Snapshot {
    dependencies: String,
    dependency_json: Value,
    projection: Value,
    scope: (String, String, i64),
    segment_scope: Option<Value>,
    voice_samples: Vec<Value>,
    voice_sample_revisions: Value,
}

#[allow(clippy::result_large_err)]
async fn prepare_snapshot(
    tx: &mut Transaction<'_, Sqlite>,
    input: &BuildRequest,
) -> Result<Snapshot, Response> {
    if let Some(context) = &input.context {
        let snapshot = crate::application::inference_context::snapshot(tx, context)
            .await
            .map_err(|error| match error {
                WorkspaceError::NotFound => crate::error(
                    StatusCode::NOT_FOUND,
                    "TARGET_UNAVAILABLE",
                    "The selected party, opportunity, segment, evidence or resource is unavailable",
                ),
                WorkspaceError::StaleRevision => crate::error(
                    StatusCode::CONFLICT,
                    "STALE_INPUT",
                    "The selected context changed; refresh before building",
                ),
                WorkspaceError::Invalid(code) if code == "CONTEXT_BUDGET" => crate::error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "CONTEXT_BUDGET",
                    "Narrow the selected context and try again",
                ),
                WorkspaceError::Invalid(code) if code == "RESOURCE_SHORTLIST_REQUIRED" => {
                    crate::error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "RESOURCE_SHORTLIST_REQUIRED",
                        "Select resources from an eligible shortlist for this opportunity",
                    )
                }
                WorkspaceError::Invalid(code) if code == "RESOURCE_ASSESSMENT_UNAVAILABLE" => {
                    crate::error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "RESOURCE_ASSESSMENT_UNAVAILABLE",
                        "Run a calibrated match for this opportunity before selecting resources",
                    )
                }
                WorkspaceError::Invalid(code) if code == "RESOURCE_NOT_ELIGIBLE" => crate::error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "RESOURCE_NOT_ELIGIBLE",
                    "A selected resource is not eligible for this context",
                ),
                WorkspaceError::Invalid(code) if code == "DUPLICATE_CONTEXT_REFERENCE" => {
                    crate::error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "DUPLICATE_CONTEXT_REFERENCE",
                        "Evidence and resource selections must be unique",
                    )
                }
                WorkspaceError::Invalid(code) if code == "EVIDENCE_SPAN_INVALID" => crate::error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "EVIDENCE_SPAN_INVALID",
                    "Evidence spans must be bounded and non-empty",
                ),
                WorkspaceError::Invalid(code) if code == "VOICE_SAMPLE_LIMIT" => crate::error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "VOICE_SAMPLE_LIMIT",
                    "Choose at most five active voice samples",
                ),
                WorkspaceError::Invalid(code)
                    if code == "DUPLICATE_VOICE_SAMPLE"
                        || code == "VOICE_SAMPLE_INVALID" =>
                {
                    crate::error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "VOICE_SAMPLE_INVALID",
                        "Voice sample selections must be unique and valid",
                    )
                }
                WorkspaceError::Invalid(_) => crate::error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "TEMPLATE_BUILD_CONTEXT_INVALID",
                    "The contextual build request is invalid",
                ),
                other => crate::command_error(other),
            })?;
        let scope = &snapshot.projection["scope"];
        let (scope_kind, scope_id, scope_revision) = match scope["kind"].as_str() {
            Some("party") => (
                "party".to_owned(),
                scope["id"].as_str().unwrap_or_default().to_owned(),
                scope["revision"].as_i64().unwrap_or_default(),
            ),
            _ => (
                "opportunity".to_owned(),
                format!(
                    "{}:{}",
                    scope["id"].as_str().unwrap_or_default(),
                    scope["recipient_id"].as_str().unwrap_or_default()
                ),
                scope["revision"].as_i64().unwrap_or_default(),
            ),
        };
        let dependencies = fingerprint(&snapshot.dependency_json)
            .map_err(crate::command_error)?;
        return Ok(Snapshot {
            dependencies,
            dependency_json: snapshot.dependency_json.clone(),
            projection: snapshot.projection.clone(),
            scope: (scope_kind, scope_id, scope_revision),
            segment_scope: None,
            voice_samples: Vec::new(),
            voice_sample_revisions: json!({}),
        });
    }
    // General owner/segment authoring shares the same role and validator
    // without requiring a real preview.
    let (scope_kind, scope_id, scope_revision, segment_scope) =
        if let Some(segment) = input.segment_id.as_deref() {
            let row: Option<(String, Option<String>, i64)> = sqlx::query_as(
                "SELECT name,description,revision FROM segment WHERE id=?",
            )
            .bind(segment)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| crate::command_error(error.into()))?;
            let Some((name, description, revision)) = row else {
                return Err(crate::error(
                    StatusCode::NOT_FOUND,
                    "SEGMENT_NOT_FOUND",
                    "Template segment does not exist",
                ));
            };
            (
                "segment".to_owned(),
                segment.to_owned(),
                revision,
                Some(json!({"kind":"segment","id":segment,"revision":revision,"name":name,"description":description})),
            )
        } else {
            ("owner".to_owned(), "owner".to_owned(), 1, None)
        };
    let voice_samples = resolve_voice_samples(tx, &input.voice_sample_ids).await?;
    let mut dependency_json = Vec::new();
    if let Some(segment) = &segment_scope {
        dependency_json.push(segment.clone());
    }
    let mut voice_sample_revisions = serde_json::Map::new();
    for sample in &voice_samples {
        let id = sample["id"].as_str().unwrap_or_default();
        let revision = sample["revision"].as_i64().unwrap_or_default();
        voice_sample_revisions.insert(id.to_owned(), json!(revision));
        dependency_json.push(json!({"kind":"voice_sample","id":id,"revision":revision}));
    }
    dependency_json.sort_by_key(|item| item.to_string());
    let dependency_json = Value::Array(dependency_json);
    let dependencies = fingerprint(&dependency_json).map_err(crate::command_error)?;
    let projection = json!({
        "scope":json!({"kind":"owner"}),
        "recipient_name":Value::Null,
        "segment":segment_scope,
        "evidence":[],
        "resources":[],
        "voice_samples":voice_samples,
    });
    Ok(Snapshot {
        dependencies,
        dependency_json,
        projection,
        scope: (scope_kind, scope_id, scope_revision),
        segment_scope,
        voice_samples,
        voice_sample_revisions: Value::Object(voice_sample_revisions),
    })
}

#[allow(clippy::result_large_err)]
async fn resolve_voice_samples(
    tx: &mut Transaction<'_, Sqlite>,
    voice_sample_ids: &[String],
) -> Result<Vec<Value>, Response> {
    if voice_sample_ids.len() > MAX_VOICE {
        return Err(crate::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VOICE_SAMPLE_LIMIT",
            "Choose at most five active voice samples",
        ));
    }
    if voice_sample_ids.is_empty() {
        return Ok(Vec::new());
    }
    let unique = voice_sample_ids.iter().collect::<BTreeSet<_>>();
    if unique.len() != voice_sample_ids.len() {
        return Err(crate::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VOICE_SAMPLE_DUPLICATE",
            "Voice sample IDs must be unique",
        ));
    }
    let ids_json = serde_json::to_string(voice_sample_ids)
        .map_err(|_| crate::error(
            StatusCode::BAD_REQUEST,
            "VOICE_SAMPLE_INVALID",
            "Voice sample selection is invalid",
        ))?;
    let samples: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT id,text,revision FROM voice_sample WHERE id IN (SELECT value FROM json_each(?)) AND status='active' ORDER BY id",
    )
    .bind(&ids_json)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| crate::command_error(error.into()))?;
    if samples.len() != unique.len() {
        return Err(crate::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VOICE_SAMPLE_MISSING",
            "Every selected voice sample must exist in the owner bank",
        ));
    }
    Ok(samples
        .into_iter()
        .map(|(id, text, revision)| json!({"id":id,"text":text,"revision":revision}))
        .collect())
}

fn template_response(status: StatusCode, value: Value, epoch: &str) -> Response {
    let mut response = (status, Json(value)).into_response();
    crate::private_headers(&mut response, epoch);
    response
}

/// Read candidate proposals with their separate preview bindings.
pub(crate) async fn list_candidates(
    state: State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Response {
    if let Err(response) = crate::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let row: Result<Option<(String, String, String)>, sqlx::Error> = sqlx::query_as(
        "SELECT role,status,epoch FROM durable_job WHERE id=?",
    )
    .bind(&job_id)
    .fetch_optional(state.workspace.pool())
    .await;
    let row = match row {
        Ok(row) => row,
        Err(error) => return crate::command_error(error.into()),
    };
    let Some((role, status, epoch)) = row else {
        return crate::error(
            StatusCode::NOT_FOUND,
            "JOB_NOT_FOUND",
            "Job does not exist",
        );
    };
    if role != "template_builder" || epoch != state.workspace.epoch() {
        return crate::error(
            StatusCode::NOT_FOUND,
            "JOB_NOT_FOUND",
            "Job does not exist",
        );
    }
    type CandidateRow = (String, i64, String, String, String, String, String, String, i64, Option<String>, Option<String>, String);
    let rows: Result<Vec<CandidateRow>, sqlx::Error> = sqlx::query_as(
            "SELECT id,variant_no,name,body,sentences_json,fallbacks_json,notes,preview_json,revision,saved_version_id,family_head_id,status
             FROM template_build_candidate WHERE job_id=? ORDER BY variant_no",
        )
        .bind(&job_id)
        .fetch_all(state.workspace.pool())
        .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => return crate::command_error(error.into()),
    };
    let items = rows
        .into_iter()
        .map(|(id, variant_no, name, body, sentences, fallbacks, notes, preview, revision, saved, family_head, candidate_status)| {
            json!({
                "id":id,
                "variant_no":variant_no,
                "name":name,
                "body":body,
                "sentences":serde_json::from_str::<Value>(&sentences).unwrap_or_default(),
                "fallbacks":serde_json::from_str::<Value>(&fallbacks).unwrap_or_default(),
                "notes":notes,
                "preview":serde_json::from_str::<Value>(&preview).unwrap_or_default(),
                "revision":revision,
                "status":candidate_status,
                "saved_version_id":saved,
                "family_head_id":family_head,
            })
        })
        .collect::<Vec<_>>();
    let body = json!({
        "job_id":job_id,
        "role":"template_builder",
        "status":status,
        "items":items,
    });
    let mut response = Json(body).into_response();
    crate::private_headers(&mut response, state.workspace.epoch());
    response
}

#[derive(Debug)]
pub(crate) enum CandidateSaveError {
    NotFound,
    Stale,
    NotOwnerReview,
    FamilyHeadChanged,
    DependencyStale,
    InvalidEdits,
    Database,
}

/// Validate a candidate-derived draft save and resolve the final values.
pub(crate) async fn prepare_candidate_save(
    tx: &mut Transaction<'_, Sqlite>,
    candidate_id: &str,
    expected_revision: i64,
    expected_family_head: Option<&str>,
    edits: Option<&Value>,
) -> Result<Value, CandidateSaveError> {
    type CandidateSaveRow = (String, String, String, String, String, String, String, Option<String>, String, String, i64);
    let row: Option<CandidateSaveRow> = sqlx::query_as(
            "SELECT body,sentences_json,fallbacks_json,name,notes,requested_rules_json,optional_rules_json,family_head_id,status,job_id,revision
             FROM template_build_candidate WHERE id=?",
        )
        .bind(candidate_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| CandidateSaveError::Database)?;
    let Some((body, sentences, fallbacks, name, notes, requested_rules, optional_rules, family_head_id, status, job_id, revision)) = row else {
        return Err(CandidateSaveError::NotFound);
    };
    if status != "owner_review_required" {
        return Err(CandidateSaveError::NotOwnerReview);
    }
    if revision != expected_revision {
        return Err(CandidateSaveError::Stale);
    }
    if let Some(expected) = expected_family_head
        && family_head_id.as_deref() != Some(expected)
    {
        return Err(CandidateSaveError::FamilyHeadChanged);
    }
    let job: Option<(String, String, String)> = sqlx::query_as(
        "SELECT input_json,status,epoch FROM durable_job WHERE id=?",
    )
    .bind(&job_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| CandidateSaveError::Database)?;
    let Some((job_input, job_status, _epoch)) = job else {
        return Err(CandidateSaveError::NotFound);
    };
    if job_status != "succeeded" {
        return Err(CandidateSaveError::Stale);
    }
    let input: Value = serde_json::from_str(&job_input)
        .map_err(|_| CandidateSaveError::Database)?;
    let current_dependencies = dependency_digest_for(tx, &input)
        .await
        .map_err(|_| CandidateSaveError::DependencyStale)?;
    let frozen = input["dependency_json"]
        .as_array()
        .map(|items| {
            fingerprint(&Value::Array(items.clone()))
                .unwrap_or_default()
        })
        .unwrap_or_default();
    if current_dependencies != frozen {
        return Err(CandidateSaveError::DependencyStale);
    }

    let mut final_name = name;
    let mut final_body = body;
    let mut final_sentences: Value = serde_json::from_str(&sentences).unwrap_or_default();
    let mut final_fallbacks: Value = serde_json::from_str(&fallbacks).unwrap_or_default();
    if let Some(edits) = edits {
        if let Some(name) = edits.get("edits").and_then(|value| value.get("name")).and_then(Value::as_str) {
            final_name = name.to_owned();
        }
        if let Some(body) = edits.get("edits").and_then(|value| value.get("body")).and_then(Value::as_str) {
            final_body = body.to_owned();
        }
        if let Some(sentences) = edits.get("edits").and_then(|value| value.get("sentence_rules")) {
            final_sentences = sentences.clone();
        }
        if let Some(proof) = edits.get("edits").and_then(|value| value.get("proof_fallback")).and_then(Value::as_str)
            && let Some(object) = final_fallbacks.as_object_mut()
        {
            object.insert("proof".into(), Value::String(proof.to_owned()));
        }
    }
    let proof_fallback = final_fallbacks
        .get("proof")
        .and_then(Value::as_str);
    let tags: Vec<Vec<String>> = final_sentences
        .as_array()
        .ok_or(CandidateSaveError::InvalidEdits)?
        .iter()
        .map(|sentence| {
            sentence["rules"]
                .as_array()
                .ok_or(CandidateSaveError::InvalidEdits)
                .map(|rules| {
                    rules
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
        })
        .collect::<Result<_, _>>()?;
    if final_name.trim().is_empty()
        || final_name.len() > 200
        || final_body.len() > 10_000
        || final_body.trim().is_empty()
    {
        return Err(CandidateSaveError::InvalidEdits);
    }
    let detailed = parley_domain::template::validate_detailed(
        &final_body,
        proof_fallback,
        &tags,
    );
    if !detailed.errors.is_empty() {
        return Err(CandidateSaveError::InvalidEdits);
    }
    let derived = detailed
        .sentences
        .iter()
        .flat_map(|sentence| sentence.derived.iter().map(String::as_str))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let requested: BTreeSet<String> = serde_json::from_str(&requested_rules).unwrap_or_default();
    let optional: BTreeSet<String> = serde_json::from_str(&optional_rules).unwrap_or_default();
    let allowed = requested.difference(&optional).cloned().collect::<BTreeSet<_>>();
    if derived != allowed {
        return Err(CandidateSaveError::InvalidEdits);
    }
    // Party facts and private identifiers must not become reusable skeleton
    // text even when an owner edits a candidate before saving it.
    let context = BuildContext::from_projection(
        input.get("context_projection").unwrap_or(&Value::Null),
    );
    reject_canaries(&fixed_text(&final_body), &context)
        .map_err(|_| CandidateSaveError::InvalidEdits)?;
    let _ = notes;
    Ok(json!({
        "name":final_name,
        "body":final_body,
        "sentence_rules":final_sentences,
        "proof_fallback":proof_fallback,
        "family_head_id":family_head_id,
    }))
}

/// CAS-link the saved immutable version to its candidate. One save per
/// candidate is enforced by the database unique index and the revision check.
pub(crate) async fn link_saved_version(
    tx: &mut Transaction<'_, Sqlite>,
    candidate_id: &str,
    expected_revision: i64,
    saved_version_id: &str,
) -> Result<(), CandidateSaveError> {
    let changed = sqlx::query(
        "UPDATE template_build_candidate SET status='saved',saved_version_id=?,revision=revision+1,updated_at=?
         WHERE id=? AND revision=? AND status='owner_review_required' AND saved_version_id IS NULL",
    )
    .bind(saved_version_id)
    .bind(now_utc())
    .bind(candidate_id)
    .bind(expected_revision)
    .execute(&mut **tx)
    .await
    .map_err(|_| CandidateSaveError::Database)?
    .rows_affected();
    if changed != 1 {
        return Err(CandidateSaveError::Stale);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;

    async fn memory_db() -> SqliteConnection {
        let mut db = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("../../migrations")
            .run(&mut db)
            .await
            .unwrap();
        db
    }

    fn party_context() -> BuildContext {
        let mut context = BuildContext {
            party_id: Some("p_42".into()),
            recipient_name: Some("Casey Example".into()),
            ..BuildContext::default()
        };
        context.ref_ids.insert("p_42".into());
        context.ref_ids.insert("cl_3".into());
        context.evidence.push(EvidenceItem {
            claim_id: "cl_3".into(),
            revision: 2,
            wording: "The team reduced onboarding time from nine days to three.".into(),
        });
        context
    }

    fn raw_variants(variants: Value) -> Value {
        json!({"variants":variants})
    }

    fn variant(
        body: &str,
        rules: &[&str],
        fallbacks: Value,
    ) -> Value {
        json!({
            "name":"Variant",
            "body":body,
            "sentences":[{"text":body,"rules":rules}],
            "fallbacks":fallbacks,
            "notes":"draft"
        })
    }

    fn variant_sentences(body: &str, sentences: Value, fallbacks: Value) -> Value {
        json!({
            "name":"Variant",
            "body":body,
            "sentences":sentences,
            "fallbacks":fallbacks,
            "notes":"draft"
        })
    }

    #[tokio::test]
    async fn ar03_candidate_validation_rejects_injected_canaries() {
        let mut db = memory_db().await;
        let rules = ["R1"].into_iter().map(str::to_owned).collect::<BTreeSet<_>>();
        let optional = BTreeSet::new();
        // Party name must never become reusable skeleton text.
        let raw = raw_variants(json!([
            variant("Hi Casey Example, I can share {{gift:guide}}.", &["R1"], json!({})),
            variant("Hi Casey Example, I can share {{gift:guide}} too.", &["R1"], json!({})),
        ]));
        let error = validate_candidates(&mut db, &raw, &party_context(), &rules, &optional)
            .await
            .unwrap_err();
        assert!(error.contains("Casey") || error.contains("party name"), "{error}");
        // Private record identifiers are rejected.
        let raw = raw_variants(json!([
            variant("See p_42 for details and {{gift:guide}}.", &["R1"], json!({})),
            variant("See p_42 for details and {{gift:guide}} too.", &["R1"], json!({})),
        ]));
        let error = validate_candidates(&mut db, &raw, &party_context(), &rules, &optional)
            .await
            .unwrap_err();
        assert!(
            error.contains("identifier") || error.contains("digits"),
            "{error}"
        );
        // Distinctive evidence spans are rejected.
        let raw = raw_variants(json!([
            variant("The team reduced onboarding time from nine days to three at {{gift:guide}}.", &["R1"], json!({})),
            variant("The team reduced onboarding time from nine days to three at {{gift:guide}} too.", &["R1"], json!({})),
        ]));
        let error = validate_candidates(&mut db, &raw, &party_context(), &rules, &optional)
            .await
            .unwrap_err();
        assert!(error.contains("evidence span"), "{error}");
        // URLs are rejected as fixed text.
        let raw = raw_variants(json!([
            variant("Learn more at https://example.com and {{gift:guide}}.", &["R1"], json!({})),
            variant("Learn more at https://example.com and {{gift:guide}} too.", &["R1"], json!({})),
        ]));
        let error = validate_candidates(&mut db, &raw, &party_context(), &rules, &optional)
            .await
            .unwrap_err();
        assert!(error.contains("URL"), "{error}");
        // A clean skeleton passes validation with a bounded preview.
        let raw = raw_variants(json!([
            variant("I can share {{gift:guide}} with you.", &["R1"], json!({})),
            variant("I can share {{gift:guide}} as well.", &["R1"], json!({})),
        ]));
        let validated = validate_candidates(&mut db, &raw, &party_context(), &rules, &optional)
            .await
            .unwrap();
        assert_eq!(validated.len(), 2);
        assert!(!validated[0].preview["complete"].as_bool().unwrap());
        let missing = validated[0].preview["missing_slots"].as_array().unwrap();
        assert_eq!(missing[0]["kind"], "gift");
    }

    #[tokio::test]
    async fn ar03_optional_proof_omission_produces_complete_preview() {
        let mut db = memory_db().await;
        let rules = ["R1", "R3"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let optional = ["R3"].into_iter().map(str::to_owned).collect::<BTreeSet<_>>();
        let mut context = party_context();
        context.resources.push(ResourceItem {
            id: "r_9".into(),
            kind: "gift".into(),
            title: "A practical guide".into(),
        });
        context.ref_ids.insert("r_9".into());
        let raw = raw_variants(json!([
            variant_sentences(
                "I can share {{gift:guide}} with you. I can share {{proof:p}} too.",
                json!([
                    {"text":"I can share {{gift:guide}} with you.","rules":["R1"]},
                    {"text":"I can share {{proof:p}} too.","rules":["R3"]}
                ]),
                json!({"proof":"omit_sentence"}),
            ),
            variant_sentences(
                "I can share {{gift:guide}} as well. {{proof:p}} also applies here.",
                json!([
                    {"text":"I can share {{gift:guide}} as well.","rules":["R1"]},
                    {"text":"{{proof:p}} also applies here.","rules":["R3"]}
                ]),
                json!({"proof":"omit_sentence"}),
            ),
        ]));
        let validated = validate_candidates(&mut db, &raw, &context, &rules, &optional)
            .await
            .unwrap();
        let preview = &validated[0].preview;
        assert!(preview["complete"].as_bool().unwrap(), "{preview}");
        assert_eq!(preview["omitted_sentences"].as_array().unwrap().len(), 1);
        let effective = preview["effective_rules"].as_array().unwrap();
        assert_eq!(effective.len(), 1);
        assert_eq!(effective[0], "R1");
        let fills = preview["fills"].as_array().unwrap();
        assert_eq!(fills[0]["kind"], "gift");
        assert_eq!(fills[0]["ref"]["kind"], "resource");
        // Without the omit_sentence fallback the omission is invalid.
        let raw = raw_variants(json!([
            variant("I can share {{proof:p}} with you.", &["R3"], json!({"proof":"use_alternative_template"})),
            variant("I can share {{proof:p}} with you too.", &["R3"], json!({"proof":"use_alternative_template"})),
        ]));
        let rules_r3 = ["R3"].into_iter().map(str::to_owned).collect::<BTreeSet<_>>();
        let error = validate_candidates(&mut db, &raw, &context, &rules_r3, &optional)
            .await
            .unwrap_err();
        assert!(error.contains("omit_sentence"), "{error}");
    }

    #[tokio::test]
    async fn ar03_malformed_variants_are_rejected() {
        let mut db = memory_db().await;
        let rules = ["R1"].into_iter().map(str::to_owned).collect::<BTreeSet<_>>();
        let optional = BTreeSet::new();
        // One variant is not enough.
        let raw = raw_variants(json!([
            variant("I can share {{gift:guide}}.", &["R1"], json!({}))
        ]));
        assert!(validate_candidates(&mut db, &raw, &party_context(), &rules, &optional)
            .await
            .is_err());
        // Four variants exceed the bound.
        let raw = raw_variants(json!([
            variant("A {{gift:guide}}.", &["R1"], json!({})),
            variant("B {{gift:guide}}.", &["R1"], json!({})),
            variant("C {{gift:guide}}.", &["R1"], json!({})),
            variant("D {{gift:guide}}.", &["R1"], json!({})),
        ]));
        assert!(validate_candidates(&mut db, &raw, &party_context(), &rules, &optional)
            .await
            .is_err());
        // Rule tags must equal the requested set.
        let raw = raw_variants(json!([
            variant("I can share {{gift:guide}}.", &["R2"], json!({})),
            variant("I can share {{gift:guide}} too.", &["R2"], json!({})),
        ]));
        let error = validate_candidates(&mut db, &raw, &party_context(), &rules, &optional)
            .await
            .unwrap_err();
        assert!(error.contains("rule tags"), "{error}");
        // Unregistered slot kinds are rejected by the grammar.
        let raw = raw_variants(json!([
            variant("I can share {{gift:guide}}.", &["R1"], json!({})),
            variant("I can share {{their_org:x}}.", &["R1"], json!({})),
        ]));
        assert!(validate_candidates(&mut db, &raw, &party_context(), &rules, &optional)
            .await
            .is_err());
    }

    async fn seed_hook(db: &mut SqliteConnection, party: &str, opportunity: Option<&str>) {
        let now = crate::now_utc();
        sqlx::query("INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES(?,'person',?,1,?,?)")
            .bind(party)
            .bind("Casey Example")
            .bind(&now)
            .bind(&now)
            .execute(&mut *db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO claim(id,subject_id,predicate,current_revision,created_at) VALUES('cl_3',?,'sponsors onboarding',1,?)")
            .bind(party)
            .bind(&now)
            .execute(&mut *db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO claim_revision(claim_id,revision,wording,knowledge_kind,tier,speaker_kind,approved_wording,confirmed_by_owner,created_at)
             VALUES('cl_3',1,'We sponsor onboarding','observation',3,'party','We sponsor onboarding',1,?)",
        )
        .bind(&now)
        .execute(&mut *db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO accepted_hook(claim_id,claim_revision,published_at,publicness,accepted_at)
             VALUES('cl_3',1,?,'professional_public',?)",
        )
        .bind(&now)
        .bind(&now)
        .execute(&mut *db)
        .await
        .unwrap();
        if let Some(opportunity) = opportunity {
            sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES(?,'Opp','open',1,?,?)")
                .bind(opportunity)
                .bind(&now)
                .bind(&now)
                .execute(&mut *db)
                .await
                .unwrap();
            sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES(?,?,'participant',?)")
                .bind(opportunity)
                .bind(party)
                .bind(&now)
                .execute(&mut *db)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO opportunity_interest(id,opportunity_id,party_id,side,statement,knowledge_kind,provenance_kind,claim_id,claim_revision,revision,status,created_at)
                 VALUES('oi_1',?,?,'them','We sponsor onboarding','observation','source_claim','cl_3',1,1,'active',?)",
            )
            .bind(opportunity)
            .bind(party)
            .bind(&now)
            .execute(&mut *db)
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn ar03_hook_fill_resolves_only_accepted_current_scoped_claims() {
        let mut db = memory_db().await;
        seed_hook(&mut db, "p_42", Some("o_7")).await;
        let rules = ["R5"].into_iter().map(str::to_owned).collect::<BTreeSet<_>>();
        let optional = BTreeSet::new();
        let mut context = BuildContext {
            party_id: Some("p_42".into()),
            opportunity_id: Some("o_7".into()),
            ..BuildContext::default()
        };
        context.ref_ids.insert("p_42".into());
        context.ref_ids.insert("cl_3".into());
        context.hooks.push(HookItem {
            claim_id: "cl_3".into(),
            revision: 1,
        });
        let raw = raw_variants(json!([
            variant("I noticed {{hook:interest}} for you.", &["R5"], json!({})),
            variant("You may like {{hook:interest}} too.", &["R5"], json!({})),
        ]));
        let validated = validate_candidates(&mut db, &raw, &context, &rules, &optional)
            .await
            .unwrap();
        let preview = &validated[0].preview;
        assert!(preview["complete"].as_bool().unwrap(), "{preview}");
        let fills = preview["fills"].as_array().unwrap();
        assert_eq!(fills[0]["kind"], "hook");
        assert_eq!(fills[0]["ref"]["kind"], "claim");
        assert_eq!(fills[0]["ref"]["id"], "cl_3");
        assert_eq!(fills[0]["value"], "We sponsor onboarding");

        // A foreign opportunity scope must not resolve the same hook.
        let mut context = context.clone();
        context.opportunity_id = Some("o_foreign".into());
        let validated = validate_candidates(&mut db, &raw, &context, &rules, &optional)
            .await
            .unwrap();
        let preview = &validated[0].preview;
        assert!(!preview["complete"].as_bool().unwrap(), "{preview}");
        assert_eq!(preview["missing_slots"].as_array().unwrap()[0]["kind"], "hook");
    }
}
