//! Durable turn state and lease fencing (UCR-001 §5.2, FR-9).
//!
//! A waiting run holds no lease. Every committed turn, tool result and job
//! completion rechecks the token; a lost wake or an expired lease cannot
//! install a result from an abandoned attempt.

use crate::commands::Actor;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

pub(crate) const LEASE_SECONDS: i64 = 30;
pub(crate) const MAX_TURNS: i64 = 16;
pub(crate) const MAX_TRANSCRIPT_BYTES: i64 = 256 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct Lease {
    pub token: String,
    pub deadline: String,
}

pub(crate) fn new_lease() -> Lease {
    Lease {
        token: crate::new_id("agent-lease"),
        deadline: crate::now_utc_offset(super::LEASE_SECONDS),
    }
}

/// Claim a queued run under the writer. Only this transition makes a run
/// eligible for tool execution, and it returns the actor bound to that lease.
pub(crate) async fn claim(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
    epoch: &str,
    secret: &[u8],
) -> Result<Option<Actor>, sqlx::Error> {
    let lease = new_lease();
    let updated = sqlx::query(
        "UPDATE agent_run SET state='running', lease_token=?, lease_deadline=?, state_revision=state_revision+1, updated_at=? WHERE run_id=? AND state IN ('queued','waiting_owner','waiting_job') AND lease_token IS NULL",
    )
    .bind(&lease.token)
    .bind(&lease.deadline)
    .bind(crate::now_utc())
    .bind(run_id)
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Ok(None);
    }
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT grant_json,epoch FROM agent_run WHERE run_id=?")
            .bind(run_id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((grant_json, run_epoch)) = row else {
        return Ok(None);
    };
    if run_epoch != epoch {
        return Ok(None);
    }
    let grant: Value = serde_json::from_str(&grant_json).unwrap_or_default();
    let permissions = grant
        .get("permissions")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let targets = grant
        .get("targets")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(crate::commands::ActorTarget {
                        kind: item.get("kind")?.as_str()?.to_owned(),
                        id: item.get("id")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let allow_created_opportunities = grant
        .get("allow_created_opportunities")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let principal = crate::agent_runtime::grants::AgentPrincipal::from_grant(
        run_id,
        epoch,
        &lease.token,
        secret,
    );
    Ok(Some(Actor::agent(
        principal.pseudonym,
        principal.epoch,
        principal.run_id,
        principal.lease_token,
        permissions,
        targets,
        allow_created_opportunities,
    )))
}

/// Renew a lease only when this actor still owns it.
pub(crate) async fn renew(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Actor,
) -> Result<bool, sqlx::Error> {
    // Renew the deadline without rotating the token: the actor keeps checking
    // the same token on every turn, tool result and completion.
    let deadline = new_lease().deadline;
    let updated = sqlx::query(
        "UPDATE agent_run SET lease_deadline=?, updated_at=? WHERE run_id=? AND lease_token=? AND state='running'",
    )
    .bind(&deadline)
    .bind(crate::now_utc())
    .bind(actor.run_id.as_deref().unwrap_or(""))
    .bind(actor.lease_token.as_deref().unwrap_or(""))
    .execute(&mut **tx)
    .await?;
    Ok(updated.rows_affected() == 1)
}

pub(crate) async fn record_turn(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Actor,
    turn_no: i64,
    role: &str,
    ordinal: i64,
    body: &str,
    detail: &Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO agent_run_turn(run_id,turn_no,role,ordinal,body,detail_json,created_at) VALUES(?,?,?,?,?,?,?)",
    )
    .bind(actor.run_id.as_deref().unwrap_or(""))
    .bind(turn_no)
    .bind(role)
    .bind(ordinal)
    .bind(body)
    .bind(detail.to_string())
    .bind(crate::now_utc())
    .execute(&mut **tx)
    .await?;
    let transcript_bytes: i64 =
        sqlx::query_scalar("SELECT transcript_bytes FROM agent_run WHERE run_id=?")
            .bind(actor.run_id.as_deref().unwrap_or(""))
            .fetch_one(&mut **tx)
            .await?;
    if transcript_bytes + body.len() as i64 > MAX_TRANSCRIPT_BYTES {
        return Err(sqlx::Error::Protocol("transcript quota exceeded".into()));
    }
    sqlx::query(
        "UPDATE agent_run SET turn_count=turn_count+1, transcript_bytes=transcript_bytes+?, updated_at=? WHERE run_id=?",
    )
    .bind(body.len() as i64)
    .bind(crate::now_utc())
    .bind(actor.run_id.as_deref().unwrap_or(""))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) async fn finish(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Actor,
    state: &str,
    summary: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE agent_run SET state=?, wake_condition=?, lease_token=NULL, lease_deadline=NULL, finished_at=?, updated_at=? WHERE run_id=? AND lease_token=? AND state='running'",
    )
    .bind(state)
    .bind(summary)
    .bind(crate::now_utc())
    .bind(crate::now_utc())
    .bind(actor.run_id.as_deref().unwrap_or(""))
    .bind(actor.lease_token.as_deref().unwrap_or(""))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A run that is waiting has a persisted wake condition and no active lease.
#[allow(dead_code)] // waiting states are wired with job continuations in U05
pub(crate) async fn wait(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Actor,
    condition: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE agent_run SET state='waiting_owner', wake_condition=?, lease_token=NULL, lease_deadline=NULL, updated_at=? WHERE run_id=? AND lease_token=?",
    )
    .bind(condition)
    .bind(crate::now_utc())
    .bind(actor.run_id.as_deref().unwrap_or(""))
    .bind(actor.lease_token.as_deref().unwrap_or(""))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) fn tool_result_message(name: &str, result: &Value) -> Value {
    json!({"role": "tool", "content": serde_json::to_string(result).unwrap_or_default(), "name": name})
}
