mod party_http;
use party_http::*;
use serde_json::json;

#[tokio::test]
async fn p03_private_attachments() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let (a, _) = create_party(&service, &session, "org", "Fieldwork", "CSV").await;
    let (b, _) = create_party(&service, &session, "person", "Mira", "").await;
    let bytes = b"private fixture";
    let mut references = vec![];
    for party in [&a, &b] {
        let file = raw_request(
            &service,
            &session,
            "POST",
            "/api/v1/uploads",
            bytes,
            "text/plain",
            &[
                ("X-Parley-Filename", "name=fixture.txt"),
                ("X-Parley-Purpose", "card-attachment"),
                ("Idempotency-Key", &next_key()),
            ],
        )
        .await;
        assert_eq!(file.status, 201, "{file:?}");
        let path = format!("/api/v1/entities/{party}/attachments");
        let input = json!({"upload_id":file.body["id"]});
        let mut bound = None;
        for _ in 0..2 {
            let result = request(
                &service,
                &session,
                "POST",
                &path,
                Some(input.clone()),
                &[("Idempotency-Key", &next_key())],
            )
            .await;
            assert_eq!(result.status, 201, "{result:?}");
            assert_eq!(result.body["data"]["share_permission"], false);
            let id = result.body["data"]["id"].as_str().unwrap().to_owned();
            if let Some(prev) = &bound {
                assert_eq!(&id, prev);
            }
            bound = Some(id);
        }
        references.push(format!("{path}/{}", bound.unwrap()));
    }
    let uploads = request(&service, &session, "GET", "/api/v1/uploads", None, &[]).await;
    assert_eq!(uploads.body.as_array().unwrap().len(), 0);
    let path = format!(
        "/api/v1/entities/{b}/attachments/{}/content",
        references[0].rsplit('/').next().unwrap()
    );
    assert_eq!(
        request(&service, &session, "GET", &path, None, &[])
            .await
            .status,
        404
    );
    let res = request(
        &service,
        &session,
        "DELETE",
        &references[0],
        None,
        &[("Idempotency-Key", &next_key())],
    )
    .await;
    assert_eq!(res.status, 204);
    service.shutdown().await;
    let service = parley_service::start_workspace(&db).await.unwrap();
    let session = owner_session(&service).await;
    let res = request(
        &service,
        &session,
        "GET",
        &format!("{}/content", references[1]),
        None,
        &[],
    )
    .await;
    assert_eq!(res.status, 200);
    assert_eq!(res.bytes, bytes);
    service.shutdown().await;
}
