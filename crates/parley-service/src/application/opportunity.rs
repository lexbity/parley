//! Typed opportunity creation (UCR-001 U03 / Q1).
//!
//! This is the one application operation behind the REST opportunity-create
//! adapter and the `opportunity.create` registry command. It owns ID
//! allocation, active primary-party validation, initial board placement,
//! policy provisioning, the two creation events and the optional
//! transactional binding of a created record into a delegated run's scope.
//! It receives a reserved transaction from the writer executor, never reads
//! a pool, and never sees a transport type.
//!
//! The typed result carries the facts the REST adapter needs to project its
//! published DTO inside the same transaction, so the create reply is no
//! longer defined by a post-commit fetch.

use serde::Serialize;
use sqlx::{Sqlite, Transaction};

use crate::{new_id, now_utc};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CreateInput {
    pub title: String,
    pub primary_party_id: String,
    /// Digest of the bounded operation key stored on the creation events.
    pub operation_key_digest: String,
    /// When present, the created opportunity is bound as a target of this run
    /// inside the create transaction. The caller must have already verified
    /// the immutable "created by this run" grant and the current lease.
    pub created_by_run: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct CreateResult {
    pub opportunity_id: String,
    pub revision: i64,
    pub primary_party_id: String,
    pub list_id: String,
    pub position: i64,
    pub event_cursor: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CreateError {
    InvalidTitle,
    TargetUnavailable,
    NoActiveList,
    Database,
}

/// Create one opportunity and its initial facts. Every write and both events
/// share the caller's transaction; a domain error leaves no effect for the
/// caller to roll back at the savepoint.
pub(crate) async fn create(
    tx: &mut Transaction<'_, Sqlite>,
    input: &CreateInput,
) -> Result<CreateResult, CreateError> {
    let title = input.title.trim();
    if title.is_empty() || title.len() > 500 {
        return Err(CreateError::InvalidTitle);
    }

    // graph_card is the active-card view: a retired party cannot be selected
    // as a primary participant.
    let party_kind: Option<String> =
        sqlx::query_scalar("SELECT kind FROM graph_card WHERE id=? AND kind IN ('person','org')")
            .bind(&input.primary_party_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| CreateError::Database)?;
    if party_kind.is_none() {
        return Err(CreateError::TargetUnavailable);
    }

    let id = new_id(&input.primary_party_id);
    let now = now_utc();
    sqlx::query("INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) VALUES(?,?,'open',1,?,?)")
        .bind(&id)
        .bind(title)
        .bind(&now)
        .bind(&now)
        .execute(&mut **tx)
        .await
        .map_err(|_| CreateError::Database)?;
    sqlx::query("INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at) VALUES(?,?,'primary',?)")
        .bind(&id)
        .bind(&input.primary_party_id)
        .bind(&now)
        .execute(&mut **tx)
        .await
        .map_err(|_| CreateError::Database)?;

    let home_list: String = sqlx::query_scalar(
        "SELECT id FROM board_list WHERE archived=0 ORDER BY position,id LIMIT 1",
    )
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| CreateError::Database)?
    .ok_or(CreateError::NoActiveList)?;
    let position: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(position),0)+1024 FROM board_placement WHERE list_id=?",
    )
    .bind(&home_list)
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| CreateError::Database)?;
    sqlx::query(
        "INSERT INTO board_placement(opportunity_id,list_id,position,revision) VALUES(?,?,?,1)",
    )
    .bind(&id)
    .bind(&home_list)
    .bind(position)
    .execute(&mut **tx)
    .await
    .map_err(|_| CreateError::Database)?;

    let placement_payload = serde_json::json!({
        "list_id": home_list,
        "position": position,
        "operation_key_digest": input.operation_key_digest,
    });
    insert_event(
        tx,
        "board_placement",
        &id,
        "board.card_created",
        &placement_payload,
    )
    .await?;
    sqlx::query("INSERT INTO party_policy(party_id) VALUES(?) ON CONFLICT(party_id) DO NOTHING")
        .bind(&input.primary_party_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| CreateError::Database)?;
    let creation_payload = serde_json::json!({
        "primary_party_id": input.primary_party_id,
        "operation_key_digest": input.operation_key_digest,
    });
    let cursor = insert_event(
        tx,
        "opportunity",
        &id,
        "opportunity.created",
        &creation_payload,
    )
    .await?;

    // Bind a self-created target in the same transaction as the effect. A
    // revoked run can no longer reach this transaction, and a replay that
    // predates revocation is re-authorized before it reaches the receipt.
    if let Some(run_id) = &input.created_by_run {
        sqlx::query(
            "INSERT OR IGNORE INTO agent_run_target(run_id,kind,target_id) VALUES(?,'opportunity',?)",
        )
        .bind(run_id)
        .bind(&id)
        .execute(&mut **tx)
        .await
        .map_err(|_| CreateError::Database)?;
    }

    Ok(CreateResult {
        opportunity_id: id,
        revision: 1,
        primary_party_id: input.primary_party_id.clone(),
        list_id: home_list,
        position,
        event_cursor: cursor.to_string(),
    })
}

async fn insert_event(
    tx: &mut Transaction<'_, Sqlite>,
    entity_type: &str,
    entity_id: &str,
    event_type: &str,
    payload: &serde_json::Value,
) -> Result<i64, CreateError> {
    sqlx::query_scalar(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,?,?,1,?,?) RETURNING cursor",
    )
    .bind(new_id(entity_id))
    .bind(now_utc())
    .bind(entity_type)
    .bind(entity_id)
    .bind(event_type)
    .bind(payload.to_string())
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| CreateError::Database)
}
