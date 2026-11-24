//! Workspace-owned dispatch. Durable rows are authoritative and notifications
//! only shorten the time between a commit and the next scan.

use crate::{AppState, agent_runtime, jobs};
use std::{sync::Arc, time::Duration};
use tokio::task::JoinHandle;

#[derive(sqlx::FromRow)]
struct Head {
    id: String,
    ready_at: String,
}

pub(crate) fn start(state: Arc<AppState>) -> JoinHandle<()> {
    let mut stop = state.restart.subscribe();
    tokio::spawn(async move {
        jobs::recover_interrupted(&state.workspace).await;
        if super::probe::recover(&state).await.is_err() {
            return;
        }
        agent_runtime::recover(&state.workspace).await;
        let mut next_class = 0;
        while !*stop.borrow() {
            let heads = ready_heads(&state).await;
            if let Ok([role, run, probe]) = heads
                && let Some(class) = select_class(
                    [role.as_ref(), run.as_ref(), probe.as_ref()],
                    &mut next_class,
                )
            {
                match class {
                    0 => jobs::process(&state, &role.expect("selected ready role").id).await,
                    1 => {
                        agent_runtime::process_run(&state, &run.expect("selected ready run").id)
                            .await
                    }
                    _ => super::probe::run(&state, &probe.expect("selected ready probe").id).await,
                }
            } else {
                tokio::select! {
                    _ = state.jobs.notified() => {}
                    _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                    _ = stop.changed() => break,
                }
            }
        }
    })
}

async fn ready_heads(state: &AppState) -> Result<[Option<Head>; 3], sqlx::Error> {
    let pool = state.workspace.pool();
    let now = crate::now_utc();
    let role = sqlx::query_as(
        "SELECT id, coalesce(eligible_at,created_at) AS ready_at FROM durable_job
         WHERE status='queued' AND (eligible_at IS NULL OR eligible_at <= ?)
         ORDER BY ready_at,id LIMIT 1",
    )
    .bind(&now)
    .fetch_optional(pool)
    .await?;
    let run = sqlx::query_as(
        "SELECT r.run_id AS id, r.updated_at AS ready_at FROM agent_run r
         WHERE r.epoch=? AND (
           r.state='queued' OR
           (r.state IN ('waiting_job','waiting_owner') AND r.deadline_at <= ?) OR
           (r.state='waiting_job' AND NOT EXISTS (
             SELECT 1 FROM durable_job j WHERE j.id=r.wake_condition
             AND j.status IN ('queued','running')
           )) OR
           (r.state='waiting_owner' AND NOT EXISTS (
             SELECT 1 FROM command_approval a WHERE a.id=r.wake_condition AND a.status='pending'
           ))
         )
         ORDER BY ready_at,id LIMIT 1",
    )
    .bind(state.workspace.epoch())
    .bind(&now)
    .fetch_optional(pool)
    .await?;
    let probe = sqlx::query_as(
        "SELECT id,created_at AS ready_at FROM inference_probe
         WHERE status='queued' AND epoch=? ORDER BY created_at,id LIMIT 1",
    )
    .bind(state.workspace.epoch())
    .fetch_optional(pool)
    .await?;
    Ok([role, run, probe])
}

fn select_class(heads: [Option<&Head>; 3], next: &mut usize) -> Option<usize> {
    const ROTATION: [usize; 6] = [0, 1, 0, 1, 0, 2];
    let starved_before = crate::now_utc_offset(-45);
    if let Some((class, _)) = heads
        .iter()
        .enumerate()
        .filter_map(|(class, head)| head.map(|head| (class, head)))
        .filter(|(_, head)| head.ready_at <= starved_before)
        .min_by_key(|(class, head)| (&head.ready_at, *class))
    {
        return Some(class);
    }
    for _ in ROTATION {
        let class = ROTATION[*next];
        *next = (*next + 1) % ROTATION.len();
        if heads[class].is_some() {
            return Some(class);
        }
    }
    None
}

pub(crate) fn process_nonce() -> &'static str {
    static NONCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NONCE.get_or_init(|| crate::random_hex(32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(ready_at: String) -> Head {
        Head {
            id: "job".into(),
            ready_at,
        }
    }

    #[test]
    fn ar01_dispatch_rotates_classes_and_promotes_starved_work() {
        let ready = crate::now_utc();
        let role = head(ready.clone());
        let run = head(ready.clone());
        let probe = head(ready);
        let refs = [Some(&role), Some(&run), Some(&probe)];
        let mut next = 0;
        let selected = (0..6)
            .map(|_| select_class(refs, &mut next).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(selected, [0, 1, 0, 1, 0, 2]);

        let role = head(crate::now_utc_offset(-60));
        let run = head(crate::now_utc_offset(-50));
        let probe = head(crate::now_utc_offset(-55));
        assert_eq!(
            select_class([Some(&role), Some(&run), Some(&probe)], &mut next),
            Some(0)
        );
        assert_eq!(select_class([None, Some(&run), None], &mut next), Some(1));
        assert_eq!(select_class([None, None, None], &mut next), None);
    }
}
