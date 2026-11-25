//! AR-001 AR07: offline E-match calibration for the qualified embedding model.
//!
//! `parley calibrate-matching` runs the bundled, reviewed E-match fixture
//! through the same packaged worker and relay as production matching, trains a
//! per-kind threshold on the training split (maximizing F1, ties choose the
//! higher threshold), then evaluates on the held-out split. Only a successful
//! held-out evaluation meeting the RFC-0001 floors may commit trusted
//! calibration rows bound to the exact recipe. No public raw calibration write
//! exists.

use super::{
    coach::{ProfileRow, embed_identity, installed_supervisor, profile_fields, tokenizer_path_for},
    profile::recipe_digest,
    protocol::{Completion, Execute, Profile},
};
use crate::{Workspace, digest, new_id, now_utc};
use parley_domain::matching::{self, Asset, AssetKind, LabeledScore, Weights};
use serde_json::{Value, json};
use sqlx::SqliteConnection;
use std::{collections::BTreeMap, net::SocketAddr};
use tokio::sync::watch;

const MAX_BATCH_INPUTS: usize = 16;
const EMBED_FRAME_SLACK: usize = 64 * 1024;
/// RFC-0001 §18.2 floors: gold asset in top-3 and correct no-match rate on a
/// held-out split must both clear their targets before a threshold is trusted.
const MIN_GOLD_IN_TOP3: f64 = 0.80;
const MIN_CORRECT_NO_MATCH: f64 = 0.90;

struct Active {
    revision: i64,
    profile: Profile,
    build: String,
    embed_digest: String,
    dimensions: u32,
    endpoint: SocketAddr,
}

