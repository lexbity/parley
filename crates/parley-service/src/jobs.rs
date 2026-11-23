//! Durable, bounded Extractor jobs. Model output remains staged until an owner
//! explicitly resolves proposal items through the review command.
use super::{Workspace, WorkspaceError, digest, new_id, now_utc};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use std::{net::IpAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
};

const PROMPT: &str = include_str!("../roles/extractor/prompt.md");
const SCHEMA: &str = include_str!("../roles/extractor/schema.json");
const INTERVIEWER_PROMPT: &str = include_str!("../roles/interviewer/prompt.md");
const INTERVIEWER_SCHEMA: &str = include_str!("../roles/interviewer/schema.json");
const BRIEFER_PROMPT: &str = include_str!("../roles/briefer/prompt.md");
const BRIEFER_SCHEMA: &str = include_str!("../roles/briefer/schema.json");
const COACH_PROMPT: &str = include_str!("../roles/coach/prompt.md");
const COACH_SCHEMA: &str = include_str!("../roles/coach/schema.json");
const MATCHER_PROMPT: &str = include_str!("../roles/matcher/prompt.md");
const MATCHER_SCHEMA: &str = include_str!("../roles/matcher/schema.json");
const BUILDER_PROMPT: &str = include_str!("../roles/template_builder/prompt.md");
const BUILDER_SCHEMA: &str = include_str!("../roles/template_builder/schema.json");
const COMPOSER_PROMPT: &str = include_str!("../roles/composer/prompt.md");
const COMPOSER_SCHEMA: &str = include_str!("../roles/composer/schema.json");
const LINTER_PROMPT: &str = include_str!("../roles/linter/prompt.md");
const LINTER_SCHEMA: &str = include_str!("../roles/linter/schema.json");
const MAX_BODY: usize = 2_200_000;
const MAX_CHUNKS: usize = 16;
const MAX_RETRIES: i64 = 3;

#[derive(Clone)]
pub(crate) struct Queue(mpsc::Sender<String>);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub endpoint: String,
    pub model: String,
    pub num_ctx: u32,
    pub num_predict: u32,
    pub locale: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub role: String,
    pub status: String,
    pub current_attempt: i64,
    pub max_attempts: i64,
    pub progress_current: i64,
    pub progress_total: i64,
    pub injection_suspected: bool,
    pub last_error_code: Option<String>,
    pub last_error: Option<String>,
    pub source_occurrence_id: Option<String>,
    pub scope_kind: String,
    pub scope_id: String,
    pub proposal_batch_id: Option<String>,
    pub proposals: Vec<ProposalRecord>,
    pub result: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposalRecord {
    pub id: String,
    pub kind: String,
    pub candidate: Value,
    pub status: String,
}

type JobReadRow = (
    String,
    String,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
    i64,
    i64,
);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    kind: String,
    statement: String,
    quote: String,
    mention_name: Option<String>,
    mention_kind: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractorOutput {
    candidates: Vec<Candidate>,
    injection_suspected: bool,
    #[serde(skip)]
    dropped_items: u32,
}

#[derive(Deserialize)]
pub(crate) struct OllamaChatResponse {
    pub(crate) message: OllamaMessage,
    #[serde(default)]
    pub(crate) prompt_eval_count: Option<u32>,
    #[serde(default)]
    pub(crate) eval_count: Option<u32>,
}
#[derive(Deserialize)]
pub(crate) struct OllamaMessage {
    pub(crate) content: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderUpdate {
    endpoint: String,
    model: String,
    num_ctx: u32,
    num_predict: u32,
    locale: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProposalResolution {
    decisions: Vec<ProposalDecisionInput>,
    opportunity_id: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProposalDecisionInput {
    item_id: String,
    action: String,
    target_entity_id: Option<String>,
    corrected_statement: Option<String>,
}

pub(super) fn routes() -> Router<super::AppState> {
    Router::new()
        .route(
            "/api/v1/agent/provider",
            get(get_provider).put(update_provider),
        )
        .route("/api/v1/agent/provider/health", post(provider_health))
        .route("/api/v1/jobs/{id}", get(get_job).post(cancel_job))
        .route(
            "/api/v1/captures/{occurrence_id}/proposals",
            get(get_capture_proposals),
        )
        .route(
            "/api/v1/proposals/{batch_id}/resolve",
            post(resolve_proposals),
        )
}

pub(super) fn start(
    workspace: Arc<Workspace>,
    depth: usize,
) -> (Queue, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(depth);
    let handle = tokio::spawn(worker(workspace, rx));
    (Queue(tx), handle)
}

pub(super) fn role_allowed(role: &str) -> bool {
    matches!(
        role,
        "extractor"
            | "interviewer"
            | "briefer"
            | "coach"
            | "matcher"
            | "template_builder"
            | "composer"
            | "linter"
    )
}

#[allow(dead_code)] // Public to other service role adapters added in later RFC-0002 slices.
#[allow(clippy::too_many_arguments)] // The typed role command is the shared cross-slice worker boundary.
pub(crate) async fn enqueue_role(
    workspace: &Workspace,
    queue: &Queue,
    role: &str,
    scope_kind: &str,
    scope_id: &str,
    scope_revision: i64,
    input: &Value,
    max_attempts: i64,
) -> Result<String, WorkspaceError> {
    if !role_allowed(role) || role == "extractor" {
        return Err(WorkspaceError::Invalid(
            "agent role is not an enqueueable assistant role".into(),
        ));
    }
    if !matches!(
        scope_kind,
        "owner" | "party" | "segment" | "opportunity" | "material"
    ) || scope_id.is_empty()
        || scope_revision < 1
        || !(1..=3).contains(&max_attempts)
    {
        return Err(WorkspaceError::Invalid(
            "assistant job scope or retry budget is invalid".into(),
        ));
    }
    let input_json =
        serde_json::to_string(input).map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
    if input_json.len() > MAX_BODY {
        return Err(WorkspaceError::Invalid(
            "assistant job input exceeds the bounded size limit".into(),
        ));
    }
    let input_hash = digest(input_json.as_bytes());
    let id = new_id(&format!("{role}:{scope_kind}:{scope_id}:{input_hash}"));
    let mut tx = workspace.pool().begin_with("BEGIN IMMEDIATE").await?;
    enqueue_role_in_tx(
        &mut tx,
        role,
        scope_kind,
        scope_id,
        scope_revision,
        input,
        max_attempts,
    )
    .await?;
    tx.commit().await?;
    wake(queue, &id);
    Ok(id)
}

pub(crate) fn wake(queue: &Queue, id: &str) {
    let _ = queue.0.try_send(id.to_owned());
}

pub(crate) async fn enqueue_role_in_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    role: &str,
    scope_kind: &str,
    scope_id: &str,
    scope_revision: i64,
    input: &Value,
    max_attempts: i64,
) -> Result<String, WorkspaceError> {
    if !role_allowed(role) || role == "extractor" {
        return Err(WorkspaceError::Invalid(
            "agent role is not an enqueueable assistant role".into(),
        ));
    }
    if !matches!(
        scope_kind,
        "owner" | "party" | "segment" | "opportunity" | "material"
    ) || scope_id.is_empty()
        || scope_revision < 1
        || !(1..=3).contains(&max_attempts)
    {
        return Err(WorkspaceError::Invalid(
            "assistant job scope or retry budget is invalid".into(),
        ));
    }
    let input_json =
        serde_json::to_string(input).map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
    if input_json.len() > MAX_BODY {
        return Err(WorkspaceError::Invalid(
            "assistant job input exceeds the bounded size limit".into(),
        ));
    }
    let in_flight: i64 =
        sqlx::query_scalar("SELECT count(*) FROM durable_job WHERE status IN ('queued','running')")
            .fetch_one(&mut **tx)
            .await?;
    if in_flight >= 32 {
        return Err(WorkspaceError::Invalid("agent queue is at capacity".into()));
    }
    let input_hash = digest(input_json.as_bytes());
    let id = new_id(&format!("{role}:{scope_kind}:{scope_id}:{input_hash}"));
    let now = now_utc();
    sqlx::query("INSERT INTO durable_job(id,role,status,input_json,input_sha256,scope_kind,scope_id,scope_revision,max_attempts,created_at,updated_at) VALUES(?,?,'queued',?,?,?,?,?,?,?,?)")
        .bind(&id).bind(role).bind(&input_json).bind(&input_hash).bind(scope_kind).bind(scope_id).bind(scope_revision).bind(max_attempts).bind(&now).bind(&now).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'job',?,1,'job.queued',?)")
        .bind(new_id(&id)).bind(&now).bind(&id).bind(json!({"role":role,"scope_kind":scope_kind,"scope_id":scope_id,"scope_revision":scope_revision}).to_string()).execute(&mut **tx).await?;
    Ok(id)
}

async fn worker(workspace: Arc<Workspace>, mut rx: mpsc::Receiver<String>) {
    recover_interrupted(&workspace).await;
    let mut poll = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            id = rx.recv() => if id.is_none() { return } else if let Some(job_id)=id { process(&workspace,&job_id).await; },
            _ = poll.tick() => {
                let queued: Vec<String> = sqlx::query_scalar("SELECT id FROM durable_job WHERE status='queued' ORDER BY created_at,id LIMIT 1")
                    .fetch_all(workspace.pool()).await.unwrap_or_default();
                if let Some(id)=queued.first() { process(&workspace,id).await; }
            }
        }
    }
}

