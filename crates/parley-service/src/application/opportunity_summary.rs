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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workspace;

    const NOW: &str = "2026-11-23T12:00:00.000Z";

    async fn seeded_workspace() -> (std::path::PathBuf, Workspace) {
        let path = std::env::temp_dir().join(format!(
            "parley-opp-summary-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let workspace = Workspace::open(&path).await.expect("seeded workspace");
        let mut tx = workspace
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .unwrap();
        for (id, name) in [
            ("p-1", "Mira Chen"),
            ("p-2", "Alex Boatwright"),
            ("p-3", "Dana Lee"),
        ] {
            sqlx::query("INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES(?,'person',?,1,?,?)")
                .bind(id).bind(name).bind(NOW).bind(NOW)
                .execute(&mut *tx).await.unwrap();
        }
        sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at,stage_id,phase_revision) VALUES('o-1','Northline review','open',7,?,?, 'developing',4)")
            .bind(NOW).bind(NOW)
            .execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES('o-2','Closed thread','closed',1,?,?)")
            .bind(NOW).bind(NOW)
            .execute(&mut *tx).await.unwrap();
        for (opportunity, party, role) in [
            ("o-1", "p-1", "primary"),
            ("o-1", "p-2", "participant"),
            ("o-1", "p-3", "participant"),
            ("o-2", "p-1", "primary"),
        ] {
            sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES(?,?,?,?)")
                .bind(opportunity).bind(party).bind(role).bind(NOW)
                .execute(&mut *tx).await.unwrap();
        }
        tx.commit().await.unwrap();
        (path, workspace)
    }

    #[tokio::test]
    async fn missing_opportunity_projects_none() {
        let (path, workspace) = seeded_workspace().await;
        let mut tx = workspace.pool().begin().await.unwrap();
        let summary = summary(&mut tx, "o-absent", &["p-1".into()]).await.unwrap();
        assert!(summary.is_none());
        tx.commit().await.unwrap();
        workspace.pool().close().await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn projects_only_the_granted_target_set_sorted_and_deduplicated() {
        let (path, workspace) = seeded_workspace().await;
        let mut tx = workspace.pool().begin().await.unwrap();
        let summary = summary(
            &mut tx,
            "o-1",
            &["p-2".into(), "p-1".into(), "p-2".into(), "p-absent".into()],
        )
        .await
        .unwrap()
        .expect("seeded opportunity");
        assert_eq!(summary.id, "o-1");
        assert_eq!(summary.title, "Northline review");
        assert_eq!(summary.lifecycle, "open");
        assert_eq!(summary.stage_id.as_deref(), Some("developing"));
        // Queue-sheet triggers own this column; the projection echoes it verbatim.
        let stored: i64 =
            sqlx::query_scalar("SELECT phase_revision FROM opportunity WHERE id='o-1'")
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(summary.phase_revision, stored);
        // p-3 participates but was never granted; p-absent was granted but is
        // not on the opportunity; the duplicate grant collapses.
        assert_eq!(summary.participants, vec!["Alex Boatwright", "Mira Chen"]);
        tx.commit().await.unwrap();
        workspace.pool().close().await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn null_stage_projects_without_a_phase() {
        let (path, workspace) = seeded_workspace().await;
        let mut tx = workspace.pool().begin().await.unwrap();
        let summary = summary(&mut tx, "o-2", &["p-1".into()])
            .await
            .unwrap()
            .expect("seeded opportunity");
        assert_eq!(summary.stage_id, None);
        assert_eq!(summary.participants, vec!["Mira Chen"]);
        tx.commit().await.unwrap();
        workspace.pool().close().await;
        let _ = std::fs::remove_file(&path);
    }
}
