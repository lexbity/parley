//! Approval store for destructive registry commands (UCR-001 U08, FR-8,
//! Q5).
//!
//! Every destructive **registry** execution, for both actors, requires a
//! human approval. The approval row binds the exact intent: principal, run
//! if present, epoch, command/version, idempotency key digest, wire and
//! canonical args digests, the dependency fingerprint computed by the
//! command's read-only planner, and an expiry. Execution recomputes the
//! fingerprint under reservation before consuming the grant with the effect
//! (Q5); a changed dependency turns the grant stale instead of silently
//! broadening authorization.
//!
//! Lifecycle: pending → granted → consumed, with denied / expired / stale
//! terminal states and a deny-at-current-revision revocation of a granted
//! approval. Pending/granted rows live at most 10 minutes (30 minute
//! deployment ceiling). Terminal transitions purge the private args/preview
//! payloads; minimal status rows are retained until receipt expiry or
//! 24 hours, whichever expires first. Owner-only list and decision
//! endpoints are authorized by the HTTP layer; the state transitions below
//! never trust a caller-provided authority.

use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::WorkspaceError;
use crate::now_utc;

/// Pending/granted TTL from creation (Q5, §5 "command_approval").
pub(crate) const APPROVAL_TTL_SECONDS: i64 = 10 * 60;
/// Maximum deployment TTL ceiling.
pub(crate) const APPROVAL_MAX_TTL_SECONDS: i64 = 30 * 60;
/// NFR-6: host/approval payload quota.
pub(crate) const APPROVAL_PAYLOAD_QUOTA: i64 = 64 * 1024 * 1024;
/// Minimal status rows are retained until receipt expiry or 24 h.
pub(crate) const APPROVAL_RETAIN_SECONDS: i64 = 24 * 60 * 60;

/// A 128-bit server-minted approval id.
pub(crate) fn new_approval_id() -> String {
    crate::new_id("approval")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApprovalStatus {
    Pending,
    Granted,
    Consumed,
    Denied,
    Revoked,
    Expired,
    Stale,
}

impl ApprovalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalStatus::Pending => "pending",
            ApprovalStatus::Granted => "granted",
            ApprovalStatus::Consumed => "consumed",
            ApprovalStatus::Denied => "denied",
            ApprovalStatus::Revoked => "revoked",
            ApprovalStatus::Expired => "expired",
            ApprovalStatus::Stale => "stale",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "granted" => Some(Self::Granted),
            "consumed" => Some(Self::Consumed),
            "denied" => Some(Self::Denied),
            "revoked" => Some(Self::Revoked),
            "expired" => Some(Self::Expired),
            "stale" => Some(Self::Stale),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Consumed | Self::Denied | Self::Revoked | Self::Expired | Self::Stale
        )
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // ledger projection: fields are bound by the store, read via the API views
pub(crate) struct ApprovalRow {
    pub id: String,
    pub epoch: String,
    pub owner_pseudonym: String,
    pub principal_pseudonym: String,
    pub run_pseudonym: Option<String>,
    pub command_id: String,
    pub command_version: i64,
    pub key_digest: String,
    pub wire_args_digest: String,
    pub canonical_args_digest: String,
    pub dependency_fingerprint: String,
    pub preview_digest: String,
    pub status: ApprovalStatus,
    pub revision: i64,
    pub args_json: Option<String>,
    pub preview_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub expires_at: String,
    pub consumed_audit_id: Option<String>,
    pub decision_key_digest: Option<String>,
    pub decision_input_digest: Option<String>,
    pub decision_outcome_json: Option<String>,
}

impl ApprovalRow {
    /// A decision replay is exact: same decision key and identical input
    /// digest return the stored outcome; otherwise the key is mismatched.
    pub fn decision_input_digest(
        key: &str,
        expected_revision: i64,
        preview_digest: &str,
    ) -> String {
        crate::digest(format!("{key}\0{expected_revision}\0{preview_digest}").as_bytes())
    }
}

