mod common;

use common::Client;
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-s04-http-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn entity(client: &mut Client, kind: &str, name: &str) -> String {
    let (status, value) = client
        .call("POST", "/api/v1/entities", json!({"kind":kind,"name":name}))
        .await;
    assert_eq!(status, 201, "{value}");
    value["data"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn s04_partial_capture_review_preserves_unresolved_identity() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let mut client = Client::new(&service).await;
    let northline = entity(&mut client, "org", "Northline").await;
    let dana = entity(&mut client, "person", "Dana Lee").await;
    let (status, first) = client
        .call(
            "POST",
            "/api/v1/opportunities",
            json!({"title":"Onboarding","participant_ids":[northline]}),
        )
        .await;
    assert_eq!(status, 201, "{first}");
    let (status, second) = client
        .call(
            "POST",
            "/api/v1/opportunities",
            json!({"title":"Hiring","participant_ids":[northline]}),
        )
        .await;
    assert_eq!(status, 201, "{second}");
    let first_id = first["id"].as_str().unwrap().to_owned();
    let second_id = second["id"].as_str().unwrap().to_owned();
    let source = "The next engineer will work with Dana on onboarding.";
    let (status, staged) = client
        .call(
            "POST",
            "/api/v1/capture-stages",
            json!({
                "text":source,
                "title":"Hiring update",
                "url":"https://example.invalid/hiring",
                "opportunity_ids":[first_id,second_id],
                "actor_kind":"unknown",
                "actor_entity_id":null
            }),
        )
        .await;
    assert_eq!(status, 202, "{staged}");
    let batch = staged["batch_id"].as_str().unwrap().to_owned();
    let job = staged["job_id"].as_str().unwrap().to_owned();

    // Keep the HTTP journey deterministic without fabricating product output:
    // the proposal rows below are explicit synthetic fixture state. The real
    // Extractor is not invoked by this test and its status remains failed.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{}", db.display()))
        .await
        .unwrap();
    sqlx::query("UPDATE agent_job SET status='failed',validation_errors_json='[\"fixture provider unavailable\"]',completed_at=CURRENT_TIMESTAMP WHERE id=?")
        .bind(&job)
        .execute(&pool)
        .await
        .unwrap();
    let (variant, occurrence): (String, String) = sqlx::query_as(
        "SELECT b.source_variant_id,o.id FROM proposal_batch b JOIN source_occurrence o ON o.source_variant_id=b.source_variant_id WHERE b.id=? ORDER BY o.captured_at DESC LIMIT 1",
    )
    .bind(&batch)
    .fetch_one(&pool)
    .await
    .unwrap();
    let quote = "next engineer will work with Dana";
    let start = source.find(quote).unwrap() as i64;
    let end = start + quote.len() as i64;
    for (id, action, target_kind, label, unresolved, candidate) in [
        (
            "proposal-hiring-signal",
            "create_claim",
            "claim",
            "The next engineer will work with Dana on onboarding.",
            0_i64,
            json!({"kind":"problem_signal","statement":"The next engineer will work with Dana on onboarding.","source_occurrence_id":occurrence,"source_variant_id":variant}),
        ),
        (
            "proposal-dana-mention",
            "create_entity",
            "entity",
            "Dana",
            1_i64,
            json!({"kind":"stakeholder","name":"Dana","source_occurrence_id":occurrence,"source_variant_id":variant}),
        ),
    ] {
        sqlx::query("INSERT INTO capture_review_item(id,batch_id,action,target_kind,label,quote,tier,ambiguity,unresolved,candidate_json,quote_start,quote_end,normalized_start,normalized_end,normalization_version,created_at) VALUES(?,?,?,?,?,?,1,NULL,?,?,?,?,?,?,1,CURRENT_TIMESTAMP)")
            .bind(id)
            .bind(&batch)
            .bind(action)
            .bind(target_kind)
            .bind(label)
            .bind(quote)
            .bind(unresolved)
            .bind(candidate.to_string())
            .bind(start)
            .bind(end)
            .bind(start)
            .bind(end)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE proposal_batch SET revision=2,status='pending' WHERE id=?")
        .bind(&batch)
        .execute(&pool)
        .await
        .unwrap();

    let (status, review) = client.get(&format!("/api/v1/capture-stages/{batch}")).await;
    assert_eq!(status, 200, "{review}");
    assert_eq!(review["injection_suspected"], false);
    assert_eq!(review["source"]["text"], source);
    assert_eq!(review["opportunity_ids"].as_array().unwrap().len(), 2);
    assert_eq!(review["proposals"].as_array().unwrap().len(), 2);
    assert!(
        review["proposals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["unresolved"] == true && p["target_id"].is_null())
    );

    let signal_id = review["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["target_kind"] == "claim")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let (status, applied) = client
        .call(
            "POST",
            &format!("/api/v1/capture-stages/{batch}/decisions"),
            json!({
                "expected_revision":2,
                "decisions":[{"proposal_id":signal_id,"action":"confirm","target_id":northline}]
            }),
        )
        .await;
    assert_eq!(status, 200, "{applied}");
    assert_eq!(applied["status"], "partially_applied");
    let (status, after) = client.get(&format!("/api/v1/capture-stages/{batch}")).await;
    assert_eq!(status, 200, "{after}");
    let unresolved = after["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["target_label"] == "Dana")
        .unwrap();
    assert_eq!(unresolved["review_state"], "proposed");
    assert_eq!(unresolved["unresolved"], true);
    assert!(unresolved["target_id"].is_null());

    let (status, linked) = client
        .get(&format!("/api/v1/opportunities/{first_id}/materials"))
        .await;
    assert_eq!(status, 200, "{linked}");
    // The source is one shared normalized variant linked to both opportunities;
    // it is never duplicated by a review decision.
    let association_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM capture_batch_opportunity WHERE batch_id=?")
            .bind(&batch)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(association_count, 2);
    let variant_count: i64 = sqlx::query_scalar("SELECT count(*) FROM source_variant WHERE id=?")
        .bind(&variant)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(variant_count, 1);

    // A separate capture exercises durable cancellation and the explicit
    // source-only path without accepting any staged proposals.
    let (status, cancel_stage) = client
        .call(
            "POST",
            "/api/v1/capture-stages",
            json!({"text":"A second, independent source.","title":null,"url":null,"opportunity_ids":[],"actor_kind":"unknown","actor_entity_id":null}),
        )
        .await;
    assert_eq!(status, 202, "{cancel_stage}");
    let cancel_batch = cancel_stage["batch_id"].as_str().unwrap();
    let cancel_job = cancel_stage["job_id"].as_str().unwrap();
    sqlx::query("UPDATE agent_job SET status='running' WHERE id=?")
        .bind(cancel_job)
        .execute(&pool)
        .await
        .unwrap();
    let (status, cancelled) = client
        .call(
            "POST",
            &format!("/api/v1/capture-stages/{cancel_batch}/cancel"),
            json!({"job_id":cancel_job}),
        )
        .await;
    assert_eq!(status, 200, "{cancelled}");
    assert_eq!(cancelled["status"], "cancelled");
    assert!(cancelled["committed"].is_null());
    let (status, cancelled_review) = client
        .get(&format!("/api/v1/capture-stages/{cancel_batch}"))
        .await;
    assert_eq!(status, 200, "{cancelled_review}");
    assert!(cancelled_review["proposals"].as_array().unwrap().is_empty());
    let (status, source_only_stage) = client
        .call(
            "POST",
            "/api/v1/capture-stages",
            json!({"text":"Keep this capture only.","title":null,"url":null,"opportunity_ids":[],"actor_kind":"unknown","actor_entity_id":null}),
        )
        .await;
    assert_eq!(status, 202, "{source_only_stage}");
    let source_only_id = source_only_stage["batch_id"].as_str().unwrap();
    let (status, source_only) = client
        .call(
            "POST",
            &format!("/api/v1/capture-stages/{source_only_id}/source-only"),
            json!({"expected_revision":1}),
        )
        .await;
    assert_eq!(status, 200, "{source_only}");
    assert_eq!(source_only["status"], "source_only");
    assert_eq!(source_only["committed"].as_array().unwrap().len(), 0);
    assert_ne!(
        northline, dana,
        "a directory person can exist without linkage to the mention"
    );

    pool.close().await;
    service.shutdown().await;
    let _ = std::fs::remove_file(db);
}
