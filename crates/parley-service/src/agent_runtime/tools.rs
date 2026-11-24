//! Server-owned tool table and dispatch (UCR-001 Q7, FR-6).
//!
//! Tool names are derived from descriptor IDs by the boot-validated mapping.
//! A provider message can only name a mapped tool; every call is persisted as
//! an intent with a host-generated operation key before it is dispatched.
//! This module holds no credential, session, HTTP header, URL or process API.

use crate::commands::{Actor, Registry};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

use super::turn;

pub(crate) const SEARCH_TOOL: &str = "commands__search";
pub(crate) const DESCRIBE_TOOL: &str = "commands__describe";
pub(crate) const RUN_TOOL: &str = "commands__run";
pub(crate) const META_TOOLS: [&str; 3] = [SEARCH_TOOL, DESCRIBE_TOOL, RUN_TOOL];

pub(crate) fn mapped_name(id: &str) -> String {
    id.replace('.', "__")
}

pub(crate) fn reverse_name(registry: &Registry, name: &str) -> Option<String> {
    if META_TOOLS.contains(&name) {
        return Some(name.to_owned());
    }
    registry
        .descriptor_ids()
        .into_iter()
        .find(|id| mapped_name(id) == name)
        .map(str::to_owned)
}

/// The tool schemas offered on one turn: at most 20 authorized Tier 1
/// commands plus the three meta-tools. Schemas carry args only.
pub(crate) fn tool_specs(registry: &Registry, permissions: &[String]) -> Vec<Value> {
    let mut tools = Vec::new();
    for descriptor in registry.agent_tier1(permissions) {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": mapped_name(descriptor.id),
                "description": descriptor.description,
                "parameters": descriptor.args_schema,
            }
        }));
    }
    tools.push(json!({
        "type": "function",
        "function": {
            "name": SEARCH_TOOL,
            "description": "Search the registered commands available to this run. Returns id, version, title, summary and effect only.",
            "parameters": {"type": "object", "additionalProperties": false, "required": ["query"], "properties": {
                "query": {"type": "string", "maxLength": 256},
                "limit": {"type": "integer", "minimum": 1, "maximum": 25}
            }}
        }
    }));
    tools.push(json!({
        "type": "function",
        "function": {
            "name": DESCRIBE_TOOL,
            "description": "Describe one registered command available to this run.",
            "parameters": {"type": "object", "additionalProperties": false, "required": ["id"], "properties": {"id": {"type": "string", "maxLength": 48}}}
        }
    }));
    tools.push(json!({
        "type": "function",
        "function": {
            "name": RUN_TOOL,
            "description": "Run one registered command with the supplied arguments. The host assigns the operation key and checks the grant again.",
            "parameters": {"type": "object", "additionalProperties": false, "required": ["id", "version", "args"], "properties": {
                "id": {"type": "string", "maxLength": 48},
                "version": {"type": "integer", "minimum": 1},
                "args": {"type": "object"}
            }}
        }
    }));
    tools
}

/// Tool intents dispatched but not yet completed: a run parked on an owner
/// approval leaves its intent `dispatched`; a resume re-dispatches the exact
/// durable invocation binding with the deterministic original key (U08).
pub(crate) async fn pending_intents(
    pool: &sqlx::SqlitePool,
    run_id: &str,
) -> Vec<(i64, i64, String, Value)> {
    let rows: Vec<(i64, i64, String, String)> = sqlx::query_as(
        "SELECT turn_no,ordinal,tool_name,args_json FROM agent_tool_intent \
         WHERE run_id=? AND state='dispatched' ORDER BY turn_no,ordinal LIMIT 8",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|(turn_no, ordinal, name, args)| {
            let args = serde_json::from_str(&args).unwrap_or_else(|_| json!({}));
            (turn_no, ordinal, name, args)
        })
        .collect()
}

#[derive(Debug)]
pub(crate) enum DispatchError {
    UnknownTool,
    InvalidArgs,
    Denied(u16, String, String),
}

