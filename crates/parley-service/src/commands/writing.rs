//! U07 writing commands: `match.start`, `material.compose_draft` and
//! `material.check_gate` (UCR-001 catalogue U07).
//!
//! The two job-start commands enqueue reviewable matcher/composer jobs inside
//! the same reserved transaction as the ledger, scoped to the granted
//! opportunity and recipient and fenced to the initiating run. The gate
//! command is a read that executes through the shared read pipeline; its
//! descriptor lives here, its serde/schema agreement checks live in
//! `commands::read` next to the other read commands.

use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

use super::Actor;
use super::descriptor::{CommandDescriptor, PaletteSpec};
use super::policy::{Effect, Permission, Surface};
use crate::application::compose_start::{
    ComposeStartError, ComposeStartInput, start as compose_start,
};
use crate::application::match_start::{MatchStartError, MatchStartInput, start as match_start};

pub(crate) const MATCH_START_ID: &str = "match.start";
pub(crate) const COMPOSE_DRAFT_ID: &str = "material.compose_draft";
pub(crate) const GATE_CHECK_ID: &str = "material.check_gate";

static MATCH_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["opportunity_id", "recipient_id", "expected_opportunity_revision", "expected_recipient_revision"],
        "properties": {
            "opportunity_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "recipient_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "expected_opportunity_revision": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64},
            "expected_recipient_revision": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64}
        }
    })
});

static MATCH_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["job_id", "role", "scope_kind", "scope_id", "state", "event_cursor"],
        "properties": {
            "job_id": {"type": "string"},
            "role": {"type": "string"},
            "scope_kind": {"type": "string"},
            "scope_id": {"type": "string"},
            "state": {"type": "string"},
            "event_cursor": {"type": "string"}
        }
    })
});

static COMPOSE_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["opportunity_id", "recipient_id", "expected_opportunity_revision", "expected_recipient_revision", "template_version_id", "exchange_version", "kind", "slot_fills"],
        "properties": {
            "opportunity_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "recipient_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "expected_opportunity_revision": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64},
            "expected_recipient_revision": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64},
            "template_version_id": {"type": "string", "minLength": 1, "maxLength": 200},
            "exchange_version": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64},
            "kind": {"type": "string", "enum": ["outreach", "followup", "handoff"]},
            "slot_fills": {
                "type": "array",
                "maxItems": 32,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["slot", "value"],
                    "properties": {
                        "slot": {"type": "string", "minLength": 1, "maxLength": 200},
                        "value": {"type": "string", "minLength": 1, "maxLength": 2000, "x-sensitive": true},
                        "resource_id": {"type": ["string", "null"], "maxLength": 200}
                    }
                }
            },
            "selected_match_ids": {
                "type": "array",
                "maxItems": 16,
                "items": {"type": "string", "maxLength": 200}
            }
        }
    })
});

static COMPOSE_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["job_id", "role", "scope_kind", "scope_id", "state", "event_cursor"],
        "properties": {
            "job_id": {"type": "string"},
            "role": {"type": "string"},
            "scope_kind": {"type": "string"},
            "scope_id": {"type": "string"},
            "state": {"type": "string"},
            "event_cursor": {"type": "string"}
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

static GATE_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["material_id", "version", "checks", "copy_enabled", "summary"],
        "properties": {
            "material_id": {"type": "string"},
            "version": {"type": "integer", "minimum": 1},
            "checks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["code", "severity", "pass"],
                    "properties": {
                        "code": {"type": "string"},
                        "severity": {"type": "string", "enum": ["hard", "soft"]},
                        "pass": {"type": "boolean"}
                    }
                }
            },
            "copy_enabled": {"type": "boolean"},
            "summary": {"type": "string"}
        }
    })
});

