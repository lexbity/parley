//! Queue sheet: phase-scoped opportunity sheet (Parley-Queue implementation
//! spec). The sheet is a filterable set of opportunity rows; each row's main
//! edit is its pack stage (phase). Phases are canonical pack_stage records,
//! not English labels. Ordinary phase edits are revision-guarded, logged as
//! events, and reversible through an explicit undo command that references
//! the original event. Lifecycle (opportunity.state), thread state and next
//! action stay separate: a phase edit never synthesizes a send, reply,
//! benefit or ladder advance.

use super::materials::workspace_error;
use super::{AppState, WorkspaceError, authenticate, command_error, new_id, now_utc};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::patch,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

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

/// Current phase state columns for the phase command.
type PhaseStateRecord = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
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
            return command_error(e.into());
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

    // Dynamic WHERE clauses are assembled with sqlx QueryBuilder, matching the
    // established filter pattern in materials.rs; every user value is bound.
    let mut builder = sqlx::QueryBuilder::<Sqlite>::new(
        "SELECT o.id,o.title,o.state,o.close_reason,o.stage_id,o.stage_pack_id,o.stage_pack_version,o.stage_class,o.phase_revision,o.last_phase_event_id,o.notes_next_task,ps.label,ps.class,ps.kind,NOT EXISTS(SELECT 1 FROM material m WHERE m.opportunity_id=o.id) FROM opportunity o LEFT JOIN pack_stage ps ON ps.pack_id=o.stage_pack_id AND ps.pack_version=o.stage_pack_version AND ps.stage_id=o.stage_id WHERE 1=1",
    );
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
    if let Some(missing) = q.missing_material {
        builder.push(if missing {
            " AND NOT EXISTS(SELECT 1 FROM material m WHERE m.opportunity_id=o.id)"
        } else {
            " AND EXISTS(SELECT 1 FROM material m WHERE m.opportunity_id=o.id)"
        });
    }
    if let Some(before) = q.deadline_before.as_deref().filter(|s| !s.is_empty()) {
        builder
            .push(" AND EXISTS(SELECT 1 FROM contact_thread t WHERE t.opportunity_id=o.id AND t.state='waiting' AND t.expires_at <= ")
            .push_bind(before)
            .push(")");
    }
    builder.push(" ORDER BY o.id");
    let rows: Vec<SheetRowRecord> = builder.build_query_as().fetch_all(&mut **tx).await?;

    let ids: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
    let participants = load_participants(tx, &ids).await?;
    let threads = load_threads(tx, &ids).await?;
    let phases = load_phase_kinds(tx).await?;

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
                    phase,
                    participants: participants.get(&oid).cloned().unwrap_or_default(),
                    next_action,
                    other_action_count,
                }
            },
        )
        .collect::<Vec<_>>();

    let (rows, total) = sort_and_page(rows, q);

    let offset = q
        .cursor
        .as_deref()
        .and_then(|c| c.parse::<usize>().ok())
        .unwrap_or(0);
    let next_cursor = if offset + rows.len() < total {
        Some((offset + rows.len()).to_string())
    } else {
        None
    };

    Ok(QueueSheetResponse {
        generation: generation.to_string(),
        total,
        rows,
        phases,
        cursor: q.cursor.clone(),
        next_cursor,
    })
}

