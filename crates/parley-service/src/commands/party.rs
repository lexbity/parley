//! U04 party edit commands: `party.rename` and `party.set_focus`
//! (UCR-001 catalogue U04).
//!
//! Both registry entries call the same typed application operation as the
//! REST entity PATCH adapter. A rename and a focus update are distinct
//! intents; each requires the party ID and the expected card revision. True
//! no-ops (the requested value already holds) emit zero events and consume
//! zero executed-write allowance.

use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

use super::descriptor::{CommandDescriptor, PaletteSpec, PromptSpec};
use super::policy::{Effect, Permission, Surface};
use crate::application::party_edit::{
    PartyEditError, PartyEditInput, PartyEditOutcome, apply_party_edit,
};

pub(crate) const PARTY_RENAME_ID: &str = "party.rename";
pub(crate) const PARTY_SET_FOCUS_ID: &str = "party.set_focus";

static RENAME_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["party_id", "expected_card_revision", "name"],
        "properties": {
            "party_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "expected_card_revision": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64},
            "name": {"type": "string", "minLength": 1, "maxLength": 500}
        }
    })
});

static SET_FOCUS_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["party_id", "expected_card_revision", "focus"],
        "properties": {
            "party_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "expected_card_revision": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64},
            "focus": {"type": "string", "maxLength": 250}
        }
    })
});

static PARTY_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "kind", "name", "focus", "revision"],
        "properties": {
            "id": {"type": "string"},
            "kind": {"type": "string"},
            "name": {"type": "string"},
            "focus": {"type": "string"},
            "revision": {"type": "integer", "minimum": 1},
            "event_id": {"type": ["string", "null"]},
            "event_cursor": {"type": ["integer", "null"]}
        }
    })
});

const PARTIES_WHEN: &str = "route == \"parties\" && selection.party";
const PARTY_CONTEXT_FIELDS: &[&str] = &["route"];
const ARGS_ALLOWLIST: &[&str] = &["party_id", "expected_card_revision", "name", "focus"];
const ARGS_SENSITIVE: &[&str] = &["party_id", "name", "focus"];

pub(crate) fn descriptors() -> Vec<&'static CommandDescriptor> {
    static RENAME: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: PARTY_RENAME_ID,
        version: 1,
        title: "Rename party",
        summary: "Change a party's name",
        description: "Changes the party's labeled name. The expected card revision from the selected row is required; a stale card fails without an effect. Contact policy, claims, graph edges and provenance remain untouched.",
        keywords: &["party", "rename", "name"],
        effect: Effect::Write,
        requires_permission: Permission::PartyEdit,
        surfaces: &[Surface::Palette, Surface::Agent],
        agent_tier: Some(2),
        args_schema: &RENAME_ARGS_SCHEMA,
        result_schema: &PARTY_RESULT_SCHEMA,
        supports_dry_run: false,
        when: Some(PARTIES_WHEN),
        palette: PaletteSpec {
            context_fields: PARTY_CONTEXT_FIELDS,
            prompts: &[PromptSpec {
                field: "name",
                picker: "text",
                title: "Party name",
            }],
        },
    });
    static SET_FOCUS: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: PARTY_SET_FOCUS_ID,
        version: 1,
        title: "Set party focus",
        summary: "Change a party's focus label",
        description: "Changes the party's short focus descriptor shown on the sheet. The expected card revision from the selected row is required; a stale card fails without an effect. Contact policy, claims, graph edges and provenance remain untouched.",
        keywords: &["party", "focus"],
        effect: Effect::Write,
        requires_permission: Permission::PartyEdit,
        surfaces: &[Surface::Palette, Surface::Agent],
        agent_tier: Some(2),
        args_schema: &SET_FOCUS_ARGS_SCHEMA,
        result_schema: &PARTY_RESULT_SCHEMA,
        supports_dry_run: false,
        when: Some(PARTIES_WHEN),
        palette: PaletteSpec {
            context_fields: PARTY_CONTEXT_FIELDS,
            prompts: &[PromptSpec {
                field: "focus",
                picker: "text",
                title: "Focus",
            }],
        },
    });
    vec![&RENAME, &SET_FOCUS]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PartyCommand {
    Rename,
    SetFocus,
}

