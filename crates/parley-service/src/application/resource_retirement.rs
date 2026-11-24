//! Typed resource retirement operation and read-only planner
//! (UCR-001 U08, §5.3 "Retirement plan").
//!
//! The planner is the single source of the destructive impact: it binds
//! the card revision, content revision, placement state and the ordered
//! distinct material-version dependency vector into a canonical fingerprint.
//! The registry approval flow stores that fingerprint; execution recomputes
//! it under reservation and refuses to consume the grant if the world
//! changed. The same operation backs the owner REST retirement route and
//! the registry command, so both share one fact/event/revision set.

use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};

use crate::{new_id, now_utc};

/// Registry intents reject more than 256 affected material versions
/// (§5.3); past that the owner uses the ordinary owner workflow.
pub(crate) const MAX_IMPACTED_VERSIONS: usize = 256;
/// Up to ten affected versions appear in a labelled preview sample.
pub(crate) const PREVIEW_SAMPLE_LIMIT: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RetirementInput {
    pub id: String,
    pub expected_card_revision: i64,
    pub expected_content_revision: i64,
    /// Digest of the bounded operation key, stored on the emitted event
    /// instead of the raw key (Q1).
    pub operation_key_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ImpactedVersion {
    pub material_id: String,
    pub version_no: i64,
    pub dependency_revision: String,
}

/// The complete observational plan for one retirement. The full dependency
/// vector (not just the sample) contributes to the fingerprint.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RetirementPlan {
    pub resource_id: String,
    pub title: String,
    pub readiness: String,
    pub card_revision: i64,
    pub content_revision: i64,
    pub placement_removed: bool,
    pub dependencies: Vec<ImpactedVersion>,
}

impl RetirementPlan {
    pub fn impacted_versions_count(&self) -> usize {
        self.dependencies.len()
    }

    pub fn sample(&self) -> Vec<ImpactedVersion> {
        self.dependencies[..self.dependencies.len().min(PREVIEW_SAMPLE_LIMIT)].to_vec()
    }

    /// Deterministic canonical fingerprint over every fact the execution
    /// depends on: card/content revisions, placement state, and the ordered
    /// material-version dependency vector with the recorded dependency
    /// revisions (Q5 / §5.3).
    pub fn fingerprint(&self) -> String {
        let dependencies: Vec<serde_json::Value> = self
            .dependencies
            .iter()
            .map(|dependency| {
                serde_json::json!({
                    "material_id": dependency.material_id,
                    "version_no": dependency.version_no,
                    "dependency_revision": dependency.dependency_revision,
                })
            })
            .collect();
        let value = serde_json::json!({
            "card_revision": self.card_revision,
            "content_revision": self.content_revision,
            "placement_removed": self.placement_removed,
            "dependencies": dependencies,
        });
        let canonical = parley_domain::jcs::canonicalize_value(&value).unwrap_or_default();
        crate::digest(canonical.as_bytes())
    }

