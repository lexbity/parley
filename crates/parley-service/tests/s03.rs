mod common;

use common::Client;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-s03-http-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn create_person(client: &mut Client, name: &str) -> String {
    let (status, person) = client
        .call(
            "POST",
            "/api/v1/entities",
            json!({"kind":"person","name":name}),
        )
        .await;
    assert_eq!(status, 201, "{person}");
    person["data"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn s03_reviewed_manual_message_copy_is_recipient_and_version_bound() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let mut client = Client::new(&service).await;
    let dana = create_person(&mut client, "Dana Lee").await;
    let (status, opportunity) = client
        .call(
            "POST",
            "/api/v1/opportunities",
            json!({"title":"Onboarding exchange","participant_ids":[dana]}),
        )
        .await;
    assert_eq!(status, 201, "{opportunity}");
    let op = opportunity["id"].as_str().unwrap().to_owned();

    let (status, their_interest) = client
        .call(
            "POST",
            &format!("/api/v1/opportunities/{op}/interests"),
            json!({"side":"them","statement":"Reduce onboarding friction"}),
        )
        .await;
    assert_eq!(status, 201, "{their_interest}");
    let (status, my_interest) = client
        .call(
            "POST",
            &format!("/api/v1/opportunities/{op}/interests"),
            json!({"side":"me","statement":"Learn from the team's process"}),
        )
        .await;
    assert_eq!(status, 201, "{my_interest}");
    let (status, exchange) = client
        .call(
            "PUT",
            &format!("/api/v1/opportunities/{op}/exchange"),
            json!({
                "expected_revision":0,
                "they_get":"A practical onboarding checklist",
                "i_get":"A chance to learn from their feedback",
                "interest_ids":[their_interest["id"],my_interest["id"]],
                "fairness_attested":true
            }),
        )
        .await;
    assert_eq!(status, 200, "{exchange}");

    let (status, material) = client
        .call(
            "POST",
            &format!("/api/v1/opportunities/{op}/materials"),
            json!({"recipient_id":dana,"kind":"outreach","title":"Checklist note"}),
        )
        .await;
    assert_eq!(status, 201, "{material}");
    let id = material["id"].as_str().unwrap().to_owned();
    let body = "Hello Dana Lee.";
    let (status, version) = client
        .call(
            "PUT",
            &format!("/api/v1/materials/{id}/versions"),
            json!({
                "expected_revision":1,
                "body":body,
                "sentences":[{"text":body,"claims":[{"type":"generic"}],"rules":[]}],
                "slot_fills":{},
                "protected_tokens":[]
            }),
        )
        .await;
    assert_eq!(status, 200, "{version}");
    let version_id = format!("{id}:1");
    let (status, gate) = client
        .get(&format!(
            "/api/v1/materials/{id}/gate?version_id={version_id}"
        ))
        .await;
    assert_eq!(status, 200, "{gate}");
    let failures: Vec<_> = gate["soft_failures"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(failures.contains(&"S1_GIVE_FIRST"));
    assert!(failures.contains(&"S2_ANCHOR"));

    let (status, denied) = client
        .call(
            "POST",
            &format!("/api/v1/materials/{id}/approval"),
            json!({"expected_revision":2,"version_id":version_id,"override_reasons":{"S1_GIVE_FIRST":"Reviewed this missing gift"}}),
        )
        .await;
    assert_eq!(
        status, 422,
        "approval must require a reason for every soft failure: {denied}"
    );
    let (status, approval) = client
        .call(
            "POST",
            &format!("/api/v1/materials/{id}/approval"),
            json!({
                "expected_revision":2,
                "version_id":version_id,
                "override_reasons":{
                    "S1_GIVE_FIRST":"Owner reviewed the missing gift and chose to proceed",
                    "S2_ANCHOR":"Owner reviewed the missing anchor and chose to proceed"
                }
            }),
        )
        .await;
    assert_eq!(status, 200, "{approval}");

    let (status, wrong_recipient) = client
        .call(
            "POST",
            &format!("/api/v1/materials/{id}/copy-grant"),
            json!({"version_id":version_id,"recipient_id":"someone-else"}),
        )
        .await;
    assert_eq!(
        status, 422,
        "recipient mismatch must deny bytes: {wrong_recipient}"
    );

    let (status, grant) = client
        .call(
            "POST",
            &format!("/api/v1/materials/{id}/copy-grant"),
            json!({"version_id":version_id,"recipient_id":dana}),
        )
        .await;
    assert_eq!(status, 201, "{grant}");
    assert_eq!(grant["body"], body);
    assert_eq!(grant["recipient_id"], dana);
    let grant_id = grant["grant_id"].as_str().unwrap();
    let (status, rejected_ack) = client
        .call(
            "POST",
            &format!("/api/v1/copy-grants/{grant_id}/acknowledgement"),
            json!({"success":false}),
        )
        .await;
    assert_eq!(status, 200, "{rejected_ack}");
    assert_eq!(rejected_ack["status"], "failed");
    let (status, receipt) = client.get(&format!("/api/v1/copy-grants/{grant_id}")).await;
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(receipt["status"], "failed");

    let (status, revised) = client
        .call(
            "PUT",
            &format!("/api/v1/materials/{id}/versions"),
            json!({
                "expected_revision":2,
                "body":"Hello Dana Lee. Revised.",
                "sentences":[{"text":"Hello Dana Lee. Revised.","claims":[{"type":"generic"}],"rules":[]}],
                "slot_fills":{},
                "protected_tokens":[]
            }),
        )
        .await;
    assert_eq!(status, 200, "{revised}");
    let (status, stale_grant) = client
        .call(
            "POST",
            &format!("/api/v1/materials/{id}/copy-grant"),
            json!({"version_id":version_id,"recipient_id":dana}),
        )
        .await;
    assert_eq!(
        status, 422,
        "a superseded version cannot be issued: {stale_grant}"
    );
    let (status, materials) = client
        .get(&format!("/api/v1/opportunities/{op}/materials"))
        .await;
    assert_eq!(status, 200, "{materials}");
    assert_eq!(materials["data"][0]["versions"][0]["body"], body);

    service.shutdown().await;
    let _ = std::fs::remove_file(db);
}
