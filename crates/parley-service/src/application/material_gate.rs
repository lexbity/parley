//! Typed scoped material gate assessment (UCR-001 U07).
//!
//! `material.check_gate` is the read counterpart of the writing jobs. It
//! evaluates the deterministic content gates for one material version and
//! projects only bounded check codes, severities and a short summary. It
//! never applies overrides, consumes an approval, or returns output bytes or
//! raw provider text (U07).

use sqlx::{Sqlite, Transaction};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MaterialGateInput {
    pub material_id: String,
    pub expected_version: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct GateCheckBrief {
    pub code: String,
    pub severity: String,
    pub pass: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct MaterialGateResult {
    pub material_id: String,
    pub version: i64,
    pub checks: Vec<GateCheckBrief>,
    pub copy_enabled: bool,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MaterialGateError {
    NotFound,
    StaleVersion,
    Database,
}

pub(crate) async fn check(
    tx: &mut Transaction<'_, Sqlite>,
    input: &MaterialGateInput,
) -> Result<MaterialGateResult, MaterialGateError> {
    let material = crate::release::load_head(tx, &input.material_id)
        .await
        .map_err(|error| match error {
            crate::WorkspaceError::Invalid(_) => MaterialGateError::NotFound,
            _ => MaterialGateError::Database,
        })?;
    if input.expected_version != material.version {
        return Err(MaterialGateError::StaleVersion);
    }
    // Content gates only; send-time schedule gates remain in the Mark sent
    // flow. A gate check never applies overrides (U07).
    let result = crate::release::evaluate_material(tx, &material, &Default::default(), false)
        .await
        .map_err(|_| MaterialGateError::Database)?;
    let hard_failures = result
        .checks
        .iter()
        .filter(|check| check.severity == parley_domain::gate::Severity::Hard && !check.pass)
        .count();
    let soft_failures = result
        .checks
        .iter()
        .filter(|check| check.severity == parley_domain::gate::Severity::Soft && !check.pass)
        .count();
    let summary = if hard_failures == 0 && soft_failures == 0 {
        "All content gates pass".to_owned()
    } else if hard_failures == 0 {
        format!("{soft_failures} soft content gate(s) fail; hard gates pass")
    } else {
        format!("{hard_failures} hard content gate(s) fail ({soft_failures} soft)")
    };
    let checks = result
        .checks
        .into_iter()
        .map(|check| GateCheckBrief {
            code: check.id,
            severity: match check.severity {
                parley_domain::gate::Severity::Hard => "hard".to_owned(),
                parley_domain::gate::Severity::Soft => "soft".to_owned(),
            },
            pass: check.pass,
        })
        .collect();
    Ok(MaterialGateResult {
        material_id: input.material_id.clone(),
        version: material.version,
        checks,
        copy_enabled: result.copy_enabled,
        summary,
    })
}