/// Persist the intent under `BEGIN IMMEDIATE` before any effect or provider
/// acknowledgement. The same row reconciles a restart at the same ordinal.
/// The wire args travel with the intent so a run parked on an approval can
/// re-dispatch the exact durable invocation binding after the owner grants
/// (Q7 / U08). The key is deterministic from the run/turn/ordinal so the
/// same intent reconciles to the same idempotency key after a restart.
#[allow(clippy::too_many_arguments)] // intent row fields are all required
pub(crate) async fn record_intent(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Actor,
    turn_no: i64,
    ordinal: i64,
    name: &str,
    key_digest: &str,
    args_digest: &str,
    args_json: &str,
) -> Result<(), sqlx::Error> {
    let run_id = actor.run_id.as_deref().unwrap_or("");
    sqlx::query(
        "INSERT OR IGNORE INTO agent_tool_intent(run_id,turn_no,ordinal,tool_name,key_digest,args_digest,args_json,state,created_at) VALUES(?,?,?,?,?,?,?,'pending',?)",
    )
    .bind(run_id)
    .bind(turn_no)
    .bind(ordinal)
    .bind(name)
    .bind(key_digest)
    .bind(args_digest)
    .bind(args_json)
    .bind(crate::now_utc())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Bind the SDK correlation id to a journaled intent when the host accepts a
/// complete assistant turn. No tool may execute before this update commits.
pub(crate) async fn accept_intent(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
    turn_no: i64,
    ordinal: i64,
    call_ref: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE agent_tool_intent SET call_ref=?, accepted_at=?
          WHERE run_id=? AND turn_no=? AND ordinal=? AND state='pending'",
    )
    .bind(call_ref)
    .bind(crate::now_utc())
    .bind(run_id)
    .bind(turn_no)
    .bind(ordinal)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The deterministic operation key for a tool intent (U08).
pub(crate) fn tool_key(run_id: &str, turn_no: i64, ordinal: i64) -> String {
    format!("tool-{run_id}-{turn_no}-{ordinal}")
}

pub(crate) async fn mark_state(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
    turn_no: i64,
    ordinal: i64,
    state: &str,
    result_ref: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE agent_tool_intent SET state=?, result_ref=? WHERE run_id=? AND turn_no=? AND ordinal=? AND state IN ('pending','accepted','dispatched')",
    )
    .bind(state)
    .bind(result_ref)
    .bind(run_id)
    .bind(turn_no)
    .bind(ordinal)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Dispatch one mapped tool. Discovery tools stay within the registry; the
/// run tool goes through the same admission, policy, ledger and writer path
/// as an owner invocation but with a server-derived agent actor.
pub(crate) async fn dispatch(
    state: &crate::AppState,
    actor: &Actor,
    turn_no: i64,
    ordinal: i64,
    name: &str,
    args: &Value,
) -> Result<Value, DispatchError> {
    let Some(id) = reverse_name(&state.registry, name) else {
        return Err(DispatchError::UnknownTool);
    };
    let run_id = actor.run_id.as_deref().unwrap_or("");
    let key = tool_key(run_id, turn_no, ordinal);
    let secret = state.workspace.command_secret();
    let key_digest =
        crate::commands::ledger::key_digest(&secret, &actor.epoch, &actor.pseudonym, &id, &key);
    let args_digest = crate::digest(
        parley_domain::jcs::canonicalize_value(args)
            .unwrap_or_default()
            .as_bytes(),
    );
    let mut tx = state
        .workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|_| {
            DispatchError::Denied(503, "unavailable".into(), "Ledger is unavailable".into())
        })?;
    record_intent(
        &mut tx,
        actor,
        turn_no,
        ordinal,
        name,
        &key_digest,
        &args_digest,
        &args.to_string(),
    )
    .await
    .map_err(|_| {
        DispatchError::Denied(
            503,
            "unavailable".into(),
            "Intent could not be recorded".into(),
        )
    })?;
    mark_state(&mut tx, run_id, turn_no, ordinal, "dispatched", None)
        .await
        .map_err(|_| {
            DispatchError::Denied(
                503,
                "unavailable".into(),
                "Intent could not be dispatched".into(),
            )
        })?;
    tx.commit().await.map_err(|_| {
        DispatchError::Denied(
            503,
            "unavailable".into(),
            "Intent could not be committed".into(),
        )
    })?;

    let result = if id == SEARCH_TOOL {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .min(25) as usize;
        let items = state
            .registry
            .agent_search(query, limit, &actor.permissions);
        json!({"items": items, "next_cursor": null})
    } else if id == DESCRIBE_TOOL {
        let command_id = args.get("id").and_then(Value::as_str).unwrap_or_default();
        match state.registry.agent_visible(command_id, &actor.permissions) {
            Some(descriptor) => serde_json::to_value(descriptor).unwrap_or_default(),
            None => {
                return Err(DispatchError::Denied(
                    404,
                    "unknown_command".into(),
                    "Command is not available".into(),
                ));
            }
        }
    } else if id == RUN_TOOL {
        if !args.is_object() {
            return Err(DispatchError::InvalidArgs);
        }
        let command_id = args.get("id").and_then(Value::as_str).unwrap_or_default();
        let version = args.get("version").and_then(Value::as_u64).unwrap_or(0) as u32;
        let command_args = args.get("args").cloned().unwrap_or_else(|| json!({}));
        let (status, reply, approval_id) = crate::commands::http::invoke_agent(
            state,
            actor.clone(),
            command_id,
            version,
            &key,
            command_args,
        )
        .await;
        if status >= 400 {
            let error = reply.get("error");
            let code = error
                .and_then(|value| value.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("invalid_args")
                .to_owned();
            let message = error
                .and_then(|value| value.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("Tool call failed")
                .to_owned();
            if code == "approval_required"
                && let Some(approval_id) = approval_id
            {
                // Park the run on the durable approval (Q7). The model sees
                // only the safe waiting denial; the approval id never leaves
                // the service boundary.
                let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
                    Ok(tx) => tx,
                    Err(_) => {
                        return Err(DispatchError::Denied(
                            503,
                            "unavailable".into(),
                            "Ledger is unavailable".into(),
                        ));
                    }
                };
                let _ = turn::wait(&mut tx, actor, "waiting_owner", &approval_id).await;
                let _ = tx.commit().await;
                return Err(DispatchError::Denied(
                    403,
                    "waiting_for_owner".into(),
                    "Waiting for owner approval".into(),
                ));
            }
            return Err(DispatchError::Denied(status, code, message));
        }
        reply
    } else {
        // A Tier 1 command tool: the args object is the command's args.
        let version = state
            .registry
            .command(&id)
            .map(|item| item.version)
            .unwrap_or(1);
        let (status, reply, approval_id) = crate::commands::http::invoke_agent(
            state,
            actor.clone(),
            &id,
            version,
            &key,
            args.clone(),
        )
        .await;
        if status >= 400 {
            let error = reply.get("error");
            let code = error
                .and_then(|value| value.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("invalid_args")
                .to_owned();
            let message = error
                .and_then(|value| value.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("Tool call failed")
                .to_owned();
            if code == "approval_required"
                && let Some(approval_id) = approval_id
            {
                let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
                    Ok(tx) => tx,
                    Err(_) => {
                        return Err(DispatchError::Denied(
                            503,
                            "unavailable".into(),
                            "Ledger is unavailable".into(),
                        ));
                    }
                };
                let _ = turn::wait(&mut tx, actor, "waiting_owner", &approval_id).await;
                let _ = tx.commit().await;
                return Err(DispatchError::Denied(
                    403,
                    "waiting_for_owner".into(),
                    "Waiting for owner approval".into(),
                ));
            }
            return Err(DispatchError::Denied(status, code, message));
        }
        reply
    };

    let mut tx = state
        .workspace
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|_| {
            DispatchError::Denied(503, "unavailable".into(), "Ledger is unavailable".into())
        })?;
    mark_state(
        &mut tx,
        run_id,
        turn_no,
        ordinal,
        "completed",
        Some(&result.to_string()),
    )
    .await
    .map_err(|_| {
        DispatchError::Denied(
            503,
            "unavailable".into(),
            "Intent could not be finalized".into(),
        )
    })?;
    tx.commit().await.map_err(|_| {
        DispatchError::Denied(
            503,
            "unavailable".into(),
            "Intent could not be finalized".into(),
        )
    })?;

    // A job-start command parks the run until its job reaches a terminal
    // state: the lease is released, the wake condition is persisted and the
    // runtime scan resumes the run after the worker publishes a reviewable
    // proposal or a terminal failure (UCR-001 U05/U07). Tier 2 commands arrive
    // through the commands__run meta-tool, so the inner id is read from its
    // arguments.
    let is_job_start = if id == RUN_TOOL {
        args.get("id").and_then(Value::as_str).is_some_and(|inner| {
            matches!(
                inner,
                crate::commands::jobs::BRIEF_START_ID
                    | crate::commands::writing::MATCH_START_ID
                    | crate::commands::writing::COMPOSE_DRAFT_ID
            )
        })
    } else {
        matches!(
            id.as_str(),
            crate::commands::jobs::BRIEF_START_ID
                | crate::commands::writing::MATCH_START_ID
                | crate::commands::writing::COMPOSE_DRAFT_ID
        )
    };
    if is_job_start {
        let job_id = result
            .get("result")
            .and_then(|value| value.get("job_id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !job_id.is_empty() {
            let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
                Ok(tx) => tx,
                Err(_) => {
                    return Err(DispatchError::Denied(
                        503,
                        "unavailable".into(),
                        "Ledger is unavailable".into(),
                    ));
                }
            };
            let _ = turn::wait(&mut tx, actor, "waiting_job", job_id).await;
            let _ = tx.commit().await;
        }
    }

    Ok(result)
}
