//! Typed Extractor admission and deterministic capture planning (AR-001 AR06).
//!
//! Capturing a source is an independent owner operation: the source is saved
//! with or without inference. When a qualified structured-output profile is
//! active and the queue has capacity, the same writer transaction admits one
//! durable Extractor job whose input freezes the canonical chunk plan, the
//! profile revision and the source dependency vector before any model request.
//! The plan reserves the complete context budget up front so a capture that
//! cannot fit the pinned prompt, schema and output reserve never reaches the
//! worker.

use crate::{Workspace, WorkspaceError, digest, new_id, now_utc, now_utc_offset};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;

pub(crate) const PROMPT: &str = include_str!("../../roles/extractor/prompt.md");
pub(crate) const SCHEMA: &str = include_str!("../../roles/extractor/schema.json");

/// Maximum cumulative chunks in one extraction attempt (AR-001 §5/§8).
pub(crate) const MAX_CHUNKS: usize = 16;
/// Reserved framing allowance for envelope, instruction boilerplate and the
/// output reserve beyond the visible prompt/schema bytes (conservative).
const FRAMING_RESERVE: usize = 1024;
/// A chunk plan cannot be useful below this source reserve (AR-001 §1.6).
const MIN_SOURCE_BUDGET: usize = 256;

/// One preplanned slice of the sanitized source, addressed by byte offsets
/// into the sanitized text. Offsets are stable because sanitization is
/// deterministic; the raw capture text itself stays on the source variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChunkPlan {
    pub(crate) index: u32,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanError {
    Budget(String),
    TooManyChunks,
}

impl fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Budget(message) => formatter.write_str(message),
            Self::TooManyChunks => {
                formatter.write_str("Capture exceeds the configured chunk budget")
            }
        }
    }
}

/// RFC-0001 capture hygiene: remove invisible bidirectional/format controls so
/// they cannot smuggle instructions or alter the copied span. Deterministic so
/// the admission plan and the execution slices agree byte-for-byte.
pub(crate) fn sanitize_source(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            !matches!(
                *ch,
                '\u{200b}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}'
                    | '\u{feff}'
            )
        })
        .collect()
}

/// Plan chunks over the sanitized source using the qualified profile's exact
/// context and output token budgets. The whole input reserve is proven before
/// the first chunk is admitted; no chunk can exceed the bound.
pub(crate) fn plan_chunks(
    text: &str,
    context_tokens: u32,
    output_tokens: u32,
) -> Result<Vec<ChunkPlan>, PlanError> {
    let framing = PROMPT
        .len()
        .saturating_add(SCHEMA.len())
        .saturating_add(FRAMING_RESERVE);
    let source_budget = (context_tokens as usize)
        .saturating_sub(output_tokens as usize)
        .saturating_sub(framing);
    if source_budget < MIN_SOURCE_BUDGET {
        return Err(PlanError::Budget(
            "Context budget cannot hold the bundled prompt, output reserve, and a source chunk"
                .into(),
        ));
    }
    let max = source_budget;
    let overlap = 300usize.min(max / 8);
    let mut result = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + max).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end <= start {
            return Err(PlanError::Budget(
                "Could not split capture at a UTF-8 boundary".into(),
            ));
        }
        result.push(ChunkPlan {
            index: result.len() as u32,
            start,
            end,
        });
        if result.len() > MAX_CHUNKS {
            return Err(PlanError::TooManyChunks);
        }
        if end == text.len() {
            break;
        }
        start = end.saturating_sub(overlap);
        while !text.is_char_boundary(start) {
            start += 1;
        }
    }
    if result.is_empty() {
        result.push(ChunkPlan {
            index: 0,
            start: 0,
            end: 0,
        });
    }
    Ok(result)
}

