//! Command ledger: durable audit, receipts, quotas and recovery
//! (UCR-001 FR-5, §5 table, §5.1).
//!
//! The ledger stores authenticated Attempt/Outcome audit pairs and receipts
//! keyed by `(epoch, principal, command_id, key_digest)`. One normalized
//! outcome per attempt is enforced by a partial unique index. Receipts
//! expire after 24 h; logical quotas fail closed after bounded expiry
//! cleanup, with a 4 MiB reserve for terminal outcomes. The shared REST
//! receipt helpers used by the adopted phase route also live here.

use std::time::{Duration, Instant};

use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::WorkspaceError;
use crate::now_utc;

pub(crate) const RECEIPT_TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const AUDIT_LOGICAL_QUOTA: i64 = 256 * 1024 * 1024;
pub(crate) const AUDIT_TERMINAL_RESERVE: i64 = 4 * 1024 * 1024;
pub(crate) const RECEIPT_LOGICAL_QUOTA: i64 = 128 * 1024 * 1024;
pub(crate) const OWNER_RATE_PER_MINUTE: i64 = 120;
pub(crate) const RUN_RATE_PER_MINUTE: i64 = 60;
pub(crate) const WORKSPACE_RATE_PER_MINUTE: i64 = 1_200;
pub(crate) const KEY_DIGEST_DOMAIN: &[u8] = b"command-key";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Execute,
    DryRun,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Execute => "execute",
            Mode::DryRun => "dry_run",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PolicyResult {
    Allowed,
    Replayed,
    Interrupted,
    DomainError,
}

impl PolicyResult {
    pub fn as_str(&self) -> &'static str {
        match self {
            PolicyResult::Allowed => "allowed",
            PolicyResult::Replayed => "replayed",
            PolicyResult::Interrupted => "interrupted",
            PolicyResult::DomainError => "domain_error",
        }
    }
}

pub(crate) struct ReplayRecord {
    pub command_version: i64,
    pub mode: String,
    pub wire_args_digest: String,
    pub original_audit_id: String,
    pub status: i64,
    pub body: serde_json::Value,
}

/// Domain-separated HMAC key digest for a bounded operation key.
pub(crate) fn key_digest(
    generation_key: &[u8],
    epoch: &str,
    principal: &str,
    command_id: &str,
    key: &str,
) -> String {
    parley_domain::hmac::sign_hex(
        generation_key,
        KEY_DIGEST_DOMAIN,
        format!("{epoch}\0{principal}\0{command_id}\0{key}").as_bytes(),
    )
}

pub(crate) async fn lookup_receipt(
    tx: &mut Transaction<'_, Sqlite>,
    epoch: &str,
    principal: &str,
    command_id: &str,
    key_digest: &str,
) -> Result<Option<ReplayRecord>, WorkspaceError> {
    let row: Option<(i64, String, String, String, i64, String)> = sqlx::query_as(
        "SELECT command_version,mode,wire_args_digest,original_audit_id,response_status,response_bytes FROM command_receipt WHERE epoch=? AND principal_pseudonym=? AND command_id=? AND key_digest=?",
    )
    .bind(epoch)
    .bind(principal)
    .bind(command_id)
    .bind(key_digest)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((command_version, mode, wire_args_digest, original_audit_id, status, body)) = row
    else {
        return Ok(None);
    };
    let body: serde_json::Value =
        serde_json::from_str(&body).map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
    Ok(Some(ReplayRecord {
        command_version,
        mode,
        wire_args_digest,
        original_audit_id,
        status,
        body,
    }))
}

