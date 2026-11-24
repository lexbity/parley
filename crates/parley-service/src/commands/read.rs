//! U02 read commands: `party.get_summary` and `opportunity.get_summary`.
//!
//! Both are Tier 1 agent-only commands and call typed summary operations.
//! Results are deliberately small: no notes, source text, attachments or
//! private evidence can reach a tool result.

use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

use super::Actor;
use super::descriptor::{CommandDescriptor, PaletteSpec};
use super::policy::{Effect, Permission, Surface};
use crate::application::job_status::status as job_status;
use crate::application::opportunity_summary::summary as opportunity_summary;
use crate::application::party_summary::summary as party_summary;

pub(crate) const PARTY_SUMMARY_ID: &str = "party.get_summary";
pub(crate) const OPPORTUNITY_SUMMARY_ID: &str = "opportunity.get_summary";
pub(crate) const JOB_STATUS_ID: &str = "job.get_status";
pub(crate) const RESOURCE_SUMMARY_ID: &str = "resource.get_summary";
pub(crate) const GATE_CHECK_ID: &str = "material.check_gate";

static PARTY_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["party_id"],
        "properties": {"party_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true}}
    })
});
static PARTY_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "kind", "name", "focus", "card_revision"],
        "properties": {
            "id": {"type": "string"}, "kind": {"type": "string"},
            "name": {"type": "string"}, "focus": {"type": "string"},
            "card_revision": {"type": "integer", "minimum": 1}
        }
    })
});
static OPPORTUNITY_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["opportunity_id"],
        "properties": {"opportunity_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true}}
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
static OPPORTUNITY_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "title", "lifecycle", "stage_id", "phase_revision", "participants"],
        "properties": {
            "id": {"type": "string"}, "title": {"type": "string"},
            "lifecycle": {"type": "string"},
            "stage_id": {"type": ["string", "null"]},
            "phase_revision": {"type": "integer", "minimum": 1},
            "participants": {"type": "array", "items": {"type": "string"}}
        }
    })
});
static RESOURCE_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["resource_id"],
        "properties": {"resource_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true}}
    })
});
static RESOURCE_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "subtype", "title", "readiness", "content_revision"],
        "properties": {
            "id": {"type": "string"},
            "subtype": {"type": "string"},
            "title": {"type": "string"},
            "readiness": {"type": "string"},
            "content_revision": {"type": "integer", "minimum": 1}
        }
    })
});
static GATE_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["material_id", "expected_version"],
        "properties": {
            "material_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "expected_version": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64}
        }
    })
});

pub(crate) fn descriptors() -> Vec<&'static CommandDescriptor> {
    static PARTY: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: PARTY_SUMMARY_ID,
        version: 1,
        title: "Summarise a party",
        summary: "Return a party's type, name, focus and card revision",
        description: "Returns only the party type, name, focus and card revision. It cannot read notes, attachments or private evidence and cannot change any record.",
        keywords: &["party", "summary"],
        effect: Effect::Read,
        requires_permission: Permission::PartyRead,
        surfaces: &[Surface::Agent],
        agent_tier: Some(1),
        args_schema: &PARTY_ARGS_SCHEMA,
        result_schema: &PARTY_RESULT_SCHEMA,
        supports_dry_run: false,
        preview_schema: None,
        when: None,
        palette: PaletteSpec {
            context_fields: &[],
            prompts: &[],
        },
    });
    static OPPORTUNITY: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: OPPORTUNITY_SUMMARY_ID,
        version: 1,
        title: "Summarise an opportunity",
        summary: "Return an opportunity's title, lifecycle, phase and granted participants",
        description: "Returns only the opportunity title, lifecycle, phase and phase revision plus participant labels inside the run's grant. It cannot read notes or private evidence and cannot change any record.",
        keywords: &["opportunity", "summary"],
        effect: Effect::Read,
        requires_permission: Permission::OpportunityRead,
        surfaces: &[Surface::Agent],
        agent_tier: Some(1),
        args_schema: &OPPORTUNITY_ARGS_SCHEMA,
        result_schema: &OPPORTUNITY_RESULT_SCHEMA,
        supports_dry_run: false,
        preview_schema: None,
        when: None,
        palette: PaletteSpec {
            context_fields: &[],
            prompts: &[],
        },
    });
    static RESOURCE: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: RESOURCE_SUMMARY_ID,
        version: 1,
        title: "Summarise a resource",
        summary: "Return a resource's subtype, title, readiness and content revision",
        description: "Returns only the resource subtype, title, readiness and content revision. It cannot read notes, attachments or private evidence and cannot change any record.",
        keywords: &["resource", "summary"],
        effect: Effect::Read,
        requires_permission: Permission::ResourceRead,
        surfaces: &[Surface::Agent],
        agent_tier: Some(1),
        args_schema: &RESOURCE_ARGS_SCHEMA,
        result_schema: &RESOURCE_RESULT_SCHEMA,
        supports_dry_run: false,
        preview_schema: None,
        when: None,
        palette: PaletteSpec {
            context_fields: &[],
            prompts: &[],
        },
    });
    vec![&PARTY, &OPPORTUNITY, &RESOURCE]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadCommand {
    Party,
    Opportunity,
    JobStatus,
    Resource,
    MaterialGate,
}

