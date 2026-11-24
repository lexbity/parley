//! Shared R5 hook resolver (AR-001 AR03).
//!
//! The typed fill for a `hook:` slot is an accepted current claim revision
//! scoped to the selected recipient and, when present, the selected
//! opportunity. Professional publicness, approved wording, a verified
//! provenance tier and the pack freshness bound are enforced here so the
//! preview, manual authoring, composition and release gate share one
//! permission check.

use sqlx::SqliteConnection;

use crate::WorkspaceError;

#[derive(Clone, Debug)]
#[allow(dead_code)] // shared resolver fields are read by gate consumption
pub(crate) struct HookEvidence {
    pub(crate) claim_id: String,
    pub(crate) claim_revision: i64,
    pub(crate) published_at: String,
    pub(crate) tier: i64,
    pub(crate) wording: String,
}

/// Resolve an accepted hook fill. Returns `None` when the hook is not
/// currently accepted, fresh, approved, or scoped to the recipient and
/// opportunity.
pub(crate) async fn hook_fill(
    conn: &mut SqliteConnection,
    claim_id: &str,
    claim_revision: i64,
    recipient_id: &str,
    opportunity_id: Option<&str>,
    cutoff: &str,
) -> Result<Option<HookEvidence>, WorkspaceError> {
    let row: Option<(String, i64, String)> = sqlx::query_as(
        "SELECT h.published_at,r.tier,r.approved_wording FROM accepted_hook h
         JOIN claim c ON c.id=h.claim_id
         JOIN claim_revision r ON r.claim_id=h.claim_id AND r.revision=h.claim_revision
         WHERE h.claim_id=? AND h.claim_revision=? AND c.current_revision=h.claim_revision
           AND r.approved_wording IS NOT NULL AND r.knowledge_kind<>'hypothesis'
           AND r.tier IN (1,3) AND h.publicness='professional_public' AND h.published_at >= ?
           AND c.subject_id=?",
    )
    .bind(claim_id)
    .bind(claim_revision)
    .bind(cutoff)
    .bind(recipient_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((published_at, tier, wording)) = row else {
        return Ok(None);
    };
    if let Some(opportunity_id) = opportunity_id {
        let scoped: Option<String> = sqlx::query_scalar(
            "SELECT id FROM opportunity_interest
             WHERE opportunity_id=? AND party_id=? AND claim_id=? AND claim_revision=?
               AND status='active'",
        )
        .bind(opportunity_id)
        .bind(recipient_id)
        .bind(claim_id)
        .bind(claim_revision)
        .fetch_optional(&mut *conn)
        .await?;
        if scoped.is_none() {
            return Ok(None);
        }
    }
    Ok(Some(HookEvidence {
        claim_id: claim_id.to_owned(),
        claim_revision,
        published_at,
        tier,
        wording,
    }))
}
