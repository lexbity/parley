//! AR-001 AR07: the matcher role runs through the packaged worker.
//!
//! Admission snapshots the eligible resource set and scoring configuration
//! (`application/match_start.rs`). Execution embeds the need and every
//! selected resource with the separately qualified embedding model over the
//! same Unix relay (batched, recipe-bound cache commits on each completed
//! batch), ranks deterministically with the trusted recipe-bound thresholds,
//! then asks the worker for bounded match explanations via the matcher
//! structured role. No Rust path touches a model transport.

use super::{
    coach::{ProfileRow, embed_identity, installed_supervisor, profile_fields, tokenizer_path},
    protocol::{Completion, Execute, Profile, Role},
};
use crate::{AppState, WorkspaceError, digest, jobs, new_id, now_utc, now_utc_offset, random_hex};
use parley_domain::matching::{self, Asset, AssetKind, MatchResult, Weights};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    time::Duration,
};
use tokio::sync::watch;

const MAX_ELIGIBLE_RESOURCES: usize = 4096;
const MAX_BATCH_INPUTS: usize = 16;
const EMBED_FRAME_SLACK: usize = 64 * 1024;
const HALF_LIFE_MONTHS: f64 = 24.0;

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Input {
    opportunity_id: String,
    recipient_id: String,
    opportunity_revision: i64,
    recipient_revision: i64,
    need_text: String,
    #[serde(default)]
    party_tags: Vec<String>,
    resources: Vec<ResourceInput>,
    proof_half_life_months: f64,
    weights: Value,
    scoring_config_digest: String,
    #[serde(default)]
    party_fields: Value,
    #[serde(default)]
    requested_at: Option<String>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ResourceInput {
    resource_id: String,
    kind: String,
    title: String,
    revision: i64,
    #[serde(default)]
    age_months: f64,
    body: Value,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    advances: u64,
    #[serde(default)]
    sends_using_asset: u64,
}

#[derive(sqlx::FromRow)]
struct Frozen {
    status: String,
    current_attempt: i64,
    max_attempts: i64,
    queue_deadline_at: String,
    total_deadline_at: Option<String>,
    profile_revision: i64,
    input_sha256: String,
    input_json: String,
}

struct Lease {
    number: i64,
    token: String,
    input_sha256: String,
    input: Input,
    profile: Profile,
    runner_digest: String,
    endpoint: SocketAddr,
    remaining_ms: u32,
    recipe_digest: String,
}

enum Claim {
    Missing,
    Failure(&'static str),
    Ready(Box<Lease>),
}

/// Resolve the active qualified profile and the observed embedding identity.
async fn qualified_profile(
    tx: &mut Transaction<'_, Sqlite>,
    revision: i64,
) -> Option<(SocketAddr, Profile, String, String, u32)> {
    let row: Option<ProfileRow> = sqlx::query_as(
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
    .fetch_optional(&mut **tx)
    .await
    .ok()?;
    let row = row?;
    let (endpoint, mut profile, build) = profile_fields(revision, row)?;
    let (digest, dimensions) = embed_identity(tx, revision).await.ok()??;
    profile.embed_digest = Some(digest.clone());
    profile.embed_dimensions = Some(dimensions);
    Some((endpoint, profile, build, digest, dimensions))
}

async fn claim(state: &AppState, id: &str) -> Result<Claim, WorkspaceError> {
    let workspace = &state.workspace;
    let mut tx = workspace.pool().begin_with("BEGIN IMMEDIATE").await?;
    let frozen: Option<Frozen> = sqlx::query_as(
        "SELECT status,current_attempt,max_attempts,queue_deadline_at,total_deadline_at,
                profile_revision,input_sha256,input_json
         FROM durable_job WHERE id=? AND epoch=? AND role='matcher' AND profile_revision IS NOT NULL",
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
    let profile = if input.is_some() {
        qualified_profile(&mut tx, frozen.profile_revision).await
    } else {
        None
    };
    let failure = if frozen.current_attempt == 0 && frozen.queue_deadline_at < now {
        Some("QUEUE_EXPIRED")
    } else if total_deadline <= now || frozen.current_attempt >= frozen.max_attempts {
        Some("ATTEMPT_TIMEOUT")
    } else if input.is_none() {
        Some("RESULT_INVALID")
    } else if profile.is_none() {
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
    let input = input.expect("validated frozen matcher input");
    let (endpoint, profile, runner_digest, embed_digest, dimensions) =
        profile.expect("validated qualified matcher profile");
    let recipe_digest = super::profile::recipe_digest(
        frozen.profile_revision,
        &embed_digest,
        dimensions,
        &input.scoring_config_digest,
    );
    let number = frozen.current_attempt + 1;
    let token = random_hex(32);
    let process_nonce = crate::inference::dispatch::process_nonce();
    sqlx::query("UPDATE durable_job SET status='running',current_attempt=?,lease_id=?,process_nonce=?,total_deadline_at=?,updated_at=? WHERE id=?")
        .bind(number).bind(&token).bind(process_nonce).bind(&total_deadline).bind(&now).bind(id)
        .execute(&mut *tx).await?;
    let prompt_digest = digest(jobs::matcher_prompt().as_bytes());
    let schema_digest = digest(jobs::matcher_schema().as_bytes());
    sqlx::query(
        "INSERT INTO job_attempt(job_id,attempt_no,status,model,model_digest,provider_snapshot_json,
             prompt_id,prompt_sha256,input_sha256,input_json,started_at)
         VALUES(?,?,'running',?,?,?,?,?,?,?,?)",
    )
        .bind(id).bind(number).bind(&profile.model_tag).bind(&profile.model_digest)
        .bind(serde_json::to_string(&profile).map_err(|_| WorkspaceError::Invalid("Invalid profile".into()))?)
        .bind("matcher@1").bind(&prompt_digest).bind(&frozen.input_sha256)
        .bind(frozen.input_json).bind(&now).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE inference_record SET runner_build_digest=?,prompt_digest=?,schema_digest=?,model_digest=? WHERE id=?",
    ).bind(&runner_digest).bind(&prompt_digest).bind(&schema_digest).bind(&embed_digest).bind(id)
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
        input_sha256: frozen.input_sha256,
        input,
        profile,
        runner_digest,
        endpoint,
        remaining_ms,
        recipe_digest,
    })))
}

