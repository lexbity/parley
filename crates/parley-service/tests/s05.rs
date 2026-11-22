mod common;

use common::Client;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "parley-s05-http-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[tokio::test]
async fn s05_board_and_markdown_share_opportunity_records() {
    let db = path();
    let service = parley_service::start_workspace(&db).await.unwrap();
    let mut client = Client::new(&service).await;
    let (status, dana) = client
        .call(
            "POST",
            "/api/v1/entities",
            json!({"kind":"person","name":"Dana Lee"}),
        )
        .await;
    assert_eq!(status, 201, "{dana}");
    let (status, opportunity) = client
        .call(
            "POST",
            "/api/v1/opportunities",
            json!({"title":"Onboarding checklist","participant_ids":[dana["data"]["id"]]}),
        )
        .await;
    assert_eq!(status, 201, "{opportunity}");
    let id = opportunity["id"].as_str().unwrap().to_owned();
    let (status, board) = client.get("/api/v1/board").await;
    assert_eq!(status, 200, "{board}");
    let card = board["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap();
    assert_eq!(card["title"], "Onboarding checklist");
    assert_eq!(card["status"], opportunity["status"]);

    let markdown = "# Notes\n- [ ] Répéter\n- [ ] Répéter\n```md\n- [ ] example\n```\n";
    let (status, notes) = client
        .call(
            "PUT",
            &format!("/api/v1/opportunities/{id}/notes"),
            json!({"expected_revision":1,"markdown":markdown}),
        )
        .await;
    assert_eq!(status, 200, "{notes}");
    assert_eq!(notes["revision"], 2);
    let second_task = markdown.match_indices("[ ] Répéter").nth(1).unwrap().0;
    let marker_start = second_task;
    let marker_end = marker_start + 3;
    let line = "- [ ] Répéter";
    let (status, toggled) = client
        .call(
            "PATCH",
            &format!("/api/v1/opportunities/{id}/notes/tasks"),
            json!({
                "expected_revision":2,
                "start_byte":marker_start,
                "end_byte":marker_end,
                "line_hash":digest(line.as_bytes()),
                "checked":true
            }),
        )
        .await;
    assert_eq!(status, 200, "{toggled}");
    let expected = "# Notes\n- [ ] Répéter\n- [x] Répéter\n```md\n- [ ] example\n```\n";
    assert_eq!(toggled["markdown"], expected);
    let (status, saved_notes) = client
        .get(&format!("/api/v1/opportunities/{id}/notes"))
        .await;
    assert_eq!(status, 200, "{saved_notes}");
    assert_eq!(saved_notes["markdown"], expected);
    let (status, refreshed_board) = client.get("/api/v1/board").await;
    assert_eq!(status, 200, "{refreshed_board}");
    let card = refreshed_board["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap();
    assert_eq!(card["next_task"], "Répéter");
    assert_eq!(card["status"], opportunity["status"]);

    let (status, created_list) = client
        .call(
            "POST",
            "/api/v1/board/lists",
            json!({"name":"Friday follow-ups","expected_revision":refreshed_board["board_revision"]}),
        )
        .await;
    assert_eq!(status, 201, "{created_list}");
    let custom = created_list["lists"]
        .as_array()
        .unwrap()
        .iter()
        .find(|list| list["name"] == "Friday follow-ups")
        .unwrap();
    let list_id = custom["id"].as_str().unwrap().to_owned();
    let inbox = created_list["lists"]
        .as_array()
        .unwrap()
        .iter()
        .find(|list| list["id"] == "inbox")
        .unwrap();
    let (status, moved) = client
        .call(
            "POST",
            &format!("/api/v1/board/cards/{id}/move"),
            json!({
                "destination_list_id":list_id,
                "anchor_opportunity_id":null,
                "expected_board_revision":created_list["board_revision"],
                "expected_source_revision":inbox["revision"],
                "expected_destination_revision":custom["revision"]
            }),
        )
        .await;
    assert_eq!(status, 200, "{moved}");
    let moved_card = moved["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap();
    assert_eq!(moved_card["list_id"], list_id);
    assert_eq!(moved_card["status"], opportunity["status"]);
    assert_eq!(moved_card["deadline"], opportunity["deadline"]);

    let (status, stale_move) = client
        .call(
            "POST",
            &format!("/api/v1/board/cards/{id}/move"),
            json!({
                "destination_list_id":"inbox",
                "anchor_opportunity_id":null,
                "expected_board_revision":created_list["board_revision"],
                "expected_source_revision":inbox["revision"],
                "expected_destination_revision":inbox["revision"]
            }),
        )
        .await;
    assert_eq!(
        status, 412,
        "stale board revision must preserve server order: {stale_move}"
    );
    let (status, final_board) = client.get("/api/v1/board").await;
    assert_eq!(status, 200, "{final_board}");
    let final_card = final_board["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap();
    assert_eq!(final_card["list_id"], list_id);
    assert_eq!(final_card["next_task"], "Répéter");

    service.shutdown().await;
    let _ = std::fs::remove_file(db);
}
