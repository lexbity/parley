//! Durable synthetic probes share the inference dispatcher. Each probe
//! exercises exactly one capability (structured output or embeddings) through
//! the packaged worker; a probe lease permits only the probed capability's
//! provider paths.

use super::{
    coach::{ProfileRow, installed_supervisor, profile_fields, tokenizer_path},
    protocol::{Capability, Completion, Execute},
};
use crate::{AppState, WorkspaceError, digest, new_id, now_utc, now_utc_offset, random_hex};
use serde::Serialize;
use sqlx::SqliteConnection;
use std::time::Duration;
use tokio::sync::watch;

#[derive(Serialize)]
pub(crate) struct Status {
    status: &'static str,
    diagnostic: Option<String>,
    checked_at: Option<String>,
    probe_id: Option<String>,
}

pub(crate) async fn status(
    conn: &mut SqliteConnection,
    revision: i64,
    epoch: &str,
    mode: &str,
) -> Result<Status, sqlx::Error> {
    let qualification: Option<String> = sqlx::query_scalar(
        "SELECT q.checked_at FROM inference_active_profile a JOIN inference_qualification q
         ON q.profile_revision=a.profile_revision WHERE a.profile_revision=?
         AND q.mode=? AND q.status='ready' AND q.checked_at >= ?
         AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=a.profile_revision)
         ORDER BY q.checked_at DESC LIMIT 1"
    ).bind(revision).bind(mode).bind(now_utc_offset(-300)).fetch_optional(&mut *conn).await?;
    let probe: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id,status,diagnostic,finished_at FROM inference_probe WHERE profile_revision=? AND epoch=? AND mode=?
         ORDER BY created_at DESC,id DESC LIMIT 1"
    ).bind(revision).bind(epoch).bind(mode).fetch_optional(&mut *conn).await?;
    if let Some(checked_at) = qualification {
        return Ok(Status {
            status: "ready",
            diagnostic: None,
            checked_at: Some(checked_at),
            probe_id: probe.map(|row| row.0),
        });
    }
    let (probe_id, status, diagnostic, checked_at) = match probe {
        Some((id, status, diagnostic, at)) => (
            Some(id),
            match status.as_str() {
                "queued" => "queued",
                "running" => "running",
                "succeeded" => "probe_succeeded",
                _ => "failed",
            },
            diagnostic,
            at,
        ),
        None => (None, "unqualified", None, None),
    };
    Ok(Status {
        status,
        diagnostic,
        checked_at,
        probe_id,
    })
}

pub(crate) async fn enqueue(
    conn: &mut SqliteConnection,
    revision: i64,
    epoch: &str,
    mode: &str,
) -> Result<bool, sqlx::Error> {
    let current: Option<(i64, String, String)> = sqlx::query_as(
        "SELECT profile_revision,epoch,mode FROM inference_probe WHERE status IN ('queued','running')",
    )
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((current_revision, current_epoch, current_mode)) = current {
        return Ok(current_revision == revision && current_epoch == epoch && current_mode == mode);
    }
    sqlx::query(
        "INSERT INTO inference_probe(id,profile_revision,epoch,mode,status,created_at,updated_at)
         VALUES(?,?,?,?,'queued',?,?)",
    )
    .bind(new_id("probe"))
    .bind(revision)
    .bind(epoch)
    .bind(mode)
    .bind(now_utc())
    .bind(now_utc())
    .execute(conn)
    .await?;
    Ok(true)
}

pub(crate) async fn recover(state: &AppState) -> Result<(), sqlx::Error> {
    let now = now_utc();
    sqlx::query("UPDATE inference_probe SET status='failed',diagnostic='INTERRUPTED',lease_id=NULL,finished_at=?,updated_at=? WHERE status='running'")
        .bind(&now).bind(&now).execute(state.workspace.pool()).await?;
    Ok(())
}

