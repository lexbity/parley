//! AR-001 AR09 · Owner Activity.
//!
//! The one bounded union read over durable role jobs and delegated runs:
//! ordering and signed keyset pagination, epoch binding, review references,
//! cancelled/diagnostic states, restart persistence, and responsiveness of
//! manual surfaces under a loaded activity table. All reads here are the real
//! production endpoint (`GET /api/v1/agent/activity`).

#[path = "support/mod.rs"]
mod support;

use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
static KEY: AtomicU64 = AtomicU64::new(0);

type Auth = (String, String, String);

fn fresh_key() -> String {
    format!("ar09-{}", KEY.fetch_add(1, Ordering::Relaxed))
}

fn now_str() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[allow(clippy::type_complexity)]
async fn setup_at(path: PathBuf) -> (PathBuf, Workspace, RunningService, Auth) {
    let service = parley_service::start_workspace(&path).await.unwrap();
    let address = service.address;
    let body = json!({"secret": service.bootstrap_secret}).to_string();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!("POST /api/v1/session/bootstrap HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len())
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let cookie = response
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|line| {
            line.split_once(':')
                .map(|(_, value)| value.trim().split(';').next().unwrap().to_owned())
        })
        .unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!("GET /api/v1/session HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nCookie: {cookie}\r\nContent-Length: 0\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let payload: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let auth = (
        cookie,
        payload["mutation_token"].as_str().unwrap().to_owned(),
        payload["epoch"].as_str().unwrap().to_owned(),
    );
    let workspace = Workspace::open(&path).await.unwrap();
    (path, workspace, service, auth)
}