/// Writer fence for intermediate and terminal writes: the job must still own
/// its lease on the current epoch with an unrevoked profile.
async fn fence(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    lease: &Lease,
) -> Result<(), WorkspaceError> {
    let bound: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM durable_job j JOIN workspace_meta w ON w.singleton=1 AND w.epoch=j.epoch
          WHERE j.id=? AND j.status='running' AND j.current_attempt=? AND j.lease_id=?
          AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=j.profile_revision))",
    ).bind(id).bind(lease.number).bind(&lease.token).fetch_one(&mut **tx).await?;
    if !bound {
        return Err(WorkspaceError::StaleRevision);
    }
    Ok(())
}

async fn run_embed_batch(
    state: &AppState,
    id: &str,
    lease: &Lease,
    inputs: Vec<String>,
) -> Result<Vec<Vec<f32>>, &'static str> {
    if lease.remaining_ms < 1 {
        return Err("ATTEMPT_TIMEOUT");
    }
    let supervisor = installed_supervisor()?;
    if supervisor.manifest_digest() != lease.runner_digest {
        return Err("PROFILE_CHANGED");
    }
    let tokenizer = tokenizer_path(state, &lease.profile)?;
    let execution = new_id("matcher-embed");
    let dimensions = lease.profile.embed_dimensions.unwrap_or_default();
    let request = Execute::Embed {
        profile: lease.profile.clone(),
        deadline_ms: lease.remaining_ms,
        inputs,
        dimensions,
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
        Completion::Embed { vectors, .. } => Ok(vectors),
        Completion::Probe { .. }
        | Completion::StructuredRole { .. }
        | Completion::Delegated { .. } => Err("WORKER_PROTOCOL"),
    }
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
        Ok(result) => {
            if let Err(error) = publish(state, id, &lease, result).await {
                let code = if matches!(error, WorkspaceError::StaleRevision) {
                    "STALE_INPUT"
                } else {
                    "PERSISTENCE_FAILED"
                };
                fail(state, id, &lease, code, "failed").await;
            }
        }
        Err(code) => {
            let disposition = match code {
                "RESULT_INVALID" if lease.number < 3 => "retry",
                "PROVIDER_FAILED" | "INFERENCE_TIMEOUT" | "MODEL_UNAVAILABLE" => "retry",
                _ => "failed",
            };
            fail(state, id, &lease, code, disposition).await;
        }
    }
}

