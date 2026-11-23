mod party_http;
use party_http::*;
use serde_json::json;

#[tokio::test]
async fn p02_related_and_existing_edges() {
    let db = path();
    let s = parley_service::start_workspace(&db).await.unwrap();
    let sess = owner_session(&s).await;
    let (org, _) = create_party(&s, &sess, "org", "Fieldwork", "").await;
    let quoted = "Fieldwork publishes the contributing guide.";
    let capture = request(
        &s,
        &sess,
        "POST",
        "/api/v1/captures",
        Some(json!({"text":quoted})),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(capture.status, 201, "{capture:?}");
    let occurrence = capture.body["occurrence_id"].as_str().unwrap();
    let mut card = json!({"kind":"source","name":"Contributing guide","related_entity_id":org,"verb":"publishes","occurrence_id":occurrence,"quote":quoted,"x":0,"y":0,"view_scope":"leads","view_id":org,"notes":"## Guide","expected_parent_revision":1,"direction":"outgoing"});
    for (key, value, status) in [
        ("staged_upload_ids", json!(["expired"]), 422),
        ("expected_parent_revision", json!(8), 412),
        ("verb", json!("maintains"), 422),
    ] {
        let mut bad = card.clone();
        bad[key] = value;
        assert_eq!(
            request(
                &s,
                &sess,
                "POST",
                "/api/v1/graph/related",
                Some(bad),
                &[("Idempotency-Key", &next_key())]
            )
            .await
            .status,
            status
        );
    }
    let initial = request(&s, &sess, "GET", "/api/v1/directory?kind=source", None, &[]).await;
    assert_eq!(initial.body["data"].as_array().unwrap().len(), 0);
    let uploaded = raw_request(
        &s,
        &sess,
        "POST",
        "/api/v1/uploads",
        b"contents",
        "text/plain",
        &[],
    )
    .await;
    card["staged_upload_ids"] = json!([uploaded.body["id"]]);
    let key = next_key();
    let created = request(
        &s,
        &sess,
        "POST",
        "/api/v1/graph/related",
        Some(card.clone()),
        &[("Idempotency-Key", &key)],
    )
    .await;
    assert_eq!(created.status, 201, "{created:?}");
    assert_eq!(
        created.body,
        request(
            &s,
            &sess,
            "POST",
            "/api/v1/graph/related",
            Some(card),
            &[("Idempotency-Key", &key)]
        )
        .await
        .body
    );
    let node = created.body["id"].as_str().unwrap();
    let dossier = request(
        &s,
        &sess,
        "GET",
        &format!("/api/v1/entities/{node}/dossier"),
        None,
        &[],
    )
    .await;
    assert_eq!(dossier.body["data"]["files"].as_array().unwrap().len(), 1);
    let notes = request(
        &s,
        &sess,
        "GET",
        &format!("/api/v1/entities/{node}/notes"),
        None,
        &[],
    )
    .await;
    assert_eq!(notes.body["data"]["markdown"], "## Guide");
    let mut input = json!({"from_id":org,"to_id":node,"verb":"publishes","occurrence_id":occurrence,"quote":quoted,"expected_from_revision":1,"expected_to_revision":1});
    let link = request(
        &s,
        &sess,
        "POST",
        "/api/v1/edges",
        Some(input.clone()),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(link.status, 201, "{link:?}");
    let edge_path = format!(
        "/api/v1/edges/{}",
        link.body["data"]["id"].as_str().unwrap()
    );
    input["expected_revision"] = json!(1);
    input["quote"] = json!("contributing guide");
    let corrected = request(
        &s,
        &sess,
        "PATCH",
        &edge_path,
        Some(input),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(corrected.status, 200, "{corrected:?}");
    assert_eq!(corrected.body["data"]["revision"], 2);
    for (revision, status) in [(1, 412), (2, 204)] {
        let removed = request(
            &s,
            &sess,
            "DELETE",
            &edge_path,
            Some(json!({"expected_revision":revision})),
            &[("Idempotency-Key", &next_key())],
        )
        .await;
        assert_eq!(removed.status, status, "{removed:?}");
    }
    let found = request(
        &s,
        &sess,
        "GET",
        "/api/v1/directory?kind=source&q=guide",
        None,
        &[],
    )
    .await;
    assert_eq!(found.body["data"][0]["id"], node);
    let artifact=request(&s,&sess,"POST","/api/v1/graph/related",Some(json!({"kind":"artifact","name":"Quoted fixture","related_entity_id":org,"verb":"could_help","occurrence_id":occurrence,"quote":quoted,"x":50,"y":30,"view_scope":"leads","view_id":org,"direction":"incoming","expected_parent_revision":1})),&[("Idempotency-Key",&next_key())]).await;
    assert_eq!(artifact.status, 201, "{artifact:?}");
    let aid = artifact.body["id"].as_str().unwrap();
    let resource_path = format!("/api/v1/entities/{aid}");
    let resource = request(&s, &sess, "GET", &resource_path, None, &[]).await;
    assert_eq!(resource.body["data"]["title"], "Quoted fixture");
    let renamed = request(
        &s,
        &sess,
        "PATCH",
        &resource_path,
        Some(json!({"title":"CSV fixture","body":{}})),
        &[
            ("Idempotency-Key", &next_key()),
            ("If-Match", resource.body["data"]["etag"].as_str().unwrap()),
        ],
    )
    .await;
    assert_eq!(renamed.status, 200, "{renamed:?}");
    let root = request(
        &s,
        &sess,
        "GET",
        &format!("/api/v1/entities/{org}/dossier"),
        None,
        &[],
    )
    .await;
    assert_eq!(
        root.body["data"]["associated_resources"][0]["paths"][0]["via"],
        "edge"
    );
    let selected = request(
        &s,
        &sess,
        "GET",
        &format!("{resource_path}/dossier"),
        None,
        &[],
    )
    .await;
    assert_eq!(selected.body["data"]["entity"]["name"], "CSV fixture");
    let card_title = request(
        &s,
        &sess,
        "PATCH",
        &resource_path,
        Some(json!({"name":"Accepted fixture"})),
        &[
            ("Idempotency-Key", &next_key()),
            (
                "If-Match",
                selected.body["data"]["entity"]["etag"].as_str().unwrap(),
            ),
        ],
    )
    .await;
    assert_eq!(card_title.status, 200, "{card_title:?}");
    let card = request(&s, &sess, "GET", &resource_path, None, &[]).await;
    assert_eq!(card.body["data"]["title"], "Accepted fixture");
    s.shutdown().await;
}