async fn recover_interrupted(workspace: &Workspace) {
    let Ok(mut tx) = workspace.pool().begin_with("BEGIN IMMEDIATE").await else {
        return;
    };
    let rows: Vec<(String, i64, i64)> = match sqlx::query_as(
        "SELECT id,current_attempt,max_attempts FROM durable_job WHERE status='running' ORDER BY created_at,id",
    ).fetch_all(&mut *tx).await {
        Ok(rows) => rows,
        Err(_) => { let _ = tx.rollback().await; return; }
    };
    for (id, attempt, maximum) in rows {
        let now = now_utc();
        let next_status = if attempt < maximum {
            "queued"
        } else {
            "failed"
        };
        let code = "INTERRUPTED";
        let message = "Worker stopped before this attempt completed";
        let attempt_update = sqlx::query("UPDATE job_attempt SET status='failed',error_code=?,error_message=?,finished_at=? WHERE job_id=? AND attempt_no=? AND status='running'")
            .bind(code).bind(message).bind(&now).bind(&id).bind(attempt).execute(&mut *tx).await;
        let job_update = sqlx::query("UPDATE durable_job SET status=?,last_error_code=?,last_error=?,updated_at=?,completed_at=? WHERE id=? AND status='running' AND current_attempt=?")
            .bind(next_status).bind(code).bind(message).bind(&now)
            .bind((next_status == "failed").then_some(now.as_str())).bind(&id).bind(attempt)
            .execute(&mut *tx).await;
        if attempt_update.is_err() || job_update.is_err() {
            let _ = tx.rollback().await;
            return;
        }
        let event = sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'job',?,?,?,?)")
            .bind(new_id(&id)).bind(&now).bind(&id).bind(attempt)
            .bind(if next_status == "queued" { "job.interrupted_retry" } else { "job.interrupted_failed" })
            .bind(json!({"error_code":code,"next_status":next_status}).to_string())
            .execute(&mut *tx).await;
        if event.is_err() {
            let _ = tx.rollback().await;
            return;
        }
    }
    let _ = tx.commit().await;
}

async fn process(workspace: &Workspace, job_id: &str) {
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM durable_job WHERE id=?")
        .bind(job_id)
        .fetch_optional(workspace.pool())
        .await
        .ok()
        .flatten();
    if let Some(role) = role.as_deref().filter(|role| *role != "extractor") {
        // One active local inference: delegated runs hold the same permit.
        let inference = workspace.inference();
        let _inference = inference.acquire().await;
        if matches!(role, "matcher" | "template_builder" | "composer" | "linter") {
            super::writing_jobs::process_role(workspace, job_id, role).await;
        } else {
            super::assistant::process_role(workspace, job_id, role).await;
        }
        return;
    }
    let started = tokio::time::Instant::now();
    loop {
        if started.elapsed() >= Duration::from_secs(360) {
            finish_job_error(
                workspace,
                job_id,
                "JOB_TIMEOUT",
                "Extractor exceeded its total time budget",
                "failed",
            )
            .await;
            return;
        }
        let attempt = match claim_attempt(workspace, job_id).await {
            Ok(Some(value)) => value,
            _ => return,
        };
        let source: Option<(String, String)> = sqlx::query_as("SELECT v.raw_text,j.input_json FROM durable_job j JOIN source_variant v ON v.id=j.source_variant_id WHERE j.id=? AND j.current_attempt=?")
            .bind(job_id).bind(attempt.number).fetch_optional(workspace.pool()).await.ok().flatten();
        let Some((raw_text, input_json)) = source else {
            finish_attempt_error(
                workspace,
                job_id,
                attempt.number,
                "SOURCE_UNAVAILABLE",
                "Capture source is no longer available",
                "failed",
            )
            .await;
            return;
        };
        let config = attempt.provider_config.clone();
        if config.model.trim().is_empty() {
            finish_attempt_error(
                workspace,
                job_id,
                attempt.number,
                "MODEL_UNAVAILABLE",
                "Choose an installed local Extractor model in Settings",
                "failed",
            )
            .await;
            return;
        }
        if attempt.model_digest.is_none() {
            finish_attempt_error(
                workspace,
                job_id,
                attempt.number,
                "MODEL_HEALTH_REQUIRED",
                "Check the installed local model in Settings before running extraction",
                "failed",
            )
            .await;
            return;
        }
        let sanitized = strip_invisible_controls(&raw_text);
        let suspicious = injection_suspected(&sanitized);
        let chunks = match chunks(&sanitized, config.num_ctx, config.num_predict) {
            Ok(value) => value,
            Err(message) => {
                finish_attempt_error(
                    workspace,
                    job_id,
                    attempt.number,
                    "CONTEXT_BUDGET",
                    &message,
                    "failed",
                )
                .await;
                return;
            }
        };
        let call = tokio::time::timeout(Duration::from_secs(120), async {
                let mut combined = ExtractorOutput { candidates: Vec::new(), injection_suspected: suspicious, dropped_items: 0 };
            let mut seen = std::collections::BTreeSet::new();
            let mut tokens_in = 0;
            let mut tokens_out = 0;
            for (index, chunk) in chunks.iter().enumerate() {
                let _ = sqlx::query("UPDATE durable_job SET progress_current=?,progress_total=? WHERE id=? AND status='running' AND current_attempt=?")
                    .bind(index as i64).bind(chunks.len() as i64).bind(job_id).bind(attempt.number)
                    .execute(workspace.pool()).await;
                let input: Value = serde_json::from_str(&input_json).map_err(|e| format!("invalid durable input: {e}"))?;
                let occurrence = input["occurrence_id"].as_str().unwrap_or_default();
                let repair = sqlx::query_scalar::<_,Option<String>>("SELECT error_message FROM job_attempt WHERE job_id=? AND attempt_no<? ORDER BY attempt_no DESC LIMIT 1")
                    .bind(job_id).bind(attempt.number).fetch_optional(workspace.pool()).await.ok().flatten().flatten();
                let content = format!("Locale: {}. Chunk {}/{}. Previous validation issue: {}\n\n<DATA>\n{}\n</DATA>",config.locale,index+1,chunks.len(),repair.as_deref().unwrap_or("none"),chunk);
                let response = ollama_chat(&config, &content, &attempt.prompt, &attempt.schema, 0.0).await?;
                tokens_in += response.prompt_eval_count.unwrap_or_else(|| token_upper_bound(&content));
                tokens_out += response.eval_count.unwrap_or(0);
                let chunk_out: ExtractorOutput = serde_json::from_str(&response.message.content).map_err(|e| format!("SCHEMA: model output did not match Extractor schema: {e}"))?;
                combined.injection_suspected |= chunk_out.injection_suspected;
                for candidate in chunk_out.candidates {
                    if !valid_candidate(&candidate) {
                        combined.dropped_items += 1;
                        continue;
                    }
                    if parley_domain::evidence::verify_quote(&raw_text, &candidate.quote, None).is_err() {
                        combined.dropped_items += 1;
                        continue;
                    }
                    let key = parley_domain::evidence::normalize(&candidate.quote);
                    if !seen.insert((key, candidate.kind.clone())) { continue; }
                    let _ = occurrence;
                    combined.candidates.push(candidate);
                }
                let _ = sqlx::query("UPDATE durable_job SET progress_current=? WHERE id=? AND status='running' AND current_attempt=?")
                    .bind((index + 1) as i64).bind(job_id).bind(attempt.number)
                    .execute(workspace.pool()).await;
            }
            Ok::<_,String>((combined,tokens_in,tokens_out))
        }).await;
        let result = match call {
            Err(_) => {
                finish_attempt_error(
                    workspace,
                    job_id,
                    attempt.number,
                    "ATTEMPT_TIMEOUT",
                    "Extractor attempt exceeded 120 seconds",
                    "failed",
                )
                .await;
                return;
            }
            Ok(Err(error)) if error.starts_with("SCHEMA:") && attempt.number < MAX_RETRIES => {
                finish_attempt_error(
                    workspace,
                    job_id,
                    attempt.number,
                    "OUTPUT_INVALID",
                    &error,
                    "retry",
                )
                .await;
                continue;
            }
            Ok(Err(error)) if error.starts_with("SCHEMA:") => {
                finish_attempt_error(
                    workspace,
                    job_id,
                    attempt.number,
                    "OUTPUT_INVALID",
                    &error,
                    "invalid",
                )
                .await;
                return;
            }
            Ok(Err(error)) => {
                finish_attempt_error(
                    workspace,
                    job_id,
                    attempt.number,
                    "AGENT_FAILED",
                    &redact_provider_error(&error),
                    "failed",
                )
                .await;
                return;
            }
            Ok(Ok(value)) => value,
        };
        match save_proposals(
            workspace,
            job_id,
            attempt.number,
            result.0,
            result.1,
            result.2,
        )
        .await
        {
            Ok(()) => return,
            Err(WorkspaceError::StaleRevision) => {
                finish_attempt_error(
                    workspace,
                    job_id,
                    attempt.number,
                    "STALE_INPUT",
                    "Capture changed while Extractor was running",
                    "failed",
                )
                .await;
                return;
            }
            Err(error) => {
                finish_attempt_error(
                    workspace,
                    job_id,
                    attempt.number,
                    "PERSIST_FAILED",
                    &error.to_string(),
                    "failed",
                )
                .await;
                return;
            }
        }
    }
}

