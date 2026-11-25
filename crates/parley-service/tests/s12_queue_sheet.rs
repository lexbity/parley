// Parley-Queue implementation spec (sheet): phase-scoped opportunity sheet,
// typed phase command and ordinary Undo. Mirrors the s10 harness shape but
// exercises the sheet read and phase write contracts.
use parley_service::{RunningService, Workspace};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

type OwnerSession = (String, String, String);
static NEXT: AtomicU64 = AtomicU64::new(0);
static SESSIONS: OnceLock<Mutex<std::collections::HashMap<u16, OwnerSession>>> = OnceLock::new();
fn db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-s12-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: &str,
) -> (u16, Value) {
    let (cookie, token, epoch) = SESSIONS
        .get()
        .unwrap()
        .lock()
        .unwrap()
        .get(&port)
        .unwrap()
        .clone();
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\nIdempotency-Key: {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
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
    let value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(payload).unwrap()
    };
    (status, value.get("data").cloned().unwrap_or(value))
}

async fn owner_session(service: &RunningService) -> OwnerSession {
    let address = service.address;
    let body = json!({"secret":service.bootstrap_secret}).to_string();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(format!("POST /api/v1/session/bootstrap HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
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
    stream.write_all(format!("GET /api/v1/session HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nCookie: {cookie}\r\nContent-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let payload: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    (
        cookie,
        payload["mutation_token"].as_str().unwrap().into(),
        payload["epoch"].as_str().unwrap().into(),
    )
}

async fn setup() -> (PathBuf, Workspace, u16, RunningService) {
    let path = db_path();
    let service = parley_service::start_workspace(&path).await.unwrap();
    let port = service.address.port();
    let owner = owner_session(&service).await;
    SESSIONS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap()
        .insert(port, owner);
    let workspace = Workspace::open(&path).await.unwrap();
    (path, workspace, port, service)
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

async fn create_opportunity(port: u16, title: &str, party: &str) -> String {
    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":title,"primary_party_id":party})),
        &format!("opportunity-{title}"),
    )
    .await;
    assert_eq!(status, 201, "{opportunity:?}");
    opportunity["id"].as_str().unwrap().to_string()
}

async fn sheet(port: u16, query: &str) -> Value {
    let (status, value) = request(
        port,
        "GET",
        &format!("/api/v1/queue?view=sheet{query}"),
        None,
        "sheet-read",
    )
    .await;
    assert_eq!(status, 200, "{value:?}");
    value
}

fn row_phase(row: &Value) -> Option<String> {
    row["phase"]["stage_id"].as_str().map(str::to_owned)
}

async fn phase_change(
    port: u16,
    id: &str,
    target: &str,
    expected_revision: i64,
    original_event_id: Option<&str>,
    key: &str,
) -> (u16, Value) {
    let mut body = json!({
        "target_stage_id": target,
        "pack_version": "1.0.0",
        "expected_phase_revision": expected_revision,
    });
    if let Some(event) = original_event_id {
        body["original_event_id"] = json!(event);
    }
    request(
        port,
        "PATCH",
        &format!("/api/v1/opportunities/{id}/phase"),
        Some(body),
        key,
    )
    .await
}

#[tokio::test]
async fn s12_sheet_defaults_phase_edit_undo_and_conflicts() {
    let (path, workspace, port, service) = setup().await;

    let party = create_party(port, "Fieldwork Lead").await;
    let opp = create_opportunity(port, "Contribute a CSV fixture", &party).await;

    // First use: all phases, unmapped rows, earliest action date first.
    let all = sheet(port, "").await;
    assert_eq!(all["total"], 1);
    let row = &all["rows"][0];
    assert_eq!(row["opportunity_id"].as_str().unwrap(), opp);
    assert_eq!(row["lifecycle"].as_str().unwrap(), "open");
    // Pack identity is present for an Unmapped row too, so the palette can
    // freeze the precondition from this authoritative read (UCR-001 U01).
    assert_eq!(row["pack_id"].as_str().unwrap(), "job_search");
    assert_eq!(row["pack_version"].as_str().unwrap(), "1.0.0");
    assert_eq!(
        row_phase(row),
        None,
        "legacy rows are Unmapped, never invented"
    );
    assert_eq!(row["next_action"]["kind"].as_str().unwrap(), "choose");
    assert_eq!(
        row["next_action"]["label"].as_str().unwrap(),
        "Choose a next action"
    );
    assert!(all["phases"].as_array().unwrap().iter().any(|p| {
        p["stage_id"].as_str() == Some("__unmapped__") && p["count"].as_u64() == Some(1)
    }));
    assert!(all["generation"].as_str().is_some());

    // Search matches the opportunity title and the party name (case-insensitive).
    assert_eq!(sheet(port, "&search=fieldwork").await["total"], 1);
    assert_eq!(sheet(port, "&search=CSV").await["total"], 1);
    assert_eq!(sheet(port, "&search=absent").await["total"], 0);

    // Ordinary phase edit: exploring, revision 1 -> 2.
    let (status, first) = phase_change(port, &opp, "exploring", 1, None, "k-1").await;
    assert_eq!(status, 200, "{first:?}");
    assert_eq!(
        first["row"]["phase"]["stage_id"].as_str().unwrap(),
        "exploring"
    );
    assert_eq!(first["phase_revision"].as_i64().unwrap(), 2);
    let event_e = first["event_id"].as_str().unwrap().to_string();
    assert!(!event_e.is_empty());

    let exploring = sheet(port, "&phase=exploring").await;
    assert_eq!(exploring["total"], 1);
    assert_eq!(
        row_phase(&exploring["rows"][0]).as_deref(),
        Some("exploring")
    );
    let preparing = sheet(port, "&phase=preparing").await;
    assert_eq!(preparing["total"], 0);
    assert_eq!(sheet(port, "&phase=__unmapped__").await["total"], 0);

    // Same-phase selection is a no-op: revision stays 2, event id unchanged.
    let (status, noop) = phase_change(port, &opp, "exploring", 2, None, "k-noop").await;
    assert_eq!(status, 200);
    assert_eq!(noop["phase_revision"].as_i64().unwrap(), 2);
    assert_eq!(noop["event_id"].as_str().unwrap(), event_e);

    // Move to preparing (revision 3), then undo back to exploring.
    let (status, second) = phase_change(port, &opp, "preparing", 2, None, "k-2").await;
    assert_eq!(status, 200);
    assert_eq!(second["phase_revision"].as_i64().unwrap(), 3);
    let event_p = second["event_id"].as_str().unwrap().to_string();

    let (status, undo) = phase_change(port, &opp, "exploring", 3, Some(&event_p), "k-undo").await;
    if status != 200 {
        eprintln!("undo rejected: {status} {undo:?}");
    }
    assert_eq!(status, 200, "{undo:?}");
    assert!(undo["reverted"].as_bool().unwrap());
    assert_eq!(undo["phase_revision"].as_i64().unwrap(), 4);
    assert_eq!(row_phase(&undo["row"]).as_deref(), Some("exploring"));
    assert!(undo["event_id"].as_str().unwrap() != event_p);

    // Undo conflict: the referenced transition is no longer the latest.
    let (status, conflict) = phase_change(
        port,
        &opp,
        "exploring",
        4,
        Some(&event_p),
        "k-undo-conflict",
    )
    .await;
    assert_eq!(status, 412, "{conflict:?}");
    assert_eq!(
        conflict["error"]["code"].as_str().unwrap(),
        "PHASE_CHANGED_ELSEWHERE"
    );

    // Idempotent retry replays the original acknowledgement; one event only.
    // (key k-2 was already committed with the identical intent; the replay
    // returns the stored result without a new phase event.)
    let (status, replay) = phase_change(port, &opp, "preparing", 2, None, "k-2").await;
    assert_eq!(status, 200);
    assert_eq!(replay["_replay_status"].as_i64().unwrap(), 200);
    assert_eq!(replay["result"]["event_id"].as_str().unwrap(), event_p);
    assert_eq!(replay["result"]["phase_revision"].as_i64().unwrap(), 3);

    // Terminal stages require the lifecycle flow, never an ordinary edit.
    let (status, terminal) = phase_change(port, &opp, "closed", 4, None, "k-terminal").await;
    assert_eq!(status, 422);
    assert_eq!(
        terminal["error"]["code"].as_str().unwrap(),
        "REQUIRES_LIFECYCLE_CONFIRM"
    );

    // A lifecycle change blocks ordinary phase edits.
    sqlx::query("UPDATE opportunity SET state='closed',close_reason='owner closed',revision=revision+1,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?")
        .bind(&opp)
        .execute(workspace.pool())
        .await
        .unwrap();
    let (status, blocked) = phase_change(port, &opp, "in_conversation", 4, None, "k-blocked").await;
    assert_eq!(status, 409);
    assert_eq!(
        blocked["error"]["code"].as_str().unwrap(),
        "LIFECYCLE_BLOCKS_PHASE"
    );

    drop(workspace);
    drop(service);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s12_sheet_sort_pagination_and_waiting_checkpoint() {
    let (path, workspace, port, service) = setup().await;
    let party_a = create_party(port, "Northline").await;
    let party_b = create_party(port, "Fieldwork").await;
    let a = create_opportunity(port, "Offer an onboarding review", &party_a).await;
    let b = create_opportunity(port, "Contribute a CSV fixture", &party_b).await;

    // Row B has a live waiting thread (reply window open): next action is a
    // waiting checkpoint identifying the contact, and it sorts by its date.
    sqlx::query("INSERT INTO contact_thread(id,opportunity_id,recipient_id,state,current_touch,expires_at,created_at,updated_at) VALUES('t-wait',?,?,'waiting',1,'2026-11-25T08:00:00.000Z','2026-11-20T08:00:00.000Z','2026-11-20T08:00:00.000Z')")
        .bind(&b)
        .bind(&party_b)
        .execute(workspace.pool())
        .await
        .unwrap();

    let due = sheet(port, "&sort=due").await;
    assert_eq!(due["total"], 2);
    assert_eq!(due["rows"][0]["opportunity_id"].as_str().unwrap(), b);
    assert_eq!(
        due["rows"][0]["next_action"]["kind"].as_str().unwrap(),
        "wait"
    );
    assert!(
        due["rows"][0]["next_action"]["label"]
            .as_str()
            .unwrap()
            .starts_with("Waiting until")
    );
    assert_eq!(
        due["rows"][0]["next_action"]["recipient_name"]
            .as_str()
            .unwrap(),
        "Fieldwork"
    );
    // Null dates sort last.
    assert_eq!(due["rows"][1]["opportunity_id"].as_str().unwrap(), a);

    let by_name = sheet(port, "&sort=name").await;
    assert_eq!(by_name["rows"][0]["opportunity_id"].as_str().unwrap(), b);
    assert_eq!(by_name["rows"][1]["opportunity_id"].as_str().unwrap(), a);

    // Pagination: limit 1 returns rows and a continuation cursor.
    let page1 = sheet(port, "&sort=name&limit=1").await;
    assert_eq!(page1["rows"].as_array().unwrap().len(), 1);
    assert_eq!(page1["total"], 2);
    let cursor = page1["next_cursor"].as_str().unwrap().to_string();
    let page2 = sheet(port, &format!("&sort=name&limit=1&cursor={cursor}")).await;
    assert_eq!(page2["rows"].as_array().unwrap().len(), 1);
    assert_eq!(page2["rows"][0]["opportunity_id"].as_str().unwrap(), a);

    // The legacy daily queue still works alongside the sheet.
    let (status, daily) =
        request(port, "GET", "/api/v1/queue?view=daily", None, "daily-read").await;
    assert_eq!(status, 200, "{daily:?}");
    assert_eq!(
        daily["actionable"].as_array().unwrap().len() + daily["blocked"].as_array().unwrap().len(),
        2
    );

    drop(workspace);
    drop(service);
    let _ = fs::remove_file(path);
}

fn legacy_order(rows: &[Value], by_name: bool) -> Vec<String> {
    let mut rows = rows.to_vec();
    rows.sort_by(|a, b| {
        let title_order = a["title"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .cmp(&b["title"].as_str().unwrap().to_lowercase());
        let id_order = a["opportunity_id"]
            .as_str()
            .unwrap()
            .cmp(b["opportunity_id"].as_str().unwrap());
        if by_name {
            title_order.then(id_order)
        } else {
            let date_order = match (
                a["next_action"]["date"].as_str(),
                b["next_action"]["date"].as_str(),
            ) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (Some(x), Some(y)) => x.cmp(y),
            };
            date_order.then(title_order).then(id_order)
        }
    });
    rows.iter()
        .map(|row| row["opportunity_id"].as_str().unwrap().to_owned())
        .collect()
}

async fn walk_pages(port: u16, sort: &str) -> Vec<String> {
    let mut cursor: Option<String> = None;
    let mut walked = Vec::new();
    loop {
        let suffix = match &cursor {
            Some(cursor) => format!("&cursor={cursor}"),
            None => String::new(),
        };
        let page = sheet(port, &format!("&sort={sort}&limit=1{suffix}")).await;
        for row in page["rows"].as_array().unwrap() {
            walked.push(row["opportunity_id"].as_str().unwrap().to_owned());
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    walked
}

#[tokio::test]
async fn s12_sheet_sql_order_matches_legacy_comparator_and_keyset_is_stable() {
    let (path, workspace, port, service) = setup().await;
    let zulu = create_party(port, "Zulu").await;
    let alpha = create_party(port, "alpha").await;
    let mike = create_party(port, "Mike").await;
    let zebra = create_opportunity(port, "Zebra review", &zulu).await;
    let alp = create_opportunity(port, "alpha plan", &alpha).await;
    let beta = create_opportunity(port, "Beta task", &mike).await;
    let gamma = create_opportunity(port, "gamma choose", &mike).await;

    sqlx::query("INSERT INTO contact_thread(id,opportunity_id,recipient_id,state,current_touch,expires_at,next_action_at,created_at,updated_at) VALUES('t-wait',?,?,'waiting',1,'2026-11-25T08:00:00.000Z',NULL,'2026-11-20T08:00:00.000Z','2026-11-20T08:00:00.000Z')")
        .bind(&zebra)
        .bind(&zulu)
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO contact_thread(id,opportunity_id,recipient_id,state,current_touch,expires_at,next_action_at,created_at,updated_at) VALUES('t-due',?,?,'conversing',1,NULL,'2026-11-21T08:00:00.000Z','2026-11-20T08:00:00.000Z','2026-11-20T08:00:00.000Z')")
        .bind(&alp)
        .bind(&alpha)
        .execute(workspace.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE opportunity SET notes_next_task='Call the team' WHERE id=?")
        .bind(&beta)
        .execute(workspace.pool())
        .await
        .unwrap();

    for sort in ["due", "name"] {
        let full = sheet(port, &format!("&sort={sort}&limit=200")).await;
        assert_eq!(full["total"], 4);
        let rows = full["rows"].as_array().unwrap();
        let ids: Vec<String> = rows
            .iter()
            .map(|row| row["opportunity_id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids, legacy_order(rows, sort == "name"), "sort={sort}");
        assert_eq!(walk_pages(port, sort).await, ids, "sort={sort} cursor walk");
    }

    // All four rows are reachable; the dated conversing tiebreak and the
    // waiting checkpoint keep their distinct next-action dates.
    assert_ne!(zebra, alp);
    assert_ne!(beta, gamma);

    drop(workspace);
    drop(service);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s12_events_filter_returns_scoped_history_with_timestamps() {
    let (path, _workspace, port, service) = setup().await;

    let party = create_party(port, "Fieldwork Lead").await;
    let opp = create_opportunity(port, "Co-write an importer guide", &party).await;
    let (status, first) = phase_change(port, &opp, "exploring", 1, None, "s12-events-k1").await;
    assert_eq!(status, 200, "{first:?}");

    // Unfiltered reads carry occurred_at for chronological history.
    let (status, all) = request(port, "GET", "/api/v1/events", None, "s12-events-all").await;
    assert_eq!(status, 200, "{all:?}");
    let items = all["items"].as_array().unwrap();
    assert!(!items.is_empty());
    let mut previous_cursor = 0i64;
    for item in items {
        assert!(
            item["occurred_at"]
                .as_str()
                .map(|value| !value.is_empty())
                .unwrap_or(false),
            "{item}"
        );
        let cursor: i64 = item["cursor"].as_str().unwrap().parse().unwrap();
        assert!(cursor > previous_cursor);
        previous_cursor = cursor;
    }

    // Focused reads restrict the feed to one entity (Queue inspector history).
    let (status, scoped) = request(
        port,
        "GET",
        &format!("/api/v1/events?entity_type=opportunity&entity_id={opp}"),
        None,
        "s12-events-scoped",
    )
    .await;
    assert_eq!(status, 200, "{scoped:?}");
    let scoped_items = scoped["items"].as_array().unwrap();
    assert!(!scoped_items.is_empty());
    for item in scoped_items {
        assert_eq!(item["entity_type"].as_str().unwrap(), "opportunity");
        assert_eq!(item["entity_id"].as_str().unwrap(), opp);
    }
    assert!(
        scoped_items
            .iter()
            .any(|item| item["event_type"].as_str() == Some("opportunity.phase_changed")),
        "{scoped_items:?}"
    );

    let (status, party_scoped) = request(
        port,
        "GET",
        &format!("/api/v1/events?entity_type=entity&entity_id={party}"),
        None,
        "s12-events-party",
    )
    .await;
    assert_eq!(status, 200, "{party_scoped:?}");
    let party_items = party_scoped["items"].as_array().unwrap();
    assert!(!party_items.is_empty());
    for item in party_items {
        assert_eq!(item["entity_id"].as_str().unwrap(), party);
    }

    drop(_workspace);
    drop(service);
    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn s12_pack_settings_validate_activate_and_remap() {
    // The pack settings surface (architecture-review finding G14): the UI and
    // the mock served GET /settings, packs/validate and packs/activate long
    // before the service did. This proof pins the default settings read, the
    // refusal paths (stale settings revision, mismatched document, missing
    // stage mapping) and a full activation that installs stages, remaps an
    // open opportunity through the owner-provided mapping and derives the
    // pack_policy fragment the queue reads.
    let (path, workspace, port, service) = setup().await;

    let lead_gen = concat!(
        "id: lead_gen\n",
        "name: Client lead generation\n",
        "version: 1.0.0\n",
        "vocabulary:\n",
        "  party_noun: prospect\n",
        "  win_noun: engagement\n",
        "stages:\n",
        "  - { name: Identified, class: identified }\n",
        "  - { name: Briefed, class: prepared }\n",
        "  - { name: Contacted, class: touched }\n",
        "ask_ladder:\n",
        "  - { rung: 1, weight: 1, label: yes/no question }\n",
        "  - { rung: 2, weight: 2, label: review }\n",
        "cadence:\n",
        "  max_touches: 3\n",
        "limits:\n",
        "  daily_send_cap: 15\n",
    );

    let party = create_party(port, "Pack Mover").await;
    let opportunity = create_opportunity(port, "Remap my stage", &party).await;
    let (status, view) = phase_change(port, &opportunity, "exploring", 1, None, "pack-phase-seed").await;
    assert_eq!(status, 200, "{view:?}");

    // Default settings: the seeded pack, revision 1, the reference document.
    let (status, settings) = request(port, "GET", "/api/v1/settings", None, "pack-settings-read").await;
    assert_eq!(status, 200, "{settings:?}");
    assert_eq!(settings["revision"], 1);
    assert_eq!(settings["active_pack_id"], "job_search");
    assert_eq!(settings["active_pack_version"], "1.0.0");
    assert!(settings["active_document"].as_str().unwrap().contains("Exploring"));
    assert!(settings["available_packs"].as_array().unwrap().iter().any(|pack| pack["id"] == "lead_gen"));

    // An invalid document validates as invalid; the active pack is untouched.
    let (status, result) = request(
        port,
        "POST",
        "/api/v1/packs/validate",
        Some(json!({"document":"id: lead_gen\nversion: one\n"})),
        "pack-validate-bad",
    )
    .await;
    assert_eq!(status, 200, "{result:?}");
    assert_eq!(result["valid"], false);
    assert!(result["errors"].as_array().unwrap().iter().any(|issue| issue["field"] == "version"));

    // A valid document validates and comes back normalized.
    let (status, result) = request(
        port,
        "POST",
        "/api/v1/packs/validate",
        Some(json!({"document": lead_gen})),
        "pack-validate-good",
    )
    .await;
    assert_eq!(status, 200, "{result:?}");
    assert_eq!(result["valid"], true);
    assert_eq!(result["pack_id"], "lead_gen");
    assert_eq!(result["normalized_document"], lead_gen);

    // Stale settings revision refuses before anything is written.
    let (status, view) = request(
        port,
        "POST",
        "/api/v1/packs/activate",
        Some(json!({"pack_id":"lead_gen","version":"1.0.0","document":lead_gen,"expected_settings_revision":99,"stage_mapping":{"exploring":"identified"}})),
        "pack-activate-stale",
    )
    .await;
    assert_eq!(status, 412, "{view:?}");
    assert_eq!(view["error"]["code"], "SETTINGS_STALE");

    // A document whose identity disagrees with the request is refused.
    let (status, view) = request(
        port,
        "POST",
        "/api/v1/packs/activate",
        Some(json!({"pack_id":"lead_gen","version":"2.0.0","document":lead_gen,"expected_settings_revision":1,"stage_mapping":{"exploring":"identified"}})),
        "pack-activate-mismatch",
    )
    .await;
    assert_eq!(status, 422, "{view:?}");
    assert_eq!(view["error"]["code"], "PACK_NOT_VALIDATED");

    // Changing pack identity without a stage mapping is refused.
    let (status, view) = request(
        port,
        "POST",
        "/api/v1/packs/activate",
        Some(json!({"pack_id":"lead_gen","version":"1.0.0","document":lead_gen,"expected_settings_revision":1,"stage_mapping":{}})),
        "pack-activate-no-mapping",
    )
    .await;
    assert_eq!(status, 422, "{view:?}");
    assert_eq!(view["error"]["code"], "STAGE_MAPPING_REQUIRED");

    // The real activation: stages installed, opportunity remapped, policy
    // derived, settings revision advanced.
    let (status, activated) = request(
        port,
        "POST",
        "/api/v1/packs/activate",
        Some(json!({"pack_id":"lead_gen","version":"1.0.0","document":lead_gen,"expected_settings_revision":1,"stage_mapping":{"exploring":"identified"}})),
        "pack-activate-real",
    )
    .await;
    assert_eq!(status, 200, "{activated:?}");
    assert_eq!(activated["revision"], 2);
    assert_eq!(activated["active_pack_id"], "lead_gen");

    let (status, settings) = request(port, "GET", "/api/v1/settings", None, "pack-settings-read-2").await;
    assert_eq!(status, 200, "{settings:?}");
    assert_eq!(settings["revision"], 2);
    assert_eq!(settings["active_document"], lead_gen);

    let stages: Vec<(String, String)> =
        sqlx::query_as("SELECT stage_id,label FROM pack_stage WHERE pack_id='lead_gen' AND pack_version='1.0.0' ORDER BY ordinal")
            .fetch_all(workspace.pool())
            .await
            .unwrap();
    assert_eq!(stages, vec![("identified".into(), "Identified".into()), ("briefed".into(), "Briefed".into()), ("contacted".into(), "Contacted".into())]);

    let (stage_id, stage_pack): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT stage_id,stage_pack_id FROM opportunity WHERE id=?",)
            .bind(&opportunity)
            .fetch_one(workspace.pool())
            .await
            .unwrap();
    assert_eq!(stage_id.as_deref(), Some("identified"), "the mapping moved the stage");
    assert_eq!(stage_pack.as_deref(), Some("lead_gen"));

    let policy: i64 = sqlx::query_scalar(
        "SELECT json_extract(settings_json,'$.pack_policy.limits.daily_send_cap') FROM owner_profile WHERE id=1",
    )
    .fetch_one(workspace.pool())
    .await
    .unwrap();
    assert_eq!(policy, 15, "the policy fragment is derived from the document");

    // Same-key replay replays the stored result: revision does not advance.
    let (status, replay) = request(
        port,
        "POST",
        "/api/v1/packs/activate",
        Some(json!({"pack_id":"lead_gen","version":"1.0.0","document":lead_gen,"expected_settings_revision":1,"stage_mapping":{"exploring":"identified"}})),
        "pack-activate-real",
    )
    .await;
    assert_eq!(status, 200, "{replay:?}");
    assert_eq!(replay["revision"], 2, "replay returns the stored receipt result");
    let current: i64 = sqlx::query_scalar("SELECT settings_revision FROM owner_profile WHERE id=1")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(current, 2);

    service.shutdown().await;
    let _ = fs::remove_file(path);
}
