//! Resource board, shared card commits, moves, owner-authored edges and
//! entity attachment bindings (amendment 2026-11-22 §2–§6).
//!
//! A board card is an `entity` row of kind `resource` with typed subtype
//! data, canonical Markdown notes and attachment bindings. Placement lives
//! in `resource_placement`; positions are spaced ranks with per-card and
//! per-column revisions. All mutations run in one writer transaction with
//! idempotency receipts so a lost acknowledgement never creates a second
//! card, child, move or binding.

use super::{
    AppState, Workspace, WorkspaceError, authenticate, digest, entity_etag, error, idempotency_key,
    new_id, now_utc,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use parley_domain::notes;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use std::collections::BTreeSet;

const RANK_STEP: i64 = 1024;
const MAX_RESOURCE_TITLE: usize = 240;
const MAX_NOTES_BYTES: usize = 1_000_000;
const MAX_BASIS_BYTES: usize = 2_000;
const MAX_VERB_BYTES: usize = 120;
/// Verbs that assert a delivery. The generic graph editor cannot manufacture
/// a delivery by assigning such a verb (Resources spec §5); delivery
/// provenance is derived only from recorded sends (resource_card_deliveries).
const RESERVED_DELIVERY_VERBS: [&str; 3] = ["shared with", "delivered to", "sent to"];
const DEFAULT_PAGE: i64 = 200;
const MAX_PAGE: i64 = 500;

type CardRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
);
type OffBoardRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    i64,
);
type ResourceCardRow = (String, String, String, String, String, i64, i64, i64);
type EditCardRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    String,
    i64,
);
type DeliveryRowTuple = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
);
type AttachmentListRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
    bool,
    bool,
    String,
);
type EdgeRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
    String,
    String,
);
type BoundAttachmentRow = (String, String, String, String, i64, String, String, String);

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/resource-board", get(get_resource_board))
        .route(
            "/api/v1/resource-board/columns",
            post(create_resource_column),
        )
        .route(
            "/api/v1/resource-board/columns/{id}",
            axum::routing::delete(remove_resource_column).patch(rename_resource_column),
        )
        .route("/api/v1/resource-cards/{id}", get(get_resource_card))
        .route(
            "/api/v1/resource-cards/{id}/deliveries",
            get(get_resource_card_deliveries),
        )
        .route(
            "/api/v1/resource-cards/{id}/retire",
            post(retire_resource_card),
        )
        .route(
            "/api/v1/resource-cards/{id}/erase",
            post(erase_resource_card),
        )
        .route(
            "/api/v1/resource-cards/{id}/deliveries/{send_id}/{material_id}/{version}",
            get(get_delivered_resource_version),
        )
        .route("/api/v1/resource-card-commits", post(commit_resource_card))
        .route("/api/v1/resource-board/moves", post(move_resource_card))
        .route("/api/v1/entities/{id}/attachments", get(list_attachments))
}