#[allow(clippy::too_many_arguments)] // ledger write fields are all required
pub(crate) async fn store_receipt(
    tx: &mut Transaction<'_, Sqlite>,
    epoch: &str,
    principal: &str,
    command_id: &str,
    command_version: i64,
    mode: Mode,
    key_digest: &str,
    wire_args_digest: &str,
    original_audit_id: &str,
    status: i64,
    body: &serde_json::Value,
    receipt_bytes: i64,
) -> Result<(), WorkspaceError> {
    let now = now_utc();
    let expires = now_utc_offset(RECEIPT_TTL.as_secs() as i64);
    let current: i64 =
        sqlx::query_scalar("SELECT receipt_bytes FROM command_ledger_meta WHERE singleton=1")
            .fetch_one(&mut **tx)
            .await?;
    if current >= RECEIPT_LOGICAL_QUOTA {
        return Err(WorkspaceError::LedgerFull);
    }
    sqlx::query(
        "INSERT INTO command_receipt(epoch,principal_pseudonym,command_id,key_digest,command_version,mode,wire_args_digest,original_audit_id,response_variant,response_status,response_bytes,committed_at,expires_at) VALUES(?,?,?,?,?,?,?,?, 'http_json',?,?,?,?)",
    )
    .bind(epoch)
    .bind(principal)
    .bind(command_id)
    .bind(key_digest)
    .bind(command_version)
    .bind(mode.as_str())
    .bind(wire_args_digest)
    .bind(original_audit_id)
    .bind(status)
    .bind(body.to_string())
    .bind(&now)
    .bind(&expires)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE command_ledger_meta SET receipt_bytes=receipt_bytes+?, updated_at=? WHERE singleton=1",
    )
    .bind(receipt_bytes)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) async fn prune_expired_receipts(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<i64, WorkspaceError> {
    let now = now_utc();
    let removed: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(length(response_bytes)+length(mode)+length(wire_args_digest)+length(original_audit_id)+128),0) FROM command_receipt WHERE expires_at <= ?",
    )
    .bind(&now)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM command_receipt WHERE expires_at <= ?")
        .bind(&now)
        .execute(&mut **tx)
        .await?;
    if removed > 0 {
        sqlx::query(
            "UPDATE command_ledger_meta SET receipt_bytes=max(receipt_bytes-?,0), updated_at=? WHERE singleton=1",
        )
        .bind(removed)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    }
    Ok(removed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmitError {
    RateLimited { limit: i64 },
    BudgetExceeded,
    Unavailable,
}

/// Authenticate a request as an admitted attempt: rate check, quota check
/// and a durable redacted attempt row. Called before the writer executor.
#[allow(clippy::too_many_arguments)] // admission record fields are all required
pub(crate) async fn admit_attempt(
    pool: &SqlitePool,
    epoch: &str,
    actor: &super::Actor,
    command_id: &str,
    command_version: u32,
    mode: Mode,
    policy_result: PolicyResult,
    key_digest: Option<&str>,
    args_json: &str,
    request_bytes: i64,
) -> Result<String, AdmitError> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|_| AdmitError::Unavailable)?;
    let _ = prune_expired_receipts(&mut tx)
        .await
        .map_err(|_| AdmitError::Unavailable)?;

    let window_start = now_utc_offset(-60);
    let owned: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM command_audit_event WHERE kind='attempt' AND epoch=? AND principal_pseudonym=? AND occurred_at > ?",
    )
    .bind(epoch)
    .bind(&actor.pseudonym)
    .bind(&window_start)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| AdmitError::Unavailable)?;
    let limit = match actor.kind {
        super::ActorKind::Owner => OWNER_RATE_PER_MINUTE,
        super::ActorKind::Agent => RUN_RATE_PER_MINUTE,
    };
    if owned >= limit {
        return Err(AdmitError::RateLimited { limit });
    }
    let aggregate: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM command_audit_event WHERE kind='attempt' AND epoch=? AND occurred_at > ?",
    )
    .bind(epoch)
    .bind(&window_start)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| AdmitError::Unavailable)?;
    if aggregate >= WORKSPACE_RATE_PER_MINUTE {
        return Err(AdmitError::RateLimited {
            limit: WORKSPACE_RATE_PER_MINUTE,
        });
    }

    let (audit_bytes, _): (i64, i64) = sqlx::query_as(
        "SELECT audit_bytes,receipt_bytes FROM command_ledger_meta WHERE singleton=1",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| AdmitError::Unavailable)?;
    // Reserve 4 MiB for terminal outcomes and recovery.
    if audit_bytes >= AUDIT_LOGICAL_QUOTA - AUDIT_TERMINAL_RESERVE {
        return Err(AdmitError::BudgetExceeded);
    }

    let audit_id = crate::new_id(command_id);
    let occurred = now_utc();
    sqlx::query(
        "INSERT INTO command_audit_event(audit_id,kind,epoch,actor_kind,principal_pseudonym,run_pseudonym,command_id,command_version,mode,policy_result,key_digest,args_json,request_bytes,occurred_at) VALUES(?, 'attempt', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&audit_id)
    .bind(epoch)
    .bind(actor.actor_kind())
    .bind(&actor.pseudonym)
    .bind(actor.run_id.as_deref())
    .bind(command_id)
    .bind(command_version as i64)
    .bind(mode.as_str())
    .bind(policy_result.as_str())
    .bind(key_digest)
    .bind(args_json)
    .bind(request_bytes)
    .bind(&occurred)
    .execute(&mut *tx)
    .await
    .map_err(|_| AdmitError::Unavailable)?;
    let est = args_json.len() as i64 + 256;
    sqlx::query(
        "UPDATE command_ledger_meta SET audit_bytes=audit_bytes+?, updated_at=? WHERE singleton=1",
    )
    .bind(est)
    .bind(occurred)
    .execute(&mut *tx)
    .await
    .map_err(|_| AdmitError::Unavailable)?;
    tx.commit().await.map_err(|_| AdmitError::Unavailable)?;
    Ok(audit_id)
}

