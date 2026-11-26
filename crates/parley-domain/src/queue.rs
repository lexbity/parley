use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueKind {
    FollowupDue,
    ReadyToSend,
    Blocked,
    NeedsInput,
    ReviewStale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: String,
    pub opportunity_id: String,
    pub party_id: String,
    pub title: String,
    pub kind: QueueKind,
    pub due_at: Option<String>,
    pub stage_weight: i64,
    pub ammo_score: u8,
    pub blocked_reasons: Vec<String>,
    pub coach_question_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueSnapshot {
    pub event_cursor: i64,
    pub actionable: Vec<QueueItem>,
    pub blocked: Vec<QueueItem>,
    pub suggested_send_count: usize,
}

pub fn compare_items(a: &QueueItem, b: &QueueItem) -> std::cmp::Ordering {
    a.due_at
        .as_deref()
        .unwrap_or("9999")
        .cmp(b.due_at.as_deref().unwrap_or("9999"))
        .then_with(|| b.stage_weight.cmp(&a.stage_weight))
        .then_with(|| b.ammo_score.cmp(&a.ammo_score))
        .then_with(|| a.party_id.cmp(&b.party_id))
        .then_with(|| a.opportunity_id.cmp(&b.opportunity_id))
        .then_with(|| a.id.cmp(&b.id))
}

pub fn build_snapshot(
    event_cursor: i64,
    cap: usize,
    items: impl IntoIterator<Item = QueueItem>,
) -> QueueSnapshot {
    let mut actionable = Vec::new();
    let mut blocked = Vec::new();
    for item in items {
        if item.kind == QueueKind::Blocked {
            blocked.push(item)
        } else {
            actionable.push(item)
        }
    }
    actionable.sort_by(compare_items);
    blocked.sort_by(compare_items);
    let suggested_send_count = actionable
        .iter()
        .filter(|i| matches!(i.kind, QueueKind::ReadyToSend | QueueKind::FollowupDue))
        .count()
        .min(cap);
    QueueSnapshot {
        event_cursor,
        actionable,
        blocked,
        suggested_send_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, kind: QueueKind) -> QueueItem {
        QueueItem {
            id: id.into(),
            opportunity_id: "opp".into(),
            party_id: "party".into(),
            title: "Title".into(),
            kind,
            due_at: None,
            stage_weight: 0,
            ammo_score: 0,
            blocked_reasons: Vec::new(),
            coach_question_id: None,
        }
    }

    #[test]
    fn compare_items_sorts_by_due_then_descending_weight_and_ammo_then_stable_ids() {
        let mut soon = item("soon", QueueKind::ReadyToSend);
        soon.due_at = Some("2026-01-01".into());
        let mut later = item("later", QueueKind::ReadyToSend);
        later.due_at = Some("2026-02-01".into());
        let mut undated = item("undated", QueueKind::ReadyToSend);
        undated.stage_weight = 100;
        let mut heavy = item("heavy", QueueKind::ReadyToSend);
        heavy.stage_weight = 5;
        let mut armed = item("armed", QueueKind::ReadyToSend);
        armed.ammo_score = 9;
        let mut by_party = item("by-party", QueueKind::ReadyToSend);
        by_party.party_id = "aaa".into();
        let mut by_opportunity = item("by-opportunity", QueueKind::ReadyToSend);
        by_opportunity.opportunity_id = "aaa".into();

        let mut items = [
            by_opportunity,
            by_party,
            armed,
            heavy,
            undated.clone(),
            later,
            soon.clone(),
        ];
        items.sort_by(compare_items);
        assert_eq!(
            items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            [
                "soon",
                "later",
                "undated",
                "heavy",
                "armed",
                "by-party",
                "by-opportunity"
            ]
        );
        assert_eq!(compare_items(&soon, &soon), std::cmp::Ordering::Equal);
    }

    #[test]
    fn build_snapshot_partitions_blocked_items_and_caps_the_send_hint() {
        let mut sendable = item("send", QueueKind::ReadyToSend);
        sendable.due_at = Some("2026-01-01".into());
        let followup = item("followup", QueueKind::FollowupDue);
        let blocked = item("blocked", QueueKind::Blocked);
        let needs_input = item("input", QueueKind::NeedsInput);
        let stale = item("stale", QueueKind::ReviewStale);

        let snapshot = build_snapshot(
            42,
            1,
            [
                blocked.clone(),
                stale,
                needs_input,
                followup.clone(),
                sendable.clone(),
            ],
        );
        assert_eq!(snapshot.event_cursor, 42);
        assert_eq!(snapshot.blocked, [blocked]);
        assert_eq!(
            snapshot
                .actionable
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            ["send", "followup", "input", "stale"]
        );
        assert_eq!(snapshot.suggested_send_count, 1);

        let uncapped = build_snapshot(0, 10, [sendable, followup]);
        assert_eq!(uncapped.suggested_send_count, 2);
        assert!(uncapped.blocked.is_empty());
    }
}
