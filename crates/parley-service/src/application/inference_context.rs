//! Bounded, owner-selected projections shared by contextual inference jobs.

use crate::{WorkspaceError, application::coach::fingerprint};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ScopeSelection {
    Party {
        id: String,
        revision: i64,
    },
    Opportunity {
        id: String,
        revision: i64,
        recipient_id: String,
        recipient_revision: i64,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvidenceSelection {
    pub(crate) claim_id: String,
    pub(crate) revision: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextRequest {
    pub(crate) scope: ScopeSelection,
    #[serde(default)]
    pub(crate) segment_id: Option<String>,
    #[serde(default)]
    pub(crate) evidence: Vec<EvidenceSelection>,
    #[serde(default)]
    pub(crate) resource_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ContextSnapshot {
    pub(crate) dependency_json: Value,
    pub(crate) projection: Value,
}

/// Re-check that a receipt's selected records are still owner-visible without
/// applying freshness revisions. Idempotent replay recovers the original
/// durable result before freshness is evaluated, but never after a selector
/// has been erased, retired, or moved out of its authorized relationship.
pub(crate) async fn authorize(
    conn: &mut SqliteConnection,
    request: &ContextRequest,
) -> Result<(), WorkspaceError> {
    let (party_id, opportunity_id) = match &request.scope {
        ScopeSelection::Party { id, .. } => {
            let exists: Option<String> = sqlx::query_scalar(
                "SELECT id FROM entity WHERE id=? AND kind IN ('person','org') AND retired_at IS NULL",
            )
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
            (exists.ok_or(WorkspaceError::NotFound)?, None)
        }
        ScopeSelection::Opportunity {
            id, recipient_id, ..
        } => {
            let exists: Option<String> = sqlx::query_scalar(
                "SELECT e.id FROM opportunity o JOIN opportunity_party op ON op.opportunity_id=o.id
                 JOIN entity e ON e.id=op.party_id WHERE o.id=? AND e.id=? AND o.state='open'
                   AND e.kind IN ('person','org') AND e.retired_at IS NULL",
            )
            .bind(id)
            .bind(recipient_id)
            .fetch_optional(&mut *conn)
            .await?;
            (exists.ok_or(WorkspaceError::NotFound)?, Some(id.as_str()))
        }
    };
    if let Some(segment_id) = request.segment_id.as_deref() {
        let exists: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM segment_party WHERE segment_id=? AND party_id=?")
                .bind(segment_id)
                .bind(&party_id)
                .fetch_optional(&mut *conn)
                .await?;
        if exists.is_none() {
            return Err(WorkspaceError::NotFound);
        }
    }
    for selected in &request.evidence {
        let exists: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM claim c JOIN claim_revision cr ON cr.claim_id=c.id
             WHERE c.id=? AND c.subject_id=? AND cr.revision=? AND cr.knowledge_kind<>'hypothesis'
               AND (cr.tier=3 OR cr.confirmed_by_owner=1)",
        )
        .bind(&selected.claim_id)
        .bind(&party_id)
        .bind(selected.revision)
        .fetch_optional(&mut *conn)
        .await?;
        if exists.is_none() {
            return Err(WorkspaceError::NotFound);
        }
    }
    for resource_id in &request.resource_ids {
        let eligible: Option<String> = sqlx::query_scalar(
            "SELECT id FROM entity WHERE id=? AND kind='resource' AND readiness='ready' AND retired_at IS NULL",
        )
        .bind(resource_id)
        .fetch_optional(&mut *conn)
        .await?;
        if eligible.is_none() || opportunity_id.is_none() {
            return Err(WorkspaceError::NotFound);
        }
    }
    Ok(())
}

pub(crate) async fn snapshot(
    conn: &mut SqliteConnection,
    request: &ContextRequest,
) -> Result<ContextSnapshot, WorkspaceError> {
    if request.evidence.len() > 8 || request.resource_ids.len() > 8 {
        return Err(WorkspaceError::Invalid("CONTEXT_BUDGET".into()));
    }
    if request
        .evidence
        .iter()
        .map(|item| &item.claim_id)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != request.evidence.len()
        || request
            .resource_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != request.resource_ids.len()
    {
        return Err(WorkspaceError::Invalid(
            "DUPLICATE_CONTEXT_REFERENCE".into(),
        ));
    }
    let (party_id, party_revision, scope_projection, opportunity_ref) = match &request.scope {
        ScopeSelection::Party { id, revision } => {
            let current: Option<(String, i64, String)> = sqlx::query_as(
                "SELECT id,revision,kind FROM entity WHERE id=? AND kind IN ('person','org') AND retired_at IS NULL",
            ).bind(id).fetch_optional(&mut *conn).await?;
            let Some((found, current_revision, _kind)) = current.filter(|row| row.1 == *revision)
            else {
                return Err(WorkspaceError::StaleRevision);
            };
            (
                found.clone(),
                current_revision,
                json!({"kind":"party","id":found,"revision":current_revision}),
                None,
            )
        }
        ScopeSelection::Opportunity {
            id,
            revision,
            recipient_id,
            recipient_revision,
        } => {
            let current: Option<(i64, i64, String)> = sqlx::query_as(
                "SELECT o.revision,e.revision,o.state FROM opportunity o
                 JOIN opportunity_party op ON op.opportunity_id=o.id AND op.party_id=?
                 JOIN entity e ON e.id=op.party_id WHERE o.id=? AND e.kind IN ('person','org') AND e.retired_at IS NULL",
            ).bind(recipient_id).bind(id).fetch_optional(&mut *conn).await?;
            let Some((op_revision, recipient_rev, state)) = current else {
                return Err(WorkspaceError::NotFound);
            };
            if op_revision != *revision || recipient_rev != *recipient_revision || state != "open" {
                return Err(WorkspaceError::StaleRevision);
            }
            (
                recipient_id.clone(),
                recipient_rev,
                json!({"kind":"opportunity","id":id,"revision":op_revision,"recipient_id":recipient_id,"recipient_revision":recipient_rev}),
                Some((id.clone(), op_revision)),
            )
        }
    };
    let segment = if let Some(segment_id) = request.segment_id.as_deref() {
        let revision: Option<i64> = sqlx::query_scalar(
            "SELECT s.revision FROM segment s JOIN segment_party sp ON sp.segment_id=s.id
             WHERE s.id=? AND sp.party_id=?",
        )
        .bind(segment_id)
        .bind(&party_id)
        .fetch_optional(&mut *conn)
        .await?;
        Some((
            segment_id.to_owned(),
            revision.ok_or(WorkspaceError::NotFound)?,
        ))
    } else {
        None
    };

    let mut evidence = Vec::with_capacity(request.evidence.len());
    let mut deps = vec![json!({"kind":"party","id":party_id,"revision":party_revision})];
    if let Some((opportunity_id, revision)) = &opportunity_ref {
        deps.push(json!({"kind":"opportunity","id":opportunity_id,"revision":revision}));
    }
    if let Some((segment_id, revision)) = &segment {
        deps.push(
            json!({"kind":"segment","id":segment_id,"revision":revision,"party_id":party_id}),
        );
    }
    for selected in &request.evidence {
        let row: Option<(i64, String, i64, String)> = sqlx::query_as(
            "SELECT cr.revision,cr.wording,cr.tier,cr.knowledge_kind FROM claim c
             JOIN claim_revision cr ON cr.claim_id=c.id AND cr.revision=c.current_revision
             WHERE c.id=? AND c.subject_id=? AND cr.revision=? AND cr.knowledge_kind<>'hypothesis'
               AND (cr.tier=3 OR cr.confirmed_by_owner=1)",
        )
        .bind(&selected.claim_id)
        .bind(&party_id)
        .bind(selected.revision)
        .fetch_optional(&mut *conn)
        .await?;
        let Some((revision, wording, tier, knowledge_kind)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        let excerpt = wording.trim();
        if excerpt.is_empty() || excerpt.len() > 1024 {
            return Err(WorkspaceError::Invalid("EVIDENCE_SPAN_INVALID".into()));
        }
        evidence.push(json!({"claim_id":selected.claim_id,"revision":revision,"tier":tier,"knowledge_kind":knowledge_kind,"excerpt":excerpt}));
        deps.push(json!({"kind":"claim","id":selected.claim_id,"revision":revision}));
    }
    let mut resources = Vec::with_capacity(request.resource_ids.len());
    for resource_id in &request.resource_ids {
        let row: Option<(String, String, String, i64, i64)> = sqlx::query_as(
            "SELECT subtype,name,body_json,resource_content_revision,revision FROM entity
             WHERE id=? AND kind='resource' AND readiness='ready' AND retired_at IS NULL",
        )
        .bind(resource_id)
        .fetch_optional(&mut *conn)
        .await?;
        let Some((kind, title, body, content_revision, card_revision)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        let body: Value = serde_json::from_str(&body)
            .map_err(|_| WorkspaceError::Invalid("RESOURCE_INVALID".into()))?;
        resources.push(json!({"id":resource_id,"kind":kind,"title":title,"content_revision":content_revision,"body":body}));
        deps.push(json!({"kind":"resource","id":resource_id,"revision":content_revision,"card_revision":card_revision}));
    }
    if !request.resource_ids.is_empty() {
        let opportunity_id = match &request.scope {
            ScopeSelection::Opportunity { id, .. } => Some(id.as_str()),
            ScopeSelection::Party { .. } => None,
        };
        let Some(opportunity_id) = opportunity_id else {
            return Err(WorkspaceError::Invalid(
                "RESOURCE_SHORTLIST_REQUIRED".into(),
            ));
        };
        let shortlist: Option<(String,String)> = sqlx::query_as(
            "SELECT m.shortlist_json,m.id FROM match_run m JOIN opportunity o ON o.id=m.opportunity_id
             JOIN entity e ON e.id=m.recipient_id
             WHERE m.opportunity_id=? AND m.recipient_id=? AND m.opportunity_revision=o.revision
               AND m.recipient_revision=e.revision AND e.id=?
             ORDER BY m.created_at DESC,m.id DESC LIMIT 1",
        ).bind(opportunity_id).bind(&party_id).bind(&party_id).fetch_optional(&mut *conn).await?;
        let Some((shortlist, match_id)) = shortlist else {
            return Err(WorkspaceError::Invalid(
                "RESOURCE_SHORTLIST_REQUIRED".into(),
            ));
        };
        let model: Option<String> =
            sqlx::query_scalar("SELECT embed_model_digest FROM agent_provider WHERE singleton=1")
                .fetch_optional(&mut *conn)
                .await?;
        let calibration: Vec<super::coach::CurrentCalibration> = sqlx::query_as(
            "SELECT c.asset_kind,c.threshold,c.fixture_digest,c.calibration_revision,c.scoring_config_digest
             FROM matching_calibration c WHERE c.trusted=1 AND c.model_digest=?
             AND c.calibration_revision=(SELECT max(c2.calibration_revision) FROM matching_calibration c2
               WHERE c2.trusted=1 AND c2.model_digest=c.model_digest AND c2.asset_kind=c.asset_kind)
             ORDER BY c.asset_kind",
        )
        .bind(model.as_deref())
        .fetch_all(&mut *conn)
        .await?;
        let assessment: Option<super::coach::Assessment> = sqlx::query_as(
            "SELECT '' AS segment_id,0 AS segment_revision,opportunity_id,opportunity_revision,recipient_id,recipient_revision,
                    id AS match_id,opportunity_revision AS match_opportunity_revision,
                    recipient_revision AS match_recipient_revision,embed_model_digest AS model_digest,
                    calibration_json,shortlist_json
             FROM match_run WHERE id=? AND opportunity_revision=? AND recipient_revision=?",
        )
        .bind(&match_id)
        .bind(opportunity_ref.as_ref().map(|(_, revision)| *revision).unwrap_or_default())
        .bind(party_revision)
        .fetch_optional(&mut *conn)
        .await?;
        if assessment.as_ref().is_none_or(|value| {
            super::coach::assessment_result(value, model.as_deref(), &calibration).is_none()
        }) {
            return Err(WorkspaceError::Invalid(
                "RESOURCE_ASSESSMENT_UNAVAILABLE".into(),
            ));
        }
        let shortlist: Value = serde_json::from_str(&shortlist)
            .map_err(|_| WorkspaceError::Invalid("ASSESSMENT_INVALID".into()))?;
        let eligible = shortlist["results"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|result| result["candidates"].as_array().into_iter().flatten())
            .filter_map(|candidate| candidate["id"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if request
            .resource_ids
            .iter()
            .any(|id| !eligible.contains(id.as_str()))
        {
            return Err(WorkspaceError::Invalid("RESOURCE_NOT_ELIGIBLE".into()));
        }
        deps.push(
            json!({"kind":"eligible_shortlist","id":match_id,"revision":fingerprint(&shortlist)?}),
        );
    }
    deps.sort_by_key(|left| left.to_string());
    let projection = json!({"scope":scope_projection,"segment":segment,"evidence":evidence,"resources":resources});
    Ok(ContextSnapshot {
        dependency_json: Value::Array(deps),
        projection,
    })
}
