//! Shared scope/freshness checks for writing roles (AR-001 S09 and AR08).
//!
//! Composer and Linter execution now runs through the packaged structured
//! worker (`inference/roles.rs`); this module owns the deterministic scope
//! and revision checks that both the role publish path and the manual draft
//! save path reuse. Deterministic lint policy stays in Rust; the local model
//! supplies bounded prose only.
use serde_json::Value;
use sqlx::{Sqlite, Transaction};

pub(crate) struct RoleFailure {
    #[allow(dead_code)] // exercised by unit tests and used as a typed tag
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl RoleFailure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
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
    use crate::Workspace;
    use serde_json::json;
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
