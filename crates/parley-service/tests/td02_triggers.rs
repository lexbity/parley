//! TD-002 review guard: the final trigger inventory after all migrations is a
//! checked-in artifact, and the immutability invariants it encodes are probed
//! directly. A future table-rebuild migration that drops a trigger without
//! recreating it fails here instead of silently removing an invariant.

use parley_service::Workspace;
use sqlx::Row;
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-td02-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn normalize(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn expected() -> BTreeMap<String, String> {
    serde_json::from_str(include_str!("fixtures/expected_triggers.json"))
        .expect("checked-in trigger inventory is valid JSON")
}

#[tokio::test]
async fn td02_final_trigger_inventory_matches_the_checked_in_set() {
    let path = db_path();
    let workspace = Workspace::open(&path).await.unwrap();
    let rows = sqlx::query("SELECT name,sql FROM sqlite_master WHERE type='trigger' ORDER BY name")
        .fetch_all(workspace.pool())
        .await
        .unwrap();
    let actual: BTreeMap<String, String> = rows
        .iter()
        .map(|row| {
            (
                row.get::<String, _>("name"),
                normalize(&row.get::<String, _>("sql")),
            )
        })
        .collect();
    let expected = expected();
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "migration set changed the trigger inventory; update the checked-in fixture deliberately"
    );
    for (name, body) in &expected {
        assert_eq!(
            actual.get(name).map(String::as_str),
            Some(body.as_str()),
            "trigger {name} body drifted from the checked-in inventory"
        );
    }
    println!("TD-02 verified {} final triggers.", actual.len());
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn td02_event_log_stays_append_only() {
    let path = db_path();
    let workspace = Workspace::open(&path).await.unwrap();
    let pool = workspace.pool();
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES('td02-event','2026-11-25T12:00:00Z','entity','td02-entity',1,'domain_changed','{}')")
        .execute(pool)
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE event SET payload_json='{}' WHERE event_id='td02-event'")
            .execute(pool)
            .await
            .is_err(),
        "event_no_update must reject a rewritten event"
    );
    assert!(
        sqlx::query("DELETE FROM event WHERE event_id='td02-event'")
            .execute(pool)
            .await
            .is_err(),
        "event_no_delete must reject an erased event"
    );
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn td02_template_versions_stay_immutable() {
    let path = db_path();
    let workspace = Workspace::open(&path).await.unwrap();
    let pool = workspace.pool();
    let now = "2026-11-25T12:00:00.000Z";
    sqlx::query(
        "INSERT INTO template_family(id,name,created_at) VALUES('td02-family','Guard family',?)",
    )
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO template_version(id,family_id,version_no,body,fallbacks_json,status,content_sha256,created_at) VALUES('td02-template','td02-family',1,'Body','{}','active',?,?)")
        .bind("a".repeat(64))
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE template_version SET body='Changed' WHERE id='td02-template'")
            .execute(pool)
            .await
            .is_err(),
        "template content immutability must hold"
    );
    assert!(
        sqlx::query("DELETE FROM template_version WHERE id='td02-template'")
            .execute(pool)
            .await
            .is_err(),
        "template_version_no_delete must hold"
    );
    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn td02_erasure_exception_is_gated_on_an_authorization() {
    // The 0011 widening of event_no_update is only reachable with a committed
    // erasure_authorization row; without one the append-only rule still holds.
    let path = db_path();
    let workspace = Workspace::open(&path).await.unwrap();
    let pool = workspace.pool();
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES('td02-erasure-event','2026-11-25T12:00:00Z','entity','td02-erasure-entity',1,'domain_changed','{}')")
        .execute(pool)
        .await
        .unwrap();
    assert!(
        sqlx::query(
            "UPDATE event SET payload_json='{\"erased\":true}' WHERE event_id='td02-erasure-event'"
        )
        .execute(pool)
        .await
        .is_err(),
        "an erasure payload without an authorization must not bypass append-only"
    );
    drop(workspace);
    let _ = fs::remove_file(path);
}
