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
use serde_json::Value;
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

mod card;
mod column;
mod http;
mod placement;
mod relation;
mod support;

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

/// Router assembled from the HTTP submodule.
pub(super) fn routes() -> axum::Router<AppState> {
    http::routes()
}