pub(crate) fn descriptors() -> Vec<&'static CommandDescriptor> {
    static MATCH: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: MATCH_START_ID,
        version: 1,
        title: "Start resource matching",
        summary: "Queue a scoped match against the granted opportunity and resources",
        description: "Starts a matcher job for the selected open opportunity and recipient. The job uses only ready resources inside the run's grant and returns a server-computed shortlist for owner selection. Nothing is sent, approved or released. The requested opportunity and recipient revisions must match the current records.",
        keywords: &["match", "resource", "shortlist", "writing"],
        effect: Effect::Write,
        requires_permission: Permission::MatchStart,
        surfaces: &[Surface::Agent],
        agent_tier: Some(2),
        args_schema: &MATCH_ARGS_SCHEMA,
        result_schema: &MATCH_RESULT_SCHEMA,
        supports_dry_run: false,
        preview_schema: None,
        when: None,
        palette: PaletteSpec {
            context_fields: &[],
            prompts: &[],
        },
    });
    static COMPOSE: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: COMPOSE_DRAFT_ID,
        version: 1,
        title: "Compose a material draft",
        summary: "Queue a reviewable draft from the active template and granted inputs",
        description: "Starts a composer job for the selected opportunity, recipient, active template version and attested exchange. Slot fills must match the deterministic template exactly and may use only ready resources inside the run's grant. The worker persists one review candidate; the owner reviews it in the output workspace. This command never approves, releases or sends anything.",
        keywords: &["compose", "draft", "material", "writing"],
        effect: Effect::Write,
        requires_permission: Permission::MaterialCompose,
        surfaces: &[Surface::Agent],
        agent_tier: Some(2),
        args_schema: &COMPOSE_ARGS_SCHEMA,
        result_schema: &COMPOSE_RESULT_SCHEMA,
        supports_dry_run: false,
        preview_schema: None,
        when: None,
        palette: PaletteSpec {
            context_fields: &[],
            prompts: &[],
        },
    });
    static GATE: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: GATE_CHECK_ID,
        version: 1,
        title: "Check material gate",
        summary: "Assess the deterministic release gates of one material version",
        description: "Evaluates the hard and soft content gates of one material version and returns bounded check codes, severities and a short summary. It never applies overrides, consumes an approval, or returns output bytes or provider text. The requested version must match the current head version.",
        keywords: &["material", "gate", "release", "check"],
        effect: Effect::Read,
        requires_permission: Permission::MaterialGate,
        surfaces: &[Surface::Agent],
        agent_tier: Some(2),
        args_schema: &GATE_ARGS_SCHEMA,
        result_schema: &GATE_RESULT_SCHEMA,
        supports_dry_run: false,
        preview_schema: None,
        when: None,
        palette: PaletteSpec {
            context_fields: &[],
            prompts: &[],
        },
    });
    vec![&MATCH, &COMPOSE, &GATE]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WritingCommand {
    Match,
    Compose,
}