/// Canonical dependency vector for one capture job. The source variant is
/// immutable once saved, so its raw digest is the stable revision that the
/// completion recheck binds to.
pub(crate) fn dependency_vector(occurrence_id: &str, variant_id: &str, raw_sha256: &str) -> Value {
    Value::Array(vec![
        json!({"kind":"source_variant","id":variant_id,"revision":raw_sha256}),
        json!({"kind":"source_occurrence","id":occurrence_id,"revision":variant_id}),
    ])
}

pub(crate) fn dependency_digest(
    occurrence_id: &str,
    variant_id: &str,
    raw_sha256: &str,
) -> Result<String, WorkspaceError> {
    super::coach::fingerprint(&dependency_vector(occurrence_id, variant_id, raw_sha256))
}

/// The frozen mutation input for an accepted Extractor job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtractInput {
    pub(crate) occurrence_id: String,
    pub(crate) source_variant_id: String,
    pub(crate) locale: String,
    pub(crate) prompt_version: String,
    pub(crate) prompt: String,
    pub(crate) chunks: Vec<ChunkPlan>,
}

/// Rebuild the per-chunk text from the raw source exactly as planned. The
/// source variant is immutable, so a stored plan always resolves.
pub(crate) fn chunk_text(raw: &str, plan: &ChunkPlan) -> String {
    let sanitized = sanitize_source(raw);
    let end = plan.end.min(sanitized.len());
    let start = plan.start.min(end);
    sanitized[start..end].to_owned()
}

/// Host-side deterministic candidate validation (mirrors the RFC-0001 S04
/// rules that previously lived on the direct provider path).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Candidate {
    pub(crate) kind: String,
    pub(crate) statement: String,
    pub(crate) quote: String,
    pub(crate) mention_name: Option<String>,
    pub(crate) mention_kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtractorOutput {
    pub(crate) candidates: Vec<Candidate>,
    pub(crate) injection_suspected: bool,
}

pub(crate) fn valid_kind(kind: &str) -> bool {
    matches!(kind, "signal" | "interest" | "hook" | "stakeholder")
}

fn has_sensitive_content(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "medical leave",
        "health condition",
        "disability",
        "pregnant",
        "religion",
        "political party",
        "sexual orientation",
        "home address",
        "family status",
        "union member",
        "immigration status",
    ]
    .iter()
    .any(|term| lower.contains(term))
}

pub(crate) fn valid_candidate(candidate: &Candidate) -> bool {
    valid_kind(&candidate.kind)
        && !candidate.statement.trim().is_empty()
        && candidate.statement.chars().count() <= 1000
        && !candidate.quote.is_empty()
        && candidate.quote.chars().count() <= 4000
        && candidate
            .mention_name
            .as_ref()
            .is_none_or(|name| !name.trim().is_empty() && name.chars().count() <= 500)
        && candidate
            .mention_kind
            .as_deref()
            .is_none_or(|kind| matches!(kind, "person" | "org"))
        && (candidate.mention_name.is_some() || candidate.mention_kind.is_none())
        && !has_sensitive_content(&candidate.statement)
        && !has_sensitive_content(&candidate.quote)
}

pub(crate) fn injection_suspected(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "ignore previous",
        "system prompt",
        "you are now",
        "<|system|>",
        "<|assistant|>",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || text.len() > 1_500_000
}

