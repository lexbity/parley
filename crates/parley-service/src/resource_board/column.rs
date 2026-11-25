use super::support::*;
use super::*;
use serde_json::json;

impl Workspace {
    /// Append a list to the resource board.
    pub async fn create_resource_column(
        &self,
        input: &CreateResourceColumn,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        let title = input.title.trim();
        if !(1..=100).contains(&title.chars().count()) {
            return Err(WorkspaceError::Invalid(
                "Enter a list name of 1–100 characters.".into(),
            ));
        }
        let canonical = canonical_digest("POST", "/resource-board/columns", input);
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status": replay.status, "result": replay.body}));
        }
        let position: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(position),0)+? FROM resource_column")
                .bind(RANK_STEP)
                .fetch_one(&mut *tx)
                .await?;
        let id = new_id("resource-column");
        sqlx::query("INSERT INTO resource_column(id,title,position,kind) VALUES(?,?,?,'owner')")
            .bind(&id)
            .bind(title)
            .bind(position)
            .execute(&mut *tx)
            .await?;
        let column = ResourceColumn {
            id,
            title: title.to_owned(),
            position,
            revision: 1,
            kind: "owner".to_owned(),
        };
        let result = json!(&column);
        write_event(
            &mut tx,
            "resource_column",
            &column.id,
            1,
            "resource_column.created",
            result.clone(),
        )
        .await?;
        receipt_store(&mut tx, key, &canonical, 201, &result).await?;
        tx.commit().await?;
        Ok(json!({"_replay_status": 201, "result": result}))
    }

    /// Rename a list in place under its revision guard.
    pub async fn rename_resource_column(
        &self,
        id: &str,
        input: &RenameResourceColumn,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        let title = input.title.trim();
        if !(1..=100).contains(&title.chars().count()) {
            return Err(WorkspaceError::Invalid(
                "Enter a list name of 1–100 characters.".into(),
            ));
        }
        let canonical = canonical_digest("PATCH", &format!("/resource-board/columns/{id}"), input);
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status": replay.status, "result": replay.body}));
        }
        let revision: i64 = sqlx::query_scalar("SELECT revision FROM resource_column WHERE id=?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(WorkspaceError::NotFound)?;
        if revision != input.expected_revision {
            return Err(WorkspaceError::StaleRevision);
        }
        sqlx::query(
            "UPDATE resource_column SET title=?,revision=revision+1 WHERE id=? AND revision=?",
        )
        .bind(title)
        .bind(id)
        .bind(revision)
        .execute(&mut *tx)
        .await?;
        let column: ResourceColumn = sqlx::query_as(
            "SELECT id,title,position,revision,kind FROM resource_column WHERE id=?",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        write_event(
            &mut tx,
            "resource_column",
            id,
            column.revision,
            "resource_column.renamed",
            json!({"title": title}),
        )
        .await?;
        let result = json!(&column);
        receipt_store(&mut tx, key, &canonical, 200, &result).await?;
        tx.commit().await?;
        Ok(json!({"_replay_status": 200, "result": result}))
    }

    /// Remove a list atomically and retain its resources in the off-board picker.
    pub async fn remove_resource_column(
        &self,
        id: &str,
        input: &RemoveResourceColumn,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        let canonical = canonical_digest("DELETE", &format!("/resource-board/columns/{id}"), input);
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status": replay.status, "result": replay.body}));
        }
        let revision: i64 = sqlx::query_scalar("SELECT revision FROM resource_column WHERE id=?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(WorkspaceError::NotFound)?;
        if revision != input.expected_revision {
            return Err(WorkspaceError::StaleRevision);
        }
        let resource_ids: Vec<String> = sqlx::query_scalar(
            "SELECT resource_id FROM resource_placement WHERE column_id=? ORDER BY position,resource_id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        let off_board = ResourcePlacementInput {
            column_id: None,
            before_resource_id: None,
        };
        for resource_id in &resource_ids {
            self.apply_placement(&mut tx, resource_id, &off_board, None, None, None)
                .await?;
        }
        let last_revision: i64 =
            sqlx::query_scalar("DELETE FROM resource_column WHERE id=? RETURNING revision")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        let result = json!({"id": id, "off_board_count": resource_ids.len()});
        write_event(
            &mut tx,
            "resource_column",
            id,
            last_revision + 1,
            "resource_column.removed",
            result.clone(),
        )
        .await?;
        receipt_store(&mut tx, key, &canonical, 200, &result).await?;
        tx.commit().await?;
        Ok(result)
    }
}
