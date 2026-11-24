use parley_service::{Workspace, create_backup_for_test};
use sha2::{Digest, Sha256};
use sqlx::{
    SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{borrow::Cow, path::PathBuf};

const FIXTURE: &str = include_str!("../../../../contract/fixtures/ar01/migration/populated.sql");

struct FixtureRoot(PathBuf);

impl FixtureRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "parley-ar01-lineage-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>(),
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn predecessor_database(path: &std::path::Path) -> SqlitePool {
    let connection = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connection)
        .await
        .unwrap();
    let current = sqlx::migrate!("../../migrations");
    let predecessor = Migrator {
        migrations: Cow::Owned(
            current
                .iter()
                .filter(|migration| migration.version <= 26)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    predecessor.run(&pool).await.unwrap();
    sqlx::raw_sql(FIXTURE).execute(&pool).await.unwrap();
    pool
}

async fn assert_intact_delivery(db: &SqlitePool) {
    let (body, digest, send_digest): (String, String, String) = sqlx::query_as(
        "SELECT mv.body,mv.content_sha256,s.content_sha256
         FROM send s JOIN send_part sp ON sp.send_id=s.id
         JOIN release_record rr ON rr.id=sp.release_id
         JOIN approval a ON a.id=rr.approval_id
         JOIN material_version mv ON mv.material_id=sp.material_id AND mv.version_no=sp.version_no
         JOIN delivered_resource_version rv ON rv.material_id=mv.material_id AND rv.version_no=mv.version_no
         JOIN material_lint ml ON ml.material_id=mv.material_id AND ml.version_no=mv.version_no
         JOIN inference_record composer ON composer.id=mv.job_id
         JOIN inference_record linter ON linter.id=ml.linter_job_id
         WHERE s.id='send' AND a.content_sha256=mv.content_sha256
           AND rv.resource_id='resource' AND composer.id='composer' AND linter.id='linter'",
    )
    .fetch_one(db)
    .await
    .unwrap();
    assert_eq!(body, "Unchanged approved delivery.");
    assert_eq!(digest, hex::encode(Sha256::digest(body.as_bytes())));
    assert_eq!(digest, send_digest);
    let immutable = sqlx::query("UPDATE material_version SET body='changed'")
        .execute(db)
        .await
        .unwrap_err();
    assert!(
        immutable
            .to_string()
            .contains("material versions are immutable")
    );
    let fk_violations: i64 = sqlx::query_scalar("SELECT count(*) FROM pragma_foreign_key_check")
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(fk_violations, 0);
}

#[tokio::test]
async fn ar01_populated_upgrade_backup_keeps_valid_lineage() {
    let root = FixtureRoot::new();
    let identity = root.0.join("workspace.sqlite");
    predecessor_database(&identity).await.close().await;
    let workspace = Workspace::open(&identity).await.unwrap();
    assert_intact_delivery(workspace.pool()).await;
    let endpoint: String =
        sqlx::query_scalar("SELECT endpoint FROM inference_profile WHERE revision=1")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(endpoint, "http://127.0.0.1:11434");
    let activations: i64 = sqlx::query_scalar("SELECT count(*) FROM inference_active_profile")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(
        activations, 0,
        "legacy provider settings require qualification"
    );
    let interrupted: (String, String) =
        sqlx::query_as("SELECT status,error_code FROM inference_record WHERE id='pending-coach'")
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(interrupted, ("failed".into(), "INTERRUPTED_UPGRADE".into()));

    let backup = create_backup_for_test(&workspace).await.unwrap();
    let settings = workspace.maintenance_settings().await.unwrap();
    let db = PathBuf::from(settings.backup_directory)
        .join(backup)
        .join("workspace.sqlite");
    assert!(
        !std::fs::read(&db)
            .unwrap()
            .windows(b"AR01_CANARY_".len())
            .any(|window| window == b"AR01_CANARY_")
    );
    let archived = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(db).foreign_keys(true))
        .await
        .unwrap();
    assert_intact_delivery(&archived).await;
    let (durable_rows, lineage_rows, chat_rows, coach_counter): (i64, i64, i64, i64) =
        sqlx::query_as(
            "SELECT (SELECT count(*) FROM durable_job),
                    (SELECT count(*) FROM inference_record),
                    (SELECT count(*) FROM agent_turn WHERE job_id='interview'),
                    (SELECT json_extract(report_json,'$.evidence.candidate_count') FROM coach_report)",
        )
        .fetch_one(&archived)
        .await
        .unwrap();
    assert_eq!(
        (durable_rows, lineage_rows, chat_rows, coach_counter),
        (0, 7, 1, 2)
    );
    let retained_capture: (String, String) = sqlx::query_as(
        "SELECT b.source_occurrence_id,j.id FROM proposal_batch b
         JOIN inference_record j ON j.id=b.job_id WHERE b.id='batch'",
    )
    .fetch_one(&archived)
    .await
    .unwrap();
    assert_eq!(retained_capture, ("occurrence".into(), "extract".into()));
    archived.close().await;
}

#[tokio::test]
async fn ar01_migration_rejects_an_orphaned_retained_reference() {
    let root = FixtureRoot::new();
    let identity = root.0.join("workspace.sqlite");
    let pool = predecessor_database(&identity).await;
    sqlx::query("UPDATE match_run SET why_job_id='legacy-orphan'")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let failure = match Workspace::open(&identity).await {
        Ok(_) => panic!("upgrade accepted orphaned historical lineage"),
        Err(error) => error,
    };
    assert!(failure.to_string().contains("INFERENCE_LINEAGE_ORPHAN"));
    assert!(identity.is_file());
}

#[tokio::test]
async fn ar01_probe_capacity_and_activation_are_database_fenced() {
    let root = FixtureRoot::new();
    let workspace = Workspace::open(root.0.join("workspace.sqlite"))
        .await
        .unwrap();
    let pool = workspace.pool();
    let start = |id: &'static str| {
        sqlx::query(
            "INSERT INTO inference_probe(id,profile_revision,epoch,status,created_at,updated_at)
             VALUES(?,1,'epoch','queued','2026-11-01','2026-11-01')",
        )
        .bind(id)
        .execute(pool)
    };
    start("probe").await.unwrap();
    assert!(
        start("second-probe")
            .await
            .unwrap_err()
            .as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
    );
    assert!(
        sqlx::query("UPDATE inference_probe SET status='running' WHERE id='probe'")
            .execute(pool)
            .await
            .is_err()
    );
    sqlx::query("UPDATE inference_probe SET status='running',lease_id='lease' WHERE id='probe'")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE inference_probe SET status='succeeded',lease_id=NULL,finished_at='2026-11-01'
         WHERE id='probe' AND status='running' AND lease_id='lease'",
    )
    .execute(pool)
    .await
    .unwrap();
    let error = sqlx::query("INSERT INTO inference_active_profile VALUES(1,1)")
        .execute(pool)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("profile needs a qualified capability")
    );
    start("second-probe").await.unwrap();
}