pub(crate) struct Attempt {
    pub(crate) number: i64,
    pub(crate) role: String,
    pub(crate) provider_config: ProviderConfig,
    pub(crate) model_digest: Option<String>,
    pub(crate) input_sha256: String,
    pub(crate) input_json: String,
    pub(crate) prompt_id: String,
    pub(crate) prompt: String,
    pub(crate) schema: String,
}
pub(crate) async fn claim_attempt(
    workspace: &Workspace,
    id: &str,
) -> Result<Option<Attempt>, WorkspaceError> {
    let mut tx = workspace.pool().begin_with("BEGIN IMMEDIATE").await?;
    let row: Option<(i64,i64,String,String,String)> = sqlx::query_as("SELECT current_attempt,max_attempts,input_json,input_sha256,role FROM durable_job WHERE id=? AND status='queued'")
        .bind(id).fetch_optional(&mut *tx).await?;
    let Some((current, max, input, hash, role)) = row else {
        tx.rollback().await?;
        return Ok(None);
    };
    if current >= max {
        sqlx::query("UPDATE durable_job SET status='invalid',last_error_code='RETRY_LIMIT',last_error='Extractor retry limit reached',completed_at=?,updated_at=? WHERE id=? AND status='queued'").bind(now_utc()).bind(now_utc()).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(None);
    }
    let number = current + 1;
    let now = now_utc();
    let changed=sqlx::query("UPDATE durable_job SET status='running',current_attempt=?,updated_at=? WHERE id=? AND status='queued' AND current_attempt=?").bind(number).bind(&now).bind(id).bind(current).execute(&mut *tx).await?.rows_affected();
    if changed != 1 {
        tx.rollback().await?;
        return Ok(None);
    }
    let config: (String, Option<String>, String, i64, i64, String) =
        sqlx::query_as("SELECT model,model_digest,endpoint,num_ctx,num_predict,locale FROM agent_provider WHERE singleton=1")
            .fetch_one(&mut *tx)
            .await?;
    let provider_config = ProviderConfig {
        endpoint: config.2,
        model: config.0.clone(),
        num_ctx: config.3 as u32,
        num_predict: config.4 as u32,
        locale: config.5,
    };
    let provider_snapshot = serde_json::to_string(&provider_config)
        .map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
    let (default_prompt, schema) = role_defaults(&role)
        .ok_or_else(|| WorkspaceError::Invalid("agent role has no bundled schema".into()))?;
    let prompt_version_id = serde_json::from_str::<Value>(&input)
        .ok()
        .and_then(|value| {
            value
                .get("prompt_version_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let prompt_version: Option<(String, String, i64)> = if let Some(version_id) = prompt_version_id
    {
        sqlx::query_as("SELECT prompt_text,prompt_sha256,version_no FROM agent_prompt_version WHERE version_id=? AND role=?")
            .bind(version_id).bind(&role).fetch_optional(&mut *tx).await?
    } else {
        None
    };
    let (prompt, prompt_id, prompt_digest) = match prompt_version {
        Some((text, hash, version)) => (text, format!("{role}@{version}"), hash),
        None => (
            default_prompt.to_owned(),
            format!("{role}@1"),
            digest(default_prompt.as_bytes()),
        ),
    };
    let prompt_hash = digest(format!("{prompt_digest}\0{schema}").as_bytes());
    sqlx::query("INSERT INTO job_attempt(job_id,attempt_no,status,model,model_digest,provider_snapshot_json,prompt_id,prompt_sha256,input_sha256,input_json,started_at) VALUES(?,?,'running',?,?,?,?,?,?,?,?)")
      .bind(id).bind(number).bind(config.0).bind(config.1.clone()).bind(provider_snapshot).bind(prompt_id.clone()).bind(prompt_hash).bind(hash.clone()).bind(input.clone()).bind(&now).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'job',?,?, 'job.attempt_started',?)")
      .bind(new_id(id)).bind(&now).bind(id).bind(number).bind(json!({"attempt":number}).to_string()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(Attempt {
        number,
        role,
        provider_config,
        model_digest: config.1,
        input_sha256: hash,
        input_json: input,
        prompt_id,
        prompt,
        schema: schema.to_owned(),
    }))
}

fn role_defaults(role: &str) -> Option<(&'static str, &'static str)> {
    match role {
        "extractor" => Some((PROMPT, SCHEMA)),
        "interviewer" => Some((INTERVIEWER_PROMPT, INTERVIEWER_SCHEMA)),
        "briefer" => Some((BRIEFER_PROMPT, BRIEFER_SCHEMA)),
        "coach" => Some((COACH_PROMPT, COACH_SCHEMA)),
        "matcher" => Some((MATCHER_PROMPT, MATCHER_SCHEMA)),
        "template_builder" => Some((BUILDER_PROMPT, BUILDER_SCHEMA)),
        "composer" => Some((COMPOSER_PROMPT, COMPOSER_SCHEMA)),
        "linter" => Some((LINTER_PROMPT, LINTER_SCHEMA)),
        _ => None,
    }
}

#[allow(dead_code)] // Role adapters may complete a result without an aggregate write.
pub(crate) async fn complete_role_result(
    workspace: &Workspace,
    id: &str,
    attempt: i64,
    input_hash: &str,
    result: &Value,
    tokens_in: u32,
    tokens_out: u32,
) -> Result<(), WorkspaceError> {
    let mut tx = workspace.pool().begin_with("BEGIN IMMEDIATE").await?;
    complete_role_result_in_tx(
        &mut tx, id, attempt, input_hash, result, tokens_in, tokens_out,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub(super) async fn complete_role_result_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    attempt: i64,
    input_hash: &str,
    result: &Value,
    tokens_in: u32,
    tokens_out: u32,
) -> Result<(), WorkspaceError> {
    let current: Option<(String, i64, String)> =
        sqlx::query_as("SELECT status,current_attempt,input_sha256 FROM durable_job WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    if current
        .as_ref()
        .is_none_or(|(status, current_attempt, hash)| {
            status != "running" || *current_attempt != attempt || hash != input_hash
        })
    {
        return Err(WorkspaceError::StaleRevision);
    }
    let attempt_hash: Option<String> = sqlx::query_scalar(
        "SELECT input_sha256 FROM job_attempt WHERE job_id=? AND attempt_no=? AND status='running'",
    )
    .bind(id)
    .bind(attempt)
    .fetch_optional(&mut **tx)
    .await?;
    if attempt_hash.as_deref() != Some(input_hash) {
        return Err(WorkspaceError::StaleRevision);
    }
    let now = now_utc();
    let result_json = serde_json::to_string(result)
        .map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
    let changed = sqlx::query("UPDATE job_attempt SET status='succeeded',output_json=?,tokens_in=?,tokens_out=?,finished_at=?,duration_ms=CAST((julianday(?) - julianday(started_at))*86400000 AS INTEGER) WHERE job_id=? AND attempt_no=? AND status='running'")
        .bind(&result_json).bind(tokens_in as i64).bind(tokens_out as i64).bind(&now).bind(&now).bind(id).bind(attempt).execute(&mut **tx).await?.rows_affected();
    if changed != 1 {
        return Err(WorkspaceError::StaleRevision);
    }
    let changed = sqlx::query("UPDATE durable_job SET status='succeeded',last_error_code=NULL,last_error=NULL,updated_at=?,completed_at=? WHERE id=? AND status='running' AND current_attempt=? AND input_sha256=?")
        .bind(&now).bind(&now).bind(id).bind(attempt).bind(input_hash).execute(&mut **tx).await?.rows_affected();
    if changed != 1 {
        return Err(WorkspaceError::StaleRevision);
    }
    // Keep provisional model prose out of the immutable event log (RFC-0002
    // S04: "Keep provisional tokens out of the event log"). The full output
    // remains on the attempt row; the event carries counts only.
    let event_payload =
        json!({"status":"succeeded","attempt":attempt,"tokens_in":tokens_in,"tokens_out":tokens_out})
            .to_string();
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'job',?,?, 'job.completed',?)")
        .bind(new_id(id)).bind(&now).bind(id).bind(attempt).bind(&event_payload).execute(&mut **tx).await?;
    Ok(())
}

async fn save_proposals(
    workspace: &Workspace,
    job_id: &str,
    attempt: i64,
    output: ExtractorOutput,
    tokens_in: u32,
    tokens_out: u32,
) -> Result<(), WorkspaceError> {
    let mut tx = workspace.pool().begin_with("BEGIN IMMEDIATE").await?;
    let job:Option<(String,String,String,String)>=sqlx::query_as("SELECT status,source_occurrence_id,source_variant_id,input_sha256 FROM durable_job WHERE id=? AND current_attempt=?")
        .bind(job_id).bind(attempt).fetch_optional(&mut *tx).await?;
    let Some((status, occurrence, variant, input_hash)) = job else {
        tx.rollback().await?;
        return Err(WorkspaceError::StaleRevision);
    };
    if status != "running" {
        tx.rollback().await?;
        return Err(WorkspaceError::StaleRevision);
    }
    let attempt_hash: Option<String> = sqlx::query_scalar(
        "SELECT input_sha256 FROM job_attempt WHERE job_id=? AND attempt_no=? AND status='running'",
    )
    .bind(job_id)
    .bind(attempt)
    .fetch_optional(&mut *tx)
    .await?;
    if attempt_hash.as_deref() != Some(input_hash.as_str()) {
        tx.rollback().await?;
        return Err(WorkspaceError::StaleRevision);
    }
    let occurrence_variant: Option<String> =
        sqlx::query_scalar("SELECT source_variant_id FROM source_occurrence WHERE id=?")
            .bind(&occurrence)
            .fetch_optional(&mut *tx)
            .await?;
    if occurrence_variant.as_deref() != Some(variant.as_str()) {
        tx.rollback().await?;
        return Err(WorkspaceError::StaleRevision);
    }
    let candidate_count = output.candidates.len();
    let injection_suspected = output.injection_suspected;
    let batch_id = new_id(job_id);
    let now = now_utc();
    sqlx::query("INSERT INTO proposal_batch(id,source_variant_id,job_id,status,created_at) VALUES(?,?,?,'pending',?)").bind(&batch_id).bind(&variant).bind(job_id).bind(&now).execute(&mut *tx).await?;
    let mut dropped = output.dropped_items as usize;
    let mut staged_count = 0usize;
    for candidate in output.candidates {
        let matched = match parley_domain::evidence::verify_quote(
            &source_text(&mut tx, &variant).await?,
            &candidate.quote,
            None,
        ) {
            Ok(value) => value,
            Err(_) => {
                dropped += 1;
                continue;
            }
        };
        let id = new_id(&format!("{job_id}:{}:{}", candidate.kind, candidate.quote));
        let data = json!({"kind":candidate.kind,"statement":candidate.statement,"quote":candidate.quote,"mention_name":candidate.mention_name,"mention_kind":candidate.mention_kind,"source_start":matched.source_start,"source_end":matched.source_end,"normalized_start":matched.normalized_start,"normalized_end":matched.normalized_end,"normalization_version":parley_domain::evidence::NORMALIZATION_VERSION,"injection_suspected":output.injection_suspected});
        sqlx::query("INSERT INTO proposal_item(id,batch_id,proposal_kind,candidate_json,occurrence_id,status) VALUES(?,?,?,?,?,'pending')").bind(&id).bind(&batch_id).bind(&candidate.kind).bind(data.to_string()).bind(&occurrence).execute(&mut *tx).await?;
        staged_count += 1;
    }
    if staged_count == 0 {
        sqlx::query("UPDATE proposal_batch SET status='applied' WHERE id=?")
            .bind(&batch_id)
            .execute(&mut *tx)
            .await?;
    }
    let output_json=json!({"proposal_batch_id":batch_id,"candidate_count":candidate_count,"dropped_items":dropped,"injection_suspected":injection_suspected}).to_string();
    sqlx::query("UPDATE job_attempt SET status='succeeded',output_json=?,tokens_in=?,tokens_out=?,finished_at=?,duration_ms=CAST((julianday(?) - julianday(started_at))*86400000 AS INTEGER) WHERE job_id=? AND attempt_no=? AND status='running'")
       .bind(&output_json).bind(tokens_in as i64).bind(tokens_out as i64).bind(&now).bind(&now).bind(job_id).bind(attempt).execute(&mut *tx).await?;
    let affected=sqlx::query("UPDATE durable_job SET status='succeeded',last_error_code=NULL,last_error=NULL,updated_at=?,completed_at=? WHERE id=? AND status='running' AND current_attempt=?")
       .bind(&now).bind(&now).bind(job_id).bind(attempt).execute(&mut *tx).await?.rows_affected();
    if affected != 1 {
        tx.rollback().await?;
        return Err(WorkspaceError::StaleRevision);
    }
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'job',?,?, 'job.completed',?)")
      .bind(new_id(job_id)).bind(&now).bind(job_id).bind(attempt).bind(&output_json).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

async fn source_text(
    tx: &mut Transaction<'_, Sqlite>,
    variant: &str,
) -> Result<String, WorkspaceError> {
    sqlx::query_scalar("SELECT raw_text FROM source_variant WHERE id=?")
        .bind(variant)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(WorkspaceError::NotFound)
}

pub(super) async fn finish_job_error(
    workspace: &Workspace,
    id: &str,
    code: &str,
    message: &str,
    status: &str,
) {
    let attempt: Option<i64> = sqlx::query_scalar(
        "SELECT current_attempt FROM durable_job WHERE id=? AND status IN ('queued','running')",
    )
    .bind(id)
    .fetch_optional(workspace.pool())
    .await
    .ok()
    .flatten();
    if let Some(attempt) = attempt {
        finish_attempt_error(workspace, id, attempt, code, message, status).await;
    }
}
pub(crate) async fn finish_attempt_error(
    workspace: &Workspace,
    id: &str,
    attempt: i64,
    code: &str,
    message: &str,
    disposition: &str,
) {
    let mut tx = match workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(value) => value,
        Err(_) => return,
    };
    let current: Option<(String, i64)> =
        sqlx::query_as("SELECT status,current_attempt FROM durable_job WHERE id=?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .ok()
            .flatten();
    if current
        .as_ref()
        .is_none_or(|(status, num)| status != "running" || *num != attempt)
    {
        let _ = tx.rollback().await;
        return;
    }
    let now = now_utc();
    let terminal = matches!(disposition, "failed" | "invalid" | "cancelled");
    let attempt_status = match disposition {
        "retry" => "invalid",
        "invalid" => "invalid",
        "cancelled" => "cancelled",
        _ => "failed",
    };
    let _=sqlx::query("UPDATE job_attempt SET status=?,error_code=?,error_message=?,finished_at=? WHERE job_id=? AND attempt_no=? AND status='running'").bind(attempt_status).bind(code).bind(message).bind(&now).bind(id).bind(attempt).execute(&mut *tx).await;
    let next_status = if disposition == "retry" && attempt < MAX_RETRIES {
        "queued"
    } else if disposition == "invalid" {
        "invalid"
    } else if disposition == "cancelled" {
        "cancelled"
    } else {
        "failed"
    };
    let _=sqlx::query("UPDATE durable_job SET status=?,last_error_code=?,last_error=?,updated_at=?,completed_at=? WHERE id=? AND status='running' AND current_attempt=?")
       .bind(next_status).bind(code).bind(message).bind(&now).bind(if terminal||next_status!="queued" {Some(now.as_str())} else {None}).bind(id).bind(attempt).execute(&mut *tx).await;
    let _=sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'job',?,?,?,?)")
       .bind(new_id(id)).bind(&now).bind(id).bind(attempt).bind(if next_status=="queued" {"job.retrying"} else {"job.failed"}).bind(json!({"error_code":code}).to_string()).execute(&mut *tx).await;
    let _ = tx.commit().await;
}

pub(crate) async fn provider_config(
    workspace: &Workspace,
) -> Result<ProviderConfig, WorkspaceError> {
    let row: (String, String, i64, i64, String) = sqlx::query_as(
        "SELECT endpoint,model,num_ctx,num_predict,locale FROM agent_provider WHERE singleton=1",
    )
    .fetch_one(workspace.pool())
    .await?;
    Ok(ProviderConfig {
        endpoint: row.0,
        model: row.1,
        num_ctx: row.2 as u32,
        num_predict: row.3 as u32,
        locale: row.4,
    })
}

pub(crate) fn validate_endpoint(endpoint: &str) -> Result<(String, u16), String> {
    let Some(rest) = endpoint.strip_prefix("http://") else {
        return Err("Provider endpoint must use local HTTP".into());
    };
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || rest[authority.len()..].chars().any(|c| !c.is_whitespace()) {
        return Err("Provider endpoint must not contain a path, query, or fragment".into());
    }
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or("Provider endpoint must include a port")?;
    let port: u16 = port.parse().map_err(|_| "Provider port is invalid")?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if !loopback || port == 0 {
        return Err("Provider endpoint must resolve to loopback".into());
    }
    Ok((host.to_owned(), port))
}

pub(crate) async fn ollama_chat(
    config: &ProviderConfig,
    content: &str,
    prompt: &str,
    schema: &str,
    temperature: f32,
) -> Result<OllamaChatResponse, String> {
    let (host, port) = validate_endpoint(&config.endpoint)?;
    // UTF-8 byte length is a conservative upper bound for token count. Include
    // fixed prompt/schema/framing costs so hostile text cannot silently
    // exceed the configured context before the model receives it.
    let prompt_upper_bound = content
        .len()
        .saturating_add(prompt.len())
        .saturating_add(schema.len())
        .saturating_add(1024);
    if prompt_upper_bound.saturating_add(config.num_predict as usize) > config.num_ctx as usize {
        return Err("Prompt exceeds the configured context budget".into());
    }
    let body=json!({"model":config.model,"stream":false,"format":serde_json::from_str::<Value>(schema).map_err(|e|e.to_string())?,"messages":[{"role":"system","content":prompt},{"role":"user","content":content}],"options":{"temperature":temperature,"seed":42,"num_ctx":config.num_ctx,"num_predict":config.num_predict}}).to_string();
    let mut stream = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| format!("local provider unavailable: {e}"))?;
    stream.write_all(format!("POST /api/chat HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.map_err(|e|e.to_string())?;
    let mut bytes = Vec::new();
    stream
        .take((MAX_BODY + 8192) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    let response = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let (head, payload) = response
        .split_once("\r\n\r\n")
        .ok_or("Provider returned an invalid HTTP response")?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err(format!(
            "provider returned HTTP status {}",
            head.lines().next().unwrap_or("unknown")
        ));
    }
    let payload = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        decode_chunks(payload)?
    } else {
        payload.to_owned()
    };
    serde_json::from_str(&payload).map_err(|e| format!("provider response was invalid JSON: {e}"))
}
pub(crate) fn decode_chunks(mut rest: &str) -> Result<String, String> {
    let mut bytes = Vec::new();
    loop {
        let (size, tail) = rest.split_once("\r\n").ok_or("invalid chunk framing")?;
        let size = usize::from_str_radix(size.trim(), 16).map_err(|e| e.to_string())?;
        rest = tail;
        if size == 0 {
            break;
        }
        if size > MAX_BODY || bytes.len() + size > MAX_BODY {
            return Err("provider response exceeds limit".into());
        }
        bytes.extend_from_slice(&rest.as_bytes()[..size]);
        rest = &rest[size + 2..]
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}
fn token_upper_bound(text: &str) -> u32 {
    text.len().min(u32::MAX as usize) as u32
}
fn chunks(text: &str, num_ctx: u32, num_predict: u32) -> Result<Vec<String>, String> {
    let framing = PROMPT
        .len()
        .saturating_add(SCHEMA.len())
        .saturating_add(1024);
    let source_budget = (num_ctx as usize)
        .saturating_sub(num_predict as usize)
        .saturating_sub(framing);
    if source_budget < 256 {
        return Err(
            "Context budget cannot hold the bundled prompt, output reserve, and a source chunk"
                .into(),
        );
    }
    let max = source_budget;
    let overlap = 300usize.min(max / 8);
    let mut result = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + max).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1
        }
        if end <= start {
            return Err("Could not split capture at a UTF-8 boundary".into());
        }
        result.push(text[start..end].to_owned());
        if result.len() > MAX_CHUNKS {
            return Err("Capture exceeds the configured chunk budget".into());
        }
        if end == text.len() {
            break;
        }
        start = end.saturating_sub(overlap);
        while !text.is_char_boundary(start) {
            start += 1
        }
    }
    Ok(result)
}
fn strip_invisible_controls(text: &str) -> String {
    text.chars().filter(|ch|!matches!(*ch,'\u{200b}'..='\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2060}'..='\u{206f}'|'\u{feff}')).collect()
}
fn injection_suspected(text: &str) -> bool {
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
fn valid_kind(kind: &str) -> bool {
    matches!(kind, "signal" | "interest" | "hook" | "stakeholder")
}
fn valid_candidate(candidate: &Candidate) -> bool {
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
fn redact_provider_error(error: &str) -> String {
    let mut s = error.replace('\n', " ");
    if let Some(index) = s.find("http://") {
        s.truncate(index);
        s.push_str("provider connection failed")
    }
    s.chars().take(240).collect()
}

async fn get_provider(State(state): State<super::AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    match provider_config(&state.workspace).await {
        Ok(config) => Json(config).into_response(),
        Err(error) => super::command_error(error),
    }
}
async fn update_provider(
    State(state): State<super::AppState>,
    headers: HeaderMap,
    Json(input): Json<ProviderUpdate>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if let Err(message) = validate_endpoint(&input.endpoint) {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "PROVIDER_ENDPOINT_INVALID",
            message,
        );
    }
    if input.model.trim().is_empty()
        || input.model.len() > 200
        || input.num_ctx < 1024
        || input.num_ctx > 131072
        || input.num_predict < 64
        || input.num_predict > 8192
        || input.num_predict + 1024 >= input.num_ctx
        || input.locale.trim().is_empty()
        || input.locale.len() > 40
    {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "PROVIDER_CONFIG_INVALID",
            "Provider model, locale, or token budgets are invalid",
        );
    }
    let key = match headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
    {
        Some(value) if !value.trim().is_empty() && value.len() <= 200 => value.to_owned(),
        _ => {
            return super::error(
                StatusCode::BAD_REQUEST,
                "IDEMPOTENCY_KEY_REQUIRED",
                "A 1–200 byte Idempotency-Key is required",
            );
        }
    };
    let canonical = match serde_json::to_vec(&input) {
        Ok(bytes) => digest(&bytes),
        Err(error) => {
            return super::error(
                StatusCode::BAD_REQUEST,
                "PROVIDER_CONFIG_INVALID",
                error.to_string(),
            );
        }
    };
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    let prior: Result<Option<(String, String)>, sqlx::Error> = sqlx::query_as(
        "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
    ).bind(state.workspace.epoch()).bind(&key).fetch_optional(&mut *tx).await;
    match prior {
        Ok(Some((old, _))) if old == canonical => {
            let _ = tx.commit().await;
            return StatusCode::NO_CONTENT.into_response();
        }
        Ok(Some(_)) => {
            let _ = tx.rollback().await;
            return super::error(
                StatusCode::CONFLICT,
                "IDEMPOTENCY_CONFLICT",
                "This key was already used for another provider configuration",
            );
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return super::command_error(error.into());
        }
        Ok(None) => {}
    }
    let now = now_utc();
    if let Err(error) = sqlx::query("UPDATE agent_provider SET endpoint=?,model=?,model_digest=NULL,num_ctx=?,num_predict=?,locale=?,updated_at=? WHERE singleton=1")
        .bind(input.endpoint).bind(input.model).bind(input.num_ctx as i64).bind(input.num_predict as i64).bind(input.locale).bind(&now)
        .execute(&mut *tx).await {
        let _ = tx.rollback().await; return super::command_error(error.into());
    }
    let revision: Result<i64, sqlx::Error> = sqlx::query_scalar(
        "UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1 RETURNING revision",
    )
    .fetch_one(&mut *tx)
    .await;
    let revision = match revision {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return super::command_error(error.into());
        }
    };
    let event_cursor: Result<i64, sqlx::Error> = sqlx::query_scalar("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'agent_provider','extractor',?,'agent.provider_updated','{}') RETURNING cursor")
        .bind(new_id(&canonical)).bind(&now).bind(revision).fetch_one(&mut *tx).await;
    let event_cursor = match event_cursor {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return super::command_error(error.into());
        }
    };
    if let Err(error) = sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,204,'{}',?,?)")
        .bind(state.workspace.epoch()).bind(key).bind(canonical).bind(event_cursor).bind(&now).execute(&mut *tx).await {
        let _ = tx.rollback().await; return super::command_error(error.into());
    }
    match tx.commit().await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => super::command_error(error.into()),
    }
}
async fn provider_health(State(state): State<super::AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let config = match provider_config(&state.workspace).await {
        Ok(value) => value,
        Err(error) => return super::command_error(error),
    };
    match ollama_tags(&config).await{
        Ok((models, selected_digest)) => {
            let available = models.iter().any(|name| name == &config.model);
            if available {
                // The digest refresh that gates dispatch must be transactional
                // and evented, and must run only when the digest actually
                // changed (a probe must not spam the immutable log).
                let now = now_utc();
                let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
                    Ok(tx) => tx,
                    Err(error) => return super::command_error(error.into()),
                };
                let current: Option<String> =
                    sqlx::query_scalar("SELECT model_digest FROM agent_provider WHERE singleton=1")
                        .fetch_optional(&mut *tx)
                        .await
                        .unwrap_or(None);
                if current.as_deref() != selected_digest.as_deref() {
                    if let Err(error) = sqlx::query("UPDATE agent_provider SET model_digest=?,updated_at=? WHERE singleton=1")
                        .bind(&selected_digest)
                        .bind(&now)
                        .execute(&mut *tx)
                        .await
                    {
                        let _ = tx.rollback().await;
                        return super::command_error(error.into());
                    }
                    let revision: i64 = match sqlx::query_scalar(
                        "UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1 RETURNING revision",
                    )
                    .fetch_one(&mut *tx)
                    .await
                    {
                        Ok(value) => value,
                        Err(error) => {
                            let _ = tx.rollback().await;
                            return super::command_error(error.into());
                        }
                    };
                    if let Err(error) = sqlx::query_scalar::<_, i64>(
                        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'agent_provider','extractor',?,'agent.provider_updated',?) RETURNING cursor",
                    )
                    .bind(new_id("provider-health"))
                    .bind(&now)
                    .bind(revision)
                    .bind(json!({"model_digest":selected_digest}).to_string())
                    .fetch_one(&mut *tx)
                    .await
                    {
                        let _ = tx.rollback().await;
                        return super::command_error(error.into());
                    }
                }
                if let Err(error) = tx.commit().await {
                    return super::command_error(error.into());
                }
            }
            Json(json!({"available":available,"model":config.model,"models":models,"message":if available{"Local model is available"}else{"Selected model is not installed"}})).into_response()
        }
        Err(error) => Json(json!({"available":false,"model":config.model,"models":[],"message":redact_provider_error(&error)})).into_response()
    }
}
#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}
#[derive(Deserialize)]
struct TagModel {
    name: String,
    digest: String,
}
async fn ollama_tags(config: &ProviderConfig) -> Result<(Vec<String>, Option<String>), String> {
    let (host, port) = validate_endpoint(&config.endpoint)?;
    let mut stream = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| format!("local provider unavailable: {e}"))?;
    stream
        .write_all(
            format!("GET /api/tags HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    stream
        .take(1_000_000)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    let body = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let (head, body) = body
        .split_once("\r\n\r\n")
        .ok_or("provider response was malformed")?;
    if !head.contains(" 200 ") {
        return Err("provider health endpoint returned an error".into());
    }
    let data: TagsResponse = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let selected = data
        .models
        .iter()
        .find(|model| {
            model.name == config.model || model.name.starts_with(&format!("{}:", config.model))
        })
        .map(|model| model.digest.clone());
    Ok((
        data.models.into_iter().map(|model| model.name).collect(),
        selected,
    ))
}

async fn get_job(
    State(state): State<super::AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let row: Option<JobReadRow> = match sqlx::query_as("SELECT j.role,j.status,j.current_attempt,j.max_attempts,j.last_error_code,j.last_error,j.source_occurrence_id,j.scope_kind,j.scope_id,j.progress_current,j.progress_total FROM durable_job j WHERE id=?")
        .bind(&id).fetch_optional(state.workspace.pool()).await {
        Ok(value) => value,
        Err(error) => return super::error(StatusCode::INTERNAL_SERVER_ERROR, "READ_FAILED", error.to_string()),
    };
    let Some(row) = row else {
        return super::error(StatusCode::NOT_FOUND, "JOB_NOT_FOUND", "Job does not exist");
    };
    let output_json: Option<String> = match sqlx::query_scalar(
        "SELECT output_json FROM job_attempt WHERE job_id=? ORDER BY attempt_no DESC LIMIT 1",
    )
    .bind(&id)
    .fetch_optional(state.workspace.pool())
    .await
    {
        Ok(value) => value.flatten(),
        Err(error) => return super::command_error(error.into()),
    };
    let injection_suspected = output_json
        .as_deref()
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .and_then(|value| value.get("injection_suspected").and_then(Value::as_bool))
        .unwrap_or(false);
    let batch: Option<String> = match sqlx::query_scalar(
        "SELECT id FROM proposal_batch WHERE job_id=? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&id)
    .fetch_optional(state.workspace.pool())
    .await
    {
        Ok(value) => value,
        Err(error) => return super::command_error(error.into()),
    };
    let proposals = if let Some(batch) = &batch {
        let rows: Vec<(String, String, String, String)> = match sqlx::query_as(
            "SELECT id,proposal_kind,candidate_json,coalesce(review_state,status) FROM proposal_item WHERE batch_id=? ORDER BY id",
        ).bind(batch).fetch_all(state.workspace.pool()).await {
            Ok(rows) => rows,
            Err(error) => return super::command_error(error.into()),
        };
        let mut proposals = Vec::with_capacity(rows.len());
        for (id, kind, candidate, status) in rows {
            let candidate = match serde_json::from_str::<Value>(&candidate) {
                Ok(value) => value,
                Err(error) => {
                    return super::error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "PROPOSAL_CORRUPT",
                        error.to_string(),
                    );
                }
            };
            proposals.push(ProposalRecord {
                id,
                kind,
                candidate,
                status,
            });
        }
        proposals
    } else {
        Vec::new()
    };
    Json(JobRecord {
        id,
        role: row.0,
        status: row.1,
        current_attempt: row.2,
        max_attempts: row.3,
        progress_current: row.9,
        progress_total: row.10,
        injection_suspected,
        last_error_code: row.4,
        last_error: row.5,
        source_occurrence_id: row.6,
        scope_kind: row.7,
        scope_id: row.8,
        proposal_batch_id: batch,
        proposals,
        result: output_json.and_then(|value| serde_json::from_str::<Value>(&value).ok()),
    })
    .into_response()
}
async fn get_capture_proposals(
    State(state): State<super::AppState>,
    Path(occurrence_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let id: Option<String> = match sqlx::query_scalar(
        "SELECT id FROM durable_job WHERE source_occurrence_id=? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&occurrence_id)
    .fetch_optional(state.workspace.pool())
    .await
    {
        Ok(value) => value,
        Err(error) => return super::command_error(error.into()),
    };
    let Some(id) = id else {
        return super::error(
            StatusCode::NOT_FOUND,
            "PROPOSALS_NOT_READY",
            "No Extractor job exists for this capture",
        );
    };
    get_job(State(state), Path(id), headers).await
}
async fn cancel_job(
    State(state): State<super::AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    let row: Option<(String, i64)> =
        match sqlx::query_as("SELECT status,current_attempt FROM durable_job WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(error) => {
                let _ = tx.rollback().await;
                return super::command_error(error.into());
            }
        };
    let Some((status, attempt)) = row else {
        let _ = tx.rollback().await;
        return super::error(StatusCode::NOT_FOUND, "JOB_NOT_FOUND", "Job does not exist");
    };
    if !matches!(status.as_str(), "queued" | "running") {
        let _ = tx.rollback().await;
        return super::error(
            StatusCode::CONFLICT,
            "JOB_TERMINAL",
            "Terminal jobs cannot be cancelled",
        );
    }
    let now = now_utc();
    let changed = sqlx::query("UPDATE durable_job SET status='cancelled',last_error_code='CANCELLED',last_error='Owner cancelled Extractor job',updated_at=?,completed_at=? WHERE id=? AND current_attempt=? AND status IN ('queued','running')").bind(&now).bind(&now).bind(&id).bind(attempt).execute(&mut *tx).await;
    match changed {
        Ok(result) if result.rows_affected() == 1 => {}
        Ok(_) => {
            let _ = tx.rollback().await;
            return super::error(
                StatusCode::CONFLICT,
                "JOB_CHANGED",
                "Job changed while cancellation was being applied",
            );
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return super::command_error(error.into());
        }
    }
    if let Err(error) = sqlx::query("UPDATE job_attempt SET status='cancelled',error_code='CANCELLED',error_message='Owner cancelled Extractor job',finished_at=? WHERE job_id=? AND attempt_no=? AND status='running'").bind(&now).bind(&id).bind(attempt).execute(&mut *tx).await {
        let _ = tx.rollback().await; return super::command_error(error.into());
    }
    if let Err(error) = sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'job',?,?, 'job.cancelled','{}')").bind(new_id(&id)).bind(&now).bind(&id).bind(attempt).execute(&mut *tx).await {
        let _ = tx.rollback().await; return super::command_error(error.into());
    }
    match tx.commit().await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => super::command_error(error.into()),
    }
}