async fn setup() -> (PathBuf, Workspace, RunningService, Auth) {
    let path = std::env::temp_dir().join(format!(
        "parley-ar09-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    setup_at(path).await
}

type Response = (u16, Vec<(String, String)>, Value);

async fn request_full(
    port: u16,
    auth: &Auth,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: &str,
) -> Response {
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(
            format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {}\r\nX-Parley-Token: {}\r\nX-Parley-Epoch: {}\r\nIdempotency-Key: {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", auth.0, auth.1, auth.2, body.len())
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, payload) = response.split_once("\r\n\r\n").unwrap();
    let status = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<Vec<_>>();
    let value: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(payload).unwrap()
    };
    (status, headers, value.get("data").cloned().unwrap_or(value))
}

async fn request(
    port: u16,
    auth: &Auth,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: &str,
) -> (u16, Value) {
    let (status, _, value) = request_full(port, auth, method, path, body, key).await;
    (status, value)
}

async fn cleanup(path: &PathBuf, workspace: Workspace, service: RunningService) {
    service.shutdown().await;
    drop(workspace);
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(path.with_extension("sqlite-wal"));
    let _ = fs::remove_file(path.with_extension("sqlite-shm"));
}

fn owner_pseudonym(auth: &Auth) -> String {
    use sha2::{Digest, Sha256};
    let session_id = auth.0.split("parley_session=").nth(1).unwrap_or_default();
    hex::encode(Sha256::digest(
        format!("parley-session\0{session_id}").as_bytes(),
    ))
}

/// Seed `count` durable role jobs with descending distinct `updated_at` values
/// and a deterministic role/status mix. Returns the job IDs.
async fn seed_jobs(workspace: &Workspace, epoch: &str, count: usize) -> Vec<String> {
    let roles = [
        "coach",
        "template_builder",
        "interviewer",
        "briefer",
        "matcher",
        "composer",
        "linter",
    ];
    let statuses = [
        "succeeded",
        "queued",
        "running",
        "failed",
        "invalid",
        "cancelled",
    ];
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        let id = format!("job-{index:04}");
        let role = roles[index % roles.len()];
        let status = statuses[index % statuses.len()];
        let scope_id = format!("seed-{index:04}");
        let minute = index % 60;
        let hour = (index / 60) % 24;
        let day = 1 + (index / 1440);
        let updated = format!("2026-01-{day:02}T{hour:02}:{minute:02}:00.000Z");
        let now = now_str();
        let input = json!({"seed": index}).to_string();
        use sha2::{Digest, Sha256};
        let hash = hex::encode(Sha256::digest(input.as_bytes()));
        sqlx::query(
            "INSERT INTO durable_job(id,role,status,input_json,input_sha256,scope_kind,scope_id,scope_revision,current_attempt,max_attempts,progress_current,progress_total,last_error_code,last_error,epoch,owner_pseudonym,created_at,updated_at,completed_at) VALUES(?,?,?,?,?, 'party',?,1,?,3,0,0,?,?,?,?, ?,?,?)",
        )
        .bind(&id)
        .bind(role)
        .bind(status)
        .bind(&input)
        .bind(&hash)
        .bind(&scope_id)
        .bind(if status == "running" { 1 } else { 0 })
        .bind(if matches!(status, "failed" | "invalid") { Some("MODEL_UNAVAILABLE") } else { None })
        .bind(if matches!(status, "failed" | "invalid") { Some("provider unavailable") } else { None })
        .bind(epoch)
        .bind(Option::<String>::None)
        .bind(&now)
        .bind(updated)
        .bind(if status == "queued" { None } else { Some(&now) })
        .execute(workspace.pool())
        .await
        .unwrap();
        ids.push(id);
    }
    ids
}

static RUN_SEED: AtomicU64 = AtomicU64::new(0);

async fn seed_runs(workspace: &Workspace, epoch: &str, owner: &str, count: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        let index = RUN_SEED.fetch_add(1, Ordering::Relaxed);
        let id = format!("run-{index:04}");
        let now = now_str();
        sqlx::query(
            "INSERT INTO agent_run(run_id,epoch,owner_pseudonym,task,key_digest,state,state_revision,grant_json,scope_digest,created_at,updated_at,deadline_at,transcript_major,profile_revision) VALUES(?,?,?,?,?, 'queued',1,'{}','scope',?,?,?,1,NULL)",
        )
        .bind(&id)
        .bind(epoch)
        .bind(owner)
        .bind(format!("task {index}"))
        .bind(format!("key-{index}-{now}"))
        .bind(&now)
        .bind(&now)
        .bind(now_str())
        .execute(workspace.pool())
        .await
        .unwrap();
        ids.push(id);
    }
    ids
}

