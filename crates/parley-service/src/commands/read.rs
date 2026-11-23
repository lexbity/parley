//! U02 read commands: `party.get_summary` and `opportunity.get_summary`.
//!
//! Both are Tier 1 agent-only commands and call typed summary operations.
//! Results are deliberately small: no notes, source text, attachments or
//! private evidence can reach a tool result.

use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

use super::descriptor::{CommandDescriptor, PaletteSpec};
use super::policy::{Effect, Permission, Surface};
use crate::application::opportunity_summary::summary as opportunity_summary;
use crate::application::party_summary::summary as party_summary;

pub(crate) const PARTY_SUMMARY_ID: &str = "party.get_summary";
pub(crate) const OPPORTUNITY_SUMMARY_ID: &str = "opportunity.get_summary";

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
        when: None,
        palette: PaletteSpec {
            context_fields: &[],
            prompts: &[],
        },
    });
    vec![&PARTY, &OPPORTUNITY]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadCommand {
    Party,
    Opportunity,
}

impl ReadCommand {
    pub(crate) fn for_id(id: &str) -> Option<Self> {
        match id {
            PARTY_SUMMARY_ID => Some(Self::Party),
            OPPORTUNITY_SUMMARY_ID => Some(Self::Opportunity),
            _ => None,
        }
    }

    #[allow(dead_code)] // schema is exposed through the descriptor
    pub(crate) fn args_schema(&self) -> &'static Value {
        match self {
            Self::Party => &PARTY_ARGS_SCHEMA,
            Self::Opportunity => &OPPORTUNITY_ARGS_SCHEMA,
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

pub(crate) enum RunError {
    InvalidArgs,
    NotFound,
}

/// Execute a read command with the immutable party target set of the run.
pub(crate) async fn run(
    command: ReadCommand,
    tx: &mut Transaction<'_, Sqlite>,
    args: &Value,
    granted_parties: &[String],
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
            let result = opportunity_summary(tx, &parsed.opportunity_id, granted_parties)
                .await
                .map_err(|_| RunError::NotFound)?
                .ok_or(RunError::NotFound)?;
            Ok(serde_json::to_value(result).map_err(|_| RunError::NotFound)?)
        }
    }
}

/// Serde/schema agreement for schema-valid samples.
pub(crate) fn parse_wire_args(command: ReadCommand, args: &Value) -> Result<(), String> {
    let ok = match command {
        ReadCommand::Party => serde_json::from_value::<PartyArgs>(args.clone()).is_ok(),
        ReadCommand::Opportunity => serde_json::from_value::<OpportunityArgs>(args.clone()).is_ok(),
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
    }
}

pub(crate) fn args_allowlist() -> &'static [&'static str] {
    &["party_id", "opportunity_id"]
}

pub(crate) fn args_sensitive() -> &'static [&'static str] {
    &["party_id", "opportunity_id"]
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
    ];
    (valid, invalid)
}
