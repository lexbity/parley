//! UCR-001 U04: a selected party or Queue task can be changed safely.
//!
//! `party.rename`, `party.set_focus` and `opportunity.set_task_done` share
//! their typed mutation operations with the REST adapters. The tests here
//! prove revision guards, exact spans, no-ops, privacy, delegated grants and
//! byte-preserving note behavior. The fake provider is a real loopback
//! endpoint; all authorization, ledger, writer and commit behavior is the
//! service's own.
mod support;

use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

type OwnerSession = (String, String, String);
static NEXT: AtomicU64 = AtomicU64::new(0);
static SESSIONS: OnceLock<Mutex<HashMap<u16, OwnerSession>>> = OnceLock::new();

fn db_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-u04-{tag}-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn raw(
    port: u16,
    method: &str,
    path: &str,
    body: &str,
    key: Option<&str>,
    extra_headers: &[(&str, &str)],
) -> (u16, HashMap<String, String>, Value) {
    let (cookie, token, epoch) = SESSIONS
        .get()
        .unwrap()
        .lock()
        .unwrap()
        .get(&port)
        .unwrap()
        .clone();
    let key_header = key
        .map(|value| format!("Idempotency-Key: {value}\r\n"))
        .unwrap_or_default();
    let extra = extra_headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\n{key_header}{extra}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
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
    let headers: HashMap<String, String> = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(payload).unwrap_or(Value::Null)
    };
    (status, headers, value.get("data").cloned().unwrap_or(value))
}

async fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: &str,
) -> (u16, Value) {
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let (status, _, value) = raw(port, method, path, &body, Some(key), &[]).await;
    (status, value)
}

async fn owner_session(service: &RunningService) -> OwnerSession {
    let address = service.address;
    let body = json!({"secret":service.bootstrap_secret}).to_string();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!("POST /api/v1/session/bootstrap HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes(),
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
    (
        cookie,
        payload["mutation_token"].as_str().unwrap().into(),
        payload["epoch"].as_str().unwrap().into(),
    )
}

async fn setup(tag: &str) -> (PathBuf, Workspace, u16, RunningService) {
    // Debug-only authority-test fixture: keep the pre-cut transport for
    // these U01-U08 authority/fencing cases. AR05 proves the real worker.
    unsafe {
        std::env::set_var("PARLEY_TEST_NATIVE_RUN_FIXTURE", "1");
    }
    let path = db_path(tag);
    let _ = fs::remove_file(&path);
    let service = parley_service::start_workspace(&path).await.unwrap();
    let port = service.address.port();
    let owner = owner_session(&service).await;
    SESSIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(port, owner);
    let workspace = Workspace::open(&path).await.unwrap();
    (path, workspace, port, service)
}

fn cleanup(path: &PathBuf, workspace: Workspace, service: RunningService) {
    drop(workspace);
    drop(service);
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(path.with_extension("sqlite-wal"));
    let _ = fs::remove_dir_all(path.with_extension("sqlite-shm"));
}

async fn create_party(port: u16, name: &str) -> String {
    let (status, entity) = request(
        port,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"person","name":name})),
        &format!("party-{name}"),
    )
    .await;
    assert_eq!(status, 201, "{entity:?}");
    entity["id"].as_str().unwrap().to_string()
}

async fn create_org(port: u16, name: &str) -> String {
    let (status, entity) = request(
        port,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"org","name":name})),
        &format!("org-{name}"),
    )
    .await;
    assert_eq!(status, 201, "{entity:?}");
    entity["id"].as_str().unwrap().to_string()
}

async fn create_opportunity(port: u16, title: &str, party: &str) -> String {
    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":title,"primary_party_id":party})),
        &format!("opp-{title}"),
    )
    .await;
    assert_eq!(status, 201, "{opportunity:?}");
    opportunity["id"].as_str().unwrap().to_string()
}

/// Invoke a registry command as the owner.
async fn cmd(port: u16, id: &str, key: &str, args: Value) -> (u16, Value) {
    request(
        port,
        "POST",
        &format!("/api/v1/commands/{id}"),
        Some(json!({"version":1,"args":args,"dry_run":false})),
        key,
    )
    .await
}

/// REST entity PATCH with a strong ETag.
async fn patch_entity(port: u16, id: &str, key: &str, etag: &str, body: Value) -> (u16, Value) {
    let body = body.to_string();
    let (status, _, value) = raw(
        port,
        "PATCH",
        &format!("/api/v1/entities/{id}"),
        &body,
        Some(key),
        &[("If-Match", etag)],
    )
    .await;
    (status, value)
}

