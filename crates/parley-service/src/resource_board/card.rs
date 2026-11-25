use super::support::*;
use super::*;
use serde_json::json;

impl Workspace {
    /// Supplement the semantic graph with exact sends and current material
    /// references. These are read-only projections, never editable edges.
    pub(crate) async fn add_resource_graph_records(
        &self,
        center: &str,
        nodes: &mut Vec<crate::GraphNode>,
        edges: &mut Vec<crate::GraphEdge>,
        omitted_nodes: &mut i64,
        omitted_edges: &mut i64,
    ) -> Result<(), WorkspaceError> {
        let rows: Vec<ResourceGraphRecord> = sqlx::query_as(
            "WITH connections AS (
                SELECT r.id from_id,r.name from_name,r.kind from_kind,p.id to_id,p.name to_name,p.kind to_kind,
                       'delivery' kind,o.id opportunity_id,o.title opportunity_title,m.id material_id,
                       sp.version_no material_version,s.id send_id,s.sent_at recorded_at
                FROM delivered_resource_version d
                JOIN graph_card r ON r.id=d.resource_id
                JOIN send_part sp ON sp.material_id=d.material_id AND sp.version_no=d.version_no
                JOIN send s ON s.id=sp.send_id
                JOIN graph_card p ON p.id=s.recipient_id
                JOIN material m ON m.id=sp.material_id
                JOIN opportunity o ON o.id=m.opportunity_id
                WHERE d.resource_id=?1 OR s.recipient_id=?1
                UNION ALL
                SELECT r.id,r.name,r.kind,m.id,'Material for ' || o.title,'material','drafting',o.id,o.title,
                       m.id,m.head_version,NULL,m.updated_at
                FROM version_dependency d
                JOIN graph_card r ON r.id=d.source_id
                JOIN material m ON m.id=d.material_id AND m.head_version=d.version_no
                JOIN opportunity o ON o.id=m.opportunity_id
                WHERE d.kind='resource' AND (d.source_id=?1 OR m.id=?1)
                UNION ALL
                SELECT r.id,r.name,r.kind,o.id,o.title,'opportunity','opportunity',o.id,o.title,
                       m.id,m.head_version,NULL,m.updated_at
                FROM version_dependency d
                JOIN graph_card r ON r.id=d.source_id
                JOIN material m ON m.id=d.material_id AND m.head_version=d.version_no
                JOIN opportunity o ON o.id=m.opportunity_id
                WHERE d.kind='resource' AND (d.source_id=?1 OR o.id=?1)
            )
            SELECT *, count(*) OVER() total FROM connections
            ORDER BY CASE kind WHEN 'delivery' THEN 0 ELSE 1 END, recorded_at DESC, material_id,from_id,to_id LIMIT 400",
        )
        .bind(center)
        .fetch_all(self.pool())
        .await?;
        let total = rows.first().map_or(0, |row| row.total);
        *omitted_edges += total - rows.len() as i64;
        let mut visible: std::collections::HashSet<String> =
            nodes.iter().map(|node| node.id.clone()).collect();
        let mut omitted = std::collections::HashSet::new();
        for row in rows {
            if edges.len() >= 400 {
                *omitted_edges += 1;
                continue;
            }
            for (id, name, kind) in [
                (&row.from_id, &row.from_name, &row.from_kind),
                (&row.to_id, &row.to_name, &row.to_kind),
            ] {
                if visible.contains(id) {
                    continue;
                }
                if nodes.len() >= 200 {
                    omitted.insert(id.clone());
                    continue;
                }
                visible.insert(id.clone());
                nodes.push(crate::GraphNode {
                    id: id.clone(),
                    name: name.clone(),
                    kind: kind.clone(),
                    revision: 1,
                    x: 0.0,
                    y: 0.0,
                    position_revision: 0,
                });
            }
            if !visible.contains(&row.from_id) || !visible.contains(&row.to_id) {
                *omitted_edges += 1;
                continue;
            }
            let verb = match row.kind.as_str() {
                "delivery" => "shared with",
                "drafting" => "used to prepare",
                _ => "used for",
            };
            let id = format!(
                "{}:{}:{}:{}:{}",
                row.kind,
                row.from_id,
                row.to_id,
                row.material_id,
                row.send_id.as_deref().unwrap_or("")
            );
            let record = ResourceConnectionRecord {
                kind: row.kind,
                opportunity_id: row.opportunity_id,
                opportunity_title: row.opportunity_title,
                material_id: row.material_id,
                material_version: row.material_version,
                send_id: row.send_id,
            };
            edges.push(crate::GraphEdge {
                id,
                from_id: row.from_id,
                to_id: row.to_id,
                verb: verb.into(),
                revision: 1,
                occurrence_id: None,
                quote: String::new(),
                owner_authored: false,
                tier: 1,
                speaker_kind: "owner".into(),
                captured_at: row.recorded_at,
                source_title: None,
                source_url: None,
                claim_id: None,
                claim_revision: None,
                speaker_entity_id: None,
                record: Some(sqlx::types::Json(record)),
            });
        }
        *omitted_nodes += omitted.len() as i64;
        Ok(())
    }

    /// Derived delivery rows for a resource: sends whose delivered material
    /// version contains the resource or one of its bound files. Drafting-only
    /// dependencies never appear here; repeat sends are separate rows.
    pub async fn resource_card_deliveries(
        &self,
        id: &str,
    ) -> Result<Vec<DeliveryRow>, WorkspaceError> {
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM entity WHERE id=?")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        if exists.is_none() {
            return Err(WorkspaceError::NotFound);
        }
        let rows: Vec<DeliveryRowTuple> = sqlx::query_as(
            "SELECT s.id,s.sent_at,s.recipient_id,e.name,t.opportunity_id,o.title,sp.material_id,sp.version_no,m.kind \
             FROM send s \
             JOIN entity e ON e.id=s.recipient_id \
             JOIN contact_thread t ON t.id=s.thread_id \
             JOIN opportunity o ON o.id=t.opportunity_id \
             JOIN send_part sp ON sp.send_id=s.id \
             JOIN material m ON m.id=sp.material_id \
             JOIN delivered_resource_version d ON d.material_id=sp.material_id AND d.version_no=sp.version_no \
             WHERE d.resource_id=? \
             ORDER BY s.sent_at DESC,s.id",
        )
        .bind(id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    send_id,
                    sent_at,
                    recipient_id,
                    recipient_name,
                    opportunity_id,
                    opportunity_title,
                    material_id,
                    material_version,
                    material_kind,
                )| {
                    DeliveryRow {
                        send_id,
                        sent_at,
                        recipient_id,
                        recipient_name,
                        opportunity_id,
                        opportunity_title,
                        material_id,
                        material_version,
                        material_kind,
                    }
                },
            )
            .collect())
    }

    /// Retire a resource: removed from ordinary use, board and picker.
    /// Affected material versions signal a release recheck; a later edit or
    /// link to the retired card is rejected.
    pub async fn retire_resource_card(&self, id: &str, key: &str) -> Result<Value, WorkspaceError> {
        let canonical =
            canonical_digest("POST", &format!("/resource-cards/{id}/retire"), &json!({}));
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(json!({"_replay_status": replay.status, "result": replay.body}));
        }
        // The REST owner route keeps its direct, owner-authenticated entry and
        // its wire envelope; the retirement facts are owned by the shared
        // application operation (UCR-001 U08), so both callers mutate one set
        // of revisions and events. The route reads the current revisions and
        // passes them as the expected preconditions (UCR-001 §5.3).
        let row: Option<(i64, i64)> = sqlx::query_as(
            "SELECT revision,resource_content_revision FROM entity WHERE id=? AND subtype<>'' AND readiness <> 'retired' AND retired_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((revision, content_revision)) = row else {
            return Err(WorkspaceError::NotFound);
        };
        let input = crate::application::resource_retirement::RetirementInput {
            id: id.to_owned(),
            expected_card_revision: revision,
            expected_content_revision: content_revision,
            operation_key_digest: crate::digest(key.as_bytes()),
        };
        let outcome = match crate::application::resource_retirement::retire(&mut tx, &input).await {
            Ok(outcome) => outcome,
            Err(crate::application::resource_retirement::RetirementError::NotFound) => {
                return Err(WorkspaceError::NotFound);
            }
            Err(crate::application::resource_retirement::RetirementError::StaleRevision) => {
                return Err(WorkspaceError::StaleRevision);
            }
            Err(crate::application::resource_retirement::RetirementError::TooManyVersions) => {
                return Err(WorkspaceError::Invalid(
                    "retirement impact exceeds 256 material versions".into(),
                ));
            }
            Err(crate::application::resource_retirement::RetirementError::Database) => {
                return Err(WorkspaceError::Invalid("database error".into()));
            }
        };
        let affected_json: Vec<Value> = outcome
            .impacted_versions
            .iter()
            .map(|dependency| {
                json!({"material_id": dependency.material_id, "version_no": dependency.version_no})
            })
            .collect();
        let result = json!({
            "entity": {"id": id, "kind": "resource", "readiness": "retired", "revision": outcome.card_revision},
            "affected_versions": affected_json
        });
        receipt_store(&mut tx, key, &canonical, 200, &result).await?;
        tx.commit().await?;
        Ok(json!({"_replay_status": 200, "result": result}))
    }

    /// Permanently erase a board card: the entity, its placement, attachments,
    /// owner edges and any paired legacy row. Provenance that must outlive the
    /// card refuses the erasure — recorded deliveries, drafting dependencies
    /// and claim evidence — so archive remains the loss-free path.
    pub async fn erase_resource_card(
        &self,
        id: &str,
        key: &str,
    ) -> Result<EraseOutcome, WorkspaceError> {
        let canonical =
            canonical_digest("POST", &format!("/resource-cards/{id}/erase"), &json!({}));
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(replay) = receipt_lookup(&mut tx, key, &canonical).await? {
            tx.commit().await?;
            return Ok(EraseOutcome::Erased(replay.body));
        }
        let revision: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM entity WHERE id=? AND subtype<>'' AND readiness<>'retired' AND retired_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(revision) = revision else {
            return Err(WorkspaceError::NotFound);
        };
        let delivered: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM delivered_resource_version WHERE resource_id=?",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if delivered > 0 {
            return Ok(EraseOutcome::Refused(ErasureRefusal::Delivered));
        }
        let dependencies: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM version_dependency WHERE kind='resource' AND source_id=?",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if dependencies > 0 {
            return Ok(EraseOutcome::Refused(ErasureRefusal::InMaterials));
        }
        let claims: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM claim WHERE subject_id=?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let evidenced: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM edge_evidence e JOIN edge x ON x.id=e.edge_id WHERE x.from_id=?1 OR x.to_id=?1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if claims + evidenced > 0 {
            return Ok(EraseOutcome::Refused(ErasureRefusal::InEvidence));
        }
        let referenced: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM unresolved_mention WHERE resolved_entity_id=?1) \
             + (SELECT COUNT(*) FROM proposal_decision WHERE target_entity_id=?1)",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if referenced > 0 {
            return Ok(EraseOutcome::Refused(ErasureRefusal::InEvidence));
        }
        // Owner edges and their recorded history exist only through their
        // endpoints; the erased card takes them with it.
        sqlx::query("DELETE FROM edge_history WHERE edge_id IN (SELECT id FROM edge WHERE from_id=?1 OR to_id=?1)")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM edge WHERE from_id=?1 OR to_id=?1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        // Attachment rows RESTRICT both the entity and their uploads; capture
        // the card's uploads, drop the attachments, then the entity
        // (placement and view position cascade).
        let uploads: Vec<String> =
            sqlx::query_scalar("SELECT upload_id FROM entity_attachment WHERE entity_id=?")
                .bind(id)
                .fetch_all(&mut *tx)
                .await?;
        sqlx::query("DELETE FROM entity_attachment WHERE entity_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        // Entity removal is refused by the foreign keys of any surviving
        // reference table (resource_use, resource_tag, interviewer_candidate,
        // edges, claims). The explicit delivery/dependency/evidence checks
        // above produce the precise refusal; an FK violation here means a
        // drafted material or candidate still uses the card.
        if let Err(reason) = sqlx::query("DELETE FROM entity WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
        {
            if reason
                .as_database_error()
                .is_some_and(|error| error.is_foreign_key_violation())
            {
                return Ok(EraseOutcome::Refused(ErasureRefusal::InMaterials));
            }
            return Err(reason.into());
        }
        // Release private uploads this card owned exclusively; shared digests
        // stay (the same rule as party erasure).
        for upload_id in &uploads {
            sqlx::query(
                "DELETE FROM blob_upload WHERE id=? AND card_private=1 \
                 AND id NOT IN (SELECT blob_upload_id FROM material_version WHERE blob_upload_id IS NOT NULL)",
            )
            .bind(upload_id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM blob WHERE sha256 NOT IN (SELECT blob_sha256 FROM blob_upload UNION SELECT blob_sha256 FROM material_version WHERE blob_sha256 IS NOT NULL UNION SELECT blob_sha256 FROM blob_snapshot_pin UNION SELECT blob_sha256 FROM blob_upload_lease)")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT OR REPLACE INTO erasure_tombstone(entity_id,erased_at,source_generation) VALUES(?,?,?)",
        )
        .bind(id)
        .bind(now_utc())
        .bind(self.epoch())
        .execute(&mut *tx)
        .await?;
        write_event(
            &mut tx,
            "resource",
            id,
            revision + 1,
            "resource.erased",
            json!({"erased": true}),
        )
        .await?;
        let result = json!({"id": id, "erased": true});
        receipt_store(&mut tx, key, &canonical, 200, &result).await?;
        tx.commit().await?;
        Ok(EraseOutcome::Erased(result))
    }

    /// Full read of one board card (entity kind resource) with typed body,
    /// notes and revisions, for the shared editor.
    pub async fn resource_card_read(&self, id: &str) -> Result<Value, WorkspaceError> {
        let row: Option<ResourceCardRow> =
            sqlx::query_as(
                "SELECT name,subtype,body_json,readiness,notes_md,revision,notes_rev,resource_content_revision FROM entity WHERE id=? AND subtype<>'' AND readiness <> 'retired' AND retired_at IS NULL",
            )
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        let Some((name, subtype, body, readiness, notes, revision, notes_rev, content_revision)) =
            row
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
            "etag": entity_etag(&self.epoch, id, revision)
        }))
    }
}
