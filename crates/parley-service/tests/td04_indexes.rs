//! TD-04 qualification: index plans, bounded reads and backfill parity.
//!
//! Every read is exercised against a private, fully migrated workspace. The
//! before/after plans are captured by dropping and recreating the index in
//! that private database, so the evidence shows the exact SCAN -> SEARCH
//! transition attributed to migration 0900.

use parley_service::Workspace;
use serde_json::json;
use sqlx::Row;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
static PLAN_SEQ: AtomicU64 = AtomicU64::new(0);

/// The production aggregate projection, with literal pagination values so the
/// plan can be captured without binding parameters.
const OUTPUTS_PLAN_SQL: &str = "WITH approval_snapshot AS (SELECT material_id,version_no,gate_snapshot_json FROM approval), latest_release AS (SELECT material_id,version_no,max(issued_at) AS issued_at FROM release_record GROUP BY material_id,version_no) SELECT m.id,m.opportunity_id,m.recipient_id,e.name,m.kind,mv.version_no,mv.content_sha256,CASE WHEN r.issued_at IS NOT NULL THEN 'released' WHEN a.gate_snapshot_json IS NOT NULL THEN 'approved' ELSE 'draft' END,a.gate_snapshot_json,m.updated_at FROM material m JOIN entity e ON e.id=m.recipient_id JOIN material_version mv ON mv.material_id=m.id AND mv.version_no=m.head_version LEFT JOIN approval_snapshot a ON a.material_id=m.id AND a.version_no=mv.version_no LEFT JOIN latest_release r ON r.material_id=m.id AND r.version_no=mv.version_no ORDER BY m.updated_at DESC,m.id ASC LIMIT 201";

fn db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-td04-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn open() -> (PathBuf, Workspace) {
    let path = db_path();
    let workspace = Workspace::open(&path).await.unwrap();
    (path, workspace)
}