fn sort_and_page(mut rows: Vec<QueueSheetRow>, q: &SheetQuery) -> (Vec<QueueSheetRow>, usize) {
    let total = rows.len();
    let by_name = q.sort.as_deref() == Some("name");
    rows.sort_by(|a, b| {
        if by_name {
            a.title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| a.opportunity_id.cmp(&b.opportunity_id))
        } else {
            // Action date ascending, nulls last, then normalized title, then id.
            let date_order = match (&a.next_action.date, &b.next_action.date) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (Some(x), Some(y)) => x.cmp(y),
            };
            date_order
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
                .then_with(|| a.opportunity_id.cmp(&b.opportunity_id))
        }
    });
    let offset = q
        .cursor
        .as_deref()
        .and_then(|c| c.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = q.limit.unwrap_or(50).clamp(1, 200) as usize;
    if offset >= total {
        (Vec::new(), total)
    } else {
        (rows.into_iter().skip(offset).take(limit).collect(), total)
    }
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

fn canonical_digest(method: &str, target: &str, body: &impl Serialize) -> String {
    let body = serde_json::to_vec(body).unwrap_or_default();
    super::digest(&[method.as_bytes(), b"\n", target.as_bytes(), b"\n", &body].concat())
}

struct Replay {
    status: i64,
    body: Value,
}

async fn receipt_lookup(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    canonical: &str,
) -> Result<Option<Replay>, WorkspaceError> {
    let row: Option<(String, i64, String)> = sqlx::query_as(
        "SELECT canonical_digest,status_code,result_json FROM idempotency_receipt WHERE owner_id=1 AND epoch=(SELECT epoch FROM workspace_meta WHERE singleton=1) AND key=?",
    )
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((stored, status, body)) = row else {
        return Ok(None);
    };
    if stored != canonical {
        return Err(WorkspaceError::IdempotencyConflict);
    }
    let body: Value =
        serde_json::from_str(&body).map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
    Ok(Some(Replay { status, body }))
}

async fn receipt_store(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    canonical: &str,
    status: i64,
    result: &Value,
) -> Result<(), WorkspaceError> {
    sqlx::query(
        "INSERT INTO idempotency_receipt(owner_id,epoch,key,canonical_digest,status_code,result_json,last_event_cursor,created_at) VALUES(1,(SELECT epoch FROM workspace_meta WHERE singleton=1),?,?,?,?,(SELECT COALESCE(MAX(cursor),0) FROM event),?)",
    )
    .bind(key)
    .bind(canonical)
    .bind(status)
    .bind(result.to_string())
    .bind(now_utc())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn write_event(
    tx: &mut Transaction<'_, Sqlite>,
    entity_type: &str,
    id: &str,
    revision: i64,
    kind: &str,
    payload: Value,
) -> Result<String, WorkspaceError> {
    let event_id = new_id(id);
    sqlx::query(
        "INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,?,?,?,?,?)",
    )
    .bind(&event_id)
    .bind(now_utc())
    .bind(entity_type)
    .bind(id)
    .bind(revision)
    .bind(kind)
    .bind(payload.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(event_id)
}

#[derive(Debug, Clone, Serialize)]
struct PhaseResult {
    row: QueueSheetRow,
    event_id: String,
    phase_revision: i64,
    reverted: bool,
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
    let canonical = canonical_digest("PATCH", &format!("/opportunities/{id}/phase"), &body);
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("phase command DB: {e:?}");
            return command_error(e.into());
        }
    };
    if let Some(replay) = match receipt_lookup(&mut tx, &key, &canonical).await {
        Ok(v) => v,
        Err(e) => return workspace_error(e),
    } {
        let _ = tx.commit().await;
        return Json(json!({
            "_replay_status": replay.status,
            "result": replay.body,
        }))
        .into_response();
    }

    // Load current opportunity phase state.
    let row: Option<PhaseStateRecord> = match sqlx::query_as(
        "SELECT state,stage_id,stage_pack_id,stage_pack_version,stage_class,phase_revision,last_phase_event_id FROM opportunity WHERE id=?",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(v) => v,
        Err(e) => return command_error(e.into()),
    };
    let Some((
        lifecycle,
        current_stage,
        _current_pack,
        _current_pack_version,
        current_class,
        phase_revision,
        last_phase_event_id,
    )) = row
    else {
        return workspace_error(WorkspaceError::NotFound);
    };

    // Resolve the target stage in the active pack.
    let stage: Option<(String, String, String, String)> = match sqlx::query_as(
        "SELECT stage_id,label,class,kind FROM pack_stage WHERE pack_id=(SELECT pack_id FROM owner_profile WHERE id=1) AND pack_version=? AND stage_id=?",
    )
    .bind(&input.pack_version)
    .bind(&input.target_stage_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(v) => v,
        Err(e) => return command_error(e.into()),
    };
    let Some((target_stage_id, target_label, target_class, target_kind)) = stage else {
        let _ = tx.commit().await;
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_STAGE",
            format!(
                "Stage '{}' is not valid for pack version {}.",
                input.target_stage_id, input.pack_version
            ),
        )
        .into_response();
    };

    // Undo path: reference the original manual transition.
    let mut reverted = false;
    let mut reversal_of: Option<String> = None;
    let mut from_stage_id = current_stage.clone();
    let mut from_class = current_class.clone();
    if let Some(original_event_id) = &input.original_event_id {
        reverted = true;
        reversal_of = Some(original_event_id.clone());
        if last_phase_event_id.as_deref() != Some(original_event_id.as_str()) {
            let _ = tx.commit().await;
            return super::error(
                StatusCode::PRECONDITION_FAILED,
                "PHASE_CHANGED_ELSEWHERE",
                "Undo cannot apply: a newer phase change already replaced the referenced transition. Review the current phase.",
            )
            .into_response();
        }
        let event: Option<(String, String, String)> = match sqlx::query_as(
            "SELECT entity_type,event_type,payload_json FROM event WHERE event_id=?",
        )
        .bind(original_event_id)
        .fetch_optional(&mut *tx)
        .await
        {
            Ok(v) => v,
            Err(e) => return command_error(e.into()),
        };
        let Some((entity_type, event_type, payload_json)) = event else {
            let _ = tx.commit().await;
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_UNDO_REFERENCE",
                "The referenced transition event does not exist.",
            )
            .into_response();
        };
        if entity_type != "opportunity" || event_type != "opportunity.phase_changed" {
            let _ = tx.commit().await;
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_UNDO_REFERENCE",
                "The referenced event is not a phase transition.",
            )
            .into_response();
        }
        let payload: Value = serde_json::from_str(&payload_json).unwrap_or_default();
        if payload.get("operation").and_then(|v| v.as_str()) != Some("manual") {
            let _ = tx.commit().await;
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_UNDO_REFERENCE",
                "The referenced transition is not an ordinary manual phase change.",
            )
            .into_response();
        }
        let event_from = payload.get("from_stage_id").and_then(|v| v.as_str());
        let event_to = payload.get("to_stage_id").and_then(|v| v.as_str());
        if event_to != current_stage.as_deref() {
            let _ = tx.commit().await;
            return super::error(
                StatusCode::PRECONDITION_FAILED,
                "PHASE_CHANGED_ELSEWHERE",
                "Undo cannot apply: the opportunity is no longer in the phase set by the referenced transition.",
            )
            .into_response();
        }
        let Some(event_from) = event_from else {
            let _ = tx.commit().await;
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_UNDO_REFERENCE",
                "The referenced transition carries no prior stage.",
            )
            .into_response();
        };
        if event_from != target_stage_id {
            let _ = tx.commit().await;
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_UNDO_TARGET",
                "Undo must target the stage recorded before the original transition.",
            )
            .into_response();
        }
        from_stage_id = Some(target_stage_id.clone());
        from_class = Some(target_class.clone());
    }

    // Same-phase selection is a no-op (no event, no revision bump).
    if !reverted && current_stage.as_deref() == Some(target_stage_id.as_str()) {
        let row = match row_json(&mut tx, &id).await {
            Ok(r) => r,
            Err(e) => return workspace_error(e),
        };
        let event_id = last_phase_event_id.unwrap_or_default();
        let result = PhaseResult {
            row,
            event_id,
            phase_revision,
            reverted,
        };
        let result_value = serde_json::to_value(&result).unwrap_or_default();
        if let Err(e) = receipt_store(&mut tx, &key, &canonical, 200, &result_value).await {
            return workspace_error(e);
        }
        let _ = tx.commit().await;
        return Json(result_value).into_response();
    }

    // Terminal stages require the explicit lifecycle flow, never an ordinary edit.
    if target_kind == "terminal" {
        let _ = tx.commit().await;
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "REQUIRES_LIFECYCLE_CONFIRM",
            format!(
                "Stage '{}' is terminal. Closing or resolving an opportunity requires the explicit lifecycle flow with a recorded reason; an ordinary phase edit cannot close it.",
                target_label
            ),
        )
        .into_response();
    }

    // Lifecycle eligibility: only open opportunities accept ordinary phase edits.
    if lifecycle != "open" {
        let _ = tx.commit().await;
        return super::error(
            StatusCode::CONFLICT,
            "LIFECYCLE_BLOCKS_PHASE",
            format!(
                "This opportunity is {lifecycle}; reopen or resolve it through its explicit workflow before changing its phase."
            ),
        )
        .into_response();
    }

    // Concurrency guard.
    if phase_revision != input.expected_phase_revision {
        let _ = tx.commit().await;
        return super::error(
            StatusCode::PRECONDITION_FAILED,
            "STALE_PHASE_REVISION",
            format!(
                "Expected phase revision {} but the opportunity is at revision {}.",
                input.expected_phase_revision, phase_revision
            ),
        )
        .into_response();
    }

    let next_revision = phase_revision + 1;
    let (pack_id, pack_version) = match sqlx::query_as::<_, (String, String)>(
        "SELECT pack_id,pack_version FROM owner_profile WHERE id=1",
    )
    .fetch_one(&mut *tx)
    .await
    {
        Ok(v) => v,
        Err(e) => return command_error(e.into()),
    };

    let event_payload = json!({
        "from_stage_id": from_stage_id,
        "from_class": from_class,
        "to_stage_id": target_stage_id,
        "to_class": target_class,
        "pack_id": pack_id,
        "pack_version": pack_version,
        "operation": if reverted { "undo" } else { "manual" },
        "origin": "queue.sheet",
        "operation_key": key,
        "reversal_of": reversal_of,
    });
    let event_id = match write_event(
        &mut tx,
        "opportunity",
        &id,
        next_revision,
        "opportunity.phase_changed",
        event_payload,
    )
    .await
    {
        Ok(e) => e,
        Err(e) => return workspace_error(e),
    };

    let updated = sqlx::query(
        "UPDATE opportunity SET stage_id=?,stage_pack_id=?,stage_pack_version=?,stage_class=?,phase_revision=?,last_phase_event_id=?,updated_at=? WHERE id=? AND phase_revision=?",
    )
    .bind(&target_stage_id)
    .bind(&pack_id)
    .bind(&pack_version)
    .bind(&target_class)
    .bind(next_revision)
    .bind(&event_id)
    .bind(now_utc())
    .bind(&id)
    .bind(phase_revision)
    .execute(&mut *tx)
    .await;
    match updated {
        Ok(result) if result.rows_affected() != 1 => {
            let _ = tx.rollback().await;
            return super::error(
                StatusCode::PRECONDITION_FAILED,
                "STALE_PHASE_REVISION",
                "The phase changed in another session; reload and review the current phase.",
            )
            .into_response();
        }
        Ok(_) => {}
        Err(e) => return command_error(e.into()),
    }

    let row = match row_json(&mut tx, &id).await {
        Ok(r) => r,
        Err(e) => return workspace_error(e),
    };
    let result = PhaseResult {
        row,
        event_id,
        phase_revision: next_revision,
        reverted,
    };
    let result_value = serde_json::to_value(&result).unwrap_or_default();
    if let Err(e) = receipt_store(&mut tx, &key, &canonical, 200, &result_value).await {
        return workspace_error(e);
    }
    let _ = tx.commit().await;
    let mut r = Json(result_value).into_response();
    super::private_headers(&mut r, state.workspace.epoch());
    r
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
        phase,
        participants,
        next_action,
        other_action_count,
    })
}