    /// The preview result shown to the owner: title, placement removal,
    /// impacted version count and the capped sample (§5.3).
    pub fn preview(&self) -> serde_json::Value {
        serde_json::json!({
            "resource_id": self.resource_id,
            "title": self.title,
            "readiness": self.readiness,
            "card_revision": self.card_revision,
            "content_revision": self.content_revision,
            "placement_removed": self.placement_removed,
            "impacted_versions_count": self.impacted_versions_count(),
            "impacted_versions_sample": self.sample(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RetirementOutcome {
    pub resource_id: String,
    pub title: String,
    pub readiness: String,
    pub card_revision: i64,
    pub content_revision: i64,
    pub placement_removed: bool,
    pub event_id: String,
    pub event_cursor: i64,
    pub impacted_versions_count: usize,
    pub impacted_versions: Vec<ImpactedVersion>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RetirementError {
    NotFound,
    StaleRevision,
    TooManyVersions,
    Database,
}

type Row = (String, String, i64, i64);
type PlacementRow = (String, i64, i64);
type DependencyRow = (String, i64, String);

/// Read-only planner: observe the current impact of retiring `input.id`
/// without mutating anything. Used for previews, dry runs, the pending
/// approval record and the reservation-time fingerprint recheck.
pub(crate) async fn plan_retirement(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<RetirementPlan, RetirementError> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT name,readiness,revision,resource_content_revision FROM entity \
         WHERE id=? AND subtype<>'' AND readiness <> 'retired' AND retired_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| RetirementError::Database)?;
    let Some((title, readiness, card_revision, content_revision)) = row else {
        return Err(RetirementError::NotFound);
    };
    let placement: Option<PlacementRow> = sqlx::query_as(
        "SELECT column_id,position,revision FROM resource_placement WHERE resource_id=?",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| RetirementError::Database)?;
    let dependencies: Vec<DependencyRow> = sqlx::query_as(
        "SELECT DISTINCT material_id,version_no,source_revision FROM version_dependency \
         WHERE kind='resource' AND source_id=? ORDER BY material_id,version_no LIMIT ?",
    )
    .bind(id)
    .bind(MAX_IMPACTED_VERSIONS as i64 + 1)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| RetirementError::Database)?;
    if dependencies.len() > MAX_IMPACTED_VERSIONS {
        return Err(RetirementError::TooManyVersions);
    }
    Ok(RetirementPlan {
        resource_id: id.to_owned(),
        title,
        readiness,
        card_revision,
        content_revision,
        placement_removed: placement.is_some(),
        dependencies: dependencies
            .into_iter()
            .map(
                |(material_id, version_no, dependency_revision)| ImpactedVersion {
                    material_id,
                    version_no,
                    dependency_revision,
                },
            )
            .collect(),
    })
}

/// Remove a placed resource from the board: the placement row, its column
/// revision and the `resource.removed_from_board` event (shared with the
/// board's placement path).
async fn remove_placement(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<(), RetirementError> {
    let current: Option<(String, i64, i64)> = sqlx::query_as(
        "SELECT column_id,position,revision FROM resource_placement WHERE resource_id=?",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| RetirementError::Database)?;
    let Some((source_column, position, placement_rev)) = current else {
        return Ok(());
    };
    sqlx::query("DELETE FROM resource_placement WHERE resource_id=?")
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(|_| RetirementError::Database)?;
    sqlx::query("UPDATE resource_column SET revision=revision+1 WHERE id=?")
        .bind(&source_column)
        .execute(&mut **tx)
        .await
        .map_err(|_| RetirementError::Database)?;
    sqlx::query(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) \
         VALUES(?,?, 'resource',?,?,'resource.removed_from_board',?)",
    )
    .bind(new_id(&format!("removed:{id}")))
    .bind(now_utc())
    .bind(id)
    .bind(placement_rev + 1)
    .bind(serde_json::json!({"column_id": source_column, "position": position}).to_string())
    .execute(&mut **tx)
    .await
    .map_err(|_| RetirementError::Database)?;
    Ok(())
}

/// Execute the retirement under the caller's reservation. The plan is
/// recomputed inside the transaction (the caller's savepoint), so the
/// guarded update, placement removal, retirement event and release-recheck
/// events all commit or roll back together with the approval consumption.
pub(crate) async fn retire(
    tx: &mut Transaction<'_, Sqlite>,
    input: &RetirementInput,
) -> Result<RetirementOutcome, RetirementError> {
    let plan = plan_retirement(tx, &input.id).await?;
    if plan.card_revision != input.expected_card_revision
        || plan.content_revision != input.expected_content_revision
    {
        return Err(RetirementError::StaleRevision);
    }
    let next_card = plan.card_revision + 1;
    let next_content = plan.content_revision + 1;
    let now = now_utc();
    let updated = sqlx::query(
        "UPDATE entity SET readiness='retired',revision=?,resource_content_revision=?,updated_at=? \
         WHERE id=? AND revision=? AND resource_content_revision=?",
    )
    .bind(next_card)
    .bind(next_content)
    .bind(&now)
    .bind(&input.id)
    .bind(plan.card_revision)
    .bind(plan.content_revision)
    .execute(&mut **tx)
    .await
    .map_err(|_| RetirementError::Database)?;
    if updated.rows_affected() != 1 {
        return Err(RetirementError::StaleRevision);
    }
    remove_placement(tx, &input.id).await?;
    let event_id = new_id(&input.id);
    let cursor: i64 = sqlx::query_scalar(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) \
         VALUES(?,?, 'resource',?,?,'resource.retired',?) RETURNING cursor",
    )
    .bind(&event_id)
    .bind(&now)
    .bind(&input.id)
    .bind(next_card)
    .bind(
        serde_json::json!({
            "revision": next_card,
            "content_revision": next_content,
            "operation_key_digest": input.operation_key_digest,
        })
        .to_string(),
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| RetirementError::Database)?;
    let affected: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT DISTINCT d.material_id,d.version_no,d.source_revision FROM version_dependency d \
         WHERE d.kind='resource' AND d.source_id=? AND d.source_revision <> ? ORDER BY d.material_id,d.version_no",
    )
    .bind(&input.id)
    .bind(next_content.to_string())
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| RetirementError::Database)?;
    for (material_id, version_no, _) in &affected {
        sqlx::query(
            "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) \
             VALUES(?,?, 'material',?,?,'material.release_recheck_required',?)",
        )
        .bind(new_id(&format!("recheck:{material_id}:{version_no}")))
        .bind(now_utc())
        .bind(material_id)
        .bind(version_no)
        .bind(
            serde_json::json!({"resource_id": input.id, "version_no": version_no}).to_string(),
        )
        .execute(&mut **tx)
        .await
        .map_err(|_| RetirementError::Database)?;
    }
    Ok(RetirementOutcome {
        resource_id: input.id.clone(),
        title: plan.title,
        readiness: "retired".into(),
        card_revision: next_card,
        content_revision: next_content,
        placement_removed: plan.placement_removed,
        event_id,
        event_cursor: cursor,
        impacted_versions_count: affected.len(),
        impacted_versions: affected
            .into_iter()
            .map(
                |(material_id, version_no, dependency_revision)| ImpactedVersion {
                    material_id,
                    version_no,
                    dependency_revision,
                },
            )
            .collect(),
    })
}