impl PartyCommand {
    pub(crate) fn for_id(id: &str) -> Option<Self> {
        match id {
            PARTY_RENAME_ID => Some(Self::Rename),
            PARTY_SET_FOCUS_ID => Some(Self::SetFocus),
            _ => None,
        }
    }

    pub(crate) fn command_id(&self) -> &'static str {
        match self {
            Self::Rename => PARTY_RENAME_ID,
            Self::SetFocus => PARTY_SET_FOCUS_ID,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameWireArgs {
    party_id: String,
    expected_card_revision: i64,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetFocusWireArgs {
    party_id: String,
    expected_card_revision: i64,
    focus: String,
}

pub(crate) enum RunError {
    InvalidArgs,
    Party(PartyEditError),
}

pub(crate) async fn run(
    command: PartyCommand,
    tx: &mut Transaction<'_, Sqlite>,
    args: &Value,
    operation_key_digest: &str,
) -> Result<super::RunOutcome, RunError> {
    let (party_id, expected_revision, name, focus) = match command {
        PartyCommand::Rename => {
            let parsed: RenameWireArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
            (
                parsed.party_id,
                parsed.expected_card_revision,
                Some(parsed.name),
                None,
            )
        }
        PartyCommand::SetFocus => {
            let parsed: SetFocusWireArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
            (
                parsed.party_id,
                parsed.expected_card_revision,
                None,
                Some(parsed.focus),
            )
        }
    };
    let outcome = apply_party_edit(
        tx,
        &PartyEditInput {
            party_id,
            expected_revision,
            name,
            focus,
            operation_key_digest: operation_key_digest.to_string(),
        },
    )
    .await
    .map_err(RunError::Party)?;
    match outcome {
        PartyEditOutcome::Changed(result) => Ok(super::RunOutcome {
            result: serde_json::to_value(&result).unwrap_or_default(),
            effected: true,
        }),
        PartyEditOutcome::NoOp(result) => Ok(super::RunOutcome {
            result: serde_json::to_value(&result).unwrap_or_default(),
            effected: false,
        }),
    }
}

/// Serde/schema agreement for schema-valid samples.
pub(crate) fn parse_wire_args(command: PartyCommand, args: &Value) -> Result<(), String> {
    let ok = match command {
        PartyCommand::Rename => serde_json::from_value::<RenameWireArgs>(args.clone()).is_ok(),
        PartyCommand::SetFocus => serde_json::from_value::<SetFocusWireArgs>(args.clone()).is_ok(),
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "serde rejected schema-valid args for {}",
            command.command_id()
        ))
    }
}

pub(crate) fn validation_samples() -> (Vec<super::SampleArgs>, Vec<super::SampleArgs>) {
    let valid = vec![
        super::SampleArgs {
            command: PARTY_RENAME_ID.to_string(),
            args: json!({"party_id": "party_sample", "expected_card_revision": 2, "name": "New name"}),
        },
        super::SampleArgs {
            command: PARTY_SET_FOCUS_ID.to_string(),
            args: json!({"party_id": "party_sample", "expected_card_revision": 2, "focus": "Focus"}),
        },
    ];
    let invalid = vec![
        super::SampleArgs {
            command: PARTY_RENAME_ID.to_string(),
            args: json!({"party_id": "party_sample", "expected_card_revision": 2}),
        },
        super::SampleArgs {
            command: PARTY_RENAME_ID.to_string(),
            args: json!({"party_id": "party_sample", "expected_card_revision": 2, "name": ""}),
        },
        super::SampleArgs {
            command: PARTY_SET_FOCUS_ID.to_string(),
            args: json!({"party_id": "party_sample", "name": "x"}),
        },
        super::SampleArgs {
            command: PARTY_SET_FOCUS_ID.to_string(),
            args: json!({"party_id": "party_sample", "expected_card_revision": 2, "focus": 5}),
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