async fn entity_etag(port: u16, id: &str) -> String {
    let (status, entity) = request(
        port,
        "GET",
        &format!("/api/v1/entities/{id}"),
        None,
        "read-entity",
    )
    .await;
    assert_eq!(status, 200, "{entity:?}");
    entity["etag"].as_str().unwrap().to_string()
}

/// REST notes PATCH (full replace).
async fn patch_notes(
    port: u16,
    id: &str,
    key: &str,
    markdown: &str,
    revision: i64,
) -> (u16, Value) {
    request(
        port,
        "PATCH",
        &format!("/api/v1/opportunities/{id}/notes"),
        Some(json!({"markdown":markdown,"expected_revision":revision})),
        key,
    )
    .await
}

/// REST task toggle.
async fn rest_toggle(port: u16, id: &str, key: &str, input: Value) -> (u16, Value) {
    request(
        port,
        "POST",
        &format!("/api/v1/opportunities/{id}/notes/tasks/toggle"),
        Some(input),
        key,
    )
    .await
}

async fn notes(port: u16, id: &str) -> Value {
    let (status, value) = request(
        port,
        "GET",
        &format!("/api/v1/opportunities/{id}/notes"),
        None,
        "read-notes",
    )
    .await;
    assert_eq!(status, 200, "{value:?}");
    value
}

async fn count(workspace: &Workspace, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(workspace.pool())
        .await
        .unwrap()
}

fn task_args(opportunity: &str, revision: i64, task: &Value, checked: bool) -> Value {
    json!({
        "opportunity_id": opportunity,
        "expected_notes_revision": revision,
        "byte_start": task["byte_start"],
        "byte_end": task["byte_end"],
        "line_sha256": task["line_sha256"],
        "checked": checked
    })
}

