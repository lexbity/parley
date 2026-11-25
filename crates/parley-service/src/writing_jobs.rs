//! S09 role execution. Model output remains versioned job data until an owner
//! explicitly saves or edits it. Deterministic lint policy stays in Rust; the
//! local model supplies bounded prose only. The matcher migrated to the
//! packaged worker in AR07 (`inference/matcher.rs`).
use super::Workspace;
use parley_domain::lint;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposerResult {
    body: String,
    sentences: Vec<ComposerSentence>,
    slot_fills: BTreeMap<String, String>,
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

pub(super) async fn process_role(workspace: &Workspace, job_id: &str, role: &str) {
    let started = tokio::time::Instant::now();
    let attempt = match super::jobs::claim_attempt(workspace, job_id).await {
        Ok(Some(attempt)) => attempt,
        _ => return,
    };
    if attempt.role != role {
        super::jobs::finish_attempt_error(
            workspace,
            job_id,
            attempt.number,
            "ROLE_MISMATCH",
            "Durable job role does not match the worker dispatch.",
            "failed",
        )
        .await;
        return;
    }
    let input: Value = match serde_json::from_str::<Value>(&attempt.input_json) {
        Ok(value) if value.is_object() => value,
        _ => {
            super::jobs::finish_attempt_error(
                workspace,
                job_id,
                attempt.number,
                "INVALID_INPUT",
                "Durable role input is not a JSON object.",
                "invalid",
            )
            .await;
            return;
        }
    };
    if attempt.provider_config.model.trim().is_empty() || attempt.model_digest.is_none() {
        super::jobs::finish_attempt_error(
            workspace,
            job_id,
            attempt.number,
            "MODEL_HEALTH_REQUIRED",
            "Choose and health-check the local model before running this role.",
            "failed",
        )
        .await;
        return;
    }
    let result = tokio::time::timeout(Duration::from_secs(120), async {
        match role {
            "composer" => run_composer(workspace, &attempt, &input).await,
            "linter" => run_linter(workspace, &attempt, &input).await,
            _ => Err(RoleFailure::new(
                "ROLE_UNSUPPORTED",
                "No S09 handler is registered for this role.",
            )),
        }
    })
    .await;
    let (value, tokens_in, tokens_out) = match result {
        Ok(Ok(value)) => value,
        Ok(Err(failure)) => {
            let disposition = if failure.retryable && attempt.number < 3 {
                "retry"
            } else if failure.invalid {
                "invalid"
            } else {
                "failed"
            };
            super::jobs::finish_attempt_error(
                workspace,
                job_id,
                attempt.number,
                failure.code,
                &failure.message,
                disposition,
            )
            .await;
            return;
        }
        Err(_) => {
            super::jobs::finish_attempt_error(
                workspace,
                job_id,
                attempt.number,
                "ROLE_TIMEOUT",
                "Writing role exceeded its 120 second attempt budget.",
                "failed",
            )
            .await;
            return;
        }
    };
    if started.elapsed() > Duration::from_secs(360) {
        super::jobs::finish_attempt_error(
            workspace,
            job_id,
            attempt.number,
            "JOB_TIMEOUT",
            "Writing role exceeded its total time budget.",
            "failed",
        )
        .await;
        return;
    }
    match complete_if_current(
        workspace,
        job_id,
        attempt.number,
        &attempt.input_sha256,
        &input,
        &value,
        tokens_in,
        tokens_out,
    )
    .await
    {
        Ok(()) => {}
        Err(error) => {
            super::jobs::finish_attempt_error(
                workspace,
                job_id,
                attempt.number,
                "STALE_INPUT",
                &error,
                "failed",
            )
            .await
        }
    }
}

pub(super) struct RoleFailure {
    pub(super) code: &'static str,
    pub(super) message: String,
    retryable: bool,
    invalid: bool,
}
impl RoleFailure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            invalid: false,
        }
    }
    fn provider(message: impl Into<String>) -> Self {
        Self {
            code: "PROVIDER_FAILED",
            message: message.into(),
            retryable: true,
            invalid: false,
        }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "OUTPUT_INVALID",
            message: message.into(),
            retryable: true,
            invalid: true,
        }
    }
}
type RoleResult = Result<(Value, u32, u32), RoleFailure>;