/// Typed capture admission. Saves the source and, when a qualified structured
/// profile is active and the queue has capacity, admits one durable Extractor
/// job in the same writer transaction. Always returns the saved source; never
/// fails the capture because inference is unavailable.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn save_capture_commit(
    workspace: &Workspace,
    key: &str,
    canonical: &str,
    text: &str,
    title: Option<&str>,
    url: Option<&str>,
) -> Result<(Option<String>, Value), WorkspaceError> {
    if key.trim().is_empty() || key.len() > 100 {
        return Err(WorkspaceError::Invalid(
            "idempotency key must contain 1–100 bytes".into(),
        ));
    }
    if text.trim().is_empty() {
        return Err(WorkspaceError::Invalid("source text is empty".into()));
    }
    if text.len() > 2_000_000 {
        return Err(WorkspaceError::Invalid("source exceeds 2 MB".into()));
    }
    let normalized = parley_domain::evidence::normalize(text);
    let normalized_hash = digest(normalized.as_bytes());
    let raw_hash = digest(text.as_bytes());
    let mut tx = workspace.pool().begin_with("BEGIN IMMEDIATE").await?;
    // Replay checks run again under the writer reservation: a concurrent
    // same-key commit cannot interleave a second occurrence or job (BQ1).
    if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>(
        "SELECT canonical_digest,result_json FROM idempotency_receipt
         WHERE owner_id=1 AND epoch=? AND key=?",
    )
    .bind(workspace.epoch())
    .bind(key)
    .fetch_optional(&mut *tx)
    .await?
    {
        if old_digest != canonical {
            return Err(WorkspaceError::IdempotencyConflict);
        }
        let value: Value =
            serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let job_id = value["job_id"].as_str().map(str::to_owned);
        tx.commit().await?;
        return Ok((job_id, value));
    }
    let content_id = match sqlx::query_scalar::<_, String>(
        "SELECT id FROM source_content WHERE normalized_sha256=? AND normalization_version=? ORDER BY id LIMIT 1",
    )
    .bind(&normalized_hash)
    .bind(parley_domain::evidence::NORMALIZATION_VERSION as i64)
    .fetch_optional(&mut *tx)
    .await?
    {
        Some(id) => id,
        None => {
            let id = new_id(&normalized_hash);
            sqlx::query(
                "INSERT INTO source_content(id,normalized_sha256,normalization_version,normalized_text,created_at) VALUES(?,?,?,?,?)",
            )
            .bind(&id)
            .bind(&normalized_hash)
            .bind(parley_domain::evidence::NORMALIZATION_VERSION as i64)
            .bind(&normalized)
            .bind(now_utc())
            .execute(&mut *tx)
            .await?;
            id
        }
    };
    let variant_id = match sqlx::query_scalar::<_, String>(
        "SELECT id FROM source_variant WHERE content_id=? AND raw_sha256=? ORDER BY id LIMIT 1",
    )
    .bind(&content_id)
    .bind(&raw_hash)
    .fetch_optional(&mut *tx)
    .await?
    {
        Some(id) => id,
        None => {
            let id = new_id(&raw_hash);
            sqlx::query(
                "INSERT INTO source_variant(id,content_id,raw_sha256,raw_text,title,url,captured_at) VALUES(?,?,?,?,?,?,?)",
            )
            .bind(&id)
            .bind(&content_id)
            .bind(&raw_hash)
            .bind(text)
            .bind(title)
            .bind(url)
            .bind(now_utc())
            .execute(&mut *tx)
            .await?;
            id
        }
    };
    let occurrence_id = new_id(&variant_id);
    sqlx::query(
        "INSERT INTO source_occurrence(id,source_variant_id,actor_kind,actor_entity_id,captured_at) VALUES(?,?, 'unknown',NULL,?)",
    )
    .bind(&occurrence_id)
    .bind(&variant_id)
    .bind(now_utc())
    .execute(&mut *tx)
    .await?;
    let event_payload = json!({
        "content_id": content_id,
        "variant_id": variant_id,
        "occurrence_id": occurrence_id,
        "normalized_sha256": normalized_hash,
        "raw_sha256": raw_hash,
    });
    let captured_cursor: i64 = sqlx::query_scalar(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'source',?,1,'source.captured',?) RETURNING cursor",
    )
    .bind(new_id(&occurrence_id))
    .bind(now_utc())
    .bind(&occurrence_id)
    .bind(event_payload.to_string())
    .fetch_one(&mut *tx)
    .await?;

    // Qualified structured profile, or save the source on its own.
    let profile = profile_for_intake(&mut tx).await?;
    let epoch = workspace.epoch().to_owned();
    let (job_id, job_status, last_cursor) =
        if let Some((revision, context_tokens, output_tokens, locale)) = profile {
            match plan_chunks(&sanitize_source(text), context_tokens, output_tokens) {
                Ok(chunks) => {
                    admit_job_in_tx(
                        &mut tx,
                        &epoch,
                        &occurrence_id,
                        &variant_id,
                        &raw_hash,
                        revision,
                        locale,
                        chunks,
                        captured_cursor,
                    )
                    .await?
                }
                Err(_) => (None, "not_queued".to_owned(), captured_cursor),
            }
        } else {
            (None, "not_queued".to_owned(), captured_cursor)
        };

    let full = json!({
        "content_id": content_id,
        "variant_id": variant_id,
        "occurrence_id": occurrence_id,
        "normalized_sha256": normalized_hash,
        "raw_sha256": raw_hash,
        "job_id": job_id,
        "job_status": job_status,
    });
    sqlx::query(
        "INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,201,?,?,?)",
    )
    .bind(workspace.epoch())
    .bind(key)
    .bind(canonical)
    .bind(full.to_string())
    .bind(last_cursor)
    .bind(now_utc())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((job_id, full))
}

