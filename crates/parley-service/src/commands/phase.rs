//! U01 phase commands: `opportunity.set_phase` and
//! `opportunity.undo_phase_change` (UCR-001 catalogue U01).
//!
//! Both commands call the same typed application operation
//! (`application::phase::apply_phase`). Compile-time registration is local
//! to this module; descriptors, closed schemas and sample args live here.

use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

use super::RunOutcome;
use super::descriptor::{CommandDescriptor, PaletteSpec, PromptSpec};
use super::policy::{Effect, Permission, Surface};
use crate::application::phase::{PhaseAction, PhaseError, PhaseInput, apply_phase};

pub(crate) const SET_PHASE_ID: &str = "opportunity.set_phase";
pub(crate) const UNDO_PHASE_ID: &str = "opportunity.undo_phase_change";

pub(crate) static SET_PHASE_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["opportunity_id", "expected_phase_revision", "expected_pack_id", "expected_pack_version", "stage_id"],
        "properties": {
            "opportunity_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "expected_phase_revision": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64},
            "expected_pack_id": {"type": "string", "minLength": 1, "maxLength": 100},
            "expected_pack_version": {"type": "string", "minLength": 1, "maxLength": 100},
            "stage_id": {"type": "string", "minLength": 1, "maxLength": 100}
        }
    })
});

pub(crate) static SET_PHASE_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["opportunity_id", "phase_revision", "previous_stage_id", "stage_id", "event_id", "event_cursor", "reverted"],
        "properties": {
            "opportunity_id": {"type": "string"},
            "phase_revision": {"type": "integer", "minimum": 1},
            "previous_stage_id": {"type": ["string", "null"]},
            "stage_id": {"type": ["string", "null"]},
            "event_id": {"type": ["string", "null"]},
            "event_cursor": {"type": ["string", "null"]},
            "reverted": {"type": "boolean"}
        }
    })
});

pub(crate) static UNDO_PHASE_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["opportunity_id", "expected_phase_revision", "expected_pack_id", "expected_pack_version", "original_event_id"],
        "properties": {
            "opportunity_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "expected_phase_revision": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64},
            "expected_pack_id": {"type": "string", "minLength": 1, "maxLength": 100},
            "expected_pack_version": {"type": "string", "minLength": 1, "maxLength": 100},
            "original_event_id": {"type": "string", "minLength": 1, "maxLength": 200}
        }
    })
});

pub(crate) static UNDO_PHASE_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["opportunity_id", "phase_revision", "previous_stage_id", "stage_id", "event_id", "event_cursor", "reverted"],
        "properties": {
            "opportunity_id": {"type": "string"},
            "phase_revision": {"type": "integer", "minimum": 1},
            "previous_stage_id": {"type": ["string", "null"]},
            "stage_id": {"type": ["string", "null"]},
            "event_id": {"type": ["string", "null"]},
            "event_cursor": {"type": ["string", "null"]},
            "reverted": {"type": "boolean"}
        }
    })
});

const QUEUE_WHEN: &str = "route == \"queue\" && selection.opportunity";
const CONTEXT_FIELDS: &[&str] = &["route", "selection.opportunity"];
const ARGS_ALLOWLIST: &[&str] = &[
    "opportunity_id",
    "expected_phase_revision",
    "expected_pack_id",
    "expected_pack_version",
    "stage_id",
    "original_event_id",
];
const ARGS_SENSITIVE: &[&str] = &["opportunity_id"];

pub(crate) fn descriptors() -> Vec<&'static CommandDescriptor> {
    static SET: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: SET_PHASE_ID,
        version: 1,
        title: "Change opportunity phase",
        summary: "Move an open opportunity to a planning phase",
        description: "Moves an open opportunity to a planning stage. Use for planning. Closing, sending and resolution have separate owner workflows. Updates the phase revision and history.",
        keywords: &["queue", "stage"],
        effect: Effect::Write,
        requires_permission: Permission::OpportunityPhaseEdit,
        surfaces: &[Surface::Palette, Surface::Agent],
        agent_tier: Some(2),
        args_schema: &SET_PHASE_ARGS_SCHEMA,
        result_schema: &SET_PHASE_RESULT_SCHEMA,
        supports_dry_run: false,
        when: Some(QUEUE_WHEN),
        palette: PaletteSpec {
            context_fields: CONTEXT_FIELDS,
            prompts: &[PromptSpec {
                field: "stage_id",
                picker: "planning_stage",
                title: "Planning phase",
            }],
        },
    });
    static UNDO: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: UNDO_PHASE_ID,
        version: 1,
        title: "Undo phase change",
        summary: "Revert the latest manual phase change",
        description: "Returns the opportunity to the stage recorded before its latest manual phase transition, including Unmapped when the original transition captured it. Requires the exact original event ID.",
        keywords: &["queue", "stage", "undo"],
        effect: Effect::Write,
        requires_permission: Permission::OpportunityPhaseEdit,
        surfaces: &[Surface::Palette, Surface::Agent],
        agent_tier: Some(2),
        args_schema: &UNDO_PHASE_ARGS_SCHEMA,
        result_schema: &UNDO_PHASE_RESULT_SCHEMA,
        supports_dry_run: false,
        when: Some(QUEUE_WHEN),
        palette: PaletteSpec {
            context_fields: CONTEXT_FIELDS,
            prompts: &[],
        },
    });
    vec![&SET, &UNDO]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PhaseCommand {
    Set,
    Undo,
}