impl WritingCommand {
    pub(crate) fn for_id(id: &str) -> Option<Self> {
        match id {
            MATCH_START_ID => Some(Self::Match),
            COMPOSE_DRAFT_ID => Some(Self::Compose),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchWireArgs {
    opportunity_id: String,
    recipient_id: String,
    expected_opportunity_revision: i64,
    expected_recipient_revision: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposeWireArgs {
    opportunity_id: String,
    recipient_id: String,
    expected_opportunity_revision: i64,
    expected_recipient_revision: i64,
    template_version_id: String,
    exchange_version: i64,
    kind: String,
    slot_fills: Vec<crate::application::compose_start::SlotFill>,
    #[serde(default)]
    selected_match_ids: Vec<String>,
}

#[derive(Debug)]
pub(crate) enum RunError {
    InvalidArgs,
    Match(MatchStartError),
    Compose(ComposeStartError),
}

pub(crate) async fn run(
    command: WritingCommand,
    tx: &mut Transaction<'_, Sqlite>,
    args: &Value,
    actor: &Actor,
) -> Result<super::RunOutcome, RunError> {
    match command {
        WritingCommand::Match => {
            let parsed: MatchWireArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
            let outcome = match_start(
                tx,
                &MatchStartInput {
                    opportunity_id: parsed.opportunity_id,
                    recipient_id: parsed.recipient_id,
                    expected_opportunity_revision: parsed.expected_opportunity_revision,
                    expected_recipient_revision: parsed.expected_recipient_revision,
                    epoch: actor.epoch.clone(),
                    run_id: actor.run_id.clone(),
                    owner_pseudonym: Some(actor.pseudonym.clone()),
                    granted_targets: actor.targets.clone(),
                },
            )
            .await
            .map_err(RunError::Match)?;
            Ok(super::RunOutcome {
                result: serde_json::to_value(&outcome).unwrap_or(Value::Null),
                effected: true,
            })
        }
        WritingCommand::Compose => {
            let parsed: ComposeWireArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
            let outcome = compose_start(
                tx,
                &ComposeStartInput {
                    opportunity_id: parsed.opportunity_id,
                    recipient_id: parsed.recipient_id,
                    expected_opportunity_revision: parsed.expected_opportunity_revision,
                    expected_recipient_revision: parsed.expected_recipient_revision,
                    kind: parsed.kind,
                    template_version_id: parsed.template_version_id,
                    exchange_version: parsed.exchange_version,
                    slot_fills: parsed.slot_fills,
                    selected_match_ids: parsed.selected_match_ids,
                    epoch: actor.epoch.clone(),
                    run_id: actor.run_id.clone(),
                    owner_pseudonym: Some(actor.pseudonym.clone()),
                    granted_targets: actor.targets.clone(),
                },
            )
            .await
            .map_err(RunError::Compose)?;
            Ok(super::RunOutcome {
                result: serde_json::to_value(&outcome).unwrap_or(Value::Null),
                effected: true,
            })
        }
    }
}

/// Serde/schema agreement for schema-valid samples.
pub(crate) fn parse_wire_args(command: WritingCommand, args: &Value) -> Result<(), String> {
    let ok = match command {
        WritingCommand::Match => serde_json::from_value::<MatchWireArgs>(args.clone()).is_ok(),
        WritingCommand::Compose => serde_json::from_value::<ComposeWireArgs>(args.clone()).is_ok(),
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

fn command_id(command: WritingCommand) -> &'static str {
    match command {
        WritingCommand::Match => MATCH_START_ID,
        WritingCommand::Compose => COMPOSE_DRAFT_ID,
    }
}

pub(crate) fn validation_samples() -> (Vec<super::SampleArgs>, Vec<super::SampleArgs>) {
    let slot_fills = json!([
        {"slot": "gift:item", "value": "A gift", "resource_id": "res_sample"},
        {"slot": "proof:item", "value": "A proof", "resource_id": "res_proof_sample"}
    ]);
    let valid = vec![
        super::SampleArgs {
            command: MATCH_START_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "recipient_id": "party_sample",
                "expected_opportunity_revision": 2,
                "expected_recipient_revision": 1
            }),
        },
        super::SampleArgs {
            command: COMPOSE_DRAFT_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "recipient_id": "party_sample",
                "expected_opportunity_revision": 2,
                "expected_recipient_revision": 1,
                "template_version_id": "template_sample",
                "exchange_version": 1,
                "kind": "outreach",
                "slot_fills": slot_fills,
                "selected_match_ids": ["res_sample"]
            }),
        },
    ];
    let invalid = vec![
        super::SampleArgs {
            command: MATCH_START_ID.to_string(),
            args: json!({"opportunity_id": "opp_sample", "expected_opportunity_revision": 1}),
        },
        super::SampleArgs {
            command: MATCH_START_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "recipient_id": "party_sample",
                "expected_opportunity_revision": 1,
                "expected_recipient_revision": 1,
                "extra": true
            }),
        },
        super::SampleArgs {
            command: COMPOSE_DRAFT_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "recipient_id": "party_sample",
                "expected_opportunity_revision": 2,
                "expected_recipient_revision": 1,
                "template_version_id": "template_sample",
                "exchange_version": 0,
                "kind": "outreach",
                "slot_fills": []
            }),
        },
        super::SampleArgs {
            command: COMPOSE_DRAFT_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "recipient_id": "party_sample",
                "expected_opportunity_revision": 2,
                "expected_recipient_revision": 1,
                "template_version_id": "template_sample",
                "exchange_version": 1,
                "kind": "letter",
                "slot_fills": [],
                "selected_match_ids": []
            }),
        },
    ];
    (valid, invalid)
}

pub(crate) fn args_allowlist() -> &'static [&'static str] {
    &[
        "opportunity_id",
        "recipient_id",
        "expected_opportunity_revision",
        "expected_recipient_revision",
        "template_version_id",
        "exchange_version",
        "kind",
        "slot_fills",
        "selected_match_ids",
        "material_id",
        "expected_version",
    ]
}

pub(crate) fn args_sensitive() -> &'static [&'static str] {
    &[
        "opportunity_id",
        "recipient_id",
        "slot_fills",
        "selected_match_ids",
        "material_id",
    ]
}
