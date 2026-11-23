//! Typed party summary projection (UCR-001 U02 / Q4).
//!
//! Only the fields named by the catalogue are read. Notes, attachments,
//! source text and private evidence are never selected, and the projection
//! never grows when a caller asks for more.

use serde::Serialize;
use sqlx::{Sqlite, Transaction};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct PartySummary {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub focus: String,
    pub card_revision: i64,
}

pub(crate) async fn summary(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<PartySummary>, sqlx::Error> {
    let row: Option<(String, String, String, i64)> = sqlx::query_as(
        "SELECT kind,name,focus,revision FROM entity WHERE id=? AND kind IN ('person','org','project')",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|(kind, name, focus, card_revision)| PartySummary {
        id: id.to_owned(),
        kind,
        name,
        focus,
        card_revision,
    }))
}