async fn resolve_proposals(
    State(state): State<super::AppState>,
    Path(batch_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ProposalResolution>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    let key = match headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
    {
        Some(value) if !value.trim().is_empty() && value.len() <= 200 => value.to_owned(),
        _ => {
            let _ = tx.rollback().await;
            return super::error(
                StatusCode::BAD_REQUEST,
                "IDEMPOTENCY_KEY_REQUIRED",
                "A 1–200 byte Idempotency-Key is required",
            );
        }
    };
    let canonical = match serde_json::to_vec(&(batch_id.as_str(), &input)) {
        Ok(bytes) => digest(&bytes),
        Err(error) => {
            let _ = tx.rollback().await;
            return super::error(StatusCode::BAD_REQUEST, "REVIEW_INVALID", error.to_string());
        }
    };
    let prior: Result<Option<(String, String)>, sqlx::Error> = sqlx::query_as(
        "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
    ).bind(state.workspace.epoch()).bind(&key).fetch_optional(&mut *tx).await;
    match prior {
        Ok(Some((old_digest, result))) if old_digest == canonical => {
            let _ = tx.commit().await;
            return match serde_json::from_str::<Value>(&result) {
                Ok(value) => Json(value).into_response(),
                Err(error) => super::error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "RECEIPT_CORRUPT",
                    error.to_string(),
                ),
            };
        }
        Ok(Some(_)) => {
            let _ = tx.rollback().await;
            return super::error(
                StatusCode::CONFLICT,
                "IDEMPOTENCY_CONFLICT",
                "This key was already used for a different review request",
            );
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return super::command_error(error.into());
        }
        Ok(None) => {}
    }
    match apply_proposal_decisions(&mut tx, &batch_id, input).await {
        Ok(result) => {
            let result_json = result.to_string();
            let event_cursor = result["last_event_cursor"].as_i64().unwrap_or(0);
            let now = now_utc();
            let receipt = sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,200,?,?,?)")
                .bind(state.workspace.epoch()).bind(key).bind(canonical).bind(&result_json).bind(event_cursor).bind(now)
                .execute(&mut *tx).await;
            if let Err(error) = receipt {
                let _ = tx.rollback().await;
                return super::command_error(error.into());
            }
            match tx.commit().await {
                Ok(()) => Json(result).into_response(),
                Err(error) => super::command_error(error.into()),
            }
        }
        Err(failure) => {
            let _ = tx.rollback().await;
            super::error(failure.status, failure.code, failure.message)
        }
    }
}