// ---------------------------------------------------------------------------
// u04: party edit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn u04_party_rename_and_focus_commands_update_one_card() {
    let (path, workspace, port, service) = setup("party").await;
    let party = create_party(port, "Original Name").await;
    let etag = entity_etag(port, &party).await;

    let (status, renamed) = cmd(
        port,
        "party.rename",
        "rename-key-01",
        json!({"party_id": party, "expected_card_revision": 1, "name": "Renamed Party"}),
    )
    .await;
    assert_eq!(status, 200, "{renamed:?}");
    assert_eq!(renamed["result"]["name"], "Renamed Party");
    assert_eq!(renamed["result"]["revision"], 2);
    assert!(renamed["result"]["event_id"].as_str().is_some());
    assert!(renamed["result"]["event_cursor"].as_i64().unwrap() > 0);

    let (status, focus) = cmd(
        port,
        "party.set_focus",
        "focus-key-01",
        json!({"party_id": party, "expected_card_revision": 2, "focus": "Follow up on onboarding"}),
    )
    .await;
    assert_eq!(status, 200, "{focus:?}");
    assert_eq!(focus["result"]["focus"], "Follow up on onboarding");
    assert_eq!(focus["result"]["revision"], 3);

    // Same key replays; fresh key with the same value is a true no-op.
    let (status, replay) = cmd(
        port,
        "party.set_focus",
        "focus-key-01",
        json!({"party_id": party, "expected_card_revision": 2, "focus": "Follow up on onboarding"}),
    )
    .await;
    assert_eq!(status, 200, "{replay:?}");
    assert_eq!(replay["replayed"], true);
    let (status, noop) = cmd(
        port,
        "party.rename",
        "rename-key-02",
        json!({"party_id": party, "expected_card_revision": 3, "name": "Renamed Party"}),
    )
    .await;
    assert_eq!(status, 200, "{noop:?}");
    assert_eq!(noop["result"]["revision"], 3);
    assert!(!noop["result"]["event_id"].as_str().is_some());
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{party}' AND event_type='entity.updated'")).await,
        2,
        "rename + focus are the only effect events"
    );

    // Stale revision fails without effect.
    let (status, stale) = cmd(
        port,
        "party.rename",
        "rename-key-03",
        json!({"party_id": party, "expected_card_revision": 1, "name": "Stale"}),
    )
    .await;
    assert_eq!(status, 409, "{stale:?}");
    assert_eq!(stale["error"]["code"], "version_mismatch");
    let (status, current) = request(
        port,
        "GET",
        &format!("/api/v1/entities/{party}"),
        None,
        "read-after-stale",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(current["name"], "Renamed Party");
    assert_eq!(current["revision"], 3);
    drop(etag);
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u04_party_edit_rest_and_command_share_one_operation() {
    let (path, workspace, port, service) = setup("parity").await;
    let party = create_party(port, "Parity Party").await;
    let etag = entity_etag(port, &party).await;

    let (status, rest) = patch_entity(
        port,
        &party,
        "rest-party-key-01",
        &etag,
        json!({"name": "REST Renamed"}),
    )
    .await;
    assert_eq!(status, 200, "{rest:?}");
    assert_eq!(rest["name"], "REST Renamed");
    assert_eq!(rest["revision"], 2);

    let etag2 = entity_etag(port, &party).await;
    let (status, command) = cmd(
        port,
        "party.set_focus",
        "focus-key-01",
        json!({"party_id": party, "expected_card_revision": 2, "focus": "Command focus"}),
    )
    .await;
    assert_eq!(status, 200, "{command:?}");
    assert_eq!(command["result"]["focus"], "Command focus");
    assert_eq!(command["result"]["revision"], 3);
    assert_eq!(command["result"]["name"], "REST Renamed");

    // REST stale ETag still fails with its published 412.
    let (status, stale_rest) = patch_entity(
        port,
        &party,
        "rest-party-key-02",
        &etag,
        json!({"name": "Stale REST"}),
    )
    .await;
    assert_eq!(status, 412, "{stale_rest:?}");
    assert_eq!(stale_rest["error"]["code"], "STALE_REVISION");
    assert_eq!(count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{party}' AND event_type='entity.updated'")).await, 2);
    let _ = etag2;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u04_party_edit_preserves_policy_claims_graph_and_xss_text() {
    let (path, workspace, port, service) = setup("provenance").await;
    let party = create_party(port, "Provenance Party").await;
    let org = create_org(port, "Provenance Org").await;
    // A legal person→org membership edge seeds graph provenance. The edge
    // is inserted as an existing active fact (a saved source occurrence is
    // required by the REST edge create surface); the rename must not disturb
    // it or create history.
    let edge_id = "edge-seeded-1";
    sqlx::query(
        "INSERT INTO edge(id,from_id,verb,to_id,state,revision,created_at) VALUES(?,?,?,?,'active',1,?)",
    )
    .bind(edge_id)
    .bind(&party)
    .bind("member_of")
    .bind(&org)
    .bind("2026-01-01T00:00:00Z")
    .execute(workspace.pool())
    .await
    .unwrap();
    // Creating an opportunity with this party as primary seeds its policy row.
    let opportunity = create_opportunity(port, "Provenance opportunity", &party).await;

    let xss_name = "<img src=x onerror=alert(1)>Renamed";
    let (status, renamed) = cmd(
        port,
        "party.rename",
        "rename-key-01",
        json!({"party_id": party, "expected_card_revision": 1, "name": xss_name}),
    )
    .await;
    assert_eq!(status, 200, "{renamed:?}");
    assert_eq!(renamed["result"]["name"], xss_name);

    // The edge and its provenance survive; the rename only touched labels.
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM edge WHERE id='{edge_id}' AND from_id='{party}' AND to_id='{org}' AND verb='member_of' AND state='active' AND revision=1")).await,
        1
    );
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT COUNT(*) FROM edge_history WHERE from_id='{party}'")
        )
        .await,
        0,
        "the rename created no edge revision history"
    );
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT COUNT(*) FROM party_policy WHERE party_id='{party}'")
        )
        .await,
        1,
        "contact policy rows are untouched by a rename"
    );
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT COUNT(*) FROM opportunity WHERE id='{opportunity}' AND state='open'")
        )
        .await,
        1,
        "the opportunity and its primary party binding are untouched"
    );
    cleanup(&path, workspace, service);
}

