//! Deterministic weekly Coach assessment and seed validation.

use crate::{WorkspaceError, digest};
use chrono::{Datelike, NaiveDate, Utc, Weekday};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const PROMPT: &str = include_str!("../../roles/coach/prompt.md");
pub(crate) const SCHEMA: &str = include_str!("../../roles/coach/schema.json");
const MAX_ASSESSMENTS: usize = 8192;

#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    AssessmentCheck,
    MissingProof,
    MissingGift,
    MissingArtifact,
    JargonCheck,
    TemplateReview,
    CapacityUpdate,
    MissingIdentity,
    Insight,
    ContextReview,
}

impl Reason {
    pub(crate) fn kind(self) -> &'static str {
        match self {
            Self::AssessmentCheck => "assessment_check",
            Self::MissingProof => "missing_proof",
            Self::MissingGift => "missing_gift",
            Self::MissingArtifact => "missing_artifact",
            Self::JargonCheck => "jargon_check",
            Self::TemplateReview => "template_review",
            Self::CapacityUpdate => "capacity_update",
            Self::MissingIdentity => "missing_identity",
            Self::Insight => "insight",
            Self::ContextReview => "context_review",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Target {
    Party {
        party_id: String,
    },
    Opportunity {
        opportunity_id: String,
        recipient_id: String,
    },
    Segment {
        segment_id: String,
    },
    Term {
        term: String,
    },
    FlaggedTemplates,
    StaleCapacity,
    OwnerIdentity,
    FirstTouches {
        first_touch_count: i64,
    },
}

impl Target {
    pub(crate) fn wire(&self) -> Value {
        match self {
            Self::Party { party_id } => json!({"party_id":party_id}),
            Self::Opportunity {
                opportunity_id,
                recipient_id,
            } => json!({"opportunity_id":opportunity_id,"recipient_id":recipient_id}),
            Self::Segment { segment_id } => json!({"segment_id":segment_id}),
            Self::Term { term } => json!({"term":term}),
            Self::FlaggedTemplates => json!({"status":"flagged"}),
            Self::StaleCapacity => json!({"stale_days":30}),
            Self::OwnerIdentity => json!({"scope":"owner"}),
            Self::FirstTouches { first_touch_count } => {
                json!({"first_touch_count":first_touch_count})
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Seed {
    pub(crate) key: String,
    pub(crate) reason: Reason,
    pub(crate) target: Target,
    pub(crate) impact: i64,
}

impl Seed {
    fn new(reason: Reason, target: Target, impact: i64) -> Result<Self, WorkspaceError> {
        let key = fingerprint(&json!({"reason":reason,"target":target}))?;
        Ok(Self {
            key,
            reason,
            target,
            impact,
        })
    }

    pub(crate) fn answer_schema(&self) -> Value {
        match self.reason {
            Reason::MissingProof => {
                json!({"type":"object","properties":{"proof":{"type":"object"}}})
            }
            Reason::MissingGift => json!({"type":"object","properties":{"gift":{"type":"object"}}}),
            Reason::MissingArtifact => {
                json!({"type":"object","properties":{"artifact":{"type":"object"}}})
            }
            Reason::JargonCheck => {
                json!({"type":"object","required":["fluency"],"properties":{"fluency":{"enum":["learning","fluent"]}}})
            }
            Reason::TemplateReview => {
                json!({"type":"object","required":["decision"],"properties":{"decision":{"enum":["revise","retire"]}}})
            }
            Reason::CapacityUpdate => {
                json!({"type":"object","required":["capacity"],"properties":{"capacity":{"type":"object"}}})
            }
            Reason::MissingIdentity => {
                json!({"type":"object","required":["identity"],"properties":{"identity":{"type":"string","minLength":1}}})
            }
            Reason::AssessmentCheck | Reason::Insight | Reason::ContextReview => {
                json!({"type":"object","required":["note"],"properties":{"note":{"type":"string","minLength":1}}})
            }
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Coverage {
    pub(crate) current: i64,
    pub(crate) missing: i64,
    pub(crate) stale: i64,
    pub(crate) no_match: i64,
}

pub(crate) struct Plan {
    pub(crate) seeds: Vec<Seed>,
    pub(crate) coverage: Coverage,
    pub(crate) dependencies: String,
}

pub(crate) struct ContextPlan {
    pub(crate) seeds: Vec<Seed>,
    pub(crate) coverage: Coverage,
    pub(crate) dependencies: String,
    pub(crate) dependency_json: Value,
    pub(crate) projection: Value,
}

/// Apply the shared deterministic planner to an explicitly selected party or
/// opportunity projection. No model input is read until selectors and all
/// selected references have been resolved under this snapshot.
pub(crate) async fn plan_context(
    conn: &mut SqliteConnection,
    request: &super::inference_context::ContextRequest,
) -> Result<ContextPlan, WorkspaceError> {
    let snapshot = super::inference_context::snapshot(conn, request).await?;
    let scope = &snapshot.projection["scope"];
    let mut coverage = Coverage::default();
    let mut seeds = Vec::new();
    if scope["kind"] == "opportunity" {
        let opportunity_id = scope["id"]
            .as_str()
            .ok_or_else(|| WorkspaceError::Invalid("Invalid Coach scope".into()))?;
        let recipient_id = scope["recipient_id"]
            .as_str()
            .ok_or_else(|| WorkspaceError::Invalid("Invalid Coach scope".into()))?;
        let latest: Option<(String, i64, i64, String, String, String)> = sqlx::query_as(
            "SELECT id,opportunity_revision,recipient_revision,embed_model_digest,calibration_json,shortlist_json FROM match_run
             WHERE opportunity_id=? AND recipient_id=? ORDER BY created_at DESC,id DESC LIMIT 1",
        ).bind(opportunity_id).bind(recipient_id).fetch_optional(&mut *conn).await?;
        let model: Option<String> =
            sqlx::query_scalar("SELECT embed_model_digest FROM agent_provider WHERE singleton=1")
                .fetch_optional(&mut *conn)
                .await?;
        let calibration: Vec<CurrentCalibration> = sqlx::query_as(
            "SELECT c.asset_kind,c.threshold,c.fixture_digest,c.calibration_revision,c.scoring_config_digest
             FROM matching_calibration c WHERE c.trusted=1 AND c.model_digest=?
             AND c.calibration_revision=(SELECT max(c2.calibration_revision) FROM matching_calibration c2
               WHERE c2.trusted=1 AND c2.model_digest=c.model_digest AND c2.asset_kind=c.asset_kind)
             ORDER BY c.asset_kind",
        ).bind(model.as_deref()).fetch_all(&mut *conn).await?;
        match latest {
            None => {
                coverage.missing = 1;
                seeds.push(Seed::new(
                    Reason::AssessmentCheck,
                    Target::Opportunity {
                        opportunity_id: opportunity_id.to_owned(),
                        recipient_id: recipient_id.to_owned(),
                    },
                    2,
                )?);
            }
            Some((
                match_id,
                opportunity_revision,
                recipient_revision,
                model_digest,
                calibration_json,
                shortlist_json,
            )) => {
                let current = opportunity_revision
                    == scope["revision"].as_i64().unwrap_or_default()
                    && recipient_revision
                        == scope["recipient_revision"].as_i64().unwrap_or_default();
                if current {
                    let row = Assessment {
                        segment_id: String::new(),
                        segment_revision: 0,
                        opportunity_id: opportunity_id.to_owned(),
                        opportunity_revision,
                        recipient_id: recipient_id.to_owned(),
                        recipient_revision,
                        match_id: Some(match_id.clone()),
                        match_opportunity_revision: Some(opportunity_revision),
                        match_recipient_revision: Some(recipient_revision),
                        model_digest: Some(model_digest.clone()),
                        calibration_json: Some(calibration_json),
                        shortlist_json: Some(shortlist_json),
                    };
                    if let Some(gaps) = assessment_result(&row, model.as_deref(), &calibration) {
                        coverage.current = 1;
                        coverage.no_match = i64::from(!gaps.is_empty());
                        for kind in gaps {
                            let reason = match kind.as_str() {
                                "proof" => Reason::MissingProof,
                                "gift" => Reason::MissingGift,
                                "artifact" => Reason::MissingArtifact,
                                _ => continue,
                            };
                            seeds.push(Seed::new(
                                reason,
                                Target::Opportunity {
                                    opportunity_id: opportunity_id.to_owned(),
                                    recipient_id: recipient_id.to_owned(),
                                },
                                2,
                            )?);
                        }
                    } else {
                        coverage.stale = 1;
                        seeds.push(Seed::new(
                            Reason::AssessmentCheck,
                            Target::Opportunity {
                                opportunity_id: opportunity_id.to_owned(),
                                recipient_id: recipient_id.to_owned(),
                            },
                            2,
                        )?);
                    }
                } else {
                    coverage.stale = 1;
                    seeds.push(Seed::new(
                        Reason::AssessmentCheck,
                        Target::Opportunity {
                            opportunity_id: opportunity_id.to_owned(),
                            recipient_id: recipient_id.to_owned(),
                        },
                        2,
                    )?);
                }
            }
        }
    } else {
        // A party selection has no implicit match/assessment. Its contextual
        // question is explicitly about the selected research context.
        seeds.push(Seed::new(
            Reason::ContextReview,
            Target::Party {
                party_id: scope["id"].as_str().unwrap_or_default().to_owned(),
            },
            1,
        )?);
    }
    let dependency_json = if scope["kind"] == "opportunity" {
        let opportunity_id = scope["id"].as_str().unwrap_or_default();
        let recipient_id = scope["recipient_id"].as_str().unwrap_or_default();
        let assessment: Option<(String,i64,i64,String)> = sqlx::query_as("SELECT id,opportunity_revision,recipient_revision,embed_model_digest FROM match_run WHERE opportunity_id=? AND recipient_id=? ORDER BY created_at DESC,id DESC LIMIT 1")
            .bind(opportunity_id).bind(recipient_id).fetch_optional(&mut *conn).await?;
        let mut refs = snapshot
            .dependency_json
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some((id, opportunity_revision, recipient_revision, model_digest)) = assessment {
            refs.push(json!({"kind":"assessment","id":id,"revision":format!("{opportunity_revision}:{recipient_revision}:{model_digest}")}));
        } else {
            refs.push(json!({"kind":"assessment_collection","id":format!("{opportunity_id}:{recipient_id}"),"revision":"missing"}));
        }
        Value::Array(refs)
    } else {
        snapshot.dependency_json.clone()
    };
    let dependencies = fingerprint(&dependency_json)?;
    for seed in &mut seeds {
        // A material change in the selected assessment or evidence context
        // creates a new semantic question key; an unchanged dismissal or
        // deferral remains suppressed for the same dependency vector.
        seed.key = fingerprint(&json!({
            "selector":scope,"reason":seed.reason,"target":seed.target,
            "dependencies":dependencies
        }))?;
    }
    Ok(ContextPlan {
        seeds,
        coverage,
        dependencies,
        dependency_json,
        projection: snapshot.projection,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Week {
    pub(crate) period: String,
    pub(crate) through: String,
    pub(crate) as_of: String,
    pub(crate) current: bool,
}

pub(crate) enum PeriodError {
    Invalid,
    Database(sqlx::Error),
}

pub(crate) async fn resolve_week(
    conn: &mut SqliteConnection,
    period: Option<&str>,
    as_of: chrono::DateTime<Utc>,
) -> Result<Week, PeriodError> {
    let zone: String = sqlx::query_scalar("SELECT timezone FROM owner_profile WHERE id=1")
        .fetch_one(&mut *conn)
        .await
        .map_err(PeriodError::Database)?;
    let tz: chrono_tz::Tz = zone.parse().map_err(|_| PeriodError::Invalid)?;
    let current = as_of.with_timezone(&tz).date_naive().iso_week();
    let (year, week) = if let Some(period) = period {
        if period.len() != 8 || !period.is_ascii() || &period[4..6] != "-W" {
            return Err(PeriodError::Invalid);
        }
        (
            period[..4].parse().map_err(|_| PeriodError::Invalid)?,
            period[6..].parse().map_err(|_| PeriodError::Invalid)?,
        )
    } else {
        (current.year(), current.week())
    };
    let monday =
        NaiveDate::from_isoywd_opt(year, week, Weekday::Mon).ok_or(PeriodError::Invalid)?;
    let start = monday.and_hms_opt(0, 0, 0).ok_or(PeriodError::Invalid)?;
    let next = start
        .checked_add_signed(chrono::Duration::days(7))
        .ok_or(PeriodError::Invalid)?;
    let current_start = NaiveDate::from_isoywd_opt(current.year(), current.week(), Weekday::Mon)
        .ok_or(PeriodError::Invalid)?;
    if monday > current_start
        || year < 1970
        || period.is_some_and(|p| p != format!("{year:04}-W{week:02}"))
    {
        return Err(PeriodError::Invalid);
    }
    let earliest: Option<String> = sqlx::query_scalar("SELECT min(occurred_at) FROM event")
        .fetch_one(&mut *conn)
        .await
        .map_err(PeriodError::Database)?;
    let through = if monday == current_start {
        as_of
    } else {
        next.and_local_timezone(tz)
            .earliest()
            .ok_or(PeriodError::Invalid)?
            .with_timezone(&Utc)
    };
    let through = through.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    if monday != current_start && earliest.as_ref().is_none_or(|at| at >= &through) {
        return Err(PeriodError::Invalid);
    }
    Ok(Week {
        period: format!("{year:04}-W{week:02}"),
        through,
        current: monday == current_start,
        as_of: as_of.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    })
}

#[derive(Serialize, sqlx::FromRow)]
pub(crate) struct Assessment {
    pub(crate) segment_id: String,
    pub(crate) segment_revision: i64,
    pub(crate) opportunity_id: String,
    pub(crate) opportunity_revision: i64,
    pub(crate) recipient_id: String,
    pub(crate) recipient_revision: i64,
    pub(crate) match_id: Option<String>,
    pub(crate) match_opportunity_revision: Option<i64>,
    pub(crate) match_recipient_revision: Option<i64>,
    pub(crate) model_digest: Option<String>,
    pub(crate) calibration_json: Option<String>,
    pub(crate) shortlist_json: Option<String>,
}

#[derive(Deserialize)]
struct Shortlist {
    results: Vec<MatchOutcome>,
}

#[derive(Deserialize)]
struct MatchOutcome {
    kind: String,
    no_match: bool,
}

#[derive(Deserialize)]
struct Calibration {
    threshold: f64,
    fixture_digest: String,
    model_digest: String,
}

pub(crate) type CurrentCalibration = (String, f64, String, i64, String);

pub(crate) fn assessment_result(
    row: &Assessment,
    model: Option<&str>,
    current: &[CurrentCalibration],
) -> Option<BTreeSet<String>> {
    if row.match_opportunity_revision != Some(row.opportunity_revision)
        || row.match_recipient_revision != Some(row.recipient_revision)
        || model.is_none()
        || row.model_digest.as_deref() != model
    {
        return None;
    }
    let stored: BTreeMap<String, Calibration> =
        serde_json::from_str(row.calibration_json.as_deref()?).ok()?;
    if stored.len() != 3
        || current.len() != 3
        || current.iter().any(|(kind, threshold, fixture, _, _)| {
            stored.get(kind).is_none_or(|item| {
                item.threshold != *threshold
                    || item.fixture_digest != *fixture
                    || Some(item.model_digest.as_str()) != model
            })
        })
    {
        return None;
    }
    let shortlist: Shortlist = serde_json::from_str(row.shortlist_json.as_deref()?).ok()?;
    let mut kinds = BTreeSet::new();
    let mut gaps = BTreeSet::new();
    for outcome in shortlist.results {
        if !stored.contains_key(&outcome.kind) || !kinds.insert(outcome.kind.clone()) {
            return None;
        }
        if outcome.no_match {
            gaps.insert(outcome.kind);
        }
    }
    (kinds.len() == 3).then_some(gaps)
}

pub(crate) async fn plan(conn: &mut SqliteConnection, week: &Week) -> Result<Plan, WorkspaceError> {
    let owner: (String, String, String) =
        sqlx::query_as("SELECT timezone,pack_id,pack_version FROM owner_profile WHERE id=1")
            .fetch_one(&mut *conn)
            .await?;
    let model: Option<String> =
        sqlx::query_scalar("SELECT embed_model_digest FROM agent_provider WHERE singleton=1")
            .fetch_one(&mut *conn)
            .await?;
    let through = if week.current {
        crate::now_utc()
    } else {
        week.through.clone()
    };
    let rows: Vec<Assessment> = sqlx::query_as(
        "SELECT s.id AS segment_id,s.revision AS segment_revision,
          o.id AS opportunity_id,o.revision AS opportunity_revision,
          e.id AS recipient_id,e.revision AS recipient_revision,
          m.id AS match_id,m.opportunity_revision AS match_opportunity_revision,
          m.recipient_revision AS match_recipient_revision,m.embed_model_digest AS model_digest,
          m.calibration_json,m.shortlist_json
         FROM segment_party sp JOIN segment s ON s.id=sp.segment_id
         JOIN opportunity_party op ON op.party_id=sp.party_id
         JOIN opportunity o ON o.id=op.opportunity_id AND o.state='open' AND o.created_at <= ?
         JOIN entity e ON e.id=op.party_id
         LEFT JOIN match_run m ON m.id=(SELECT m2.id FROM match_run m2
           WHERE m2.opportunity_id=o.id AND m2.recipient_id=e.id AND m2.created_at <= ?
           ORDER BY m2.created_at DESC,m2.id DESC LIMIT 1)
         ORDER BY s.id,o.id,e.id LIMIT 8193",
    )
    .bind(&through)
    .bind(&through)
    .fetch_all(&mut *conn)
    .await?;
    if rows.len() > MAX_ASSESSMENTS {
        return Err(WorkspaceError::Invalid("CONTEXT_BUDGET".into()));
    }
    let calibration: Vec<CurrentCalibration> = sqlx::query_as(
        "SELECT c.asset_kind,c.threshold,c.fixture_digest,c.calibration_revision,c.scoring_config_digest
         FROM matching_calibration c WHERE c.trusted=1 AND c.model_digest=?
         AND c.calibration_revision=(SELECT max(c2.calibration_revision) FROM matching_calibration c2
           WHERE c2.trusted=1 AND c2.model_digest=c.model_digest AND c2.asset_kind=c.asset_kind)
         ORDER BY c.asset_kind",
    )
    .bind(model.as_deref())
    .fetch_all(&mut *conn)
    .await?;
    let mut seeds = Vec::new();
    let mut counts = BTreeMap::<(String, Reason), i64>::new();
    let mut unique = BTreeSet::new();
    let mut coverage = Coverage::default();
    for row in &rows {
        let first = unique.insert((&row.opportunity_id, &row.recipient_id));
        if let Some(gaps) = assessment_result(row, model.as_deref(), &calibration) {
            coverage.current += i64::from(first);
            coverage.no_match += i64::from(first && !gaps.is_empty());
            for kind in gaps {
                let reason = match kind.as_str() {
                    "proof" => Reason::MissingProof,
                    "gift" => Reason::MissingGift,
                    "artifact" => Reason::MissingArtifact,
                    _ => continue,
                };
                *counts.entry((row.segment_id.clone(), reason)).or_default() += 1;
            }
        } else {
            if first {
                if row.match_id.is_some() {
                    coverage.stale += 1;
                } else {
                    coverage.missing += 1;
                }
            }
            *counts
                .entry((row.segment_id.clone(), Reason::AssessmentCheck))
                .or_default() += 1;
        }
    }
    for ((segment_id, reason), impact) in counts {
        seeds.push(Seed::new(reason, Target::Segment { segment_id }, impact)?);
    }
    let terms: Vec<String> = sqlx::query_scalar(
        "SELECT term FROM fluency WHERE status='unknown' ORDER BY term LIMIT 8193",
    )
    .fetch_all(&mut *conn)
    .await?;
    if terms.len() > MAX_ASSESSMENTS {
        return Err(WorkspaceError::Invalid("CONTEXT_BUDGET".into()));
    }
    for term in &terms {
        seeds.push(Seed::new(
            Reason::JargonCheck,
            Target::Term { term: term.clone() },
            1,
        )?);
    }
    let (flagged, capacity, identities, first_touches): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM template_version WHERE status='flagged'),
          (SELECT count(*) FROM capacity_ledger WHERE julianday(updated_at) < julianday(?)-30),
          (SELECT count(*) FROM owner_identity_tag),
          (SELECT count(*) FROM send WHERE touch_no=1 AND sent_at <= ?)",
    )
    .bind(&through)
    .bind(&through)
    .fetch_one(&mut *conn)
    .await?;
    if flagged > 0 {
        seeds.push(Seed::new(
            Reason::TemplateReview,
            Target::FlaggedTemplates,
            flagged,
        )?);
    }
    if capacity > 0 {
        seeds.push(Seed::new(Reason::CapacityUpdate, Target::StaleCapacity, 1)?);
    }
    if identities == 0 {
        seeds.push(Seed::new(
            Reason::MissingIdentity,
            Target::OwnerIdentity,
            1,
        )?);
    }
    if (1..20).contains(&first_touches) {
        seeds.push(Seed::new(
            Reason::Insight,
            Target::FirstTouches {
                first_touch_count: first_touches,
            },
            1,
        )?);
    }
    seeds.sort_by(|a, b| {
        (a.reason > Reason::MissingArtifact)
            .cmp(&(b.reason > Reason::MissingArtifact))
            .then_with(|| b.impact.cmp(&a.impact))
            .then_with(|| a.reason.cmp(&b.reason))
            .then_with(|| a.key.cmp(&b.key))
    });
    let dependencies = fingerprint(&json!({
        "owner":owner, "model":model,"calibration":calibration,"assessments":rows,
        "terms":terms, "flagged":flagged, "capacity":capacity,
        "identities":identities, "first_touches":first_touches
    }))?;
    Ok(Plan {
        seeds,
        coverage,
        dependencies,
    })
}

pub(crate) fn fingerprint(value: &Value) -> Result<String, WorkspaceError> {
    parley_domain::jcs::canonicalize_value(value)
        .map(|canonical| digest(canonical.as_bytes()))
        .map_err(|_| WorkspaceError::Invalid("Noncanonical Coach data".into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    questions: Vec<Question>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Question {
    question_key: String,
    text: String,
    #[serde(default)]
    resource_ref: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedQuestion {
    pub(crate) text: String,
    pub(crate) resource_ref: Option<String>,
}

pub(crate) fn validate_output(
    candidate: Value,
    seeds: &[Seed],
    allowed_resources: &[String],
) -> Option<Vec<ValidatedQuestion>> {
    let output: Output = serde_json::from_value(candidate).ok()?;
    let mut questions = BTreeMap::new();
    if output.questions.len() != seeds.len() {
        return None;
    }
    for question in output.questions {
        let text = question.text.trim();
        if !(4..=1200).contains(&text.chars().count())
            || !text.ends_with('?')
            || text.chars().any(|c| {
                c.is_control() || c.is_numeric() || matches!(c, '<' | '>' | '"' | '“' | '”')
            })
            || text.contains("://")
            || question.resource_ref.as_ref().is_some_and(|reference| {
                !allowed_resources.iter().any(|allowed| allowed == reference)
            })
            || questions
                .insert(
                    question.question_key,
                    ValidatedQuestion {
                        text: text.to_owned(),
                        resource_ref: question.resource_ref,
                    },
                )
                .is_some()
        {
            return None;
        }
    }
    seeds
        .iter()
        .map(|seed| questions.remove(&seed.key))
        .collect()
}