struct ReviewFailure {
    status: StatusCode,
    code: &'static str,
    message: String,
}
impl ReviewFailure {
    fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code,
            message: message.into(),
        }
    }
    fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            message: message.into(),
        }
    }
    fn storage(error: sqlx::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "STORAGE_ERROR",
            message: error.to_string(),
        }
    }
}

struct ReviewedPlan {
    item_id: String,
    action: String,
    kind: String,
    data: Value,
    target: Option<String>,
    wording: Option<String>,
    source_span: Option<(i64, i64, i64, i64)>,
}

async fn apply_proposal_decisions(
    tx: &mut Transaction<'_, Sqlite>,
    batch_id: &str,
    input: ProposalResolution,
) -> Result<Value, ReviewFailure> {
    if input.decisions.is_empty() || input.decisions.len() > 500 {
        return Err(ReviewFailure::invalid(
            "REVIEW_INVALID",
            "Review must select 1 to 500 proposal decisions",
        ));
    }
    let source: Option<(String, String, String)> = sqlx::query_as(
        "SELECT b.source_variant_id,j.source_occurrence_id,j.id FROM proposal_batch b JOIN durable_job j ON j.id=b.job_id WHERE b.id=? AND j.status='succeeded'",
    ).bind(batch_id).fetch_optional(&mut **tx).await.map_err(ReviewFailure::storage)?;
    let Some((variant, occurrence, job)) = source else {
        return Err(ReviewFailure {
            status: StatusCode::NOT_FOUND,
            code: "PROPOSAL_BATCH_NOT_FOUND",
            message: "Completed proposal batch does not exist".into(),
        });
    };
    let text: String = sqlx::query_scalar("SELECT raw_text FROM source_variant WHERE id=?")
        .bind(&variant)
        .fetch_optional(&mut **tx)
        .await
        .map_err(ReviewFailure::storage)?
        .ok_or_else(|| {
            ReviewFailure::invalid(
                "SOURCE_PURGED",
                "Source text is unavailable for quote verification",
            )
        })?;
    if let Some(opportunity) = input.opportunity_id.as_deref() {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM opportunity WHERE id=?)")
                .bind(opportunity)
                .fetch_one(&mut **tx)
                .await
                .map_err(ReviewFailure::storage)?;
        if !exists {
            return Err(ReviewFailure::invalid(
                "OPPORTUNITY_INVALID",
                "Capture association opportunity does not exist",
            ));
        }
    }

    // Validate the complete selected set before writing the first accepted fact.
    let mut seen = std::collections::BTreeSet::new();
    let mut plans = Vec::with_capacity(input.decisions.len());
    for decision in &input.decisions {
        if !seen.insert(decision.item_id.as_str()) {
            return Err(ReviewFailure::invalid(
                "DUPLICATE_DECISION",
                "Each proposal can be decided once per command",
            ));
        }
        if !matches!(
            decision.action.as_str(),
            "confirm" | "correct" | "dismiss" | "source_only" | "unresolved"
        ) {
            return Err(ReviewFailure::invalid(
                "DECISION_INVALID",
                "Unsupported proposal decision",
            ));
        }
        let item: Option<(String, String, String)> = sqlx::query_as(
            "SELECT candidate_json,proposal_kind,status FROM proposal_item WHERE id=? AND batch_id=?",
        ).bind(&decision.item_id).bind(batch_id).fetch_optional(&mut **tx).await.map_err(ReviewFailure::storage)?;
        let Some((candidate, kind, status)) = item else {
            return Err(ReviewFailure::invalid(
                "PROPOSAL_NOT_IN_BATCH",
                "Selected proposal is missing from this batch",
            ));
        };
        if status != "pending" {
            return Err(ReviewFailure::conflict(
                "PROPOSAL_ALREADY_DECIDED",
                "A selected proposal is no longer pending",
            ));
        }
        let data: Value = serde_json::from_str(&candidate).map_err(|_| ReviewFailure {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "PROPOSAL_CORRUPT",
            message: "Stored proposal data is invalid".into(),
        })?;
        let target = decision.target_entity_id.clone();
        let wording = if decision.action == "confirm" || decision.action == "correct" {
            let value = decision
                .corrected_statement
                .as_deref()
                .filter(|_| decision.action == "correct")
                .or_else(|| data["statement"].as_str())
                .unwrap_or_default()
                .trim()
                .to_owned();
            let target_id = target.as_deref().ok_or_else(|| ReviewFailure::invalid(
                "REVIEW_TARGET_REQUIRED", "Confirm or correct requires an explicitly selected existing person or organization"
            ))?;
            if value.is_empty() {
                return Err(ReviewFailure::invalid(
                    "REVIEW_WORDING_REQUIRED",
                    "Approved wording cannot be empty",
                ));
            }
            let target_kind: Option<String> =
                sqlx::query_scalar("SELECT kind FROM entity WHERE id=?")
                    .bind(target_id)
                    .fetch_optional(&mut **tx)
                    .await
                    .map_err(ReviewFailure::storage)?;
            if target_kind
                .as_deref()
                .is_none_or(|value| !matches!(value, "person" | "org"))
            {
                return Err(ReviewFailure::invalid(
                    "REVIEW_TARGET_INVALID",
                    "Target must be an explicitly selected saved person or organization",
                ));
            }
            let quote = data["quote"].as_str().unwrap_or_default();
            let matched =
                parley_domain::evidence::verify_quote(&text, quote, None).map_err(|_| {
                    ReviewFailure::invalid(
                        "QUOTE_INVALID",
                        "Proposal quote is no longer an exact unambiguous source span",
                    )
                })?;
            (
                Some(value),
                Some((
                    matched.source_start as i64,
                    matched.source_end as i64,
                    matched.normalized_start as i64,
                    matched.normalized_end as i64,
                )),
            )
        } else {
            if target.is_some() || decision.corrected_statement.is_some() {
                return Err(ReviewFailure::invalid(
                    "DECISION_FIELDS_INVALID",
                    "Target and corrected wording are only valid for confirm/correct",
                ));
            }
            (None, None)
        };
        plans.push(ReviewedPlan {
            item_id: decision.item_id.clone(),
            action: decision.action.clone(),
            kind,
            data,
            target,
            wording: wording.0,
            source_span: wording.1,
        });
    }

    let now = now_utc();
    for plan in &plans {
        let (decision_kind, item_status, review_state) = match plan.action.as_str() {
            "confirm" => ("confirmed", "accepted", None),
            "correct" => ("corrected", "corrected", None),
            "dismiss" => ("dismissed", "dismissed", None),
            "source_only" => ("source_only", "source_only", None),
            _ => ("unresolved", "source_only", Some("unresolved")),
        };
        if let (Some(target), Some(wording), Some((ss, se, ns, ne))) = (
            plan.target.as_ref(),
            plan.wording.as_ref(),
            plan.source_span,
        ) {
            let claim_id = new_id(&plan.item_id);
            sqlx::query("INSERT INTO claim(id,subject_id,predicate,current_revision,created_at) VALUES(?,?,?,1,?)")
                .bind(&claim_id).bind(target).bind(format!("proposal:{}", plan.kind)).bind(&now)
                .execute(&mut **tx).await.map_err(ReviewFailure::storage)?;
            sqlx::query("INSERT INTO claim_revision(claim_id,revision,wording,knowledge_kind,tier,speaker_kind,occurrence_id,source_variant_id,source_start,source_end,normalized_start,normalized_end,normalization_version,approved_wording,confirmed_by_owner,created_at) VALUES(?,1,?,'observation',1,'unknown',?,?,?,?,?,?,?, ?,1,?)")
                .bind(&claim_id).bind(wording).bind(&occurrence).bind(&variant).bind(ss).bind(se).bind(ns).bind(ne)
                .bind(parley_domain::evidence::NORMALIZATION_VERSION as i64).bind(wording).bind(&now)
                .execute(&mut **tx).await.map_err(ReviewFailure::storage)?;
        }
        if let Some(name) = plan.data["mention_name"]
            .as_str()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            let span = plan.source_span.unwrap_or_else(|| {
                let start = plan.data["source_start"].as_i64().unwrap_or(0);
                let end = plan.data["source_end"].as_i64().unwrap_or(start + 1);
                (start, end, 0, 0)
            });
            sqlx::query("INSERT INTO unresolved_mention(id,occurrence_id,proposal_item_id,mention_name,mention_kind,source_start,source_end,status,resolved_entity_id,created_at) VALUES(?,?,?,?,?,?,?,'unresolved',NULL,?) ON CONFLICT(proposal_item_id) DO NOTHING")
                .bind(new_id(&plan.item_id)).bind(&occurrence).bind(&plan.item_id).bind(name)
                .bind(plan.data["mention_kind"].as_str()).bind(span.0).bind(span.1).bind(&now)
                .execute(&mut **tx).await.map_err(ReviewFailure::storage)?;
        }
        sqlx::query("INSERT INTO proposal_decision(id,proposal_item_id,decision,target_entity_id,approved_wording,opportunity_id,decided_at) VALUES(?,?,?,?,?,?,?)")
            .bind(new_id(&plan.item_id)).bind(&plan.item_id).bind(decision_kind).bind(plan.target.as_deref())
            .bind(plan.wording.as_deref()).bind(input.opportunity_id.as_deref()).bind(&now)
            .execute(&mut **tx).await.map_err(ReviewFailure::storage)?;
        let changed = sqlx::query(
            "UPDATE proposal_item SET status=?,review_state=? WHERE id=? AND batch_id=? AND status='pending'",
        )
        .bind(item_status)
            .bind(review_state)
        .bind(&plan.item_id)
        .bind(batch_id)
        .execute(&mut **tx)
        .await
        .map_err(ReviewFailure::storage)?
        .rows_affected();
        if changed != 1 {
            return Err(ReviewFailure::conflict(
                "PROPOSAL_ALREADY_DECIDED",
                "Proposal changed during review",
            ));
        }
    }
    if let Some(opportunity) = input.opportunity_id.as_deref() {
        sqlx::query("INSERT INTO source_opportunity_link(source_variant_id,opportunity_id,created_at) VALUES(?,?,?) ON CONFLICT DO NOTHING")
            .bind(&variant).bind(opportunity).bind(&now).execute(&mut **tx).await.map_err(ReviewFailure::storage)?;
    }
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proposal_item WHERE batch_id=? AND status='pending'",
    )
    .bind(batch_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(ReviewFailure::storage)?;
    let decided: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proposal_item WHERE batch_id=? AND status<>'pending'",
    )
    .bind(batch_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(ReviewFailure::storage)?;
    let batch_status = if pending == 0 {
        "applied"
    } else if decided > 0 {
        "partially_applied"
    } else {
        "pending"
    };
    sqlx::query("UPDATE proposal_batch SET status=? WHERE id=?")
        .bind(batch_status)
        .bind(batch_id)
        .execute(&mut **tx)
        .await
        .map_err(ReviewFailure::storage)?;
    let event_payload =
        json!({"batch_id":batch_id,"decision_count":plans.len(),"pending_count":pending})
            .to_string();
    let cursor: i64 = sqlx::query_scalar("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'job',?,?, 'proposals.reviewed',?) RETURNING cursor")
        .bind(new_id(&job)).bind(&now).bind(&job).bind(decided).bind(event_payload)
        .fetch_one(&mut **tx).await.map_err(ReviewFailure::storage)?;
    Ok(
        json!({"batch_id":batch_id,"status":batch_status,"decided_count":decided,"pending_count":pending,"last_event_cursor":cursor}),
    )
}
