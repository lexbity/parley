//! U04 task command: `opportunity.set_task_done` (UCR-001 catalogue U04).
//!
//! The registry entry calls the same typed application operation as the REST
//! `POST /opportunities/{id}/notes/tasks/toggle` adapter. The args carry the
//! opportunity ID, the notes revision, the typed byte span, the line digest
//! and the desired checked state. The palette fills the task reference from
//! the authoritative task picker; the span and revision are frozen from the
//! selected record. A request whose desired state already holds is a true
//! no-op.

use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

use super::descriptor::{CommandDescriptor, PaletteSpec, PromptSpec};
use super::policy::{Effect, Permission, Surface};
use crate::application::opportunity_task::{TaskError, TaskInput, TaskOutcome, apply_task};

pub(crate) const TASK_DONE_ID: &str = "opportunity.set_task_done";

static TASK_ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["opportunity_id", "expected_notes_revision", "byte_start", "byte_end", "line_sha256", "checked"],
        "properties": {
            "opportunity_id": {"type": "string", "minLength": 1, "maxLength": 200, "x-sensitive": true},
            "expected_notes_revision": {"type": "integer", "minimum": 1, "maximum": 9007199254740991i64},
            "byte_start": {"type": "integer", "minimum": 0, "maximum": 9007199254740991i64},
            "byte_end": {"type": "integer", "minimum": 0, "maximum": 9007199254740991i64},
            "line_sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
            "checked": {"type": "boolean"}
        }
    })
});

static TASK_RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["opportunity_id", "notes_revision", "next_task", "task_count", "event_id", "event_cursor"],
        "properties": {
            "opportunity_id": {"type": "string"},
            "notes_revision": {"type": "integer", "minimum": 1},
            "next_task": {"type": ["string", "null"]},
            "task_count": {"type": "integer", "minimum": 0},
            "event_id": {"type": ["string", "null"]},
            "event_cursor": {"type": ["integer", "null"]}
        }
    })
});

const QUEUE_WHEN: &str = "route == \"queue\" && selection.opportunity";
const TASK_CONTEXT_FIELDS: &[&str] = &["route"];
const ARGS_ALLOWLIST: &[&str] = &[
    "opportunity_id",
    "expected_notes_revision",
    "byte_start",
    "byte_end",
    "line_sha256",
    "checked",
];
const ARGS_SENSITIVE: &[&str] = &["opportunity_id", "line_sha256"];

pub(crate) fn descriptors() -> Vec<&'static CommandDescriptor> {
    static TASK: LazyLock<CommandDescriptor> = LazyLock::new(|| CommandDescriptor {
        id: TASK_DONE_ID,
        version: 1,
        title: "Set task done",
        summary: "Mark one opportunity task checked or unchecked",
        description: "Changes the checked state of exactly one Markdown task inside the opportunity notes by its typed byte span and line digest. The expected notes revision is required; stale or malformed spans fail without an effect. Desired states that already hold are a no-op. Notes are not otherwise changed.",
        keywords: &["queue", "task", "notes", "check"],
        effect: Effect::Write,
        requires_permission: Permission::OpportunityTaskEdit,
        surfaces: &[Surface::Palette, Surface::Agent],
        agent_tier: Some(2),
        args_schema: &TASK_ARGS_SCHEMA,
        result_schema: &TASK_RESULT_SCHEMA,
        supports_dry_run: false,
        preview_schema: None,
        when: Some(QUEUE_WHEN),
        palette: PaletteSpec {
            context_fields: TASK_CONTEXT_FIELDS,
            prompts: &[PromptSpec {
                field: "line_sha256",
                picker: "opportunity_task",
                title: "Task",
            }],
        },
    });
    vec![&TASK]
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskWireArgs {
    opportunity_id: String,
    expected_notes_revision: i64,
    byte_start: usize,
    byte_end: usize,
    line_sha256: String,
    checked: bool,
}

pub(crate) enum RunError {
    InvalidArgs,
    Task(TaskError),
}

pub(crate) async fn run(
    tx: &mut Transaction<'_, Sqlite>,
    args: &Value,
    operation_key_digest: &str,
) -> Result<super::RunOutcome, RunError> {
    let parsed: TaskWireArgs =
        serde_json::from_value(args.clone()).map_err(|_| RunError::InvalidArgs)?;
    let outcome = apply_task(
        tx,
        &TaskInput {
            opportunity_id: parsed.opportunity_id,
            expected_notes_revision: parsed.expected_notes_revision,
            byte_start: parsed.byte_start,
            byte_end: parsed.byte_end,
            line_sha256: parsed.line_sha256,
            checked: parsed.checked,
            operation_key_digest: operation_key_digest.to_string(),
        },
    )
    .await
    .map_err(RunError::Task)?;
    match outcome {
        TaskOutcome::Changed(result) => Ok(super::RunOutcome {
            result: serde_json::to_value(&result).unwrap_or_default(),
            effected: true,
        }),
        TaskOutcome::NoOp(result) => Ok(super::RunOutcome {
            result: serde_json::to_value(&result).unwrap_or_default(),
            effected: false,
        }),
    }
}

/// Serde/schema agreement for schema-valid samples.
pub(crate) fn parse_wire_args(args: &Value) -> Result<(), String> {
    serde_json::from_value::<TaskWireArgs>(args.clone())
        .map(|_| ())
        .map_err(|error| format!("serde rejected schema-valid args for {TASK_DONE_ID}: {error}"))
}

pub(crate) fn validation_samples() -> (Vec<super::SampleArgs>, Vec<super::SampleArgs>) {
    let valid = vec![super::SampleArgs {
        command: TASK_DONE_ID.to_string(),
        args: json!({
            "opportunity_id": "opp_sample",
            "expected_notes_revision": 2,
            "byte_start": 0,
            "byte_end": 31,
            "line_sha256": "a".repeat(64),
            "checked": true
        }),
    }];
    let invalid = vec![
        super::SampleArgs {
            command: TASK_DONE_ID.to_string(),
            args: json!({"opportunity_id": "opp_sample", "expected_notes_revision": 2, "checked": true}),
        },
        super::SampleArgs {
            command: TASK_DONE_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "expected_notes_revision": 2,
                "byte_start": 0,
                "byte_end": 31,
                "line_sha256": "not-a-hash",
                "checked": true
            }),
        },
        super::SampleArgs {
            command: TASK_DONE_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "expected_notes_revision": 2,
                "byte_start": 0,
                "byte_end": 31,
                "line_sha256": "a".repeat(64),
                "checked": "yes"
            }),
        },
        super::SampleArgs {
            command: TASK_DONE_ID.to_string(),
            args: json!({
                "opportunity_id": "opp_sample",
                "expected_notes_revision": 2,
                "byte_start": 0,
                "byte_end": 31,
                "line_sha256": "a".repeat(64),
                "checked": true,
                "extra": 1
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
