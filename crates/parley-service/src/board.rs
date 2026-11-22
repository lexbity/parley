//! Transactional board ordering and canonical Markdown notes.

use super::{
    AppState, Workspace, WorkspaceError, authenticate, digest, idempotency_key, new_id, now_utc,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use parley_domain::notes::{self, Task};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use std::collections::{BTreeMap, BTreeSet};

const RANK_STEP: i64 = 1024;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/board", get(get_board))
        .route("/api/v1/board/lists", post(create_list))
        .route("/api/v1/board/lists/reorder", post(reorder_lists))
        .route("/api/v1/board/lists/{list_id}", patch(rename_list))
        .route("/api/v1/board/lists/{list_id}/archive", post(archive_list))
        .route("/api/v1/opportunities/{id}/placement", post(move_card))
        .route(
            "/api/v1/opportunities/{id}/notes",
            get(get_notes).patch(update_notes),
        )
        .route(
            "/api/v1/opportunities/{id}/notes/tasks/toggle",
            post(toggle_task),
        )
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoardList {
    pub id: String,
    pub title: String,
    pub position: i64,
    pub archived: bool,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoardPlacement {
    pub opportunity_id: String,
    pub list_id: String,
    pub position: i64,
    pub revision: i64,
    pub next_task: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoardCard {
    pub opportunity: super::materials::Opportunity,
    pub placement: BoardPlacement,
}

#[derive(sqlx::FromRow)]
struct BoardPlacementRow {
    opportunity_id: String,
    list_id: String,
    position: i64,
    placement_revision: i64,
    next_task: Option<String>,
    title: String,
    state: String,
    opportunity_revision: i64,
    participants: String,
    sources: String,
    materials: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoardSnapshot {
    pub lists: Vec<BoardList>,
    pub cards: Vec<BoardCard>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpportunityNotes {
    pub opportunity_id: String,
    pub markdown: String,
    pub revision: i64,
    pub next_task: Option<String>,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CreateListInput {
    title: String,
    #[serde(default)]
    before_list_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RenameListInput {
    title: String,
    expected_revision: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ArchiveListInput {
    destination_list_id: String,
    expected_list_revision: i64,
    expected_destination_revision: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReorderListsInput {
    ordered_ids: Vec<String>,
    expected_revisions: BTreeMap<String, i64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MoveCardInput {
    list_id: String,
    before_opportunity_id: Option<String>,
    expected_source_list_revision: i64,
    expected_list_revision: i64,
    expected_placement_revision: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdateNotesInput {
    markdown: String,
    expected_revision: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ToggleTaskInput {
    notes_revision: i64,
    byte_start: usize,
    byte_end: usize,
    line_sha256: String,
    checked: bool,
}

#[derive(Debug)]
struct Replay {
    status: i64,
    body: Value,
}

impl Workspace {
    async fn board_snapshot(&self) -> Result<BoardSnapshot, WorkspaceError> {
        let mut tx = self.pool().begin().await?;
        let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT id,title,position,revision FROM board_list WHERE archived=0 ORDER BY position,id",
        )
        .fetch_all(&mut *tx)
        .await?;
        let lists: Vec<BoardList> = rows
            .into_iter()
            .map(|(id, title, position, revision)| BoardList {
                id,
                title,
                position,
                archived: false,
                revision,
            })
            .collect();
        let placements: Vec<BoardPlacementRow> = sqlx::query_as(
            "SELECT p.opportunity_id,p.list_id,p.position,p.revision AS placement_revision,o.notes_next_task AS next_task,o.title,o.state,o.revision AS opportunity_revision, \
             (SELECT json_group_array(party_id) FROM opportunity_party op WHERE op.opportunity_id=o.id) AS participants, \
             (SELECT json_group_array(source_variant_id) FROM source_opportunity_link sl WHERE sl.opportunity_id=o.id) AS sources, \
             (SELECT json_group_array(id) FROM material m WHERE m.opportunity_id=o.id) AS materials \
             FROM board_placement p JOIN opportunity o ON o.id=p.opportunity_id JOIN board_list l ON l.id=p.list_id WHERE l.archived=0 ORDER BY l.position,p.position,p.opportunity_id",
        )
        .fetch_all(&mut *tx)
        .await?;
        let mut cards = Vec::with_capacity(placements.len());
        for row in placements {
            let BoardPlacementRow {
                opportunity_id,
                list_id,
                position,
                placement_revision: revision,
                next_task,
                title,
                state,
                opportunity_revision: op_revision,
                participants,
                sources,
                materials,
            } = row;
            let opportunity = super::materials::Opportunity {
                id: opportunity_id.clone(),
                title,
                state,
                revision: op_revision,
                participant_ids: serde_json::from_str(&participants).unwrap_or_default(),
                source_variant_ids: serde_json::from_str(&sources).unwrap_or_default(),
                material_ids: serde_json::from_str(&materials).unwrap_or_default(),
            };
            cards.push(BoardCard {
                opportunity,
                placement: BoardPlacement {
                    opportunity_id,
                    list_id,
                    position,
                    revision,
                    next_task,
                },
            });
        }
        tx.commit().await?;
        Ok(BoardSnapshot { lists, cards })
    }

    async fn read_notes(&self, id: &str) -> Result<OpportunityNotes, WorkspaceError> {
        let row: Option<(String, i64, Option<String>)> = sqlx::query_as(
            "SELECT notes_markdown,notes_revision,notes_next_task FROM opportunity WHERE id=?",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        let Some((markdown, revision, next_task)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        Ok(OpportunityNotes {
            opportunity_id: id.to_owned(),
            tasks: notes::index(&markdown),
            markdown,
            revision,
            next_task,
        })
    }

    async fn update_notes(
        &self,
        id: &str,
        input: &UpdateNotesInput,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        let markdown = notes::canonicalize(&input.markdown);
        if markdown.len() > 1_000_000 {
            return Err(WorkspaceError::Invalid(
                "notes exceed the 1 MB limit".into(),
            ));
        }
        let body = json!({"markdown":markdown,"expected_revision":input.expected_revision});
        let canonical = canonical_digest("PATCH", &format!("/opportunities/{id}/notes"), &body);
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status":replay.status,"result":replay.body}));
        }
        let row: Option<(i64, String)> =
            sqlx::query_as("SELECT notes_revision,notes_markdown FROM opportunity WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((revision, current)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        if revision != input.expected_revision {
            return Err(WorkspaceError::StaleRevision);
        }
        let next = if current == markdown {
            revision
        } else {
            revision + 1
        };
        let tasks = notes::index(&markdown);
        let next_task = tasks
            .iter()
            .find(|task| !task.checked)
            .map(|task| task.text.clone());
        if next != revision {
            sqlx::query("UPDATE opportunity SET notes_markdown=?,notes_revision=?,notes_next_task=?,updated_at=? WHERE id=? AND notes_revision=?")
                .bind(&markdown).bind(next).bind(&next_task).bind(now_utc()).bind(id).bind(revision).execute(&mut *tx).await?;
            write_event(
                &mut tx,
                "opportunity_notes",
                id,
                next,
                "opportunity.notes_updated",
                json!({"notes_revision":next,"task_count":tasks.len()}),
            )
            .await?;
        }
        let result = notes_value(id, &markdown, next, next_task, tasks);
        receipt_store(&mut tx, key, &canonical, 200, &result).await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn toggle_task(
        &self,
        id: &str,
        input: &ToggleTaskInput,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        let body = serde_json::to_value(input)
            .map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
        let canonical = canonical_digest(
            "POST",
            &format!("/opportunities/{id}/notes/tasks/toggle"),
            &body,
        );
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status":replay.status,"result":replay.body}));
        }
        let row: Option<(i64, String)> =
            sqlx::query_as("SELECT notes_revision,notes_markdown FROM opportunity WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((revision, markdown)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        if revision != input.notes_revision {
            return Err(WorkspaceError::StaleRevision);
        }
        let changed = notes::toggle(
            &markdown,
            input.byte_start,
            input.byte_end,
            &input.line_sha256,
            input.checked,
        )
        .map_err(|message| WorkspaceError::Invalid(message.to_owned()))?;
        let next_revision = if changed.markdown == markdown {
            revision
        } else {
            revision + 1
        };
        let next_task = changed
            .tasks
            .iter()
            .find(|task| !task.checked)
            .map(|task| task.text.clone());
        if next_revision != revision {
            sqlx::query("UPDATE opportunity SET notes_markdown=?,notes_revision=?,notes_next_task=?,updated_at=? WHERE id=? AND notes_revision=?")
                .bind(&changed.markdown).bind(next_revision).bind(&next_task).bind(now_utc()).bind(id).bind(revision).execute(&mut *tx).await?;
            write_event(&mut tx, "opportunity_notes", id, next_revision, "opportunity.task_toggled", json!({"notes_revision":next_revision,"byte_start":input.byte_start,"byte_end":input.byte_end,"checked":input.checked})).await?;
        }
        let result = notes_value(
            id,
            &changed.markdown,
            next_revision,
            next_task,
            changed.tasks,
        );
        receipt_store(&mut tx, key, &canonical, 200, &result).await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn create_list(
        &self,
        input: &CreateListInput,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        let title = input.title.trim();
        if title.is_empty() || title.chars().count() > 100 {
            return Err(WorkspaceError::Invalid(
                "board list title must contain 1–100 characters".into(),
            ));
        }
        let canonical = canonical_digest("POST", "/board/lists", input);
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status":replay.status,"result":replay.body}));
        }
        let mut lists = active_lists(&mut tx).await?;
        let original_positions: BTreeMap<_, _> = lists
            .iter()
            .map(|list| (list.id.clone(), list.position))
            .collect();
        let index = match &input.before_list_id {
            Some(id) => lists
                .iter()
                .position(|list| &list.id == id)
                .ok_or(WorkspaceError::NotFound)?,
            None => lists.len(),
        };
        let id = new_id(title);
        sqlx::query(
            "INSERT INTO board_list(id,title,position,archived,revision) VALUES(?,?,0,0,1)",
        )
        .bind(&id)
        .bind(title)
        .execute(&mut *tx)
        .await?;
        lists.insert(
            index,
            BoardList {
                id: id.clone(),
                title: title.to_owned(),
                position: 0,
                archived: false,
                revision: 1,
            },
        );
        reorder_active_lists(&mut tx, &lists, Some(&id)).await?;
        for list in &lists {
            if list.id == id || original_positions.get(&list.id) == Some(&list.position) {
                continue;
            }
            let updated = list_by_id(&mut tx, &list.id).await?;
            write_event(
                &mut tx,
                "board_list",
                &updated.id,
                updated.revision,
                "board_list.order_changed",
                json!({"position":updated.position}),
            )
            .await?;
        }
        let list = list_by_id(&mut tx, &id).await?;
        write_event(
            &mut tx,
            "board_list",
            &id,
            list.revision,
            "board_list.created",
            json!({"title":title,"position":list.position}),
        )
        .await?;
        let result = json!({"list":list,"board_revision":1});
        receipt_store(&mut tx, key, &canonical, 201, &result).await?;
        tx.commit().await?;
        Ok(json!({"_replay_status":201,"result":result}))
    }

    async fn rename_list(
        &self,
        id: &str,
        input: &RenameListInput,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        let title = input.title.trim();
        if title.is_empty() || title.chars().count() > 100 {
            return Err(WorkspaceError::Invalid(
                "board list title must contain 1–100 characters".into(),
            ));
        }
        let canonical = canonical_digest("PATCH", &format!("/board/lists/{id}"), input);
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status":replay.status,"result":replay.body}));
        }
        let current: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM board_list WHERE id=? AND archived=0")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(revision) = current else {
            return Err(WorkspaceError::NotFound);
        };
        if revision != input.expected_revision {
            return Err(WorkspaceError::StaleRevision);
        }
        sqlx::query("UPDATE board_list SET title=?,revision=revision+1 WHERE id=? AND archived=0")
            .bind(title)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let list = list_by_id(&mut tx, id).await?;
        write_event(
            &mut tx,
            "board_list",
            id,
            list.revision,
            "board_list.renamed",
            json!({"title":title}),
        )
        .await?;
        let result = json!({"list":list});
        receipt_store(&mut tx, key, &canonical, 200, &result).await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn reorder_lists(
        &self,
        input: &ReorderListsInput,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        let canonical = canonical_digest("POST", "/board/lists/reorder", input);
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status":replay.status,"result":replay.body}));
        }
        let current = active_lists(&mut tx).await?;
        let current_ids: BTreeSet<_> = current.iter().map(|list| list.id.as_str()).collect();
        let requested_ids: BTreeSet<_> = input.ordered_ids.iter().map(String::as_str).collect();
        let revision_ids: BTreeSet<_> = input
            .expected_revisions
            .keys()
            .map(String::as_str)
            .collect();
        if current_ids.len() != requested_ids.len()
            || current_ids != requested_ids
            || current_ids != revision_ids
            || input.ordered_ids.len() != input.ordered_ids.iter().collect::<BTreeSet<_>>().len()
        {
            return Err(WorkspaceError::Invalid(
                "ordered_ids and expected_revisions must be the exact active-list set".into(),
            ));
        }
        for list in &current {
            if input.expected_revisions.get(&list.id) != Some(&list.revision) {
                return Err(WorkspaceError::StaleRevision);
            }
        }
        let reordered: Vec<BoardList> = input
            .ordered_ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let mut list = current.iter().find(|list| &list.id == id).unwrap().clone();
                list.position = (index as i64 + 1) * RANK_STEP;
                list
            })
            .collect();
        apply_list_order(&mut tx, &reordered, None).await?;
        let mut lists = Vec::with_capacity(reordered.len());
        for list in &reordered {
            let updated = list_by_id(&mut tx, &list.id).await?;
            write_event(
                &mut tx,
                "board_list",
                &updated.id,
                updated.revision,
                "board_list.reordered",
                json!({"position":updated.position}),
            )
            .await?;
            lists.push(updated);
        }
        let result = json!({"lists":lists});
        receipt_store(&mut tx, key, &canonical, 200, &result).await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn archive_list(
        &self,
        id: &str,
        input: &ArchiveListInput,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        if id == input.destination_list_id {
            return Err(WorkspaceError::Invalid(
                "archive destination must be another active list".into(),
            ));
        }
        let canonical = canonical_digest("POST", &format!("/board/lists/{id}/archive"), input);
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status":replay.status,"result":replay.body}));
        }
        let lists = active_lists(&mut tx).await?;
        if lists.len() <= 1 {
            return Err(WorkspaceError::Invalid(
                "the last active board list cannot be archived".into(),
            ));
        }
        let source = lists
            .iter()
            .find(|list| list.id == id)
            .ok_or(WorkspaceError::NotFound)?;
        let destination = lists
            .iter()
            .find(|list| list.id == input.destination_list_id)
            .ok_or(WorkspaceError::NotFound)?;
        if source.revision != input.expected_list_revision
            || destination.revision != input.expected_destination_revision
        {
            return Err(WorkspaceError::StaleRevision);
        }
        let moving: Vec<(String,i64,i64)> = sqlx::query_as("SELECT opportunity_id,position,revision FROM board_placement WHERE list_id=? ORDER BY position,opportunity_id")
            .bind(id).fetch_all(&mut *tx).await?;
        let target: Vec<(String,i64,i64)> = sqlx::query_as("SELECT opportunity_id,position,revision FROM board_placement WHERE list_id=? ORDER BY position,opportunity_id")
            .bind(&input.destination_list_id).fetch_all(&mut *tx).await?;
        let all_ids: Vec<String> = target
            .iter()
            .map(|row| row.0.clone())
            .chain(moving.iter().map(|row| row.0.clone()))
            .collect();
        for (index, opportunity_id) in all_ids.iter().enumerate() {
            sqlx::query("UPDATE board_placement SET position=? WHERE opportunity_id=?")
                .bind(-((index as i64) + 1))
                .bind(opportunity_id)
                .execute(&mut *tx)
                .await?;
        }
        let moving_ids: BTreeSet<_> = moving.iter().map(|row| row.0.as_str()).collect();
        let mut moved = Vec::with_capacity(moving.len());
        let mut reordered_existing = Vec::new();
        for (index, (opportunity_id, old_position, revision)) in
            target.iter().chain(moving.iter()).enumerate()
        {
            let position = (index as i64 + 1) * RANK_STEP;
            if moving_ids.contains(opportunity_id.as_str()) {
                sqlx::query("UPDATE board_placement SET list_id=?,position=?,revision=revision+1 WHERE opportunity_id=?")
                    .bind(&input.destination_list_id).bind(position).bind(opportunity_id).execute(&mut *tx).await?;
                moved.push(json!({"opportunity_id":opportunity_id,"position":position,"placement_revision":revision+1}));
                write_event(
                    &mut tx,
                    "board_placement",
                    opportunity_id,
                    revision + 1,
                    "board.card_moved",
                    json!({"from_list_id":id,"list_id":input.destination_list_id,"position":position}),
                )
                .await?;
            } else if *old_position != position {
                sqlx::query("UPDATE board_placement SET position=?,revision=revision+1 WHERE opportunity_id=?")
                    .bind(position).bind(opportunity_id).execute(&mut *tx).await?;
                reordered_existing.push((opportunity_id.clone(), position, revision + 1));
            } else {
                sqlx::query("UPDATE board_placement SET position=? WHERE opportunity_id=?")
                    .bind(position)
                    .bind(opportunity_id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        // A nonempty list is moved in the same transaction; archive guard is
        // therefore satisfied only after every membership has changed.
        sqlx::query(
            "UPDATE board_list SET archived=1,revision=revision+1 WHERE id=? AND archived=0",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE board_list SET revision=revision+1 WHERE id=? AND archived=0")
            .bind(&input.destination_list_id)
            .execute(&mut *tx)
            .await?;
        let archived_list = list_by_id_including_archived(&mut tx, id).await?;
        let destination_list = list_by_id(&mut tx, &input.destination_list_id).await?;
        for (opportunity_id, position, revision) in reordered_existing {
            write_event(
                &mut tx,
                "board_placement",
                &opportunity_id,
                revision,
                "board.card_reordered",
                json!({"list_id":input.destination_list_id,"position":position}),
            )
            .await?;
        }
        write_event(
            &mut tx,
            "board_list",
            id,
            archived_list.revision,
            "board_list.archived",
            json!({"destination_list_id":input.destination_list_id,"moved_count":moved.len()}),
        )
        .await?;
        write_event(
            &mut tx,
            "board_list",
            &destination_list.id,
            destination_list.revision,
            "board_list.received_archived_cards",
            json!({"source_list_id":id,"moved_count":moved.len()}),
        )
        .await?;
        let result = json!({"archived_list":archived_list,"destination_list":destination_list,"moved":moved});
        receipt_store(&mut tx, key, &canonical, 200, &result).await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn move_card(
        &self,
        id: &str,
        input: &MoveCardInput,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        let canonical = canonical_digest("POST", &format!("/opportunities/{id}/placement"), input);
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status":replay.status,"result":replay.body}));
        }
        let placement: Option<(String, i64)> =
            sqlx::query_as("SELECT list_id,revision FROM board_placement WHERE opportunity_id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((source_id, placement_revision)) = placement else {
            return Err(WorkspaceError::NotFound);
        };
        let source = list_by_id(&mut tx, &source_id).await?;
        let destination = list_by_id(&mut tx, &input.list_id).await?;
        if source.revision != input.expected_source_list_revision
            || destination.revision != input.expected_list_revision
            || placement_revision != input.expected_placement_revision
        {
            return Err(WorkspaceError::StaleRevision);
        }
        if let Some(anchor) = &input.before_opportunity_id {
            if anchor == id {
                return Err(WorkspaceError::Invalid(
                    "a card cannot be placed before itself".into(),
                ));
            }
            let anchor_list: Option<String> =
                sqlx::query_scalar("SELECT list_id FROM board_placement WHERE opportunity_id=?")
                    .bind(anchor)
                    .fetch_optional(&mut *tx)
                    .await?;
            if anchor_list.as_deref() != Some(input.list_id.as_str()) {
                return Err(WorkspaceError::Invalid(
                    "before_opportunity_id must be in the destination list".into(),
                ));
            }
        }
        let mut target: Vec<(String,i64,i64)> = sqlx::query_as("SELECT opportunity_id,position,revision FROM board_placement WHERE list_id=? AND opportunity_id<>? ORDER BY position,opportunity_id")
            .bind(&input.list_id).bind(id).fetch_all(&mut *tx).await?;
        let insert_index = match &input.before_opportunity_id {
            Some(anchor) => target
                .iter()
                .position(|row| &row.0 == anchor)
                .ok_or(WorkspaceError::NotFound)?,
            None => target.len(),
        };
        let left = insert_index
            .checked_sub(1)
            .map(|index| target[index].1)
            .unwrap_or(0);
        let right = target.get(insert_index).map(|row| row.1);
        let candidate = match right {
            Some(right) if right - left > 1 => left + (right - left) / 2,
            Some(_) => 0,
            None => left.saturating_add(RANK_STEP),
        };
        let mut reordered_placements = Vec::new();
        if candidate == 0 {
            // A rank gap was exhausted. Stage all destination rows at unique
            // negative keys, then assign canonical spaced positions atomically.
            for (index, (opportunity_id, _, _)) in target.iter().enumerate() {
                sqlx::query("UPDATE board_placement SET position=? WHERE opportunity_id=?")
                    .bind(-((index as i64) + 1))
                    .bind(opportunity_id)
                    .execute(&mut *tx)
                    .await?;
            }
            target.insert(insert_index, (id.to_owned(), 0, placement_revision));
            for (index, (opportunity_id, _, revision)) in target.iter_mut().enumerate() {
                let position = (index as i64 + 1) * RANK_STEP;
                if opportunity_id == id {
                    sqlx::query("UPDATE board_placement SET list_id=?,position=?,revision=revision+1 WHERE opportunity_id=?")
                        .bind(&input.list_id).bind(position).bind(id).execute(&mut *tx).await?;
                    *revision += 1;
                } else {
                    sqlx::query("UPDATE board_placement SET position=?,revision=revision+1 WHERE opportunity_id=?")
                        .bind(position).bind(opportunity_id.as_str()).execute(&mut *tx).await?;
                    *revision += 1;
                    reordered_placements.push((opportunity_id.clone(), position, *revision));
                }
            }
        } else {
            let temp_position = i64::MIN + placement_revision;
            sqlx::query("UPDATE board_placement SET position=? WHERE opportunity_id=?")
                .bind(temp_position)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE board_placement SET list_id=?,position=?,revision=revision+1 WHERE opportunity_id=?")
                .bind(&input.list_id).bind(candidate).bind(id).execute(&mut *tx).await?;
        }
        let touched = BTreeSet::from([source_id.clone(), input.list_id.clone()]);
        for list_id in touched.iter() {
            sqlx::query("UPDATE board_list SET revision=revision+1 WHERE id=? AND archived=0")
                .bind(list_id)
                .execute(&mut *tx)
                .await?;
        }
        let (list_id, position, revision): (String, i64, i64) = sqlx::query_as(
            "SELECT list_id,position,revision FROM board_placement WHERE opportunity_id=?",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        write_event(
            &mut tx,
            "board_placement",
            id,
            revision,
            "board.card_moved",
            json!({"from_list_id":source_id,"list_id":list_id,"position":position}),
        )
        .await?;
        for (opportunity_id, position, placement_revision) in reordered_placements {
            write_event(
                &mut tx,
                "board_placement",
                &opportunity_id,
                placement_revision,
                "board.card_reordered",
                json!({"list_id":input.list_id,"position":position}),
            )
            .await?;
        }
        let source_list = list_by_id(&mut tx, &source_id).await?;
        let destination_list = if input.list_id == source_id {
            source_list.clone()
        } else {
            list_by_id(&mut tx, &input.list_id).await?
        };
        write_event(
            &mut tx,
            "board_list",
            &source_list.id,
            source_list.revision,
            "board_list.order_changed",
            json!({"list_id":source_list.id}),
        )
        .await?;
        if destination_list.id != source_list.id {
            write_event(
                &mut tx,
                "board_list",
                &destination_list.id,
                destination_list.revision,
                "board_list.order_changed",
                json!({"list_id":destination_list.id}),
            )
            .await?;
        }
        let result = json!({"opportunity_id":id,"list_id":list_id,"position":position,"placement_revision":revision,"source_list_revision":source_list.revision,"destination_list_revision":destination_list.revision});
        receipt_store(&mut tx, key, &canonical, 200, &result).await?;
        tx.commit().await?;
        Ok(result)
    }
}

async fn get_board(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(filters): Query<super::materials::OpportunityFilters>,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return error.into_response();
    }
    let matching = match state.workspace.opportunity_summaries(&filters).await {
        Ok(rows) => rows.into_iter().map(|r| r.id).collect::<BTreeSet<_>>(),
        Err(error) => return board_error(error),
    };
    match state.workspace.board_snapshot().await {
        Ok(mut board) => {
            board
                .cards
                .retain(|card| matching.contains(&card.opportunity.id));
            (StatusCode::OK, Json(board)).into_response()
        }
        Err(error) => board_error(error),
    }
}

async fn create_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CreateListInput>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.create_list(&input, &key)).await
}

async fn rename_list(
    State(state): State<AppState>,
    Path(list_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<RenameListInput>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.rename_list(&list_id, &input, &key)).await
}

async fn archive_list(
    State(state): State<AppState>,
    Path(list_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ArchiveListInput>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.archive_list(&list_id, &input, &key)).await
}

async fn reorder_lists(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<ReorderListsInput>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.reorder_lists(&input, &key)).await
}

async fn move_card(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<MoveCardInput>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.move_card(&id, &input, &key)).await
}

async fn get_notes(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return error.into_response();
    }
    match state.workspace.read_notes(&id).await {
        Ok(notes) => (StatusCode::OK, Json(notes)).into_response(),
        Err(error) => board_error(error),
    }
}

async fn update_notes(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<UpdateNotesInput>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.update_notes(&id, &input, &key)).await
}

async fn toggle_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ToggleTaskInput>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.toggle_task(&id, &input, &key)).await
}

async fn command_key(state: &AppState, headers: &HeaderMap) -> Result<String, Box<Response>> {
    if let Err(error) = authenticate(headers, state).await {
        return Err(Box::new(error.into_response()));
    }
    idempotency_key(headers).map_err(|error| Box::new(key_error(error)))
}

async fn command_handler<F>(command: F) -> Response
where
    F: std::future::Future<Output = Result<Value, WorkspaceError>>,
{
    match command.await {
        Ok(value) => {
            let status = value
                .get("_replay_status")
                .and_then(Value::as_i64)
                .unwrap_or(StatusCode::OK.as_u16() as i64) as u16;
            let body = value.get("result").cloned().unwrap_or(value);
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            (status, Json(body)).into_response()
        }
        Err(error) => board_error(error),
    }
}

fn key_error(error: (StatusCode, &'static str, &'static str)) -> Response {
    (
        error.0,
        Json(json!({"error":{"code":error.1,"message":error.2,"details":[]}})),
    )
        .into_response()
}

fn board_error(error: WorkspaceError) -> Response {
    let (status, code, message) = match error {
        WorkspaceError::Database(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "WRITE_FAILED",
            "The workspace could not complete the command".to_owned(),
        ),
        WorkspaceError::Invalid(message) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "INVALID_REQUEST", message)
        }
        WorkspaceError::IdempotencyConflict => (
            StatusCode::CONFLICT,
            "KEY_REUSED",
            "Idempotency key was used for a different command".to_owned(),
        ),
        WorkspaceError::NotFound => (
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Resource does not exist".to_owned(),
        ),
        WorkspaceError::StaleRevision => (
            StatusCode::PRECONDITION_FAILED,
            "STALE_REVISION",
            "The addressed notes, placement, or list changed in another session".to_owned(),
        ),
    };
    (
        status,
        Json(json!({"error":{"code":code,"message":message,"details":[]}})),
    )
        .into_response()
}

fn canonical_digest(method: &str, target: &str, body: &impl Serialize) -> String {
    let body = serde_json::to_vec(body).unwrap_or_default();
    digest(&[method.as_bytes(), b"\n", target.as_bytes(), b"\n", &body].concat())
}

async fn receipt_lookup(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    canonical: &str,
) -> Result<Option<Replay>, WorkspaceError> {
    let row:Option<(String,i64,String)>=sqlx::query_as("SELECT canonical_digest,status_code,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=(SELECT epoch FROM workspace_meta WHERE singleton=1) AND key=?")
        .bind(key).fetch_optional(&mut **tx).await?;
    let Some((stored, status, body)) = row else {
        return Ok(None);
    };
    if stored != canonical {
        return Err(WorkspaceError::IdempotencyConflict);
    }
    let body: Value =
        serde_json::from_str(&body).map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
    Ok(Some(Replay { status, body }))
}

async fn receipt_store(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    canonical: &str,
    status: i64,
    result: &Value,
) -> Result<(), WorkspaceError> {
    sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,(SELECT epoch FROM workspace_meta WHERE singleton=1),?,?,?,?,(SELECT COALESCE(MAX(cursor),0) FROM event),?)")
        .bind(key).bind(canonical).bind(status).bind(result.to_string()).bind(now_utc()).execute(&mut **tx).await?;
    Ok(())
}

async fn write_event(
    tx: &mut Transaction<'_, Sqlite>,
    entity_type: &str,
    id: &str,
    revision: i64,
    kind: &str,
    payload: Value,
) -> Result<(), WorkspaceError> {
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,?,?,?,?,?)")
        .bind(new_id(id)).bind(now_utc()).bind(entity_type).bind(id).bind(revision).bind(kind).bind(payload.to_string()).execute(&mut **tx).await?;
    Ok(())
}