pub(crate) async fn run(state: &AppState, id: &str) {
    let pool = state.workspace.pool();
    let epoch = state.workspace.epoch();
    let token = random_hex(32);
    let claim: Option<(i64, String)> = sqlx::query_as(
        "UPDATE inference_probe SET status='running',lease_id=?,updated_at=? WHERE id=? AND epoch=?
         AND status='queued' RETURNING profile_revision,mode",
    )
    .bind(&token)
    .bind(now_utc())
    .bind(id)
    .bind(epoch)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some((revision, mode)) = claim else {
        return;
    };
    let outcome = execute(state, id, revision, &mode, &token).await;
    let (diagnostic, observed) = match outcome {
        Ok(observed) => (None, observed),
        Err(diagnostic) => (Some(diagnostic), ObservedProbe::default()),
    };
    let _ = finish(state, id, revision, &mode, &token, diagnostic, observed).await;
}

#[derive(Default)]
struct ObservedProbe {
    model_digest: Option<String>,
    dimensions: Option<u32>,
}

async fn execute(
    state: &AppState,
    id: &str,
    revision: i64,
    mode: &str,
    lease: &str,
) -> Result<ObservedProbe, &'static str> {
    let row: Option<ProfileRow> = sqlx::query_as(
        "SELECT p.endpoint,p.runner_build_digest,p.provider_version,p.chat_model,p.chat_model_digest,
                p.tokenizer_digest,p.recipe_json,p.context_tokens,p.output_tokens,
                p.embed_model,p.embed_model_digest
         FROM inference_profile p WHERE revision=?
           AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=p.revision)"
    ).bind(revision).fetch_optional(state.workspace.pool()).await.map_err(|_| "PERSISTENCE_FAILED")?;
    let (endpoint, profile, build) = row
        .and_then(|row| profile_fields(revision, row))
        .ok_or("CAPABILITY_UNQUALIFIED")?;
    let capability = match mode {
        "embed" => Capability::Embed,
        _ => Capability::StructuredRole,
    };
    if capability == Capability::Embed && profile.embed_tag.is_none() {
        return Err("CAPABILITY_UNQUALIFIED");
    }
    let supervisor = installed_supervisor()?;
    if build != supervisor.manifest_digest() {
        return Err("PROFILE_CHANGED");
    }
    let tokenizer = tokenizer_path(state, &profile)?;
    let execution = new_id("probe-worker");
    let (cancel, signal) = watch::channel(false);
    let mut stop = state.restart.subscribe();
    let request = Execute::Probe {
        profile,
        deadline_ms: 15_000,
        capability,
    };
    let result = supervisor.execute(endpoint, &execution, lease, &tokenizer, request, signal);
    tokio::pin!(result);
    let mut poll = tokio::time::interval(Duration::from_millis(50));
    let result = loop {
        tokio::select! {
            result = &mut result => break result,
            _ = stop.changed() => { cancel.send_replace(true); break result.await; }
            _ = poll.tick() => {
                let valid = sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS(SELECT 1 FROM inference_probe p JOIN workspace_meta w
                     ON w.singleton=1 AND w.epoch=p.epoch WHERE p.id=? AND p.lease_id=? AND p.status='running'
                     AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=p.profile_revision))"
                ).bind(id).bind(lease).fetch_one(state.workspace.pool()).await.unwrap_or(false);
                if !valid { cancel.send_replace(true); break result.await; }
            }
        }
    }.map_err(|error| error.code())?;
    match result {
        Completion::Probe {
            observed,
            dimensions,
            ..
        } => Ok(ObservedProbe {
            model_digest: (mode == "embed").then_some(observed.model_digest),
            dimensions,
        }),
        Completion::Delegated { .. }
        | Completion::StructuredRole { .. }
        | Completion::Embed { .. } => Err("WORKER_PROTOCOL"),
    }
}