struct Ranked {
    ranked: Vec<MatchResult>,
    asset_fields: BTreeMap<String, BTreeSet<String>>,
    calibration: Value,
    party_fields: BTreeSet<String>,
}

async fn execute(state: &AppState, id: &str, lease: &Lease) -> Result<Ranked, &'static str> {
    let input = &lease.input;
    if input.resources.len() > MAX_ELIGIBLE_RESOURCES {
        return Err("CONTEXT_BUDGET");
    }
    let need_hash = digest(input.need_text.as_bytes());
    let mut texts: Vec<(String, String)> = Vec::with_capacity(input.resources.len() + 1);
    texts.push((need_hash.clone(), input.need_text.clone()));
    for resource in &input.resources {
        let text = format!("{}\n{}", resource.title, resource.body);
        texts.push((digest(text.as_bytes()), text));
    }
    let cached: BTreeSet<String> =
        sqlx::query_scalar("SELECT text_sha256 FROM embedding_cache WHERE recipe_digest=?")
            .bind(&lease.recipe_digest)
            .fetch_all(state.workspace.pool())
            .await
            .map_err(|_| "PERSISTENCE_FAILED")?
            .into_iter()
            .collect();
    let dimensions = lease.profile.embed_dimensions.unwrap_or_default() as usize;
    let batch_cap = MAX_BATCH_INPUTS.min(
        (super::protocol::MAX_FRAME - EMBED_FRAME_SLACK)
            .checked_div(dimensions.saturating_mul(9).saturating_add(64))
            .unwrap_or(1)
            .max(1),
    );
    let mut vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
    let mut pending: Vec<(String, String)> = texts
        .iter()
        .filter(|(hash, _)| !cached.contains(hash))
        .cloned()
        .collect();
    let total = texts.len();
    while !pending.is_empty() {
        let batch: Vec<(String, String)> = pending.drain(..batch_cap.min(pending.len())).collect();
        let batch_hashes: Vec<String> = batch.iter().map(|(hash, _)| hash.clone()).collect();
        let batch_inputs: Vec<String> = batch.into_iter().map(|(_, text)| text).collect();
        let result = run_embed_batch(state, id, lease, batch_inputs).await?;
        for (hash, vector) in batch_hashes.into_iter().zip(result) {
            vectors.insert(hash, vector);
        }
        // Commit completed batches only under a still-valid lease and recipe.
        let mut tx = state
            .workspace
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| "PERSISTENCE_FAILED")?;
        fence(&mut tx, id, lease).await.map_err(|_| "STALE_INPUT")?;
        for (hash, vector) in &vectors {
            let json_vector = serde_json::to_string(vector).map_err(|_| "RESULT_INVALID")?;
            sqlx::query(
                "INSERT OR IGNORE INTO embedding_cache(recipe_digest,model_digest,text_sha256,dimensions,vector_json,created_at)
                 VALUES(?,?,?,?,?,?)",
            )
            .bind(&lease.recipe_digest)
            .bind(lease.profile.embed_digest.as_deref().unwrap_or_default())
            .bind(hash)
            .bind(dimensions as i64)
            .bind(&json_vector)
            .bind(now_utc())
            .execute(&mut *tx)
            .await
            .map_err(|_| "PERSISTENCE_FAILED")?;
        }
        let done = total - pending.len();
        sqlx::query("UPDATE durable_job SET progress_current=?,progress_total=?,updated_at=? WHERE id=? AND lease_id=?")
            .bind(done as i64).bind(total as i64).bind(now_utc()).bind(id).bind(&lease.token)
            .execute(&mut *tx).await.map_err(|_| "PERSISTENCE_FAILED")?;
        tx.commit().await.map_err(|_| "PERSISTENCE_FAILED")?;
    }
    let need_vector = match vectors.get(&need_hash).cloned() {
        Some(vector) => vector,
        None => {
            let json_vector: String = sqlx::query_scalar(
                "SELECT vector_json FROM embedding_cache WHERE recipe_digest=? AND text_sha256=?",
            )
            .bind(&lease.recipe_digest)
            .bind(&need_hash)
            .fetch_one(state.workspace.pool())
            .await
            .map_err(|_| "PERSISTENCE_FAILED")?;
            serde_json::from_str(&json_vector).map_err(|_| "RESULT_INVALID")?
        }
    };
    let mut assets = Vec::new();
    let mut asset_fields = BTreeMap::<String, BTreeSet<String>>::new();
    for resource in &input.resources {
        let resource_hash = digest(format!("{}\n{}", resource.title, resource.body).as_bytes());
        let vector = match vectors.get(&resource_hash).cloned() {
            Some(vector) => vector,
            None => {
                let json_vector: String = sqlx::query_scalar(
                    "SELECT vector_json FROM embedding_cache WHERE recipe_digest=? AND text_sha256=?",
                )
                .bind(&lease.recipe_digest)
                .bind(&resource_hash)
                .fetch_one(state.workspace.pool())
                .await
                .map_err(|_| "PERSISTENCE_FAILED")?;
                serde_json::from_str(&json_vector).map_err(|_| "RESULT_INVALID")?
            }
        };
        let kind = match resource.kind.as_str() {
            "gift" => AssetKind::Gift,
            "proof" => AssetKind::Proof,
            "artifact" => AssetKind::Artifact,
            _ => return Err("RESULT_INVALID"),
        };
        let mut fields = resource
            .body
            .as_object()
            .into_iter()
            .flat_map(|object| object.keys().cloned())
            .collect::<BTreeSet<_>>();
        fields.insert("title".to_owned());
        asset_fields.insert(resource.resource_id.clone(), fields);
        assets.push(Asset {
            id: resource.resource_id.clone(),
            kind,
            embedding: vector,
            tags: resource.tags.clone(),
            age_months: resource.age_months,
            advances: resource.advances.min(u32::MAX as u64) as u32,
            sends_using_asset: resource.sends_using_asset.min(u32::MAX as u64) as u32,
        });
    }
    let weights: Weights =
        serde_json::from_value(input.weights.clone()).map_err(|_| "RESULT_INVALID")?;
    let calibrations: Vec<(String, f64, String)> = sqlx::query_as(
        "SELECT asset_kind,threshold,fixture_digest FROM matching_calibration
         WHERE trusted=1 AND recipe_digest=? ORDER BY asset_kind",
    )
    .bind(&lease.recipe_digest)
    .fetch_all(state.workspace.pool())
    .await
    .map_err(|_| "PERSISTENCE_FAILED")?;
    if calibrations.len() != 3 {
        return Err("CALIBRATION_REQUIRED");
    }
    let mut thresholds = Vec::new();
    let mut calibration_json = serde_json::Map::new();
    for (kind, value, fixture_digest) in calibrations {
        let kind = match kind.as_str() {
            "gift" => AssetKind::Gift,
            "proof" => AssetKind::Proof,
            "artifact" => AssetKind::Artifact,
            _ => continue,
        };
        thresholds.push((kind, value));
        calibration_json.insert(
            format!("{kind:?}").to_lowercase(),
            json!({"threshold":value,"fixture_digest":fixture_digest,
                   "model_digest":lease.profile.embed_digest,"recipe_digest":lease.recipe_digest}),
        );
    }
    let ranked = matching::rank(
        &assets,
        &need_vector,
        &input.party_tags,
        if input.proof_half_life_months.is_finite() && input.proof_half_life_months > 0.0 {
            input.proof_half_life_months
        } else {
            HALF_LIFE_MONTHS
        },
        weights,
        &thresholds,
    )
    .map_err(|_| "RESULT_INVALID")?;
    let party_fields = BTreeSet::from(["need".to_owned()]);
    Ok(Ranked {
        ranked,
        asset_fields,
        calibration: Value::Object(calibration_json),
        party_fields,
    })
}

