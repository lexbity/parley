use super::*;
use serde_json::json;

impl Workspace {
    /// Correct an owner-authored edge (verb/basis) under its revision.
    pub async fn update_owner_edge(
        &self,
        id: &str,
        input: &UpdateEdgeInput,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        let canonical = canonical_digest("PATCH", &format!("/edges/{id}"), input);
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status": replay.status, "result": replay.body}));
        }
        let row: Option<(String, String, String, i64, String, String)> = sqlx::query_as(
            "SELECT from_id,verb,to_id,revision,basis,author_kind FROM edge WHERE id=? AND state='active' AND author_kind='owner' AND NOT EXISTS (SELECT 1 FROM edge_evidence WHERE edge_id=edge.id)",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((from_id, current_verb, to_id, revision, current_basis, author_kind)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        if revision != input.expected_revision {
            return Err(WorkspaceError::StaleRevision);
        }
        let next_verb = match &input.verb {
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() || trimmed.len() > MAX_VERB_BYTES {
                    return Err(WorkspaceError::Invalid(
                        "relationship verb must contain 1–120 bytes".into(),
                    ));
                }
                trimmed.to_owned()
            }
            None => current_verb,
        };
        let next_basis = match &input.basis {
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() || trimmed.len() > MAX_BASIS_BYTES {
                    return Err(WorkspaceError::Invalid(
                        "relationship basis must contain 1–2000 bytes".into(),
                    ));
                }
                trimmed.to_owned()
            }
            None => current_basis,
        };
        if RESERVED_DELIVERY_VERBS.contains(
            &next_verb
                .to_lowercase()
                .replace('_', " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .as_str(),
        ) {
            return Err(WorkspaceError::Invalid(
                "Use Mark sent to record deliveries.".into(),
            ));
        }
        let (from_id, to_id) = if input.reverse {
            (to_id, from_id)
        } else {
            (from_id, to_id)
        };
        let duplicate: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM edge WHERE state='active' AND from_id=? AND to_id=? AND verb=? AND id<>?)",
        ).bind(&from_id).bind(&to_id).bind(&next_verb).bind(id).fetch_one(&mut *tx).await?;
        if duplicate {
            return Err(WorkspaceError::Invalid(
                "This relationship already exists.".into(),
            ));
        }
        crate::relations::archive(&mut tx, id, revision).await?;
        let next_revision = revision + 1;
        sqlx::query("UPDATE edge SET verb=?,basis=?,from_id=?,to_id=?,revision=? WHERE id=?")
            .bind(&next_verb)
            .bind(&next_basis)
            .bind(&from_id)
            .bind(&to_id)
            .bind(next_revision)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        write_event(
            &mut tx,
            "edge",
            id,
            next_revision,
            "edge.corrected",
            json!({"from_id": from_id, "verb": next_verb, "to_id": to_id}),
        )
        .await?;
        let edge = edge_view(&mut tx, id).await?;
        let result = json!({"edge": edge, "author_kind": author_kind});
        receipt_store(&mut tx, key, &canonical, 200, &result).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Attachments for an entity, with sharing/review flags from the upload.
    pub async fn list_attachments(
        &self,
        entity_id: &str,
    ) -> Result<Vec<EntityAttachmentRow>, WorkspaceError> {
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM entity WHERE id=?")
            .bind(entity_id)
            .fetch_optional(self.pool())
            .await?;
        if exists.is_none() {
            return Err(WorkspaceError::NotFound);
        }
        let rows: Vec<AttachmentListRow> = sqlx::query_as(
            "SELECT a.id,a.entity_id,a.upload_id,a.filename,a.content_type,a.size,a.sha256,u.share_permission,u.reviewed_by_owner,a.created_at \
             FROM entity_attachment a JOIN blob_upload u ON u.id=a.upload_id WHERE a.entity_id=? ORDER BY a.created_at,a.id",
        )
        .bind(entity_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    id,
                    entity_id,
                    upload_id,
                    filename,
                    content_type,
                    size,
                    sha256,
                    share_permission,
                    reviewed_by_owner,
                    created_at,
                )| {
                    EntityAttachmentRow {
                        id,
                        entity_id,
                        upload_id,
                        filename,
                        content_type,
                        size,
                        sha256,
                        share_permission,
                        reviewed_by_owner,
                        created_at,
                    }
                },
            )
            .collect())
    }

    /// Bind a staged upload to an entity card (one binding per upload).
    pub async fn bind_attachment(
        &self,
        entity_id: &str,
        upload_id: &str,
        key: &str,
    ) -> Result<Value, WorkspaceError> {
        let body = json!({"entity_id": entity_id, "upload_id": upload_id});
        let canonical =
            canonical_digest("POST", &format!("/entities/{entity_id}/attachments"), &body);
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status": replay.status, "result": replay.body}));
        }
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM entity WHERE id=?")
            .bind(entity_id)
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Err(WorkspaceError::NotFound);
        }
        bind_upload_row(&mut tx, entity_id, upload_id).await?;
        let attachment: Option<BoundAttachmentRow> = sqlx::query_as(
            "SELECT a.id,a.upload_id,a.filename,a.content_type,a.size,a.sha256,u.share_permission,u.reviewed_by_owner \
             FROM entity_attachment a JOIN blob_upload u ON u.id=a.upload_id WHERE a.entity_id=? AND a.upload_id=?",
        )
        .bind(entity_id)
        .bind(upload_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((
            id,
            upload_id,
            filename,
            content_type,
            size,
            sha256,
            share_permission,
            reviewed_by_owner,
        )) = attachment
        else {
            return Err(WorkspaceError::NotFound);
        };
        let result = json!({"attachment": {"id": id, "entity_id": entity_id, "upload_id": upload_id, "filename": filename, "content_type": content_type, "size": size, "sha256": sha256, "share_permission": share_permission, "reviewed_by_owner": reviewed_by_owner, "created_at": now_utc()}});
        receipt_store(&mut tx, key, &canonical, 201, &result).await?;
        tx.commit().await?;
        Ok(json!({"_replay_status": 201, "result": result}))
    }

    /// Unbind an attachment binding (the file itself is untouched).
    pub async fn unbind_attachment(
        &self,
        entity_id: &str,
        attachment_id: &str,
        key: &str,
    ) -> Result<(), WorkspaceError> {
        let canonical = canonical_digest(
            "DELETE",
            &format!("/entities/{entity_id}/attachments/{attachment_id}"),
            &json!({}),
        );
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            if replay.status == 204 {
                return Ok(());
            }
            return Err(WorkspaceError::Invalid("attachment unbind replay".into()));
        }
        unbind_attachment_row(&mut tx, entity_id, attachment_id).await?;
        receipt_store(&mut tx, key, &canonical, 204, &json!({})).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Owner-content read: stream the bound file's verified bytes.
    pub async fn attachment_bytes(
        &self,
        entity_id: &str,
        attachment_id: &str,
    ) -> Result<(Vec<u8>, String), WorkspaceError> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT a.sha256,a.filename FROM entity_attachment a WHERE a.id=? AND a.entity_id=?",
        )
        .bind(attachment_id)
        .bind(entity_id)
        .fetch_optional(self.pool())
        .await?;
        let Some((sha256, filename)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        let path = self.blob_root().join(&sha256);
        let bytes = tokio::fs::read(&path).await.map_err(|error| {
            WorkspaceError::Invalid(format!("blob file is not readable: {error}"))
        })?;
        Ok((bytes, filename))
    }
}

