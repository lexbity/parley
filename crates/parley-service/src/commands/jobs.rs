//! U05 job commands: `brief.start` and `job.get_status` (UCR-001 catalogue
//! U05).
//!
//! `brief.start` enqueues a reviewable briefer job about a selected party or
//! eligible opportunity inside the same reserved transaction as the ledger.
//! `job.get_status` is the scoped read counterpart; the read execution lives
//! in `commands::read` so it shares the read pipeline.

use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

use super::Actor;
use super::descriptor::{CommandDescriptor, PaletteSpec};
use super::policy::{Effect, Permission, Surface};
use crate::application::brief::{BriefError, BriefInput, start as brief_start};

pub(crate) const BRIEF_START_ID: &str = "brief.start";
pub(crate) const JOB_STATUS_ID: &str = "job.get_status";

static BRIEF_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["expected_revision", "purpose"],
        "properties": {
            "party_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "opportunity_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "expected_revision": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64},
            "purpose": {"type": "string", "minLength": 1, "maxLength": 2000, "x-sensitive": true}
        },
        "oneOf": [
            {"required": ["party_id"]},
            {"required": ["opportunity_id"]}
        ]
    })
});

static BRIEF_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["job_id", "scope_kind", "scope_id", "state", "event_cursor"],
        "properties": {
            "job_id": {"type": "string"},
            "scope_kind": {"type": "string"},
            "scope_id": {"type": "string"},
            "state": {"type": "string"},
            "event_cursor": {"type": "string"}
        }
    })
});

static JOB_STATUS_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["job_id"],
        "properties": {"job_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true}}
    })
});

static JOB_STATUS_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["job_id", "role", "state", "current_attempt", "max_attempts", "progress_current", "progress_total", "last_error_code", "review_reference", "event_cursor"],
        "properties": {
            "job_id": {"type": "string"},
            "role": {"type": "string"},
            "state": {"type": "string"},
            "current_attempt": {"type": "integer", "minimum": 0},
            "max_attempts": {"type": "integer", "minimum": 0},
            "progress_current": {"type": "integer", "minimum": 0},
            "progress_total": {"type": "integer", "minimum": 0},
            "last_error_code": {"type": ["string", "null"]},
            "review_reference": {"type": ["object", "null"]},
            "event_cursor": {"type": "string"}
        }
    })
});

pub(crate) fn descriptors() -> Vec<&'static CommandDescriptor> {
    static BRIEF: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: BRIEF_START_ID,
        version: 1,
        title: "Start a brief",
        summary: "Queue a reviewable brief about a granted party or opportunity",
        description: "Starts a briefer job about the selected party, or about the primary party of the selected open opportunity. The job collects only saved source evidence already scoped to that party and persists reviewable proposals; nothing is accepted automatically. The requested source revision must match the current record. The owner reviews the proposal in the Brief workspace. This command never sends or approves anything.",
        keywords: &["brief", "research", "evidence", "proposal"],
        effect: Effect::Write,
        requires_permission: Permission::BriefStart,
        surfaces: &[Surface::Agent],
        agent_tier: Some(2),
        args_schema: &BRIEF_ARGS_SCHEMA,
        result_schema: &BRIEF_RESULT_SCHEMA,
        supports_dry_run: false,
        preview_schema: None,
        when: None,
        palette: PaletteSpec {
            context_fields: &[],
            prompts: &[],
        },
    });
    static JOB_STATUS: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: JOB_STATUS_ID,
        version: 1,
        title: "Job status",
        summary: "Read the bounded status of a job this run started",
        description: "Returns bounded progress, a safe error code and a typed review reference for a job started by this run. It never returns raw model output, job inputs or transcripts. Jobs started by other runs are concealed.",
        keywords: &["job", "status", "progress", "review"],
        effect: Effect::Read,
        requires_permission: Permission::JobRead,
        surfaces: &[Surface::Agent],
        agent_tier: Some(2),
        args_schema: &JOB_STATUS_ARGS_SCHEMA,
        result_schema: &JOB_STATUS_RESULT_SCHEMA,
        supports_dry_run: false,
        preview_schema: None,
        when: None,
        palette: PaletteSpec {
            context_fields: &[],
            prompts: &[],
        },
    });
    vec![&BRIEF, &JOB_STATUS]
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BriefStartWireArgs {
    party_id: Option<String>,
    opportunity_id: Option<String>,
    expected_revision: i64,
    purpose: String,
}