type ProfileForIntake = Option<(i64, u32, u32, String)>;

async fn profile_for_intake(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<ProfileForIntake, WorkspaceError> {
    let row: Option<(i64, i64, i64, String)> = sqlx::query_as(
        "SELECT a.profile_revision,p.context_tokens,p.output_tokens,p.locale
         FROM inference_active_profile a
         JOIN inference_qualification q ON q.profile_revision=a.profile_revision AND q.mode='structured_role'
         JOIN inference_profile p ON p.revision=a.profile_revision
         WHERE a.singleton=1 AND q.status='ready' AND q.checked_at >= ?
           AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=a.profile_revision)
         ORDER BY q.checked_at DESC LIMIT 1",
    )
    .bind(now_utc_offset(-300))
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|(revision, context, output, locale)| {
        (revision, context as u32, output as u32, locale)
    }))
}

#[allow(clippy::too_many_arguments)]
async fn admit_job_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    epoch: &str,
    occurrence_id: &str,
    variant_id: &str,
    raw_sha256: &str,
    profile_revision: i64,
    locale: String,
    chunks: Vec<ChunkPlan>,
    captured_cursor: i64,
) -> Result<(Option<String>, String, i64), WorkspaceError> {
    let in_flight: i64 =
        sqlx::query_scalar("SELECT count(*) FROM durable_job WHERE status IN ('queued','running')")
            .fetch_one(&mut **tx)
            .await?;
    if in_flight >= 32 {
        return Ok((None, "not_queued".to_owned(), captured_cursor));
    }
    let (prompt_version, prompt): (String, String) = sqlx::query_as(
        "SELECT p.version_id,p.prompt_text FROM agent_prompt_active a
         JOIN agent_prompt_version p ON p.version_id=a.version_id WHERE a.role='extractor'",
    )
    .fetch_optional(&mut **tx)
    .await?
    .unwrap_or_else(|| ("extractor-v1".into(), PROMPT.into()));
    let input = ExtractInput {
        occurrence_id: occurrence_id.to_owned(),
        source_variant_id: variant_id.to_owned(),
        locale,
        prompt_version,
        prompt,
        chunks,
    };
    let input_json = serde_json::to_string(&input)
        .map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
    let input_sha256 = digest(input_json.as_bytes());
    let owner_pseudonym = digest(format!("parley-owner:1:{epoch}").as_bytes());
    let dependencies = dependency_digest(occurrence_id, variant_id, raw_sha256)?;
    let equivalence = super::coach::fingerprint(&json!({
        "occurrence_id": occurrence_id,
        "source_variant_id": variant_id,
        "profile_revision": profile_revision,
        "chunks": input.chunks,
    }))?;
    let id = new_id(&input_sha256);
    let now = now_utc();
    sqlx::query(
        "INSERT INTO durable_job(
             id,role,status,input_json,input_sha256,source_occurrence_id,source_variant_id,
             scope_kind,scope_id,scope_revision,current_attempt,max_attempts,epoch,run_id,
             owner_pseudonym,profile_revision,dependency_json,dependency_digest,equivalence_key,
             eligible_at,queue_deadline_at,created_at,updated_at)
         VALUES(?,?,'queued',?,?,?,?, 'source',?,1,0,3,?,NULL,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind("extractor")
    .bind(&input_json)
    .bind(&input_sha256)
    .bind(occurrence_id)
    .bind(variant_id)
    .bind(occurrence_id)
    .bind(epoch)
    .bind(&owner_pseudonym)
    .bind(profile_revision)
    .bind(dependency_vector(occurrence_id, variant_id, raw_sha256).to_string())
    .bind(&dependencies)
    .bind(&equivalence)
    .bind(&now)
    .bind(now_utc_offset(300))
    .bind(&now)
    .bind(&now)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO inference_job_dependency(job_id,kind,ref_id,revision) VALUES(?,?,?,?)",
    )
    .bind(&id)
    .bind("source_variant")
    .bind(variant_id)
    .bind(raw_sha256)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO inference_job_dependency(job_id,kind,ref_id,revision) VALUES(?,?,?,?)",
    )
    .bind(&id)
    .bind("source_occurrence")
    .bind(occurrence_id)
    .bind(variant_id)
    .execute(&mut **tx)
    .await?;
    let job_cursor: i64 = sqlx::query_scalar(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'job',?,1,'job.queued',?) RETURNING cursor",
    )
    .bind(new_id(&id))
    .bind(&now)
    .bind(&id)
    .bind(json!({"role":"extractor","source_occurrence_id":occurrence_id}).to_string())
    .fetch_one(&mut **tx)
    .await?;
    Ok((Some(id), "queued".to_owned(), job_cursor))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(len: usize) -> String {
        "a".repeat(len)
    }

    #[test]
    fn ar06_plan_reserves_full_budget_before_first_chunk() {
        assert!(matches!(
            plan_chunks(&source(100), 512, 256),
            Err(PlanError::Budget(_))
        ));
        let plan = plan_chunks(&source(1000), 8192, 256).unwrap();
        assert_eq!(plan.len(), 1);
        let plan = plan_chunks(&source(40_000), 8192, 256).unwrap();
        assert!(plan.len() > 1);
        assert!(plan.len() <= MAX_CHUNKS);
        for window in plan.windows(2) {
            // Every chunk starts inside the previous chunk's overlap or at its
            // end, so a quote spanning the cut appears in both slices.
            assert!(window[1].start <= window[0].end);
            assert!(window[1].start < window[1].end);
        }
    }

    #[test]
    fn ar06_plan_rejects_sources_beyond_the_chunk_cap() {
        assert!(matches!(
            plan_chunks(&source(3_000_000), 8192, 256),
            Err(PlanError::TooManyChunks)
        ));
    }

    #[test]
    fn ar06_chunk_text_slices_sanitized_source_deterministically() {
        let raw = "a\u{200b}b c";
        let plan = plan_chunks(&sanitize_source(raw), 8192, 256).unwrap();
        let one = &plan[0];
        assert_eq!(chunk_text(raw, one), "ab c");
    }

    #[test]
    fn ar06_candidate_validation_matches_rfc_rules() {
        let good = Candidate {
            kind: "signal".into(),
            statement: "The team is expanding.".into(),
            quote: "The team is expanding.".into(),
            mention_name: None,
            mention_kind: None,
        };
        assert!(valid_candidate(&good));
        let bad_kind = Candidate {
            kind: "claim".into(),
            statement: "The team is expanding.".into(),
            quote: "The team is expanding.".into(),
            mention_name: None,
            mention_kind: None,
        };
        assert!(!valid_candidate(&bad_kind));
        let sensitive = Candidate {
            statement: "Their medical leave was sensitive.".into(),
            quote: "Their medical leave was sensitive.".into(),
            mention_name: None,
            mention_kind: None,
            kind: "signal".into(),
        };
        assert!(!valid_candidate(&sensitive));
    }
}