async fn explain(
    state: &AppState,
    id: &str,
    lease: &Lease,
    ranked: &Ranked,
) -> Result<Value, &'static str> {
    let shortlist_ids: Vec<String> = ranked
        .ranked
        .iter()
        .flat_map(|result| result.candidates.iter())
        .map(|candidate| candidate.id.clone())
        .collect();
    if shortlist_ids.is_empty() {
        return Ok(json!({"explanations":[]}));
    }
    let shortlist: Vec<Value> = ranked
        .ranked
        .iter()
        .flat_map(|result| result.candidates.iter())
        .map(|candidate| {
            json!({
                "id": candidate.id,
                "kind": candidate.kind,
                "score": candidate.score,
                "components": {
                    "cosine": candidate.cosine,
                    "tags": candidate.tag_overlap,
                    "recency": candidate.recency,
                    "track": candidate.track,
                },
            })
        })
        .collect();
    let input = json!({
        "party_fields": ranked.party_fields,
        "shortlist": shortlist,
    });
    let supervisor = installed_supervisor()?;
    if supervisor.manifest_digest() != lease.runner_digest {
        return Err("PROFILE_CHANGED");
    }
    let tokenizer = tokenizer_path(state, &lease.profile)?;
    let execution = new_id("matcher-explain");
    let request = Execute::StructuredRole {
        profile: lease.profile.clone(),
        deadline_ms: lease.remaining_ms,
        role: Role::Matcher,
        prompt_version: "matcher@1".into(),
        schema_digest: digest(jobs::matcher_schema().as_bytes()),
        system: jobs::matcher_prompt().to_owned(),
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
    let candidate = match result {
        Completion::StructuredRole { candidate, .. } => candidate,
        Completion::Probe { .. } | Completion::Delegated { .. } | Completion::Embed { .. } => {
            return Err("WORKER_PROTOCOL");
        }
    };
    let explanations = crate::writing::validate_match_explanations(
        &shortlist_ids,
        &candidate,
        &ranked.party_fields,
        &ranked.asset_fields,
    );
    Ok(json!({"explanations": shortlist_ids.iter().map(|id| {
        json!({
            "resource_id": id,
            "why": explanations.get(id).cloned().unwrap_or_else(|| "Matches on deterministic similarity and available resource evidence.".to_owned()),
        })
    }).collect::<Vec<_>>()}))
}