pub(crate) async fn run(workspace: &Workspace, fixture: Value) -> Result<Value, String> {
    let cases = fixture["cases"]
        .as_array()
        .ok_or("E-match fixture has no cases")?;
    let need = fixture["need"]
        .as_str()
        .ok_or("E-match fixture has no need text")?
        .to_owned();
    let split = fixture["split"]
        .as_str()
        .ok_or("E-match fixture has no split marker")?;

    let mut conn = workspace
        .pool()
        .acquire()
        .await
        .map_err(|e| e.to_string())?;
    let active = active_profile(&mut conn).await?;
    let (weights, half_life) = crate::writing::scoring_config(&mut conn)
        .await
        .map_err(|e| e.to_string())?;
    let scoring =
        crate::writing::scoring_config_digest(&weights, half_life).map_err(|e| e.to_string())?;
    let recipe = recipe_digest(
        active.revision,
        &active.embed_digest,
        active.dimensions,
        &scoring,
    );

    let mut texts: Vec<String> = vec![need.clone()];
    for case in cases {
        let text = case["text"].as_str().ok_or("E-match case has no text")?;
        if !texts.iter().any(|existing| existing == text) {
            texts.push(text.to_owned());
        }
    }
    let vectors = embed_texts(workspace, &active, &texts).await?;
    let mut by_text: BTreeMap<String, Vec<f32>> = BTreeMap::new();
    for (text, vector) in texts.iter().zip(vectors) {
        by_text.insert(text.clone(), vector);
    }
    let need_vector = by_text.get(&need).ok_or("need embedding missing")?.clone();

    let weights: Weights =
        serde_json::from_value(weights).map_err(|e| format!("scoring weights invalid: {e}"))?;
    let mut train: Vec<Vec<LabeledScore>> = vec![Vec::new(), Vec::new(), Vec::new()];
    let mut holdout: Vec<Vec<LabeledScore>> = vec![Vec::new(), Vec::new(), Vec::new()];
    for case in cases {
        let kind = kind_of(case["kind"].as_str().ok_or("E-match case has no kind")?)?;
        let text = case["text"].as_str().ok_or("E-match case has no text")?;
        let relevant = case["relevant"]
            .as_bool()
            .ok_or("E-match case has no label")?;
        let vector = by_text.get(text).ok_or("case embedding missing")?.clone();
        let asset = Asset {
            id: format!("case:{}", case["id"].as_str().unwrap_or("?")),
            kind,
            embedding: vector,
            tags: Vec::new(),
            age_months: 0.0,
            advances: 0,
            sends_using_asset: 0,
        };
        let scored = matching::score(&asset, &need_vector, &[], half_life, weights)
            .map_err(|e| format!("scoring failed: {e:?}"))?;
        let label = LabeledScore {
            score: scored.score,
            relevant,
        };
        let bucket = match kind {
            AssetKind::Gift => 0,
            AssetKind::Proof => 1,
            AssetKind::Artifact => 2,
        };
        if case[split].as_bool().unwrap_or(false) {
            holdout[bucket].push(label);
        } else {
            train[bucket].push(label);
        }
    }

    let fixture_digest = digest(&serde_json::to_vec(&fixture).map_err(|e| e.to_string())?);
    let mut calibrations = Vec::new();
    let mut holdout_summary = serde_json::Map::new();
    for (index, kind) in [AssetKind::Gift, AssetKind::Proof, AssetKind::Artifact]
        .into_iter()
        .enumerate()
    {
        let train_samples = &train[index];
        let holdout_samples = &holdout[index];
        if train_samples.len() < 6 || holdout_samples.len() < 4 {
            return Err(format!(
                "E-match fixture is too small for {}; training={} held-out={}",
                format!("{kind:?}").to_lowercase(),
                train_samples.len(),
                holdout_samples.len()
            ));
        }
        let report = matching::calibrate(&active.embed_digest, &fixture_digest, train_samples)
            .map_err(|e| format!("calibration failed for {kind:?}: {e:?}"))?;
        let gold_in_top3 = recall_at_top3(holdout_samples, report.threshold);
        let correct_no_match = no_match_rate(holdout_samples, report.threshold);
        if gold_in_top3 < MIN_GOLD_IN_TOP3 || correct_no_match < MIN_CORRECT_NO_MATCH {
            return Err(format!(
                "E-match held-out floors not met for {}: gold-in-top3={gold_in_top3:.3} (<{MIN_GOLD_IN_TOP3}), correct-no-match={correct_no_match:.3} (<{MIN_CORRECT_NO_MATCH})",
                format!("{kind:?}").to_lowercase()
            ));
        }
        calibrations.push(json!({
            "asset_kind": format!("{kind:?}").to_lowercase(),
            "threshold": report.threshold,
            "fixture_digest": report.fixture_digest,
            "positives": report.positives,
            "negatives": report.negatives,
            "true_positives": report.true_positives,
            "false_positives": report.false_positives,
            "false_negatives": report.false_negatives,
            "holdout_gold_in_top3": gold_in_top3,
            "holdout_correct_no_match": correct_no_match,
        }));
        holdout_summary.insert(
            format!("{kind:?}").to_lowercase(),
            json!({"gold_in_top3": gold_in_top3, "correct_no_match": correct_no_match}),
        );
    }

    // Commit trusted rows under the writer only while the profile is still the
    // active qualified one; a changed profile invalidates the whole recipe.
    let mut tx = workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| e.to_string())?;
    let current = active_profile(&mut tx).await?;
    if current.revision != active.revision
        || current.embed_digest != active.embed_digest
        || current.dimensions != active.dimensions
    {
        return Err("PROFILE_CHANGED".into());
    }
    let now = now_utc();
    for row in &calibrations {
        let kind = row["asset_kind"].as_str().unwrap_or_default();
        let threshold = row["threshold"].as_f64().unwrap_or_default();
        let fixture_digest_row = row["fixture_digest"].as_str().unwrap_or_default();
        let revision: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(calibration_revision),0)+1 FROM matching_calibration
             WHERE recipe_digest=? AND asset_kind=?",
        )
        .bind(&recipe)
        .bind(kind)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        sqlx::query(
            "INSERT INTO matching_calibration(
               model_digest,asset_kind,threshold,fixture_digest,positives,negatives,
               true_positives,false_positives,false_negatives,calibration_revision,
               scoring_config_digest,trusted,recipe_digest,accuracy,precision,recall,created_at)
             VALUES(?,?,?,?,?,?,?,?,?,?,?,1,?,?,?,?,?)",
        )
        .bind(&active.embed_digest)
        .bind(kind)
        .bind(threshold)
        .bind(fixture_digest_row)
        .bind(row["positives"].as_i64().unwrap_or_default())
        .bind(row["negatives"].as_i64().unwrap_or_default())
        .bind(row["true_positives"].as_i64().unwrap_or_default())
        .bind(row["false_positives"].as_i64().unwrap_or_default())
        .bind(row["false_negatives"].as_i64().unwrap_or_default())
        .bind(revision)
        .bind(&scoring)
        .bind(&recipe)
        .bind(row["holdout_gold_in_top3"].as_f64().unwrap_or_default())
        .bind(row["holdout_gold_in_top3"].as_f64().unwrap_or_default())
        .bind(row["holdout_correct_no_match"].as_f64().unwrap_or_default())
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }
    let event = json!({
        "recipe_digest": recipe,
        "embed_model_digest": active.embed_digest,
        "calibrations": calibrations,
        "holdout": holdout_summary,
    });
    sqlx::query(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json)
         VALUES(?,?, 'embedding_model','workspace',1,'embedding.calibrated',?)",
    )
    .bind(new_id(&recipe))
    .bind(&now)
    .bind(event.to_string())
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(json!({
        "status": "calibrated",
        "recipe_digest": recipe,
        "embed_model_digest": active.embed_digest,
        "calibrations": calibrations,
        "holdout": holdout_summary,
    }))
}

