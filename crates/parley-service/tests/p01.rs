// P01: Party sheet rename, typed focus edits, entity notes and the inspector
// dossier (amendment 2026-11-22; RFC-0004 wire conventions).
mod party_http;
use party_http::*;
use serde_json::json;
use std::{
    fs,
    time::{Duration, Instant},
};
#[tokio::test]
async fn p01_session_advertises_party_capabilities() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let response = request(&service, &session, "GET", "/api/v1/session", None, &[]).await;
    assert_eq!(response.status, 200);
    let capabilities = response.body["capabilities"].as_array().unwrap();
    for expected in ["party.sheet", "party.notes", "party.dossier"] {
        assert!(
            capabilities
                .iter()
                .any(|item| item.as_str() == Some(expected)),
            "missing capability {expected}: {capabilities:?}"
        );
    }
    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn p01_party_sheet_focus_edit_discards_stale_and_blank_writes() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let (id, first_etag) =
        create_party(&service, &session, "person", "Dana Lee", "Maintenance").await;

    let graph = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/graph?center_id={id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(graph.status, 200);
    assert_eq!(graph.body["omitted_nodes"], 0);
    assert_eq!(graph.body["omitted_edges"], 0);

    // Directory rows carry the editable focus and the strong entity ETag.
    let list = request(
        &service,
        &session,
        "GET",
        "/api/v1/entities?kind=person&kind=org",
        None,
        &[],
    )
    .await;
    assert_eq!(list.status, 200);
    let rows = list.body["data"].as_array().unwrap();
    let row = rows.iter().find(|row| row["id"] == id).unwrap();
    assert_eq!(row["focus"], "Maintenance");
    assert_eq!(row["etag"], first_etag);

    // Typed focus PATCH under the current ETag.
    let updated = request(
        &service,
        &session,
        "PATCH",
        &format!("/api/v1/entities/{id}"),
        Some(json!({"focus": "CSV tools"})),
        &[
            ("If-Match", first_etag.as_str()),
            ("Idempotency-Key", next_key().as_str()),
        ],
    )
    .await;
    assert_eq!(updated.status, 200, "{updated:?}");
    let data = &updated.body["data"];
    assert_eq!(data["focus"], "CSV tools");
    assert_eq!(data["revision"], 2);
    let second_etag = data["etag"].as_str().unwrap().to_owned();

    // Stale ETag is rejected without consuming the write.
    let stale = request(
        &service,
        &session,
        "PATCH",
        &format!("/api/v1/entities/{id}"),
        Some(json!({"focus": "Outdated"})),
        &[
            ("If-Match", first_etag.as_str()),
            ("Idempotency-Key", next_key().as_str()),
        ],
    )
    .await;
    assert_eq!(stale.status, 412);
    assert_eq!(stale.body["error"]["code"], "STALE_REVISION");

    // Blank names are rejected; subtype transitions stay out of label edits.
    let blank = request(
        &service,
        &session,
        "PATCH",
        &format!("/api/v1/entities/{id}"),
        Some(json!({"name": "   "})),
        &[
            ("If-Match", second_etag.as_str()),
            ("Idempotency-Key", next_key().as_str()),
        ],
    )
    .await;
    assert_eq!(blank.status, 422, "{blank:?}");
    let confirm = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(confirm.status, 200);
    assert_eq!(confirm.body["data"]["name"], "Dana Lee");

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn p01_entity_notes_canonical_roundtrip_conflict_and_replay() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let (id, _) = create_party(&service, &session, "org", "Fieldwork", "CSV tools").await;

    let initial = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{id}/notes"),
        None,
        &[],
    )
    .await;
    assert_eq!(initial.status, 200);
    assert_eq!(initial.body["data"]["markdown"], "");
    assert_eq!(initial.body["data"]["revision"], 1);
    let initial_etag = initial.body["data"]["etag"].as_str().unwrap().to_owned();

    // CRLF input is canonicalized to LF and round-trips byte-exactly.
    let key = next_key();
    let written = request(
        &service,
        &session,
        "PUT",
        &format!("/api/v1/entities/{id}/notes"),
        Some(json!({"markdown": "Welcomes **small, reproducible fixtures**.\r\n- line one\r\n- line two\r\n", "expected_notes_rev": 1})),
        &[
            ("If-Match", initial_etag.as_str()),
            ("Idempotency-Key", key.as_str()),
        ],
    )
    .await;
    assert_eq!(written.status, 200, "{written:?}");
    assert_eq!(written.body["data"]["revision"], 2);
    assert_eq!(
        written.body["data"]["markdown"],
        "Welcomes **small, reproducible fixtures**.\n- line one\n- line two\n"
    );
    let second_etag = written.body["data"]["etag"].as_str().unwrap().to_owned();

    // A competing stale write is rejected and keeps the committed revision.
    let stale = request(
        &service,
        &session,
        "PUT",
        &format!("/api/v1/entities/{id}/notes"),
        Some(json!({"markdown": "stale", "expected_notes_rev": 1})),
        &[
            ("If-Match", initial_etag.as_str()),
            ("Idempotency-Key", next_key().as_str()),
        ],
    )
    .await;
    assert_eq!(stale.status, 412, "{stale:?}");

    // Replaying the exact committed command under the same key returns the
    // original success; reusing the key for a different body conflicts.
    let replay = request(
        &service,
        &session,
        "PUT",
        &format!("/api/v1/entities/{id}/notes"),
        Some(json!({"markdown": "Welcomes **small, reproducible fixtures**.\r\n- line one\r\n- line two\r\n", "expected_notes_rev": 1})),
        &[
            ("If-Match", initial_etag.as_str()),
            ("Idempotency-Key", key.as_str()),
        ],
    )
    .await;
    assert_eq!(replay.status, 200, "{replay:?}");
    assert_eq!(replay.body["data"]["revision"], 2);
    let reused = request(
        &service,
        &session,
        "PUT",
        &format!("/api/v1/entities/{id}/notes"),
        Some(json!({"markdown": "different body", "expected_notes_rev": 2})),
        &[
            ("If-Match", second_etag.as_str()),
            ("Idempotency-Key", key.as_str()),
        ],
    )
    .await;
    assert_eq!(reused.status, 409);
    assert_eq!(reused.body["error"]["code"], "KEY_REUSED");

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn p01_dossier_lists_direct_links_and_open_opportunities() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let (org_id, _) = create_party(&service, &session, "org", "Fieldwork", "CSV tools").await;

    // Capture a source occurrence and attach a source-backed related person.
    let captured = request(
        &service,
        &session,
        "POST",
        "/api/v1/captures",
        Some(json!({"text": "Fieldwork welcomes small fixtures. Mira maintains the CSV tools.", "title": "Guide", "url": "https://example.invalid/guide"})),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(captured.status, 201, "{captured:?}");
    let occurrence = captured.body["occurrence_id"].as_str().unwrap().to_owned();

    let related = request(
        &service,
        &session,
        "POST",
        "/api/v1/graph/related",
        Some(json!({
            "kind": "person", "name": "Mira Chen", "related_entity_id": org_id,
            "verb": "member_of", "occurrence_id": occurrence,
            "quote": "Mira maintains the CSV tools.",
            "view_scope": "leads", "view_id": org_id, "x": 120.0, "y": 40.0
        })),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(related.status, 201, "{related:?}");

    // Open an opportunity with the organization as primary participant.
    let opportunity = request(
        &service,
        &session,
        "POST",
        "/api/v1/opportunities",
        Some(json!({"title": "Onboarding exchange", "primary_party_id": org_id})),
        &[("Idempotency-Key", next_key().as_str())],
    )
    .await;
    assert_eq!(opportunity.status, 201, "{opportunity:?}");

    let dossier = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/entities/{org_id}/dossier"),
        None,
        &[],
    )
    .await;
    assert_eq!(dossier.status, 200, "{dossier:?}");
    let data = &dossier.body["data"];
    assert_eq!(data["entity"]["id"], org_id);
    assert_eq!(data["entity"]["focus"], "CSV tools");
    let links = data["direct_links"].as_array().unwrap();
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0]["verb"], "member_of");
    assert!(links[0]["tier"].as_i64().unwrap() >= 1);
    let opportunities = data["opportunities"].as_array().unwrap();
    assert_eq!(opportunities.len(), 1);
    assert_eq!(opportunities[0]["title"], "Onboarding exchange");
    assert!(data["files"].as_array().unwrap().is_empty());
    assert!(data["associated_resources"].as_array().unwrap().is_empty());

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn p01_bounded_directory_and_neighborhood_at_5000_parties() {
    let db = path();
    let workspace = parley_service::Workspace::open(&db).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<5000) INSERT INTO entity(id,kind,name,revision,created_at,updated_at) SELECT printf('scale-%05d',i),'person',printf('Person %05d',i),1,'2026-11-22','2026-11-22' FROM n").execute(workspace.pool()).await.unwrap();
    workspace.pool().close().await;
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let mut latency = Vec::new();
    for _ in 0..20 {
        let start = std::time::Instant::now();
        let list = request(
            &service,
            &session,
            "GET",
            "/api/v1/directory?limit=50",
            None,
            &[],
        )
        .await;
        let page = &list.body["data"];
        assert_eq!(page.as_array().unwrap().len(), 50);
        assert_eq!(page[0]["id"], "scale-00001");
        let next = list.body["next_cursor"].as_str().unwrap();
        let next_page = request(
            &service,
            &session,
            "GET",
            &format!("/api/v1/directory?limit=50&cursor={next}"),
            None,
            &[],
        )
        .await;
        assert_eq!(next_page.body["data"][0]["id"], "scale-00051");
        let neighborhood = request(
            &service,
            &session,
            "GET",
            "/api/v1/graph?center_id=scale-05000",
            None,
            &[],
        )
        .await;
        assert_eq!(neighborhood.body["nodes"].as_array().unwrap().len(), 1);
        latency.push(start.elapsed());
    }
    latency.sort();
    eprintln!(
        "Party directory, next page and neighborhood, 5000 parties: p95 {:?}",
        latency[18]
    );
    assert!(latency[18] < std::time::Duration::from_millis(500));
    service.shutdown().await;
}

