use crate::*;

impl Workspace {
    pub async fn list_resources(&self) -> Result<Vec<ResourceRecord>, WorkspaceError> {
        let rows=sqlx::query_as::<_,(String,String,String,String,String,i64)>("SELECT id,subtype,name,body_json,readiness,revision FROM entity WHERE subtype<>'' AND readiness <> 'retired' AND retired_at IS NULL ORDER BY created_at,id").fetch_all(&self.pool).await?;
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

    pub async fn list_sources(&self) -> Result<Vec<SavedSource>, WorkspaceError> {
        let rows=sqlx::query_as::<_,(String,String,String,String,String)>("SELECT c.id,v.id,o.id,c.normalized_sha256,v.raw_sha256 FROM source_occurrence o JOIN source_variant v ON v.id=o.source_variant_id JOIN source_content c ON c.id=v.content_id ORDER BY o.captured_at DESC,o.id").fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(
                |(content_id, variant_id, occurrence_id, normalized_sha256, raw_sha256)| {
                    SavedSource {
                        content_id,
                        variant_id,
                        occurrence_id,
                        normalized_sha256,
                        raw_sha256,
                    }
                },
            )
            .collect())
    }

    /// Save evidence independently of identity resolution. Identical normalized
    /// content shares a group while each raw variant and capture remains distinct.
    pub async fn save_source(
        &self,
        text: &str,
        title: Option<&str>,
        url: Option<&str>,
    ) -> Result<SavedSource, WorkspaceError> {
        self.save_source_attributed(text, title, url, "unknown", None)
            .await
    }

    pub async fn save_source_attributed(
        &self,
        text: &str,
        title: Option<&str>,
        url: Option<&str>,
        actor_kind: &str,
        actor_entity_id: Option<&str>,
    ) -> Result<SavedSource, WorkspaceError> {
        if !matches!(actor_kind, "owner" | "party" | "unknown")
            || ((actor_kind == "party") != actor_entity_id.is_some())
        {
            return Err(WorkspaceError::Invalid(
                "source attribution must identify an owner, a party entity, or remain unknown"
                    .into(),
            ));
        }
        if text.trim().is_empty() {
            return Err(WorkspaceError::Invalid("source text is empty".into()));
        }
        if text.len() > 2_000_000 {
            return Err(WorkspaceError::Invalid("source exceeds 2 MB".into()));
        }
        let normalized = parley_domain::evidence::normalize(text);
        let normalized_hash = digest(normalized.as_bytes());
        let raw_hash = digest(text.as_bytes());
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(actor_id) = actor_entity_id {
            let exists: Option<String> = sqlx::query_scalar("SELECT id FROM entity WHERE id=?")
                .bind(actor_id)
                .fetch_optional(&mut *tx)
                .await?;
            if exists.is_none() {
                return Err(WorkspaceError::Invalid(
                    "attributed speaker does not exist".into(),
                ));
            }
        }
        let content_id = match sqlx::query_scalar::<_, String>("SELECT id FROM source_content WHERE normalized_sha256=? AND normalization_version=? ORDER BY id LIMIT 1")
            .bind(&normalized_hash).bind(parley_domain::evidence::NORMALIZATION_VERSION as i64).fetch_optional(&mut *tx).await? {
                Some(id) => id,
                None => {
                    let id = new_id(&normalized_hash);
                    sqlx::query("INSERT INTO source_content(id,normalized_sha256,normalization_version,normalized_text,created_at) VALUES(?,?,?,?,?)")
                        .bind(&id).bind(&normalized_hash).bind(parley_domain::evidence::NORMALIZATION_VERSION as i64).bind(&normalized).bind(now_utc()).execute(&mut *tx).await?;
                    id
                }
            };
        let variant_id = match sqlx::query_scalar::<_, String>(
            "SELECT id FROM source_variant WHERE content_id=? AND raw_sha256=? ORDER BY id LIMIT 1",
        )
        .bind(&content_id)
        .bind(&raw_hash)
        .fetch_optional(&mut *tx)
        .await?
        {
            Some(id) => id,
            None => {
                let id = new_id(&raw_hash);
                sqlx::query("INSERT INTO source_variant(id,content_id,raw_sha256,raw_text,title,url,captured_at) VALUES(?,?,?,?,?,?,?)")
                        .bind(&id).bind(&content_id).bind(&raw_hash).bind(text).bind(title).bind(url).bind(now_utc()).execute(&mut *tx).await?;
                id
            }
        };
        let occurrence_id = new_id(&variant_id);
        sqlx::query("INSERT INTO source_occurrence(id,source_variant_id,actor_kind,actor_entity_id,captured_at) VALUES(?,?,?,?,?)")
            .bind(&occurrence_id).bind(&variant_id).bind(actor_kind).bind(actor_entity_id).bind(now_utc()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(SavedSource {
            content_id,
            variant_id,
            occurrence_id,
            normalized_sha256: normalized_hash,
            raw_sha256: raw_hash,
        })
    }

    /// Replay lookup for a capture command. Returns the stored success result
    /// when the exact key+canonical pair was already committed (RFC-0004
    /// INT-03), `None` for a fresh command, and a conflict when the key was
    /// used for a different command.
    pub async fn save_capture_replay(
        &self,
        key: &str,
        canonical: &str,
    ) -> Result<Option<Value>, WorkspaceError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        )
        .bind(&self.epoch)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((stored, result)) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        if stored != canonical {
            return Err(WorkspaceError::IdempotencyConflict);
        }
        let value: Value =
            serde_json::from_str(&result).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        tx.commit().await?;
        Ok(Some(value))
    }

    /// Commit a capture together with its extractor job, event rows and
    /// idempotency receipt in one writer transaction (B-FR-1, EVT-1, BQ1):
    /// a lost-ack retry must produce exactly one occurrence and one job, and
    /// a crash between "capture" and "enqueue" is impossible because the job
    /// row commits atomically with the source. The typed admission lives in
    /// `application::extract` (AR-001 AR06): the source is always saved, and
    /// a durable Extractor job is admitted only when a qualified structured
    /// profile is active and the full chunk plan fits the context budget.
    /// Returns the job id (when the queue admitted it) and the final response
    /// payload, which is also what the receipt stores.
    pub async fn save_capture_commit(
        &self,
        key: &str,
        canonical: &str,
        text: &str,
        title: Option<&str>,
        url: Option<&str>,
    ) -> Result<(Option<String>, Value), WorkspaceError> {
        application::extract::save_capture_commit(self, key, canonical, text, title, url).await
    }
}
