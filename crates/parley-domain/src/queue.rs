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