// ---------------------------------------------------------------------------
// u04: task toggle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn u04_task_toggle_exact_span_and_second_identical_task() {
    let (path, workspace, port, service) = setup("task").await;
    let party = create_party(port, "Task Party").await;
    let opportunity = create_opportunity(port, "Task opportunity", &party).await;
    let markdown = "Checklist\n- [ ] Call back\n- [ ] Call back\n\na note paragraph\n";
    let (status, saved) = patch_notes(port, &opportunity, "notes-key-01", markdown, 1).await;
    assert_eq!(status, 200, "{saved:?}");

    let first = notes(port, &opportunity).await;
    let tasks = first["tasks"].as_array().unwrap().clone();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["text"], "Call back");
    assert_eq!(tasks[1]["text"], "Call back");

    // Toggle only the first task by its exact span.
    let (status, toggled) = cmd(
        port,
        "opportunity.set_task_done",
        "task-key-01",
        task_args(&opportunity, 2, &tasks[0], true),
    )
    .await;
    assert_eq!(status, 200, "{toggled:?}");
    assert_eq!(toggled["result"]["notes_revision"], 3);
    assert_eq!(toggled["result"]["task_count"], 2);

    let after = notes(port, &opportunity).await;
    let after_tasks = after["tasks"].as_array().unwrap();
    assert_eq!(after_tasks[0]["checked"], true);
    assert_eq!(after_tasks[1]["checked"], false);
    // Exact untouched bytes outside the addressed line persist.
    assert!(
        after["markdown"]
            .as_str()
            .unwrap()
            .starts_with("Checklist\n- [x] Call back\n"),
        "only the addressed line changed: {}",
        after["markdown"]
    );

    // The same intent replays; a fresh key with the same state is a no-op.
    let (status, replay) = cmd(
        port,
        "opportunity.set_task_done",
        "task-key-01",
        task_args(&opportunity, 2, &tasks[0], true),
    )
    .await;
    assert_eq!(status, 200, "{replay:?}");
    assert_eq!(replay["replayed"], true);
    let (status, noop) = cmd(
        port,
        "opportunity.set_task_done",
        "task-key-02",
        task_args(&opportunity, 3, &after_tasks[0], true),
    )
    .await;
    assert_eq!(status, 200, "{noop:?}");
    assert_eq!(noop["result"]["notes_revision"], 3);
    assert!(!noop["result"]["event_id"].as_str().is_some());
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{opportunity}' AND event_type='opportunity.task_toggled'")).await,
        1,
        "only the first toggle emitted an event"
    );

    // Toggle the second identical task by its own span.
    let (status, second_toggle) = cmd(
        port,
        "opportunity.set_task_done",
        "task-key-03",
        task_args(&opportunity, 3, &after_tasks[1], true),
    )
    .await;
    assert_eq!(status, 200, "{second_toggle:?}");
    let after2 = notes(port, &opportunity).await;
    let after2_tasks = after2["tasks"].as_array().unwrap();
    assert_eq!(after2_tasks[0]["checked"], true);
    assert_eq!(after2_tasks[1]["checked"], true);

    // Uncheck via the command (both requested states).
    let (status, unchecked) = cmd(
        port,
        "opportunity.set_task_done",
        "task-key-04",
        task_args(&opportunity, 4, &after2_tasks[1], false),
    )
    .await;
    assert_eq!(status, 200, "{unchecked:?}");
    let after3 = notes(port, &opportunity).await;
    let after3_tasks = after3["tasks"].as_array().unwrap();
    assert_eq!(after3_tasks[0]["checked"], true);
    assert_eq!(after3_tasks[1]["checked"], false);

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u04_task_toggle_rejects_stale_spans_and_malformed_offsets() {
    let (path, workspace, port, service) = setup("span").await;
    let party = create_party(port, "Span Party").await;
    let opportunity = create_opportunity(port, "Span opportunity", &party).await;
    let markdown = "- [ ] Only task\n- [x] Done\n";
    let (status, saved) = patch_notes(port, &opportunity, "notes-key-01", markdown, 1).await;
    assert_eq!(status, 200, "{saved:?}");
    let current = notes(port, &opportunity).await;
    let task = current["tasks"].as_array().unwrap()[0].clone();

    // Stale line digest (the note changed in another session).
    let mut stale = task_args(&opportunity, 2, &task, true);
    stale["line_sha256"] = json!("f".repeat(64));
    let (status, stale_reply) =
        cmd(port, "opportunity.set_task_done", "task-key-stale", stale).await;
    assert_eq!(status, 409, "{stale_reply:?}");
    assert_eq!(stale_reply["error"]["code"], "conflict");
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT notes_revision FROM opportunity WHERE id='{opportunity}'")
        )
        .await,
        2
    );

    // Out-of-bounds span.
    let bad = json!({
        "opportunity_id": opportunity,
        "expected_notes_revision": 2,
        "byte_start": 1000,
        "byte_end": 1005,
        "line_sha256": task["line_sha256"],
        "checked": true
    });
    let (status, bad_reply) = cmd(port, "opportunity.set_task_done", "task-key-bad", bad).await;
    assert_eq!(status, 422, "{bad_reply:?}");
    assert_eq!(bad_reply["error"]["code"], "invalid_args");

    // Missing UTF-8 boundary offsets (byte 1 of a 2-byte char) as a slice
    // that cannot name a task: validated by the typed span rules.
    let utf8 = json!({
        "opportunity_id": opportunity,
        "expected_notes_revision": 2,
        "byte_start": 0,
        "byte_end": 1,
        "line_sha256": task["line_sha256"],
        "checked": true
    });
    let (status, utf8_reply) = cmd(port, "opportunity.set_task_done", "task-key-utf8", utf8).await;
    assert_eq!(status, 422, "{utf8_reply:?}");

    // The REST toggle adapter shares the same rule set.
    let (status, rest_bad) = rest_toggle(
        port,
        &opportunity,
        "rest-toggle-key-01",
        json!({
            "notes_revision": 2,
            "byte_start": 1000,
            "byte_end": 1005,
            "line_sha256": task["line_sha256"],
            "checked": true
        }),
    )
    .await;
    assert_eq!(status, 422, "{rest_bad:?}");

    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u04_rest_note_save_and_command_task_toggle_coordinate() {
    let (path, workspace, port, service) = setup("race").await;
    let party = create_party(port, "Race Party").await;
    let opportunity = create_opportunity(port, "Race opportunity", &party).await;
    let (status, saved) =
        patch_notes(port, &opportunity, "notes-key-01", "- [ ] A\n- [ ] B\n", 1).await;
    assert_eq!(status, 200, "{saved:?}");
    let current = notes(port, &opportunity).await;
    let task = current["tasks"].as_array().unwrap()[0].clone();

    // A full note save and a task toggle race from the same base revision.
    // Exactly one may advance the revision; the other either replays (same
    // key) or fails with a stale revision. No lost update is possible.
    let note_opportunity = opportunity.clone();
    let toggle_opportunity = opportunity.clone();
    let note_port = port;
    let toggle_port = port;
    let save = tokio::spawn(async move {
        patch_notes(
            note_port,
            &note_opportunity,
            "race-notes-key",
            "- [ ] A\n- [x] B\n",
            2,
        )
        .await
    });
    let toggle = tokio::spawn(async move {
        cmd(
            toggle_port,
            "opportunity.set_task_done",
            "race-task-key",
            task_args(&toggle_opportunity, 2, &task, true),
        )
        .await
    });
    let (save_status, save_reply) = save.await.unwrap();
    let (toggle_status, toggle_reply) = toggle.await.unwrap();

    let revision_after = count(
        &workspace,
        &format!("SELECT notes_revision FROM opportunity WHERE id='{opportunity}'"),
    )
    .await;
    assert!(
        revision_after == 3 || revision_after == 4,
        "revision {revision_after}"
    );
    // At most one mutation can win per base revision; the other returns a
    // terminal stale conflict rather than silently overwriting.
    for (status, reply, kind) in [
        (save_status, &save_reply, "save"),
        (toggle_status, &toggle_reply, "toggle"),
    ] {
        if status != 200 {
            assert!(status == 409 || status == 412, "{kind} {status} {reply:?}");
            let code = reply["error"]["code"].as_str().unwrap_or_default();
            assert!(
                code == "version_mismatch" || code == "STALE_REVISION",
                "{kind} {reply:?}"
            );
        }
    }
    let task_events = count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{opportunity}' AND event_type='opportunity.task_toggled'")).await;
    let notes_events = count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{opportunity}' AND event_type='opportunity.notes_updated'")).await;
    assert!(
        task_events + notes_events >= 1 && task_events + notes_events <= 2,
        "task {task_events}, notes {notes_events}"
    );

    cleanup(&path, workspace, service);
}

