//! Queue sheet: phase-scoped opportunity sheet (Parley-Queue implementation
//! spec). The sheet is a filterable set of opportunity rows; each row's main
//! edit is its pack stage (phase). Phases are canonical pack_stage records,
//! not English labels. Ordinary phase edits are revision-guarded, logged as
//! events, and reversible through an explicit undo command that references
//! the original event. Lifecycle (opportunity.state), thread state and next
//! action stay separate: a phase edit never synthesizes a send, reply,
//! benefit or ladder advance.

use super::materials::workspace_error;
use super::{AppState, WorkspaceError, authenticate};
use crate::application::writer::{Failure, JobResult};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::patch,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Connection, Sqlite, Transaction};
use std::time::Instant;

/// One opportunity row from the sheet SELECT (phase columns + derived flags).
type SheetRowRecord = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
);

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/opportunities/{id}/phase", patch(set_phase))
}

#[derive(Debug, Deserialize)]
pub(super) struct SheetQuery {
    pub view: Option<String>,
    pub search: Option<String>,
    pub phase: Option<String>,
    pub sort: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    pub status: Option<String>,
    pub party_id: Option<String>,
    pub tag: Option<String>,
    pub missing_material: Option<bool>,
    pub deadline_before: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct SheetPhase {
    pub stage_id: String,
    pub label: String,
    pub class: String,
    pub kind: String,
    pub pack_id: String,
    pub pack_version: String,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct SheetParticipant {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct SheetNextAction {
    pub kind: String,
    pub label: String,
    pub reason: String,
    pub date: Option<String>,
    pub thread_id: Option<String>,
    pub recipient_id: Option<String>,
    pub recipient_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct QueueSheetRow {
    pub opportunity_id: String,
    pub title: String,
    pub lifecycle: String,
    pub close_reason: Option<String>,
    /// Active pack identity, present even for an Unmapped row so a palette
    /// prompt can freeze the pack precondition from the authoritative read.
    pub pack_id: String,
    pub pack_version: String,
    pub phase_revision: i64,
    pub phase: Option<SheetPhase>,
    pub participants: Vec<SheetParticipant>,
    pub next_action: SheetNextAction,
    pub other_action_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PhaseKind {
    pub stage_id: String,
    pub label: String,
    pub class: String,
    pub kind: String,
    pub pack_id: String,
    pub pack_version: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct QueueSheetResponse {
    pub generation: String,
    pub total: usize,
    pub rows: Vec<QueueSheetRow>,
    pub phases: Vec<PhaseKind>,
    pub cursor: Option<String>,
    pub next_cursor: Option<String>,
}

pub(super) async fn sheet_query(
    state: &AppState,
    headers: &HeaderMap,
    query: SheetQuery,
) -> Response {
    if let Err(e) = authenticate(headers, state).await {
        return (e.0, e.1).into_response();
    }
    let mut tx = match state.workspace.pool().begin().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("queue sheet DB: {e:?}");
            return workspace_error(e.into());
        }
    };
    let result = build_sheet(&mut tx, &query).await;
    let _ = tx.commit().await;
    match result {
        Ok(sheet) => {
            let mut r = Json(sheet).into_response();
            super::private_headers(&mut r, state.workspace.epoch());
            r
        }
        Err(e) => workspace_error(e),
    }
}

fn like_pattern(term: &str) -> String {
    let escaped = term
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// SQL projection of the date shown as an opportunity's next action. It
/// mirrors `next_action_for`: a dated conversing thread wins; an owner task
/// suppresses the waiting checkpoint; the earliest waiting expiry is the
/// fallback, and a null expiry sorts as null. Correlated lookups use the
/// `contact_thread(opportunity_id, recipient_id)` index.
const ACTION_DATE_SQL: &str = "(CASE WHEN o.state<>'open' THEN NULL WHEN EXISTS(SELECT 1 FROM contact_thread d1 WHERE d1.opportunity_id=o.id AND d1.state='conversing' AND d1.next_action_at IS NOT NULL) THEN (SELECT d2.next_action_at FROM contact_thread d2 WHERE d2.opportunity_id=o.id AND d2.state='conversing' AND d2.next_action_at IS NOT NULL ORDER BY d2.next_action_at,d2.id LIMIT 1) WHEN o.notes_next_task IS NOT NULL AND trim(o.notes_next_task)<>'' THEN NULL WHEN EXISTS(SELECT 1 FROM contact_thread d3 WHERE d3.opportunity_id=o.id AND d3.state='waiting' AND d3.expires_at IS NULL) THEN NULL ELSE (SELECT d4.expires_at FROM contact_thread d4 WHERE d4.opportunity_id=o.id AND d4.state='waiting' AND d4.expires_at IS NOT NULL ORDER BY d4.expires_at,d4.id LIMIT 1) END)";

/// Opaque continuation token. The client passes it back verbatim, so its
/// shape is an implementation detail. Keyset values are the last row's sort
/// keys, which makes pagination stable when rows change between pages.
#[derive(Debug)]
enum SheetCursor {
    Name {
        title: String,
        id: String,
    },
    Due {
        date: Option<String>,
        title: String,
        id: String,
    },
}

/// Hex keeps the opaque token safe in a raw query string (no percent-encoding
/// required by callers) while preserving arbitrary title bytes.
fn encode_sheet_cursor(value: Value) -> String {
    hex::encode(value.to_string().as_bytes())
}

fn parse_sheet_cursor(raw: &str) -> Option<SheetCursor> {
    let decoded = String::from_utf8(hex::decode(raw).ok()?).ok()?;
    let parts: Vec<Value> = serde_json::from_str(&decoded).ok()?;
    match parts.as_slice() {
        [Value::String(mode), Value::String(title), Value::String(id)] if mode == "n" => {
            Some(SheetCursor::Name {
                title: title.clone(),
                id: id.clone(),
            })
        }
        [
            Value::String(mode),
            date,
            Value::String(title),
            Value::String(id),
        ] if mode == "d" => {
            let date = match date {
                Value::String(value) if !value.is_empty() => Some(value.clone()),
                Value::String(_) => None,
                _ => return None,
            };
            Some(SheetCursor::Due {
                date,
                title: title.clone(),
                id: id.clone(),
            })
        }
        _ => None,
    }
}

struct SheetFilters<'a> {
    search: Option<&'a str>,
    status: Option<&'a str>,
    phase: Option<&'a str>,
    party: Option<&'a str>,
    tag: Option<&'a str>,
    missing_material: Option<bool>,
    deadline_before: Option<&'a str>,
}

fn push_sheet_filters(builder: &mut sqlx::QueryBuilder<Sqlite>, filters: &SheetFilters<'_>) {
    let SheetFilters {
        search,
        status,
        phase,
        party,
        tag,
        missing_material,
        deadline_before,
    } = *filters;
    if let Some(term) = search {
        let like = like_pattern(term);
        builder
            .push(" AND (o.title LIKE ")
            .push_bind(like.clone())
            .push(" ESCAPE '\\' OR EXISTS(SELECT 1 FROM opportunity_party x JOIN entity e ON e.id=x.party_id WHERE x.opportunity_id=o.id AND e.name LIKE ")
            .push_bind(like)
            .push(" ESCAPE '\\'))");
    }
    if let Some(status) = status {
        builder.push(" AND o.state=").push_bind(status);
    }
    match phase {
        Some("__unmapped__") => {
            builder.push(" AND o.stage_id IS NULL");
        }
        Some(stage) => {
            builder
                .push(" AND o.stage_id=")
                .push_bind(stage)
                .push(" AND o.stage_pack_id=(SELECT pack_id FROM owner_profile WHERE id=1) AND o.stage_pack_version=(SELECT pack_version FROM owner_profile WHERE id=1)");
        }
        _ => {}
    }
    if let Some(party) = party {
        builder
            .push(" AND EXISTS(SELECT 1 FROM opportunity_party x WHERE x.opportunity_id=o.id AND x.party_id=")
            .push_bind(party)
            .push(")");
    }
    if let Some(tag) = tag {
        builder
            .push(" AND EXISTS(SELECT 1 FROM opportunity_tag x WHERE x.opportunity_id=o.id AND x.tag=")
            .push_bind(tag)
            .push(")");
    }
    if let Some(missing) = missing_material {
        builder.push(if missing {
            " AND NOT EXISTS(SELECT 1 FROM material m WHERE m.opportunity_id=o.id)"
        } else {
            " AND EXISTS(SELECT 1 FROM material m WHERE m.opportunity_id=o.id)"
        });
    }
    if let Some(before) = deadline_before {
        builder
            .push(" AND EXISTS(SELECT 1 FROM contact_thread t WHERE t.opportunity_id=o.id AND t.state='waiting' AND t.expires_at <= ")
            .push_bind(before)
            .push(")");
    }
}

#[allow(clippy::type_complexity)]
async fn build_sheet(
    tx: &mut Transaction<'_, Sqlite>,
    q: &SheetQuery,
) -> Result<QueueSheetResponse, WorkspaceError> {
    let generation: i64 = sqlx::query_scalar("SELECT coalesce(max(cursor),0) FROM event")
        .fetch_one(&mut **tx)
        .await?;
    let search = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let status = q.status.as_deref().filter(|s| !s.is_empty());
    if let Some(status) = status
        && !matches!(status, "open" | "resolved" | "dormant" | "closed")
    {
        return Err(WorkspaceError::Invalid(
            "invalid lifecycle status filter".into(),
        ));
    }
    let phase = q.phase.as_deref().filter(|s| !s.is_empty());
    let party = q.party_id.as_deref().filter(|s| !s.is_empty());
    let tag = q.tag.as_deref().filter(|s| !s.is_empty());
    let deadline = q.deadline_before.as_deref().filter(|s| !s.is_empty());
    let filters = SheetFilters {
        search,
        status,
        phase,
        party,
        tag,
        missing_material: q.missing_material,
        deadline_before: deadline,
    };
    let by_name = q.sort.as_deref() == Some("name");
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let cursor = q.cursor.as_deref().and_then(parse_sheet_cursor);

    // The total is a count over the filtered set; it never depends on the page
    // or the sort order. Every user value stays bound through QueryBuilder.
    let mut count =
        sqlx::QueryBuilder::<Sqlite>::new("SELECT count(*) FROM opportunity o WHERE 1=1");
    push_sheet_filters(&mut count, &filters);
    let total: i64 = count.build_query_scalar().fetch_one(&mut **tx).await?;

    // Only the requested page is fetched. ORDER BY is performed by SQLite over
    // the same keys the previous Rust sort used, so a single page costs the
    // same at 10,000 rows as at 10.
    // The action-date expression involves correlated thread lookups. Compute
    // it once per filtered row in a materialized CTE so the sorter compares a
    // cached scalar instead of re-running the subqueries per comparison.
    let mut page = sqlx::QueryBuilder::<Sqlite>::new("WITH filtered AS MATERIALIZED (SELECT o.*,");
    if by_name {
        page.push("NULL");
    } else {
        page.push(ACTION_DATE_SQL);
    }
    page.push(" AS action_date FROM opportunity o WHERE 1=1");
    push_sheet_filters(&mut page, &filters);
    page.push(") SELECT f.id,f.title,f.state,f.close_reason,f.stage_id,f.stage_pack_id,f.stage_pack_version,f.stage_class,f.phase_revision,f.last_phase_event_id,f.notes_next_task,ps.label,ps.class,ps.kind,NOT EXISTS(SELECT 1 FROM material m WHERE m.opportunity_id=f.id) FROM filtered f LEFT JOIN pack_stage ps ON ps.pack_id=f.stage_pack_id AND ps.pack_version=f.stage_pack_version AND ps.stage_id=f.stage_id WHERE 1=1");
    match cursor {
        Some(SheetCursor::Name { title, id }) if by_name => {
            page.push(" AND (lower(f.title)>")
                .push_bind(title.clone())
                .push(" OR (lower(f.title)=")
                .push_bind(title)
                .push(" AND f.id>")
                .push_bind(id)
                .push("))");
        }
        Some(SheetCursor::Due {
            date: Some(date),
            title,
            id,
        }) if !by_name => {
            page.push(" AND (f.action_date>")
                .push_bind(date.clone())
                .push(" OR (f.action_date=")
                .push_bind(date)
                .push(" AND (lower(f.title)>")
                .push_bind(title.clone())
                .push(" OR (lower(f.title)=")
                .push_bind(title)
                .push(" AND f.id>")
                .push_bind(id)
                .push("))) OR f.action_date IS NULL)");
        }
        Some(SheetCursor::Due {
            date: None,
            title,
            id,
        }) if !by_name => {
            page.push(" AND f.action_date IS NULL AND (lower(f.title)>")
                .push_bind(title.clone())
                .push(" OR (lower(f.title)=")
                .push_bind(title)
                .push(" AND f.id>")
                .push_bind(id)
                .push("))");
        }
        // A cursor from a different sort is ignored rather than misapplied.
        _ => {}
    }
    if by_name {
        page.push(" ORDER BY lower(f.title),f.id");
    } else {
        page.push(" ORDER BY f.action_date NULLS LAST,lower(f.title),f.id");
    }
    page.push(" LIMIT ").push_bind(limit + 1);
    let mut rows: Vec<SheetRowRecord> = page.build_query_as().fetch_all(&mut **tx).await?;
    let has_next = rows.len() > limit as usize;
    if has_next {
        rows.truncate(limit as usize);
    }

    let ids: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
    let participants = load_participants(tx, &ids).await?;
    let threads = load_threads(tx, &ids).await?;
    let phases = load_phase_kinds(tx).await?;
    let (active_pack_id, active_pack_version): (String, String) =
        sqlx::query_as("SELECT pack_id,pack_version FROM owner_profile WHERE id=1")
            .fetch_one(&mut **tx)
            .await?;

    let rows = rows
        .into_iter()
        .map(
            |(
                oid,
                title,
                state,
                close_reason,
                stage_id,
                pack_id,
                pack_version,
                stage_class,
                phase_revision,
                _last_event,
                next_task,
                label,
                class,
                kind,
                missing_material,
            )| {
                let phase = match (
                    stage_id,
                    pack_id,
                    pack_version,
                    stage_class,
                    label,
                    class,
                    kind,
                ) {
                    (
                        Some(sid),
                        Some(pid),
                        Some(pver),
                        Some(_sclass),
                        Some(lbl),
                        Some(cls),
                        Some(knd),
                    ) => Some(SheetPhase {
                        stage_id: sid,
                        label: lbl,
                        class: cls,
                        kind: knd,
                        pack_id: pid,
                        pack_version: pver,
                        revision: phase_revision,
                    }),
                    _ => None,
                };
                let (next_action, other_action_count) = next_action_for(
                    &state,
                    close_reason.as_deref(),
                    next_task.as_deref(),
                    missing_material,
                    threads.get(&oid).map(Vec::as_slice).unwrap_or_default(),
                );
                QueueSheetRow {
                    opportunity_id: oid.clone(),
                    title,
                    lifecycle: state,
                    close_reason,
                    pack_id: active_pack_id.clone(),
                    pack_version: active_pack_version.clone(),
                    phase_revision,
                    phase,
                    participants: participants.get(&oid).cloned().unwrap_or_default(),
                    next_action,
                    other_action_count,
                }
            },
        )
        .collect::<Vec<_>>();

    let next_cursor = if has_next {
        let last = rows.last().expect("a full page has a last row");
        if by_name {
            Some(encode_sheet_cursor(json!([
                "n",
                last.title.to_lowercase(),
                last.opportunity_id
            ])))
        } else {
            Some(encode_sheet_cursor(json!([
                "d",
                last.next_action.date.clone().unwrap_or_default(),
                last.title.to_lowercase(),
                last.opportunity_id
            ])))
        }
    } else {
        None
    };

    Ok(QueueSheetResponse {
        generation: generation.to_string(),
        total: total as usize,
        rows,
        phases,
        cursor: q.cursor.clone(),
        next_cursor,
    })
}

#[allow(clippy::type_complexity)]
async fn load_participants(
    tx: &mut Transaction<'_, Sqlite>,
    ids: &[String],
) -> Result<std::collections::HashMap<String, Vec<SheetParticipant>>, WorkspaceError> {
    let mut out: std::collections::HashMap<String, Vec<SheetParticipant>> =
        std::collections::HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT op.opportunity_id,e.id,e.name,e.kind,op.role FROM opportunity_party op JOIN entity e ON e.id=op.party_id WHERE op.opportunity_id IN (SELECT value FROM json_each(?)) ORDER BY op.opportunity_id,CASE op.role WHEN 'primary' THEN 0 ELSE 1 END,e.id",
    )
    .bind(serde_json::to_string(ids).unwrap_or_default())
    .fetch_all(&mut **tx)
    .await?;
    for (oid, id, name, kind, role) in rows {
        out.entry(oid).or_default().push(SheetParticipant {
            id,
            name,
            kind,
            role,
        });
    }
    Ok(out)
}

#[allow(clippy::type_complexity)]
async fn load_threads(
    tx: &mut Transaction<'_, Sqlite>,
    ids: &[String],
) -> Result<std::collections::HashMap<String, Vec<ThreadContext>>, WorkspaceError> {
    let mut out: std::collections::HashMap<String, Vec<ThreadContext>> =
        std::collections::HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let rows: Vec<(String, String, String, String, String, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT ct.opportunity_id,ct.id,ct.recipient_id,e.name,ct.state,ct.expires_at,ct.next_action_at FROM contact_thread ct JOIN entity e ON e.id=ct.recipient_id WHERE ct.opportunity_id IN (SELECT value FROM json_each(?)) AND ct.state IN ('waiting','conversing') ORDER BY ct.opportunity_id,ct.expires_at,ct.id",
        )
        .bind(serde_json::to_string(ids).unwrap_or_default())
        .fetch_all(&mut **tx)
        .await?;
    for (oid, id, recipient_id, recipient_name, state, expires_at, next_action_at) in rows {
        out.entry(oid).or_default().push(ThreadContext {
            id,
            recipient_id,
            recipient_name,
            state,
            expires_at,
            next_action_at,
        });
    }
    Ok(out)
}

#[derive(Debug, Clone)]
struct ThreadContext {
    id: String,
    recipient_id: String,
    recipient_name: String,
    state: String,
    expires_at: Option<String>,
    next_action_at: Option<String>,
}

async fn load_phase_kinds(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<Vec<PhaseKind>, WorkspaceError> {
    let rows: Vec<(String, String, String, String, String, String, i64)> = sqlx::query_as(
        "SELECT ps.stage_id,ps.label,ps.class,ps.kind,ps.pack_id,ps.pack_version,count(o.id) FROM pack_stage ps LEFT JOIN opportunity o ON o.stage_id=ps.stage_id AND o.stage_pack_id=ps.pack_id AND o.stage_pack_version=ps.pack_version WHERE ps.pack_id=(SELECT pack_id FROM owner_profile WHERE id=1) AND ps.pack_version=(SELECT pack_version FROM owner_profile WHERE id=1) GROUP BY ps.id ORDER BY ps.ordinal,ps.stage_id",
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut kinds: Vec<PhaseKind> = rows
        .into_iter()
        .map(
            |(stage_id, label, class, kind, pack_id, pack_version, count)| PhaseKind {
                stage_id,
                label,
                class,
                kind,
                pack_id,
                pack_version,
                count: count as usize,
            },
        )
        .collect();
    let unmapped: i64 =
        sqlx::query_scalar("SELECT count(*) FROM opportunity WHERE stage_id IS NULL")
            .fetch_one(&mut **tx)
            .await?;
    if unmapped > 0 {
        kinds.push(PhaseKind {
            stage_id: "__unmapped__".to_string(),
            label: "Unmapped".to_string(),
            class: "unmapped".to_string(),
            kind: "planning".to_string(),
            pack_id: String::new(),
            pack_version: String::new(),
            count: unmapped as usize,
        });
    }
    Ok(kinds)
}

/// Next-action derivation (spec §3). The backend computes the display from
/// authoritative opportunity, task, gate and thread records; the client never
/// guesses from phase text.
fn next_action_for(
    state: &str,
    close_reason: Option<&str>,
    next_task: Option<&str>,
    missing_material: bool,
    threads: &[ThreadContext],
) -> (SheetNextAction, usize) {
    if state != "open" {
        let reason = close_reason
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Opportunity is {state}."));
        return (
            SheetNextAction {
                kind: "terminal".to_string(),
                label: "No open action".to_string(),
                reason,
                date: None,
                thread_id: None,
                recipient_id: None,
                recipient_name: None,
            },
            0,
        );
    }

    // Actionable candidates.
    let mut dated: Vec<(&ThreadContext, &str)> = Vec::new();
    let mut undated: Vec<SheetNextAction> = Vec::new();
    for thread in threads {
        if thread.state == "conversing"
            && let Some(when) = thread.next_action_at.as_deref()
        {
            dated.push((thread, when));
        }
    }
    if let Some(task) = next_task
        && !task.trim().is_empty()
    {
        undated.push(SheetNextAction {
            kind: "task".to_string(),
            label: task.trim().to_string(),
            reason: "Owner-planned next task.".to_string(),
            date: None,
            thread_id: None,
            recipient_id: None,
            recipient_name: None,
        });
    }

    // Earliest dated actionable candidate wins; ties break on stable thread id.
    // A blocker (missing material) replaces its dependent prohibited send with
    // a concrete remedy instead of offering the send.
    dated.sort_by(|(a, ad), (b, bd)| ad.cmp(bd).then_with(|| a.id.cmp(&b.id)));
    if let Some((thread, when)) = dated.first().copied() {
        let others = dated.len() - 1 + undated.len();
        if missing_material {
            return (
                SheetNextAction {
                    kind: "blocked".to_string(),
                    label: "Gather material before sending".to_string(),
                    reason: "A follow-up is due, but no material is linked to this opportunity yet; a send is not possible until one exists.".to_string(),
                    date: Some(when.to_string()),
                    thread_id: Some(thread.id.clone()),
                    recipient_id: Some(thread.recipient_id.clone()),
                    recipient_name: Some(thread.recipient_name.clone()),
                },
                others,
            );
        }
        return (
            SheetNextAction {
                kind: "touch".to_string(),
                label: format!("Follow up with {}", thread.recipient_name),
                reason: format!(
                    "Cadence-based follow-up to {} is due.",
                    thread.recipient_name
                ),
                date: Some(when.to_string()),
                thread_id: Some(thread.id.clone()),
                recipient_id: Some(thread.recipient_id.clone()),
                recipient_name: Some(thread.recipient_name.clone()),
            },
            others,
        );
    }

    // Undated actionable candidates (owner tasks) before waiting.
    if let Some(first) = undated.into_iter().next() {
        let others = dated.len() + usize::from(missing_material);
        return (first, others);
    }

    // Nothing actionable but live threads are waiting: earliest waiting checkpoint.
    let waiting: Vec<&ThreadContext> = threads.iter().filter(|t| t.state == "waiting").collect();
    if let Some(first) = waiting.iter().min_by(|a, b| {
        a.expires_at
            .as_deref()
            .cmp(&b.expires_at.as_deref())
            .then_with(|| a.id.cmp(&b.id))
    }) {
        return (
            SheetNextAction {
                kind: "wait".to_string(),
                label: first.expires_at.as_deref().map_or_else(
                    || format!("Waiting on {}", first.recipient_name),
                    |when| format!("Waiting until {when}"),
                ),
                reason: format!("{}'s reply window is still open.", first.recipient_name),
                date: first.expires_at.clone(),
                thread_id: Some(first.id.clone()),
                recipient_id: Some(first.recipient_id.clone()),
                recipient_name: Some(first.recipient_name.clone()),
            },
            waiting.len() - 1,
        );
    }

    (
        SheetNextAction {
            kind: "choose".to_string(),
            label: "Choose a next action".to_string(),
            reason: "No work or wait is known for this opportunity.".to_string(),
            date: None,
            thread_id: None,
            recipient_id: None,
            recipient_name: None,
        },
        0,
    )
}

// ---------------------------------------------------------------------------
// Phase command
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetPhaseInput {
    target_stage_id: String,
    pack_version: String,
    expected_phase_revision: i64,
    original_event_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PhaseResult {
    row: QueueSheetRow,
    event_id: String,
    phase_revision: i64,
    reverted: bool,
}

/// Finish a REST phase transaction: commit under the deadline or fail the
/// job. Terminal legacy errors commit an empty transaction (nothing was
/// written) so their body reaches the transport.
async fn finish_rest_tx(
    tx: Transaction<'_, Sqlite>,
    deadline: Instant,
    status: u16,
    body: Value,
) -> JobResult {
    let transport = json!({
        "_parley_rest_status": status,
        "_parley_rest_body": body,
    });
    match crate::application::writer::commit_tx(tx, deadline, "").await {
        Ok(()) => JobResult {
            commit: true,
            body: transport,
            failure: None,
        },
        Err(failure) => JobResult {
            commit: false,
            body: Value::Null,
            failure: Some(failure),
        },
    }
}

fn rest_error(_status: u16, code: &'static str, message: impl Into<String>) -> Value {
    json!({"error": {"code": code, "message": message.into(), "details": []}})
}

/// Legacy REST error mapping for the shared phase operation (published
/// statuses preserved at the adapter).
fn legacy_phase_error(error: crate::application::phase::PhaseError) -> (u16, &'static str, String) {
    use crate::application::phase::PhaseError;
    match error {
        PhaseError::NotFound => (404, "NOT_FOUND", "Opportunity does not exist".into()),
        PhaseError::InvalidStage => (
            422,
            "INVALID_STAGE",
            "Stage is not valid for the selected pack".into(),
        ),
        PhaseError::TerminalStage => (
            422,
            "REQUIRES_LIFECYCLE_CONFIRM",
            "Closing or resolving an opportunity requires the explicit lifecycle flow with a recorded reason; an ordinary phase edit cannot close it."
                .into(),
        ),
        PhaseError::LifecycleBlocked => (
            409,
            "LIFECYCLE_BLOCKS_PHASE",
            "This opportunity is not open; reopen or resolve it through its explicit workflow before changing its phase."
                .into(),
        ),
        PhaseError::StaleRevision => (
            412,
            "STALE_PHASE_REVISION",
            "Expected phase revision is stale; reload the current phase.".into(),
        ),
        PhaseError::PackMismatch => (
            412,
            "STALE_PHASE_REVISION",
            "Pack identity does not match the opportunity.".into(),
        ),
        PhaseError::UndoInvalidReference => (
            422,
            "INVALID_UNDO_REFERENCE",
            "The referenced transition is not an ordinary manual phase change.".into(),
        ),
        PhaseError::UndoNotLatest => (
            412,
            "PHASE_CHANGED_ELSEWHERE",
            "Undo cannot apply: a newer phase change already replaced the referenced transition. Review the current phase."
                .into(),
        ),
        PhaseError::UndoTargetMismatch => (
            422,
            "INVALID_UNDO_TARGET",
            "Undo must target the stage recorded before the original transition.".into(),
        ),
    }
}

async fn set_phase(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<SetPhaseInput>,
) -> Response {
    if let Err(e) = authenticate(&headers, &state).await {
        return (e.0, e.1).into_response();
    }
    let key = match headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 200)
    {
        Some(v) => v.to_owned(),
        None => {
            return super::error(
                StatusCode::BAD_REQUEST,
                "IDEMPOTENCY_KEY_REQUIRED",
                "Idempotency-Key is required",
            );
        }
    };
    let body = json!({
        "target_stage_id": input.target_stage_id,
        "pack_version": input.pack_version,
        "expected_phase_revision": input.expected_phase_revision,
        "original_event_id": input.original_event_id,
    });
    let canonical = crate::commands::ledger::rest_canonical_digest(
        "PATCH",
        &format!("/opportunities/{id}/phase"),
        &body,
    );
    let action = match input.original_event_id.clone() {
        Some(original_event_id) => {
            crate::application::phase::PhaseAction::Undo { original_event_id }
        }
        None => crate::application::phase::PhaseAction::Set {
            target_stage_id: input.target_stage_id.clone(),
        },
    };
    let epoch = state.workspace.epoch().to_string();
    let generation = state.workspace.generation_token().await;
    let opportunity_id = id.clone();
    let pack_version = input.pack_version.clone();
    let input_target_stage = input.target_stage_id.clone();
    let expected_revision = input.expected_phase_revision;
    let operation_digest = crate::digest(format!("rest-phase\0{key}\0{opportunity_id}").as_bytes());
    let job = crate::application::writer::WriterJob {
        deadline: Instant::now() + crate::application::writer::EXECUTION_DEADLINE,
        generation,
        run: Box::new(
            move |connection: &mut sqlx::sqlite::SqliteConnection, deadline: Instant| {
                let key = key.clone();
                let canonical = canonical.clone();
                let epoch = epoch.clone();
                let opportunity_id = opportunity_id.clone();
                let pack_version = pack_version.clone();
                let action = action.clone();
                let operation_digest = operation_digest.clone();
                let input_target_stage = input_target_stage.clone();
                Box::pin(async move {
                    let mut tx = match connection.begin_with("BEGIN IMMEDIATE").await {
                        Ok(tx) => tx,
                        Err(_) => {
                            return JobResult {
                                commit: false,
                                body: Value::Null,
                                failure: Some(Failure::Unavailable),
                            };
                        }
                    };
                    // Historical receipts keep their published replay envelope.
                    match crate::commands::ledger::rest_receipt_lookup(&mut tx, &key, &canonical)
                        .await
                    {
                        Ok(Some(replay)) => {
                            let replayed =
                                json!({"_replay_status": replay.status, "result": replay.body});
                            return finish_rest_tx(tx, deadline, 200, replayed).await;
                        }
                        Ok(None) => {}
                        Err(_) => {
                            // Same key with a different canonical intent is a
                            // terminal conflict; never re-execute the effect.
                            return finish_rest_tx(
                                tx,
                                deadline,
                                409,
                                rest_error(
                                    409,
                                    "IDEMPOTENCY_CONFLICT",
                                    "Idempotency key was used for a different request",
                                ),
                            )
                            .await;
                        }
                    }
                    let current_epoch: Option<String> =
                        sqlx::query_scalar("SELECT epoch FROM workspace_meta WHERE singleton=1")
                            .fetch_optional(&mut *tx)
                            .await
                            .ok()
                            .flatten();
                    if current_epoch.as_deref() != Some(&epoch) {
                        return finish_rest_tx(
                            tx,
                            deadline,
                            409,
                            rest_error(
                                409,
                                "WORKSPACE_REPLACED",
                                "Workspace changed; reconnect before continuing",
                            ),
                        )
                        .await;
                    }
                    let pack_id: Option<String> =
                        sqlx::query_scalar("SELECT pack_id FROM owner_profile WHERE id=1")
                            .fetch_optional(&mut *tx)
                            .await
                            .ok()
                            .flatten();
                    let Some(pack_id) = pack_id else {
                        return JobResult {
                            commit: false,
                            body: Value::Null,
                            failure: Some(Failure::Unavailable),
                        };
                    };
                    if sqlx::query("SAVEPOINT ucr_rest_operation")
                        .execute(&mut *tx)
                        .await
                        .is_err()
                    {
                        return JobResult {
                            commit: false,
                            body: Value::Null,
                            failure: Some(Failure::Unavailable),
                        };
                    }
                    let expected_target_stage_id = match &action {
                        crate::application::phase::PhaseAction::Undo { .. } => {
                            Some(input_target_stage.clone())
                        }
                        crate::application::phase::PhaseAction::Set { .. } => None,
                    };
                    let phase_input = crate::application::phase::PhaseInput {
                        opportunity_id: opportunity_id.clone(),
                        expected_phase_revision: expected_revision,
                        expected_pack_id: pack_id,
                        expected_pack_version: pack_version,
                        action,
                        operation_key_digest: operation_digest,
                        expected_target_stage_id,
                    };
                    match crate::application::phase::apply_phase(&mut tx, &phase_input).await {
                        Ok(outcome) => {
                            if sqlx::query("RELEASE SAVEPOINT ucr_rest_operation")
                                .execute(&mut *tx)
                                .await
                                .is_err()
                            {
                                return JobResult {
                                    commit: false,
                                    body: Value::Null,
                                    failure: Some(Failure::Unavailable),
                                };
                            }
                            let (event_id_from_op, phase_revision, reverted) = match outcome {
                                crate::application::phase::PhaseOutcome::Changed(result) => {
                                    (result.event_id, result.phase_revision, result.reverted)
                                }
                                crate::application::phase::PhaseOutcome::NoOp(result) => {
                                    (None, result.phase_revision, result.reverted)
                                }
                            };
                            // The published narrow no-op acknowledgement keeps
                            // the row's recorded event id (transport choice).
                            let event_id = match event_id_from_op {
                                Some(event_id) => event_id,
                                None => sqlx::query_scalar(
                                    "SELECT COALESCE(last_phase_event_id,'') FROM opportunity WHERE id=?",
                                )
                                .bind(&opportunity_id)
                                .fetch_one(&mut *tx)
                                .await
                                .unwrap_or_default(),
                            };
                            let row = match row_json(&mut tx, &opportunity_id).await {
                                Ok(row) => row,
                                Err(_) => {
                                    return JobResult {
                                        commit: false,
                                        body: Value::Null,
                                        failure: Some(Failure::Unavailable),
                                    };
                                }
                            };
                            let result = PhaseResult {
                                row,
                                event_id,
                                phase_revision,
                                reverted,
                            };
                            let result_value = serde_json::to_value(&result).unwrap_or_default();
                            if crate::commands::ledger::rest_receipt_store(
                                &mut tx,
                                &key,
                                &canonical,
                                200,
                                &result_value,
                            )
                            .await
                            .is_err()
                            {
                                return JobResult {
                                    commit: false,
                                    body: Value::Null,
                                    failure: Some(Failure::Unavailable),
                                };
                            }
                            finish_rest_tx(tx, deadline, 200, result_value).await
                        }
                        Err(error) => {
                            // Roll the operation back; the outer transaction
                            // still commits the published terminal response.
                            let _ = sqlx::query("ROLLBACK TO SAVEPOINT ucr_rest_operation")
                                .execute(&mut *tx)
                                .await;
                            let _ = sqlx::query("RELEASE SAVEPOINT ucr_rest_operation")
                                .execute(&mut *tx)
                                .await;
                            let (status, code, message) = legacy_phase_error(error);
                            finish_rest_tx(tx, deadline, status, rest_error(status, code, message))
                                .await
                        }
                    }
                })
            },
        ),
        reply: tokio::sync::oneshot::channel().0,
    };
    match state.writer.run(job).await {
        crate::application::writer::WriteOutcome::Committed(body) => {
            let status = body
                .get("_parley_rest_status")
                .and_then(|v| v.as_u64())
                .map(|v| v as u16)
                .unwrap_or(200);
            let payload = body.get("_parley_rest_body").cloned().unwrap_or(body);
            let mut response = Json(payload).into_response();
            *response.status_mut() =
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            super::private_headers(&mut response, state.workspace.epoch());
            response
        }
        crate::application::writer::WriteOutcome::Rejected(failure) => match failure {
            Failure::Timeout => super::error(
                StatusCode::GATEWAY_TIMEOUT,
                "TIMEOUT",
                "The command timed out",
            ),
            _ => {
                let mut response = super::error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "WRITER_BUSY",
                    "The workspace is busy. Retry the same operation.",
                );
                response.headers_mut().insert(
                    axum::http::header::RETRY_AFTER,
                    axum::http::HeaderValue::from_static("1"),
                );
                response
            }
        },
    }
}

/// Canonical single-row projection shared by list and command responses.
async fn row_json(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<QueueSheetRow, WorkspaceError> {
    let row: Option<SheetRowRecord> = sqlx::query_as(
        "SELECT o.id,o.title,o.state,o.close_reason,o.stage_id,o.stage_pack_id,o.stage_pack_version,o.stage_class,o.phase_revision,o.last_phase_event_id,o.notes_next_task,ps.label,ps.class,ps.kind,NOT EXISTS(SELECT 1 FROM material m WHERE m.opportunity_id=o.id) FROM opportunity o LEFT JOIN pack_stage ps ON ps.pack_id=o.stage_pack_id AND ps.pack_version=o.stage_pack_version AND ps.stage_id=o.stage_id WHERE o.id=?",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((
        oid,
        title,
        state,
        close_reason,
        stage_id,
        pack_id,
        pack_version,
        stage_class,
        phase_revision,
        _last_event,
        next_task,
        label,
        class,
        kind,
        missing_material,
    )) = row
    else {
        return Err(WorkspaceError::NotFound);
    };
    let (active_pack_id, active_pack_version): (String, String) =
        sqlx::query_as("SELECT pack_id,pack_version FROM owner_profile WHERE id=1")
            .fetch_one(&mut **tx)
            .await?;
    let phase = match (
        stage_id,
        pack_id,
        pack_version,
        stage_class,
        label,
        class,
        kind,
    ) {
        (Some(sid), Some(pid), Some(pver), Some(_sclass), Some(lbl), Some(cls), Some(knd)) => {
            Some(SheetPhase {
                stage_id: sid,
                label: lbl,
                class: cls,
                kind: knd,
                pack_id: pid,
                pack_version: pver,
                revision: phase_revision,
            })
        }
        _ => None,
    };
    let participants = load_participants(tx, &[id.to_string()])
        .await?
        .remove(&oid)
        .unwrap_or_default();
    let threads = load_threads(tx, &[id.to_string()])
        .await?
        .remove(&oid)
        .unwrap_or_default();
    let (next_action, other_action_count) = next_action_for(
        &state,
        close_reason.as_deref(),
        next_task.as_deref(),
        missing_material,
        &threads,
    );
    Ok(QueueSheetRow {
        opportunity_id: oid,
        title,
        lifecycle: state,
        close_reason,
        pack_id: active_pack_id,
        pack_version: active_pack_version,
        phase_revision,
        phase,
        participants,
        next_action,
        other_action_count,
    })
}
