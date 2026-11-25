//! Durable, bounded Extractor jobs. Model output remains staged until an owner
//! explicitly resolves proposal items through the review command.
use super::{Workspace, WorkspaceError, digest, new_id, now_utc};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

const INTERVIEWER_PROMPT: &str = include_str!("../roles/interviewer/prompt.md");
const INTERVIEWER_SCHEMA: &str = include_str!("../roles/interviewer/schema.json");
const BRIEFER_PROMPT: &str = include_str!("../roles/briefer/prompt.md");
const BRIEFER_SCHEMA: &str = include_str!("../roles/briefer/schema.json");
const MATCHER_PROMPT: &str = include_str!("../roles/matcher/prompt.md");
const MATCHER_SCHEMA: &str = include_str!("../roles/matcher/schema.json");
const BUILDER_PROMPT: &str = include_str!("../roles/template_builder/prompt.md");
const BUILDER_SCHEMA: &str = include_str!("../roles/template_builder/schema.json");
const COMPOSER_PROMPT: &str = include_str!("../roles/composer/prompt.md");
const COMPOSER_SCHEMA: &str = include_str!("../roles/composer/schema.json");
const LINTER_PROMPT: &str = include_str!("../roles/linter/prompt.md");
const LINTER_SCHEMA: &str = include_str!("../roles/linter/schema.json");

pub(crate) fn interviewer_prompt() -> &'static str {
    INTERVIEWER_PROMPT
}

pub(crate) fn interviewer_schema() -> &'static str {
    INTERVIEWER_SCHEMA
}

pub(crate) fn briefer_schema() -> &'static str {
    BRIEFER_SCHEMA
}

pub(crate) fn matcher_prompt() -> &'static str {
    MATCHER_PROMPT
}

pub(crate) fn matcher_schema() -> &'static str {
    MATCHER_SCHEMA
}

pub(crate) fn composer_schema() -> &'static str {
    COMPOSER_SCHEMA
}

pub(crate) fn linter_schema() -> &'static str {
    LINTER_SCHEMA
}

/// The bundled prompt/schema pair for an assistant role. Used by admission to
/// resolve the explicit fallback prompt when no reviewed owner prompt version
/// is recorded (AR-001 AR04/AR08).
pub(crate) fn bundled_role_prompt(role: &str) -> Option<&'static str> {
    role_defaults(role).map(|(prompt, _)| prompt)
}