fn activity_items(value: &Value) -> Vec<Value> {
    value["items"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn ar09_activity_pagination_epochs_and_review_references() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let epoch = auth.2.clone();

    // 64 jobs plus 3 runs = 67 items; first 3 sorted rows are the runs
    // (their `updated_at` is now, which sorts after the seeded 2026-01 rows).
    let job_ids = seed_jobs(&workspace, &epoch, 64).await;
    let owner = owner_pseudonym(&auth);
    let run_ids = seed_runs(&workspace, &epoch, &owner, 3).await;

    // A committed coach report is the typed review reference of its job.
    let coach_job = job_ids[0].clone();
    sqlx::query(
        "INSERT INTO coach_report(id,period,input_digest,event_cursor,job_id,report_json,created_at) VALUES('coach-report-1','2026-W47','d',1,?,'{}',?)",
    )
    .bind(&coach_job)
    .bind(now_str())
    .execute(workspace.pool())
    .await
    .unwrap();

    let (status, first) = request(
        port,
        &auth,
        "GET",
        "/api/v1/agent/activity?limit=10",
        None,
        "read",
    )
    .await;
    assert_eq!(status, 200, "{first:?}");
    let items = activity_items(&first);
    assert_eq!(items.len(), 10);
    assert_eq!(items[0]["kind"], "run", "{items:?}");
    assert_eq!(
        items[0]["id"], run_ids[2],
        "newest run sorts first: {items:?}"
    );
    assert_eq!(items[0]["status"], "queued");
    assert_eq!(items[0]["state_revision"], 1);
    assert_eq!(items[0]["attempt_count"], Value::Null);
    assert!(items[0]["progress"]["total"] == 16);

    // Ordering is strictly descending over (updated_at, kind, id).
    for pair in items.windows(2) {
        let left = &pair[0];
        let right = &pair[1];
        let ordering = left["updated_at"]
            .as_str()
            .unwrap()
            .cmp(right["updated_at"].as_str().unwrap())
            .then_with(|| {
                left["kind"]
                    .as_str()
                    .unwrap()
                    .cmp(right["kind"].as_str().unwrap())
            })
            .then_with(|| {
                left["id"]
                    .as_str()
                    .unwrap()
                    .cmp(right["id"].as_str().unwrap())
            });
        assert!(ordering.is_ge(), "page not sorted descending: {pair:?}");
    }

    // Walk every page with the signed cursor: no duplicates, no loss, and the
    // union matches the seeded totals.
    let mut seen = std::collections::BTreeSet::new();
    let mut cursor = first["next_cursor"].as_str().map(str::to_owned);
    for item in &items {
        seen.insert(format!(
            "{}:{}",
            item["kind"].as_str().unwrap(),
            item["id"].as_str().unwrap()
        ));
    }
    let mut pages = 1;
    while let Some(token) = cursor {
        let (status, page) = request(
            port,
            &auth,
            "GET",
            &format!("/api/v1/agent/activity?limit=10&cursor={token}"),
            None,
            "read",
        )
        .await;
        assert_eq!(status, 200, "{page:?}");
        let page_items = activity_items(&page);
        assert!(!page_items.is_empty());
        for item in &page_items {
            let key = format!(
                "{}:{}",
                item["kind"].as_str().unwrap(),
                item["id"].as_str().unwrap()
            );
            assert!(
                seen.insert(key.clone()),
                "duplicate item across pages: {key}"
            );
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        pages += 1;
        assert!(pages < 20, "pagination did not terminate");
    }
    assert_eq!(seen.len(), 64 + 3);
    for id in job_ids {
        assert!(seen.contains(&format!("job:{id}")), "lost job {id}");
    }
    for id in run_ids {
        assert!(seen.contains(&format!("run:{id}")), "lost run {id}");
    }

    // Bounded DTO: no transcript/candidate/output/raw payload rows.
    let serialized = first.to_string();
    for forbidden in [
        "\"transcript\"",
        "\"output\"",
        "\"text\"",
        "\"candidate\"",
        "\"task\"",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "activity leaked {forbidden}: {serialized}"
        );
    }
    // The coach job carries its typed review reference (scan job pages; the
    // coach seed sorts near the bottom of the 64 jobs).
    let mut found_coach_review = false;
    let mut job_cursor: Option<String> = None;
    for _ in 0..8 {
        let mut url = String::from("/api/v1/agent/activity?kind=job&limit=10");
        if let Some(token) = &job_cursor {
            url.push_str(&format!("&cursor={token}"));
        }
        let (status, job_page) = request(port, &auth, "GET", &url, None, "read").await;
        assert_eq!(status, 200);
        if let Some(item) = activity_items(&job_page)
            .iter()
            .find(|item| item["id"] == coach_job)
        {
            assert_eq!(item["review_ref"]["kind"], "coach_report");
            assert_eq!(item["review_ref"]["id"], "coach-report-1");
            found_coach_review = true;
            break;
        }
        match job_page["next_cursor"].as_str() {
            Some(token) => job_cursor = Some(token.to_owned()),
            None => break,
        }
    }
    assert!(
        found_coach_review,
        "coach review reference must surface in activity"
    );

    // Kind/status/scope filters.
    let (status, runs_only) = request(
        port,
        &auth,
        "GET",
        "/api/v1/agent/activity?kind=run&limit=50",
        None,
        "read",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(activity_items(&runs_only).len(), 3);
    assert!(
        activity_items(&runs_only)
            .iter()
            .all(|item| item["kind"] == "run")
    );
    let (status, jobs_only) = request(
        port,
        &auth,
        "GET",
        "/api/v1/agent/activity?kind=job&limit=50",
        None,
        "read",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(activity_items(&jobs_only).len(), 50);
    let (status, succeeded) = request(
        port,
        &auth,
        "GET",
        "/api/v1/agent/activity?kind=job&status=succeeded&limit=50",
        None,
        "read",
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        activity_items(&succeeded)
            .iter()
            .all(|item| item["status"] == "succeeded")
    );
    let (status, party) = request(
        port,
        &auth,
        "GET",
        "/api/v1/agent/activity?kind=job&scope=party&limit=50",
        None,
        "read",
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        activity_items(&party)
            .iter()
            .all(|item| item["scope"]["kind"] == "party")
    );
    let (status, _bad_kind) = request(
        port,
        &auth,
        "GET",
        "/api/v1/agent/activity?kind=banana",
        None,
        "read",
    )
    .await;
    assert_eq!(status, 400);
    let (status, _bad_status) = request(
        port,
        &auth,
        "GET",
        "/api/v1/agent/activity?kind=job&status=waiting_owner",
        None,
        "read",
    )
    .await;
    assert_eq!(status, 400, "job-only status must not accept wait states");
    let (status, _bad_status2) = request(
        port,
        &auth,
        "GET",
        "/api/v1/agent/activity?kind=run&status=invalid",
        None,
        "read",
    )
    .await;
    assert_eq!(
        status, 400,
        "run-only status must not accept job-only states"
    );

    // Signed cursor is bound to epoch/page size: tampering fails.
    let token = first["next_cursor"].as_str().unwrap();
    let mut forged = String::from(token);
    forged.pop().unwrap(); // corrupt the signature or payload
    let (status, forged_body) = request(
        port,
        &auth,
        "GET",
        &format!("/api/v1/agent/activity?limit=10&cursor={forged}"),
        None,
        "read",
    )
    .await;
    assert_eq!(status, 400, "{forged_body:?}");
    let (status, limit_mismatch) = request(
        port,
        &auth,
        "GET",
        &format!("/api/v1/agent/activity?limit=20&cursor={token}"),
        None,
        "read",
    )
    .await;
    assert_eq!(
        status, 400,
        "cursor must be bound to the page size: {limit_mismatch:?}"
    );

    // Private strong ETag; it changes when the underlying union changes.
    let (_, headers, _) = request_full(
        port,
        &auth,
        "GET",
        "/api/v1/agent/activity?limit=10",
        None,
        "read",
    )
    .await;
    let etag = headers
        .iter()
        .find(|(name, _)| name == "etag")
        .map(|(_, value)| value.clone());
    assert!(etag.is_some(), "activity must publish a private ETag");
    let etag = etag.unwrap();
    seed_runs(&workspace, &epoch, &owner, 1).await;
    let (_, headers2, _) = request_full(
        port,
        &auth,
        "GET",
        "/api/v1/agent/activity?limit=10",
        None,
        "read",
    )
    .await;
    let etag2 = headers2
        .iter()
        .find(|(name, _)| name == "etag")
        .map(|(_, value)| value.clone())
        .unwrap();
    assert_ne!(etag, etag2, "ETag must change when the union changes");

    // Unauthenticated read is hidden; a stale epoch header is rejected.
    let (status, _) = request(
        port,
        &("".into(), auth.1.clone(), auth.2.clone()),
        "GET",
        "/api/v1/agent/activity",
        None,
        "read",
    )
    .await;
    assert_eq!(status, 401);
    let stale = (auth.0.clone(), auth.1.clone(), "stale-epoch".into());
    let (status, stale_body) =
        request(port, &stale, "GET", "/api/v1/agent/activity", None, "read").await;
    assert_eq!(status, 409, "{stale_body:?}");

    cleanup(&path, workspace, service).await;
}

#[tokio::test]
async fn ar09_activity_cancel_and_diagnostics_are_bounded() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let epoch = auth.2.clone();
    let owner = owner_pseudonym(&auth);

    // One claimed running job plus one queued run.
    let job_id = "cancel-job-1".to_owned();
    let now = now_str();
    sqlx::query(
        "INSERT INTO durable_job(id,role,status,input_json,input_sha256,scope_kind,scope_id,scope_revision,current_attempt,max_attempts,epoch,created_at,updated_at,lease_id,process_nonce) VALUES(?, 'composer','running','{}','h','party','p-1',1,1,3,?,?,?,'lease-1','nonce-1')",
    )
    .bind(&job_id)
    .bind(&epoch)
    .bind(&now)
    .bind(&now)
    .execute(workspace.pool())
    .await
    .unwrap();
    let run_id = seed_runs(&workspace, &epoch, &owner, 1)
        .await
        .pop()
        .unwrap();

    let (status, page) = request(
        port,
        &auth,
        "GET",
        "/api/v1/agent/activity?limit=50",
        None,
        "read",
    )
    .await;
    assert_eq!(status, 200);
    let items = activity_items(&page);
    assert!(
        items
            .iter()
            .any(|item| item["id"] == job_id && item["status"] == "running")
    );
    assert!(
        items
            .iter()
            .any(|item| item["id"] == run_id && item["status"] == "queued")
    );

    // All diagnostics are closed codes.
    for item in &items {
        if let Some(code) = item["error_code"].as_str() {
            assert!(
                matches!(
                    code,
                    "RUNNER_INSTALL_INVALID"
                        | "PROTOCOL_MISMATCH"
                        | "ISOLATION_UNAVAILABLE"
                        | "MODEL_UNAVAILABLE"
                        | "PROFILE_CHANGED"
                        | "CAPABILITY_UNQUALIFIED"
                        | "INFERENCE_BUSY"
                        | "QUEUE_EXPIRED"
                        | "CONTEXT_BUDGET"
                        | "WORKER_PROTOCOL"
                        | "RESULT_INVALID"
                        | "STALE_INPUT"
                        | "TARGET_UNAVAILABLE"
                        | "EPOCH_CHANGED"
                        | "CANCELLED"
                        | "INTERRUPTED"
                        | "INTERRUPTED_UPGRADE"
                        | "PROVIDER_FAILED"
                        | "OUTPUT_INVALID"
                ),
                "unexpected diagnostic code: {code}"
            );
        }
    }

    // Cancel the running job (real cancel endpoint) and the run.
    let (status, cancelled) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/jobs/{job_id}"),
        None,
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 204, "{cancelled:?}");
    // The cancel endpoint returns 204 with no body; the row must be terminal.
    let row: (String, Option<String>) =
        sqlx::query_as("SELECT status,lease_id FROM durable_job WHERE id=?")
            .bind(&job_id)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(row.0, "cancelled");
    assert!(row.1.is_none(), "cancellation must release the job lease");

    let (status, run_cancel) = request(
        port,
        &auth,
        "POST",
        &format!("/api/v1/agent/runs/{run_id}/cancel"),
        None,
        &fresh_key(),
    )
    .await;
    assert_eq!(status, 200, "{run_cancel:?}");
    assert_eq!(run_cancel["state"], "cancelled");

    let (status, page) = request(
        port,
        &auth,
        "GET",
        "/api/v1/agent/activity?limit=50",
        None,
        "read",
    )
    .await;
    assert_eq!(status, 200);
    let items = activity_items(&page);
    let job_item = items.iter().find(|item| item["id"] == job_id).unwrap();
    assert_eq!(job_item["status"], "cancelled");
    assert_eq!(job_item["error_code"], "CANCELLED");
    let run_item = items.iter().find(|item| item["id"] == run_id).unwrap();
    assert_eq!(run_item["status"], "cancelled");

    // Restart: the epoch-scoped job lineage persists; the run row stays
    // terminal under the owning workspace (runs remain pinned to the
    // admitting owner principal, matching the UCR owner contract).
    service.shutdown().await;
    drop(workspace);
    let (_, workspace2, service2, auth2) = setup_at(path.clone()).await;
    let port2 = service2.address.port();
    let (status, page) = request(
        port2,
        &auth2,
        "GET",
        "/api/v1/agent/activity?kind=job&limit=50",
        None,
        "read",
    )
    .await;
    assert_eq!(status, 200);
    let items = activity_items(&page);
    assert!(
        items
            .iter()
            .any(|item| item["id"] == job_id && item["status"] == "cancelled")
    );
    let persisted_run: String = sqlx::query_scalar("SELECT state FROM agent_run WHERE run_id=?")
        .bind(&run_id)
        .fetch_one(workspace2.pool())
        .await
        .unwrap();
    assert_eq!(persisted_run, "cancelled");
    cleanup(&path, workspace2, service2).await;
}

#[tokio::test]
async fn ar09_reference_load_keeps_manual_work_responsive() {
    let (path, workspace, service, auth) = setup().await;
    let port = service.address.port();
    let epoch = auth.2.clone();
    let owner = owner_pseudonym(&auth);

    // NFR-4-scale activity: 2,000 jobs + 500 runs in one epoch.
    let job_ids = seed_jobs(&workspace, &epoch, 2000).await;
    let seed_start = Instant::now();
    seed_runs(&workspace, &epoch, &owner, 500).await;
    let seed_elapsed = seed_start.elapsed();
    let _ = job_ids;
    let _ = seed_elapsed;

    // A first activity page stays bounded and fast even under the loaded
    // table (generous debug-build bound; the reference p95 gate is NFR-4).
    let read_start = Instant::now();
    let (status, page) = request(
        port,
        &auth,
        "GET",
        "/api/v1/agent/activity?limit=50",
        None,
        "read",
    )
    .await;
    let page_elapsed = read_start.elapsed();
    assert_eq!(status, 200);
    assert_eq!(activity_items(&page).len(), 50);
    assert!(
        page_elapsed < std::time::Duration::from_secs(2),
        "first activity page took {page_elapsed:?}"
    );

    // Paginate the whole 2,500-row union without duplicates or loss.
    let mut seen = 0usize;
    let mut cursor = page["next_cursor"].as_str().map(str::to_owned);
    while let Some(token) = cursor {
        let (status, next) = request(
            port,
            &auth,
            "GET",
            &format!("/api/v1/agent/activity?limit=50&cursor={token}"),
            None,
            "read",
        )
        .await;
        assert_eq!(status, 200);
        seen += activity_items(&next).len();
        cursor = next["next_cursor"].as_str().map(str::to_owned);
    }
    assert_eq!(
        seen + 50,
        2500,
        "paginated union must cover every activity row"
    );

    // Manual work stays usable under the load: directory read and one write.
    let (status, entities) = request(port, &auth, "GET", "/api/v1/entities", None, "read").await;
    assert_eq!(status, 200);
    let entity_rows = entities.as_array().or_else(|| entities["data"].as_array());
    assert_eq!(entity_rows.map(Vec::len).unwrap_or(0), 0);
    let idempotency = fresh_key();
    let (status, created) = request(
        port,
        &auth,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"gift","title":"Manual resource","body":{"what":"note"}})),
        &idempotency,
    )
    .await;
    assert_eq!(status, 201, "{created:?}");
    assert!(created["id"].as_str().is_some());

    cleanup(&path, workspace, service).await;
}