fn notes_value(
    id: &str,
    markdown: &str,
    revision: i64,
    next_task: Option<String>,
    tasks: Vec<Task>,
) -> Value {
    json!({"opportunity_id":id,"markdown":markdown,"revision":revision,"next_task":next_task,"tasks":tasks})
}

async fn active_lists(tx: &mut Transaction<'_, Sqlite>) -> Result<Vec<BoardList>, WorkspaceError> {
    let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT id,title,position,revision FROM board_list WHERE archived=0 ORDER BY position,id",
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, title, position, revision)| BoardList {
            id,
            title,
            position,
            archived: false,
            revision,
        })
        .collect())
}

async fn list_by_id(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<BoardList, WorkspaceError> {
    let row: Option<(String, String, i64, i64, bool)> = sqlx::query_as(
        "SELECT id,title,position,revision,archived FROM board_list WHERE id=? AND archived=0",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|(id, title, position, revision, archived)| BoardList {
        id,
        title,
        position,
        revision,
        archived,
    })
    .ok_or(WorkspaceError::NotFound)
}

async fn list_by_id_including_archived(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<BoardList, WorkspaceError> {
    let row: Option<(String, String, i64, i64, bool)> =
        sqlx::query_as("SELECT id,title,position,revision,archived FROM board_list WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    row.map(|(id, title, position, revision, archived)| BoardList {
        id,
        title,
        position,
        revision,
        archived,
    })
    .ok_or(WorkspaceError::NotFound)
}

async fn reorder_active_lists(
    tx: &mut Transaction<'_, Sqlite>,
    lists: &[BoardList],
    new_id: Option<&str>,
) -> Result<(), WorkspaceError> {
    apply_list_order(tx, lists, new_id).await
}

async fn apply_list_order(
    tx: &mut Transaction<'_, Sqlite>,
    lists: &[BoardList],
    new_id: Option<&str>,
) -> Result<(), WorkspaceError> {
    let mut old_positions = BTreeMap::new();
    for (index, list) in lists.iter().enumerate() {
        let old: i64 =
            sqlx::query_scalar("SELECT position FROM board_list WHERE id=? AND archived=0")
                .bind(&list.id)
                .fetch_one(&mut **tx)
                .await?;
        old_positions.insert(list.id.clone(), old);
        sqlx::query("UPDATE board_list SET position=? WHERE id=? AND archived=0")
            .bind(-((index as i64) + 1))
            .bind(&list.id)
            .execute(&mut **tx)
            .await?;
    }
    for (index, list) in lists.iter().enumerate() {
        let position = (index as i64 + 1) * RANK_STEP;
        let changed = old_positions.get(&list.id).copied() != Some(position);
        let increment = changed && new_id != Some(list.id.as_str());
        sqlx::query(
            "UPDATE board_list SET position=?,revision=revision+? WHERE id=? AND archived=0",
        )
        .bind(position)
        .bind(i64::from(increment))
        .bind(&list.id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}