#[derive(Debug)]
pub(crate) enum RunError {
    InvalidArgs,
    Brief(BriefError),
}

/// Execute `brief.start`. The actor is the checked run or owner context; the
/// operation resolves the scope, verifies granted targets and enqueues the
/// job in the caller's transaction.
pub(crate) async fn run(
    tx: &mut Transaction<'_, Sqlite>,
    args: &Value,
    _operation_key_digest: &str,
    actor: &Actor,
) -> Result<super::RunOutcome, RunError> {
    let parsed: BriefStartWireArgs =
        serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
    let (target_kind, target_id) =
        match (parsed.party_id.as_deref(), parsed.opportunity_id.as_deref()) {
            (Some(id), None) => ("party", id.to_owned()),
            (None, Some(id)) => ("opportunity", id.to_owned()),
            _ => return Err(RunError::InvalidArgs),
        };
    let outcome = brief_start(
        tx,
        &BriefInput {
            target_kind,
            target_id,
            expected_revision: parsed.expected_revision,
            purpose: parsed.purpose,
            epoch: actor.epoch.clone(),
            run_id: actor.run_id.clone(),
            owner_pseudonym: Some(actor.pseudonym.clone()),
            granted_targets: actor.targets.clone(),
        },
    )
    .await
    .map_err(RunError::Brief)?;
    Ok(super::RunOutcome {
        result: serde_json::to_value(&outcome).unwrap_or(serde_json::Value::Null),
        effected: true,
    })
}

/// Serde/schema agreement for schema-valid samples.
pub(crate) fn parse_wire_args(args: &Value) -> Result<(), String> {
    let parsed: BriefStartWireArgs = serde_json::from_value(args.clone()).map_err(|error| {
        format!("serde rejected schema-valid args for {BRIEF_START_ID}: {error}")
    })?;
    let one = parsed.party_id.is_some() as u8 + parsed.opportunity_id.is_some() as u8;
    if one != 1 {
        return Err(format!(
            "serde rejected schema-valid args for {BRIEF_START_ID}: exactly one target is required"
        ));
    }
    Ok(())
}

pub(crate) fn validation_samples() -> (Vec<super::SampleArgs>, Vec<super::SampleArgs>) {
    let valid = vec![
        super::SampleArgs {
            command: BRIEF_START_ID.to_string(),
            args: json!({
                "party_id": "party_sample",
                "expected_revision": 2,
                "purpose": "Prepare a reviewable brief"
            }),
        },
        super::SampleArgs {
            command: BRIEF_START_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "expected_revision": 1,
                "purpose": "Prepare a reviewable brief"
            }),
        },
    ];
    let invalid = vec![
        super::SampleArgs {
            command: BRIEF_START_ID.to_string(),
            args: json!({"expected_revision": 1, "purpose": "no target"}),
        },
        super::SampleArgs {
            command: BRIEF_START_ID.to_string(),
            args: json!({
                "party_id": "party_sample",
                "opportunity_id": "opp_sample",
                "expected_revision": 1,
                "purpose": "both targets"
            }),
        },
        super::SampleArgs {
            command: BRIEF_START_ID.to_string(),
            args: json!({
                "party_id": "party_sample",
                "expected_revision": 1,
                "purpose": "",
                "extra": true
            }),
        },
    ];
    (valid, invalid)
}

pub(crate) fn args_allowlist() -> &'static [&'static str] {
    &[
        "party_id",
        "opportunity_id",
        "expected_revision",
        "purpose",
        "job_id",
    ]
}

pub(crate) fn args_sensitive() -> &'static [&'static str] {
    &["party_id", "opportunity_id", "purpose", "job_id"]
}