/// Raw persisted shape (sqlx tuples cap out below 23 columns).
#[derive(Debug, Clone, sqlx::FromRow)]
struct ApprovalRowRow {
    id: String,
    epoch: String,
    owner_pseudonym: String,
    principal_pseudonym: String,
    run_pseudonym: Option<String>,
    command_id: String,
    command_version: i64,
    key_digest: String,
    wire_args_digest: String,
    canonical_args_digest: String,
    dependency_fingerprint: String,
    preview_digest: String,
    status: String,
    revision: i64,
    args_json: Option<String>,
    preview_json: Option<String>,
    created_at: String,
    updated_at: String,
    expires_at: String,
    consumed_audit_id: Option<String>,
    decision_key_digest: Option<String>,
    decision_input_digest: Option<String>,
    decision_outcome_json: Option<String>,
}

fn row_from_tuple(row: ApprovalRowRow) -> Option<ApprovalRow> {
    Some(ApprovalRow {
        id: row.id,
        epoch: row.epoch,
        owner_pseudonym: row.owner_pseudonym,
        principal_pseudonym: row.principal_pseudonym,
        run_pseudonym: row.run_pseudonym,
        command_id: row.command_id,
        command_version: row.command_version,
        key_digest: row.key_digest,
        wire_args_digest: row.wire_args_digest,
        canonical_args_digest: row.canonical_args_digest,
        dependency_fingerprint: row.dependency_fingerprint,
        preview_digest: row.preview_digest,
        status: ApprovalStatus::parse(&row.status)?,
        revision: row.revision,
        args_json: row.args_json,
        preview_json: row.preview_json,
        created_at: row.created_at,
        updated_at: row.updated_at,
        expires_at: row.expires_at,
        consumed_audit_id: row.consumed_audit_id,
        decision_key_digest: row.decision_key_digest,
        decision_input_digest: row.decision_input_digest,
        decision_outcome_json: row.decision_outcome_json,
    })
}

const ROW_COLUMNS: &str = "id,epoch,owner_pseudonym,principal_pseudonym,run_pseudonym,command_id,command_version,key_digest,wire_args_digest,canonical_args_digest,dependency_fingerprint,preview_digest,status,revision,args_json,preview_json,created_at,updated_at,expires_at,consumed_audit_id,decision_key_digest,decision_input_digest,decision_outcome_json";

pub(crate) async fn lookup_by_key(
    tx: &mut Transaction<'_, Sqlite>,
    epoch: &str,
    principal: &str,
    command_id: &str,
    key_digest: &str,
) -> Result<Option<ApprovalRow>, sqlx::Error> {
    let row: Option<ApprovalRowRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ROW_COLUMNS} FROM command_approval \
         WHERE epoch=? AND principal_pseudonym=? AND command_id=? AND key_digest=? ORDER BY created_at DESC LIMIT 1"
    )))
    .bind(epoch)
    .bind(principal)
    .bind(command_id)
    .bind(key_digest)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.and_then(row_from_tuple))
}

pub(crate) async fn lookup_by_id(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<ApprovalRow>, sqlx::Error> {
    let row: Option<ApprovalRowRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ROW_COLUMNS} FROM command_approval WHERE id=?"
    )))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.and_then(row_from_tuple))
}

pub(crate) struct NewApproval<'a> {
    pub epoch: &'a str,
    pub owner_pseudonym: &'a str,
    pub principal_pseudonym: &'a str,
    pub run_pseudonym: Option<&'a str>,
    pub command_id: &'a str,
    pub command_version: i64,
    pub key_digest: &'a str,
    pub wire_args_digest: &'a str,
    pub canonical_args_digest: &'a str,
    pub dependency_fingerprint: &'a str,
    pub preview_digest: &'a str,
    /// Redacted normalized args for the owner panel (exact intent).
    pub args_json: String,
    /// The observational preview (structured impact) for the owner panel.
    pub preview_json: String,
}