/// Write the terminal audit outcome for an attempt plus an optional receipt.
/// Called inside the writer transaction; the effect and this outcome share
/// one commit (Q3).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn finalize_attempt(
    tx: &mut Transaction<'_, Sqlite>,
    epoch: &str,
    attempt_audit_id: &str,
    actor: &super::Actor,
    command_id: &str,
    command_version: u32,
    mode: Mode,
    policy_result: PolicyResult,
    outcome_code: Option<&str>,
    replay_pointer: Option<&str>,
    error_code: Option<&str>,
    error_message: Option<&str>,
    duration_ms: i64,
    result_bytes: i64,
    // Receipt payload (None to skip receipt storage).
    receipt: Option<ReceiptWrite<'_, '_>>,
) -> Result<(), WorkspaceError> {
    let occurred = now_utc();
    sqlx::query(
        "INSERT INTO command_audit_event(audit_id,kind,epoch,actor_kind,principal_pseudonym,run_pseudonym,command_id,command_version,mode,policy_result,outcome_code,attempt_audit_id,replay_pointer,error_code,error_message,duration_ms,result_bytes,occurred_at) VALUES(?, 'outcome', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(crate::new_id(command_id))
    .bind(epoch)
    .bind(actor.actor_kind())
    .bind(&actor.pseudonym)
    .bind(actor.run_id.as_deref())
    .bind(command_id)
    .bind(command_version as i64)
    .bind(mode.as_str())
    .bind(policy_result.as_str())
    .bind(outcome_code)
    .bind(attempt_audit_id)
    .bind(replay_pointer)
    .bind(error_code)
    .bind(error_message)
    .bind(duration_ms)
    .bind(result_bytes)
    .bind(&occurred)
    .execute(&mut **tx)
    .await?;
    // Meter the terminal row against the logical audit quota. The reserve
    // checked at admission leaves room for these rows (NFR-6).
    sqlx::query(
        "UPDATE command_ledger_meta SET audit_bytes=audit_bytes+?, updated_at=? WHERE singleton=1",
    )
    .bind(
        error_code
            .map(|code| code.len() as i64 + 192)
            .unwrap_or(192)
            + result_bytes.max(0),
    )
    .bind(&occurred)
    .execute(&mut **tx)
    .await?;
    if let Some(receipt) = receipt {
        store_receipt(
            tx,
            epoch,
            &actor.pseudonym,
            command_id,
            command_version as i64,
            mode,
            receipt.key_digest,
            receipt.wire_args_digest,
            attempt_audit_id,
            receipt.status,
            receipt.body,
            receipt.bytes,
        )
        .await?;
    }
    Ok(())
}

pub(crate) struct ReceiptWrite<'a, 'b> {
    pub key_digest: &'a str,
    pub wire_args_digest: &'a str,
    pub status: i64,
    pub body: &'b serde_json::Value,
    pub bytes: i64,
}

/// Build a redacted args record from a positive per-command allowlist.
pub(crate) fn redact_args(
    raw: &serde_json::Map<String, serde_json::Value>,
    allowlist: &[&str],
    sensitive: &[&str],
) -> String {
    let mut out = serde_json::Map::new();
    for key in allowlist {
        if let Some(value) = raw.get(*key) {
            if sensitive.contains(key) {
                out.insert(
                    (*key).to_string(),
                    serde_json::Value::String("[redacted]".into()),
                );
            } else {
                out.insert((*key).to_string(), value.clone());
            }
        }
    }
    serde_json::Value::Object(out).to_string()
}

/// One interrupted attempt row: audit ID, epoch, actor kind, principal
/// pseudonym, run pseudonym, command ID, command version and mode.
type InterruptedAttempt = (String, String, String, String, String, Option<String>, i64, String);

/// Startup finalization: every attempt without a terminal outcome from an
/// earlier process is marked interrupted.
pub(crate) async fn recover_interrupted_attempts(
    pool: &SqlitePool,
) -> Result<usize, WorkspaceError> {
    let attempts: Vec<InterruptedAttempt> = sqlx::query_as(
        "SELECT a.audit_id,a.epoch,a.actor_kind,a.principal_pseudonym,a.run_pseudonym,a.command_id,a.command_version,a.mode FROM command_audit_event a WHERE a.kind='attempt' AND NOT EXISTS(SELECT 1 FROM command_audit_event o WHERE o.kind='outcome' AND o.attempt_audit_id=a.audit_id)",
    )
    .fetch_all(pool)
    .await?;
    let interrupted = attempts.len();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let now = now_utc();
    for (
        audit_id,
        epoch,
        actor_kind,
        principal,
        run_pseudonym,
        command_id,
        command_version,
        mode,
    ) in attempts
    {
        sqlx::query(
            "INSERT INTO command_audit_event(audit_id,kind,epoch,actor_kind,principal_pseudonym,run_pseudonym,command_id,command_version,mode,policy_result,outcome_code,attempt_audit_id,occurred_at) VALUES(?, 'outcome', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(crate::new_id(&format!("interrupted:{audit_id}")))
        .bind(&epoch)
        .bind(&actor_kind)
        .bind(&principal)
        .bind(run_pseudonym)
        .bind(&command_id)
        .bind(command_version)
        .bind(&mode)
        .bind(PolicyResult::Interrupted.as_str())
        .bind("interrupted")
        .bind(&audit_id)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(interrupted)
}

fn now_utc_offset(seconds: i64) -> String {
    let value = chrono::Utc::now() + chrono::Duration::seconds(seconds);
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Shared REST receipt helpers (adopted phase route only). They retain the
/// published REST replay envelope semantics and the legacy table.
pub(crate) struct RestReplay {
    pub status: i64,
    pub body: serde_json::Value,
}

pub(crate) fn rest_canonical_digest(
    method: &str,
    target: &str,
    body: &impl serde::Serialize,
) -> String {
    let body = serde_json::to_vec(body).unwrap_or_default();
    crate::digest(&[method.as_bytes(), b"\n", target.as_bytes(), b"\n", &body].concat())
}

pub(crate) async fn rest_receipt_lookup(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    canonical: &str,
) -> Result<Option<RestReplay>, WorkspaceError> {
    let row: Option<(String, i64, String)> = sqlx::query_as(
        "SELECT canonical_digest,status_code,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=(SELECT epoch FROM workspace_meta WHERE singleton=1) AND key=?",
    )
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((stored, status, body)) = row else {
        return Ok(None);
    };
    if stored != canonical {
        return Err(WorkspaceError::IdempotencyConflict);
    }
    let body: serde_json::Value =
        serde_json::from_str(&body).map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
    Ok(Some(RestReplay { status, body }))
}

pub(crate) async fn rest_receipt_store(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    canonical: &str,
    status: i64,
    result: &serde_json::Value,
) -> Result<(), WorkspaceError> {
    sqlx::query(
        "INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,(SELECT epoch FROM workspace_meta WHERE singleton=1),?,?,?,?,(SELECT COALESCE(MAX(cursor),0) FROM event),?)",
    )
    .bind(key)
    .bind(canonical)
    .bind(status)
    .bind(result.to_string())
    .bind(now_utc())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Terminal outcome for an admitted attempt that never reached the writer
/// (admission saturation, timeout or generation fence). This keeps the
/// one-attempt/one-outcome invariant true for every admitted request even
/// when the writer itself is unavailable.
#[allow(clippy::too_many_arguments)] // outcome correlation fields are all required
pub(crate) async fn record_rejection(
    pool: &SqlitePool,
    epoch: &str,
    attempt_audit_id: &str,
    actor: &super::Actor,
    command_id: &str,
    command_version: u32,
    mode: Mode,
    outcome_code: &'static str,
) {
    let occurred = now_utc();
    let outcome_id = crate::new_id(&format!("rejected:{attempt_audit_id}"));
    let result = async {
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT OR IGNORE INTO command_audit_event(audit_id,kind,epoch,actor_kind,principal_pseudonym,run_pseudonym,command_id,command_version,mode,policy_result,outcome_code,attempt_audit_id,occurred_at) VALUES(?, 'outcome', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&outcome_id)
        .bind(epoch)
        .bind(actor.actor_kind())
        .bind(&actor.pseudonym)
        .bind(actor.run_id.as_deref())
        .bind(command_id)
        .bind(command_version as i64)
        .bind(mode.as_str())
        .bind(PolicyResult::Interrupted.as_str())
        .bind(outcome_code)
        .bind(attempt_audit_id)
        .bind(&occurred)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE command_ledger_meta SET audit_bytes=audit_bytes+192, updated_at=? WHERE singleton=1",
        )
        .bind(&occurred)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }
    .await;
    if let Err(error) = result {
        // Startup recovery finalizes the attempt pair if this best-effort
        // write could not be committed; never turn a rejection into an effect.
        eprintln!("command ledger: could not record rejection outcome: {error:?}");
    }
}

pub(crate) struct Timing {
    pub started: Instant,
}

impl Timing {
    pub fn start() -> Self {
        Timing {
            started: Instant::now(),
        }
    }

    pub fn elapsed_ms(&self) -> i64 {
        self.started.elapsed().as_millis() as i64
    }
}
