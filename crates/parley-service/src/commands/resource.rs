//! U06 resource commands: `resource.create_draft` and `resource.mark_ready`
//! (UCR-001 catalogue U06).
//!
//! Both call the same typed application operations as the REST adapters
//! (`application::resources`). Draft creation prompts title and subtype; the
//! readiness command is frozen from the selected resource card by the
//! palette front door, so its args are server-derived and it never prompts.

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

use super::descriptor::{CommandDescriptor, PaletteSpec, PromptSpec};
use super::policy::{Effect, Permission, Surface};
use crate::application::resource_retirement::{self, RetirementError, RetirementInput};
use crate::application::resources::{
    ResourceDraftError, ResourceDraftInput, ResourceReadyError, ResourceReadyInput, create_draft,
    mark_ready,
};

pub(crate) const CREATE_DRAFT_ID: &str = "resource.create_draft";
pub(crate) const MARK_READY_ID: &str = "resource.mark_ready";
pub(crate) const RETIRE_ID: &str = "resource.retire";

static CREATE_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["title", "subtype"],
        "properties": {
            "title": {"type": "string", "minLength": 1, "maxLength": 240},
            "subtype": {"type": "string", "enum": ["gift", "proof", "artifact", "skill", "interest", "note"]}
        }
    })
});
static CREATE_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "title", "subtype", "readiness", "revision", "content_revision", "event_id", "event_cursor"],
        "properties": {
            "id": {"type": "string"},
            "title": {"type": "string"},
            "subtype": {"type": "string"},
            "readiness": {"type": "string"},
            "revision": {"type": "integer", "minimum": 1},
            "content_revision": {"type": "integer", "minimum": 1},
            "event_id": {"type": "string"},
            "event_cursor": {"type": "integer", "minimum": 1}
        }
    })
});
static READY_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["resource_id", "expected_content_revision"],
        "properties": {
            "resource_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "expected_content_revision": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64}
        }
    })
});
static READY_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "title", "subtype", "readiness", "revision", "content_revision", "event_id", "event_cursor"],
        "properties": {
            "id": {"type": "string"},
            "title": {"type": "string"},
            "subtype": {"type": "string"},
            "readiness": {"type": "string"},
            "revision": {"type": "integer", "minimum": 1},
            "content_revision": {"type": "integer", "minimum": 1},
            "event_id": {"type": ["string", "null"]},
            "event_cursor": {"type": ["integer", "null"]}
        }
    })
});
static RETIRE_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["resource_id", "expected_card_revision", "expected_content_revision"],
        "properties": {
            "resource_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "expected_card_revision": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64},
            "expected_content_revision": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64}
        }
    })
});
static RETIRE_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["resource_id", "title", "readiness", "card_revision", "content_revision", "event_id", "event_cursor", "impacted_versions_count", "placement_removed"],
        "properties": {
            "resource_id": {"type": "string"},
            "title": {"type": "string"},
            "readiness": {"type": "string"},
            "card_revision": {"type": "integer", "minimum": 1},
            "content_revision": {"type": "integer", "minimum": 1},
            "event_id": {"type": "string"},
            "event_cursor": {"type": "integer", "minimum": 1},
            "impacted_versions_count": {"type": "integer", "minimum": 0},
            "placement_removed": {"type": "boolean"}
        }
    })
});
static RETIRE_PREVIEW_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["resource_id", "title", "readiness", "card_revision", "content_revision", "placement_removed", "impacted_versions_count", "impacted_versions_sample"],
        "properties": {
            "resource_id": {"type": "string"},
            "title": {"type": "string"},
            "readiness": {"type": "string"},
            "card_revision": {"type": "integer", "minimum": 1},
            "content_revision": {"type": "integer", "minimum": 1},
            "placement_removed": {"type": "boolean"},
            "impacted_versions_count": {"type": "integer", "minimum": 0},
            "impacted_versions_sample": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["material_id", "version_no", "dependency_revision"],
                    "properties": {
                        "material_id": {"type": "string"},
                        "version_no": {"type": "integer", "minimum": 1},
                        "dependency_revision": {"type": "string"}
                    }
                }
            }
        }
    })
});

const CREATE_WHEN: &str = "route == \"resources\"";
const READY_WHEN: &str = "route == \"resources\" && selection.resource";
const RETIRE_WHEN: &str = "route == \"resources\" && selection.resource";
const CREATE_CONTEXT_FIELDS: &[&str] = &["route"];
const READY_CONTEXT_FIELDS: &[&str] = &["route", "selection.resource"];
const RETIRE_CONTEXT_FIELDS: &[&str] = &["route", "selection.resource"];
const ARGS_ALLOWLIST: &[&str] = &[
    "title",
    "subtype",
    "resource_id",
    "expected_content_revision",
    "expected_card_revision",
];
const ARGS_SENSITIVE: &[&str] = &["resource_id"];

