//! Typed opportunity notes and task operations (UCR-001 U04).
//!
//! `apply_task` is the single task-toggle operation behind the
//! `opportunity.set_task_done` registry command and the REST
//! `POST /opportunities/{id}/notes/tasks/toggle` adapter. `apply_notes_replace`
//! is the canonical Markdown replace behind the REST notes PATCH. Both run
//! inside the reserved transaction, use the pure `parley-domain` task
//! rewrite, emit typed events and share the same revision precondition. A
//! request whose desired state already holds is a true no-op: zero events and
//! zero revision change.

use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};

use crate::new_id;
use crate::now_utc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TaskInput {
    pub opportunity_id: String,
    pub expected_notes_revision: i64,
    pub byte_start: usize,
    pub byte_end: usize,
    pub line_sha256: String,
    pub checked: bool,
    /// Digest of the bounded operation key, stored on the emitted event
    /// instead of the raw key.
    pub operation_key_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct TaskResult {
    pub opportunity_id: String,
    pub notes_revision: i64,
    pub next_task: Option<String>,
    pub task_count: i64,
    pub event_id: Option<String>,
    pub event_cursor: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TaskError {
    NotFound,
    StaleRevision,
    /// A typed stale-span or malformed-span failure. The message is bounded
    /// and safe for transport; it never includes note text.
    InvalidSpan(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TaskOutcome {
    Changed(TaskResult),
    NoOp(TaskResult),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct NotesReplaceInput {
    pub opportunity_id: String,
    pub expected_revision: i64,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct NotesReplaceResult {
    pub opportunity_id: String,
    pub markdown: String,
    pub tasks: Vec<parley_domain::notes::Task>,
    pub revision: i64,
    pub next_task: Option<String>,
    pub event_id: Option<String>,
    pub event_cursor: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NotesReplaceError {
    NotFound,
    StaleRevision,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NotesReplaceOutcome {
    Changed(NotesReplaceResult),
    NoOp(NotesReplaceResult),
}

type NotesRowRecord = (String, i64);

async fn load_notes(
    tx: &mut Transaction<'_, Sqlite>,
    opportunity_id: &str,
    expected_revision: i64,
) -> Result<(String, i64), TaskError> {
    let row: Option<NotesRowRecord> =
        sqlx::query_as("SELECT notes_markdown,notes_revision FROM opportunity WHERE id=?")
            .bind(opportunity_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| TaskError::NotFound)?;
    let Some((markdown, revision)) = row else {
        return Err(TaskError::NotFound);
    };
    if revision != expected_revision {
        return Err(TaskError::StaleRevision);
    }
    Ok((markdown, revision))
}

fn next_task(tasks: &[parley_domain::notes::Task]) -> Option<String> {
    tasks
        .iter()
        .find(|task| !task.checked)
        .map(|task| task.text.clone())
}

#[allow(clippy::too_many_arguments)] // typed notes bookkeeping fields are all required
async fn persist_notes(
    tx: &mut Transaction<'_, Sqlite>,
    opportunity_id: &str,
    markdown: &str,
    old_revision: i64,
    next_revision: i64,
    next_task: &Option<String>,
    event_type: &str,
    payload: serde_json::Value,
) -> Result<i64, TaskError> {
    let updated = sqlx::query(
        "UPDATE opportunity SET notes_markdown=?,notes_revision=?,notes_next_task=?,updated_at=? WHERE id=? AND notes_revision=?",
    )
    .bind(markdown)
    .bind(next_revision)
    .bind(next_task)
    .bind(now_utc())
    .bind(opportunity_id)
    .bind(old_revision)
    .execute(&mut **tx)
    .await
    .map_err(|_| TaskError::NotFound)?;
    if updated.rows_affected() != 1 {
        return Err(TaskError::StaleRevision);
    }
    let event_id = new_id(opportunity_id);
    let cursor: i64 = sqlx::query_scalar(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'opportunity',?,?,?,?) RETURNING cursor",
    )
    .bind(&event_id)
    .bind(now_utc())
    .bind(opportunity_id)
    .bind(next_revision)
    .bind(event_type)
    .bind(payload.to_string())
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| TaskError::NotFound)?;
    Ok(cursor)
}

pub(crate) async fn apply_task(
    tx: &mut Transaction<'_, Sqlite>,
    input: &TaskInput,
) -> Result<TaskOutcome, TaskError> {
    let (markdown, revision) =
        load_notes(tx, &input.opportunity_id, input.expected_notes_revision).await?;
    let changed = parley_domain::notes::toggle(
        &markdown,
        input.byte_start,
        input.byte_end,
        &input.line_sha256,
        input.checked,
    )
    .map_err(|message| {
        let bounded: &'static str = match message {
            "task span is out of bounds or not on UTF-8 boundaries" => {
                "task span is out of bounds or not on UTF-8 boundaries"
            }
            "task span must address one complete LF line" => {
                "task span must address one complete LF line"
            }
            "task line changed; reload notes before toggling" => {
                "task line changed; reload notes before toggling"
            }
            "addressed Markdown line is not a task" => "addressed Markdown line is not a task",
            "addressed line is inside a fence, quote or code block and cannot be toggled" => {
                "addressed line is inside a fence, quote or code block and cannot be toggled"
            }
            "addressed Markdown line is not a list item" => {
                "addressed Markdown line is not a list item"
            }
            "task update produced invalid UTF-8" => "task update produced invalid UTF-8",
            _ => "task update failed",
        };
        TaskError::InvalidSpan(bounded)
    })?;

    let tasks = &changed.tasks;
    let task_count = tasks.len() as i64;
    let next = next_task(tasks);
    if changed.markdown == markdown {
        return Ok(TaskOutcome::NoOp(TaskResult {
            opportunity_id: input.opportunity_id.clone(),
            notes_revision: revision,
            next_task: next,
            task_count,
            event_id: None,
            event_cursor: None,
        }));
    }

    let next_revision = revision + 1;
    let cursor = persist_notes(
        tx,
        &input.opportunity_id,
        &changed.markdown,
        revision,
        next_revision,
        &next,
        "opportunity.task_toggled",
        serde_json::json!({
            "notes_revision": next_revision,
            "byte_start": input.byte_start,
            "byte_end": input.byte_end,
            "checked": input.checked,
            "operation_key_digest": input.operation_key_digest,
        }),
    )
    .await?;
    Ok(TaskOutcome::Changed(TaskResult {
        opportunity_id: input.opportunity_id.clone(),
        notes_revision: next_revision,
        next_task: next,
        task_count,
        event_id: None,
        event_cursor: Some(cursor),
    }))
}

pub(crate) async fn apply_notes_replace(
    tx: &mut Transaction<'_, Sqlite>,
    input: &NotesReplaceInput,
) -> Result<NotesReplaceOutcome, NotesReplaceError> {
    let markdown = parley_domain::notes::canonicalize(&input.markdown);
    if markdown.len() > 1_000_000 {
        return Err(NotesReplaceError::TooLarge);
    }
    let (current, revision) = load_notes(tx, &input.opportunity_id, input.expected_revision)
        .await
        .map_err(|error| match error {
            TaskError::NotFound => NotesReplaceError::NotFound,
            TaskError::StaleRevision => NotesReplaceError::StaleRevision,
            TaskError::InvalidSpan(_) => NotesReplaceError::NotFound,
        })?;
    let tasks = parley_domain::notes::index(&markdown);
    let task_count = tasks.len() as i64;
    let next = next_task(&tasks);
    if current == markdown {
        return Ok(NotesReplaceOutcome::NoOp(NotesReplaceResult {
            opportunity_id: input.opportunity_id.clone(),
            markdown,
            tasks,
            revision,
            next_task: next,
            event_id: None,
            event_cursor: None,
        }));
    }
    let next_revision = revision + 1;
    let cursor = persist_notes(
        tx,
        &input.opportunity_id,
        &markdown,
        revision,
        next_revision,
        &next,
        "opportunity.notes_updated",
        serde_json::json!({
            "notes_revision": next_revision,
            "task_count": task_count,
        }),
    )
    .await
    .map_err(|error| match error {
        TaskError::NotFound => NotesReplaceError::NotFound,
        TaskError::StaleRevision => NotesReplaceError::StaleRevision,
        TaskError::InvalidSpan(_) => NotesReplaceError::NotFound,
    })?;
    Ok(NotesReplaceOutcome::Changed(NotesReplaceResult {
        opportunity_id: input.opportunity_id.clone(),
        markdown,
        tasks,
        revision: next_revision,
        next_task: next,
        event_id: None,
        event_cursor: Some(cursor),
    }))
}