const MAX_BODY: usize = 2_200_000;
const MAX_RETRIES: i64 = 3;

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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderUpdate {
    expected_profile_revision: Option<i64>,
    pairing: Option<super::inference::profile::ObservedPairing>,
    endpoint: String,
    model: String,
    num_ctx: u32,
    num_predict: u32,
    locale: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProposalResolution {
    decisions: Vec<ProposalDecisionInput>,
    opportunity_id: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProposalDecisionInput {
    item_id: String,
    action: String,
    target_entity_id: Option<String>,
    corrected_statement: Option<String>,
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

/// Enqueue one assistant job inside the caller's transaction. A registered
/// command binds the job to its initiating epoch and run so admission,
/// completion and status reads can be fenced (UCR-001 U05). Owner-launched
/// jobs pass `epoch` and `None` run/owner and keep their previous identity
/// rules. The job id includes the binding so two runs never collide.
#[allow(clippy::too_many_arguments)] // The typed role command is the shared cross-slice worker boundary.
pub(crate) async fn enqueue_role_in_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    role: &str,
    scope_kind: &str,
    scope_id: &str,
    scope_revision: i64,
    input: &Value,
    max_attempts: i64,
    epoch: &str,
    run_id: Option<&str>,
    owner_pseudonym: Option<&str>,
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
    let id = if let Some(run_id) = run_id {
        new_id(&format!(
            "{role}:{epoch}:{run_id}:{scope_kind}:{scope_id}:{input_hash}"
        ))
    } else {
        new_id(&format!("{role}:{scope_kind}:{scope_id}:{input_hash}"))
    };
    let now = now_utc();
    sqlx::query("INSERT INTO durable_job(id,role,status,input_json,input_sha256,scope_kind,scope_id,scope_revision,max_attempts,epoch,run_id,owner_pseudonym,created_at,updated_at) VALUES(?,?,'queued',?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&id).bind(role).bind(&input_json).bind(&input_hash).bind(scope_kind).bind(scope_id).bind(scope_revision).bind(max_attempts).bind(epoch).bind(run_id).bind(owner_pseudonym).bind(&now).bind(&now).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'job',?,1,'job.queued',?)")
        .bind(new_id(&id)).bind(&now).bind(&id).bind(json!({"role":role,"scope_kind":scope_kind,"scope_id":scope_id,"scope_revision":scope_revision,"run_id":run_id}).to_string()).execute(&mut **tx).await?;
    Ok(id)
}

/// Completion fence for a job started by a registered command. The run that
/// started the job must still be active in the same epoch, and the workspace
/// must still be on that epoch, or the publication is stale and the worker
/// must not install reviewable proposals (UCR-001 U05). Cancellation and
/// epoch rotation therefore take no dependency on a provider answer.
pub(crate) async fn scoped_job_fence(
    tx: &mut Transaction<'_, Sqlite>,
    job_id: &str,
) -> Result<(), ()> {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT epoch,run_id FROM durable_job WHERE id=?")
            .bind(job_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| ())?;
    let Some((job_epoch, run_id)) = row else {
        return Err(());
    };
    let Some(run_id) = run_id else {
        return Ok(());
    };
    let current_epoch: Option<String> =
        sqlx::query_scalar("SELECT epoch FROM workspace_meta WHERE singleton=1")
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| ())?
            .flatten();
    if current_epoch.as_deref() != Some(job_epoch.as_str()) {
        return Err(());
    }
    let run: Option<(String, String)> =
        sqlx::query_as("SELECT state,epoch FROM agent_run WHERE run_id=?")
            .bind(run_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| ())?;
    match run {
        Some((state, run_epoch))
            if run_epoch == job_epoch
                && matches!(state.as_str(), "running" | "waiting_owner" | "waiting_job") =>
        {
            Ok(())
        }
        _ => Err(()),
    }
}

pub(crate) async fn recover_interrupted(workspace: &Workspace) {
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

pub(crate) async fn process(state: &super::AppState, job_id: &str) {
    process_inner(state, job_id).await;
    // Post-commit hint: the durable job row has reached a new state
    // (running→terminal, retry, or an immediate pre-flight failure). The
    // role runtimes commit before returning, so this send happens after the
    // transition is durable. Activity surfaces re-read GET /jobs/{id} on
    // this signal instead of polling (L1).
    let _ = state
        .notify
        .send(crate::CommandSignal::JobChanged(job_id.to_owned()));
}

async fn process_inner(state: &super::AppState, job_id: &str) {
    let workspace = &state.workspace;
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM durable_job WHERE id=?")
        .bind(job_id)
        .fetch_optional(workspace.pool())
        .await
        .ok()
        .flatten();
    if role.as_deref() == Some("coach") {
        super::inference::coach::run(state, job_id).await;
        return;
    }
    if role.as_deref() == Some("template_builder") {
        super::inference::template_builder::run(state, job_id).await;
        return;
    }
    if role.as_deref() == Some("extractor") {
        super::inference::extractor::run(state, job_id).await;
        return;
    }
    if role.as_deref() == Some("matcher") {
        super::inference::matcher::run(state, job_id).await;
        return;
    }
    if let Some(role) = role.as_deref() {
        if matches!(role, "interviewer" | "briefer" | "composer" | "linter") {
            super::inference::roles::run(state, job_id).await;
        } else if matches!(role, "template_builder") {
            super::inference::template_builder::run(state, job_id).await;
        } else {
            finish_job_error(
                workspace,
                job_id,
                "ROLE_UNSUPPORTED",
                "This role is not registered by any execution runtime",
                "failed",
            )
            .await;
        }
    }
}

fn role_defaults(role: &str) -> Option<(&'static str, &'static str)> {
    match role {
        "interviewer" => Some((INTERVIEWER_PROMPT, INTERVIEWER_SCHEMA)),
        "briefer" => Some((BRIEFER_PROMPT, BRIEFER_SCHEMA)),
        "matcher" => Some((MATCHER_PROMPT, MATCHER_SCHEMA)),
        "template_builder" => Some((BUILDER_PROMPT, BUILDER_SCHEMA)),
        "composer" => Some((COMPOSER_PROMPT, COMPOSER_SCHEMA)),
        "linter" => Some((LINTER_PROMPT, LINTER_SCHEMA)),
        _ => None,
    }
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
pub(crate) async fn get_provider(
    State(state): State<super::AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    use super::inference::{probe, profile};
    let result = async {
        let mut conn = state.workspace.pool().acquire().await?;
        let settings = profile::get_settings(&mut conn)
            .await
            .map_err(|_| WorkspaceError::Invalid("Local inference profile is unavailable".into()))?
            .ok_or(WorkspaceError::NotFound)?;
        let structured = probe::status(
            &mut conn,
            settings.revision,
            state.workspace.epoch(),
            "structured_role",
        )
        .await?;
        let embed = probe::status(
            &mut conn,
            settings.revision,
            state.workspace.epoch(),
            "embed",
        )
        .await?;
        Ok(json!({
            "endpoint":settings.endpoint,"model":settings.model.clone().unwrap_or_default(),"num_ctx":settings.num_ctx,
            "num_predict":settings.num_predict,"locale":settings.locale,
            "profile_revision":settings.revision,"pairing":settings.pairing,
            "structured":structured,"embed":embed
        }))
    }
    .await;
    match result {
        Ok(body) => {
            let mut response = Json(body).into_response();
            super::private_headers(&mut response, state.workspace.epoch());
            response
        }
        Err(error) => super::command_error(error),
    }
}
pub(crate) async fn update_provider(
    State(state): State<super::AppState>,
    headers: HeaderMap,
    Json(input): Json<ProviderUpdate>,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    if super::inference::profile::loopback_authority(&input.endpoint).is_err() {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "PROVIDER_ENDPOINT_INVALID",
            "Provider endpoint must be a canonical numeric loopback HTTP authority",
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
    use super::inference::profile::{self, ProfileConfiguration, ProfileError};
    let latest: i64 =
        match sqlx::query_scalar("SELECT coalesce(max(revision),0) FROM inference_profile")
            .fetch_one(&mut *tx)
            .await
        {
            Ok(revision) => revision,
            Err(error) => return super::command_error(error.into()),
        };
    if input.pairing.is_some() && input.expected_profile_revision.is_none() {
        return super::error(
            StatusCode::PRECONDITION_FAILED,
            "PROFILE_CHANGED",
            "Refresh provider settings before changing the pairing",
        );
    }
    let configuration = ProfileConfiguration {
        endpoint: input.endpoint.clone(),
        model: input.model.clone(),
        num_ctx: i64::from(input.num_ctx),
        num_predict: i64::from(input.num_predict),
        locale: input.locale.clone(),
        pairing: input.pairing.clone(),
    };
    match profile::replace_settings(
        &mut tx,
        input.expected_profile_revision.unwrap_or(latest),
        configuration,
        option_env!("PARLEY_RUNTIME_MANIFEST_SHA256"),
    )
    .await
    {
        Ok(_) => {}
        Err(ProfileError::Conflict) => {
            return super::error(
                StatusCode::PRECONDITION_FAILED,
                "PROFILE_CHANGED",
                "Provider settings changed. Reload them before saving",
            );
        }
        Err(ProfileError::Invalid(_)) => {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "PROVIDER_CONFIG_INVALID",
                "Check the local endpoint, model and pairing settings",
            );
        }
        Err(_) => {
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "PERSISTENCE_FAILED",
                "Provider settings are unavailable",
            );
        }
    }
    let now = now_utc();
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
    let event_cursor: Result<i64, sqlx::Error> = sqlx::query_scalar("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'inference_profile','extractor',?,'agent.provider_updated','{}') RETURNING cursor")
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
pub(crate) async fn provider_health(
    State(state): State<super::AppState>,
    headers: HeaderMap,
    payload: String,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    use super::inference::{probe, profile};
    let parsed = if payload.trim().is_empty() {
        None
    } else {
        serde_json::from_str::<Value>(&payload).ok()
    };
    let mode = match parsed
        .as_ref()
        .and_then(|value| value.get("mode"))
        .and_then(Value::as_str)
    {
        Some("embed") => "embed",
        None | Some("structured_role") => "structured_role",
        _ => {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "PROBE_MODE_INVALID",
                "Supported probe modes are structured_role and embed",
            );
        }
    };
    let label = if mode == "embed" {
        "Embedding"
    } else {
        "Structured"
    };
    let paired = async {
        let mut tx = state
            .workspace
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| super::command_error(error.into()))?;
        let settings = profile::get_settings(&mut tx).await.map_err(|_| {
            super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "PERSISTENCE_FAILED",
                "Provider settings are unavailable",
            )
        })?;
        let Some(settings) = settings.filter(|settings| settings.pairing.is_some()) else {
            return Ok(None);
        };
        if mode == "embed" && settings.embed_model.is_none() {
            return Err(super::error(
                StatusCode::PRECONDITION_FAILED,
                "PROFILE_CHANGED",
                "Choose an embedding model before checking embedding readiness",
            ));
        }
        if !probe::enqueue(&mut tx, settings.revision, state.workspace.epoch(), mode)
            .await
            .map_err(|error| super::command_error(error.into()))?
        {
            return Err(super::error(
                StatusCode::TOO_MANY_REQUESTS,
                "INFERENCE_BUSY",
                "A local model check is already queued",
            ));
        }
        let readiness = probe::status(&mut tx, settings.revision, state.workspace.epoch(), mode)
            .await
            .map_err(|error| super::command_error(error.into()))?;
        tx.commit()
            .await
            .map_err(|error| super::command_error(error.into()))?;
        state.jobs.notify_one();
        let mut response = (
            StatusCode::ACCEPTED,
            Json(json!({
                "available":false,"model":settings.model.unwrap_or_default(),"models":[],
                "message":format!("{label} model check queued"),
                "structured":if mode=="structured_role" {serde_json::to_value(&readiness).ok()} else {None},
                "embed":if mode=="embed" {serde_json::to_value(&readiness).ok()} else {None},
            })),
        )
            .into_response();
        super::private_headers(&mut response, state.workspace.epoch());
        Ok(Some(response))
    }
    .await;
    match paired {
        Ok(Some(response)) | Err(response) => return response,
        Ok(None) => {}
    }
    let mut conn = match state.workspace.pool().acquire().await {
        Ok(conn) => conn,
        Err(error) => return super::command_error(error.into()),
    };
    let settings = match profile::get_settings(&mut conn).await {
        Ok(Some(settings)) => settings,
        Ok(None) => {
            return Json(json!({
                "available":false,"model":"","models":[],
                "message":"Local inference is not configured",
            }))
            .into_response();
        }
        Err(_) => {
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "PERSISTENCE_FAILED",
                "Provider settings are unavailable",
            );
        }
    };
    let model = settings.model.clone().unwrap_or_default();
    Json(json!({
        "available":false,"model":model,"models":[],
        "message":"No paired local model is ready; use Settings to configure and qualify a local model",
        "diagnostic":settings.diagnostic,
    }))
    .into_response()
}
pub(crate) async fn get_job(
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
    if row.0 == "coach" {
        let projection: Result<Option<(i64, String, Option<String>)>, sqlx::Error> =
            sqlx::query_as(
                "SELECT j.state_revision, coalesce(r.period,j.scope_id), r.id
             FROM durable_job j LEFT JOIN coach_report r ON r.job_id=j.id
             WHERE j.id=? AND j.epoch=?",
            )
            .bind(&id)
            .bind(state.workspace.epoch())
            .fetch_optional(state.workspace.pool())
            .await;
        return match projection {
            Ok(Some((state_revision, period, report))) => {
                let mut response = Json(json!({
                    "id":id,"role":"coach","status":row.1,"state_revision":state_revision,
                    "attempt_count":row.2,"scope":{"kind":"weekly","period":period},
                    "progress":null,"error_code":row.4,
                    "review_ref":report.map(|id| json!({"kind":"coach_report","id":id}))
                }))
                .into_response();
                super::private_headers(&mut response, state.workspace.epoch());
                response
            }
            Ok(None) => super::error(StatusCode::NOT_FOUND, "JOB_NOT_FOUND", "Job does not exist"),
            Err(e) => super::command_error(e.into()),
        };
    }
    if row.0 == "template_builder" {
        let projection: Result<Option<(i64, Option<String>)>, sqlx::Error> = sqlx::query_as(
            "SELECT j.state_revision, (SELECT c.id FROM template_build_candidate c
              WHERE c.job_id=j.id ORDER BY c.variant_no LIMIT 1)
             FROM durable_job j WHERE j.id=? AND j.epoch=?",
        )
        .bind(&id)
        .bind(state.workspace.epoch())
        .fetch_optional(state.workspace.pool())
        .await;
        return match projection {
            Ok(Some((state_revision, candidate))) => {
                let mut response = Json(json!({
                    "id":id,"role":"template_builder","status":row.1,"state_revision":state_revision,
                    "attempt_count":row.2,"scope":{"kind":row.7,"id":row.8},
                    "progress":null,"error_code":row.4,
                    "review_ref":candidate.map(|id| json!({"kind":"template_build_candidate","id":id}))
                }))
                .into_response();
                super::private_headers(&mut response, state.workspace.epoch());
                response
            }
            Ok(None) => super::error(StatusCode::NOT_FOUND, "JOB_NOT_FOUND", "Job does not exist"),
            Err(e) => super::command_error(e.into()),
        };
    }
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
    if row.0 == "interviewer" {
        type Projection = (i64, Option<String>, Option<String>, Option<String>);
        let projection: Result<Option<Projection>, sqlx::Error> = sqlx::query_as(
            "SELECT j.state_revision, j.last_error_code, j.last_error,
                    (SELECT c.id FROM interviewer_candidate c WHERE c.job_id=j.id
                     ORDER BY c.created_at DESC LIMIT 1)
             FROM durable_job j WHERE j.id=?",
        )
        .bind(&id)
        .fetch_optional(state.workspace.pool())
        .await;
        return match projection {
            Ok(Some((state_revision, error_code, last_error, candidate))) => {
                let mut response = Json(json!({
                    "id":id,"role":"interviewer","status":row.1,"state_revision":state_revision,
                    "attempt_count":row.2,"current_attempt":row.2,"max_attempts":row.3,
                    "progress_current":row.9,"progress_total":row.10,
                    "injection_suspected":false,
                    "last_error_code":error_code,"last_error":last_error,
                    "source_occurrence_id":null,"scope_kind":"owner","scope_id":row.8,
                    "proposal_batch_id":null,"proposals":[],
                    "result":output_json.and_then(|value| serde_json::from_str::<Value>(&value).ok()),
                    "review_ref":candidate.map(|candidate| json!({"kind":"interviewer_candidate","id":candidate}))
                }))
                .into_response();
                super::private_headers(&mut response, state.workspace.epoch());
                response
            }
            Ok(None) => super::error(StatusCode::NOT_FOUND, "JOB_NOT_FOUND", "Job does not exist"),
            Err(e) => super::command_error(e.into()),
        };
    }
    if row.0 == "briefer" {
        type Projection = (i64, Option<String>, Option<String>);
        let projection: Result<Option<Projection>, sqlx::Error> = sqlx::query_as(
            "SELECT j.state_revision, j.last_error_code, j.last_error
             FROM durable_job j WHERE j.id=?",
        )
        .bind(&id)
        .fetch_optional(state.workspace.pool())
        .await;
        return match projection {
            Ok(Some((state_revision, error_code, last_error))) => {
                let mut response = Json(json!({
                    "id":id,"role":"briefer","status":row.1,"state_revision":state_revision,
                    "attempt_count":row.2,"current_attempt":row.2,"max_attempts":row.3,
                    "progress_current":row.9,"progress_total":row.10,
                    "injection_suspected":false,
                    "last_error_code":error_code,"last_error":last_error,
                    "source_occurrence_id":null,"scope_kind":row.7,"scope_id":row.8,
                    "proposal_batch_id":null,"proposals":[],
                    "result":output_json.and_then(|value| serde_json::from_str::<Value>(&value).ok()),
                    "review_ref":json!({"kind":"brief_scope","id":row.8})
                }))
                .into_response();
                super::private_headers(&mut response, state.workspace.epoch());
                response
            }
            Ok(None) => super::error(StatusCode::NOT_FOUND, "JOB_NOT_FOUND", "Job does not exist"),
            Err(e) => super::command_error(e.into()),
        };
    }
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
pub(crate) async fn get_capture_proposals(
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
pub(crate) async fn cancel_job(
    State(state): State<super::AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::authenticate(&headers, &state).await {
        return response.into_response();
    }
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|key| !key.trim().is_empty() && key.len() <= 200)
    else {
        return super::error(
            StatusCode::BAD_REQUEST,
            "IDEMPOTENCY_KEY_REQUIRED",
            "Idempotency-Key is required",
        );
    };
    let canonical =
        match super::application::coach::fingerprint(&json!({"operation":"cancelJob","job_id":id}))
        {
            Ok(canonical) => canonical,
            Err(error) => return super::command_error(error),
        };
    let epoch = state.workspace.epoch();
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    let row: Option<(String, i64)> = match sqlx::query_as(
        "SELECT status,current_attempt FROM durable_job WHERE id=? AND epoch=?",
    )
    .bind(&id)
    .bind(epoch)
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
    let receipt: Option<String> = match sqlx::query_scalar(
        "SELECT canonical_digest FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
    )
    .bind(epoch)
    .bind(key)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(receipt) => receipt,
        Err(error) => return super::command_error(error.into()),
    };
    if let Some(replay) = receipt {
        return if replay == canonical {
            let mut response = StatusCode::NO_CONTENT.into_response();
            super::private_headers(&mut response, epoch);
            response
        } else {
            super::error(
                StatusCode::CONFLICT,
                "IDEMPOTENCY_MISMATCH",
                "Idempotency key was used for a different request",
            )
        };
    }
    if !matches!(status.as_str(), "queued" | "running") {
        let _ = tx.rollback().await;
        return super::error(
            StatusCode::CONFLICT,
            "JOB_TERMINAL",
            "Terminal jobs cannot be cancelled",
        );
    }
    let now = now_utc();
    let changed = sqlx::query("UPDATE durable_job SET status='cancelled',last_error_code='CANCELLED',last_error='Owner cancelled job',lease_id=NULL,process_nonce=NULL,updated_at=?,completed_at=? WHERE id=? AND current_attempt=? AND status IN ('queued','running')").bind(&now).bind(&now).bind(&id).bind(attempt).execute(&mut *tx).await;
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
    if let Err(error) = sqlx::query("UPDATE job_attempt SET status='cancelled',error_code='CANCELLED',error_message='Owner cancelled job',finished_at=? WHERE job_id=? AND attempt_no=? AND status='running'").bind(&now).bind(&id).bind(attempt).execute(&mut *tx).await {
        let _ = tx.rollback().await; return super::command_error(error.into());
    }
    if let Err(error) = sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'job',?,?, 'job.cancelled','{}')").bind(new_id(&id)).bind(&now).bind(&id).bind(attempt).execute(&mut *tx).await {
        let _ = tx.rollback().await; return super::command_error(error.into());
    }
    let cursor = match sqlx::query_scalar("SELECT last_insert_rowid()")
        .fetch_one(&mut *tx)
        .await
    {
        Ok(cursor) => cursor,
        Err(error) => return super::command_error(error.into()),
    };
    if let Err(error) =
        super::assistant::write_receipt(&mut tx, epoch, key, &canonical, 204, &json!({}), cursor)
            .await
    {
        return super::command_error(error);
    }
    match tx.commit().await {
        Ok(()) => {
            state.jobs.notify_one();
            let mut response = StatusCode::NO_CONTENT.into_response();
            super::private_headers(&mut response, epoch);
            response
        }
        Err(error) => super::command_error(error.into()),
    }
}
pub(crate) async fn resolve_proposals(
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

pub(crate) struct ReviewFailure {
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

pub(crate) struct ReviewedPlan {
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
        "SELECT b.source_variant_id,b.source_occurrence_id,j.id FROM proposal_batch b JOIN inference_record j ON j.id=b.job_id WHERE b.id=? AND b.source_occurrence_id IS NOT NULL AND j.status='succeeded'",
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