const SUBTYPES: &[&str] = &["gift", "proof", "artifact", "skill", "interest", "note"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct ResourceColumn {
    pub id: String,
    pub title: String,
    pub position: i64,
    pub revision: i64,
    /// `system` marks the seeded Inbox; every other list is owner-managed.
    /// System lists demand an extra confirmation before removal because the
    /// seed never recreates them.
    pub kind: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateResourceColumn {
    pub title: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveResourceColumn {
    pub expected_revision: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenameResourceColumn {
    pub title: String,
    pub expected_revision: i64,
}

/// Why a card cannot be erased; the UI routes the owner to archive instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErasureRefusal {
    /// Recorded sends captured this card's content in a material version.
    Delivered,
    /// Drafted material versions depend on this card's content or files.
    InMaterials,
    /// Claims or claim-evidenced relationships must keep citing the card.
    InEvidence,
}

impl ErasureRefusal {
    pub fn code(&self) -> &'static str {
        match self {
            ErasureRefusal::Delivered => "RESOURCE_DELIVERED",
            ErasureRefusal::InMaterials => "RESOURCE_IN_MATERIALS",
            ErasureRefusal::InEvidence => "RESOURCE_IN_EVIDENCE",
        }
    }

    pub fn message(&self) -> String {
        match self {
            ErasureRefusal::Delivered => {
                "This card was delivered, so its content is part of the delivery record. Archive it instead."
                    .to_owned()
            }
            ErasureRefusal::InMaterials => {
                "Drafted materials still use this card. Remove it from those materials or archive it instead."
                    .to_owned()
            }
            ErasureRefusal::InEvidence => {
                "Claims and evidenced relationships still cite this card. Retract them or archive the card instead."
                    .to_owned()
            }
        }
    }
}

/// The outcome of an erase command: either the card is gone (with the
/// receipted result body) or the domain refused it for a stated reason.
pub enum EraseOutcome {
    Erased(Value),
    Refused(ErasureRefusal),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlacementView {
    pub column_id: String,
    pub position: i64,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceCounts {
    pub files: i64,
    pub connections: i64,
    pub proposed: i64,
    pub deliveries: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceCardSummary {
    pub resource_id: String,
    pub subtype: String,
    pub title: String,
    pub readiness: String,
    /// Entity revision; guards in-place title edits from the tile itself.
    pub revision: i64,
    /// Content revision; materials bind this revision and a change here
    /// rechecks dependent material versions (U06).
    pub content_revision: i64,
    pub placement: Option<PlacementView>,
    pub counts: ResourceCounts,
    /// Missing typed fields for this subtype; an empty list means eligible.
    pub gaps: Vec<String>,
    pub etag: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OffBoardResource {
    pub resource_id: String,
    pub subtype: String,
    pub title: String,
    pub readiness: String,
    pub revision: i64,
    pub content_revision: i64,
    pub legacy: bool,
    pub counts: ResourceCounts,
    pub gaps: Vec<String>,
    pub etag: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryRow {
    pub send_id: String,
    pub sent_at: String,
    pub recipient_id: String,
    pub recipient_name: String,
    pub opportunity_id: String,
    pub opportunity_title: String,
    pub material_id: String,
    pub material_version: i64,
    /// Material kind (outreach/followup/handoff) as the channel-like label.
    pub material_kind: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ResourceConnectionRecord {
    kind: String,
    opportunity_id: String,
    opportunity_title: String,
    material_id: String,
    material_version: i64,
    send_id: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ResourceGraphRecord {
    from_id: String,
    from_name: String,
    from_kind: String,
    to_id: String,
    to_name: String,
    to_kind: String,
    kind: String,
    opportunity_id: String,
    opportunity_title: String,
    material_id: String,
    material_version: i64,
    send_id: Option<String>,
    recorded_at: String,
    total: i64,
}

/// Deterministic missing-typed-field computation shared by board summaries,
/// the editor and the inspector. "Available" is an owner label; the backend
/// computes eligibility separately (an unfinished proof in Available is
/// still ineligible).
fn subtype_gaps(subtype: &str, body: &serde_json::Value) -> Vec<String> {
    let text = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
    };
    let mut gaps = Vec::new();
    match subtype {
        "proof" => {
            if text("result").is_empty() {
                gaps.push("Result".into());
            }
            if text("context").is_empty() {
                gaps.push("Context".into());
            }
            if text("time").is_empty() {
                gaps.push("Time".into());
            }
            if text("owner_role").is_empty() {
                gaps.push("Owner role".into());
            }
        }
        "gift" => {
            if text("effort").is_empty() {
                gaps.push("Effort estimate".into());
            }
        }
        "artifact" => {
            if text("reference").is_empty() {
                gaps.push("Available reference".into());
            }
            if body.get("share_permission") != Some(&Value::Bool(true)) {
                gaps.push("Permission to share".into());
            }
        }
        _ => {}
    }
    gaps
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceBoardSnapshot {
    pub columns: Vec<ResourceColumn>,
    pub cards: Vec<ResourceCardSummary>,
    pub off_board: Vec<OffBoardResource>,
    pub next_cursor: Option<String>,
    pub event_cursor: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BoardQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    include_offboard: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcePlacementInput {
    /// Column ID, or null to place off board.
    pub column_id: Option<String>,
    /// Optional anchor: place immediately before this card in the column.
    #[serde(default)]
    pub before_resource_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateResourceCard {
    #[serde(default)]
    pub staged_uploads: Vec<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtype: String,
    #[serde(default = "serde_json::Map::new")]
    pub body: serde_json::Map<String, Value>,
    #[serde(default)]
    pub notes_md: String,
    #[serde(default)]
    pub placement: Option<ResourcePlacementInput>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditResourceCard {
    pub resource_id: String,
    pub expected_revision: i64,
    pub expected_notes_rev: i64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub body: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    pub notes_md: Option<String>,
    #[serde(default)]
    pub staged_uploads: Vec<String>,
    #[serde(default)]
    pub unbind_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRelatedCard {
    #[serde(default)]
    pub staged_uploads: Vec<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtype: String,
    #[serde(default = "serde_json::Map::new")]
    pub body: serde_json::Map<String, Value>,
    #[serde(default)]
    pub notes_md: String,
    #[serde(default)]
    pub placement: Option<ResourcePlacementInput>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelatedPartyInput {
    kind: String,
    name: String,
    #[serde(default)]
    focus: String,
    #[serde(default)]
    notes_md: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelationAdd {
    pub verb: String,
    #[serde(default)]
    pub incoming: bool,
    #[serde(default)]
    pub expected_target_revision: Option<i64>,
    /// Link to an existing endpoint (resource or party), or create a related
    /// card with `create`.
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub create: Option<CreateRelatedCard>,
    #[serde(default)]
    pub create_party: Option<RelatedPartyInput>,
    #[serde(default)]
    pub basis: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCardCommitInput {
    #[serde(default)]
    pub create: Option<CreateResourceCard>,
    #[serde(default)]
    pub edit: Option<EditResourceCard>,
    #[serde(default)]
    pub placement: Option<ResourcePlacementInput>,
    #[serde(default)]
    pub relation_add: Option<RelationAdd>,
    #[serde(default)]
    pub expected_placement_revision: Option<i64>,
    #[serde(default)]
    pub expected_source_column_revision: Option<i64>,
    #[serde(default)]
    pub expected_destination_column_revision: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MoveResourceCardInput {
    pub resource_id: String,
    /// Destination column, or null to remove the card from the board.
    pub destination_column_id: Option<String>,
    #[serde(default)]
    pub before_resource_id: Option<String>,
    #[serde(default)]
    pub expected_source_column_revision: Option<i64>,
    #[serde(default)]
    pub expected_destination_column_revision: Option<i64>,
    #[serde(default)]
    pub expected_placement_revision: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateEdgeInput {
    pub expected_revision: i64,
    #[serde(default)]
    pub verb: Option<String>,
    #[serde(default)]
    pub basis: Option<String>,
    #[serde(default)]
    pub reverse: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntityAttachmentRow {
    pub id: String,
    pub entity_id: String,
    pub upload_id: String,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
    pub sha256: String,
    pub share_permission: bool,
    pub reviewed_by_owner: bool,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EdgeView {
    pub id: String,
    pub from_id: String,
    pub verb: String,
    pub to_id: String,
    pub state: String,
    pub revision: i64,
    pub basis: String,
    pub author_kind: String,
    pub created_at: String,
}

#[derive(Debug)]
struct Replay {
    status: i64,
    body: Value,
}

#[derive(Debug)]
struct PlacementApply {
    view: Option<PlacementView>,
    source_column_revision: Option<i64>,
    destination_column_revision: Option<i64>,
}

impl Workspace {
    /// Supplement the semantic graph with exact sends and current material
    /// references. These are read-only projections, never editable edges.
    pub(super) async fn add_resource_graph_records(
        &self,
        center: &str,
        nodes: &mut Vec<super::GraphNode>,
        edges: &mut Vec<super::GraphEdge>,
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
                nodes.push(super::GraphNode {
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
            edges.push(super::GraphEdge {
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
            operation_key_digest: super::digest(key.as_bytes()),
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
    pub(super) async fn place_confirmed_resource(
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

    async fn apply_placement(
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
        super::relations::archive(&mut tx, id, revision).await?;
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

async fn edge_view(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<EdgeView, WorkspaceError> {
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

fn validate_title(title: &str) -> Result<String, WorkspaceError> {
    let trimmed = title.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_RESOURCE_TITLE {
        return Err(WorkspaceError::Invalid(
            "card title must contain 1–240 characters".into(),
        ));
    }
    Ok(trimmed.to_owned())
}

fn validate_subtype(subtype: &str) -> Result<String, WorkspaceError> {
    if !SUBTYPES.contains(&subtype) {
        return Err(WorkspaceError::Invalid(format!(
            "subtype must be one of {}",
            SUBTYPES.join(", ")
        )));
    }
    Ok(subtype.to_owned())
}

async fn bind_upload_row(
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

async fn unbind_attachment_row(
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

async fn upsert_placement(
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

async fn entity_card_json(
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

async fn current_placement_json(
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

async fn max_event_cursor(tx: &mut Transaction<'_, Sqlite>) -> Result<i64, WorkspaceError> {
    Ok(
        sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event")
            .fetch_one(&mut **tx)
            .await?,
    )
}

/// Keyset cursor: `column position | card position | resource id`. It is
/// opaque to the client and decoded without a lookup.
fn encode_cursor(column_position: i64, position: i64, resource_id: String) -> String {
    format!("{column_position}|{position}|{resource_id}")
}

fn decode_cursor(value: &str) -> Result<(i64, i64, String), WorkspaceError> {
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

fn canonical_digest(method: &str, target: &str, body: &impl Serialize) -> String {
    let body = serde_json::to_vec(body).unwrap_or_default();
    digest(&[method.as_bytes(), b"\n", target.as_bytes(), b"\n", &body].concat())
}

async fn receipt_lookup(
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

async fn receipt_store(
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

async fn write_event(
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

// Handlers.

async fn get_resource_board(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<BoardQuery>,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return error.into_response();
    }
    match state
        .workspace
        .resource_board_snapshot(query.limit, query.cursor, query.include_offboard)
        .await
    {
        Ok(board) => (StatusCode::OK, Json(board)).into_response(),
        Err(error) => rb_error(error),
    }
}

async fn create_resource_column(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CreateResourceColumn>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.create_resource_column(&input, &key)).await
}

async fn remove_resource_column(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<RemoveResourceColumn>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.remove_resource_column(&id, &input, &key)).await
}

async fn rename_resource_column(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<RenameResourceColumn>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.rename_resource_column(&id, &input, &key)).await
}

async fn erase_resource_card(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    match state.workspace.erase_resource_card(&id, &key).await {
        Ok(EraseOutcome::Erased(result)) => (StatusCode::OK, Json(result)).into_response(),
        Ok(EraseOutcome::Refused(refusal)) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": {"code": refusal.code(), "message": refusal.message(), "details": []}
            })),
        )
            .into_response(),
        Err(error) => rb_error(error),
    }
}

async fn get_resource_card(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return error.into_response();
    }
    match state.workspace.resource_card_read(&id).await {
        Ok(entity) => (StatusCode::OK, Json(json!({"data": entity}))).into_response(),
        Err(WorkspaceError::NotFound) => {
            let lifecycle: Result<Option<String>, _> = sqlx::query_scalar(
                "SELECT CASE WHEN EXISTS(SELECT 1 FROM erasure_tombstone WHERE entity_id=?1) THEN 'erased'
                             WHEN EXISTS(SELECT 1 FROM entity WHERE id=?1 AND subtype<>'' AND (readiness='retired' OR retired_at IS NOT NULL)) THEN 'retired'
                        END",
            ).bind(&id).fetch_one(state.workspace.pool()).await;
            match lifecycle {
                Ok(Some(value)) if value == "erased" => error(
                    StatusCode::NOT_FOUND,
                    "RESOURCE_ERASED",
                    "This resource was erased.",
                ),
                Ok(Some(value)) if value == "retired" => error(
                    StatusCode::NOT_FOUND,
                    "RESOURCE_RETIRED",
                    "This resource was retired.",
                ),
                Ok(_) => rb_error(WorkspaceError::NotFound),
                Err(reason) => rb_error(reason.into()),
            }
        }
        Err(error) => rb_error(error),
    }
}

async fn get_resource_card_deliveries(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return error.into_response();
    }
    match state.workspace.resource_card_deliveries(&id).await {
        Ok(rows) => (StatusCode::OK, Json(json!({"data": rows}))).into_response(),
        Err(error) => rb_error(error),
    }
}

#[derive(Serialize, sqlx::FromRow)]
struct DeliveredResourceVersion {
    resource_title: String,
    resource_revision: i64,
    body: String,
    filename: Option<String>,
}

async fn get_delivered_resource_version(
    State(state): State<AppState>,
    Path((id, send_id, material_id, version)): Path<(String, String, String, i64)>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return error.into_response();
    }
    let row = sqlx::query_as::<_, DeliveredResourceVersion>(
        "SELECT d.resource_title,d.resource_revision,mv.body,mv.filename
         FROM delivered_resource_version d
         JOIN send_part sp ON sp.material_id=d.material_id AND sp.version_no=d.version_no
         JOIN material_version mv ON mv.material_id=sp.material_id AND mv.version_no=sp.version_no
         WHERE d.resource_id=? AND sp.send_id=? AND sp.material_id=? AND sp.version_no=?",
    )
    .bind(id)
    .bind(send_id)
    .bind(material_id)
    .bind(version)
    .fetch_optional(state.workspace.pool())
    .await;
    match row {
        Ok(Some(version)) => {
            let mut response = Json(version).into_response();
            super::private_headers(&mut response, state.workspace.epoch());
            response
        }
        Ok(None) => rb_error(WorkspaceError::NotFound),
        Err(error) => rb_error(error.into()),
    }
}

async fn retire_resource_card(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.retire_resource_card(&id, &key)).await
}

async fn commit_resource_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<ResourceCardCommitInput>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.commit_resource_card(&input, &key)).await
}

async fn move_resource_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<MoveResourceCardInput>,
) -> Response {
    let key = match command_key(&state, &headers).await {
        Ok(key) => key,
        Err(response) => return *response,
    };
    command_handler(state.workspace.move_resource_card(&input, &key)).await
}

async fn list_attachments(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return error.into_response();
    }
    match state.workspace.list_attachments(&id).await {
        Ok(items) => (StatusCode::OK, Json(json!({"data": items}))).into_response(),
        Err(error) => rb_error(error),
    }
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
        Err(error) => rb_error(error),
    }
}

fn key_error(error: (StatusCode, &'static str, &'static str)) -> Response {
    (
        error.0,
        Json(json!({"error": {"code": error.1, "message": error.2, "details": []}})),
    )
        .into_response()
}

fn rb_error(error: WorkspaceError) -> Response {
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
            "The addressed resource, notes, placement, or column changed in another session"
                .to_owned(),
        ),
        WorkspaceError::LedgerFull => (
            StatusCode::SERVICE_UNAVAILABLE,
            "LEDGER_CAPACITY",
            "The command ledger is at capacity".to_owned(),
        ),
    };
    (
        status,
        Json(json!({"error": {"code": code, "message": message, "details": []}})),
    )
        .into_response()
}