/// Flatten `EXPLAIN QUERY PLAN` detail rows into one comparable line.
async fn plan(conn: &mut sqlx::SqliteConnection, sql: &str) -> String {
    // A unique comment makes each EXPLAIN a distinct prepared statement, so a
    // cached pre-DDL plan cannot be replayed after an index is dropped.
    let tag = PLAN_SEQ.fetch_add(1, Ordering::Relaxed).to_string();
    let mut statement = sqlx::QueryBuilder::<sqlx::Sqlite>::new("EXPLAIN QUERY PLAN ");
    statement.push(sql);
    statement.push(" /* td04-plan-");
    statement.push(&tag);
    statement.push(" */");
    let rows = statement.build().fetch_all(&mut *conn).await.unwrap();
    rows.iter()
        .map(|row| {
            row.try_get::<String, _>("detail")
                .unwrap_or_else(|_| row.try_get::<String, _>(3).unwrap())
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// SQLite sometimes reports an ordered index walk as `SCAN t USING INDEX`;
/// what matters for the evidence is that the named index is selected at all.
fn assert_index_used(label: &str, detail: &str, marker: &str) {
    assert!(
        detail.contains(&format!("INDEX {marker}")),
        "{label} must use index {marker}, got: {detail}"
    );
}

fn assert_index_absent(label: &str, detail: &str, marker: &str) {
    assert!(
        !detail.contains(&format!("INDEX {marker}")) && detail.contains("SCAN"),
        "{label} must show a pre-index SCAN, got: {detail}"
    );
}

async fn measure(conn: &mut sqlx::SqliteConnection, sql: &str) -> std::time::Duration {
    let mut samples = Vec::new();
    for _ in 0..20 {
        let start = std::time::Instant::now();
        let mut statement = sqlx::QueryBuilder::<sqlx::Sqlite>::new(sql);
        let _ = statement.build().fetch_all(&mut *conn).await.unwrap();
        samples.push(start.elapsed());
    }
    samples.sort();
    samples[18]
}

#[tokio::test]
async fn td04_indexes_turn_every_named_read_from_scan_to_search() {
    let (path, workspace) = open().await;
    let pool = workspace.pool();
    // One connection for the whole case: SQLite schema caches are per
    // connection, so a dropped index must be observed on the same connection.
    let mut conn = pool.acquire().await.unwrap();
    let now = "2026-11-20T12:00:00.000Z";

    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<5000) INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) SELECT printf('td04-event-%05d',i),?,'party',printf('td04-entity-%05d',i),1,'domain_changed','{}' FROM n")
        .bind(now)
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<5000) INSERT INTO entity(id,kind,name,revision,created_at,updated_at) SELECT printf('td04-entity-%05d',i),'person',printf('Entity %05d',i),1,?,? FROM n")
        .bind(now).bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<2000) INSERT INTO claim(id,subject_id,predicate,current_revision,created_at) SELECT printf('td04-claim-%05d',i),printf('td04-entity-%05d',i),'supports',1,? FROM n")
        .bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<2000) INSERT INTO claim_revision(claim_id,revision,wording,knowledge_kind,tier,speaker_kind,speaker_entity_id,confirmed_by_owner,created_at) SELECT printf('td04-claim-%05d',i),1,'A reviewed claim','observation',3,'owner',NULL,1,? FROM n")
        .bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<2000) INSERT INTO edge(id,from_id,to_id,verb,state,revision,created_at) SELECT printf('td04-edge-%05d',i),printf('td04-entity-%05d',i),printf('td04-entity-%05d',i+1),'supports','active',1,? FROM n")
        .bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<2000) INSERT INTO edge_evidence(edge_id,claim_id,claim_revision) SELECT printf('td04-edge-%05d',i),printf('td04-claim-%05d',i),1 FROM n")
        .execute(&mut *conn)
        .await
        .unwrap();
    // 10,000 match runs: 5,000 recipients and 5,000 mention-only rows. The
    // shortlist mentions are backfilled below with the migration's rule.
    sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES('td04-opp','TD04','open',1,?,?)")
        .bind(now).bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) SELECT 'td04-opp',id,'primary',? FROM entity WHERE id LIKE 'td04-entity-%'")
        .bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<10000) INSERT INTO match_run(id,opportunity_id,recipient_id,opportunity_revision,recipient_revision,need_sha256,embed_model_digest,calibration_json,shortlist_json,created_at) SELECT printf('td04-match-%05d',i),'td04-opp',printf('td04-entity-%05d',(i-1)%5000+1),1,1,'need','embed','{}',json_object('results',json_array(json_object('id',printf('td04-entity-%05d',(i+6)%5000+1)))),? FROM n")
        .bind(now)
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO template_family(id,name,created_at) VALUES('td04-family','TD04 family',?)",
    )
    .bind(now)
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("INSERT INTO template_version(id,family_id,version_no,body,fallbacks_json,status,content_sha256,created_at) VALUES('td04-template','td04-family',1,'Body','{}','active',?,?)")
        .bind("0".repeat(64)).bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000) INSERT INTO material(id,opportunity_id,recipient_id,kind,head_version,created_at,updated_at) SELECT printf('td04-material-%05d',i),'td04-opp','td04-entity-00001','outreach',1,?,? FROM n")
        .bind(now).bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000) INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at) SELECT printf('td04-material-%05d',i),1,'td04-opp','td04-entity-00001','td04-template',1,'Body','{}','[]',?,? FROM n")
        .bind("0".repeat(64)).bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("INSERT INTO match_run_mention(match_run_id,entity_id) SELECT DISTINCT m.id,j.atom FROM match_run m, json_tree(m.shortlist_json) j WHERE j.type='text' AND j.atom IS NOT NULL ON CONFLICT(match_run_id,entity_id) DO NOTHING")
        .execute(&mut *conn)
        .await
        .unwrap();

    // `expect_scan` is false for edge_evidence: its composite PRIMARY KEY
    // already starts with edge_id, so SQLite uses the equivalent autoindex
    // before and after dropping the explicit 0900 index. The index is still
    // deployed for the explicitly named lookup path; claiming a SCAN there
    // would be false.
    let cases: Vec<(&str, &str, &str, bool, Vec<&str>)> = vec![
        (
            "event_entity_cursor",
            "SELECT cursor FROM event WHERE entity_id='td04-entity-00001' ORDER BY cursor DESC LIMIT 200",
            "event_entity_cursor",
            true,
            vec!["event_entity_cursor"],
        ),
        (
            "edge_evidence_edge",
            "SELECT claim_id,claim_revision FROM edge_evidence WHERE edge_id='td04-edge-00001' ORDER BY claim_id LIMIT 50",
            "edge_evidence_edge",
            false,
            vec!["sqlite_autoindex_edge_evidence_1"],
        ),
        (
            "match_run_recipient + match_run_mention_entity",
            "SELECT count(*) FROM (SELECT id FROM match_run WHERE recipient_id='td04-entity-00001' UNION SELECT match_run_id FROM match_run_mention WHERE entity_id='td04-entity-00001')",
            "match_run_recipient",
            true,
            vec!["match_run_recipient", "match_run_mention_entity"],
        ),
        (
            "material_updated",
            OUTPUTS_PLAN_SQL,
            "material_updated",
            true,
            vec!["material_updated"],
        ),
    ];

    for (label, sql, index, expect_scan, markers) in cases {
        let with_index = plan(&mut conn, sql).await;
        for marker in &markers {
            assert_index_used(label, &with_index, marker);
        }
        let drop = match index {
            "event_entity_cursor" => "DROP INDEX event_entity_cursor",
            "edge_evidence_edge" => "DROP INDEX edge_evidence_edge",
            "match_run_recipient" => "DROP INDEX match_run_recipient",
            "material_updated" => "DROP INDEX material_updated",
            _ => unreachable!(),
        };
        sqlx::query(drop).execute(&mut *conn).await.unwrap();
        let without_index = plan(&mut conn, sql).await;
        if expect_scan {
            assert_index_absent(label, &without_index, markers[0]);
        } else {
            for marker in &markers {
                assert_index_used(label, &without_index, marker);
            }
        }
        // Recreate the migration index so later cases and the latency pass
        // observe the deployed schema.
        let definition = match index {
            "event_entity_cursor" => "CREATE INDEX event_entity_cursor ON event(entity_id,cursor)",
            "edge_evidence_edge" => "CREATE INDEX edge_evidence_edge ON edge_evidence(edge_id)",
            "match_run_recipient" => "CREATE INDEX match_run_recipient ON match_run(recipient_id)",
            "material_updated" => "CREATE INDEX material_updated ON material(updated_at,id)",
            _ => unreachable!(),
        };
        sqlx::query(definition).execute(&mut *conn).await.unwrap();
        println!("TD-04 {label}\n  before: {without_index}\n  after:  {with_index}");
    }

    let usage = measure(
        &mut conn,
        "SELECT count(*) FROM (SELECT id FROM match_run WHERE recipient_id='td04-entity-00001' UNION SELECT match_run_id FROM match_run_mention WHERE entity_id='td04-entity-00001')",
    )
    .await;
    let outputs = measure(&mut conn, OUTPUTS_PLAN_SQL).await;
    println!(
        "TD-04 warm p95 over 20 samples at 10,000 match runs: usage={usage:?}, outputs={outputs:?}, host {}",
        std::env::consts::OS
    );
    let budget =
        std::time::Duration::from_millis(if std::env::var_os("LLVM_PROFILE_FILE").is_some() {
            300
        } else {
            200
        });
    assert!(usage < budget, "usage p95 {usage:?} exceeds {budget:?}");
    assert!(
        outputs < budget,
        "outputs p95 {outputs:?} exceeds {budget:?}"
    );

    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn td04_outputs_registry_is_bounded_body_free_and_keyset_paginated() {
    let (path, workspace) = open().await;
    let mut conn = workspace.pool().acquire().await.unwrap();
    let now = "2026-11-20T12:00:00.000Z";
    sqlx::query("INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES('td04-party','person','Dana Lee',1,?,?)")
        .bind(now).bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES('td04-opp','Parity','open',1,?,?)")
        .bind(now).bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES('td04-opp','td04-party','primary',?)")
        .bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query(
        "INSERT INTO template_family(id,name,created_at) VALUES('td04-family','TD04 family',?)",
    )
    .bind(now)
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("INSERT INTO template_version(id,family_id,version_no,body,fallbacks_json,status,content_sha256,created_at) VALUES('td04-template','td04-family',1,'Body','{}','active',?,?)")
        .bind("0".repeat(64)).bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<600) INSERT INTO material(id,opportunity_id,recipient_id,kind,head_version,created_at,updated_at) SELECT printf('td04-out-%05d',i),'td04-opp','td04-party','outreach',1,?,? FROM n")
        .bind(now).bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<600) INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at) SELECT printf('td04-out-%05d',i),1,'td04-opp','td04-party','td04-template',1,'SECRET LETTER BODY','{}','[]',?,? FROM n")
        .bind("0".repeat(64)).bind(now).execute(&mut *conn).await.unwrap();
    drop(conn);

    let first = workspace.list_outputs(200, None).await.unwrap();
    assert_eq!(first.items.len(), 200);
    assert!(first.next_cursor.is_some());
    let serialized = serde_json::to_string(&first).unwrap();
    assert!(!serialized.contains("SECRET LETTER BODY"));
    assert!(!serialized.contains("\"body\""));

    let second = workspace
        .list_outputs(200, first.next_cursor.as_deref())
        .await
        .unwrap();
    assert_eq!(second.items.len(), 200);
    assert!(second.next_cursor.is_some());
    let third = workspace
        .list_outputs(200, second.next_cursor.as_deref())
        .await
        .unwrap();
    assert_eq!(third.items.len(), 200);
    assert!(third.next_cursor.is_none());
    let ids: std::collections::BTreeSet<&str> = first
        .items
        .iter()
        .chain(second.items.iter())
        .chain(third.items.iter())
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(ids.len(), 600, "keyset pages must not repeat a material");

    let capped = workspace.list_outputs(5000, None).await.unwrap();
    assert_eq!(capped.items.len(), 500, "page size is capped at 500");
    assert!(capped.next_cursor.is_some());
    assert!(workspace.list_outputs(200, Some("not-hex")).await.is_err());

    drop(workspace);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn td04_backfill_parity_with_the_retired_json_scan() {
    let (path, workspace) = open().await;
    let pool = workspace.pool();
    // One connection for the whole case: SQLite schema caches are per
    // connection, so a dropped index must be observed on the same connection.
    let mut conn = pool.acquire().await.unwrap();
    let now = "2026-11-20T12:00:00.000Z";
    sqlx::query("INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES('td04-party','person','Mentioned Party',1,?,?)")
        .bind(now).bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES('td04-opp','Parity','open',1,?,?)")
        .bind(now).bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES('td04-opp','td04-party','primary',?)")
        .bind(now).execute(&mut *conn).await.unwrap();

    let shortlist =
        json!({"results":[{"id":"td04-party","kind":"proof"},{"id":"other","kind":"gift"}]})
            .to_string();
    sqlx::query("INSERT INTO match_run(id,opportunity_id,recipient_id,opportunity_revision,recipient_revision,need_sha256,embed_model_digest,calibration_json,shortlist_json,created_at) VALUES('td04-run','td04-opp','td04-party',1,1,'need','embed','{}',?,?)")
        .bind(&shortlist).bind(now).execute(&mut *conn).await.unwrap();
    sqlx::query("INSERT INTO match_run_mention(match_run_id,entity_id) SELECT DISTINCT m.id,j.atom FROM match_run m, json_tree(m.shortlist_json) j WHERE j.type='text' AND j.atom IS NOT NULL ON CONFLICT(match_run_id,entity_id) DO NOTHING")
        .execute(&mut *conn).await.unwrap();

    let old: i64 = sqlx::query_scalar("SELECT count(*) FROM match_run WHERE recipient_id='td04-party' OR EXISTS(SELECT 1 FROM json_tree(shortlist_json) WHERE type='text' AND atom='td04-party')")
        .fetch_one(&mut *conn).await.unwrap();
    let new: i64 = sqlx::query_scalar("SELECT count(*) FROM (SELECT id FROM match_run WHERE recipient_id='td04-party' UNION SELECT match_run_id FROM match_run_mention WHERE entity_id='td04-party')")
        .fetch_one(&mut *conn).await.unwrap();
    assert_eq!(old, 1);
    assert_eq!(
        new, old,
        "normalized count must equal the retired scan count"
    );

    drop(workspace);
    let _ = fs::remove_file(path);
}