impl ReadCommand {
    pub(crate) fn for_id(id: &str) -> Option<Self> {
        match id {
            PARTY_SUMMARY_ID => Some(Self::Party),
            OPPORTUNITY_SUMMARY_ID => Some(Self::Opportunity),
            JOB_STATUS_ID => Some(Self::JobStatus),
            RESOURCE_SUMMARY_ID => Some(Self::Resource),
            GATE_CHECK_ID => Some(Self::MaterialGate),
            _ => None,
        }
    }

    #[allow(dead_code)] // schema is exposed through the descriptor
    pub(crate) fn args_schema(&self) -> &'static Value {
        match self {
            Self::Party => &PARTY_ARGS_SCHEMA,
            Self::Opportunity => &OPPORTUNITY_ARGS_SCHEMA,
            Self::JobStatus => &JOB_STATUS_ARGS_SCHEMA,
            Self::Resource => &RESOURCE_ARGS_SCHEMA,
            Self::MaterialGate => &GATE_ARGS_SCHEMA,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartyArgs {
    party_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpportunityArgs {
    opportunity_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobArgs {
    job_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceArgs {
    resource_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterialGateArgs {
    material_id: String,
    expected_version: i64,
}

#[derive(Debug)]
pub(crate) enum RunError {
    InvalidArgs,
    NotFound,
}

/// Execute a read command with the checked actor context of the invocation.
/// The job-status read is scoped to jobs the run itself started.
pub(crate) async fn run(
    command: ReadCommand,
    tx: &mut Transaction<'_, Sqlite>,
    args: &Value,
    actor: &Actor,
) -> Result<Value, RunError> {
    match command {
        ReadCommand::Party => {
            let parsed: PartyArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
            let result = party_summary(tx, &parsed.party_id)
                .await
                .map_err(|_| RunError::NotFound)?
                .ok_or(RunError::NotFound)?;
            Ok(serde_json::to_value(result).map_err(|_| RunError::NotFound)?)
        }
        ReadCommand::Opportunity => {
            let parsed: OpportunityArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
            let result = opportunity_summary(tx, &parsed.opportunity_id, &actor.party_ids())
                .await
                .map_err(|_| RunError::NotFound)?
                .ok_or(RunError::NotFound)?;
            Ok(serde_json::to_value(result).map_err(|_| RunError::NotFound)?)
        }
        ReadCommand::JobStatus => {
            let parsed: JobArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
            let result = job_status(tx, actor, &parsed.job_id)
                .await
                .map_err(|_| RunError::NotFound)?
                .ok_or(RunError::NotFound)?;
            Ok(serde_json::to_value(result).map_err(|_| RunError::NotFound)?)
        }
        ReadCommand::Resource => {
            let parsed: ResourceArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
            let result = crate::application::resources::summary(tx, &parsed.resource_id)
                .await
                .map_err(|_| RunError::NotFound)?
                .ok_or(RunError::NotFound)?;
            Ok(serde_json::to_value(result).map_err(|_| RunError::NotFound)?)
        }
        ReadCommand::MaterialGate => {
            let parsed: MaterialGateArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
            let result = crate::application::material_gate::check(
                tx,
                &crate::application::material_gate::MaterialGateInput {
                    material_id: parsed.material_id,
                    expected_version: parsed.expected_version,
                },
            )
            .await
            .map_err(|error| match error {
                crate::application::material_gate::MaterialGateError::NotFound
                | crate::application::material_gate::MaterialGateError::StaleVersion => {
                    RunError::NotFound
                }
                crate::application::material_gate::MaterialGateError::Database => {
                    RunError::NotFound
                }
            })?;
            Ok(serde_json::to_value(result).map_err(|_| RunError::NotFound)?)
        }
    }
}

/// Serde/schema agreement for schema-valid samples.
pub(crate) fn parse_wire_args(command: ReadCommand, args: &Value) -> Result<(), String> {
    let ok = match command {
        ReadCommand::Party => serde_json::from_value::<PartyArgs>(args.clone()).is_ok(),
        ReadCommand::Opportunity => serde_json::from_value::<OpportunityArgs>(args.clone()).is_ok(),
        ReadCommand::JobStatus => serde_json::from_value::<JobArgs>(args.clone()).is_ok(),
        ReadCommand::Resource => serde_json::from_value::<ResourceArgs>(args.clone()).is_ok(),
        ReadCommand::MaterialGate => {
            serde_json::from_value::<MaterialGateArgs>(args.clone()).is_ok()
        }
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "serde rejected schema-valid args for {}",
            command_id(command)
        ))
    }
}

fn command_id(command: ReadCommand) -> &'static str {
    match command {
        ReadCommand::Party => PARTY_SUMMARY_ID,
        ReadCommand::Opportunity => OPPORTUNITY_SUMMARY_ID,
        ReadCommand::JobStatus => JOB_STATUS_ID,
        ReadCommand::Resource => RESOURCE_SUMMARY_ID,
        ReadCommand::MaterialGate => GATE_CHECK_ID,
    }
}

pub(crate) fn args_allowlist() -> &'static [&'static str] {
    &[
        "party_id",
        "opportunity_id",
        "job_id",
        "resource_id",
        "material_id",
        "expected_version",
    ]
}

pub(crate) fn args_sensitive() -> &'static [&'static str] {
    &[
        "party_id",
        "opportunity_id",
        "job_id",
        "resource_id",
        "material_id",
    ]
}

pub(crate) fn validation_samples() -> (Vec<super::SampleArgs>, Vec<super::SampleArgs>) {
    let valid = vec![
        super::SampleArgs {
            command: PARTY_SUMMARY_ID.to_string(),
            args: json!({"party_id": "party_sample"}),
        },
        super::SampleArgs {
            command: OPPORTUNITY_SUMMARY_ID.to_string(),
            args: json!({"opportunity_id": "opp_sample"}),
        },
        super::SampleArgs {
            command: JOB_STATUS_ID.to_string(),
            args: json!({"job_id": "job_sample"}),
        },
        super::SampleArgs {
            command: RESOURCE_SUMMARY_ID.to_string(),
            args: json!({"resource_id": "res_sample"}),
        },
        super::SampleArgs {
            command: GATE_CHECK_ID.to_string(),
            args: json!({"material_id": "mat_sample", "expected_version": 1}),
        },
    ];
    let invalid = vec![
        super::SampleArgs {
            command: PARTY_SUMMARY_ID.to_string(),
            args: json!({}),
        },
        super::SampleArgs {
            command: OPPORTUNITY_SUMMARY_ID.to_string(),
            args: json!({"opportunity_id": "opp_sample", "notes": "private"}),
        },
        super::SampleArgs {
            command: JOB_STATUS_ID.to_string(),
            args: json!({}),
        },
        super::SampleArgs {
            command: RESOURCE_SUMMARY_ID.to_string(),
            args: json!({"resource_id": "res_sample", "notes": "private"}),
        },
        super::SampleArgs {
            command: GATE_CHECK_ID.to_string(),
            args: json!({"material_id": "mat_sample"}),
        },
        super::SampleArgs {
            command: GATE_CHECK_ID.to_string(),
            args: json!({"material_id": "mat_sample", "expected_version": 0}),
        },
    ];
    (valid, invalid)
}
