//! Test-only diagnostic probe for delegated runs (TD-001 / NFR-5).
//!
//! One call returns the ledger facts a delegated failure must be classified
//! against: the run and budget rows, attempt/outcome audit pairs, durable tool
//! intents, durable jobs and attempts, the committed transcript, and the event
//! tail for the run's entity. The dump implements `Display` so an assertion can
//! attach it as failure context without changing what the assertion counts.
//!
//! This is a probe, not a second source of truth: every value comes from the
//! same SQLite workspace the service wrote. It does not reimplement parsing or
//! authorization and it never mutates the workspace.

use serde_json::{Value, json};
use sqlx::SqlitePool;
use std::fmt;

/// A bounded, read-only snapshot of a delegated run's durable state.
#[derive(Debug, Default)]
pub struct ProbeDump {
    pub run: Option<Value>,
    pub budget: Option<Value>,
    pub audit: Vec<Value>,
    pub intents: Vec<Value>,
    pub jobs: Vec<Value>,
    pub job_attempts: Vec<Value>,
    pub turns: Vec<Value>,
    pub events: Vec<Value>,
    pub notes: Vec<String>,
}

/// Collect every probe section for `run_id`. `entity_id` scopes the event tail;
/// pass `None` to use the run id itself.
pub async fn dump(pool: &SqlitePool, run_id: &str, entity_id: Option<&str>) -> ProbeDump {
    let mut out = ProbeDump::default();

    match sqlx::query_as::<
        _,
        (
            String,
            i64,
            i64,
            i64,
            Option<String>,
            Option<String>,
            String,
        ),
    >(
        "SELECT state,state_revision,turn_count,tool_count,wake_condition,wait_intent,deadline_at \
         FROM agent_run WHERE run_id=?",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some((state, revision, turns, tools, wake, wait_intent, deadline))) => {
            out.run = Some(json!({
                "state": state, "revision": revision, "turn_count": turns,
                "tool_count": tools, "wake_condition": wake,
                "wait_intent": wait_intent, "deadline_at": deadline,
            }));
        }
        Ok(None) => out.notes.push(format!("no agent_run row for {run_id}")),
        Err(error) => out.notes.push(format!("agent_run read failed: {error}")),
    }

    match sqlx::query_as::<_, (i64, i64, i64, i64, String)>(
        "SELECT invocations,distinct_intents,executed_writes,destructive_effects,updated_at \
         FROM agent_run_budget WHERE run_id=?",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some((invocations, distinct, writes, destructive, updated_at))) => {
            out.budget = Some(json!({
                "invocations": invocations, "distinct_intents": distinct,
                "executed_writes": writes, "destructive_effects": destructive,
                "updated_at": updated_at,
            }));
        }
        Ok(None) => out
            .notes
            .push(format!("no agent_run_budget row for {run_id}")),
        Err(error) => out.notes.push(format!("budget read failed: {error}")),
    }

    // Each integration test owns a private workspace, so all agent audit rows
    // belong to the run under inspection. Pair attempts with their single
    // terminal outcome on `attempt_audit_id` (the UNIQUE index guarantees at
    // most one outcome per attempt).
    match sqlx::query_as::<_, (String, String, Option<String>, String, Option<String>, Option<String>, Option<i64>, String)>(
        "SELECT audit_id,kind,command_id,policy_result,outcome_code,attempt_audit_id,duration_ms,occurred_at \
         FROM command_audit_event WHERE actor_kind='agent' \
         ORDER BY occurred_at, audit_id LIMIT 200",
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => {
            let mut attempts: Vec<Value> = Vec::new();
            let mut by_id = std::collections::BTreeMap::new();
            for (id, kind, command, policy, outcome, attempt_id, duration, at) in rows {
                let row = json!({
                    "audit_id": id, "kind": kind, "command_id": command,
                    "policy_result": policy, "outcome_code": outcome,
                    "attempt_audit_id": attempt_id, "duration_ms": duration,
                    "at": at,
                });
                if kind == "attempt" {
                    by_id.insert(id.clone(), attempts.len());
                    attempts.push(row);
                } else if let Some(index) = attempt_id.and_then(|value| by_id.get(&value).copied()) {
                    let entry = &mut attempts[index];
                    entry["outcome_code"] = outcome.unwrap_or_default().into();
                    entry["duration_ms"] = duration.into();
                } else {
                    attempts.push(row);
                }
            }
            out.audit = attempts;
        }
        Err(error) => out.notes.push(format!("audit read failed: {error}")),
    }

    match sqlx::query_as::<_, (i64, i64, String, String, String, String)>(
        "SELECT turn_no,ordinal,tool_name,state,key_digest,args_digest \
         FROM agent_tool_intent WHERE run_id=? ORDER BY turn_no,ordinal LIMIT 200",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => {
            out.intents = rows
                .into_iter()
                .map(|(turn, ordinal, name, state, key, args)| {
                    json!({"turn": turn, "ordinal": ordinal, "tool": name,
                           "state": state, "key": key, "args_digest": args})
                })
                .collect();
        }
        Err(error) => out.notes.push(format!("intent read failed: {error}")),
    }

    match sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            i64,
            i64,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT id,role,status,current_attempt,max_attempts,last_error_code,last_error \
         FROM durable_job WHERE run_id=? ORDER BY created_at,id LIMIT 50",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => {
            out.jobs = rows
                .into_iter()
                .map(|(id, role, status, attempt, max, code, error)| {
                    json!({"id": id, "role": role, "status": status,
                           "attempt": attempt, "max_attempts": max,
                           "error_code": code, "error": error})
                })
                .collect();
        }
        Err(error) => out.notes.push(format!("job read failed: {error}")),
    }

    match sqlx::query_as::<_, (String, i64, String, Option<String>, Option<i64>)>(
        "SELECT job_id,attempt_no,status,error_code,duration_ms FROM job_attempt \
         WHERE job_id IN (SELECT id FROM durable_job WHERE run_id=?) \
         ORDER BY job_id,attempt_no LIMIT 100",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => {
            out.job_attempts = rows
                .into_iter()
                .map(|(job, attempt, status, code, duration)| {
                    json!({"job": job, "attempt": attempt, "status": status,
                           "error_code": code, "duration_ms": duration})
                })
                .collect();
        }
        Err(error) => out.notes.push(format!("job_attempt read failed: {error}")),
    }

    match sqlx::query_as::<_, (i64, String, i64, String)>(
        "SELECT turn_no,role,ordinal,body FROM agent_run_turn \
         WHERE run_id=? ORDER BY turn_no,ordinal,role LIMIT 200",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => {
            out.turns = rows
                .into_iter()
                .map(|(turn, role, ordinal, body)| {
                    json!({"turn": turn, "role": role, "ordinal": ordinal, "body": body})
                })
                .collect();
        }
        Err(error) => out.notes.push(format!("transcript read failed: {error}")),
    }

    let entity = entity_id.unwrap_or(run_id);
    match sqlx::query_as::<_, (i64, String, i64, String)>(
        "SELECT cursor,event_type,revision,occurred_at FROM event \
         WHERE entity_id=? ORDER BY cursor DESC LIMIT 40",
    )
    .bind(entity)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => {
            out.events = rows
                .into_iter()
                .map(|(cursor, kind, revision, at)| {
                    json!({"cursor": cursor, "kind": kind, "revision": revision, "at": at})
                })
                .collect();
        }
        Err(error) => out.notes.push(format!("event read failed: {error}")),
    }
    out
}