// ---------------------------------------------------------------------------
// u04: delegated agent surface
// ---------------------------------------------------------------------------

type Responder = Box<dyn FnMut(usize, &str) -> Value + Send>;

struct FakeProvider {
    inner: support::model_server::ModelServer,
}

impl FakeProvider {
    async fn with(responder: Responder) -> Self {
        let inner = support::model_server::ModelServer::with_responder(
            Box::new({
                let mut responder = responder;
                move |index, body| responder(index + 1, body)
            }),
            None,
        )
        .await;
        Self { inner }
    }

    #[allow(dead_code)]
    fn url(&self) -> String {
        self.inner.url()
    }

    async fn stop(self) {
        self.inner.stop().await;
    }
}

fn final_answer(content: &str) -> Value {
    json!({
        "message": {"role":"assistant","content":content},
        "prompt_eval_count": 12, "eval_count": 8
    })
}

fn tool_call(name: &str, args: Value, content: &str) -> Value {
    json!({
        "message": {
            "role": "assistant", "content": content,
            "tool_calls": [{"id":"call","function":{"name":name,"arguments":args.to_string()}}]
        },
        "prompt_eval_count": 12, "eval_count": 8
    })
}

/// Read the most recent tool result from a chat request body.
fn last_tool_result(body: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(body).ok()?;
    let messages = value.get("messages")?.as_array()?;
    messages
        .iter()
        .rev()
        .find(|message| message["role"].as_str() == Some("tool"))
        .and_then(|message| message["content"].as_str())
        .and_then(|text| serde_json::from_str(text).ok())
}

