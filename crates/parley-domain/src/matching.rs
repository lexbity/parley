//! Exact deterministic asset matching (RFC-0001 §10.4).
//!
//! Embeddings and labels are supplied by the service adapter. Ranking never
//! calls a model and never lowers a calibrated per-kind threshold to fill a
//! shortlist.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Gift,
    Proof,
    Artifact,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Weights {
    pub cosine: f64,
    pub tags: f64,
    pub recency: f64,
    pub track: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            cosine: 0.5,
            tags: 0.25,
            recency: 0.15,
            track: 0.10,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub id: String,
    pub kind: AssetKind,
    pub embedding: Vec<f32>,
    pub tags: Vec<String>,
    /// Age in fractional months, measured when the matching input snapshot
    /// was formed. The caller owns clock and timezone interpretation.
    pub age_months: f64,
    pub advances: u32,
    pub sends_using_asset: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScoredAsset {
    pub id: String,
    pub kind: AssetKind,
    pub score: f64,
    pub cosine: f64,
    pub tag_overlap: f64,
    pub recency: f64,
    pub track: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchResult {
    pub kind: AssetKind,
    pub threshold: f64,
    pub candidates: Vec<ScoredAsset>,
    pub no_match: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LabeledScore {
    pub score: f64,
    pub relevant: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    pub model_digest: String,
    pub threshold: f64,
    pub positives: usize,
    pub negatives: usize,
    pub true_positives: usize,
    pub false_positives: usize,
    pub false_negatives: usize,
    pub fixture_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchError {
    InvalidWeights,
    InvalidVector,
    InvalidAge,
    InvalidHistory,
    InvalidThreshold,
    InsufficientCalibration,
}

pub fn cosine(left: &[f32], right: &[f32]) -> Result<f64, MatchError> {
    if left.is_empty()
        || left.len() != right.len()
        || left.iter().chain(right).any(|v| !v.is_finite())
    {
        return Err(MatchError::InvalidVector);
    }
    let mut dot = 0.0f64;
    let mut left_sq = 0.0f64;
    let mut right_sq = 0.0f64;
    for (&a, &b) in left.iter().zip(right) {
        let a = f64::from(a);
        let b = f64::from(b);
        dot += a * b;
        left_sq += a * a;
        right_sq += b * b;
    }
    if left_sq == 0.0 || right_sq == 0.0 {
        return Err(MatchError::InvalidVector);
    }
    Ok((dot / (left_sq.sqrt() * right_sq.sqrt())).clamp(-1.0, 1.0))
}

pub fn score(
    asset: &Asset,
    need_embedding: &[f32],
    party_tags: &[String],
    proof_half_life_months: f64,
    weights: Weights,
) -> Result<ScoredAsset, MatchError> {
    if [weights.cosine, weights.tags, weights.recency, weights.track]
        .iter()
        .any(|w| !w.is_finite() || *w < 0.0)
    {
        return Err(MatchError::InvalidWeights);
    }
    if !asset.age_months.is_finite()
        || asset.age_months < 0.0
        || !proof_half_life_months.is_finite()
        || proof_half_life_months <= 0.0
    {
        return Err(MatchError::InvalidAge);
    }
    if asset.advances > asset.sends_using_asset {
        return Err(MatchError::InvalidHistory);
    }
    let cosine = cosine(&asset.embedding, need_embedding)?;
    let asset_tags = normalized_tags(&asset.tags);
    let party_tags = normalized_tags(party_tags);
    let tag_overlap = if asset_tags.is_empty() && party_tags.is_empty() {
        0.0
    } else {
        let intersection = asset_tags.intersection(&party_tags).count();
        let union = asset_tags.union(&party_tags).count();
        intersection as f64 / union as f64
    };
    let recency = if asset.kind == AssetKind::Proof {
        0.5_f64.powf(asset.age_months / proof_half_life_months)
    } else {
        1.0
    };
    let track = (f64::from(asset.advances) + 1.0) / (f64::from(asset.sends_using_asset) + 2.0);
    let score = weights.cosine * cosine
        + weights.tags * tag_overlap
        + weights.recency * recency
        + weights.track * track;
    Ok(ScoredAsset {
        id: asset.id.clone(),
        kind: asset.kind,
        score,
        cosine,
        tag_overlap,
        recency,
        track,
    })
}

pub fn rank(
    assets: &[Asset],
    need_embedding: &[f32],
    party_tags: &[String],
    proof_half_life_months: f64,
    weights: Weights,
    thresholds: &[(AssetKind, f64)],
) -> Result<Vec<MatchResult>, MatchError> {
    let mut results = Vec::new();
    for kind in [AssetKind::Gift, AssetKind::Proof, AssetKind::Artifact] {
        let threshold = thresholds
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, v)| *v)
            .ok_or(MatchError::InvalidThreshold)?;
        if !threshold.is_finite() || !(-1.0..=1.0).contains(&threshold) {
            return Err(MatchError::InvalidThreshold);
        }
        let mut candidates = assets
            .iter()
            .filter(|a| a.kind == kind)
            .map(|a| {
                score(
                    a,
                    need_embedding,
                    party_tags,
                    proof_half_life_months,
                    weights,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        candidates.retain(|candidate| candidate.score >= threshold);
        candidates.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        candidates.truncate(3);
        results.push(MatchResult {
            kind,
            threshold,
            no_match: candidates.is_empty(),
            candidates,
        });
    }
    Ok(results)
}

/// Select a threshold maximizing F1 on the versioned calibration set. Ties
/// choose the highest threshold, the conservative choice with fewer false
/// positives. Fixture digest binds the threshold to the exact evaluated set.
pub fn calibrate(
    model_digest: &str,
    fixture_digest: &str,
    samples: &[LabeledScore],
) -> Result<Calibration, MatchError> {
    let positives = samples.iter().filter(|s| s.relevant).count();
    let negatives = samples.len().saturating_sub(positives);
    if model_digest.trim().is_empty()
        || fixture_digest.trim().is_empty()
        || positives == 0
        || negatives == 0
        || samples
            .iter()
            .any(|s| !s.score.is_finite() || !(-1.0..=1.0).contains(&s.score))
    {
        return Err(MatchError::InsufficientCalibration);
    }
    let mut thresholds = samples.iter().map(|s| s.score).collect::<Vec<_>>();
    thresholds.push(1.0);
    thresholds.sort_by(f64::total_cmp);
    thresholds.dedup_by(|a, b| a.total_cmp(b).is_eq());
    let mut best: Option<(f64, usize, usize, usize)> = None;
    for threshold in thresholds {
        let tp = samples
            .iter()
            .filter(|s| s.relevant && s.score >= threshold)
            .count();
        let fp = samples
            .iter()
            .filter(|s| !s.relevant && s.score >= threshold)
            .count();
        let fn_ = positives - tp;
        let denominator = 2 * tp + fp + fn_;
        let numerator = 2 * tp;
        let replace = match best {
            None => true,
            Some((old_threshold, old_tp, old_fp, old_fn)) => {
                let old_den = 2 * old_tp + old_fp + old_fn;
                numerator * old_den > 2 * old_tp * denominator
                    || (numerator * old_den == 2 * old_tp * denominator
                        && threshold > old_threshold)
            }
        };
        if replace {
            best = Some((threshold, tp, fp, fn_));
        }
    }
    let (threshold, true_positives, false_positives, false_negatives) =
        best.ok_or(MatchError::InsufficientCalibration)?;
    Ok(Calibration {
        model_digest: model_digest.to_owned(),
        threshold,
        positives,
        negatives,
        true_positives,
        false_positives,
        false_negatives,
        fixture_digest: fixture_digest.to_owned(),
    })
}

fn normalized_tags(tags: &[String]) -> BTreeSet<String> {
    tags.iter()
        .map(|tag| tag.trim().to_lowercase())
        .filter(|tag| !tag.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(
        id: &str,
        kind: AssetKind,
        embedding: &[f32],
        tags: &[&str],
        age_months: f64,
        advances: u32,
        sends: u32,
    ) -> Asset {
        Asset {
            id: id.into(),
            kind,
            embedding: embedding.to_vec(),
            tags: tags.iter().map(|s| (*s).into()).collect(),
            age_months,
            advances,
            sends_using_asset: sends,
        }
    }

    #[test]
    fn exact_rank_uses_all_components_and_returns_no_match_without_lowering_threshold() {
        let assets = vec![
            asset(
                "p2",
                AssetKind::Proof,
                &[1.0, 0.0],
                &["Fintech"],
                24.0,
                2,
                3,
            ),
            asset("p1", AssetKind::Proof, &[1.0, 0.0], &["fintech"], 0.0, 2, 3),
        ];
        let ranked = rank(
            &assets,
            &[1.0, 0.0],
            &["FINTECH".into()],
            24.0,
            Weights::default(),
            &[
                (AssetKind::Gift, 0.2),
                (AssetKind::Proof, 0.5),
                (AssetKind::Artifact, 0.2),
            ],
        )
        .unwrap();
        assert_eq!(ranked[1].candidates[0].id, "p1");
        assert!(!ranked[1].no_match);
        assert!(ranked[0].no_match);
        let no_match = rank(
            &assets,
            &[0.0, 1.0],
            &[],
            24.0,
            Weights::default(),
            &[
                (AssetKind::Gift, 0.1),
                (AssetKind::Proof, 0.9),
                (AssetKind::Artifact, 0.1),
            ],
        )
        .unwrap();
        assert!(no_match[1].no_match);
    }

    #[test]
    fn ranking_is_stable_for_ties_and_rejects_invalid_embeddings_or_history() {
        let assets = vec![
            asset("b", AssetKind::Gift, &[1.0], &[], 0.0, 0, 0),
            asset("a", AssetKind::Gift, &[1.0], &[], 0.0, 0, 0),
        ];
        let ranked = rank(
            &assets,
            &[1.0],
            &[],
            24.0,
            Weights::default(),
            &[
                (AssetKind::Gift, 0.0),
                (AssetKind::Proof, 0.0),
                (AssetKind::Artifact, 0.0),
            ],
        )
        .unwrap();
        assert_eq!(
            ranked[0]
                .candidates
                .iter()
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(cosine(&[0.0], &[1.0]), Err(MatchError::InvalidVector));
        assert_eq!(
            score(
                &asset("bad", AssetKind::Gift, &[1.0], &[], 0.0, 2, 1),
                &[1.0],
                &[],
                24.0,
                Weights::default()
            ),
            Err(MatchError::InvalidHistory)
        );
    }

    #[test]
    fn calibration_is_digest_bound_and_maximizes_fixture_f1() {
        let report = calibrate(
            "model-sha",
            "fixture-sha",
            &[
                LabeledScore {
                    score: 0.9,
                    relevant: true,
                },
                LabeledScore {
                    score: 0.8,
                    relevant: true,
                },
                LabeledScore {
                    score: 0.8,
                    relevant: false,
                },
                LabeledScore {
                    score: 0.2,
                    relevant: false,
                },
            ],
        )
        .unwrap();
        assert_eq!(report.model_digest, "model-sha");
        assert_eq!(report.fixture_digest, "fixture-sha");
        assert_eq!(report.threshold, 0.8);
        assert_eq!(
            calibrate(
                "model",
                "fixture",
                &[LabeledScore {
                    score: 0.9,
                    relevant: true
                }]
            ),
            Err(MatchError::InsufficientCalibration)
        );
    }
}
