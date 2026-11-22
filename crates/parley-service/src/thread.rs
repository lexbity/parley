//! Contact threads, explicit Mark sent, response deadlines and party-wide
//! suppression (RFC-0002 S07, BE-08).
//!
//! One thread exists per opportunity/recipient pair. Each send records the
//! exact released material versions (primary plus companions), the gate
//! snapshot that authorized them and an exclusive response deadline. Timeouts
//! are reconciled from the waiting set keyed by (thread, current touch,
//! expiry) so restart/retry/concurrent shells produce exactly one timeout
//! event per touch. A decline is party-wide: it suppresses the party across
//! every opportunity and closes each affected opportunity only when no other
//! live contact remains.

use super::materials::{event as append_event, workspace_error};
use super::release::{
    capacity_claim_matches_ledger, capacity_line_fill, evaluate_material, followup_days_for_touch,
    load_approval, load_head,
};
use super::{AppState, Workspace, WorkspaceError, authenticate, new_id, now_utc};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use parley_domain::lifecycle::{
    DEFAULT_CAPACITY_PERIOD, DEFAULT_CAPACITY_UNIT, DEFAULT_REPLY_WINDOW_DAYS, DeadlineKind,
    DeadlineSpec, ResolvedDeadline, ThreadEvent, ThreadState, apply_thread_event,
    opportunity_consequence, resolve_deadline,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Sqlite, Transaction};
use std::collections::BTreeMap;
use std::str::FromStr;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/threads", get(list_threads))
        .route("/api/v1/threads/{id}/send", post(mark_sent))
        .route("/api/v1/threads/{id}/reopen", post(reopen_thread))
        .route("/api/v1/sends/{id}/outcome", post(record_outcome))
        .route("/api/v1/entities/{id}/clear-dnc", post(clear_party_dnc))
        .route("/api/v1/sends", get(list_sends))
        .route(
            "/api/v1/sends/{id}/outcome/correction",
            post(correct_outcome),
        )
        .route(
            "/api/v1/opportunities/{id}/resolution",
            post(record_resolution),
        )
        .route("/api/v1/capacity", get(list_capacity).post(set_capacity))
}

