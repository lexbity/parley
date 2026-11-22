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
static REQUESTS: AtomicU64 = AtomicU64::new(0);
static SESSIONS: OnceLock<Mutex<std::collections::HashMap<u16, OwnerSession>>> = OnceLock::new();

fn db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-s05-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn request(port: u16, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
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
    let key = REQUESTS.fetch_add(1, Ordering::Relaxed);
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCookie: {cookie}\r\nX-Parley-Token: {token}\r\nX-Parley-Epoch: {epoch}\r\nIdempotency-Key: s05-{key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
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
    let value: Value = if payload.is_empty() {
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
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
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
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let payload: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    (
        cookie,
        payload["mutation_token"].as_str().unwrap().to_owned(),
        payload["epoch"].as_str().unwrap().to_owned(),
    )
}

async fn setup() -> (PathBuf, Workspace, u16, RunningService) {
    let path = db_path();
    let service = parley_service::start_workspace(&path).await.unwrap();
    let session = owner_session(&service).await;
    let port = service.address.port();
    SESSIONS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap()
        .insert(port, session);
    let workspace = Workspace::open(&path).await.unwrap();
    (path, workspace, port, service)
}

async fn create_opportunity(port: u16, title: &str) -> String {
    let (status, party) = request(
        port,
        "POST",
        "/api/v1/entities",
        Some(json!({"kind":"person","name":format!("{title} contact")})),
    )
    .await;
    assert_eq!(status, 201, "{party}");
    let (status, opportunity) = request(
        port,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title":title,"primary_party_id":party["id"]})),
    )
    .await;
    assert_eq!(status, 201, "{opportunity}");
    opportunity["id"].as_str().unwrap().to_owned()
}

fn remove_db(path: &PathBuf) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(path.with_extension("sqlite-wal"));
    let _ = fs::remove_file(path.with_extension("sqlite-shm"));
}

#[tokio::test]
async fn s05_notes_projection_and_task_toggle_preserve_unaddressed_bytes() {
    let (path, _workspace, port, service) = setup().await;
    let opportunity = create_opportunity(port, "Notes test").await;
    let markdown = "## Work\n- [ ] First task\n  - [ ] Nested task after 🧭\n- [ ] Second task  `keep`\n\n```md\n- [ ] fenced example\n```\n    - [ ] indented code\n> - [ ] quoted source\n- \\[x] literal\n";
    let (status, notes) = request(
        port,
        "PATCH",
        &format!("/api/v1/opportunities/{opportunity}/notes"),
        Some(json!({"markdown":markdown,"expected_revision":1})),
    )
    .await;
    assert_eq!(status, 200, "{notes}");
    assert_eq!(notes["tasks"].as_array().unwrap().len(), 3, "{notes}");
    assert_eq!(notes["next_task"], "First task");
    let task = &notes["tasks"][1];
    let start = task["byte_start"].as_u64().unwrap() as usize;
    let end = task["byte_end"].as_u64().unwrap() as usize;
    let before = notes["markdown"].as_str().unwrap().as_bytes().to_vec();
    let (status, toggled) = request(port, "POST", &format!("/api/v1/opportunities/{opportunity}/notes/tasks/toggle"), Some(json!({"notes_revision":notes["revision"],"byte_start":start,"byte_end":end,"line_sha256":task["line_sha256"],"checked":true}))).await;
    assert_eq!(status, 200, "{toggled}");
    let after = toggled["markdown"].as_str().unwrap().as_bytes();
    assert_eq!(&before[..start], &after[..start]);
    assert_eq!(&before[end..], &after[end..]);
    assert!(
        after
            .windows(b"[x] Nested task after".len())
            .any(|part| part == b"[x] Nested task after")
    );
    assert_eq!(toggled["next_task"], "First task");
    drop(service);
    remove_db(&path);
}

#[tokio::test]
async fn s05_stale_task_toggle_rejects_without_mutation() {
    let (path, workspace, port, service) = setup().await;
    let opportunity = create_opportunity(port, "Stale notes").await;
    let (status, notes) = request(
        port,
        "PATCH",
        &format!("/api/v1/opportunities/{opportunity}/notes"),
        Some(json!({"markdown":"- [ ] Keep source bytes\n","expected_revision":1})),
    )
    .await;
    assert_eq!(status, 200, "{notes}");
    let task = &notes["tasks"][0];
    let before = notes["markdown"].clone();
    let (status, error) = request(port, "POST", &format!("/api/v1/opportunities/{opportunity}/notes/tasks/toggle"), Some(json!({"notes_revision":0,"byte_start":task["byte_start"],"byte_end":task["byte_end"],"line_sha256":task["line_sha256"],"checked":true}))).await;
    assert_eq!(status, 412, "{error}");
    let (status, current) = request(
        port,
        "GET",
        &format!("/api/v1/opportunities/{opportunity}/notes"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(current["markdown"], before);
    assert_eq!(current["revision"], notes["revision"]);
    let stored: String = sqlx::query_scalar("SELECT notes_markdown FROM opportunity WHERE id=?")
        .bind(&opportunity)
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(stored, before);
    drop(service);
    remove_db(&path);
}

#[tokio::test]
async fn s05_board_placement_and_archive_are_atomic_and_independent() {
    let (path, workspace, port, service) = setup().await;
    let first = create_opportunity(port, "First card").await;
    let second = create_opportunity(port, "Second card").await;
    let (status, notes) = request(
        port,
        "PATCH",
        &format!("/api/v1/opportunities/{first}/notes"),
        Some(json!({"markdown":"- [ ] Preserve this follow-up\n","expected_revision":1})),
    )
    .await;
    assert_eq!(status, 200, "{notes}");
    let (status, board) = request(port, "GET", "/api/v1/board", None).await;
    assert_eq!(status, 200, "{board}");
    let inbox = board["lists"][0]["id"].as_str().unwrap().to_owned();
    let (status, friday) = request(
        port,
        "POST",
        "/api/v1/board/lists",
        Some(json!({"title":"Friday follow-ups"})),
    )
    .await;
    assert_eq!(status, 201, "{friday}");
    let friday_id = friday["list"]["id"].as_str().unwrap().to_owned();
    let (status, board) = request(port, "GET", "/api/v1/board", None).await;
    assert_eq!(status, 200);
    let inbox_revision = board["lists"]
        .as_array()
        .unwrap()
        .iter()
        .find(|list| list["id"] == inbox)
        .unwrap()["revision"]
        .as_i64()
        .unwrap();
    let friday_revision = board["lists"]
        .as_array()
        .unwrap()
        .iter()
        .find(|list| list["id"] == friday_id)
        .unwrap()["revision"]
        .as_i64()
        .unwrap();
    let placement_revision = board["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["opportunity"]["id"] == first)
        .unwrap()["placement"]["revision"]
        .as_i64()
        .unwrap();
    let (status, moved) = request(port, "POST", &format!("/api/v1/opportunities/{first}/placement"), Some(json!({"list_id":friday_id,"before_opportunity_id":null,"expected_source_list_revision":inbox_revision,"expected_list_revision":friday_revision,"expected_placement_revision":placement_revision}))).await;
    assert_eq!(status, 200, "{moved}");
    let (status, archived) = request(port, "POST", &format!("/api/v1/board/lists/{inbox}/archive"), Some(json!({"destination_list_id":friday_id,"expected_list_revision":moved["source_list_revision"],"expected_destination_revision":moved["destination_list_revision"]}))).await;
    assert_eq!(status, 200, "{archived}");
    let (status, after) = request(port, "GET", "/api/v1/board", None).await;
    assert_eq!(status, 200);
    assert_eq!(after["lists"].as_array().unwrap().len(), 1);
    let cards = after["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 2);
    assert!(
        cards
            .iter()
            .all(|card| card["placement"]["list_id"] == friday_id)
    );
    let ids = cards
        .iter()
        .map(|card| card["opportunity"]["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![first.as_str(), second.as_str()]);
    let (status, preserved_notes) = request(
        port,
        "GET",
        &format!("/api/v1/opportunities/{first}/notes"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(preserved_notes["markdown"], notes["markdown"]);
    assert_eq!(preserved_notes["revision"], notes["revision"]);
    let placement_count: i64 = sqlx::query_scalar("SELECT count(*) FROM board_placement")
        .fetch_one(workspace.pool())
        .await
        .unwrap();
    assert_eq!(placement_count, 2);
    let only_list = &after["lists"][0];
    let (status, error) = request(
        port,
        "POST",
        &format!("/api/v1/board/lists/{friday_id}/archive"),
        Some(json!({"destination_list_id":inbox,"expected_list_revision":only_list["revision"],"expected_destination_revision":0})),
    )
    .await;
    assert_eq!(status, 422, "{error}");
    drop(service);
    remove_db(&path);
}

#[tokio::test]
async fn s05_list_reorder_requires_exact_set_and_revisions() {
    let (path, _workspace, port, service) = setup().await;
    let (status, created) = request(
        port,
        "POST",
        "/api/v1/board/lists",
        Some(json!({"title":"A"})),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    let (status, second) = request(
        port,
        "POST",
        "/api/v1/board/lists",
        Some(json!({"title":"B"})),
    )
    .await;
    assert_eq!(status, 201, "{second}");
    let (status, board) = request(port, "GET", "/api/v1/board", None).await;
    assert_eq!(status, 200);
    let lists = board["lists"].as_array().unwrap();
    let ids = lists
        .iter()
        .map(|list| list["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let revisions = lists
        .iter()
        .map(|list| {
            (
                list["id"].as_str().unwrap().to_owned(),
                list["revision"].as_i64().unwrap(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut reverse = ids.clone();
    reverse.reverse();
    let mut stale_revisions = revisions.clone();
    *stale_revisions.get_mut(&ids[0]).unwrap() += 1;
    let (status, error) = request(
        port,
        "POST",
        "/api/v1/board/lists/reorder",
        Some(json!({"ordered_ids":reverse,"expected_revisions":stale_revisions})),
    )
    .await;
    assert_eq!(status, 412, "{error}");
    let (status, reordered) = request(
        port,
        "POST",
        "/api/v1/board/lists/reorder",
        Some(json!({"ordered_ids":reverse,"expected_revisions":revisions})),
    )
    .await;
    assert_eq!(status, 200, "{reordered}");
    let (status, after) = request(port, "GET", "/api/v1/board", None).await;
    assert_eq!(status, 200);
    let final_ids = after["lists"]
        .as_array()
        .unwrap()
        .iter()
        .map(|list| list["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        final_ids,
        reverse.iter().map(String::as_str).collect::<Vec<_>>()
    );
    drop(service);
    remove_db(&path);
}
