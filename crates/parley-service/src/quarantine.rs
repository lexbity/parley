//! Draft-00 workspace quarantine: move legacy tables out of the migrated
//! namespace and convert the records that have an unambiguous mapping.

use crate::*;
use sqlx::Row;

pub(super) async fn quarantine_draft00_schema(pool: &SqlitePool) -> Result<bool, WorkspaceError> {
    let has_party: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='party'",
    )
    .fetch_one(pool)
    .await?;
    let has_meta: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='workspace_meta'",
    )
    .fetch_one(pool)
    .await?;
    if has_party == 0 || has_meta != 0 {
        return Ok(false);
    }

    let mut connection = pool.acquire().await?;
    sqlx::query("PRAGMA foreign_keys=OFF")
        .execute(&mut *connection)
        .await?;
    let result = async {
        let mut tx = connection.begin_with("BEGIN IMMEDIATE").await?;
        let triggers: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='trigger' AND name NOT LIKE 'sqlite_%'",
        )
        .fetch_all(&mut *tx)
        .await?;
        for trigger in triggers {
            let quoted = trigger.replace('"', "\"\"");
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP TRIGGER IF EXISTS \"{quoted}\"")))
                .execute(&mut *tx)
                .await?;
        }
        let indexes: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='index' AND sql IS NOT NULL AND name NOT LIKE 'sqlite_%'",
        )
        .fetch_all(&mut *tx)
        .await?;
        for index in indexes {
            let quoted = index.replace('"', "\"\"");
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP INDEX IF EXISTS \"{quoted}\"")))
                .execute(&mut *tx)
                .await?;
        }
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(&mut *tx)
        .await?;
        for table in tables {
            if table.starts_with("draft00_") {
                return Err(WorkspaceError::Invalid(
                    "legacy Draft-00 conversion namespace already exists".into(),
                ));
            }
            let quoted = table.replace('"', "\"\"");
            let renamed = format!("draft00_{table}").replace('"', "\"\"");
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("ALTER TABLE \"{quoted}\" RENAME TO \"{renamed}\"")))
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok::<(), WorkspaceError>(())
    }
    .await;
    let enable = sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut *connection)
        .await;
    result?;
    enable?;
    Ok(true)
}

/// Deterministic Draft-00 party conversion. Legacy bytes are retained in
/// draft00_* tables; incomplete/unrecognized rows remain visible as diagnostics.
pub(super) async fn convert_draft00_parties(pool: &SqlitePool) -> Result<(), WorkspaceError> {
    let present: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='draft00_party'",
    )
    .fetch_one(pool)
    .await?;
    if present == 0 {
        return Ok(());
    }
    let columns: Vec<String> = sqlx::query("PRAGMA table_info(draft00_party)")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| r.get::<String, _>("name"))
        .collect();
    let rows = sqlx::query("SELECT * FROM draft00_party")
        .fetch_all(pool)
        .await?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    for row in rows {
        let source_key = legacy_value(&row, &columns, "id").unwrap_or_default();
        let name = legacy_value(&row, &columns, "name").unwrap_or_default();
        let kind = legacy_value(&row, &columns, "kind").unwrap_or_else(|| "person".into());
        if source_key.is_empty()
            || name.trim().is_empty()
            || !["person", "org"].contains(&kind.as_str())
        {
            sqlx::query("INSERT INTO legacy_conversion_diagnostic(source_format,source_table,source_key,code,detail,created_at) VALUES('draft-00','party',?,'AMBIGUOUS_PARTY','Missing stable key/name or unsupported party kind',?)")
                .bind(source_key).bind(now_utc()).execute(&mut *tx).await?;
            continue;
        }
        let identity = format!("draft-00\0party\0{source_key}");
        let entity_id = format!(
            "legacy-{}",
            hex::encode(Sha256::digest(identity.as_bytes()))
        );
        let opportunity_id = format!(
            "legacy-opportunity-{}",
            hex::encode(Sha256::digest(
                format!("draft-00\0opportunity\0{source_key}").as_bytes()
            ))
        );
        let stage_class =
            legacy_value(&row, &columns, "stage_class").unwrap_or_else(|| "identified".into());
        let state = match stage_class.as_str() {
            "resolved" => "resolved",
            "dormant" => "dormant",
            "closed" => "closed",
            _ => "open",
        };
        let dnc = legacy_value(&row, &columns, "do_not_contact")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
            .clamp(0, 1);
        let rung = legacy_value(&row, &columns, "highest_rung")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
            .max(0);
        let touches = legacy_value(&row, &columns, "touches")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
            .max(0);
        let stamp = now_utc();
        sqlx::query(
            "INSERT INTO entity(id,kind,name,revision,created_at,updated_at) VALUES(?,?,?,1,?,?)",
        )
        .bind(&entity_id)
        .bind(&kind)
        .bind(name.trim())
        .bind(&stamp)
        .bind(&stamp)
        .execute(&mut *tx)
        .await?;
        let policy_state = if state == "resolved" { "closed" } else { state };
        sqlx::query("INSERT INTO party_policy(party_id,state,do_not_contact,highest_rung,touch_count,revision) VALUES(?,?,?,?,?,1)")
            .bind(&entity_id).bind(policy_state).bind(dnc).bind(rung).bind(touches).execute(&mut *tx).await?;
        let title = format!("Legacy: {}", name.trim());
        sqlx::query("INSERT INTO opportunity(id,title,state,close_reason,revision,created_at,updated_at) VALUES(?,?,?, ?,1,?,?)")
            .bind(&opportunity_id).bind(title).bind(state).bind(if state == "open" { None } else { Some("draft00_migration") }).bind(&stamp).bind(&stamp).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES(?,?,'primary',?)")
            .bind(&opportunity_id).bind(&entity_id).bind(&stamp).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO legacy_key_map(source_format,source_table,source_key,destination_id,status) VALUES('draft-00','party',?,?,'converted')")
            .bind(&source_key).bind(&entity_id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO legacy_key_map(source_format,source_table,source_key,destination_id,status) VALUES('draft-00','opportunity',?,?,'converted')")
            .bind(source_key).bind(opportunity_id).execute(&mut *tx).await?;
    }
    // Mark untranslated legacy tables explicitly, without inventing facts,
    // provenance, recipient consent, or reply deadlines.
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'draft00_%'",
    )
    .fetch_all(&mut *tx)
    .await?;
    for table in tables.into_iter().filter(|t| t != "draft00_party") {
        sqlx::query("INSERT INTO legacy_conversion_diagnostic(source_format,source_table,code,detail,created_at) VALUES('draft-00',?,'RETAINED_UNMAPPED','Original rows retained; no safe automatic semantic conversion is defined for this record type',?)")
            .bind(table.trim_start_matches("draft00_")).bind(now_utc()).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

fn legacy_value(row: &sqlx::sqlite::SqliteRow, columns: &[String], field: &str) -> Option<String> {
    if !columns.iter().any(|c| c == field) {
        return None;
    }
    if let Ok(value) = row.try_get::<String, _>(field) {
        return Some(value);
    }
    if let Ok(value) = row.try_get::<i64, _>(field) {
        return Some(value.to_string());
    }
    None
}