#[derive(Debug, Deserialize)]
struct SendHistoryQuery {
    opportunity_id: String,
    recipient_id: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutcomeCorrectionInput {
    expected_revision: i64,
    class: String,
    rung_accepted: Option<i64>,
    received_at: Option<String>,
    note: Option<String>,
    reason: String,
}

async fn correct_outcome(
    State(state): State<AppState>,
    Path(send_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<OutcomeCorrectionInput>,
) -> Response {
    if let Err(e) = authenticate(&headers, &state).await {
        return (e.0, e.1).into_response();
    }
    if input.reason.trim().chars().count() < 10
        || !matches!(
            input.class.as_str(),
            "no_reply"
                | "reply_positive"
                | "reply_neutral"
                | "reply_negative"
                | "declined"
                | "rung_accepted"
                | "call_scheduled"
        )
        || (input.class == "rung_accepted" && input.rung_accepted.is_none())
        || (input.class != "rung_accepted" && input.rung_accepted.is_some())
    {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "CORRECTION_INVALID",
            "Outcome class/rung invalid or correction reason shorter than ten characters",
        );
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
    let canonical = super::digest(
        format!(
            "outcome-correction\0{send_id}\0{}\0{}\0{}",
            input.expected_revision,
            input.class,
            input.reason.trim()
        )
        .as_bytes(),
    );
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    if let Some(r) =
        super::assistant::receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return r;
    }
    let row:Option<(i64,String)>=match sqlx::query_as("SELECT (SELECT coalesce(max(revision),0) FROM outcome_revision WHERE send_id=s.id),s.recipient_id FROM send s WHERE s.id=? AND EXISTS(SELECT 1 FROM outcome WHERE send_id=s.id)").bind(&send_id).fetch_optional(&mut *tx).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
    let Some((revision, party)) = row else {
        return super::error(
            StatusCode::NOT_FOUND,
            "OUTCOME_NOT_FOUND",
            "No outcome is recorded for this send",
        );
    };
    if revision != input.expected_revision {
        return super::error(
            StatusCode::CONFLICT,
            "OUTCOME_REVISION_CONFLICT",
            "Outcome changed; reload history",
        );
    }
    let next = revision + 1;
    let now = now_utc();
    if let Err(e)=sqlx::query("INSERT INTO outcome_revision(send_id,revision,class,rung_accepted,received_at,recorded_at,note,correction_reason) VALUES(?,?,?,?,?,?,?,?)").bind(&send_id).bind(next).bind(&input.class).bind(input.rung_accepted).bind(&input.received_at).bind(&now).bind(&input.note).bind(input.reason.trim()).execute(&mut *tx).await{return super::command_error(e.into())}
    if input.class == "declined"
        && let Err(e)=sqlx::query("UPDATE party_policy SET do_not_contact=1,state='closed',revision=revision+1 WHERE party_id=?").bind(&party).execute(&mut *tx).await
    {
        return super::command_error(e.into());
    }
    let result = json!({"send_id":send_id,"revision":next,"class":input.class,"rung_accepted":input.rung_accepted,"received_at":input.received_at,"recorded_at":now,"note":input.note,"reason":input.reason.trim()});
    if let Err(e) = append_event(
        &mut tx,
        "outcome",
        &send_id,
        next,
        "outcome.corrected",
        result.clone(),
    )
    .await
    {
        return workspace_error(e);
    }
    let cursor: i64 = match sqlx::query_scalar("SELECT max(cursor) FROM event")
        .fetch_one(&mut *tx)
        .await
    {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    if let Err(e) = super::assistant::write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        200,
        &result,
        cursor,
    )
    .await
    {
        return workspace_error(e);
    }
    if let Err(e) = tx.commit().await {
        return super::command_error(e.into());
    }
    let mut r = Json(result).into_response();
    super::private_headers(&mut r, state.workspace.epoch());
    r
}

#[allow(clippy::type_complexity)]
async fn list_sends(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SendHistoryQuery>,
) -> Response {
    if let Err(e) = authenticate(&headers, &state).await {
        return (e.0, e.1).into_response();
    }
    if query.opportunity_id.trim().is_empty() {
        return super::error(
            StatusCode::BAD_REQUEST,
            "OPPORTUNITY_REQUIRED",
            "opportunity_id is required",
        );
    }
    let rows:Vec<(String,String,i64,String,i64,String,String,String,String,String,i64,Option<String>,Option<String>)>=match sqlx::query_as("SELECT s.id,t.opportunity_id,s.touch_no,s.primary_material_id,s.primary_version_no,s.recipient_id,s.content_sha256,s.sent_at,s.response_deadline,s.gate_snapshot_json,s.capacity_charged,s.deadline_entered_date,s.deadline_zone FROM send s JOIN contact_thread t ON t.id=s.thread_id WHERE t.opportunity_id=? AND (? IS NULL OR s.recipient_id=?) ORDER BY s.sent_at,s.id").bind(&query.opportunity_id).bind(&query.recipient_id).bind(&query.recipient_id).fetch_all(state.workspace.pool()).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
    let mut sends = Vec::new();
    for (
        id,
        opportunity_id,
        touch,
        material_id,
        version,
        recipient,
        digest,
        sent,
        deadline,
        gate,
        charged,
        entered,
        zone,
    ) in rows
    {
        let original:Option<(String,Option<i64>,Option<String>,String,Option<String>)>=match sqlx::query_as("SELECT class,rung_accepted,received_at,recorded_at,note FROM outcome WHERE send_id=?").bind(&id).fetch_optional(state.workspace.pool()).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
        let revisions:Vec<(i64,String,Option<i64>,Option<String>,String,Option<String>,Option<String>)>=match sqlx::query_as("SELECT revision,class,rung_accepted,received_at,recorded_at,correction_reason,note FROM outcome_revision WHERE send_id=? ORDER BY revision").bind(&id).fetch_all(state.workspace.pool()).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
        let corrections=revisions.iter().skip(1).map(|(revision,class,rung,received,recorded,reason,note)|json!({"revision":revision,"class":class,"rung_accepted":rung,"received_at":received,"recorded_at":recorded,"reason":reason,"note":note})).collect::<Vec<_>>();
        let effective=revisions.last().map(|(_,class,rung,received,recorded,_,note)|json!({"class":class,"rung_accepted":rung,"received_at":received,"recorded_at":recorded,"note":note}));
        let parts:Vec<(String,i64,String,String)>=match sqlx::query_as("SELECT sp.material_id,sp.version_no,sp.kind,mv.content_sha256 FROM send_part sp JOIN material_version mv ON mv.material_id=sp.material_id AND mv.version_no=sp.version_no WHERE sp.send_id=? ORDER BY sp.kind,sp.material_id,sp.version_no").bind(&id).fetch_all(state.workspace.pool()).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
        let parts=parts.into_iter().map(|(material_id,version_no,kind,content_sha256)|json!({"material_id":material_id,"version_no":version_no,"kind":kind,"content_sha256":content_sha256})).collect::<Vec<_>>();
        let original=original.map(|(class,rung,received,recorded,note)|json!({"class":class,"rung_accepted":rung,"received_at":received,"recorded_at":recorded,"note":note}));
        sends.push(json!({"send":{"id":id,"opportunity_id":opportunity_id,"touch_no":touch,"primary_material_id":material_id,"primary_version_no":version,"recipient_id":recipient,"content_sha256":digest,"sent_at":sent,"response_deadline":deadline,"deadline_entered_date":entered,"deadline_zone":zone,"capacity_charged":charged!=0,"gate_snapshot":serde_json::from_str::<serde_json::Value>(&gate).unwrap_or_default(),"parts":parts},"original_outcome":original,"effective_outcome":effective,"corrections":corrections}));
    }
    let mut response = Json(json!({"items":sends})).into_response();
    super::private_headers(&mut response, state.workspace.epoch());
    response
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadRecord {
    pub id: String,
    pub opportunity_id: String,
    pub recipient_id: String,
    pub state: String,
    pub current_touch: i64,
    pub expires_at: Option<String>,
    pub next_action_at: Option<String>,
    pub sent_at: Option<String>,
    pub updated_at: String,
    /// Per-touch send history for this thread, newest last. Each entry
    /// carries its outcome when one is recorded, so a reloaded client can
    /// attach later responses to a historical send (RFC-0003 FQ5).
    pub sends: Vec<ThreadSend>,
}

/// One recorded touch with its (optional) outcome. Same fields as `SendRecord`
/// with a nested outcome; the wire contract is `ThreadSend`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadSend {
    pub id: String,
    pub touch_no: i64,
    pub primary_material_id: String,
    pub primary_version_no: i64,
    pub recipient_id: String,
    pub content_sha256: String,
    pub sent_at: String,
    pub response_deadline: String,
    pub deadline_zone: String,
    pub next_action_at: Option<String>,
    pub capacity_charged: bool,
    pub outcome: Option<OutcomeRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PartyPolicyRecord {
    pub party_id: String,
    pub state: String,
    pub do_not_contact: bool,
    pub highest_rung: i64,
    pub touch_count: i64,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendRecord {
    pub id: String,
    pub thread_id: String,
    pub touch_no: i64,
    pub primary_material_id: String,
    pub primary_version_no: i64,
    pub recipient_id: String,
    pub content_sha256: String,
    pub sent_at: String,
    pub response_deadline: String,
    pub deadline_zone: String,
    pub next_action_at: Option<String>,
    pub capacity_charged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutcomeRecord {
    pub id: String,
    pub send_id: String,
    pub class: String,
    pub rung_accepted: Option<i64>,
    pub received_at: Option<String>,
    pub recorded_at: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolutionRecord {
    pub id: String,
    pub opportunity_id: String,
    pub thread_id: Option<String>,
    pub result: String,
    pub they_benefited: String,
    pub i_benefited: String,
    pub noted_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapacityRecord {
    pub period: String,
    pub unit: String,
    pub total: f64,
    pub committed: f64,
    pub remaining: f64,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ReconcileReport {
    pub scanned: i64,
    pub timed_out: i64,
    pub opportunities_closed: i64,
}

#[derive(Debug, Deserialize)]
struct ThreadQuery {
    opportunity_id: Option<String>,
    recipient_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MaterialVersionRef {
    material_id: String,
    version: i64,
    release_id: String,
}

#[derive(Debug, Deserialize)]
struct CompanionRef {
    material_id: String,
    version: i64,
    release_id: String,
}

#[derive(Debug, Deserialize)]
struct DeadlineInput {
    days: Option<i64>,
    date: Option<String>,
    at: Option<String>,
    zone: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SendInput {
    primary: MaterialVersionRef,
    #[serde(default)]
    companions: Vec<CompanionRef>,
    #[serde(default)]
    deadline: Option<DeadlineInput>,
    /// Used only when the thread does not exist yet: the pair is created
    /// lazily on the first explicit send.
    #[serde(default)]
    opportunity_id: Option<String>,
    #[serde(default)]
    recipient_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OutcomeInput {
    class: String,
    rung_accepted: Option<i64>,
    received_at: Option<String>,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClearDncInput {
    reason: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResolutionInput {
    result: String,
    they_benefited: String,
    i_benefited: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CapacityInput {
    period: String,
    unit: String,
    total: f64,
}

fn thread_state_name(state: ThreadState) -> &'static str {
    match state {
        ThreadState::Waiting => "waiting",
        ThreadState::Conversing => "conversing",
        ThreadState::TimedOut => "timed_out",
        ThreadState::ClosedDeclined => "closed_declined",
        ThreadState::Resolved => "resolved",
        ThreadState::Dormant => "dormant",
    }
}

fn parse_thread_state(state: &str) -> Result<ThreadState, WorkspaceError> {
    Ok(match state {
        "waiting" => ThreadState::Waiting,
        "conversing" => ThreadState::Conversing,
        "timed_out" => ThreadState::TimedOut,
        "closed_declined" => ThreadState::ClosedDeclined,
        "resolved" => ThreadState::Resolved,
        "dormant" => ThreadState::Dormant,
        _ => return Err(WorkspaceError::Invalid("unknown thread state".into())),
    })
}

fn parse_zone(value: &str) -> Result<chrono_tz::Tz, WorkspaceError> {
    chrono_tz::Tz::from_str(value)
        .map_err(|_| WorkspaceError::Invalid(format!("unknown IANA timezone: {value}")))
}

fn format_utc(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn resolve_deadline_input(
    input: &Option<DeadlineInput>,
    owner_zone: &str,
    now: &str,
) -> Result<ResolvedDeadline, WorkspaceError> {
    let zone = match input {
        Some(value) if value.zone.as_deref().is_some_and(|z| !z.trim().is_empty()) => {
            parse_zone(value.zone.as_deref().unwrap())?
        }
        _ => parse_zone(owner_zone)?,
    };
    let now_utc = parse_utc(now)?;
    let kind = match input {
        Some(value) if value.at.as_deref().is_some_and(|v| !v.trim().is_empty()) => {
            DeadlineKind::Timestamp {
                at: parse_utc(value.at.as_deref().unwrap())?,
            }
        }
        Some(value) if value.date.as_deref().is_some_and(|v| !v.trim().is_empty()) => {
            let date =
                chrono::NaiveDate::parse_from_str(value.date.as_deref().unwrap(), "%Y-%m-%d")
                    .map_err(|_| WorkspaceError::Invalid("date must be YYYY-MM-DD".into()))?;
            DeadlineKind::Date { date }
        }
        Some(value) if value.days.is_some() => DeadlineKind::RelativeDays {
            days: value.days.unwrap_or(0),
        },
        _ => DeadlineKind::RelativeDays {
            days: DEFAULT_REPLY_WINDOW_DAYS,
        },
    };
    Ok(resolve_deadline(&DeadlineSpec { kind, zone }, now_utc))
}

async fn owner_timezone(tx: &mut Transaction<'_, Sqlite>) -> Result<String, WorkspaceError> {
    Ok(
        sqlx::query_scalar("SELECT timezone FROM owner_profile WHERE id=1")
            .fetch_one(&mut **tx)
            .await?,
    )
}

fn parse_utc(value: &str) -> Result<chrono::DateTime<chrono::Utc>, WorkspaceError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|_| WorkspaceError::Invalid("timestamp must be RFC3339 UTC".into()))
}

/// Next follow-up action at the pack cadence after `prior_touches` sends
/// (RFC-0001 §10.6 CAD-1), measured in business days from today.
fn plan_next_action(
    now: &str,
    prior_touches: i64,
    pack_id: &str,
) -> Result<String, WorkspaceError> {
    let today = chrono::NaiveDate::parse_from_str(&now[..10], "%Y-%m-%d")
        .map_err(|_| WorkspaceError::Invalid("now has an unexpected date".into()))?;
    let interval = followup_days_for_touch(pack_id, prior_touches);
    let target = parley_domain::lifecycle::add_business_days(today, interval);
    Ok(format!("{}T00:00:00.000Z", target.format("%Y-%m-%d")))
}

struct ThreadRow {
    id: String,
    opportunity_id: String,
    recipient_id: String,
    state: ThreadState,
    current_touch: i64,
}

async fn load_thread(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: &str,
) -> Result<ThreadRow, WorkspaceError> {
    let row = sqlx::query_as::<_, (String, String, String, String, i64)>(
        "SELECT id,opportunity_id,recipient_id,state,current_touch FROM contact_thread WHERE id=?",
    )
    .bind(thread_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((id, opportunity_id, recipient_id, state, current_touch)) = row else {
        return Err(WorkspaceError::Invalid("thread does not exist".into()));
    };
    Ok(ThreadRow {
        id,
        opportunity_id,
        recipient_id,
        state: parse_thread_state(&state)?,
        current_touch,
    })
}

struct SendRow {
    thread_id: String,
    touch_no: i64,
}

async fn load_send(
    tx: &mut Transaction<'_, Sqlite>,
    send_id: &str,
) -> Result<SendRow, WorkspaceError> {
    let row = sqlx::query_as::<_, (String, i64)>("SELECT thread_id,touch_no FROM send WHERE id=?")
        .bind(send_id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some((thread_id, touch_no)) = row else {
        return Err(WorkspaceError::Invalid("send does not exist".into()));
    };
    Ok(SendRow {
        thread_id,
        touch_no,
    })
}

async fn recoup_opportunity(
    tx: &mut Transaction<'_, Sqlite>,
    opportunity_id: &str,
    now: &str,
    force_reason: Option<&str>,
) -> Result<bool, WorkspaceError> {
    let states: Vec<String> = sqlx::query_scalar(
        "SELECT state FROM contact_thread WHERE opportunity_id=? AND state IN ('waiting','conversing','timed_out','closed_declined')",
    )
    .bind(opportunity_id)
    .fetch_all(&mut **tx)
    .await?;
    let parsed: Vec<ThreadState> = states
        .iter()
        .map(|state| parse_thread_state(state))
        .collect::<Result<_, _>>()?;
    let consequence = if let Some(reason) = force_reason {
        if reason == "declined" {
            opportunity_consequence(&[ThreadState::ClosedDeclined])
        } else if reason == "timed_out" {
            opportunity_consequence(&[ThreadState::TimedOut])
        } else {
            opportunity_consequence(&parsed)
        }
    } else {
        opportunity_consequence(&parsed)
    };
    let (state, reason) = match consequence {
        parley_domain::lifecycle::OpportunityConsequence::Active => return Ok(false),
        parley_domain::lifecycle::OpportunityConsequence::CloseTimedOut => ("closed", "timed_out"),
        parley_domain::lifecycle::OpportunityConsequence::CloseDeclined => ("closed", "declined"),
    };
    let current: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT state,close_reason FROM opportunity WHERE id=?")
            .bind(opportunity_id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((current_state, current_reason)) = current else {
        return Err(WorkspaceError::Invalid("opportunity does not exist".into()));
    };
    if current_state == state && current_reason.as_deref() == Some(reason) {
        return Ok(false);
    }
    if current_state != "open" && !(current_state == "closed" && current_reason.is_some()) {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE opportunity SET state=?,close_reason=?,revision=revision+1,updated_at=? WHERE id=?",
    )
    .bind(state)
    .bind(reason)
    .bind(now)
    .bind(opportunity_id)
    .execute(&mut **tx)
    .await?;
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM opportunity WHERE id=?")
        .bind(opportunity_id)
        .fetch_one(&mut **tx)
        .await?;
    append_event(
        tx,
        "opportunity",
        opportunity_id,
        revision,
        "opportunity.closed",
        json!({"reason": reason}),
    )
    .await?;
    Ok(true)
}

async fn recoup_party_decline(
    tx: &mut Transaction<'_, Sqlite>,
    party_id: &str,
    now: &str,
) -> Result<(), WorkspaceError> {
    // Close every live thread for the party across opportunities.
    let affected: Vec<(String, String)> = sqlx::query_as(
        "SELECT id,opportunity_id FROM contact_thread WHERE recipient_id=? AND state IN ('waiting','conversing')",
    )
    .bind(party_id)
    .fetch_all(&mut **tx)
    .await?;
    for (thread_id, _) in &affected {
        sqlx::query("UPDATE contact_thread SET state='closed_declined',expires_at=NULL,updated_at=? WHERE id=? AND state IN ('waiting','conversing')")
            .bind(now)
            .bind(thread_id)
            .execute(&mut **tx)
            .await?;
        append_event(
            tx,
            "thread",
            thread_id,
            0,
            "thread.closed_declined",
            json!({"reason":"declined"}),
        )
        .await?;
    }
    let mut seen = std::collections::BTreeSet::new();
    for (_, opportunity_id) in affected {
        if seen.insert(opportunity_id.clone()) {
            recoup_opportunity(tx, &opportunity_id, now, Some("declined")).await?;
        }
    }
    Ok(())
}

impl Workspace {
    /// Send history for one thread, newest last, with outcomes attached.
    async fn load_thread_sends(&self, thread_id: &str) -> Result<Vec<ThreadSend>, WorkspaceError> {
        let rows = sqlx::query_as::<_, (String, i64, String, i64, String, String, String, String, String, bool)>(
            "SELECT id,touch_no,primary_material_id,primary_version_no,recipient_id,content_sha256,sent_at,response_deadline,deadline_zone,capacity_charged FROM send WHERE thread_id=? ORDER BY touch_no,id",
        )
        .bind(thread_id)
        .fetch_all(&self.pool)
        .await?;
        let mut sends = Vec::with_capacity(rows.len());
        for (
            id,
            touch_no,
            primary_material_id,
            primary_version_no,
            recipient_id,
            content_sha256,
            sent_at,
            response_deadline,
            deadline_zone,
            capacity_charged,
        ) in rows
        {
            let outcome: Option<OutcomeRecord> = sqlx::query_as::<_, (String, String, String, Option<i64>, Option<String>, String, Option<String>)>(
                "SELECT id,send_id,class,rung_accepted,received_at,recorded_at,note FROM outcome WHERE send_id=?",
            )
            .bind(&id)
            .fetch_optional(&self.pool)
            .await?
            .map(|(id, send_id, class, rung_accepted, received_at, recorded_at, note)| OutcomeRecord {
                id,
                send_id,
                class,
                rung_accepted,
                received_at,
                recorded_at,
                note,
            });
            sends.push(ThreadSend {
                id,
                touch_no,
                primary_material_id,
                primary_version_no,
                recipient_id,
                content_sha256,
                sent_at,
                response_deadline,
                deadline_zone,
                // The per-touch next action is a thread-level schedule, not a
                // column on send; history entries leave it unset.
                next_action_at: None,
                capacity_charged,
                outcome,
            });
        }
        Ok(sends)
    }

    pub async fn list_threads(
        &self,
        opportunity_id: Option<&str>,
        recipient_id: Option<&str>,
    ) -> Result<Vec<ThreadRecord>, WorkspaceError> {
        let mut builder = sqlx::QueryBuilder::new(
            "SELECT id,opportunity_id,recipient_id,state,current_touch,expires_at,next_action_at,sent_at,updated_at FROM contact_thread WHERE 1=1",
        );
        if let Some(opportunity_id) = opportunity_id {
            builder.push(" AND opportunity_id = ");
            builder.push_bind(opportunity_id);
        }
        if let Some(recipient_id) = recipient_id {
            builder.push(" AND recipient_id = ");
            builder.push_bind(recipient_id);
        }
        builder.push(" ORDER BY updated_at DESC,id");
        let rows = builder
            .build_query_as::<(
                String,
                String,
                String,
                String,
                i64,
                Option<String>,
                Option<String>,
                Option<String>,
                String,
            )>()
            .fetch_all(self.pool())
            .await?;
        let mut threads = Vec::with_capacity(rows.len());
        for (
            id,
            opportunity_id,
            recipient_id,
            state,
            current_touch,
            expires_at,
            next_action_at,
            sent_at,
            updated_at,
        ) in rows
        {
            let sends = self.load_thread_sends(&id).await?;
            threads.push(ThreadRecord {
                id,
                opportunity_id,
                recipient_id,
                state,
                current_touch,
                expires_at,
                next_action_at,
                sent_at,
                updated_at,
                sends,
            });
        }
        Ok(threads)
    }

    /// Reconcile due threads. A thread whose exclusive expiry has passed is
    /// timed out exactly once: the row update is conditioned on the waiting
    /// state so concurrent reconcile, restart or retry attempts cannot double-
    /// apply. Opportunities with no remaining live thread close with the
    /// matching reason.
    pub async fn reconcile_deadlines(&self, now: &str) -> Result<ReconcileReport, WorkspaceError> {
        let mut report = ReconcileReport {
            scanned: 0,
            timed_out: 0,
            opportunities_closed: 0,
        };
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let due: Vec<(String, String)> = sqlx::query_as(
            "SELECT id,opportunity_id FROM contact_thread WHERE state='waiting' AND expires_at IS NOT NULL AND expires_at <= ? ORDER BY expires_at,id LIMIT 128",
        )
        .bind(now)
        .fetch_all(&mut *tx)
        .await?;
        report.scanned = due.len() as i64;
        let mut affected = std::collections::BTreeSet::new();
        let mut live = Vec::new();
        for (thread_id, opportunity_id) in due {
            let updated = sqlx::query(
                "UPDATE contact_thread SET state='timed_out',updated_at=? WHERE id=? AND state='waiting' AND expires_at IS NOT NULL AND expires_at <= ?",
            )
            .bind(now)
            .bind(&thread_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() == 1 {
                report.timed_out += 1;
                live.push((thread_id, opportunity_id.clone()));
                affected.insert(opportunity_id);
            }
        }
        for (thread_id, _) in &live {
            append_event(
                &mut tx,
                "thread",
                thread_id,
                0,
                "thread.timed_out",
                json!({"reason":"unanswered"}),
            )
            .await?;
        }
        for opportunity_id in affected {
            if recoup_opportunity(&mut tx, &opportunity_id, now, Some("timed_out")).await? {
                report.opportunities_closed += 1;
            }
        }
        tx.commit().await?;
        Ok(report)
    }

    pub(crate) async fn mark_sent(
        &self,
        thread_id: &str,
        input: &SendInput,
        now: &str,
    ) -> Result<SendRecord, WorkspaceError> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let thread = match load_thread(&mut tx, thread_id).await {
            Ok(thread) => thread,
            Err(_) => {
                let opportunity_id = input.opportunity_id.as_deref().ok_or_else(|| {
                    WorkspaceError::Invalid(
                        "a new thread needs an opportunity and recipient pair".into(),
                    )
                })?;
                let recipient_id = input.recipient_id.as_deref().ok_or_else(|| {
                    WorkspaceError::Invalid(
                        "a new thread needs an opportunity and recipient pair".into(),
                    )
                })?;
                let member: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM opportunity_party WHERE opportunity_id=? AND party_id=?",
                )
                .bind(opportunity_id)
                .bind(recipient_id)
                .fetch_one(&mut *tx)
                .await?;
                if member != 1 {
                    return Err(WorkspaceError::Invalid(
                        "recipient is not an opportunity participant".into(),
                    ));
                }
                // One current thread per opportunity/recipient pair: reuse the
                // existing thread when the pair is already threaded.
                if let Some(existing) = sqlx::query_scalar::<_, String>(
                    "SELECT id FROM contact_thread WHERE opportunity_id=? AND recipient_id=?",
                )
                .bind(opportunity_id)
                .bind(recipient_id)
                .fetch_optional(&mut *tx)
                .await?
                {
                    load_thread(&mut tx, &existing).await?
                } else {
                    sqlx::query("INSERT INTO contact_thread(id,opportunity_id,recipient_id,state,current_touch,created_at,updated_at) VALUES(?,?,?,'waiting',0,?,?)")
                        .bind(thread_id)
                        .bind(opportunity_id)
                        .bind(recipient_id)
                        .bind(now)
                        .bind(now)
                        .execute(&mut *tx)
                        .await?;
                    ThreadRow {
                        id: thread_id.to_owned(),
                        opportunity_id: opportunity_id.to_owned(),
                        recipient_id: recipient_id.to_owned(),
                        state: ThreadState::Waiting,
                        current_touch: 0,
                    }
                }
            }
        };
        let opp_state: String = sqlx::query_scalar("SELECT state FROM opportunity WHERE id=?")
            .bind(&thread.opportunity_id)
            .fetch_one(&mut *tx)
            .await?;
        if opp_state != "open" {
            return Err(WorkspaceError::Invalid(
                "sending requires an open opportunity".into(),
            ));
        }
        let now_zone = owner_timezone(&mut tx).await?;
        let resolved = resolve_deadline_input(&input.deadline, &now_zone, now)?;
        let deadline_str = format_utc(resolved.expiry);

        // Primary material: head, approval, release and full gate recheck.
        let primary_head = load_head(&mut tx, &input.primary.material_id).await?;
        if primary_head.version != input.primary.version {
            return Err(WorkspaceError::Invalid(
                "primary material version does not match the head version".into(),
            ));
        }
        if primary_head.opportunity_id != thread.opportunity_id
            || primary_head.recipient_id != thread.recipient_id
        {
            return Err(WorkspaceError::Invalid(
                "primary material must target this thread's opportunity and recipient".into(),
            ));
        }
        let primary_approval = load_approval(&mut tx, &primary_head).await?;
        let primary_overrides: BTreeMap<String, String> =
            serde_json::from_str(&primary_approval.overrides_json)
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let primary_gate =
            evaluate_material(&mut tx, &primary_head, &primary_overrides, true).await?;
        if !primary_gate.copy_enabled {
            return Err(WorkspaceError::Invalid(format!(
                "mark sent blocked: {}",
                primary_gate
                    .checks
                    .iter()
                    .filter(|check| !check.pass && !check.overridden)
                    .map(|check| check.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let primary_release =
            verify_release(&mut tx, &input.primary.release_id, &primary_head).await?;
        if primary_release != primary_head.hash {
            return Err(WorkspaceError::Invalid(
                "primary release bytes do not match the approved material version".into(),
            ));
        }

        let mut parts: Vec<(String, String, i64, String, String)> = vec![(
            "primary".to_owned(),
            input.primary.material_id.clone(),
            input.primary.version,
            input.primary.release_id.clone(),
            primary_head.hash.clone(),
        )];

        for companion in &input.companions {
            let companion_head = load_head(&mut tx, &companion.material_id).await?;
            if companion_head.version != companion.version {
                return Err(WorkspaceError::Invalid(
                    "companion material version does not match the head version".into(),
                ));
            }
            if companion_head.opportunity_id != thread.opportunity_id
                || companion_head.recipient_id != thread.recipient_id
            {
                return Err(WorkspaceError::Invalid(
                    "companion release must target the same opportunity and recipient as the primary"
                        .into(),
                ));
            }
            let companion_approval = load_approval(&mut tx, &companion_head).await?;
            let companion_overrides: BTreeMap<String, String> =
                serde_json::from_str(&companion_approval.overrides_json)
                    .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
            let companion_gate =
                evaluate_material(&mut tx, &companion_head, &companion_overrides, true).await?;
            if !companion_gate.copy_enabled {
                return Err(WorkspaceError::Invalid(format!(
                    "companion release blocked: {}",
                    companion_gate
                        .checks
                        .iter()
                        .filter(|check| !check.pass && !check.overridden)
                        .map(|check| check.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            let companion_release =
                verify_release(&mut tx, &companion.release_id, &companion_head).await?;
            if companion_release != companion_head.hash {
                return Err(WorkspaceError::Invalid(
                    "companion release bytes do not match its approved material version".into(),
                ));
            }
            parts.push((
                "companion".to_owned(),
                companion.material_id.clone(),
                companion.version,
                companion.release_id.clone(),
                companion_head.hash.clone(),
            ));
        }

        // Capacity: if the primary carries a capacity-line claim, the gate has
        // already verified it equals the ledger's remaining value; charge it
        // exactly once for this send.
        let capacity_fill = capacity_line_fill(&primary_head.slot_fills);
        let mut capacity_charged = false;
        if let Some(value) = &capacity_fill
            && capacity_claim_matches_ledger(&mut tx, value).await?
        {
            let updated = sqlx::query(
                "UPDATE capacity_ledger SET committed=committed+1,updated_at=? WHERE period=? AND unit=? AND committed < total",
            )
            .bind(now)
            .bind(DEFAULT_CAPACITY_PERIOD)
            .bind(DEFAULT_CAPACITY_UNIT)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() == 1 {
                capacity_charged = true;
            } else {
                return Err(WorkspaceError::Invalid(
                    "capacity ledger has no remaining commitment".into(),
                ));
            }
        }

        let touch_no = thread.current_touch + 1;
        let send_id = new_id(&thread.id);
        let pack_id: String = sqlx::query_scalar("SELECT pack_id FROM owner_profile WHERE id=1")
            .fetch_one(&mut *tx)
            .await?;
        let next_action_at = plan_next_action(now, thread.current_touch, &pack_id)?;
        let gate_json = serde_json::to_string(&primary_gate)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let overrides_json = serde_json::to_string(&primary_overrides)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let segment_ids: Vec<String> = sqlx::query_scalar(
            "SELECT segment_id FROM segment_party WHERE party_id=? ORDER BY segment_id",
        )
        .bind(&thread.recipient_id)
        .fetch_all(&mut *tx)
        .await?;
        let segment_snapshot = serde_json::to_string(&segment_ids)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        sqlx::query(
            "INSERT INTO send(id,thread_id,touch_no,primary_material_id,primary_version_no,recipient_id,gate_snapshot_json,overrides_json,content_sha256,sent_at,response_deadline,deadline_entered_date,deadline_zone,capacity_charged,segment_ids_snapshot_json) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&send_id)
        .bind(&thread.id)
        .bind(touch_no)
        .bind(&primary_head.id)
        .bind(primary_head.version)
        .bind(&thread.recipient_id)
        .bind(&gate_json)
        .bind(&overrides_json)
        .bind(&primary_head.hash)
        .bind(now)
        .bind(&deadline_str)
        .bind(resolved.entered_date.map(|date| date.format("%Y-%m-%d").to_string()))
        .bind(resolved.zone.to_string())
        .bind(capacity_charged)
        .bind(segment_snapshot)
        .execute(&mut *tx)
        .await?;
        for (kind, material_id, version, release_id, _hash) in &parts {
            sqlx::query(
                "INSERT INTO send_part(send_id,material_id,version_no,release_id,kind) VALUES(?,?,?,?,?)",
            )
            .bind(&send_id)
            .bind(material_id)
            .bind(version)
            .bind(release_id)
            .bind(kind)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE contact_thread SET state='waiting',current_touch=?,expires_at=?,next_action_at=?,sent_at=?,updated_at=? WHERE id=?",
        )
        .bind(touch_no)
        .bind(&deadline_str)
        .bind(&next_action_at)
        .bind(now)
        .bind(now)
        .bind(&thread.id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE party_policy SET touch_count=touch_count+1,revision=revision+1 WHERE party_id=?")
            .bind(&thread.recipient_id)
            .execute(&mut *tx)
            .await?;
        append_event(
            &mut tx,
            "send",
            &send_id,
            touch_no,
            "send.marked_sent",
            json!({"thread_id": thread.id, "touch_no": touch_no, "primary_material_id": primary_head.id, "primary_version_no": primary_head.version, "content_sha256": primary_head.hash, "response_deadline": deadline_str, "capacity_charged": capacity_charged}),
        )
        .await?;
        append_event(
            &mut tx,
            "thread",
            &thread.id,
            touch_no,
            "thread.deadline_set",
            json!({"expires_at": deadline_str, "touch_no": touch_no}),
        )
        .await?;
        tx.commit().await?;
        Ok(SendRecord {
            id: send_id,
            thread_id: thread.id,
            touch_no,
            primary_material_id: primary_head.id,
            primary_version_no: primary_head.version,
            recipient_id: thread.recipient_id,
            content_sha256: primary_head.hash,
            sent_at: now.to_owned(),
            response_deadline: deadline_str,
            deadline_zone: resolved.zone.to_string(),
            next_action_at: Some(next_action_at),
            capacity_charged,
        })
    }

    pub(crate) async fn record_outcome(
        &self,
        send_id: &str,
        input: &OutcomeInput,
        now: &str,
    ) -> Result<OutcomeRecord, WorkspaceError> {
        const CLASSES: &[&str] = &[
            "no_reply",
            "reply_positive",
            "reply_neutral",
            "reply_negative",
            "declined",
            "rung_accepted",
            "call_scheduled",
        ];
        if !CLASSES.contains(&input.class.as_str()) {
            return Err(WorkspaceError::Invalid("unknown outcome class".into()));
        }
        if input.class == "rung_accepted"
            && !input
                .rung_accepted
                .is_some_and(|rung| (1..=5).contains(&rung))
        {
            return Err(WorkspaceError::Invalid(
                "rung_accepted outcome requires a rung between 1 and 5".into(),
            ));
        }
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let send = load_send(&mut tx, send_id).await?;
        let thread = load_thread(&mut tx, &send.thread_id).await?;
        let existing: Option<(String, String)> =
            sqlx::query_as("SELECT id,class FROM outcome WHERE send_id=?")
                .bind(send_id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((id, class)) = existing {
            if class == input.class {
                tx.commit().await?;
                return Ok(OutcomeRecord {
                    id,
                    send_id: send_id.to_owned(),
                    class: input.class.clone(),
                    rung_accepted: input.rung_accepted,
                    received_at: input.received_at.clone(),
                    recorded_at: now.to_owned(),
                    note: input.note.clone(),
                });
            }
            return Err(WorkspaceError::Invalid(
                "an outcome is already recorded for this send".into(),
            ));
        }
        let outcome_id = new_id(send_id);
        sqlx::query("INSERT INTO outcome(id,send_id,class,rung_accepted,received_at,recorded_at,note) VALUES(?,?,?,?,?,?,?)")
            .bind(&outcome_id)
            .bind(send_id)
            .bind(&input.class)
            .bind(input.rung_accepted)
            .bind(input.received_at.as_deref())
            .bind(now)
            .bind(input.note.as_deref())
            .execute(&mut *tx)
            .await?;
        if input.class == "declined" {
            sqlx::query("UPDATE party_policy SET state='closed',do_not_contact=1,revision=revision+1 WHERE party_id=?")
                .bind(&thread.recipient_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE contact_thread SET state='closed_declined',expires_at=NULL,updated_at=? WHERE id=?")
                .bind(now)
                .bind(&thread.id)
                .execute(&mut *tx)
                .await?;
            append_event(
                &mut tx,
                "thread",
                &thread.id,
                thread.current_touch,
                "thread.closed_declined",
                json!({"reason":"declined"}),
            )
            .await?;
            recoup_party_decline(&mut tx, &thread.recipient_id, now).await?;
        } else {
            if let Some(rung) = input.rung_accepted {
                sqlx::query(
                    "UPDATE party_policy SET highest_rung=?,revision=revision+1 WHERE party_id=? AND highest_rung < ?",
                )
                .bind(rung)
                .bind(&thread.recipient_id)
                .bind(rung)
                .execute(&mut *tx)
                .await?;
            }
            if send.touch_no == thread.current_touch {
                match thread.state {
                    ThreadState::Waiting => {
                        sqlx::query("UPDATE contact_thread SET state='conversing',expires_at=NULL,updated_at=? WHERE id=?")
                            .bind(now)
                            .bind(&thread.id)
                            .execute(&mut *tx)
                            .await?;
                        append_event(
                            &mut tx,
                            "thread",
                            &thread.id,
                            thread.current_touch,
                            "thread.in_conversation",
                            json!({"send_id": send_id}),
                        )
                        .await?;
                    }
                    ThreadState::TimedOut => {
                        append_event(
                            &mut tx,
                            "thread",
                            &thread.id,
                            thread.current_touch,
                            "outcome.recorded_late",
                            json!({"send_id": send_id}),
                        )
                        .await?;
                    }
                    _ => {}
                }
            }
        }
        append_event(
            &mut tx,
            "send",
            send_id,
            send.touch_no,
            "outcome.recorded",
            json!({"class": input.class, "rung_accepted": input.rung_accepted}),
        )
        .await?;
        tx.commit().await?;
        Ok(OutcomeRecord {
            id: outcome_id,
            send_id: send_id.to_owned(),
            class: input.class.clone(),
            rung_accepted: input.rung_accepted,
            received_at: input.received_at.clone(),
            recorded_at: now.to_owned(),
            note: input.note.clone(),
        })
    }

    /// Explicit owner action that clears party-wide do-not-contact suppression
    /// (RFC-0001 GATE-5, RFC-0002 BE-08). A decline closed the party; only a
    /// separate, reason-bearing clearance re-opens its eligibility for fresh
    /// releases. It never by itself reopens closed threads; that stays the
    /// separate `reopen_thread` action. The reason is recorded on the event.
    pub(crate) async fn clear_dnc(
        &self,
        party_id: &str,
        reason: &str,
    ) -> Result<PartyPolicyRecord, WorkspaceError> {
        let reason = reason.trim();
        if reason.len() < 10 {
            return Err(WorkspaceError::Invalid(
                "do-not-contact clearance requires an explicit reason of at least ten characters"
                    .into(),
            ));
        }
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let policy: Option<(String, i64, i64, i64, i64)> = sqlx::query_as(
            "SELECT state,do_not_contact,highest_rung,touch_count,revision FROM party_policy WHERE party_id=?",
        )
        .bind(party_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((state, do_not_contact, highest_rung, touch_count, revision)) = policy else {
            return Err(WorkspaceError::NotFound);
        };
        if do_not_contact != 1 {
            return Err(WorkspaceError::Invalid(
                "party has no do-not-contact suppression to clear".into(),
            ));
        }
        let next_revision = revision + 1;
        sqlx::query("UPDATE party_policy SET state='open',do_not_contact=0,revision=? WHERE party_id=? AND do_not_contact=1")
            .bind(next_revision)
            .bind(party_id)
            .execute(&mut *tx)
            .await?;
        append_event(
            &mut tx,
            "party",
            party_id,
            next_revision,
            "party.dnc_cleared",
            json!({"reason": reason, "from_state": state}),
        )
        .await?;
        tx.commit().await?;
        Ok(PartyPolicyRecord {
            party_id: party_id.to_owned(),
            state: "open".to_owned(),
            do_not_contact: false,
            highest_rung,
            touch_count,
            revision: next_revision,
        })
    }

    pub(crate) async fn reopen_thread(
        &self,
        thread_id: &str,
        now: &str,
    ) -> Result<ThreadRecord, WorkspaceError> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let thread = load_thread(&mut tx, thread_id).await?;
        let next = apply_thread_event(thread.state, ThreadEvent::Reopen).ok_or_else(|| {
            WorkspaceError::Invalid("thread cannot be reopened from its current state".into())
        })?;
        sqlx::query("UPDATE contact_thread SET state=?,expires_at=NULL,next_action_at=NULL,updated_at=? WHERE id=?")
            .bind(thread_state_name(next))
            .bind(now)
            .bind(thread_id)
            .execute(&mut *tx)
            .await?;
        append_event(
            &mut tx,
            "thread",
            thread_id,
            thread.current_touch,
            "thread.reopened",
            json!({"from": thread_state_name(thread.state)}),
        )
        .await?;
        let opp_state: (String, Option<String>) =
            sqlx::query_as("SELECT state,close_reason FROM opportunity WHERE id=?")
                .bind(&thread.opportunity_id)
                .fetch_one(&mut *tx)
                .await?;
        if opp_state.0 == "closed" {
            sqlx::query("UPDATE opportunity SET state='open',close_reason=NULL,revision=revision+1,updated_at=? WHERE id=? AND state='closed'")
                .bind(now)
                .bind(&thread.opportunity_id)
                .execute(&mut *tx)
                .await?;
            let revision: i64 = sqlx::query_scalar("SELECT revision FROM opportunity WHERE id=?")
                .bind(&thread.opportunity_id)
                .fetch_one(&mut *tx)
                .await?;
            append_event(
                &mut tx,
                "opportunity",
                &thread.opportunity_id,
                revision,
                "opportunity.reopened",
                json!({"reason": opp_state.1}),
            )
            .await?;
        }
        tx.commit().await?;
        let sends = self.load_thread_sends(thread_id).await?;
        Ok(ThreadRecord {
            id: thread.id,
            opportunity_id: thread.opportunity_id,
            recipient_id: thread.recipient_id,
            state: thread_state_name(next).to_owned(),
            current_touch: thread.current_touch,
            expires_at: None,
            next_action_at: None,
            sent_at: None,
            updated_at: now.to_owned(),
            sends,
        })
    }

    pub(crate) async fn record_resolution(
        &self,
        opportunity_id: &str,
        input: &ResolutionInput,
        now: &str,
    ) -> Result<ResolutionRecord, WorkspaceError> {
        const RESULTS: &[&str] = &["mutual_win", "one_sided", "no_deal"];
        const BENEFITS: &[&str] = &["yes", "partial", "no", "unknown"];
        if !RESULTS.contains(&input.result.as_str())
            || !BENEFITS.contains(&input.they_benefited.as_str())
            || !BENEFITS.contains(&input.i_benefited.as_str())
        {
            return Err(WorkspaceError::Invalid(
                "resolution requires a result and both benefit assessments".into(),
            ));
        }
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM opportunity WHERE id=?")
            .bind(opportunity_id)
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Err(WorkspaceError::Invalid("opportunity does not exist".into()));
        }
        let thread_id = new_id(opportunity_id);
        sqlx::query(
            "INSERT INTO resolution(id,opportunity_id,thread_id,result,they_benefited,i_benefited,noted_at) VALUES(?,?,?,?,?,?,?)",
        )
        .bind(&thread_id)
        .bind(opportunity_id)
        .bind(Option::<String>::None)
        .bind(&input.result)
        .bind(&input.they_benefited)
        .bind(&input.i_benefited)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE contact_thread SET state='resolved',expires_at=NULL,updated_at=? WHERE opportunity_id=? AND state IN ('waiting','conversing','timed_out')",
        )
        .bind(now)
        .bind(opportunity_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE opportunity SET state='resolved',close_reason='resolved',revision=revision+1,updated_at=? WHERE id=?",
        )
        .bind(now)
        .bind(opportunity_id)
        .execute(&mut *tx)
        .await?;
        let revision: i64 = sqlx::query_scalar("SELECT revision FROM opportunity WHERE id=?")
            .bind(opportunity_id)
            .fetch_one(&mut *tx)
            .await?;
        append_event(
            &mut tx,
            "opportunity",
            opportunity_id,
            revision,
            "opportunity.resolved",
            json!({"result": input.result, "resolution_id": thread_id}),
        )
        .await?;
        tx.commit().await?;
        Ok(ResolutionRecord {
            id: thread_id,
            opportunity_id: opportunity_id.to_owned(),
            thread_id: None,
            result: input.result.clone(),
            they_benefited: input.they_benefited.clone(),
            i_benefited: input.i_benefited.clone(),
            noted_at: now.to_owned(),
        })
    }

    pub async fn list_capacity(&self) -> Result<Vec<CapacityRecord>, WorkspaceError> {
        let rows = sqlx::query_as::<_, (String, String, f64, f64, String)>(
            "SELECT period,unit,total,committed,updated_at FROM capacity_ledger ORDER BY period,unit",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(period, unit, total, committed, updated_at)| CapacityRecord {
                    period,
                    unit,
                    total,
                    committed,
                    remaining: total - committed,
                    updated_at,
                },
            )
            .collect())
    }

    pub(crate) async fn set_capacity(
        &self,
        input: &CapacityInput,
        now: &str,
    ) -> Result<CapacityRecord, WorkspaceError> {
        if input.period.trim().is_empty()
            || input.unit.trim().is_empty()
            || !input.total.is_finite()
            || input.total < 0.0
        {
            return Err(WorkspaceError::Invalid(
                "capacity requires a period, a unit and a non-negative total".into(),
            ));
        }
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT INTO capacity_ledger(period,unit,total,committed,updated_at) VALUES(?,?,?,0,?) ON CONFLICT(period,unit) DO UPDATE SET total=excluded.total,committed=0,updated_at=excluded.updated_at",
        )
        .bind(input.period.trim())
        .bind(input.unit.trim())
        .bind(input.total)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        append_event(
            &mut tx,
            "capacity",
            format!("{}:{}", input.period.trim(), input.unit.trim()).as_str(),
            1,
            "capacity.provisioned",
            json!({"period": input.period.trim(), "unit": input.unit.trim(), "total": input.total}),
        )
        .await?;
        tx.commit().await?;
        Ok(CapacityRecord {
            period: input.period.trim().to_owned(),
            unit: input.unit.trim().to_owned(),
            total: input.total,
            committed: 0.0,
            remaining: input.total,
            updated_at: now.to_owned(),
        })
    }
}

async fn verify_release(
    tx: &mut Transaction<'_, Sqlite>,
    release_id: &str,
    material: &super::release::HeadMaterial,
) -> Result<String, WorkspaceError> {
    let row: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT content_sha256,material_id,version_no FROM release_record WHERE id=?",
    )
    .bind(release_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((sha, material_id, version)) = row else {
        return Err(WorkspaceError::Invalid(
            "release grant does not exist".into(),
        ));
    };
    if material_id != material.id || version != material.version {
        return Err(WorkspaceError::Invalid(
            "release grant does not match this material version".into(),
        ));
    }
    Ok(sha)
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

async fn list_threads(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ThreadQuery>,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state
        .workspace
        .list_threads(
            query.opportunity_id.as_deref(),
            query.recipient_id.as_deref(),
        )
        .await
    {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn mark_sent(
    State(state): State<AppState>,
    Path(thread_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<SendInput>,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    let now = now_utc();
    if let Err(error) = state.workspace.reconcile_deadlines(&now).await {
        return workspace_error(error);
    }
    match state.workspace.mark_sent(&thread_id, &input, &now).await {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn record_outcome(
    State(state): State<AppState>,
    Path(send_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<OutcomeInput>,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    let now = now_utc();
    if let Err(error) = state.workspace.reconcile_deadlines(&now).await {
        return workspace_error(error);
    }
    match state.workspace.record_outcome(&send_id, &input, &now).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn clear_party_dnc(
    State(state): State<AppState>,
    Path(party_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ClearDncInput>,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.clear_dnc(&party_id, &input.reason).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn reopen_thread(
    State(state): State<AppState>,
    Path(thread_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    let now = now_utc();
    match state.workspace.reopen_thread(&thread_id, &now).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn record_resolution(
    State(state): State<AppState>,
    Path(opportunity_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ResolutionInput>,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    let now = now_utc();
    if let Err(error) = state.workspace.reconcile_deadlines(&now).await {
        return workspace_error(error);
    }
    match state
        .workspace
        .record_resolution(&opportunity_id, &input, &now)
        .await
    {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn list_capacity(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    match state.workspace.list_capacity().await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}

async fn set_capacity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CapacityInput>,
) -> Response {
    if let Err(error) = authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    let now = now_utc();
    match state.workspace.set_capacity(&input, &now).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => workspace_error(error),
    }
}
