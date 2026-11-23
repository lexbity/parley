//! U03 create command: `opportunity.create` (UCR-001 catalogue U03).
//!
//! The registry entry calls the same typed application operation as the REST
//! create route (`application::opportunity::create`). Compile-time
//! registration is local to this module; descriptors, closed schemas, sample
//! args and the audit allowlist live here.

use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

use super::descriptor::{CommandDescriptor, PaletteSpec, PromptSpec};
use super::policy::{Effect, Permission, Surface};
use crate::application::opportunity::{CreateError, CreateInput, create};

pub(crate) const CREATE_ID: &str = "opportunity.create";

pub(crate) static CREATE_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["title", "primary_party_id"],
        "properties": {
            "title": {"type": "string", "minLength": 1, "maxLength": 500},
            "primary_party_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true}
        }
    })
});

pub(crate) static CREATE_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["opportunity_id", "revision", "primary_party_id", "list_id", "position", "event_cursor"],
        "properties": {
            "opportunity_id": {"type": "string"},
            "revision": {"type": "integer", "minimum": 1},
            "primary_party_id": {"type": "string"},
            "list_id": {"type": "string"},
            "position": {"type": "integer"},
            "event_cursor": {"type": "string"}
        }
    })
});

const QUEUE_WHEN: &str = "route == \"queue\"";
const CONTEXT_FIELDS: &[&str] = &["route"];
const ARGS_ALLOWLIST: &[&str] = &["title", "primary_party_id"];
const ARGS_SENSITIVE: &[&str] = &["title", "primary_party_id"];

/// The only registry command whose effect requires the immutable
/// "created by this run" grant. The HTTP pipeline asks this before rechecking
/// the grant under reservation, so the check stays local to this module.
pub(crate) fn requires_self_created_grant(command_id: &str) -> bool {
    command_id == CREATE_ID
}

pub(crate) fn descriptors() -> Vec<&'static CommandDescriptor> {
    static CREATE: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: CREATE_ID,
        version: 1,
        title: "Create opportunity",
        summary: "Create an opportunity for a selected party",
        description: "Creates one open opportunity with the selected active party as its primary participant, places it in the first board list and emits the creation events. Use for new work only. It does not create outreach, materials or contact lifecycle facts. A delegated run needs an explicit create grant and, for self-created targets, the created-by-this-run grant.",
        keywords: &["queue", "new", "create", "lead"],
        effect: Effect::Write,
        requires_permission: Permission::OpportunityCreate,
        surfaces: &[Surface::Palette, Surface::Agent],
        agent_tier: Some(2),
        args_schema: &CREATE_ARGS_SCHEMA,
        result_schema: &CREATE_RESULT_SCHEMA,
        supports_dry_run: false,
        when: Some(QUEUE_WHEN),
        palette: PaletteSpec {
            context_fields: CONTEXT_FIELDS,
            prompts: &[
                PromptSpec {
                    field: "title",
                    picker: "text",
                    title: "Opportunity title",
                },
                PromptSpec {
                    field: "primary_party_id",
                    picker: "party",
                    title: "Primary party",
                },
            ],
        },
    });
    vec![&CREATE]
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateWireArgs {
    title: String,
    primary_party_id: String,
}

pub(crate) enum RunError {
    InvalidArgs,
    Create(CreateError),
}

/// Execute the typed create. `created_by_run` is set only for a delegated
/// caller whose durable grant includes the created-target permission; the
/// writer pipeline verifies that grant before this function is reached.
pub(crate) async fn run(
    tx: &mut Transaction<'_, Sqlite>,
    args: &Value,
    operation_key_digest: &str,
    created_by_run: Option<String>,
) -> Result<Value, RunError> {
    let parsed: CreateWireArgs =
        serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
    let result = create(
        tx,
        &CreateInput {
            title: parsed.title,
            primary_party_id: parsed.primary_party_id,
            operation_key_digest: operation_key_digest.to_string(),
            created_by_run,
        },
    )
    .await
    .map_err(RunError::Create)?;
    Ok(serde_json::to_value(&result).unwrap_or_default())
}

/// Serde/schema agreement: parse valid wire args through the typed struct.
pub(crate) fn parse_wire_args(args: &Value) -> Result<(), String> {
    serde_json::from_value::<CreateWireArgs>(args.clone())
        .map(|_| ())
        .map_err(|error| format!("serde rejected schema-valid args for {CREATE_ID}: {error}"))
}

pub(crate) fn validation_samples() -> (Vec<super::SampleArgs>, Vec<super::SampleArgs>) {
    let valid = vec![super::SampleArgs {
        command: CREATE_ID.to_string(),
        args: json!({"title": "Sample opportunity", "primary_party_id": "party_sample"}),
    }];
    let invalid = vec![
        super::SampleArgs {
            command: CREATE_ID.to_string(),
            args: json!({"title": "Sample opportunity"}),
        },
        super::SampleArgs {
            command: CREATE_ID.to_string(),
            args: json!({"title": 3, "primary_party_id": "party_sample"}),
        },
        super::SampleArgs {
            command: CREATE_ID.to_string(),
            args: json!({"title": "Sample", "primary_party_id": "party", "extra": true}),
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