#[tokio::test]
async fn p01_opportunity_list_page_is_bounded_searchable_and_keyset_paginated() {
    let db = path();
    let workspace = parley_service::Workspace::open(&db).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<12) INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) SELECT printf('page-opp-%02d',i),printf('Scoped title %02d',i),'open',1,'2026-11-24','2026-11-24T00:00:00Z' FROM n")
        .execute(workspace.pool())
        .await
        .unwrap();
    workspace.pool().close().await;
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;

    let first = request(
        &service,
        &session,
        "GET",
        "/api/v1/opportunities?limit=5&sort=title",
        None,
        &[],
    )
    .await;
    assert_eq!(first.status, 200, "{:?}", first.body);
    let page = &first.body;
    assert!(page.get("items").is_some(), "paged shape: {page:?}");
    assert_eq!(page["items"].as_array().unwrap().len(), 5);
    assert_eq!(page["items"][0]["title"], "Scoped title 01");
    let cursor = page["next_cursor"].as_str().unwrap().to_owned();

    let second = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/opportunities?limit=5&sort=title&cursor={cursor}"),
        None,
        &[],
    )
    .await;
    assert_eq!(second.status, 200, "{:?}", second.body);
    let ids: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second.body["items"].as_array().unwrap().iter())
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 10);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        10
    );
    assert_eq!(second.body["items"][0]["title"], "Scoped title 06");

    // A cursor bound to another query is rejected rather than silently widening.
    let wrong = request(
        &service,
        &session,
        "GET",
        &format!("/api/v1/opportunities?limit=5&sort=title&q=missing&cursor={cursor}"),
        None,
        &[],
    )
    .await;
    assert_eq!(wrong.status, 422, "{:?}", wrong.body);

    // Server-side typeahead narrows the whole domain, not just the first page.
    let search = request(
        &service,
        &session,
        "GET",
        "/api/v1/opportunities?limit=5&q=title%2011",
        None,
        &[],
    )
    .await;
    assert_eq!(search.status, 200, "{:?}", search.body);
    let rows = search.body["items"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["title"], "Scoped title 11");

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn p01_queue_sheet_list_at_10000_opportunities_meets_render_budget() {
    let db = path();
    let workspace = parley_service::Workspace::open(&db).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<10000) INSERT INTO opportunity(id,title,state,revision,created_at,updated_at) SELECT printf('scale-opp-%05d',i),printf('Opportunity %05d',i),'open',1,'2026-11-24','2026-11-24' FROM n")
        .execute(workspace.pool())
        .await
        .unwrap();
    workspace.pool().close().await;
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;

    // The UI renders view=list. Its first page must not grow with the table.
    // Mirror the s10 benchmark: one cold read, then 20 loopback reads with the
    // response JSON parsed; p95 is the 19th of 20 sorted samples.
    let cold = request(
        &service,
        &session,
        "GET",
        "/api/v1/queue?view=list&limit=50",
        None,
        &[],
    )
    .await;
    assert_eq!(cold.status, 200, "{:?}", cold.body);
    let mut latency = Vec::new();
    for _ in 0..20 {
        let started = Instant::now();
        let response = request(
            &service,
            &session,
            "GET",
            "/api/v1/queue?view=list&limit=50",
            None,
            &[],
        )
        .await;
        assert_eq!(response.status, 200, "{:?}", response.body);
        let sheet = &response.body;
        assert_eq!(sheet["total"], 10000);
        assert_eq!(sheet["rows"].as_array().unwrap().len(), 50);
        latency.push(started.elapsed());
    }
    latency.sort();
    let p95 = latency[18];
    eprintln!("Queue sheet list at 10000 opportunities: p95 {p95:?}");
    assert!(p95 < Duration::from_millis(200), "sheet p95 {p95:?}");

    // Keyset pagination walks every row exactly once.
    let mut cursor: Option<String> = None;
    let mut seen = std::collections::HashSet::new();
    let mut pages = 0;
    loop {
        let path = match &cursor {
            Some(cursor) => format!("/api/v1/queue?view=sheet&limit=200&cursor={cursor}"),
            None => "/api/v1/queue?view=sheet&limit=200".to_owned(),
        };
        let response = request(&service, &session, "GET", &path, None, &[]).await;
        assert_eq!(response.status, 200, "{:?}", response.body);
        let sheet = &response.body;
        assert_eq!(sheet["total"], 10000);
        for row in sheet["rows"].as_array().unwrap() {
            assert!(seen.insert(row["opportunity_id"].as_str().unwrap().to_owned()));
        }
        pages += 1;
        cursor = sheet["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
        assert!(pages <= 60, "pagination did not terminate");
    }
    assert_eq!(seen.len(), 10000);
    assert_eq!(pages, 50);

    service.shutdown().await;
    let _ = fs::remove_file(&db);
}

#[tokio::test]
async fn p01_shell_deep_links_reach_the_packaged_index() {
    let db = path();
    let s = parley_service::start_workspace(&db).await.unwrap();
    let sess = owner_session(&s).await;
    let index = request(&s, &sess, "GET", "/", None, &[]).await;
    for tab in [
        "resources",
        "parties",
        "leads",
        "queue",
        "output",
        "settings",
    ] {
        for suffix in ["", "/", "/record", "/record/"] {
            let page = request(&s, &sess, "GET", &format!("/{tab}{suffix}"), None, &[]).await;
            assert_eq!(page.status, index.status, "{tab}{suffix}");
            assert_eq!(page.bytes, index.bytes, "{tab}{suffix}");
        }
    }
    for path in ["/api/v1/no-route", "/parties/bad/route", "/not-a-tab"] {
        assert_eq!(request(&s, &sess, "GET", path, None, &[]).await.status, 404);
    }
    s.shutdown().await;
}