pub(crate) fn descriptors() -> Vec<&'static CommandDescriptor> {
    static CREATE: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: CREATE_DRAFT_ID,
        version: 1,
        title: "Create resource draft",
        summary: "Create a new draft resource with a subtype",
        description: "Creates a draft resource row with the given title and subtype. The draft stays in draft readiness until its typed fields are complete; full editing continues in the Resources editor. Does not place the card on the board.",
        keywords: &["resource", "draft", "create"],
        effect: Effect::Write,
        requires_permission: Permission::ResourceWrite,
        surfaces: &[Surface::Palette],
        agent_tier: None,
        args_schema: &CREATE_ARGS_SCHEMA,
        result_schema: &CREATE_RESULT_SCHEMA,
        supports_dry_run: false,
        preview_schema: None,
        when: Some(CREATE_WHEN),
        palette: PaletteSpec {
            context_fields: CREATE_CONTEXT_FIELDS,
            prompts: &[
                PromptSpec {
                    field: "title",
                    picker: "text",
                    title: "Title",
                },
                PromptSpec {
                    field: "subtype",
                    picker: "resource_subtype",
                    title: "Subtype",
                },
            ],
        },
    });
    static READY: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: MARK_READY_ID,
        version: 1,
        title: "Mark resource ready",
        summary: "Validate and ready a draft resource",
        description: "Validates the persisted typed fields of the selected resource. When every required field is present the resource becomes ready; otherwise the command reports the bounded missing fields without an effect. Requires the exact content revision of the selected card.",
        keywords: &["resource", "ready", "validate"],
        effect: Effect::Write,
        requires_permission: Permission::ResourceWrite,
        surfaces: &[Surface::Palette],
        agent_tier: None,
        args_schema: &READY_ARGS_SCHEMA,
        result_schema: &READY_RESULT_SCHEMA,
        supports_dry_run: false,
        preview_schema: None,
        when: Some(READY_WHEN),
        palette: PaletteSpec {
            context_fields: READY_CONTEXT_FIELDS,
            prompts: &[],
        },
    });
    static RETIRE: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: RETIRE_ID,
        version: 1,
        title: "Retire resource",
        summary: "Remove a resource from ordinary use after human approval",
        description: "Retires the selected resource: it leaves the board and pickers, and its current material versions are signalled for release recheck. Every destructive execution needs exact human review of a preview of this intent. The card and content revisions must match the current record; future edits and links to the retired card are rejected. Historical deliveries stay readable.",
        keywords: &["resource", "retire", "remove", "archive"],
        effect: Effect::Destructive,
        requires_permission: Permission::ResourceRetire,
        surfaces: &[Surface::Palette, Surface::Agent],
        agent_tier: Some(2),
        args_schema: &RETIRE_ARGS_SCHEMA,
        result_schema: &RETIRE_RESULT_SCHEMA,
        supports_dry_run: true,
        preview_schema: Some(&RETIRE_PREVIEW_SCHEMA),
        when: Some(RETIRE_WHEN),
        palette: PaletteSpec {
            context_fields: RETIRE_CONTEXT_FIELDS,
            prompts: &[],
        },
    });
    vec![&CREATE, &READY, &RETIRE]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceCommand {
    CreateDraft,
    MarkReady,
    Retire,
}

