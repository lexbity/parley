mod party_http;
use party_http::*;
use serde_json::{Value, json};

#[tokio::test]
async fn p04_csv_preview_commit() {
    let db = path();
    let s = parley_service::start_workspace(&db).await.unwrap();
    let sess = owner_session(&s).await;
    let raw = "\u{feff}Name,Notes,Skip\r\n\"Mira, Chen\",\"one\r\ntwo\"\"!\",unused\r\nMira,,unused\r\nMira,,unused\r\n,missing name,unused\r\n";
    let stage = raw_request(
        &s,
        &sess,
        "POST",
        "/api/v1/party-imports",
        raw.as_bytes(),
        "text/csv",
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(stage.status, 201, "{stage:?}");
    let base = format!(
        "/api/v1/party-imports/{}",
        stage.body["data"]["stage_id"].as_str().unwrap()
    );
    let mut mapping = json!({"name_column":0,"notes_column":1,"party_type":"person","excluded_records":[],"distinct_records":[]});
    let preview_path = format!("{base}/preview");
    let commit_path = format!("{base}/commit");
    let preview = request(
        &s,
        &sess,
        "POST",
        &preview_path,
        Some(mapping.clone()),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(preview.status, 200, "{preview:?}");
    assert_eq!(preview.body["data"]["error_count"], 3);
    mapping["excluded_records"] = json!([4, 5]);
    mapping["distinct_records"] = json!([3]);
    let approval =
        |v: &Value| json!({"preview_id":v["data"]["preview_id"],"digest":v["data"]["digest"]});
    let preview = request(
        &s,
        &sess,
        "POST",
        &preview_path,
        Some(mapping.clone()),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    create_party(&s, &sess, "person", "Mira, Chen", "existing").await;
    let stale = request(
        &s,
        &sess,
        "POST",
        &commit_path,
        Some(approval(&preview.body)),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(stale.status, 412, "{stale:?}");
    mapping["distinct_records"] = json!([2, 3]);
    let preview = request(
        &s,
        &sess,
        "POST",
        &preview_path,
        Some(mapping),
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(preview.body["data"]["error_count"], 0);
    let key = next_key();
    let mut previous = None;
    for _ in 0..2 {
        let result = request(
            &s,
            &sess,
            "POST",
            &commit_path,
            Some(approval(&preview.body)),
            &[("Idempotency-Key", &key)],
        )
        .await;
        assert_eq!(result.status, 201, "{result:?}");
        assert_eq!(result.body["data"]["skipped_count"], 2);
        if let Some(old) = &previous {
            assert_eq!(old, &result.body);
        }
        previous = Some(result.body);
    }
    let created = previous.unwrap()["data"]["created"].clone();
    assert_eq!(created.as_array().unwrap().len(), 2);
    assert_eq!(created[0]["source_record"], 2);
    let notes = request(
        &s,
        &sess,
        "GET",
        &format!(
            "/api/v1/entities/{}/notes",
            created[0]["party_id"].as_str().unwrap()
        ),
        None,
        &[],
    )
    .await;
    assert_eq!(notes.body["data"]["markdown"], "one\ntwo\"!");
    for raw in [b"Name,Focus\nx".as_slice(), b"Name\nmi\"ra", b"\xff"] {
        let bad = raw_request(
            &s,
            &sess,
            "POST",
            "/api/v1/party-imports",
            raw,
            "text/csv",
            &[("Idempotency-Key", &next_key())],
        )
        .await;
        assert_eq!(bad.status, 422, "{bad:?}");
    }
    s.shutdown().await;
}
