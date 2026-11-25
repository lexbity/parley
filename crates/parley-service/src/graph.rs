use crate::*;

impl Workspace {
    pub(super) async fn graph_snapshot(
        &self,
        center_id: Option<&str>,
        view_scope: &str,
        view_id: &str,
    ) -> Result<GraphSnapshot, WorkspaceError> {
        let total_nodes = if let Some(center) = center_id {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM graph_card e WHERE e.id=? OR e.id IN (SELECT from_id FROM edge WHERE state='active' AND to_id=?) OR e.id IN (SELECT to_id FROM edge WHERE state='active' AND from_id=?)")
                .bind(center).bind(center).bind(center).fetch_one(&self.pool).await?
        } else {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM graph_card")
                .fetch_one(&self.pool)
                .await?
        };
        let mut rows = if let Some(center) = center_id {
            sqlx::query_as::<_, (String, String, String, i64, f64, f64, i64)>(
                "SELECT e.id,e.kind,e.name,e.revision,COALESCE(v.x,((ROW_NUMBER() OVER(ORDER BY e.name COLLATE NOCASE,e.id)-1)%8)*240.0),COALESCE(v.y,((ROW_NUMBER() OVER(ORDER BY e.name COLLATE NOCASE,e.id)-1)/8)*160.0),COALESCE(v.revision,0) FROM graph_card e LEFT JOIN view_position v ON v.entity_id=e.id AND v.view_scope=? AND v.view_id=? WHERE e.id=? OR e.id IN (SELECT from_id FROM edge WHERE state='active' AND to_id=?) OR e.id IN (SELECT to_id FROM edge WHERE state='active' AND from_id=?) ORDER BY (e.id=?) DESC,e.name COLLATE NOCASE,e.id LIMIT 201",
            )
            .bind(view_scope).bind(view_id).bind(center).bind(center).bind(center).bind(center).fetch_all(&self.pool).await?
        } else {
            sqlx::query_as::<_, (String, String, String, i64, f64, f64, i64)>(
                "SELECT e.id,e.kind,e.name,e.revision,COALESCE(v.x,((ROW_NUMBER() OVER(ORDER BY e.name COLLATE NOCASE,e.id)-1)%8)*240.0),COALESCE(v.y,((ROW_NUMBER() OVER(ORDER BY e.name COLLATE NOCASE,e.id)-1)/8)*160.0),COALESCE(v.revision,0) FROM graph_card e LEFT JOIN view_position v ON v.entity_id=e.id AND v.view_scope=? AND v.view_id=? ORDER BY e.name COLLATE NOCASE,e.id LIMIT 201",
            )
            .bind(view_scope).bind(view_id).fetch_all(&self.pool).await?
        };
        let mut omitted_nodes = (total_nodes - 200).max(0);
        rows.truncate(200);
        let visible: std::collections::HashSet<String> =
            rows.iter().map(|row| row.0.clone()).collect();
        let mut nodes = rows
            .into_iter()
            .map(
                |(id, kind, name, revision, x, y, position_revision)| GraphNode {
                    id,
                    kind,
                    name,
                    revision,
                    x,
                    y,
                    position_revision,
                },
            )
            .collect::<Vec<_>>();
        let total_edges = if visible.is_empty() {
            0
        } else {
            let mut query = sqlx::QueryBuilder::new(
                "SELECT COUNT(*) FROM edge WHERE state='active' AND from_id IN (",
            );
            {
                let mut separated = query.separated(",");
                for id in &visible {
                    separated.push_bind(id);
                }
            }
            query.push(") AND to_id IN (");
            {
                let mut separated = query.separated(",");
                for id in &visible {
                    separated.push_bind(id);
                }
            }
            query.push(")");
            query
                .build_query_scalar::<i64>()
                .fetch_one(&self.pool)
                .await?
        };
        let mut edges: Vec<GraphEdge> = if visible.is_empty() {
            Vec::new()
        } else {
            let mut query = sqlx::QueryBuilder::new(
                "SELECT e.id,e.from_id,e.to_id,e.verb,e.revision,cr.occurrence_id,COALESCE(cr.wording,e.basis) quote, (cr.claim_id IS NULL) owner_authored, COALESCE(cr.tier,1) tier, COALESCE(cr.speaker_kind,e.author_kind) speaker_kind,COALESCE(so.captured_at,e.created_at) captured_at,sv.title source_title,sv.url source_url,cr.claim_id,cr.revision claim_revision,cr.speaker_entity_id FROM edge e LEFT JOIN edge_evidence ee ON ee.rowid=(SELECT newest.rowid FROM edge_evidence newest WHERE newest.edge_id=e.id ORDER BY newest.rowid DESC LIMIT 1) LEFT JOIN claim_revision cr ON cr.claim_id=ee.claim_id AND cr.revision=ee.claim_revision LEFT JOIN source_occurrence so ON so.id=cr.occurrence_id LEFT JOIN source_variant sv ON sv.id=cr.source_variant_id WHERE e.state='active' AND e.from_id IN (",
            );
            {
                let mut separated = query.separated(",");
                for id in &visible {
                    separated.push_bind(id);
                }
            }
            query.push(") AND e.to_id IN (");
            {
                let mut separated = query.separated(",");
                for id in &visible {
                    separated.push_bind(id);
                }
            }
            query.push(") ORDER BY e.from_id,e.to_id,e.id LIMIT 401");
            query.build_query_as().fetch_all(&self.pool).await?
        };
        let mut omitted_edges = (total_edges - 400).max(0);
        edges.truncate(400);
        if view_scope == "resources"
            && let Some(center) = center_id
        {
            self.add_resource_graph_records(
                center,
                &mut nodes,
                &mut edges,
                &mut omitted_nodes,
                &mut omitted_edges,
            )
            .await?;
        }
        Ok(GraphSnapshot {
            nodes,
            edges,
            omitted_nodes,
            omitted_edges,
        })
    }