impl ResourceCommand {
    pub(crate) fn for_id(id: &str) -> Option<Self> {
        match id {
            CREATE_DRAFT_ID => Some(Self::CreateDraft),
            MARK_READY_ID => Some(Self::MarkReady),
            RETIRE_ID => Some(Self::Retire),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateDraftArgs {
    title: String,
    subtype: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkReadyArgs {
    resource_id: String,
    expected_content_revision: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RetireWireArgs {
    resource_id: String,
    expected_card_revision: i64,
    expected_content_revision: i64,
}

pub(crate) enum RunError {
    InvalidArgs,
    Draft(ResourceDraftError),
    Ready(ResourceReadyError),
    Retire(RetirementError),
}

pub(crate) async fn run(
    tx: &mut Transaction<'_, Sqlite>,
    command: ResourceCommand,
    args: &Value,
    operation_key_digest: &str,
) -> Result<super::RunOutcome, RunError> {
    match command {
        ResourceCommand::CreateDraft => {
            let parsed: CreateDraftArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
            let result = create_draft(
                tx,
                &ResourceDraftInput {
                    title: parsed.title,
                    subtype: parsed.subtype,
                    body: None,
                    operation_key_digest: operation_key_digest.to_string(),
                },
            )
            .await
            .map_err(RunError::Draft)?;
            Ok(super::RunOutcome {
                result: serde_json::to_value(&result).unwrap_or_default(),
                effected: true,
            })
        }
        ResourceCommand::MarkReady => {
            let parsed: MarkReadyArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
            let outcome = mark_ready(
                tx,
                &ResourceReadyInput {
                    id: parsed.resource_id,
                    expected_content_revision: parsed.expected_content_revision,
                    body_override: None,
                    operation_key_digest: operation_key_digest.to_string(),
                },
            )
            .await
            .map_err(RunError::Ready)?;
            match outcome {
                crate::application::resources::ResourceReadyOutcome::Changed(result) => {
                    Ok(super::RunOutcome {
                        result: serde_json::to_value(&result).unwrap_or_default(),
                        effected: true,
                    })
                }
                crate::application::resources::ResourceReadyOutcome::NoOp(result) => {
                    Ok(super::RunOutcome {
                        result: serde_json::to_value(&result).unwrap_or_default(),
                        effected: false,
                    })
                }
            }
        }
        ResourceCommand::Retire => {
            let parsed: RetireWireArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
            let outcome = resource_retirement::retire(
                tx,
                &RetirementInput {
                    id: parsed.resource_id,
                    expected_card_revision: parsed.expected_card_revision,
                    expected_content_revision: parsed.expected_content_revision,
                    operation_key_digest: operation_key_digest.to_string(),
                },
            )
            .await
            .map_err(RunError::Retire)?;
            Ok(super::RunOutcome {
                result: serde_json::json!({
                    "resource_id": outcome.resource_id,
                    "title": outcome.title,
                    "readiness": outcome.readiness,
                    "card_revision": outcome.card_revision,
                    "content_revision": outcome.content_revision,
                    "event_id": outcome.event_id,
                    "event_cursor": outcome.event_cursor,
                    "impacted_versions_count": outcome.impacted_versions_count,
                    "placement_removed": outcome.placement_removed,
                }),
                effected: true,
            })
        }
    }
}

/// The normalized typed args (with explicit defaults applied by serde),
/// used as the canonical approval binding digest (§4.2).
pub(crate) fn canonical_args(args: &Value) -> Option<Value> {
    let parsed: RetireWireArgs = serde_json::from_value(args.clone()).ok()?;
    serde_json::to_value(&parsed).ok()
}

/// Read-only destructive planning used by previews, dry runs and the
/// approval fingerprint (U08 / §5.3).
pub(crate) async fn plan(
    tx: &mut Transaction<'_, Sqlite>,
    command: ResourceCommand,
    args: &Value,
) -> Result<resource_retirement::RetirementPlan, RunError> {
    match command {
        ResourceCommand::Retire => {
            let parsed: RetireWireArgs =
                serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
            resource_retirement::plan_retirement(tx, &parsed.resource_id)
                .await
                .map_err(RunError::Retire)
        }
        _ => Err(RunError::InvalidArgs),
    }
}

/// The boot-validated preview schema for the destructive retire command.
pub(crate) fn retire_preview_schema() -> &'static Value {
    &RETIRE_PREVIEW_SCHEMA
}
pub(crate) fn parse_wire_args(command: ResourceCommand, args: &Value) -> Result<(), String> {
    let ok = match command {
        ResourceCommand::CreateDraft => {
            serde_json::from_value::<CreateDraftArgs>(args.clone()).is_ok()
        }
        ResourceCommand::MarkReady => serde_json::from_value::<MarkReadyArgs>(args.clone()).is_ok(),
        ResourceCommand::Retire => serde_json::from_value::<RetireWireArgs>(args.clone()).is_ok(),
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

fn command_id(command: ResourceCommand) -> &'static str {
    match command {
        ResourceCommand::CreateDraft => CREATE_DRAFT_ID,
        ResourceCommand::MarkReady => MARK_READY_ID,
        ResourceCommand::Retire => RETIRE_ID,
    }
}

pub(crate) fn validation_samples() -> (Vec<super::SampleArgs>, Vec<super::SampleArgs>) {
    let valid = vec![
        super::SampleArgs {
            command: CREATE_DRAFT_ID.to_string(),
            args: json!({"title": "Draft sample", "subtype": "gift"}),
        },
        super::SampleArgs {
            command: MARK_READY_ID.to_string(),
            args: json!({"resource_id": "res_sample", "expected_content_revision": 3}),
        },
        super::SampleArgs {
            command: RETIRE_ID.to_string(),
            args: json!({
                "resource_id": "res_sample",
                "expected_card_revision": 4,
                "expected_content_revision": 3
            }),
        },
    ];
    let invalid = vec![
        super::SampleArgs {
            command: CREATE_DRAFT_ID.to_string(),
            args: json!({"title": "", "subtype": "gift"}),
        },
        super::SampleArgs {
            command: CREATE_DRAFT_ID.to_string(),
            args: json!({"title": "Draft sample", "subtype": "robot"}),
        },
        super::SampleArgs {
            command: CREATE_DRAFT_ID.to_string(),
            args: json!({"title": "Draft sample", "subtype": "gift", "extra": 1}),
        },
        super::SampleArgs {
            command: MARK_READY_ID.to_string(),
            args: json!({"resource_id": "res_sample"}),
        },
        super::SampleArgs {
            command: MARK_READY_ID.to_string(),
            args: json!({"resource_id": "res_sample", "expected_content_revision": 0}),
        },
        super::SampleArgs {
            command: RETIRE_ID.to_string(),
            args: json!({"resource_id": "res_sample", "expected_card_revision": 1}),
        },
        super::SampleArgs {
            command: RETIRE_ID.to_string(),
            args: json!({
                "resource_id": "res_sample",
                "expected_card_revision": 0,
                "expected_content_revision": 1
            }),
        },
        super::SampleArgs {
            command: RETIRE_ID.to_string(),
            args: json!({
                "resource_id": "res_sample",
                "expected_card_revision": 1,
                "expected_content_revision": 1,
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
