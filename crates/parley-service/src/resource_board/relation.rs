use super::support::*;
use super::*;
use serde_json::json;

impl Workspace {
    /// Atomic resource card commit: create or edit the entity, bind/unbind
    /// staged attachments, change placement, and optionally add a graph-only
    /// relationship (link existing or create a related card), all in one
    /// writer transaction.
    pub async fn commit_resource_card(
        &self,
        input: &ResourceCardCommitInput,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        if input.create.is_none() && input.edit.is_none() {
            return Err(WorkspaceError::Invalid(
                "a commit must create or edit a card".into(),
            ));
        }
        if input.create.is_some() && input.edit.is_some() {
            return Err(WorkspaceError::Invalid(
                "create and edit are mutually exclusive".into(),
            ));
        }
        let canonical = canonical_digest("POST", "/resource-card-commits", input);
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status": replay.status, "result": replay.body}));
        }
        let (status, result) = if let Some(create) = &input.create {
            let id = self
                .create_entity_card(
                    &mut tx,
                    &create.title,
                    &create.subtype,
                    &create.body,
                    &create.notes_md,
                    "resource.created",
                )
                .await?;
            for upload_id in &create.staged_uploads {
                bind_upload_row(&mut tx, &id, upload_id).await?;
            }
            let related = if let Some(relation) = &input.relation_add {
                Some(self.apply_relation(&mut tx, &id, relation).await?)
            } else {
                None
            };
            if let Some(placement) = &create.placement {
                let applied = self
                    .apply_placement(&mut tx, &id, placement, None, None, None)
                    .await?;
                let _ = applied;
            }
            let entity = entity_card_json(&self.epoch, &mut tx, &id).await?;
            let placement = current_placement_json(&mut tx, &id).await?;
            let cursor = max_event_cursor(&mut tx).await?;
            let result = json!({
                "entity": entity,
                "placement": placement,
                "created_related": related,
                "event_cursor": cursor
            });
            receipt_store(&mut tx, key, &canonical, 201, &result).await?;
            (201, result)
        } else if let Some(edit) = &input.edit {
            let id = self.edit_entity_card(&mut tx, edit).await?;
            let related = if let Some(relation) = &input.relation_add {
                Some(self.apply_relation(&mut tx, &id, relation).await?)
            } else {
                None
            };
            if let Some(placement) = &input.placement {
                let applied = self
                    .apply_placement(
                        &mut tx,
                        &id,
                        placement,
                        input.expected_source_column_revision,
                        input.expected_destination_column_revision,
                        input.expected_placement_revision,
                    )
                    .await?;
                let _ = applied;
            }
            let entity = entity_card_json(&self.epoch, &mut tx, &id).await?;
            let placement = current_placement_json(&mut tx, &id).await?;
            let new_content_rev: i64 =
                sqlx::query_scalar("SELECT resource_content_revision FROM entity WHERE id=?")
                    .bind(&id)
                    .fetch_one(&mut *tx)
                    .await?;
            let affected: Vec<(String, i64)> = sqlx::query_as(
                "SELECT DISTINCT d.material_id,d.version_no FROM version_dependency d \
                 WHERE d.kind='resource' AND d.source_id=? AND d.source_revision <> ?",
            )
            .bind(&id)
            .bind(new_content_rev.to_string())
            .fetch_all(&mut *tx)
            .await?;
            let affected_json: Vec<Value> = affected
                .iter()
                .map(|(material_id, version_no)| {
                    json!({"material_id": material_id, "version_no": version_no})
                })
                .collect();
            for (material_id, version_no) in &affected {
                write_event(
                    &mut tx,
                    "material",
                    material_id,
                    *version_no,
                    "material.release_recheck_required",
                    json!({"resource_id": id, "version_no": version_no}),
                )
                .await?;
            }
            let result = json!({
                "entity": entity,
                "placement": placement,
                "created_related": related,
                "affected_versions": affected_json,
                "event_cursor": max_event_cursor(&mut tx).await?
            });
            receipt_store(&mut tx, key, &canonical, 200, &result).await?;
            (200, result)
        } else {
            unreachable!("create or edit is present")
        };
        tx.commit().await?;
        Ok(json!({"_replay_status": status, "result": result}))
    }

    async fn create_entity_card(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        title: &str,
        subtype: &str,
        body: &serde_json::Map<String, Value>,
        notes_md: &str,
        event_kind: &str,
    ) -> Result<String, WorkspaceError> {
        let title = validate_title(title)?;
        let subtype = validate_subtype(subtype)?;
        let notes = notes::canonicalize(notes_md);
        if notes.len() > MAX_NOTES_BYTES {
            return Err(WorkspaceError::Invalid(
                "notes exceed the 1 MB limit".into(),
            ));
        }
        let id = new_id(&digest(format!("{title}\0{subtype}\0{notes}").as_bytes()));
        sqlx::query(
            "INSERT INTO entity(id,kind,name,focus,notes_md,notes_rev,revision,created_at,updated_at,subtype,body_json,readiness,resource_content_revision,subtype_locked) \
             VALUES(?,?,?,'',?,?,1,?,?,?,?,'draft',1,0)",
        )
        .bind(&id)
        .bind("resource")
        .bind(&title)
        .bind(&notes)
        .bind(1)
        .bind(now_utc())
        .bind(now_utc())
        .bind(&subtype)
        .bind(serde_json::to_string(body).unwrap())
        .execute(&mut **tx)
        .await?;
        write_event(
            tx,
            "resource",
            &id,
            1,
            event_kind,
            json!({"subtype": subtype, "title": title}),
        )
        .await?;
        Ok(id)
    }

    async fn edit_entity_card(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        edit: &EditResourceCard,
    ) -> Result<String, WorkspaceError> {
        let row: Option<EditCardRow> = sqlx::query_as(
                "SELECT name,notes_md,subtype,body_json,readiness,revision,notes_rev,id,subtype_locked FROM entity WHERE id=? AND subtype<>'' AND readiness <> 'retired' AND retired_at IS NULL",
            )
            .bind(&edit.resource_id)
            .fetch_optional(&mut **tx)
            .await?;
        let Some((
            name,
            current_notes,
            subtype,
            body_json,
            _readiness,
            revision,
            notes_rev,
            id,
            subtype_locked,
        )) = row
        else {
            return Err(WorkspaceError::NotFound);
        };
        let changes_content = edit.title.is_some() || edit.subtype.is_some() || edit.body.is_some();
        let changes_files = !edit.staged_uploads.is_empty() || !edit.unbind_ids.is_empty();
        if (changes_content || changes_files || edit.notes_md.is_none())
            && revision != edit.expected_revision
        {
            return Err(WorkspaceError::StaleRevision);
        }
        if !changes_content && !changes_files && edit.notes_md.is_none() {
            return Ok(id);
        }
        let next_notes = match &edit.notes_md {
            Some(raw) => {
                if notes_rev != edit.expected_notes_rev {
                    return Err(WorkspaceError::StaleRevision);
                }
                let canonical = notes::canonicalize(raw);
                if canonical.len() > MAX_NOTES_BYTES {
                    return Err(WorkspaceError::Invalid(
                        "notes exceed the 1 MB limit".into(),
                    ));
                }
                canonical
            }
            None => current_notes,
        };
        let next_subtype = match &edit.subtype {
            Some(raw) => {
                let next = validate_subtype(raw)?;
                if next != subtype
                    && (subtype_locked == 1
                        || serde_json::from_str::<Value>(&body_json)
                            .unwrap_or(Value::Null)
                            .as_object()
                            .is_some_and(|body| !body.is_empty())
                        || _readiness == "ready")
                {
                    return Err(WorkspaceError::Invalid(
                        "A resource with typed data or ready status cannot change type.".into(),
                    ));
                }
                next
            }
            None => subtype,
        };
        let next_name = match &edit.title {
            Some(raw) => validate_title(raw)?,
            None => name,
        };
        let next_body = match &edit.body {
            Some(raw) => serde_json::to_string(raw).unwrap(),
            None => body_json,
        };
        let next_revision = revision + i64::from(changes_content || changes_files);
        let content_revision: i64 =
            sqlx::query_scalar("SELECT resource_content_revision FROM entity WHERE id=?")
                .bind(&id)
                .fetch_one(&mut **tx)
                .await?;
        let next_content_revision = content_revision + i64::from(changes_content);
        sqlx::query(
            "UPDATE entity SET name=?,notes_md=?,notes_rev=?,subtype=?,body_json=?,revision=?,resource_content_revision=?,updated_at=? WHERE id=?",
        )
        .bind(&next_name)
        .bind(&next_notes)
        .bind(if edit.notes_md.is_some() { notes_rev + 1 } else { notes_rev })
        .bind(&next_subtype)
        .bind(&next_body)
        .bind(next_revision)
        .bind(next_content_revision)
        .bind(now_utc())
        .bind(&id)
        .execute(&mut **tx)
        .await?;
        if changes_content {
            sqlx::query("UPDATE entity SET subtype_locked=1 WHERE id=?")
                .bind(&id)
                .execute(&mut **tx)
                .await?;
        }
        write_event(
            tx,
            "resource",
            &id,
            next_revision,
            "resource.updated",
            json!({"revision": next_revision}),
        )
        .await?;
        for upload_id in &edit.staged_uploads {
            bind_upload_row(tx, &id, upload_id).await?;
        }
        for attachment_id in &edit.unbind_ids {
            unbind_attachment_row(tx, &id, attachment_id).await?;
        }
        Ok(id)
    }

    /// Apply a relationship intent: link to an existing endpoint or create a
    /// related card (off board by default) and connect it.
    async fn apply_relation(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        from_id: &str,
        relation: &RelationAdd,
    ) -> Result<Value, WorkspaceError> {
        let verb = relation.verb.trim();
        if verb.is_empty() || verb.len() > MAX_VERB_BYTES {
            return Err(WorkspaceError::Invalid(
                "relationship verb must contain 1–120 bytes".into(),
            ));
        }
        let normalized_verb = verb.to_lowercase().replace('_', " ");
        let normalized_verb = normalized_verb
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if RESERVED_DELIVERY_VERBS.contains(&normalized_verb.as_str()) {
            return Err(WorkspaceError::Invalid(format!(
                "A relationship cannot assert a delivery ('{verb}'). Record the delivery \
                     through the opportunity's Mark sent workflow; delivery provenance is shown \
                     from recorded sends."
            )));
        }
        let basis = relation.basis.trim();
        if basis.is_empty() || basis.len() > MAX_BASIS_BYTES {
            return Err(WorkspaceError::Invalid(
                "relationship basis must contain 1–2000 bytes".into(),
            ));
        }
        let to_id = match (
            &relation.target_id,
            &relation.create,
            &relation.create_party,
        ) {
            (Some(target), None, None) => {
                let revision: Option<i64> =
                    sqlx::query_scalar("SELECT revision FROM graph_card WHERE id=?")
                        .bind(target)
                        .fetch_optional(&mut **tx)
                        .await?;
                let revision = revision.ok_or(WorkspaceError::NotFound)?;
                if relation
                    .expected_target_revision
                    .is_some_and(|expected| expected != revision)
                {
                    return Err(WorkspaceError::StaleRevision);
                }
                target.clone()
            }
            (None, Some(child), None) => {
                let id = self
                    .create_entity_card(
                        tx,
                        &child.title,
                        &child.subtype,
                        &child.body,
                        &child.notes_md,
                        "resource.created",
                    )
                    .await?;
                for upload_id in &child.staged_uploads {
                    bind_upload_row(tx, &id, upload_id).await?;
                }
                if let Some(placement) = &child.placement {
                    self.apply_placement(tx, &id, placement, None, None, None)
                        .await?;
                }
                id
            }
            (None, None, Some(party)) => {
                let name = validate_title(&party.name)?;
                let notes = notes::canonicalize(&party.notes_md);
                if !matches!(party.kind.as_str(), "person" | "org")
                    || party.focus.chars().count() > 2000
                    || notes.len() > MAX_NOTES_BYTES
                {
                    return Err(WorkspaceError::Invalid(
                        "Check the related party's type, focus, and notes.".into(),
                    ));
                }
                let id = new_id(&name);
                let now = now_utc();
                sqlx::query("INSERT INTO entity(id,kind,name,focus,notes_md,notes_rev,revision,created_at,updated_at) VALUES(?,?,?,?,?,1,1,?,?)")
                    .bind(&id).bind(&party.kind).bind(name).bind(&party.focus).bind(notes).bind(&now).bind(now)
                    .execute(&mut **tx).await?;
                write_event(
                    tx,
                    "entity",
                    &id,
                    1,
                    "entity.created",
                    json!({"kind": party.kind}),
                )
                .await?;
                id
            }
            _ => {
                return Err(WorkspaceError::Invalid(
                    "a relationship needs exactly one target or created card".into(),
                ));
            }
        };
        if to_id == from_id {
            return Err(WorkspaceError::Invalid(
                "an edge cannot connect a card to itself".into(),
            ));
        }
        let (source, target) = if relation.incoming {
            (to_id.as_str(), from_id)
        } else {
            (from_id, to_id.as_str())
        };
        let duplicate: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM edge WHERE state='active' AND from_id=? AND verb=? AND to_id=?",
        )
        .bind(source)
        .bind(verb)
        .bind(target)
        .fetch_optional(&mut **tx)
        .await?;
        if duplicate.is_some() {
            return Err(WorkspaceError::Invalid(
                "this relationship already exists".into(),
            ));
        }
        let edge_id = new_id(&digest(format!("{source}\0{verb}\0{target}").as_bytes()));
        sqlx::query(
            "INSERT INTO edge(id,from_id,verb,to_id,state,revision,basis,author_kind,created_at) \
             VALUES(?,?,?,?, 'active',1,?, 'owner',?)",
        )
        .bind(&edge_id)
        .bind(source)
        .bind(verb)
        .bind(target)
        .bind(basis)
        .bind(now_utc())
        .execute(&mut **tx)
        .await?;
        write_event(
            tx,
            "edge",
            &edge_id,
            1,
            "edge.created",
            json!({"from_id": source, "verb": verb, "to_id": target}),
        )
        .await?;
        Ok(json!({"edge_id": edge_id, "to_id": to_id}))
    }
}
