use super::support::*;
use super::*;
use serde_json::json;

impl Workspace {
    /// Read the resource board: ordered columns, paged card summaries and,
    /// optionally, active off-board resources (resource picker).
    pub async fn resource_board_snapshot(
        &self,
        limit: Option<i64>,
        cursor: Option<String>,
        include_offboard: bool,
    ) -> Result<ResourceBoardSnapshot, WorkspaceError> {
        let limit = limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
        let mut read = self.pool().begin().await?;
        let columns: Vec<ResourceColumn> = sqlx::query_as(
            "SELECT id,title,position,revision,kind FROM resource_column ORDER BY position,id",
        )
        .fetch_all(&mut *read)
        .await?;
        let after = cursor.as_deref().map(decode_cursor).transpose()?;
        let mut builder = sqlx::QueryBuilder::new(
            "SELECT p.resource_id,e.subtype,e.name,e.readiness,e.body_json,p.column_id,p.position,p.revision,e.revision AS entity_rev,e.resource_content_revision AS content_rev,c.position AS col_pos, \
             (SELECT COUNT(*) FROM entity_attachment a WHERE a.entity_id=e.id) AS files, \
             ((SELECT COUNT(*) FROM edge x JOIN graph_card n ON n.id=x.to_id WHERE x.state='active' AND x.from_id=e.id)
               + (SELECT COUNT(*) FROM edge x JOIN graph_card n ON n.id=x.from_id WHERE x.state='active' AND x.to_id=e.id)) + (SELECT COUNT(*) FROM version_dependency d JOIN material m ON m.id=d.material_id AND m.head_version=d.version_no WHERE d.kind='resource' AND d.source_id=e.id) * 2 AS connections, \
             (SELECT COUNT(*) FROM delivered_resource_version d JOIN send_part sp ON sp.material_id=d.material_id AND sp.version_no=d.version_no \
               WHERE d.resource_id=e.id) AS deliveries \
             FROM resource_placement p JOIN entity e ON e.id=p.resource_id JOIN resource_column c ON c.id=p.column_id \
             WHERE e.readiness <> 'retired' AND e.retired_at IS NULL",
        );
        if let Some((col_pos, pos, id)) = after {
            builder
                .push(" AND (c.position,p.position,p.resource_id) > (")
                .push_bind(col_pos)
                .push(",")
                .push_bind(pos)
                .push(",")
                .push_bind(id)
                .push(")");
        }
        builder.push(" ORDER BY c.position,p.position,p.resource_id LIMIT ");
        builder.push_bind(limit + 1);
        let rows: Vec<CardRow> = builder.build_query_as().fetch_all(&mut *read).await?;
        let mut cards = Vec::with_capacity(rows.len());
        let mut next_cursor = None;
        for (index, row) in rows.into_iter().enumerate() {
            if index as i64 == limit {
                let last: &ResourceCardSummary = cards.last().expect("a positive page limit");
                let placement = last.placement.as_ref().expect("board card has placement");
                let column = columns
                    .iter()
                    .find(|column| column.id == placement.column_id)
                    .expect("valid resource column");
                next_cursor = Some(encode_cursor(
                    column.position,
                    placement.position,
                    last.resource_id.clone(),
                ));
                break;
            }
            let (
                resource_id,
                subtype,
                title,
                readiness,
                body_json,
                column_id,
                position,
                placement_rev,
                entity_rev,
                content_rev,
                _col_pos,
                files,
                connections,
                deliveries,
            ) = row;
            let body = serde_json::from_str::<Value>(&body_json).unwrap_or_else(|_| json!({}));
            let gaps = subtype_gaps(&subtype, &body);
            cards.push(ResourceCardSummary {
                resource_id: resource_id.clone(),
                subtype,
                title,
                readiness,
                revision: entity_rev,
                content_revision: content_rev,
                placement: Some(PlacementView {
                    column_id,
                    position,
                    revision: placement_rev,
                }),
                counts: ResourceCounts {
                    files,
                    connections: connections + deliveries,
                    proposed: 0,
                    deliveries,
                },
                gaps,
                etag: entity_etag(&self.epoch, &resource_id, entity_rev),
            });
        }
        let off_board = if include_offboard {
            self.off_board_resources(&mut read).await?
        } else {
            Vec::new()
        };
        let event_cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
            .fetch_one(&mut *read)
            .await?;
        read.commit().await?;
        Ok(ResourceBoardSnapshot {
            columns,
            cards,
            off_board,
            next_cursor,
            event_cursor,
        })
    }

    async fn off_board_resources(
        &self,
        read: &mut Transaction<'_, Sqlite>,
    ) -> Result<Vec<OffBoardResource>, WorkspaceError> {
        // Entity-kind resources without a placement.
        let rows: Vec<OffBoardRow> = sqlx::query_as(
            "SELECT e.id,e.subtype,e.name,e.readiness,e.body_json,e.revision,e.resource_content_revision, \
             (SELECT COUNT(*) FROM entity_attachment a WHERE a.entity_id=e.id), \
             ((SELECT COUNT(*) FROM edge x JOIN graph_card n ON n.id=x.to_id WHERE x.state='active' AND x.from_id=e.id)
               + (SELECT COUNT(*) FROM edge x JOIN graph_card n ON n.id=x.from_id WHERE x.state='active' AND x.to_id=e.id)) + (SELECT COUNT(*) FROM version_dependency d JOIN material m ON m.id=d.material_id AND m.head_version=d.version_no WHERE d.kind='resource' AND d.source_id=e.id) * 2, \
             (SELECT COUNT(*) FROM delivered_resource_version d JOIN send_part sp ON sp.material_id=d.material_id AND sp.version_no=d.version_no \
               WHERE d.resource_id=e.id) \
             FROM entity e WHERE e.subtype<>'' AND e.readiness <> 'retired' AND e.retired_at IS NULL \
             AND NOT EXISTS (SELECT 1 FROM resource_placement p WHERE p.resource_id=e.id) \
             ORDER BY e.name COLLATE NOCASE,e.id",
        )
        .fetch_all(&mut **read)
        .await?;
        let off = rows
            .into_iter()
            .map(
                |(
                    id,
                    subtype,
                    title,
                    readiness,
                    body_json,
                    revision,
                    content_revision,
                    files,
                    connections,
                    deliveries,
                )| {
                    let body =
                        serde_json::from_str::<Value>(&body_json).unwrap_or_else(|_| json!({}));
                    let gaps = subtype_gaps(&subtype, &body);
                    OffBoardResource {
                        resource_id: id.clone(),
                        subtype,
                        title,
                        readiness,
                        revision,
                        content_revision,
                        legacy: false,
                        counts: ResourceCounts {
                            files,
                            connections: connections + deliveries,
                            proposed: 0,
                            deliveries,
                        },
                        gaps,
                        etag: entity_etag(&self.epoch, &id, revision),
                    }
                },
            )
            .collect::<Vec<_>>();
        Ok(off)
    }

    /// Move a card between columns, reorder within one column, or remove it
    /// from the board. Order changes commit atomically with revision checks.
    pub async fn move_resource_card(
        &self,
        input: &MoveResourceCardInput,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        let canonical = canonical_digest("POST", "/resource-board/moves", input);
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status": replay.status, "result": replay.body}));
        }
        let placement = ResourcePlacementInput {
            column_id: input.destination_column_id.clone(),
            before_resource_id: input.before_resource_id.clone(),
        };
        let applied = self
            .apply_placement(
                &mut tx,
                &input.resource_id,
                &placement,
                input.expected_source_column_revision,
                input.expected_destination_column_revision,
                input.expected_placement_revision,
            )
            .await?;
        let result = json!({
            "placement": applied.view,
            "source_column_revision": applied.source_column_revision,
            "destination_column_revision": applied.destination_column_revision
        });
        receipt_store(&mut tx, key, &canonical, 200, &result).await?;
        tx.commit().await?;
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn place_confirmed_resource(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        resource_id: &str,
        column_id: &str,
    ) -> Result<(), WorkspaceError> {
        self.apply_placement(
            tx,
            resource_id,
            &ResourcePlacementInput {
                column_id: Some(column_id.to_owned()),
                before_resource_id: None,
            },
            None,
            None,
            None,
        )
        .await?;
        Ok(())
    }

    pub(super) async fn apply_placement(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        resource_id: &str,
        input: &ResourcePlacementInput,
        expected_source_column_revision: Option<i64>,
        expected_destination_column_revision: Option<i64>,
        expected_placement_revision: Option<i64>,
    ) -> Result<PlacementApply, WorkspaceError> {
        let current: Option<(String, i64, i64)> = sqlx::query_as(
            "SELECT column_id,position,revision FROM resource_placement WHERE resource_id=?",
        )
        .bind(resource_id)
        .fetch_optional(&mut **tx)
        .await?;
        let current_revision = current.as_ref().map_or(0, |(_, _, revision)| *revision);
        if expected_placement_revision.is_some_and(|expected| expected != current_revision) {
            return Err(WorkspaceError::StaleRevision);
        }
        let Some(destination_column_id) = &input.column_id else {
            if let Some((source_column, position, placement_rev)) = &current {
                if let Some(expected) = expected_source_column_revision {
                    let source_rev: i64 =
                        sqlx::query_scalar("SELECT revision FROM resource_column WHERE id=?")
                            .bind(source_column)
                            .fetch_one(&mut **tx)
                            .await?;
                    if source_rev != expected {
                        return Err(WorkspaceError::StaleRevision);
                    }
                }
                sqlx::query("DELETE FROM resource_placement WHERE resource_id=?")
                    .bind(resource_id)
                    .execute(&mut **tx)
                    .await?;
                sqlx::query("UPDATE resource_column SET revision=revision+1 WHERE id=?")
                    .bind(source_column)
                    .execute(&mut **tx)
                    .await?;
                write_event(
                    tx,
                    "resource",
                    resource_id,
                    *placement_rev + 1,
                    "resource.removed_from_board",
                    json!({"column_id": source_column, "position": position}),
                )
                .await?;
                let source_rev: i64 =
                    sqlx::query_scalar("SELECT revision FROM resource_column WHERE id=?")
                        .bind(source_column)
                        .fetch_one(&mut **tx)
                        .await?;
                return Ok(PlacementApply {
                    view: None,
                    source_column_revision: Some(source_rev),
                    destination_column_revision: None,
                });
            }
            return Ok(PlacementApply {
                view: None,
                source_column_revision: None,
                destination_column_revision: None,
            });
        };
        let destination_rev: i64 =
            sqlx::query_scalar("SELECT revision FROM resource_column WHERE id=?")
                .bind(destination_column_id)
                .fetch_optional(&mut **tx)
                .await?
                .ok_or_else(|| {
                    WorkspaceError::Invalid("Choose a column on the resource board.".into())
                })?;
        if let Some(expected) = expected_destination_column_revision
            && destination_rev != expected
        {
            return Err(WorkspaceError::StaleRevision);
        }
        if let Some((source_column, _, _)) = &current {
            if let Some(expected) = expected_source_column_revision {
                let source_rev: i64 =
                    sqlx::query_scalar("SELECT revision FROM resource_column WHERE id=?")
                        .bind(source_column)
                        .fetch_one(&mut **tx)
                        .await?;
                if source_rev != expected {
                    return Err(WorkspaceError::StaleRevision);
                }
            }
            if source_column == destination_column_id
                && input.before_resource_id.as_deref() == Some(resource_id)
            {
                return Err(WorkspaceError::Invalid(
                    "a card cannot be placed before itself".into(),
                ));
            }
            if input.before_resource_id.as_deref() == Some(resource_id) {
                return Err(WorkspaceError::Invalid(
                    "a card cannot be placed before itself".into(),
                ));
            }
        } else if input.before_resource_id.as_deref() == Some(resource_id) {
            return Err(WorkspaceError::Invalid(
                "a card cannot be placed before itself".into(),
            ));
        }
        if let Some(anchor) = &input.before_resource_id {
            let anchor_column: Option<String> =
                sqlx::query_scalar("SELECT column_id FROM resource_placement WHERE resource_id=?")
                    .bind(anchor)
                    .fetch_optional(&mut **tx)
                    .await?;
            if anchor_column.as_deref() != Some(destination_column_id.as_str()) {
                return Err(WorkspaceError::Invalid(
                    "before_resource_id must be in the destination column".into(),
                ));
            }
        }
        let mut target: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT resource_id,position,revision FROM resource_placement WHERE column_id=? AND resource_id<>? ORDER BY position,resource_id",
        )
        .bind(destination_column_id)
        .bind(resource_id)
        .fetch_all(&mut **tx)
        .await?;
        let insert_index = match &input.before_resource_id {
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
        let prior_placement_rev = current.as_ref().map(|row| row.2).unwrap_or(0);
        let mut reordered = Vec::new();
        if candidate == 0 {
            for (index, (id, _, _)) in target.iter().enumerate() {
                sqlx::query("UPDATE resource_placement SET position=? WHERE resource_id=?")
                    .bind(-((index as i64) + 1))
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
            target.insert(
                insert_index,
                (resource_id.to_owned(), 0, prior_placement_rev),
            );
            for (index, (id, _, revision)) in target.iter_mut().enumerate() {
                let position = (index as i64 + 1) * RANK_STEP;
                if id.as_str() == resource_id {
                    upsert_placement(
                        tx,
                        resource_id,
                        destination_column_id,
                        position,
                        prior_placement_rev,
                    )
                    .await?;
                } else {
                    sqlx::query(
                        "UPDATE resource_placement SET position=?,revision=revision+1 WHERE resource_id=?",
                    )
                    .bind(position)
                    .bind(&*id)
                    .execute(&mut **tx)
                    .await?;
                    reordered.push((id.clone(), position, *revision + 1));
                }
            }
        } else {
            let temp_position = i64::MIN + prior_placement_rev;
            if let Some((_, _, _)) = &current {
                sqlx::query("UPDATE resource_placement SET position=? WHERE resource_id=?")
                    .bind(temp_position)
                    .bind(resource_id)
                    .execute(&mut **tx)
                    .await?;
            }
            upsert_placement(
                tx,
                resource_id,
                destination_column_id,
                candidate,
                prior_placement_rev,
            )
            .await?;
        }
        let source_column_id = current.as_ref().map(|row| row.0.clone());
        let touched = BTreeSet::from([
            source_column_id.clone(),
            Some(destination_column_id.clone()),
        ]);
        for column_id in touched.iter().flatten() {
            sqlx::query("UPDATE resource_column SET revision=revision+1 WHERE id=?")
                .bind(column_id)
                .execute(&mut **tx)
                .await?;
        }
        let (column_id, position, revision): (String, i64, i64) = sqlx::query_as(
            "SELECT column_id,position,revision FROM resource_placement WHERE resource_id=?",
        )
        .bind(resource_id)
        .fetch_one(&mut **tx)
        .await?;
        write_event(
            tx,
            "resource",
            resource_id,
            revision,
            if current.is_some() { "resource.card_moved" } else { "resource.placed" },
            json!({"from_column_id": source_column_id, "column_id": column_id, "position": position}),
        )
        .await?;
        for (id, position, placement_rev) in reordered {
            write_event(
                tx,
                "resource",
                &id,
                placement_rev,
                "resource.card_reordered",
                json!({"column_id": column_id, "position": position}),
            )
            .await?;
        }
        let source_rev = match &source_column_id {
            Some(id) => Some(
                sqlx::query_scalar("SELECT revision FROM resource_column WHERE id=?")
                    .bind(id)
                    .fetch_one(&mut **tx)
                    .await?,
            ),
            None => None,
        };
        let destination_rev = if source_column_id.as_deref() == Some(column_id.as_str()) {
            source_rev
        } else {
            Some(
                sqlx::query_scalar("SELECT revision FROM resource_column WHERE id=?")
                    .bind(&column_id)
                    .fetch_one(&mut **tx)
                    .await?,
            )
        };
        Ok(PlacementApply {
            view: Some(PlacementView {
                column_id,
                position,
                revision,
            }),
            source_column_revision: source_rev,
            destination_column_revision: destination_rev,
        })
    }
}