async fn finish(
    state: &AppState,
    id: &str,
    revision: i64,
    mode: &str,
    lease: &str,
    diagnostic: Option<&str>,
    observed: ObservedProbe,
) -> Result<(), WorkspaceError> {
    let mut tx = state.workspace.pool().begin_with("BEGIN IMMEDIATE").await?;
    let now = now_utc();
    let changed = sqlx::query(
        "UPDATE inference_probe SET status=?,diagnostic=?,lease_id=NULL,finished_at=?,updated_at=?
         WHERE id=? AND status='running' AND lease_id=?
           AND epoch=(SELECT epoch FROM workspace_meta WHERE singleton=1)",
    )
    .bind(if diagnostic.is_none() {
        "succeeded"
    } else {
        "failed"
    })
    .bind(diagnostic)
    .bind(&now)
    .bind(&now)
    .bind(id)
    .bind(lease)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if changed == 1 {
        // A synthetic probe refreshes an already evaluated tuple. Qualification
        // creation and activation are independently authorized; the observed
        // model identity of a successful embed probe is recorded here.
        let tuple = digest(format!("{revision}:{mode}").as_bytes());
        let evaluation = digest(format!("probe:{mode}:{revision}").as_bytes());
        let details = if mode == "embed" && observed.dimensions.is_some() {
            serde_json::json!({"dimensions": observed.dimensions}).to_string()
        } else {
            "{}".to_owned()
        };
        sqlx::query(
            "INSERT INTO inference_qualification(
               tuple_digest,profile_revision,mode,evaluation_digest,model_digest,details_json,
               checked_at,status,diagnostic)
             VALUES(?,?,?,?,?,?,?,?,?)
             ON CONFLICT(tuple_digest) DO UPDATE SET
               checked_at=excluded.checked_at,status=excluded.status,diagnostic=excluded.diagnostic,
               model_digest=COALESCE(excluded.model_digest,inference_qualification.model_digest),
               details_json=excluded.details_json",
        )
        .bind(tuple)
        .bind(revision)
        .bind(mode)
        .bind(evaluation)
        .bind(observed.model_digest)
        .bind(details)
        .bind(&now)
        .bind(if diagnostic.is_none() {
            "ready"
        } else {
            "failed"
        })
        .bind(diagnostic)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;

    #[tokio::test]
    async fn ar07_probe_status_and_enqueue_are_durable_and_mode_scoped() {
        let mut db = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("../../migrations")
            .run(&mut db)
            .await
            .unwrap();
        let epoch: String =
            sqlx::query_scalar("SELECT epoch FROM workspace_meta WHERE singleton=1")
                .fetch_one(&mut db)
                .await
                .unwrap();
        assert_eq!(
            status(&mut db, 1, &epoch, "structured_role")
                .await
                .unwrap()
                .status,
            "unqualified"
        );
        assert!(
            enqueue(&mut db, 1, &epoch, "structured_role")
                .await
                .unwrap()
        );
        assert!(
            enqueue(&mut db, 1, &epoch, "structured_role")
                .await
                .unwrap()
        );
        assert!(!enqueue(&mut db, 1, &epoch, "embed").await.unwrap());
        let probe_id: String = sqlx::query_scalar("SELECT id FROM inference_probe")
            .fetch_one(&mut db)
            .await
            .unwrap();
        assert_eq!(
            status(&mut db, 1, &epoch, "structured_role")
                .await
                .unwrap()
                .status,
            "queued"
        );
        assert_eq!(
            status(&mut db, 1, &epoch, "embed").await.unwrap().status,
            "unqualified"
        );

        sqlx::query("UPDATE inference_probe SET status='succeeded',diagnostic=NULL,finished_at=?,updated_at=? WHERE id=?")
            .bind(now_utc()).bind(now_utc()).bind(&probe_id).execute(&mut db).await.unwrap();
        let succeeded = status(&mut db, 1, &epoch, "structured_role").await.unwrap();
        assert_eq!(succeeded.status, "probe_succeeded");
    }
}