/// Create a pending approval for a first destructive execution request.
/// The owning pseudonym is the owner whose inbox shows the request; the
/// principal is the invoking actor (owner or agent run).
pub(crate) async fn create_pending(
    tx: &mut Transaction<'_, Sqlite>,
    input: &NewApproval<'_>,
) -> Result<String, WorkspaceError> {
    // Bounded expiry cleanup before the quota check keeps the store
    // fail-closed only at capacity after expiry cleanup (NFR-6).
    expire_sweep(tx).await?;
    prune_terminal(tx).await?;
    let approval_bytes: i64 =
        sqlx::query_scalar("SELECT approval_bytes FROM command_ledger_meta WHERE singleton=1")
            .fetch_one(&mut **tx)
            .await?;
    let payload_bytes = input.args_json.len() as i64 + input.preview_json.len() as i64;
    if approval_bytes + payload_bytes > APPROVAL_PAYLOAD_QUOTA {
        return Err(WorkspaceError::LedgerFull);
    }
    let id = new_approval_id();
    let now = now_utc();
    let ttl_seconds = APPROVAL_TTL_SECONDS.min(APPROVAL_MAX_TTL_SECONDS);
    let expires = crate::now_utc_offset(ttl_seconds);
    sqlx::query(
        "INSERT INTO command_approval(id,epoch,owner_pseudonym,principal_pseudonym,run_pseudonym,command_id,command_version,key_digest,wire_args_digest,canonical_args_digest,dependency_fingerprint,preview_digest,status,revision,args_json,preview_json,created_at,updated_at,expires_at) \
         VALUES(?,?,?,?,?,?,?,?,?,?,?,?,'pending',1,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(input.epoch)
    .bind(input.owner_pseudonym)
    .bind(input.principal_pseudonym)
    .bind(input.run_pseudonym)
    .bind(input.command_id)
    .bind(input.command_version)
    .bind(input.key_digest)
    .bind(input.wire_args_digest)
    .bind(input.canonical_args_digest)
    .bind(input.dependency_fingerprint)
    .bind(input.preview_digest)
    .bind(&input.args_json)
    .bind(&input.preview_json)
    .bind(&now)
    .bind(&now)
    .bind(&expires)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE command_ledger_meta SET approval_bytes=approval_bytes+?, updated_at=? WHERE singleton=1",
    )
    .bind(payload_bytes)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// Purge the private args/preview payloads of one terminal row and meter the
/// freed payload quota.
async fn purge_payload(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    current_bytes: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE command_approval SET args_json=NULL, preview_json=NULL WHERE id=? AND status IN ('consumed','denied','revoked','expired','stale')",
    )
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if current_bytes > 0 {
        sqlx::query(
            "UPDATE command_ledger_meta SET approval_bytes=max(approval_bytes-?,0), updated_at=? WHERE singleton=1",
        )
        .bind(current_bytes)
        .bind(now_utc())
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Expire a single pending/granted approval (deadline passed) and purge its
/// payloads.
pub(crate) async fn expire_one(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<(), sqlx::Error> {
    let row: Option<i64> = sqlx::query_scalar(
        "SELECT length(coalesce(args_json,''))+length(coalesce(preview_json,'')) \
         FROM command_approval WHERE id=? AND status IN ('pending','granted')",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let updated = sqlx::query(
        "UPDATE command_approval SET status='expired', revision=revision+1, updated_at=? WHERE id=? AND status IN ('pending','granted')",
    )
    .bind(now_utc())
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() == 1
        && let Some(payload_bytes) = row
    {
        purge_payload(tx, id, payload_bytes).await?;
    }
    Ok(())
}

/// Expire pending/granted approvals past their TTL (bounded sweep).
pub(crate) async fn expire_sweep(tx: &mut Transaction<'_, Sqlite>) -> Result<i64, sqlx::Error> {
    let now = now_utc();
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT id, length(coalesce(args_json,''))+length(coalesce(preview_json,'')) \
         FROM command_approval WHERE status IN ('pending','granted') AND expires_at <= ?",
    )
    .bind(&now)
    .fetch_all(&mut **tx)
    .await?;
    for (id, bytes) in &rows {
        sqlx::query(
            "UPDATE command_approval SET status='expired', revision=revision+1, updated_at=? WHERE id=? AND status IN ('pending','granted')",
        )
        .bind(&now)
        .bind(id)
        .execute(&mut **tx)
        .await?;
        purge_payload(tx, id, *bytes).await?;
    }
    Ok(rows.len() as i64)
}

/// Drop terminal rows older than the 24 h retention window.
pub(crate) async fn prune_terminal(tx: &mut Transaction<'_, Sqlite>) -> Result<i64, sqlx::Error> {
    let cutoff = crate::now_utc_offset(-APPROVAL_RETAIN_SECONDS);
    let removed = sqlx::query(
        "DELETE FROM command_approval WHERE status IN ('consumed','denied','revoked','expired','stale') AND updated_at < ?",
    )
    .bind(&cutoff)
    .execute(&mut **tx)
    .await?;
    Ok(removed.rows_affected() as i64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecisionAction {
    Grant,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DecisionOutcome {
    pub id: String,
    pub revision: i64,
    pub status: ApprovalStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecisionError {
    NotFound,
    StaleRevision,
    PreviewMismatch,
    NotActionable,
    KeyMismatch,
    Database,
}

/// Apply one owner decision under the writer reservation. The decision is
/// idempotent by operation key: an exact replay returns the original
/// outcome; a reused key with different input is a terminal mismatch.
#[allow(clippy::too_many_arguments)] // decision fields are all required
pub(crate) async fn decide(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    epoch: &str,
    owner: &str,
    action: DecisionAction,
    expected_revision: i64,
    preview_digest: &str,
    key_digest: &str,
) -> Result<DecisionOutcome, DecisionError> {
    let failure = DecisionError::Database;
    let Some(row) = lookup_by_id(tx, id).await.map_err(|_| failure)? else {
        return Err(DecisionError::NotFound);
    };
    if row.epoch != epoch || row.owner_pseudonym != owner {
        return Err(DecisionError::NotFound);
    }
    let input_digest =
        ApprovalRow::decision_input_digest(key_digest, expected_revision, preview_digest);
    if let Some(prior_key) = &row.decision_key_digest
        && prior_key == key_digest
    {
        if row.decision_input_digest.as_deref() == Some(input_digest.as_str())
            && let Some(outcome) = row
                .decision_outcome_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<DecisionOutcome>(json).ok())
        {
            return Ok(outcome);
        }
        return Err(DecisionError::KeyMismatch);
    }
    if row.revision != expected_revision {
        return Err(DecisionError::StaleRevision);
    }
    if row.preview_digest != preview_digest {
        return Err(DecisionError::PreviewMismatch);
    }
    let (next_status, purge) = match (action, row.status) {
        (DecisionAction::Grant, ApprovalStatus::Pending) => (ApprovalStatus::Granted, false),
        (DecisionAction::Grant, ApprovalStatus::Granted) => {
            // Exact replay across a repeated grant at the current revision.
            return Ok(DecisionOutcome {
                id: row.id.clone(),
                revision: row.revision,
                status: ApprovalStatus::Granted,
            });
        }
        (DecisionAction::Deny, ApprovalStatus::Pending) => (ApprovalStatus::Denied, true),
        // A deny at the current revision revokes a previously granted approval.
        (DecisionAction::Deny, ApprovalStatus::Granted) => (ApprovalStatus::Revoked, true),
        (DecisionAction::Deny, ApprovalStatus::Denied) => {
            return Ok(DecisionOutcome {
                id: row.id.clone(),
                revision: row.revision,
                status: ApprovalStatus::Denied,
            });
        }
        _ => return Err(DecisionError::NotActionable),
    };
    let next_revision = row.revision + 1;
    let outcome = DecisionOutcome {
        id: row.id.clone(),
        revision: next_revision,
        status: next_status,
    };
    let outcome_json = serde_json::to_string(&outcome).map_err(|_| DecisionError::Database)?;
    sqlx::query(
        "UPDATE command_approval SET status=?, revision=?, decision_key_digest=?, decision_input_digest=?, decision_outcome_json=?, updated_at=? WHERE id=?",
    )
    .bind(next_status.as_str())
    .bind(next_revision)
    .bind(key_digest)
    .bind(&input_digest)
    .bind(&outcome_json)
    .bind(now_utc())
    .bind(id)
    .execute(&mut **tx)
    .await
    .map_err(|_| DecisionError::Database)?;
    if purge {
        let bytes = row.args_json.as_ref().map(String::len).unwrap_or(0) as i64
            + row.preview_json.as_ref().map(String::len).unwrap_or(0) as i64;
        purge_payload(tx, id, bytes)
            .await
            .map_err(|_| DecisionError::Database)?;
    }
    Ok(outcome)
}

/// Consume a granted approval inside the same savepoint as the effect. The
/// row must still be granted at the expected revision; the caller rolls the
/// savepoint back on any failed effect, which restores the grant (Q5).
pub(crate) async fn consume(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    expected_revision: i64,
    audit_id: &str,
) -> Result<bool, sqlx::Error> {
    let row: Option<(i64, i64)> = sqlx::query_as(
        "SELECT revision, length(coalesce(args_json,''))+length(coalesce(preview_json,'')) \
         FROM command_approval WHERE id=? AND status='granted'",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((revision, payload_bytes)) = row else {
        return Ok(false);
    };
    if revision != expected_revision {
        return Ok(false);
    }
    let updated = sqlx::query(
        "UPDATE command_approval SET status='consumed', consumed_audit_id=?, revision=revision+1, updated_at=? WHERE id=? AND status='granted' AND revision=?",
    )
    .bind(audit_id)
    .bind(now_utc())
    .bind(id)
    .bind(expected_revision)
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Ok(false);
    }
    purge_payload(tx, id, payload_bytes).await?;
    Ok(true)
}

/// Record a stale disposition when the reservation-time fingerprint no
/// longer matches the bound grant (Q5: stale denial over silent widening).
pub(crate) async fn mark_stale(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<(), sqlx::Error> {
    let row: Option<i64> = sqlx::query_scalar(
        "SELECT length(coalesce(args_json,''))+length(coalesce(preview_json,'')) \
         FROM command_approval WHERE id=? AND status='granted'",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let updated = sqlx::query(
        "UPDATE command_approval SET status='stale', revision=revision+1, updated_at=? WHERE id=? AND status IN ('pending','granted')",
    )
    .bind(now_utc())
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() == 1
        && let Some(payload_bytes) = row
    {
        purge_payload(tx, id, payload_bytes).await?;
    }
    Ok(())
}

/// One owner-facing list row. Terminal rows keep their minimal status and
/// omit `args`/`preview`; active rows include the exact normalized args and
/// the structured preview (FR-8).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ApprovalListItem {
    pub id: String,
    pub command_id: String,
    pub command_version: i64,
    pub revision: i64,
    pub status: String,
    pub expires_at: String,
    pub preview_digest: String,
    pub title: Option<String>,
    pub impacted_versions_count: Option<usize>,
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<serde_json::Value>,
}

pub(crate) async fn list_owner(
    pool: &SqlitePool,
    epoch: &str,
    owner: &str,
    status: Option<&str>,
    limit: usize,
    cursor: &str,
) -> Result<(Vec<ApprovalListItem>, Option<String>), sqlx::Error> {
    let status_filter = status.and_then(ApprovalStatus::parse);
    let rows: Vec<ApprovalRowRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ROW_COLUMNS} FROM command_approval \
         WHERE epoch=? AND owner_pseudonym=? AND (?='' OR status=?) AND id>? \
         ORDER BY id LIMIT ?"
    )))
    .bind(epoch)
    .bind(owner)
    .bind(status_filter.map(|status| status.as_str()).unwrap_or(""))
    .bind(status_filter.map(|status| status.as_str()).unwrap_or(""))
    .bind(cursor)
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;
    let next = rows
        .last()
        .and_then(|row| row_from_tuple(row.clone()))
        .map(|row| row.id);
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(row) = row_from_tuple(row) else {
            continue;
        };
        let preview_value = row
            .preview_json
            .as_deref()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok());
        let args_value = row
            .args_json
            .as_deref()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok());
        let active = !row.status.is_terminal();
        let title = preview_value
            .as_ref()
            .and_then(|value| value.get("title"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let impacted_versions_count = preview_value
            .as_ref()
            .and_then(|value| value.get("impacted_versions_count"))
            .and_then(serde_json::Value::as_u64)
            .map(|count| count as usize);
        items.push(ApprovalListItem {
            id: row.id,
            command_id: row.command_id,
            command_version: row.command_version,
            revision: row.revision,
            status: row.status.as_str().to_owned(),
            expires_at: row.expires_at,
            preview_digest: row.preview_digest,
            title,
            impacted_versions_count,
            run_id: row.run_pseudonym,
            args: active.then_some(args_value).flatten(),
            preview: active.then_some(preview_value).flatten(),
        });
    }
    Ok((items, next))
}