async fn publish(
    state: &AppState,
    id: &str,
    lease: &Lease,
    ranked: Ranked,
) -> Result<(), WorkspaceError> {
    let explanations = explain(state, id, lease, &ranked).await.map_err(|code| {
        if code == "STALE_INPUT" {
            WorkspaceError::StaleRevision
        } else {
            WorkspaceError::Invalid(code.into())
        }
    })?;
    let explanations_by_id: BTreeMap<String, String> = explanations["explanations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some((
                item["resource_id"].as_str()?.to_owned(),
                item["why"].as_str()?.to_owned(),
            ))
        })
        .collect();
    let results: Vec<Value> = ranked
        .ranked
        .iter()
        .map(|result| {
            let candidates = result
                .candidates
                .iter()
                .map(|candidate| {
                    json!({
                        "id": candidate.id,
                        "score": candidate.score,
                        "components": {
                            "cosine": candidate.cosine,
                            "tags": candidate.tag_overlap,
                            "recency": candidate.recency,
                            "track": candidate.track,
                        },
                        "why": explanations_by_id.get(&candidate.id).cloned().unwrap_or_default(),
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "kind": result.kind,
                "threshold": result.threshold,
                "no_match": result.no_match,
                "candidates": candidates,
            })
        })
        .collect();
    let input = &lease.input;
    let shortlist = serde_json::to_string(&json!({"results": results}))
        .map_err(|_| WorkspaceError::Invalid("Shortlist serialization failed".into()))?;
    let calibration = serde_json::to_string(&ranked.calibration)
        .map_err(|_| WorkspaceError::Invalid("Calibration serialization failed".into()))?;
    let mut tx = state.workspace.pool().begin_with("BEGIN IMMEDIATE").await?;
    fence(&mut tx, id, lease).await?;
    jobs::scoped_job_fence(&mut tx, id)
        .await
        .map_err(|_| WorkspaceError::StaleRevision)?;
    crate::writing_jobs::ensure_scope_current(
        &mut tx,
        id,
        &serde_json::to_value(input).unwrap_or_default(),
    )
    .await
    .map_err(|_| WorkspaceError::StaleRevision)?;
    // The recipe must still match the live qualified embedding identity.
    let (_, _, _, embed_digest, dimensions) =
        qualified_profile(&mut tx, lease.profile.revision as i64)
            .await
            .ok_or(WorkspaceError::StaleRevision)?;
    let current_recipe = super::profile::recipe_digest(
        lease.profile.revision as i64,
        &embed_digest,
        dimensions,
        &input.scoring_config_digest,
    );
    if current_recipe != lease.recipe_digest {
        return Err(WorkspaceError::StaleRevision);
    }
    let id_run = new_id(&format!("{id}:match"));
    sqlx::query(
        "INSERT INTO match_run(id,opportunity_id,recipient_id,opportunity_revision,recipient_revision,
             need_sha256,embed_model_digest,recipe_digest,calibration_json,shortlist_json,why_job_id,created_at)
         VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&id_run)
    .bind(&input.opportunity_id)
    .bind(&input.recipient_id)
    .bind(input.opportunity_revision)
    .bind(input.recipient_revision)
    .bind(digest(input.need_text.as_bytes()))
    .bind(lease.profile.embed_digest.as_deref().unwrap_or_default())
    .bind(&lease.recipe_digest)
    .bind(calibration)
    .bind(&shortlist)
    .bind(id)
    .bind(now_utc())
    .execute(&mut *tx)
    .await?;
    // TD-04: normalize every text atom of the committed shortlist in the same
    // transaction, using the identical extraction rule as migration 0900's
    // backfill and the old usage-count query. The side table is what makes the
    // party usage read O(index probe) instead of O(all match runs).
    sqlx::query(
        "INSERT INTO match_run_mention(match_run_id,entity_id) \
         SELECT DISTINCT ?,j.atom FROM json_tree(?) j \
         WHERE j.type='text' AND j.atom IS NOT NULL \
         ON CONFLICT(match_run_id,entity_id) DO NOTHING",
    )
    .bind(&id_run)
    .bind(&shortlist)
    .execute(&mut *tx)
    .await?;
    let result = json!({
        "opportunity_id": input.opportunity_id,
        "recipient_id": input.recipient_id,
        "model_digest": lease.profile.embed_digest,
        "recipe_digest": lease.recipe_digest,
        "calibration": ranked.calibration,
        "results": results,
        "status": "ready_for_owner_selection",
    });
    let (tokens_in, tokens_out) = (0u32, 0u32);
    jobs::complete_role_result_in_tx(
        &mut tx,
        id,
        lease.number,
        &lease.input_sha256,
        &result,
        tokens_in,
        tokens_out,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn fail(state: &AppState, id: &str, lease: &Lease, code: &str, disposition: &str) {
    jobs::finish_attempt_error(
        &state.workspace,
        id,
        lease.number,
        code,
        "Local inference did not publish a match result",
        disposition,
    )
    .await;
}