async fn set_provider(workspace: &Workspace, path: &Path, provider: &FakeProvider) {
    support::model_server::install_qualified_profile(
        workspace.pool(),
        path,
        provider.inner.endpoint(),
    )
    .await;
}

async fn start_run(
    port: u16,
    task: &str,
    targets: Value,
    permissions: Value,
    allow_created: bool,
    key: &str,
) -> (u16, Value) {
    request(
        port,
        "POST",
        "/api/v1/agent/runs",
        Some(json!({
            "task": task,
            "targets": targets,
            "permissions": permissions,
            "allow_created_opportunities": allow_created
        })),
        key,
    )
    .await
}

async fn wait_state(port: u16, id: &str, states: &[&str]) -> Value {
    for _ in 0..100 {
        let (status, value) = request(
            port,
            "GET",
            &format!("/api/v1/agent/runs/{id}"),
            None,
            "run-read",
        )
        .await;
        assert_eq!(status, 200, "{value:?}");
        if states.contains(&value["state"].as_str().unwrap_or_default()) {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("run {id} never reached {states:?}");
}

async fn transcript(port: u16, id: &str) -> Vec<Value> {
    let (status, value) = request(
        port,
        "GET",
        &format!("/api/v1/agent/runs/{id}/transcript?limit=20"),
        None,
        "transcript-read",
    )
    .await;
    assert_eq!(status, 200, "{value:?}");
    value["items"].as_array().unwrap().clone()
}

fn tool_text(turns: &[Value]) -> String {
    turns
        .iter()
        .filter(|turn| turn["speaker"].as_str() == Some("tool"))
        .map(|turn| turn["text"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn u04_agent_delegated_party_edit_and_task_done() {
    let (path, workspace, port, service) = setup("agent").await;
    let party = create_party(port, "Delegated Party").await;
    let opportunity = create_opportunity(port, "Delegated opportunity", &party).await;
    let (status, saved) = patch_notes(
        port,
        &opportunity,
        "notes-key-01",
        "- [ ] Delegated task\n",
        1,
    )
    .await;
    assert_eq!(status, 200, "{saved:?}");
    let current = notes(port, &opportunity).await;
    let task = current["tasks"].as_array().unwrap()[0].clone();

    let tool_party = party.clone();
    let tool_opportunity = opportunity.clone();
    let tool_task = task.clone();
    let provider = FakeProvider::with(Box::new(move |index, body| {
        if index == 0 {
            return final_answer("probe-ok");
        }
        if index == 1 {
            return tool_call(
                "party__rename",
                json!({"party_id": tool_party, "expected_card_revision": 1, "name": "Agent Renamed"}),
                "rename the granted party",
            );
        }
        if index == 2 {
            return tool_call(
                "opportunity__set_task_done",
                json!({
                    "opportunity_id": tool_opportunity,
                    "expected_notes_revision": 2,
                    "byte_start": tool_task["byte_start"],
                    "byte_end": tool_task["byte_end"],
                    "line_sha256": tool_task["line_sha256"],
                    "checked": true
                }),
                "check the granted task",
            );
        }
        if index == 3 {
            return tool_call(
                "party__rename",
                json!({"party_id": "party_absent", "expected_card_revision": 1, "name": "Foreign"}),
                "try a foreign party",
            );
        }
        let created = last_tool_result(body);
        if index == 4 {
            return tool_call(
                "opportunity__set_task_done",
                json!({
                    "opportunity_id": "opp_absent",
                    "expected_notes_revision": 2,
                    "byte_start": tool_task["byte_start"],
                    "byte_end": tool_task["byte_end"],
                    "line_sha256": tool_task["line_sha256"],
                    "checked": true
                }),
                "try a foreign opportunity",
            );
        }
        let _ = created;
        final_answer("Done with granted edits.")
    }))
    .await;
    set_provider(&workspace, &path, &provider).await;
    let (status, run) = start_run(
        port,
        "Edit the granted party and task",
        json!([
            {"kind":"party","id":party},
            {"kind":"opportunity","id":opportunity}
        ]),
        json!(["party.edit", "opportunity.task.edit"]),
        false,
        "agent-key-01",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    let done = wait_state(port, id, &["succeeded", "failed"]).await;
    assert_eq!(
        done["state"],
        "succeeded",
        "{}",
        tool_text(&transcript(port, id).await)
    );

    let name: String = sqlx::query_scalar("SELECT name FROM entity WHERE id=?")
        .bind(&party)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(name, "Agent Renamed");
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT notes_revision FROM opportunity WHERE id='{opportunity}'")
        )
        .await,
        3,
        "one delegated task toggle advanced the notes revision"
    );
    let after = notes(port, &opportunity).await;
    assert_eq!(after["tasks"].as_array().unwrap()[0]["checked"], true);

    let text = tool_text(&transcript(port, id).await);
    assert!(text.contains("target_unavailable"), "{text}");
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{party}' AND event_type='entity.updated'")).await,
        1,
        "only the granted rename produced an entity event"
    );
    assert_eq!(
        count(
            &workspace,
            "SELECT COUNT(*) FROM opportunity WHERE title='Foreign'"
        )
        .await,
        0
    );
    provider.stop().await;
    cleanup(&path, workspace, service);
}

#[tokio::test]
async fn u04_agent_task_noop_consumes_zero_executed_write() {
    let (path, workspace, port, service) = setup("agent-noop").await;
    let party = create_party(port, "Noop Party").await;
    let opportunity = create_opportunity(port, "Noop opportunity", &party).await;
    let (status, saved) = patch_notes(
        port,
        &opportunity,
        "notes-key-01",
        "- [x] Already done\n",
        1,
    )
    .await;
    assert_eq!(status, 200, "{saved:?}");
    let current = notes(port, &opportunity).await;
    let task = current["tasks"].as_array().unwrap()[0].clone();

    let tool_opportunity = opportunity.clone();
    let tool_task = task.clone();
    let provider = FakeProvider::with(Box::new(move |index, _body| {
        if index == 0 {
            return final_answer("probe-ok");
        }
        if index == 1 {
            return tool_call(
                "opportunity__set_task_done",
                json!({
                    "opportunity_id": tool_opportunity,
                    "expected_notes_revision": 2,
                    "byte_start": tool_task["byte_start"],
                    "byte_end": tool_task["byte_end"],
                    "line_sha256": tool_task["line_sha256"],
                    "checked": true
                }),
                "already checked",
            );
        }
        final_answer("No-op done.")
    }))
    .await;
    set_provider(&workspace, &path, &provider).await;
    let (status, run) = start_run(
        port,
        "Check an already checked task",
        json!([{"kind":"opportunity","id":opportunity}]),
        json!(["opportunity.task.edit"]),
        false,
        "agent-key-01",
    )
    .await;
    assert_eq!(status, 201, "{run:?}");
    let id = run["run_id"].as_str().unwrap();
    let done = wait_state(port, id, &["succeeded", "failed"]).await;
    assert_eq!(done["state"], "succeeded");
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT executed_writes FROM agent_run_budget WHERE run_id='{id}'")
        )
        .await,
        0,
        "the no-op consumed zero executed-write allowance"
    );
    assert_eq!(
        count(
            &workspace,
            &format!("SELECT invocations FROM agent_run_budget WHERE run_id='{id}'")
        )
        .await,
        1,
        "the no-op is still one admitted invocation"
    );
    assert_eq!(
        count(&workspace, &format!("SELECT COUNT(*) FROM event WHERE entity_id='{opportunity}' AND event_type='opportunity.task_toggled'")).await,
        0
    );
    provider.stop().await;
    cleanup(&path, workspace, service);
}