impl PhaseCommand {
    pub fn for_id(id: &str) -> Option<Self> {
        match id {
            SET_PHASE_ID => Some(PhaseCommand::Set),
            UNDO_PHASE_ID => Some(PhaseCommand::Undo),
            _ => None,
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            PhaseCommand::Set => SET_PHASE_ID,
            PhaseCommand::Undo => UNDO_PHASE_ID,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetPhaseWireArgs {
    opportunity_id: String,
    expected_phase_revision: i64,
    expected_pack_id: String,
    expected_pack_version: String,
    stage_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UndoPhaseWireArgs {
    opportunity_id: String,
    expected_phase_revision: i64,
    expected_pack_id: String,
    expected_pack_version: String,
    original_event_id: String,
}

pub(crate) enum RunError {
    InvalidArgs { path: String, problem: &'static str },
    Phase(PhaseError),
}

/// Execute the phase operation with its typed wire args.
pub(crate) async fn run(
    command: PhaseCommand,
    tx: &mut Transaction<'_, Sqlite>,
    args: &Value,
    operation_key_digest: &str,
) -> Result<RunOutcome, RunError> {
    let input = match command {
        PhaseCommand::Set => {
            let parsed: SetPhaseWireArgs =
                serde_json::from_value(args.clone()).map_err(|_error| RunError::InvalidArgs {
                    path: String::new(),
                    problem: "invalid_args",
                })?;
            PhaseInput {
                opportunity_id: parsed.opportunity_id,
                expected_phase_revision: parsed.expected_phase_revision,
                expected_pack_id: parsed.expected_pack_id,
                expected_pack_version: parsed.expected_pack_version,
                action: PhaseAction::Set {
                    target_stage_id: parsed.stage_id,
                },
                operation_key_digest: operation_key_digest.to_string(),
                expected_target_stage_id: None,
            }
        }
        PhaseCommand::Undo => {
            let parsed: UndoPhaseWireArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs {
                    path: String::new(),
                    problem: "invalid_args",
                })?;
            PhaseInput {
                opportunity_id: parsed.opportunity_id,
                expected_phase_revision: parsed.expected_phase_revision,
                expected_pack_id: parsed.expected_pack_id,
                expected_pack_version: parsed.expected_pack_version,
                action: PhaseAction::Undo {
                    original_event_id: parsed.original_event_id,
                },
                operation_key_digest: operation_key_digest.to_string(),
                expected_target_stage_id: None,
            }
        }
    };
    let outcome = apply_phase(tx, &input).await.map_err(RunError::Phase)?;
    let (result, effected) = match outcome {
        crate::application::phase::PhaseOutcome::Changed(result) => (result, true),
        crate::application::phase::PhaseOutcome::NoOp(result) => (result, false),
    };
    Ok(RunOutcome {
        result: serde_json::json!({
            "opportunity_id": result.opportunity_id,
            "phase_revision": result.phase_revision,
            "previous_stage_id": result.previous_stage_id,
            "stage_id": result.stage_id,
            "event_id": result.event_id,
            "event_cursor": result.event_cursor.map(|cursor| cursor.to_string()),
            "reverted": result.reverted,
        }),
        effected,
    })
}

/// Serde/schema agreement: parse valid wire args through the typed structs.
pub(crate) fn parse_wire_args(command: PhaseCommand, args: &Value) -> Result<(), String> {
    let result = match command {
        PhaseCommand::Set => serde_json::from_value::<SetPhaseWireArgs>(args.clone()).is_ok(),
        PhaseCommand::Undo => serde_json::from_value::<UndoPhaseWireArgs>(args.clone()).is_ok(),
    };
    if result {
        Ok(())
    } else {
        Err(format!(
            "serde rejected schema-valid args for {}",
            command.id()
        ))
    }
}

pub(crate) fn validation_samples() -> (Vec<super::SampleArgs>, Vec<super::SampleArgs>) {
    let valid = vec![
        super::SampleArgs {
            command: SET_PHASE_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "expected_phase_revision": 1,
                "expected_pack_id": "job_search",
                "expected_pack_version": "1.0.0",
                "stage_id": "preparing"
            }),
        },
        super::SampleArgs {
            command: UNDO_PHASE_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "expected_phase_revision": 1,
                "expected_pack_id": "job_search",
                "expected_pack_version": "1.0.0",
                "original_event_id": "evt_sample"
            }),
        },
    ];
    let invalid = vec![
        super::SampleArgs {
            command: SET_PHASE_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "expected_phase_revision": 1,
                "expected_pack_id": "job_search",
                "expected_pack_version": "1.0.0"
            }),
        },
        super::SampleArgs {
            command: SET_PHASE_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "expected_phase_revision": "one",
                "expected_pack_id": "job_search",
                "expected_pack_version": "1.0.0",
                "stage_id": "preparing"
            }),
        },
        super::SampleArgs {
            command: UNDO_PHASE_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "expected_phase_revision": 1,
                "expected_pack_id": "job_search",
                "expected_pack_version": "1.0.0",
                "stage_id": "preparing"
            }),
        },
        super::SampleArgs {
            command: UNDO_PHASE_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "expected_phase_revision": 1,
                "expected_pack_id": "job_search",
                "expected_pack_version": "1.0.0",
                "original_event_id": "evt_sample",
                "extra": true
            }),
        },
    ];
    (valid, invalid)
}

pub(crate) fn args_allowlist() -> &'static [&'static str] {
    ARGS_ALLOWLIST
}

pub(crate) fn args_sensitive() -> &'static [&'static str] {
    ARGS_SENSITIVE
}