    pub(super) async fn save_graph_layout(
        &self,
        idempotency_key: &str,
        input: GraphLayoutInput,
    ) -> Result<GraphLayoutResult, WorkspaceError> {
        if idempotency_key.is_empty()
            || idempotency_key.len() > 100
            || input.view_scope.is_empty()
            || input.view_scope.len() > 80
            || input.view_id.is_empty()
            || input.view_id.len() > 160
            || input.positions.is_empty()
            || input.positions.len() > 200
        {
            return Err(WorkspaceError::Invalid(
                "graph layout command is outside its bounds".into(),
            ));
        }
        let mut ids = std::collections::HashSet::new();
        if input.positions.iter().any(|position| {
            !position.x.is_finite()
                || !position.y.is_finite()
                || position.x.abs() > 1_000_000.0
                || position.y.abs() > 1_000_000.0
                || position.expected_revision < 0
                || !ids.insert(position.entity_id.as_str())
        }) {
            return Err(WorkspaceError::Invalid(
                "graph coordinates or entity set is invalid".into(),
            ));
        }
        let canonical = digest(
            &serde_json::to_vec(&input)
                .map_err(|error| WorkspaceError::Invalid(error.to_string()))?,
        );
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old_digest, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT canonical_digest,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=? AND key=?",
        ).bind(&self.epoch).bind(idempotency_key).fetch_optional(&mut *tx).await? {
            if old_digest != canonical { return Err(WorkspaceError::IdempotencyConflict); }
            let result = serde_json::from_str(&result).map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
            tx.commit().await?;
            return Ok(result);
        }
        let now = now_utc();
        let mut saved = Vec::with_capacity(input.positions.len());
        let mut last_cursor = 0_i64;
        for position in input.positions {
            let exists: Option<String> = sqlx::query_scalar("SELECT id FROM entity WHERE id=?")
                .bind(&position.entity_id)
                .fetch_optional(&mut *tx)
                .await?;
            if exists.is_none() {
                return Err(WorkspaceError::NotFound);
            }
            let current: Option<i64> = sqlx::query_scalar("SELECT revision FROM view_position WHERE view_scope=? AND view_id=? AND entity_id=?")
                .bind(&input.view_scope).bind(&input.view_id).bind(&position.entity_id).fetch_optional(&mut *tx).await?;
            let current_revision = current.unwrap_or(0);
            if current_revision != position.expected_revision {
                return Err(WorkspaceError::StaleRevision);
            }
            let revision = current_revision + 1;
            sqlx::query("INSERT INTO view_position(view_scope,view_id,entity_id,x,y,pinned,revision) VALUES(?,?,?,?,?,1,?) ON CONFLICT(view_scope,view_id,entity_id) DO UPDATE SET x=excluded.x,y=excluded.y,pinned=1,revision=excluded.revision")
                .bind(&input.view_scope).bind(&input.view_id).bind(&position.entity_id).bind(position.x).bind(position.y).bind(revision).execute(&mut *tx).await?;
            last_cursor = sqlx::query_scalar("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?, 'entity',?,?, 'graph.positioned','{}') RETURNING cursor")
                .bind(new_id(&position.entity_id)).bind(&now).bind(&position.entity_id).bind(revision).fetch_one(&mut *tx).await?;
            saved.push(GraphPositionResult {
                entity_id: position.entity_id,
                x: position.x,
                y: position.y,
                revision,
            });
        }
        let result = GraphLayoutResult { positions: saved };
        let result_json = serde_json::to_string(&result)
            .map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
        sqlx::query("INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,?,?,?,200,?,?,?)")
            .bind(&self.epoch).bind(idempotency_key).bind(canonical).bind(result_json).bind(last_cursor).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
    }
}
