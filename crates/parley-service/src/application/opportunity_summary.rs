//! Typed opportunity summary projection (UCR-001 U02 / Q4).
//!
//! Participant labels are returned only for the immutable target set of the
//! run; the agent cannot widen that set through an argument or a provider
//! message. The projection carries no notes, attachments or source text.

use serde::Serialize;
use sqlx::{Sqlite, Transaction};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OpportunitySummary {
    pub id: String,
    pub title: String,
    pub lifecycle: String,
    pub stage_id: Option<String>,
    pub phase_revision: i64,
    pub participants: Vec<String>,
}

/// `granted_parties` is the immutable target set; every other participant is
/// omitted from the projection rather than returned as a redacted placeholder.
pub(crate) async fn summary(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    granted_parties: &[String],
) -> Result<Option<OpportunitySummary>, sqlx::Error> {
    let row: Option<(String, String, Option<String>, i64)> =
        sqlx::query_as("SELECT title,state,stage_id,phase_revision FROM opportunity WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((title, lifecycle, stage_id, phase_revision)) = row else {
        return Ok(None);
    };
    let mut participants = Vec::new();
    for party_id in granted_parties {
        let name: Option<String> = sqlx::query_scalar(
            "SELECT e.name FROM opportunity_party op JOIN entity e ON e.id=op.party_id WHERE op.opportunity_id=? AND op.party_id=?",
        )
        .bind(id)
        .bind(party_id)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(name) = name {
            participants.push(name);
        }
    }
    participants.sort();
    participants.dedup();
    Ok(Some(OpportunitySummary {
        id: id.to_owned(),
        title,
        lifecycle,
        stage_id,
        phase_revision,
        participants,
    }))
}