fn kind_of(kind: &str) -> Result<AssetKind, String> {
    match kind {
        "gift" => Ok(AssetKind::Gift),
        "proof" => Ok(AssetKind::Proof),
        "artifact" => Ok(AssetKind::Artifact),
        _ => Err(format!("unsupported E-match kind {kind}")),
    }
}

async fn active_profile(conn: &mut SqliteConnection) -> Result<Active, String> {
    let revision: i64 = sqlx::query_scalar(
        "SELECT a.profile_revision FROM inference_active_profile a
         WHERE a.singleton=1
           AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r
                          WHERE r.profile_revision=a.profile_revision)",
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| e.to_string())?;
    let row: Option<ProfileRow> = sqlx::query_as(
        "SELECT p.endpoint,p.runner_build_digest,p.provider_version,p.chat_model,p.chat_model_digest,
                p.tokenizer_digest,p.recipe_json,p.context_tokens,p.output_tokens,
                p.embed_model,p.embed_model_digest
         FROM inference_profile p WHERE revision=?
           AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=p.revision)
           AND EXISTS(SELECT 1 FROM inference_qualification q WHERE q.profile_revision=p.revision
                      AND q.mode='embed' AND q.status='ready' AND q.checked_at >= ?)",
    )
    .bind(revision)
    .bind(crate::now_utc_offset(-300))
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| e.to_string())?;
    let (endpoint, mut profile, build) =
        profile_fields(revision, row.ok_or("CAPABILITY_UNQUALIFIED")?)
            .ok_or("CAPABILITY_UNQUALIFIED")?;
    let (digest, dimensions) = embed_identity(&mut *conn, revision)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("CAPABILITY_UNQUALIFIED")?;
    profile.embed_digest = Some(digest.clone());
    profile.embed_dimensions = Some(dimensions);
    Ok(Active {
        revision,
        profile,
        build,
        embed_digest: digest,
        dimensions,
        endpoint,
    })
}

async fn embed_texts(
    workspace: &Workspace,
    active: &Active,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    let supervisor = installed_supervisor().map_err(|code| code.to_string())?;
    if supervisor.manifest_digest() != active.build {
        return Err("PROFILE_CHANGED".into());
    }
    let tokenizer = tokenizer_path_for(workspace, &active.profile)?;
    let dimensions = active.dimensions as usize;
    let batch_cap = MAX_BATCH_INPUTS.min(
        (super::protocol::MAX_FRAME - EMBED_FRAME_SLACK)
            .checked_div(dimensions.saturating_mul(9).saturating_add(64))
            .unwrap_or(1)
            .max(1),
    );
    let mut vectors = Vec::with_capacity(texts.len());
    for batch in texts.chunks(batch_cap) {
        let execution = new_id("calibrate-embed");
        let request = Execute::Embed {
            profile: active.profile.clone(),
            deadline_ms: 120_000,
            inputs: batch.to_vec(),
            dimensions: active.dimensions,
        };
        let (_cancel, signal) = watch::channel(false);
        let result = supervisor
            .execute(
                active.endpoint,
                &execution,
                "calibrate-lease",
                &tokenizer,
                request,
                signal,
            )
            .await;
        let completion = result.map_err(|error| error.code().to_string())?;
        match completion {
            Completion::Embed {
                vectors: batch_vectors,
                ..
            } => vectors.extend(batch_vectors),
            _ => return Err("WORKER_PROTOCOL".into()),
        }
    }
    Ok(vectors)
}

/// Recall at top-3: the share of relevant held-out samples ranked in the top
/// three by score (mirrors RFC-0001 "gold asset in top-3").
fn recall_at_top3(samples: &[LabeledScore], _threshold: f64) -> f64 {
    let relevant = samples.iter().filter(|s| s.relevant).count();
    if relevant == 0 {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| b.score.total_cmp(&a.score));
    let in_top3 = sorted.iter().take(3).filter(|s| s.relevant).count();
    in_top3 as f64 / relevant as f64
}

/// Correct no-match rate: the share of irrelevant held-out samples that score
/// below the committed threshold (must return `no_match`).
fn no_match_rate(samples: &[LabeledScore], threshold: f64) -> f64 {
    let irrelevant = samples.iter().filter(|s| !s.relevant).count();
    if irrelevant == 0 {
        return 0.0;
    }
    let correct = samples
        .iter()
        .filter(|s| !s.relevant && s.score < threshold)
        .count();
    correct as f64 / irrelevant as f64
}