pub(super) async fn edge_view(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<EdgeView, WorkspaceError> {
    let row: Option<EdgeRow> = sqlx::query_as(
            "SELECT id,from_id,verb,to_id,state,revision,basis,author_kind,created_at FROM edge WHERE id=?",
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some((id, from_id, verb, to_id, state, revision, basis, author_kind, created_at)) = row
    else {
        return Err(WorkspaceError::NotFound);
    };
    Ok(EdgeView {
        id,
        from_id,
        verb,
        to_id,
        state,
        revision,
        basis,
        author_kind,
        created_at,
    })
}

pub(super) fn validate_title(title: &str) -> Result<String, WorkspaceError> {
    let trimmed = title.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_RESOURCE_TITLE {
        return Err(WorkspaceError::Invalid(
            "card title must contain 1–240 characters".into(),
        ));
    }
    Ok(trimmed.to_owned())
}

pub(super) fn validate_subtype(subtype: &str) -> Result<String, WorkspaceError> {
    if !SUBTYPES.contains(&subtype) {
        return Err(WorkspaceError::Invalid(format!(
            "subtype must be one of {}",
            SUBTYPES.join(", ")
        )));
    }
    Ok(subtype.to_owned())
}

pub(super) async fn bind_upload_row(
    tx: &mut Transaction<'_, Sqlite>,
    entity_id: &str,
    upload_id: &str,
) -> Result<(), WorkspaceError> {
    let upload: Option<(String, String, i64, String)> =
        sqlx::query_as("SELECT filename,mime,size,blob_sha256 FROM blob_upload WHERE id=? AND (card_private=0 OR unixepoch(created_at)>unixepoch()-1800)")
            .bind(upload_id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((filename, mime, size, sha256)) = upload else {
        return Err(WorkspaceError::Invalid(format!(
            "staged upload {upload_id} is missing or expired; upload the file again"
        )));
    };
    let bound: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM entity_attachment WHERE upload_id=?")
            .bind(upload_id)
            .fetch_optional(&mut **tx)
            .await?;
    if bound.is_some() {
        return Err(WorkspaceError::Invalid(
            "staged upload is already bound to a card".into(),
        ));
    }
    let id = new_id(&digest(format!("{entity_id}\0{upload_id}").as_bytes()));
    sqlx::query(
        "INSERT INTO entity_attachment(id,entity_id,upload_id,filename,content_type,size,sha256,created_at) \
         VALUES(?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(entity_id)
    .bind(upload_id)
    .bind(&filename)
    .bind(&mime)
    .bind(size)
    .bind(&sha256)
    .bind(now_utc())
    .execute(&mut **tx)
    .await?;
    write_event(
        tx,
        "entity_attachment",
        &id,
        1,
        "entity_attachment.bound",
        json!({"entity_id": entity_id, "upload_id": upload_id}),
    )
    .await?;
    Ok(())
}

pub(super) async fn unbind_attachment_row(
    tx: &mut Transaction<'_, Sqlite>,
    entity_id: &str,
    attachment_id: &str,
) -> Result<(), WorkspaceError> {
    let changed = sqlx::query("DELETE FROM entity_attachment WHERE id=? AND entity_id=?")
        .bind(attachment_id)
        .bind(entity_id)
        .execute(&mut **tx)
        .await?;
    if changed.rows_affected() == 0 {
        return Err(WorkspaceError::NotFound);
    }
    write_event(
        tx,
        "entity_attachment",
        attachment_id,
        1,
        "entity_attachment.unbound",
        json!({"entity_id": entity_id}),
    )
    .await?;
    Ok(())
}

pub(super) async fn upsert_placement(
    tx: &mut Transaction<'_, Sqlite>,
    resource_id: &str,
    column_id: &str,
    position: i64,
    prior_revision: i64,
) -> Result<(), WorkspaceError> {
    sqlx::query(
        "INSERT INTO resource_placement(resource_id,column_id,position,revision) VALUES(?,?,?,?) \
         ON CONFLICT(resource_id) DO UPDATE SET column_id=excluded.column_id,position=excluded.position,revision=excluded.revision",
    )
    .bind(resource_id)
    .bind(column_id)
    .bind(position)
    .bind(prior_revision + 1)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(super) async fn entity_card_json(
    epoch: &str,
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Value, WorkspaceError> {
    let row: Option<ResourceCardRow> = sqlx::query_as(
        "SELECT name,subtype,body_json,readiness,notes_md,revision,notes_rev,resource_content_revision FROM entity WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((name, subtype, body, readiness, notes, revision, notes_rev, content_revision)) = row
    else {
        return Err(WorkspaceError::NotFound);
    };
    Ok(json!({
        "id": id,
        "kind": "resource",
        "title": name,
        "subtype": subtype,
        "body": serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({})),
        "readiness": readiness,
        "revision": revision,
        "content_revision": content_revision,
        "notes": {"markdown": notes, "revision": notes_rev},
        "etag": entity_etag(epoch, id, revision)
    }))
}

pub(super) async fn current_placement_json(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Value, WorkspaceError> {
    let row: Option<(String, i64, i64)> = sqlx::query_as(
        "SELECT column_id,position,revision FROM resource_placement WHERE resource_id=?",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(match row {
        Some((column_id, position, revision)) => {
            json!({"column_id": column_id, "position": position, "revision": revision})
        }
        None => Value::Null,
    })
}

pub(super) async fn max_event_cursor(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<i64, WorkspaceError> {
    Ok(
        sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
            .fetch_one(&mut **tx)
            .await?,
    )
}

/// Keyset cursor: `column position | card position | resource id`. It is
/// opaque to the client and decoded without a lookup.
pub(super) fn encode_cursor(column_position: i64, position: i64, resource_id: String) -> String {
    format!("{column_position}|{position}|{resource_id}")
}

pub(super) fn decode_cursor(value: &str) -> Result<(i64, i64, String), WorkspaceError> {
    let mut parts = value.splitn(3, '|');
    let column_position = parts
        .next()
        .unwrap_or_default()
        .parse::<i64>()
        .map_err(|_| WorkspaceError::Invalid("malformed board cursor".into()))?;
    let position = parts
        .next()
        .unwrap_or_default()
        .parse::<i64>()
        .map_err(|_| WorkspaceError::Invalid("malformed board cursor".into()))?;
    let resource_id = parts.next().unwrap_or_default();
    if resource_id.is_empty() {
        return Err(WorkspaceError::Invalid("malformed board cursor".into()));
    }
    Ok((column_position, position, resource_id.to_owned()))
}

// Receipt/event plumbing (mirrors board.rs).

pub(super) fn canonical_digest(method: &str, target: &str, body: &impl Serialize) -> String {
    let body = serde_json::to_vec(body).unwrap_or_default();
    digest(&[method.as_bytes(), b"\n", target.as_bytes(), b"\n", &body].concat())
}

pub(super) async fn receipt_lookup(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    canonical: &str,
) -> Result<Option<Replay>, WorkspaceError> {
    let row: Option<(String, i64, String)> = sqlx::query_as(
        "SELECT canonical_digest,status_code,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=(SELECT epoch FROM workspace_meta WHERE singleton=1) AND key=?",
    )
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?;
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

pub(super) async fn receipt_store(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    canonical: &str,
    status: i64,
    result: &Value,
) -> Result<(), WorkspaceError> {
    sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,(SELECT epoch FROM workspace_meta WHERE singleton=1),?,?,?,?,(SELECT COALESCE(MAX(cursor),0) FROM event),?)")
        .bind(key)
        .bind(canonical)
        .bind(status)
        .bind(result.to_string())
        .bind(now_utc())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(super) async fn write_event(
    tx: &mut Transaction<'_, Sqlite>,
    entity_type: &str,
    id: &str,
    revision: i64,
    kind: &str,
    payload: Value,
) -> Result<(), WorkspaceError> {
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,?,?,?,?,?)")
        .bind(new_id(id))
        .bind(now_utc())
        .bind(entity_type)
        .bind(id)
        .bind(revision)
        .bind(kind)
        .bind(payload.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}