/// The job-side view for failures that have no delegated run yet (a queued
/// job with a lost wake, for example). No run/budget/transcript sections apply.
pub async fn dump_job(pool: &SqlitePool, job_id: &str) -> ProbeDump {
    let mut out = ProbeDump::default();
    out.notes.push(format!("job-scoped probe for {job_id}"));

    match sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            i64,
            i64,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT id,role,status,current_attempt,max_attempts,last_error_code,last_error \
         FROM durable_job WHERE id=?",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some((id, role, status, attempt, max, code, error))) => {
            out.jobs
                .push(json!({"id": id, "role": role, "status": status,
                                 "attempt": attempt, "max_attempts": max,
                                 "error_code": code, "error": error}));
        }
        Ok(None) => out.notes.push(format!("no durable_job row for {job_id}")),
        Err(error) => out.notes.push(format!("job read failed: {error}")),
    }

    match sqlx::query_as::<_, (i64, String, Option<String>, Option<i64>)>(
        "SELECT attempt_no,status,error_code,duration_ms FROM job_attempt \
         WHERE job_id=? ORDER BY attempt_no LIMIT 10",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => {
            out.job_attempts = rows
                .into_iter()
                .map(|(attempt, status, code, duration)| {
                    json!({"attempt": attempt, "status": status,
                           "error_code": code, "duration_ms": duration})
                })
                .collect();
        }
        Err(error) => out.notes.push(format!("job_attempt read failed: {error}")),
    }

    match sqlx::query_as::<_, (i64, String, i64, String)>(
        "SELECT cursor,event_type,revision,occurred_at FROM event \
         WHERE entity_id=? ORDER BY cursor DESC LIMIT 40",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => {
            out.events = rows
                .into_iter()
                .map(|(cursor, kind, revision, at)| {
                    json!({"cursor": cursor, "kind": kind, "revision": revision, "at": at})
                })
                .collect();
        }
        Err(error) => out.notes.push(format!("event read failed: {error}")),
    }
    out
}

impl fmt::Display for ProbeDump {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "--- delegated probe ---")?;
        writeln!(f, "run:      {}", compact(&self.run))?;
        writeln!(f, "budget:   {}", compact(&self.budget))?;
        writeln!(f, "audit ({} attempt/outcome pairs):", self.audit.len())?;
        for row in &self.audit {
            writeln!(f, "  {row}")?;
        }
        writeln!(f, "intents ({}):", self.intents.len())?;
        for row in &self.intents {
            writeln!(f, "  {row}")?;
        }
        writeln!(f, "jobs ({}):", self.jobs.len())?;
        for row in &self.jobs {
            writeln!(f, "  {row}")?;
        }
        writeln!(f, "job attempts ({}):", self.job_attempts.len())?;
        for row in &self.job_attempts {
            writeln!(f, "  {row}")?;
        }
        writeln!(f, "transcript ({} rows):", self.turns.len())?;
        for row in &self.turns {
            writeln!(f, "  {}", abbreviate(row))?;
        }
        writeln!(f, "event tail ({}):", self.events.len())?;
        for row in &self.events {
            writeln!(f, "  {row}")?;
        }
        for note in &self.notes {
            writeln!(f, "note: {note}")?;
        }
        writeln!(f, "--- end probe ---")
    }
}

fn compact(value: &Option<Value>) -> String {
    value
        .as_ref()
        .map(Value::to_string)
        .unwrap_or_else(|| "none".to_owned())
}

fn abbreviate(value: &Value) -> String {
    let mut text = value.to_string();
    if text.len() > 640 {
        text.truncate(640);
        text.push_str("...");
    }
    text
}
