use crate::*;

impl Workspace {
    /// Idempotent first-launch resource write. Resource, revision, event and
    /// receipt are committed under one SQLite writer reservation. The legacy
    /// `resource` table is gone; the entity row is the record (U06).
    pub async fn create_resource(
        &self,
        request: CreateResource,
    ) -> Result<ResourceRecord, WorkspaceError> {
        if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 200 {
            return Err(WorkspaceError::Invalid(
                "idempotency key must contain 1–200 bytes".into(),
            ));
        }
        if !matches!(request.kind.as_str(), "gift" | "proof" | "artifact") {
            return Err(WorkspaceError::Invalid(
                "kind must be gift, proof, or artifact".into(),
            ));
        }
        if request.title.chars().count() > 240 {
            return Err(WorkspaceError::Invalid(
                "title exceeds 240 characters".into(),
            ));
        }
        if !request.body.is_object() {
            return Err(WorkspaceError::Invalid(
                "resource body must be an object".into(),
            ));
        }
        let body = serde_json::to_string(&request.body)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let canonical = digest(format!("{}\0{}\0{}", request.kind, request.title, body).as_bytes());
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        )
        .bind(&self.epoch)
        .bind(&request.idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            if old_digest != canonical {
                return Err(WorkspaceError::IdempotencyConflict);
            }
            let record =
                serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            tx.commit().await?;
            return Ok(record);
        }
        let now = now_utc();
        let id = new_id(&canonical);
        sqlx::query("INSERT INTO entity(id,kind,name,focus,notes_md,notes_rev,revision,created_at,updated_at,subtype,body_json,readiness,resource_content_revision,subtype_locked) VALUES(?,?,?,'',?,1,1,?,?,?,?,'draft',1,0)")
            .bind(&id).bind("resource").bind(&request.title).bind("").bind(&now).bind(&now).bind(&request.kind).bind(&body).execute(&mut *tx).await?;
        sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        let event_id = new_id(&id);
        let result = ResourceRecord {
            id: id.clone(),
            kind: request.kind,
            title: request.title,
            body: request.body,
            readiness: "draft".into(),
            revision: 1,
        };
        let result_json =
            serde_json::to_string(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let cursor: i64 = sqlx::query_scalar("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'resource',?,1,'resource.created',?) RETURNING cursor")
            .bind(event_id).bind(&now).bind(&id).bind(format!("{{\"kind\":{:?}}}", result.kind)).fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,201,?,?,?)")
            .bind(&self.epoch).bind(&request.idempotency_key).bind(canonical).bind(&result_json).bind(cursor).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Conditional draft edit. The receipt is bound to the addressed record,
    /// the presented ETag and the typed body; a stale precondition fails
    /// without consuming the key so the corrected command can reuse it. The
    /// entity row is the record; content edits advance the separate content
    /// revision and flag affected material versions (U06).
    pub async fn update_resource(
        &self,
        idempotency_key: &str,
        id: &str,
        if_match: &str,
        title: &str,
        body: &serde_json::Value,
    ) -> Result<ResourceRecord, WorkspaceError> {
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 200 {
            return Err(WorkspaceError::Invalid(
                "idempotency key must contain 1–200 bytes".into(),
            ));
        }
        if title.chars().count() > 240 {
            return Err(WorkspaceError::Invalid(
                "title exceeds 240 characters".into(),
            ));
        }
        if !body.is_object() {
            return Err(WorkspaceError::Invalid(
                "resource body must be an object".into(),
            ));
        }
        let body_json =
            serde_json::to_string(body).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let canonical = digest(format!("{id}\0{if_match}\0{title}\0{body_json}").as_bytes());
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        )
        .bind(&self.epoch)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            if old_digest != canonical {
                return Err(WorkspaceError::IdempotencyConflict);
            }
            let record =
                serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            tx.commit().await?;
            return Ok(record);
        }
        let row = sqlx::query_as::<_, (String, String, i64, i64, i64)>(
            "SELECT subtype,readiness,revision,resource_content_revision,subtype_locked FROM entity WHERE id=? AND subtype<>'' AND readiness <> 'retired' AND retired_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((kind, readiness, revision, content_revision, _subtype_locked)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        if if_match != resource_etag(&self.epoch, id, revision) {
            return Err(WorkspaceError::StaleRevision);
        }
        let next_revision = revision + 1;
        let next_content_revision = content_revision + 1;
        let now = now_utc();
        sqlx::query("UPDATE entity SET name=?,body_json=?,revision=?,resource_content_revision=?,updated_at=? WHERE id=? AND revision=?")
            .bind(title)
            .bind(&body_json)
            .bind(next_revision)
            .bind(next_content_revision)
            .bind(&now)
            .bind(id)
            .bind(revision)
            .execute(&mut *tx)
            .await?;
        if readiness != "ready" {
            sqlx::query("UPDATE entity SET subtype_locked=1 WHERE id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        let event_id = new_id(id);
        let updated = ResourceRecord {
            id: id.to_owned(),
            kind,
            title: title.to_owned(),
            body: body.clone(),
            readiness,
            revision: next_revision,
        };
        let result_json =
            serde_json::to_string(&updated).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,'resource',?,?,'resource.updated','{}')")
            .bind(event_id).bind(&now).bind(id).bind(next_revision).execute(&mut *tx).await?;
        let affected: Vec<(String, i64)> = sqlx::query_as(
            "SELECT DISTINCT d.material_id,d.version_no FROM version_dependency d \
             WHERE d.kind='resource' AND d.source_id=? AND d.source_revision <> ?",
        )
        .bind(id)
        .bind(next_content_revision.to_string())
        .fetch_all(&mut *tx)
        .await?;
        for (material_id, version_no) in &affected {
            let event_id = new_id(id);
            sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,'material',?,?,'material.release_recheck_required',?)")
                .bind(event_id).bind(&now).bind(material_id).bind(version_no).bind(format!("{{\"resource_id\":{id:?},\"version_no\":{version_no}}}")).execute(&mut *tx).await?;
        }
        let cursor: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,200,?,?,?)")
            .bind(&self.epoch).bind(idempotency_key).bind(canonical).bind(&result_json).bind(cursor).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Saved owner resources for directory reads, optionally narrowed to
    /// resource kinds. Ordered most recently updated first with a stable ID
    /// tiebreaker. Reads the canonical entity rows (U06).
    pub async fn list_resources_filtered(
        &self,
        kinds: &[&str],
    ) -> Result<Vec<ResourceRecord>, WorkspaceError> {
        let mut builder = sqlx::QueryBuilder::new(
            "SELECT id,subtype,name,body_json,readiness,revision FROM entity WHERE subtype<>'' AND readiness <> 'retired' AND retired_at IS NULL",
        );
        if !kinds.is_empty() {
            builder.push(" AND subtype IN (");
            let mut separated = builder.separated(", ");
            for kind in kinds {
                match *kind {
                    "resource" => {
                        separated.push_bind("gift");
                        separated.push_bind("proof");
                        separated.push_bind("artifact");
                    }
                    other => {
                        separated.push_bind(other);
                    }
                };
            }
            builder.push(")");
        }
        builder.push(" ORDER BY updated_at DESC,id ASC");
        let rows = builder
            .build_query_as::<(String, String, String, String, String, i64)>()
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|(id, kind, title, body, readiness, revision)| {
                Ok(ResourceRecord {
                    id,
                    kind,
                    title,
                    body: serde_json::from_str(&body)
                        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?,
                    readiness,
                    revision,
                })
            })
            .collect()
    }

    pub async fn get_resource(&self, id: &str) -> Result<Option<ResourceRecord>, WorkspaceError> {
        let row = sqlx::query_as::<_, (String,String,String,String,String,i64)>("SELECT id,subtype,name,body_json,readiness,revision FROM entity WHERE id=? AND subtype<>'' AND readiness <> 'retired' AND retired_at IS NULL")
            .bind(id).fetch_optional(&self.pool).await?;
        row.map(|(id, kind, title, body, readiness, revision)| {
            Ok(ResourceRecord {
                id,
                kind,
                title,
                body: serde_json::from_str(&body)
                    .map_err(|e| WorkspaceError::Invalid(e.to_string()))?,
                readiness,
                revision,
            })
        })
        .transpose()
    }

    /// Directory entities, optionally narrowed to entity kinds. Ordered by
    /// normalized name with a stable ID tiebreaker.
    pub async fn list_entities_filtered(
        &self,
        kinds: &[&str],
    ) -> Result<Vec<EntityRecord>, WorkspaceError> {
        let mut builder =
            sqlx::QueryBuilder::new("SELECT id,kind,name,focus,revision FROM graph_card");
        if !kinds.is_empty() {
            builder.push(" WHERE kind IN (");
            let mut separated = builder.separated(", ");
            for kind in kinds {
                separated.push_bind(*kind);
            }
            builder.push(")");
        }
        builder.push(" ORDER BY name COLLATE NOCASE,id");
        let rows = builder
            .build_query_as::<(String, String, String, String, i64)>()
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|(id, kind, name, focus, revision)| EntityRecord {
                id,
                kind,
                name,
                focus,
                revision,
            })
            .collect())
    }

    /// One directory entity by ID (person, organization, project or resource
    /// kind entity). None when the ID is unknown.
    pub async fn get_entity(&self, id: &str) -> Result<Option<EntityRecord>, WorkspaceError> {
        let row = sqlx::query_as::<_, (String, String, String, String, i64)>(
            "SELECT id,kind,name,focus,revision FROM entity WHERE id=?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id, kind, name, focus, revision)| EntityRecord {
            id,
            kind,
            name,
            focus,
            revision,
        }))
    }

    /// Conditional typed party edit. `name` and `focus` are the editable sheet
    /// fields; the strong entity ETag is the write scope. Blank names and
    /// missing update fields are rejected; subtype transitions stay out of
    /// ordinary label editing. The mutation rules live in the shared typed
    /// operation `application::party_edit`; this adapter keeps the REST
    /// idempotency envelope and its published error mapping.
    pub async fn update_entity(
        &self,
        idempotency_key: &str,
        id: &str,
        if_match: &str,
        name: Option<String>,
        focus: Option<String>,
    ) -> Result<EntityRecord, WorkspaceError> {
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 200 {
            return Err(WorkspaceError::Invalid(
                "idempotency key must contain 1–200 bytes".into(),
            ));
        }
        if !is_strong_etag(if_match) {
            return Err(WorkspaceError::Invalid(
                "If-Match must be a strong entity ETag".into(),
            ));
        }
        if name.is_none() && focus.is_none() {
            return Err(WorkspaceError::Invalid("no entity fields to update".into()));
        }
        let canonical = digest(format!("{id}\0{if_match}\0{name:?}\0{focus:?}").as_bytes());
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        )
        .bind(&self.epoch)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            if old_digest != canonical {
                return Err(WorkspaceError::IdempotencyConflict);
            }
            let record =
                serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            tx.commit().await?;
            return Ok(record);
        }
        let row = sqlx::query_as::<_, (String, String, String, i64)>(
            "SELECT kind,name,focus,revision FROM graph_card WHERE id=?",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((kind, current_name, current_focus, revision)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        if if_match != entity_etag(&self.epoch, id, revision) {
            return Err(WorkspaceError::StaleRevision);
        }
        let resource: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM entity WHERE id=? AND subtype<>'')")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        if resource {
            let title_changed = name
                .as_deref()
                .map(|provided| provided.trim() != current_name)
                .unwrap_or(false);
            let next_name = match name {
                Some(name) => {
                    let trimmed = name.trim();
                    if trimmed.is_empty() || trimmed.len() > 500 {
                        return Err(WorkspaceError::Invalid(
                            "entity name must contain 1–500 bytes".into(),
                        ));
                    }
                    trimmed.to_owned()
                }
                None => current_name,
            };
            let next_focus = match focus {
                Some(focus) => {
                    if focus.chars().count() > 250 {
                        return Err(WorkspaceError::Invalid(
                            "focus exceeds 250 characters".into(),
                        ));
                    }
                    focus
                }
                None => current_focus,
            };
            let next_revision = revision + 1;
            let now = now_utc();
            if next_name.chars().count() > 240 {
                return Err(WorkspaceError::Invalid(
                    "Resource title exceeds 240 characters.".into(),
                ));
            }
            let next_content_revision: i64 =
                sqlx::query_scalar("SELECT resource_content_revision FROM entity WHERE id=?")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
            let next_content = next_content_revision + i64::from(title_changed);
            sqlx::query(
                "UPDATE entity SET name=?,focus=?,revision=?,resource_content_revision=?,updated_at=? WHERE id=?",
            )
            .bind(&next_name)
            .bind(&next_focus)
            .bind(next_revision)
            .bind(next_content)
            .bind(&now)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
                .execute(&mut *tx)
                .await?;
            let updated = EntityRecord {
                id: id.to_owned(),
                kind,
                name: next_name,
                focus: next_focus,
                revision: next_revision,
            };
            let result_json = serde_json::to_string(&updated)
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,'entity',?,?,'entity.updated','{}')")
                .bind(new_id(id)).bind(&now).bind(id).bind(next_revision).execute(&mut *tx).await?;
            if title_changed {
                let affected: Vec<(String, i64)> = sqlx::query_as(
                    "SELECT DISTINCT d.material_id,d.version_no FROM version_dependency d \
                     WHERE d.kind='resource' AND d.source_id=? AND d.source_revision <> ?",
                )
                .bind(id)
                .bind(next_content.to_string())
                .fetch_all(&mut *tx)
                .await?;
                for (material_id, version_no) in &affected {
                    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,'material',?,?,'material.release_recheck_required',?)")
                        .bind(new_id(id)).bind(&now).bind(material_id).bind(version_no).bind(format!("{{\"resource_id\":{id:?},\"version_no\":{version_no}}}")).execute(&mut *tx).await?;
                }
            }
            let cursor: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
                .fetch_one(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,200,?,?,?)")
                .bind(&self.epoch).bind(idempotency_key).bind(canonical).bind(&result_json).bind(cursor).bind(now).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(updated);
        }

        // Shared typed party edit: the operation owns the revision guard, the
        // field bounds and the event emission.
        let outcome = crate::application::party_edit::apply_party_edit(
            &mut tx,
            &crate::application::party_edit::PartyEditInput {
                party_id: id.to_owned(),
                expected_revision: revision,
                name,
                focus,
                operation_key_digest: crate::digest(
                    format!("rest-party\0{idempotency_key}\0{id}").as_bytes(),
                ),
            },
        )
        .await
        .map_err(|error| match error {
            crate::application::party_edit::PartyEditError::NotFound => WorkspaceError::NotFound,
            crate::application::party_edit::PartyEditError::StaleRevision => {
                WorkspaceError::StaleRevision
            }
            crate::application::party_edit::PartyEditError::InvalidName => {
                WorkspaceError::Invalid("entity name must contain 1–500 bytes".into())
            }
            crate::application::party_edit::PartyEditError::InvalidFocus => {
                WorkspaceError::Invalid("focus exceeds 250 characters".into())
            }
            crate::application::party_edit::PartyEditError::NoFields => {
                WorkspaceError::Invalid("no entity fields to update".into())
            }
        })?;
        let updated = match outcome {
            crate::application::party_edit::PartyEditOutcome::Changed(result)
            | crate::application::party_edit::PartyEditOutcome::NoOp(result) => EntityRecord {
                id: result.id,
                kind: result.kind,
                name: result.name,
                focus: result.focus,
                revision: result.revision,
            },
        };
        let result_json =
            serde_json::to_string(&updated).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let cursor: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,200,?,?,?)")
            .bind(&self.epoch).bind(idempotency_key).bind(canonical).bind(&result_json).bind(cursor).bind(now_utc()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Canonical entity notes (UTF-8, LF line endings) with their independent
    /// revision. Returns `{entity_id, markdown, revision}` when the entity
    /// exists (notes default to empty at revision 1).
    pub async fn get_entity_notes(&self, id: &str) -> Result<Option<EntityNotes>, WorkspaceError> {
        let row =
            sqlx::query_as::<_, (String, i64)>("SELECT notes_md,notes_rev FROM entity WHERE id=?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(markdown, revision)| EntityNotes {
            entity_id: id.to_owned(),
            markdown,
            revision,
        }))
    }

    /// Replace canonical entity notes under the notes-scoped strong ETag and
    /// the body's expected revision. A stale notes revision fails without
    /// consuming the idempotency key.
    pub async fn update_entity_notes(
        &self,
        idempotency_key: &str,
        id: &str,
        if_match: &str,
        expected_notes_rev: i64,
        markdown: &str,
    ) -> Result<EntityNotes, WorkspaceError> {
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 200 {
            return Err(WorkspaceError::Invalid(
                "idempotency key must contain 1–200 bytes".into(),
            ));
        }
        if !is_strong_etag(if_match) {
            return Err(WorkspaceError::Invalid(
                "If-Match must be a strong notes ETag".into(),
            ));
        }
        if markdown.len() > 512 * 1024 {
            return Err(WorkspaceError::Invalid("notes exceed 512 KiB".into()));
        }
        let canonical_markdown = canonical_markdown(markdown);
        let canonical = digest(
            format!("{id}\0{if_match}\0{expected_notes_rev}\0{canonical_markdown}").as_bytes(),
        );
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        )
        .bind(&self.epoch)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            if old_digest != canonical {
                return Err(WorkspaceError::IdempotencyConflict);
            }
            let notes =
                serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            tx.commit().await?;
            return Ok(notes);
        }
        let row = sqlx::query_as::<_, (i64,)>(
            "SELECT notes_rev FROM entity WHERE id=? AND retired_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((current_rev,)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        if expected_notes_rev != current_rev || if_match != notes_etag(&self.epoch, id, current_rev)
        {
            return Err(WorkspaceError::StaleRevision);
        }
        let next_rev = current_rev + 1;
        let now = now_utc();
        sqlx::query("UPDATE entity SET notes_md=?,notes_rev=?,updated_at=? WHERE id=?")
            .bind(&canonical_markdown)
            .bind(next_rev)
            .bind(&now)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        let notes = EntityNotes {
            entity_id: id.to_owned(),
            markdown: canonical_markdown,
            revision: next_rev,
        };
        let result_json =
            serde_json::to_string(&notes).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,'entity',?,?,'entity.notes_updated','{}')")
            .bind(new_id(id)).bind(&now).bind(id).bind(next_rev).execute(&mut *tx).await?;
        let cursor: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,200,?,?,?)")
            .bind(&self.epoch).bind(idempotency_key).bind(canonical).bind(&result_json).bind(cursor).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(notes)
    }

    /// Inspector dossier for one party: direct attributed links, files bound
    /// to the card, open opportunities involving the party, and resources used
    /// by visible opportunities, each bounded and deduplicated by ID.
    pub async fn entity_dossier(&self, id: &str) -> Result<Option<EntityDossier>, WorkspaceError> {
        let entity = match self.get_entity(id).await? {
            Some(entity) => entity,
            None => return Ok(None),
        };
        let direct_links=sqlx::query_as::<_,DossierLink>("SELECT eg.id,eg.from_id,eg.to_id,eg.verb,eg.revision,cr.tier,cr.wording,cr.claim_id,cr.revision claim_revision,cr.speaker_kind,cr.speaker_entity_id,cr.occurrence_id
            FROM edge eg LEFT JOIN edge_evidence ee ON ee.rowid=(SELECT latest.rowid FROM edge_evidence latest WHERE latest.edge_id=eg.id ORDER BY latest.rowid DESC LIMIT 1)
            LEFT JOIN claim c ON c.id=ee.claim_id LEFT JOIN claim_revision cr ON cr.claim_id=c.id AND cr.revision=c.current_revision
            WHERE eg.state='active' AND (eg.from_id=? OR eg.to_id=?) ORDER BY eg.created_at DESC,eg.id LIMIT 60")
            .bind(id).bind(id).fetch_all(&self.pool).await?;
        let files = sqlx::query_as::<_, (String, String, String, i64, String, i64, i64, String)>(
            "SELECT a.id,a.filename,a.content_type,a.size,a.sha256,u.share_permission,u.reviewed_by_owner,a.created_at \
             FROM entity_attachment a JOIN blob_upload u ON u.id=a.upload_id \
             WHERE a.entity_id=? ORDER BY a.created_at DESC LIMIT 60",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let files = files
            .into_iter()
            .map(
                |(
                    file_id,
                    filename,
                    content_type,
                    size,
                    sha256,
                    share_permission,
                    reviewed_by_owner,
                    created_at,
                )| {
                    DossierFile {
                        id: file_id,
                        filename,
                        content_type,
                        size,
                        sha256,
                        share_permission: share_permission != 0,
                        reviewed_by_owner: reviewed_by_owner != 0,
                        created_at,
                    }
                },
            )
            .collect();
        let opportunities = sqlx::query_as::<_, (String, String, String)>(
            "SELECT o.id,o.title,op.role FROM opportunity o \
             JOIN opportunity_party op ON op.opportunity_id=o.id \
             WHERE op.party_id=? AND o.state='open' \
             ORDER BY o.updated_at DESC LIMIT 60",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let opportunities = opportunities
            .into_iter()
            .map(|(opportunity_id, title, role)| DossierOpportunity {
                id: opportunity_id,
                title,
                role,
            })
            .collect();
        let paths=sqlx::query_as::<_,(String,String,String,String,String,String)>(
            "WITH paths AS (SELECT e.id,e.name AS title,e.subtype AS kind,'edge' AS via,eg.id AS hop,eg.verb AS label FROM edge eg JOIN entity e ON e.id=CASE WHEN eg.from_id=? THEN eg.to_id ELSE eg.from_id END WHERE (eg.from_id=? OR eg.to_id=?) AND eg.state='active' AND e.subtype<>'' AND e.readiness<>'retired' AND e.retired_at IS NULL
            UNION SELECT e.id,e.name,e.subtype,'opportunity',o.id,o.title FROM opportunity_party op JOIN opportunity o ON o.id=op.opportunity_id JOIN material m ON m.opportunity_id=o.id JOIN resource_use ru ON ru.material_id=m.id AND ru.version_no=m.head_version JOIN entity e ON e.id=ru.resource_id WHERE op.party_id=? AND o.state='open' AND e.subtype<>'' AND e.readiness<>'retired' AND e.retired_at IS NULL)
            SELECT * FROM paths WHERE id IN (SELECT id FROM paths GROUP BY id ORDER BY title COLLATE NOCASE,id LIMIT 60) ORDER BY title COLLATE NOCASE,id,via,hop")
            .bind(id).bind(id).bind(id).bind(id).fetch_all(&self.pool).await?;
        let mut associated_resources: Vec<DossierResource> = Vec::new();
        for (resource_id, title, kind, via, hop, label) in paths {
            if associated_resources
                .last()
                .is_none_or(|r| r.id != resource_id)
            {
                associated_resources.push(DossierResource {
                    id: resource_id,
                    title,
                    kind,
                    opportunity_id: None,
                    opportunity_title: None,
                    paths: vec![],
                });
            }
            let resource = associated_resources
                .last_mut()
                .expect("resource group was initialized");
            let path = if via == "edge" {
                json!({"via":"edge","edge_id":hop,"verb":label,"participant_id":id})
            } else {
                resource.opportunity_id.get_or_insert_with(|| hop.clone());
                resource
                    .opportunity_title
                    .get_or_insert_with(|| label.clone());
                json!({"via":"opportunity","opportunity_id":hop,"opportunity_title":label,"participant_id":id})
            };
            resource.paths.push(path);
        }
        let import_origin = sqlx::query_as::<_, (String, i64)>(
            "SELECT batch_id,source_record FROM party_import_origin WHERE party_id=?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .map(|(batch, record)| json!({"batch_id":batch,"source_record":record}));
        let retired_at = sqlx::query_scalar("SELECT retired_at FROM entity WHERE id=?")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        Ok(Some(EntityDossier {
            retired_at,
            entity,
            import_origin,
            direct_links,
            files,
            opportunities,
            associated_resources,
        }))
    }

    /// Create a distinct graph identity. Names are deliberately not keys: two
    /// people with the same display name remain separate records. The entity,
    /// its creation event and the receipt commit under one writer reservation.
    pub async fn create_entity(
        &self,
        idempotency_key: &str,
        kind: &str,
        name: &str,
        focus: &str,
    ) -> Result<EntityRecord, WorkspaceError> {
        self.create_card(idempotency_key, kind, name, focus, "", &[])
            .await
    }

    pub(super) async fn create_card(
        &self,
        idempotency_key: &str,
        kind: &str,
        name: &str,
        focus: &str,
        notes: &str,
        uploads: &[String],
    ) -> Result<EntityRecord, WorkspaceError> {
        if uploads.len() > 100 || notes.len() > 512 * 1024 {
            return Err(WorkspaceError::Invalid(
                "Card notes or file count exceeds the permitted limit.".into(),
            ));
        }
        let notes = canonical_markdown(notes);
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 200 {
            return Err(WorkspaceError::Invalid(
                "idempotency key must contain 1–200 bytes".into(),
            ));
        }
        if !matches!(kind, "person" | "org" | "project" | "resource") {
            return Err(WorkspaceError::Invalid("unsupported entity kind".into()));
        }
        if name.trim().is_empty() || name.len() > 500 {
            return Err(WorkspaceError::Invalid(
                "entity name must contain 1–500 bytes".into(),
            ));
        }
        if focus.chars().count() > 250 {
            return Err(WorkspaceError::Invalid(
                "focus exceeds 250 characters".into(),
            ));
        }
        let canonical = digest(
            json!(["create-card", kind, name, focus, notes, uploads])
                .to_string()
                .as_bytes(),
        );
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        )
        .bind(&self.epoch)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            if old_digest != canonical {
                return Err(WorkspaceError::IdempotencyConflict);
            }
            let record =
                serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            tx.commit().await?;
            return Ok(record);
        }
        let id = new_id(name);
        let now = now_utc();
        sqlx::query(
            "INSERT INTO entity(id,kind,name,focus,notes_md,revision,created_at,updated_at) VALUES(?,?,?,?,?,1,?,?)",
        )
        .bind(&id)
        .bind(kind)
        .bind(name)
        .bind(focus)
        .bind(notes)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'entity',?,1,'entity.created','{}')")
            .bind(new_id(&id)).bind(&now).bind(&id).execute(&mut *tx).await?;
        for upload in uploads {
            parties::bind_upload(&mut tx, &id, upload).await?;
        }
        let record = EntityRecord {
            id: id.clone(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            focus: focus.to_owned(),
            revision: 1,
        };
        let result_json =
            serde_json::to_string(&record).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,201,?,(SELECT COALESCE(MAX(cursor),0) FROM event),?)")
            .bind(&self.epoch).bind(idempotency_key).bind(canonical).bind(&result_json).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(record)
    }

    /// Last committed workspace event sequence, used as the read cursor.
    pub async fn current_cursor(&self) -> Result<i64, WorkspaceError> {
        let cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
            .fetch_one(&self.pool)
            .await?;
        Ok(cursor)
    }
}