async fn chat(
    _workspace: &Workspace,
    attempt: &super::jobs::Attempt,
    content: &str,
    temperature: f32,
) -> Result<(String, u32, u32), RoleFailure> {
    let response = super::jobs::ollama_chat(
        &attempt.provider_config,
        content,
        &attempt.prompt,
        &attempt.schema,
        temperature,
    )
    .await
    .map_err(RoleFailure::provider)?;
    let input = response
        .prompt_eval_count
        .unwrap_or_else(|| content.len().min(u32::MAX as usize) as u32);
    let output = response.eval_count.unwrap_or(0);
    Ok((response.message.content, input, output))
}

async fn run_composer(
    workspace: &Workspace,
    attempt: &super::jobs::Attempt,
    input: &Value,
) -> RoleResult {
    let content = format!(
        "Compose only from the approved slot fills in this snapshot. Preserve every slot value exactly. Cite references only to supplied records. Return one review candidate; do not approve or release it.\n<INPUT>\n{}\n</INPUT>",
        input
    );
    let (text, tokens_in, tokens_out) = chat(workspace, attempt, &content, 0.3).await?;
    let parsed: ComposerResult = serde_json::from_str(&text).map_err(|error| {
        RoleFailure::invalid(format!("composer schema validation failed: {error}"))
    })?;
    let fills = input
        .get("slot_fills")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            RoleFailure::new(
                "COMPOSER_INPUT_INVALID",
                "Composer snapshot lacks slot fills.",
            )
        })?;
    let mut expected = BTreeMap::new();
    let mut reference_ids = BTreeSet::new();
    for fill in fills {
        let slot = fill
            .get("slot")
            .and_then(Value::as_str)
            .ok_or_else(|| RoleFailure::new("COMPOSER_INPUT_INVALID", "Slot name missing."))?;
        let value = fill
            .get("value")
            .and_then(Value::as_str)
            .ok_or_else(|| RoleFailure::new("COMPOSER_INPUT_INVALID", "Slot value missing."))?;
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
        return Err(RoleFailure::invalid(
            "composer changed, omitted, or failed to preserve an approved slot value",
        ));
    }
    let sentence_text = parsed
        .sentences
        .iter()
        .map(|sentence| sentence.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if sentence_text.trim().is_empty() {
        return Err(RoleFailure::invalid(
            "composer output has no sentence-level claim audit",
        ));
    }
    for sentence in &parsed.sentences {
        if !parsed.body.contains(&sentence.text) {
            return Err(RoleFailure::invalid(
                "composer sentence audit does not match the generated body",
            ));
        }
        for claim in &sentence.claims {
            if !matches!(
                claim.kind.as_str(),
                "self_fact" | "other_fact" | "offer" | "question" | "ask" | "generic"
            ) {
                return Err(RoleFailure::invalid("composer used an unknown claim type"));
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
                return Err(RoleFailure::invalid(
                    "composer used an unknown claim reference kind",
                ));
            }
            if !reference_ids.contains(&claim.r#ref.id)
                && claim.r#ref.kind != "interest"
                && claim.r#ref.kind != "ladder"
            {
                return Err(RoleFailure::invalid(
                    "composer claim references a record outside the approved input",
                ));
            }
            if claim.kind == "other_fact"
                && input
                    .get("tier2_interest_ids")
                    .and_then(Value::as_array)
                    .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(&claim.r#ref.id)))
            {
                return Err(RoleFailure::invalid(
                    "a tier-2 interest cannot support an asserted fact",
                ));
            }
        }
    }
    let protected = expected.values().cloned().collect::<Vec<_>>();
    if protected.iter().any(|token| !parsed.body.contains(token)) {
        return Err(RoleFailure::invalid(
            "protected slot token was not preserved",
        ));
    }
    let sentences=parsed.sentences.into_iter().map(|sentence|json!({"text":sentence.text,"claims":sentence.claims.into_iter().map(|claim|json!({"type":claim.kind,"ref":{"kind":claim.r#ref.kind,"id":claim.r#ref.id}})).collect::<Vec<_>>(),"rules":sentence.rules})).collect::<Vec<_>>();
    Ok((
        json!({"body":parsed.body,"sentences":sentences,"slot_fills":parsed.slot_fills,"status":"draft","owner_review_required":true,"released":false,"prompt_id":attempt.prompt_id}),
        tokens_in,
        tokens_out,
    ))
}

async fn run_linter(
    workspace: &Workspace,
    attempt: &super::jobs::Attempt,
    input: &Value,
) -> RoleResult {
    let body = input
        .get("body")
        .and_then(Value::as_str)
        .ok_or_else(|| RoleFailure::new("LINTER_INPUT_INVALID", "Linter snapshot has no body."))?;
    let content = format!(
        "Review the draft against the owner voice samples. Return at most three advisory findings. Suggested rewrites must preserve every number, name, URL and quoted span from the affected text.\n<INPUT>\n{}\n</INPUT>",
        input
    );
    let (text, tokens_in, tokens_out) = chat(workspace, attempt, &content, 0.2).await?;
    let parsed: LinterResult = serde_json::from_str(&text).map_err(|error| {
        RoleFailure::invalid(format!("linter schema validation failed: {error}"))
    })?;
    if parsed.findings.len() > 3 {
        return Err(RoleFailure::invalid(
            "linter returned more than three advisory findings",
        ));
    }
    let mut accepted = Vec::new();
    for finding in parsed.findings {
        if finding.start_byte > finding.end_byte
            || finding.end_byte > body.len()
            || !body.is_char_boundary(finding.start_byte)
            || !body.is_char_boundary(finding.end_byte)
            || finding.issue.trim().is_empty()
        {
            return Err(RoleFailure::invalid(
                "linter finding has invalid span or empty issue",
            ));
        }
        let original = &body[finding.start_byte..finding.end_byte];
        if !lint::preserves_protected_tokens(original, &finding.suggested_rewrite) {
            continue;
        }
        accepted.push(json!({"start_byte":finding.start_byte,"end_byte":finding.end_byte,"issue":finding.issue,"suggested_rewrite":finding.suggested_rewrite,"advisory":true}));
    }
    Ok((
        json!({"findings":accepted,"deterministic_findings":input.get("deterministic_findings").cloned().unwrap_or(Value::Array(Vec::new())),"content_sha256":input.get("content_sha256"),"version":input.get("version"),"advisory_only":true}),
        tokens_in,
        tokens_out,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn complete_if_current(
    workspace: &Workspace,
    job_id: &str,
    attempt_no: i64,
    input_hash: &str,
    input: &Value,
    result: &Value,
    tokens_in: u32,
    tokens_out: u32,
) -> Result<(), String> {
    let mut tx = workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| error.to_string())?;
    ensure_scope_current(&mut tx, job_id, input)
        .await
        .map_err(|failure| failure.message)?;
    // A delegated job must not publish after its initiating run is cancelled
    // or the workspace epoch rotated (UCR-001 U05/U07). Owner-launched jobs
    // pass the fence unchanged.
    crate::jobs::scoped_job_fence(&mut tx, job_id)
        .await
        .map_err(|_| "the initiating run is no longer active".to_owned())?;
    super::jobs::complete_role_result_in_tx(
        &mut tx, job_id, attempt_no, input_hash, result, tokens_in, tokens_out,
    )
    .await
    .map_err(|error| error.to_string())?;
    tx.commit().await.map_err(|error| error.to_string())
}

pub(crate) async fn ensure_scope_current(
    tx: &mut Transaction<'_, Sqlite>,
    job_id: &str,
    input: &Value,
) -> Result<(), RoleFailure> {
    let scope:Option<(String,String,i64)>=sqlx::query_as("SELECT scope_kind,scope_id,scope_revision FROM durable_job WHERE id=? AND status IN ('running','succeeded')").bind(job_id).fetch_optional(&mut **tx).await.map_err(|error|RoleFailure::new("PERSISTENCE_FAILED",error.to_string()))?;
    let Some((kind, id, expected)) = scope else {
        return Err(RoleFailure::new("STALE_INPUT", "Job is no longer active."));
    };
    let current: Option<i64> = match kind.as_str() {
        "opportunity" => sqlx::query_scalar("SELECT revision FROM opportunity WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| RoleFailure::new("PERSISTENCE_FAILED", error.to_string()))?,
        "segment" => sqlx::query_scalar("SELECT revision FROM segment WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| RoleFailure::new("PERSISTENCE_FAILED", error.to_string()))?,
        "material" => sqlx::query_scalar("SELECT head_version FROM material WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| RoleFailure::new("PERSISTENCE_FAILED", error.to_string()))?,
        "owner" => Some(1),
        _ => None,
    };
    if current != Some(expected) {
        return Err(RoleFailure::new(
            "STALE_INPUT",
            "The underlying opportunity, segment, material or owner revision changed while the model was running.",
        ));
    }
    // Bind secondary revision vectors too; scope revision alone is not enough
    // for resource or recipient edits that affect a generated proposal.
    if let Some(resources) = input.get("resources").and_then(Value::as_array) {
        for resource in resources {
            let id = resource
                .get("resource_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let expected = resource
                .get("revision")
                .and_then(Value::as_i64)
                .unwrap_or(-1);
            let current: Option<i64> = sqlx::query_scalar(
                "SELECT resource_content_revision FROM entity WHERE id=? AND readiness='ready' AND retired_at IS NULL",
            )
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| RoleFailure::new("PERSISTENCE_FAILED", error.to_string()))?;
            if current != Some(expected) {
                return Err(RoleFailure::new(
                    "STALE_INPUT",
                    "A selected resource changed or is no longer ready.",
                ));
            }
        }
    }
    if let Some(resources) = input.get("resource_revisions").and_then(Value::as_object) {
        for (id, expected_value) in resources {
            let expected = expected_value.as_i64().unwrap_or(-1);
            let current: Option<i64> = sqlx::query_scalar(
                "SELECT resource_content_revision FROM entity WHERE id=? AND readiness='ready' AND retired_at IS NULL",
            )
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| RoleFailure::new("PERSISTENCE_FAILED", error.to_string()))?;
            if current != Some(expected) {
                return Err(RoleFailure::new(
                    "STALE_INPUT",
                    "A selected resource changed or is no longer ready.",
                ));
            }
        }
    }
    if let Some(samples) = input
        .get("voice_sample_revisions")
        .and_then(Value::as_object)
    {
        for (id, expected_value) in samples {
            let expected = expected_value.as_i64().unwrap_or(-1);
            let current: Option<i64> = sqlx::query_scalar(
                "SELECT revision FROM voice_sample WHERE id=? AND status='active'",
            )
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| RoleFailure::new("PERSISTENCE_FAILED", error.to_string()))?;
            if current != Some(expected) {
                return Err(RoleFailure::new(
                    "STALE_INPUT",
                    "A selected voice sample changed or was retired.",
                ));
            }
        }
    }
    if let (Some(id), Some(expected)) = (
        input.get("recipient_id").and_then(Value::as_str),
        input.get("recipient_revision").and_then(Value::as_i64),
    ) {
        let current: Option<i64> = sqlx::query_scalar("SELECT revision FROM entity WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| RoleFailure::new("PERSISTENCE_FAILED", error.to_string()))?;
        if current != Some(expected) {
            return Err(RoleFailure::new(
                "STALE_INPUT",
                "The recipient changed while the model was running.",
            ));
        }
    }
    if let (Some(template_id), Some(exchange_version)) = (
        input.get("template_version_id").and_then(Value::as_str),
        input.get("exchange_version").and_then(Value::as_i64),
    ) {
        let active: Option<String> =
            sqlx::query_scalar("SELECT id FROM template_version WHERE id=? AND status='active'")
                .bind(template_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|error| RoleFailure::new("PERSISTENCE_FAILED", error.to_string()))?;
        if active.is_none() {
            return Err(RoleFailure::new(
                "STALE_INPUT",
                "The selected template is no longer active.",
            ));
        }
        let opportunity = input
            .get("opportunity_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let recipient = input
            .get("recipient_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let exchange:Option<i64>=sqlx::query_scalar("SELECT fairness_attested FROM exchange_version WHERE opportunity_id=? AND recipient_id=? AND version_no=?").bind(opportunity).bind(recipient).bind(exchange_version).fetch_optional(&mut **tx).await.map_err(|error|RoleFailure::new("PERSISTENCE_FAILED",error.to_string()))?;
        if exchange != Some(1) {
            return Err(RoleFailure::new(
                "STALE_INPUT",
                "The selected recipient exchange is no longer attested.",
            ));
        }
        let permitted:Option<i64>=sqlx::query_scalar("SELECT count(*) FROM opportunity o JOIN opportunity_party op ON op.opportunity_id=o.id JOIN party_policy p ON p.party_id=op.party_id WHERE o.id=? AND op.party_id=? AND o.state='open' AND p.state='open' AND p.do_not_contact=0").bind(opportunity).bind(recipient).fetch_optional(&mut **tx).await.map_err(|error|RoleFailure::new("PERSISTENCE_FAILED",error.to_string()))?;
        if permitted != Some(1) {
            return Err(RoleFailure::new(
                "STALE_INPUT",
                "The opportunity closed or the recipient is no longer eligible.",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[tokio::test]
    async fn completed_proposal_is_rejected_after_scoped_revision_changes() {
        let path = std::env::temp_dir().join(format!(
            "parley-s09-stale-job-{}-{}.sqlite",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let workspace = Workspace::open(&path).await.unwrap();
        sqlx::query("INSERT INTO segment(id,name,revision,created_at,updated_at) VALUES('segment-1','Segment',1,'now','now')")
            .execute(workspace.pool()).await.unwrap();
        sqlx::query("INSERT INTO durable_job(id,role,status,input_json,input_sha256,source_occurrence_id,source_variant_id,scope_kind,scope_id,scope_revision,current_attempt,max_attempts,progress_current,progress_total,created_at,updated_at) VALUES('job-1','template_builder','running','{}','hash',NULL,NULL,'segment','segment-1',1,1,3,0,0,'now','now')")
            .execute(workspace.pool()).await.unwrap();
        let input = json!({"segment_id":"segment-1"});
        let mut tx = workspace.pool().begin().await.unwrap();
        assert!(ensure_scope_current(&mut tx, "job-1", &input).await.is_ok());
        tx.commit().await.unwrap();

        sqlx::query("UPDATE segment SET revision=revision+1 WHERE id='segment-1'")
            .execute(workspace.pool())
            .await
            .unwrap();
        let mut tx = workspace.pool().begin().await.unwrap();
        let failure = ensure_scope_current(&mut tx, "job-1", &input)
            .await
            .unwrap_err();
        assert_eq!(failure.code, "STALE_INPUT");
        tx.rollback().await.unwrap();
        drop(workspace);
        let _ = fs::remove_file(path);
    }
}
