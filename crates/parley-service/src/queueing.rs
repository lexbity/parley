use super::{AppState, authenticate, digest, now_utc};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/queue", get(queue))
        .route("/api/v1/insights", get(insights))
        .route("/api/v1/coach/questions", get(questions))
        .route("/api/v1/coach/context", post(generate_context_coach))
        .route("/api/v1/coach/questions/{id}/answer", post(answer_question))
        .route(
            "/api/v1/coach/questions/{id}/dismiss",
            post(dismiss_question),
        )
        .route("/api/v1/coach/questions/{id}/defer", post(defer_question))
        .route(
            "/api/v1/coach/weekly",
            get(get_coach_report).post(generate_coach_report),
        )
        .route("/api/v1/identity-tags", post(save_identity_tag))
        .route("/api/v1/claims/{id}/hook", post(save_hook))
}

#[allow(clippy::type_complexity)]
async fn queue(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<super::queue_sheet::SheetQuery>,
) -> Response {
    if let Err(r) = authenticate(&headers, &state).await {
        return r.into_response();
    }
    // The daily queue remains the default for legacy callers; the Queue sheet
    // (Parley-Queue spec) is requested explicitly with view=sheet/list.
    if matches!(query.view.as_deref(), Some("sheet") | Some("list")) {
        return super::queue_sheet::sheet_query(&state, &headers, query).await;
    }
    let mut tx = match state.workspace.pool().begin().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("S10 queue DB: {e:?}");
            return super::command_error(e.into());
        }
    };
    let cursor: i64 = match sqlx::query_scalar("SELECT coalesce(max(cursor),0) FROM event")
        .fetch_one(&mut *tx)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("S10 queue DB: {e:?}");
            return super::command_error(e.into());
        }
    };
    let rows:Vec<(String,String,String,String,String,i64,i64,i64,Option<String>,Option<String>)>=match sqlx::query_as("SELECT o.id,o.title,o.state,op.party_id,e.name,coalesce(pp.do_not_contact,0),coalesce(pp.touch_count,0),coalesce(pp.highest_rung,0),t.next_action_at,(SELECT m.shortlist_json FROM match_run m WHERE m.opportunity_id=o.id AND m.recipient_id=op.party_id AND m.opportunity_revision=o.revision AND m.recipient_revision=e.revision ORDER BY m.created_at DESC,m.id DESC LIMIT 1) FROM opportunity o JOIN opportunity_party op ON op.opportunity_id=o.id JOIN entity e ON e.id=op.party_id LEFT JOIN party_policy pp ON pp.party_id=e.id LEFT JOIN contact_thread t ON t.id=(SELECT t2.id FROM contact_thread t2 WHERE t2.opportunity_id=o.id AND t2.recipient_id=e.id ORDER BY CASE t2.state WHEN 'waiting' THEN 0 WHEN 'conversation' THEN 1 ELSE 2 END,t2.created_at DESC,t2.id DESC LIMIT 1) WHERE o.state='open' ORDER BY o.id,op.party_id").fetch_all(&mut *tx).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
    let policy: String =
        match sqlx::query_scalar("SELECT settings_json FROM owner_profile WHERE id=1")
            .fetch_one(&mut *tx)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("S10 queue DB: {e:?}");
                return super::command_error(e.into());
            }
        };
    let policy: serde_json::Value = serde_json::from_str(&policy).unwrap_or_default();
    let cap = policy
        .pointer("/pack_policy/limits/daily_send_cap")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let max_touches = policy
        .pointer("/pack_policy/cadence/max_touches")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let hook_days = policy
        .pointer("/pack_policy/limits/hook_max_age_days")
        .and_then(|v| v.as_i64())
        .unwrap_or(90);
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(hook_days)).to_rfc3339();
    let fresh_hooks:std::collections::HashSet<String>=match sqlx::query_scalar("SELECT DISTINCT c.subject_id FROM claim c JOIN accepted_hook h ON h.claim_id=c.id AND h.claim_revision=c.current_revision WHERE h.published_at>=? AND h.publicness='professional_public'").bind(cutoff).fetch_all(&mut *tx).await{Ok(v)=>v.into_iter().collect(),Err(e)=>return super::command_error(e.into())};
    let shared_identity:std::collections::HashSet<String>=match sqlx::query_scalar("SELECT DISTINCT p.entity_id FROM party_identity_tag p JOIN owner_identity_tag o ON o.tag=p.tag JOIN claim_revision r ON r.claim_id=p.claim_id AND r.revision=p.claim_revision WHERE r.tier IN (1,3) AND r.approved_wording IS NOT NULL AND r.knowledge_kind<>'hypothesis'").fetch_all(&mut *tx).await{Ok(v)=>v.into_iter().collect(),Err(e)=>return super::command_error(e.into())};
    let mut items = Vec::new();
    let capacity_period = policy
        .pointer("/pack_policy/scarcity/period")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let capacity_unit = policy
        .pointer("/pack_policy/scarcity/unit")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let capacity_remaining: f64 = match sqlx::query_scalar(
        "SELECT CAST(coalesce(max(total-committed),0) AS REAL) FROM capacity_ledger WHERE period=? AND unit=?",
    )
    .bind(capacity_period)
    .bind(capacity_unit)
    .fetch_one(&mut *tx)
    .await
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("S10 queue DB: {e:?}");
            return super::command_error(e.into());
        }
    };
    for (oid, title, _state, pid, name, dnc, touches, highest_rung, due, shortlist) in rows {
        let mut reasons = Vec::new();
        if dnc != 0 {
            reasons.push("party do not contact".to_owned())
        }
        if touches >= max_touches {
            reasons.push("maximum touches reached".to_owned())
        }
        let kind = if !reasons.is_empty() {
            "blocked"
        } else if due.is_some() {
            "followup_due"
        } else {
            "needs_input"
        };
        let shortlist: serde_json::Value = shortlist
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let matched = |kind: &str| {
            shortlist
                .get("results")
                .and_then(|r| r.as_array())
                .is_some_and(|rs| {
                    rs.iter()
                        .any(|r| r["kind"] == kind && r["no_match"] == false)
                })
        };
        let capacity_ready = policy
            .pointer("/pack_policy/scarcity/enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && capacity_remaining > 0.0;
        let rung_ready = highest_rung
            < policy
                .pointer("/pack_policy/ask_ladder/max_rung")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
        let ammo = u8::from(matched("gift"))
            + u8::from(matched("proof"))
            + u8::from(matched("artifact"))
            + u8::from(fresh_hooks.contains(&pid))
            + u8::from(shared_identity.contains(&pid))
            + u8::from(rung_ready)
            + u8::from(capacity_ready);
        items.push(json!({"id":format!("{oid}:{pid}"),"opportunity_id":oid,"party_id":pid,"title":title,"recipient_name":name,"kind":kind,"due_at":due,"blocked_reasons":reasons,"ammo_score":ammo}));
    }
    let _ = tx.commit().await;
    let suggested_send_count = items
        .iter()
        .filter(|i| i["kind"] == "ready_to_send" || i["kind"] == "followup_due")
        .count()
        .min(cap);
    let actionable = items
        .iter()
        .filter(|i| i["kind"] != "blocked")
        .cloned()
        .collect::<Vec<_>>();
    let blocked = items
        .iter()
        .filter(|i| i["kind"] == "blocked")
        .cloned()
        .collect::<Vec<_>>();
    let response = json!({"event_cursor":cursor.to_string(),"actionable":actionable,"blocked":blocked,"suggested_send_count":suggested_send_count});
    let mut r = Json(response).into_response();
    super::private_headers(&mut r, state.workspace.epoch());
    r
}

async fn insights(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = authenticate(&headers, &state).await {
        return r.into_response();
    }
    // One read transaction gives outcome history and its send-time segment
    // attribution a single consistent fence. Corrections select the latest
    // append-only revision; the original send snapshot remains untouched.
    let mut tx = match state.workspace.pool().begin().await {
        Ok(tx) => tx,
        Err(_) => {
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "READ_FAILED",
                "Insights snapshot could not be completed",
            );
        }
    };
    let rows: Vec<(String, String, String)> = match sqlx::query_as(
        "SELECT s.id, coalesce(r.class,'no_outcome'), s.segment_ids_snapshot_json
         FROM send s LEFT JOIN outcome_revision r ON r.send_id=s.id
         AND r.revision=(SELECT max(r2.revision) FROM outcome_revision r2 WHERE r2.send_id=s.id)
         WHERE s.touch_no=1 ORDER BY s.id",
    )
    .fetch_all(&mut *tx)
    .await
    {
        Ok(v) => v,
        Err(_) => {
            return super::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "READ_FAILED",
                "Insights history could not be read",
            );
        }
    };
    let outcomes: Vec<(String,)> = match sqlx::query_as(
        "SELECT r.class FROM outcome_revision r WHERE r.revision=(SELECT max(r2.revision) FROM outcome_revision r2 WHERE r2.send_id=r.send_id) ORDER BY r.class",
    )
    .fetch_all(&mut *tx)
    .await
    {
        Ok(v) => v,
        Err(_) => return super::error(StatusCode::SERVICE_UNAVAILABLE, "READ_FAILED", "Insights history could not be read"),
    };
    let mut counts = std::collections::BTreeMap::<String, u64>::new();
    for (class,) in outcomes {
        *counts.entry(class).or_default() += 1;
    }
    let n = rows.len() as u64;
    let replies = rows
        .iter()
        .filter(|(_, class, _)| class != "no_outcome" && class != "no_reply")
        .count() as u64;
    let advances = rows
        .iter()
        .filter(|(_, class, _)| matches!(class.as_str(), "rung_accepted" | "call_scheduled"))
        .count() as u64;
    let interval = |k: u64, n: u64| {
        parley_domain::analytics::wilson_interval(k, n)
            .map(|v| json!({"lower_95":v.lower_95,"upper_95":v.upper_95}))
    };
    let mut strata = std::collections::BTreeMap::<String, (u64, u64)>::new();
    for (_, class, raw_segments) in &rows {
        let ids: Vec<String> = serde_json::from_str(raw_segments).unwrap_or_default();
        for segment_id in ids {
            let pair = strata.entry(segment_id).or_default();
            pair.1 += 1;
            if class != "no_outcome" && class != "no_reply" {
                pair.0 += 1;
            }
        }
    }
    let strata: Vec<Value> = strata
        .into_iter()
        .map(|(segment_id, (k, n))| {
            json!({
                "segment_id":segment_id,"reply_rate":{"k":k,"n":n,"interval_95":interval(k,n)}
            })
        })
        .collect();
    let body = json!({
        "as_of": now_utc(),
        "event_cursor": sqlx::query_scalar::<_,i64>("SELECT coalesce(max(cursor),0) FROM event").fetch_one(&mut *tx).await.unwrap_or(0).to_string(),
        "effective_outcomes":counts.into_iter().map(|(class,count)|json!({"class":class,"count":count})).collect::<Vec<_>>(),
        "rates":[
            {"metric":"reply_rate","k":replies,"n":n,"interval_95":interval(replies,n)},
            {"metric":"advance_rate","k":advances,"n":n,"interval_95":interval(advances,n)}
        ],
        "strata":strata,
        "comparisons":[]
    });
    if tx.commit().await.is_err() {
        return super::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "READ_FAILED",
            "insights snapshot could not be completed",
        );
    }
    let mut r = Json(body).into_response();
    super::private_headers(&mut r, state.workspace.epoch());
    r
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoachQuestionFilter {
    scope_kind: Option<String>,
    scope_id: Option<String>,
    scope_period: Option<String>,
    recipient_id: Option<String>,
}

type CoachQuestionRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
);

async fn questions(
    State(state): State<AppState>,
    Query(filter): Query<CoachQuestionFilter>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = authenticate(&headers, &state).await {
        return r.into_response();
    }
    let selector_kind = filter.scope_kind.unwrap_or_else(|| "weekly".to_owned());
    if !matches!(selector_kind.as_str(), "weekly" | "party" | "opportunity") {
        return super::error(
            StatusCode::BAD_REQUEST,
            "SCOPE_INVALID",
            "Choose a supported Coach scope",
        );
    }
    if selector_kind != "weekly" && filter.scope_id.as_deref().is_none_or(str::is_empty) {
        return super::error(
            StatusCode::BAD_REQUEST,
            "SCOPE_INVALID",
            "A selected scope ID is required",
        );
    }
    let rows: Vec<CoachQuestionRow> = match sqlx::query_as(
        "SELECT id,question_key,kind,target_json,text,priority,impact,selector_json,status,revision,deferred_until,dependency_digest,suggested_resource_id
         FROM coach_question WHERE status IN ('open','answer_pending','deferred')
         AND json_extract(selector_json,'$.kind')=?
         AND (? IS NULL OR json_extract(selector_json,'$.id')=?)
         AND (? IS NULL OR json_extract(selector_json,'$.period')=?)
         AND (? IS NULL OR json_extract(selector_json,'$.recipient_id')=?)
         AND (status<>'deferred' OR deferred_until IS NULL OR deferred_until<=?)
         ORDER BY priority,impact DESC,id LIMIT 100")
        .bind(&selector_kind).bind(&filter.scope_id).bind(&filter.scope_id).bind(&filter.scope_period).bind(&filter.scope_period).bind(&filter.recipient_id).bind(&filter.recipient_id).bind(now_utc())
        .fetch_all(state.workspace.pool()).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
    let open_count = rows
        .iter()
        .filter(|row| row.8 == "open" || row.8 == "answer_pending")
        .count();
    let body = json!({"items":rows.into_iter().map(|(id,question_key,kind,target,text,priority,impact,selector,status,revision,deferred_until,dependency_digest,suggested_resource_ref)|json!({
        "id":id,"question_key":question_key,"kind":kind,"target":serde_json::from_str::<serde_json::Value>(&target).unwrap_or_default(),"text":text,
        "priority":priority,"impact":impact,"selector":serde_json::from_str::<serde_json::Value>(&selector).unwrap_or_default(),
        "status":status,"revision":revision,"deferred_until":deferred_until,"dependency_digest":dependency_digest,"suggested_resource_ref":suggested_resource_ref,
        "action":if kind=="missing_proof"||kind=="missing_gift"||kind=="missing_artifact" {"interviewer"} else {"research"}
    })).collect::<Vec<_>>(),"open_count":open_count,"max_open":5});
    let mut r = Json(body).into_response();
    super::private_headers(&mut r, state.workspace.epoch());
    r
}

async fn coach_questions_for_scope(
    conn: &mut sqlx::SqliteConnection,
    scope: &Value,
) -> Result<Vec<Value>, sqlx::Error> {
    let kind = scope["kind"].as_str().unwrap_or_default();
    let id = scope["id"].as_str();
    let recipient = scope["recipient_id"].as_str();
    let rows: Vec<CoachQuestionRow> = sqlx::query_as(
        "SELECT id,question_key,kind,target_json,text,priority,impact,selector_json,status,revision,deferred_until,dependency_digest,suggested_resource_id
         FROM coach_question WHERE status IN ('open','answer_pending','deferred')
         AND json_extract(selector_json,'$.kind')=?
         AND (? IS NULL OR json_extract(selector_json,'$.id')=?)
         AND (? IS NULL OR json_extract(selector_json,'$.recipient_id')=?)
         AND (status<>'deferred' OR deferred_until IS NULL OR deferred_until<=?)
         ORDER BY priority,impact DESC,id LIMIT 100")
        .bind(kind).bind(id).bind(id).bind(recipient).bind(recipient).bind(now_utc())
        .fetch_all(&mut *conn).await?;
    Ok(rows.into_iter().map(|(id,question_key,kind,target,text,priority,impact,selector,status,revision,deferred_until,dependency_digest,suggested_resource_ref)|json!({
        "id":id,"question_key":question_key,"kind":kind,"target":serde_json::from_str::<Value>(&target).unwrap_or_default(),"text":text,
        "priority":priority,"impact":impact,"selector":serde_json::from_str::<Value>(&selector).unwrap_or_default(),
        "status":status,"revision":revision,"deferred_until":deferred_until,"dependency_digest":dependency_digest,"suggested_resource_ref":suggested_resource_ref,
        "action":if kind=="missing_proof"||kind=="missing_gift"||kind=="missing_artifact" {"interviewer"} else {"research"}
    })).collect())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoachDisposition {
    answer: serde_json::Value,
    interviewer_chat_id: Option<String>,
    expected_revision: i64,
    question_key: String,
}
fn matches_schema(value: &serde_json::Value, schema: &serde_json::Value) -> bool {
    if let Some(kind) = schema["type"].as_str() {
        if kind == "object" && !value.is_object() {
            return false;
        }
        if kind == "string" && !value.is_string() {
            return false;
        }
        if kind == "number" && !value.is_number() {
            return false;
        }
        if kind == "boolean" && !value.is_boolean() {
            return false;
        }
    }
    if let Some(properties) = schema["properties"].as_object() {
        let Some(object) = value.as_object() else {
            return false;
        };
        if object.keys().any(|k| !properties.contains_key(k)) {
            return false;
        }
        if let Some(required) = schema["required"].as_array()
            && required
                .iter()
                .filter_map(|v| v.as_str())
                .any(|k| !object.contains_key(k))
        {
            return false;
        }
        for (key, val) in object {
            let rule = &properties[key];
            if !matches_schema(val, rule) {
                return false;
            }
            if let Some(options) = rule["enum"].as_array()
                && !options.contains(val)
            {
                return false;
            }
        }
    }
    true
}
async fn answer_question(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<CoachDisposition>,
) -> Response {
    if let Err(r) = authenticate(&headers, &state).await {
        return r.into_response();
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
    let canonical = digest(
        format!(
            "coach-answer\\0{id}\\0{}\\0{}\\0{}\\0{:?}",
            input.question_key, input.expected_revision, input.answer, input.interviewer_chat_id
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
    let row: Option<(String, String, String, i64, String)> = match sqlx::query_as(
        "SELECT kind,answer_schema_json,status,revision,question_key FROM coach_question WHERE id=?",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    let Some((kind, schema, status, revision, question_key)) = row else {
        return super::error(
            StatusCode::NOT_FOUND,
            "QUESTION_NOT_FOUND",
            "Coach question does not exist",
        );
    };
    if revision != input.expected_revision || question_key != input.question_key {
        return super::error(
            StatusCode::CONFLICT,
            "QUESTION_STALE",
            "Refresh this question before recording an answer",
        );
    }
    if status != "open" {
        return super::error(
            StatusCode::CONFLICT,
            "QUESTION_CLOSED",
            "Coach question is no longer open",
        );
    }
    if matches!(
        kind.as_str(),
        "missing_proof" | "missing_gift" | "missing_artifact" | "missing_identity"
    ) {
        let asset_kind = match kind.as_str() {
            "missing_proof" => "proof",
            "missing_gift" => "gift",
            "missing_artifact" => "artifact",
            _ => {
                return super::error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "IDENTITY_INTERVIEW_UNAVAILABLE",
                    "Identity tags require the typed identity editor",
                );
            }
        };
        let chat_id = input.interviewer_chat_id.clone().unwrap_or_else(|| {
            super::new_id(format!("coach-answer:{id}:{}", input.question_key).as_str())
        });
        let text = input
            .answer
            .as_str()
            .or_else(|| input.answer.get("text").and_then(|v| v.as_str()))
            .unwrap_or("")
            .trim();
        if text.is_empty() || text.len() > 12000 {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "ANSWER_INVALID",
                "Interviewer answer needs 1–12,000 bytes",
            );
        }
        // One typed owner-turn intake: it creates the owner Interviewer chat
        // when the answer does not target an existing one, records the turn
        // and enqueues exactly one role job (AR-001 AR04).
        let outcome = match crate::application::interview::start_owner_turn(
            &mut tx,
            &crate::application::interview::OwnerTurnInput {
                chat_id: chat_id.clone(),
                text: text.to_owned(),
                epoch: state.workspace.epoch().to_owned(),
                create_chat_if_missing: input.interviewer_chat_id.is_none(),
                asset_kind: Some(asset_kind.to_owned()),
                coach_question_id: Some(id.clone()),
            },
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(error) => return interview_answer_error(error),
        };
        let job = outcome.job_id;
        let chat_id = outcome.chat_id;
        let now = now_utc();
        let disposition = json!({"answer":input.answer,"interviewer_chat_id":chat_id,"job_id":job});
        if let Err(e)=sqlx::query("UPDATE coach_question SET status='answer_pending',disposition_json=?,revision=revision+1,updated_at=? WHERE id=? AND revision=? AND status='open'").bind(disposition.to_string()).bind(&now).bind(&id).bind(input.expected_revision).execute(&mut *tx).await{return super::command_error(e.into())};
        let result = json!({"id":id,"status":"answer_pending","chat_id":chat_id,"job_id":job,"revision":input.expected_revision+1});
        let cursor = match super::assistant::append_event(
            &mut tx,
            &id,
            "coach.question_answered",
            result.clone(),
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return super::materials::workspace_error(e),
        };
        if let Err(e) = super::assistant::write_receipt(
            &mut tx,
            state.workspace.epoch(),
            &key,
            &canonical,
            202,
            &result,
            cursor,
        )
        .await
        {
            return super::materials::workspace_error(e);
        }
        if let Err(e) = tx.commit().await {
            return super::command_error(e.into());
        };
        state.jobs.notify_one();
        let mut response = (StatusCode::ACCEPTED, Json(result)).into_response();
        super::private_headers(&mut response, state.workspace.epoch());
        return response;
    }
    let schema: serde_json::Value = serde_json::from_str(&schema).unwrap_or_default();
    if !matches_schema(&input.answer, &schema) {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "ANSWER_SCHEMA_INVALID",
            "Answer does not match this question's declared schema",
        );
    }
    let now = now_utc();
    if let Err(e)=sqlx::query("UPDATE coach_question SET status='answered',disposition_json=?,revision=revision+1,updated_at=? WHERE id=? AND revision=? AND status='open'").bind(input.answer.to_string()).bind(&now).bind(&id).bind(input.expected_revision).execute(&mut *tx).await{return super::command_error(e.into())};
    let result = json!({"id":id,"status":"answered","answer":input.answer,"recorded_at":now,"revision":input.expected_revision+1});
    let cursor = match super::assistant::append_event(
        &mut tx,
        &id,
        "coach.question_answered",
        result.clone(),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return super::materials::workspace_error(e),
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
        return super::materials::workspace_error(e);
    }
    if let Err(e) = tx.commit().await {
        return super::command_error(e.into());
    };
    let mut r = Json(result).into_response();
    super::private_headers(&mut r, state.workspace.epoch());
    r
}

fn interview_answer_error(error: crate::application::interview::InterviewError) -> Response {
    use axum::response::IntoResponse as _;
    match error {
        crate::application::interview::InterviewError::NotFound => {
            let response = super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INTERVIEWER_SCOPE_INVALID",
                "Choose an owner Interviewer chat",
            );
            response.into_response()
        }
        crate::application::interview::InterviewError::InvalidKind => super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INTERVIEWER_ASSET_KIND_MISMATCH",
            "Interviewer chat must match the Coach question resource kind",
        ),
        crate::application::interview::InterviewError::StaleRevision => super::error(
            StatusCode::CONFLICT,
            "QUESTION_STALE",
            "Refresh this question before recording an answer",
        ),
        crate::application::interview::InterviewError::InvalidTurn => super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "ANSWER_INVALID",
            "Interviewer answer needs 1–12,000 bytes",
        ),
        crate::application::interview::InterviewError::BankMissing => super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "QUESTION_BANK_MISSING",
            "Interviewer question bank is unavailable",
        ),
        crate::application::interview::InterviewError::QueueAtCapacity
        | crate::application::interview::InterviewError::InferenceBusy => {
            let mut response = super::error(
                StatusCode::TOO_MANY_REQUESTS,
                "INFERENCE_BUSY",
                "The local inference queue is full",
            );
            response
                .headers_mut()
                .insert("retry-after", axum::http::HeaderValue::from_static("2"));
            response.into_response()
        }
        crate::application::interview::InterviewError::Database => super::command_error(
            crate::WorkspaceError::Invalid("interview intake failed".into()),
        ),
    }
}

async fn dismiss_question(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<CoachQuestionRevision>,
) -> Response {
    if let Err(r) = authenticate(&headers, &state).await {
        return r.into_response();
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
    let canonical = digest(
        format!(
            "coach-dismiss\\0{id}\\0{}\\0{}",
            input.question_key, input.expected_revision
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
    };
    let current: Option<(String, i64, String)> =
        match sqlx::query_as("SELECT question_key,revision,status FROM coach_question WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(value) => value,
            Err(error) => return super::command_error(error.into()),
        };
    let Some((question_key, revision, status)) = current else {
        return super::error(
            StatusCode::NOT_FOUND,
            "QUESTION_NOT_FOUND",
            "Coach question does not exist",
        );
    };
    if question_key != input.question_key || revision != input.expected_revision || status != "open"
    {
        return super::error(
            StatusCode::CONFLICT,
            "QUESTION_STALE",
            "Refresh this question before dismissing it",
        );
    }
    let changed = match sqlx::query(
        "UPDATE coach_question SET status='dismissed',revision=revision+1,updated_at=? WHERE id=? AND status='open' AND revision=?",
    ).bind(now_utc()).bind(&id).bind(input.expected_revision).execute(&mut *tx).await {
        Ok(v) => v.rows_affected(),
        Err(e) => return super::command_error(e.into()),
    };
    if changed == 0 {
        return super::error(
            StatusCode::CONFLICT,
            "QUESTION_CLOSED",
            "Question is absent or already closed",
        );
    };
    let result = json!({"id":id,"status":"dismissed","revision":input.expected_revision+1});
    let cursor = match super::assistant::append_event(
        &mut tx,
        &id,
        "coach.question_dismissed",
        result.clone(),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return super::materials::workspace_error(e),
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
        return super::materials::workspace_error(e);
    }
    if let Err(e) = tx.commit().await {
        return super::command_error(e.into());
    };
    let mut r = Json(result).into_response();
    super::private_headers(&mut r, state.workspace.epoch());
    r
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoachQuestionRevision {
    expected_revision: i64,
    question_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoachQuestionDefer {
    expected_revision: i64,
    question_key: String,
    defer_days: Option<u8>,
}

async fn defer_question(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<CoachQuestionDefer>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.trim().is_empty() && v.len() <= 200)
    else {
        return super::error(
            StatusCode::BAD_REQUEST,
            "IDEMPOTENCY_KEY_REQUIRED",
            "Idempotency-Key is required",
        );
    };
    let days = input.defer_days.unwrap_or(7);
    if days == 0 || days > 30 {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "DEFER_RANGE_INVALID",
            "Choose a defer period from one to thirty days",
        );
    }
    let canonical = match super::application::coach::fingerprint(
        &json!({"operation":"deferCoachQuestion","id":id,"question_key":input.question_key,"revision":input.expected_revision,"days":days}),
    ) {
        Ok(v) => v,
        Err(e) => return super::command_error(e),
    };
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(e) => return super::command_error(e.into()),
    };
    if let Some(response) =
        super::assistant::receipt_response(&mut tx, state.workspace.epoch(), key, &canonical).await
    {
        return response;
    }
    let row: Option<(String, i64, String)> =
        match sqlx::query_as("SELECT question_key,revision,status FROM coach_question WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(e) => return super::command_error(e.into()),
        };
    let Some((question_key, revision, status)) = row else {
        return super::error(
            StatusCode::NOT_FOUND,
            "QUESTION_NOT_FOUND",
            "Coach question does not exist",
        );
    };
    if question_key != input.question_key || revision != input.expected_revision || status != "open"
    {
        return super::error(
            StatusCode::CONFLICT,
            "QUESTION_STALE",
            "Refresh this question before deferring it",
        );
    }
    let zone: String = match sqlx::query_scalar("SELECT timezone FROM owner_profile WHERE id=1")
        .fetch_one(&mut *tx)
        .await
    {
        Ok(value) => value,
        Err(e) => return super::command_error(e.into()),
    };
    let Some(zone) = zone.parse::<chrono_tz::Tz>().ok() else {
        return super::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "TIMEZONE_INVALID",
            "Owner timezone settings are invalid",
        );
    };
    let day = chrono::Utc::now().with_timezone(&zone).date_naive()
        + chrono::Duration::days(i64::from(days));
    let Some(defer_until) = day
        .and_hms_opt(0, 0, 0)
        .and_then(|local| local.and_local_timezone(zone).earliest())
        .map(|value| {
            value
                .with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
    else {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "DEFER_RANGE_INVALID",
            "The selected date cannot be represented in the owner timezone",
        );
    };
    if let Err(e)=sqlx::query("UPDATE coach_question SET status='deferred',deferred_until=?,revision=revision+1,updated_at=? WHERE id=? AND revision=? AND status='open'").bind(&defer_until).bind(now_utc()).bind(&id).bind(input.expected_revision).execute(&mut *tx).await {return super::command_error(e.into());}
    let result = json!({"id":id,"status":"deferred","deferred_until":defer_until,"revision":input.expected_revision+1});
    let cursor = match super::assistant::append_event(
        &mut tx,
        &id,
        "coach.question_deferred",
        result.clone(),
    )
    .await
    {
        Ok(cursor) => cursor,
        Err(e) => return super::materials::workspace_error(e),
    };
    if let Err(e) = super::assistant::write_receipt(
        &mut tx,
        state.workspace.epoch(),
        key,
        &canonical,
        200,
        &result,
        cursor,
    )
    .await
    {
        return super::materials::workspace_error(e);
    }
    if let Err(e) = tx.commit().await {
        return super::command_error(e.into());
    }
    let mut response = Json(result).into_response();
    super::private_headers(&mut response, state.workspace.epoch());
    response
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CoachPeriod {
    #[serde(skip_serializing_if = "Option::is_none")]
    period: Option<String>,
}

async fn get_coach_report(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = authenticate(&headers, &state).await {
        return r.into_response();
    }
    let mut tx = match state.workspace.pool().begin().await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    let result = async {
        let week = coach_week(&mut tx, None, chrono::Utc::now()).await?;
        let plan = coach_plan(&mut tx, &week).await?;
        coach_snapshot(&mut tx, state.workspace.epoch(), &week, &plan).await
    }
    .await;
    match result {
        Ok(body) => coach_response(StatusCode::OK, body, state.workspace.epoch()),
        Err(response) => response,
    }
}

#[allow(clippy::result_large_err)] // Keeps response construction at the HTTP boundary.
async fn coach_week(
    conn: &mut sqlx::SqliteConnection,
    period: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<super::application::coach::Week, Response> {
    use super::application::coach::{PeriodError, resolve_week};
    resolve_week(conn, period, now)
        .await
        .map_err(|error| match error {
            PeriodError::Invalid => super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "PERIOD_INVALID",
                "Choose an available ISO week in the owner timezone",
            ),
            PeriodError::Database(error) => super::command_error(error.into()),
        })
}

#[allow(clippy::result_large_err)] // Keeps response construction at the HTTP boundary.
async fn coach_plan(
    conn: &mut sqlx::SqliteConnection,
    week: &super::application::coach::Week,
) -> Result<super::application::coach::Plan, Response> {
    match tokio::time::timeout(
        std::time::Duration::from_millis(500),
        super::application::coach::plan(conn, week),
    )
    .await
    {
        Err(_) => Err(coach_context_budget()),
        Ok(Err(super::WorkspaceError::Invalid(code))) if code == "CONTEXT_BUDGET" => {
            Err(coach_context_budget())
        }
        Ok(Err(error)) => Err(super::command_error(error)),
        Ok(Ok(plan)) => Ok(plan),
    }
}

fn coach_context_budget() -> Response {
    super::error(
        StatusCode::UNPROCESSABLE_ENTITY,
        "CONTEXT_BUDGET",
        "Choose a narrower period for the Coach assessment",
    )
}

#[allow(clippy::result_large_err)] // Keeps response construction at the HTTP boundary.
async fn coach_snapshot(
    conn: &mut sqlx::SqliteConnection,
    epoch: &str,
    week: &super::application::coach::Week,
    plan: &super::application::coach::Plan,
) -> Result<Value, Response> {
    let report: Option<(String, String)> = sqlx::query_as(
        "SELECT report_json,input_digest FROM coach_report WHERE period=?
         ORDER BY created_at DESC,id DESC LIMIT 1",
    )
    .bind(&week.period)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|error| super::command_error(error.into()))?;
    let jobs: Vec<(String, String)> = sqlx::query_as(
        "SELECT id,status FROM durable_job WHERE epoch=? AND role='coach' AND scope_id=?
         ORDER BY (status IN ('queued','running')) DESC,created_at DESC,id DESC LIMIT 1",
    )
    .bind(epoch)
    .bind(&week.period)
    .fetch_all(conn)
    .await
    .map_err(|error| super::command_error(error.into()))?;
    let latest = jobs.first();
    let report_body = report
        .as_ref()
        .and_then(|(body, _)| serde_json::from_str::<Value>(body).ok());
    Ok(json!({
        "report": report_body, "period": week.period, "as_of": week.as_of,
        "coverage": plan.coverage, "input_digest": report.map(|(_, digest)| digest),
        "active_job_id": latest.filter(|(_, status)| matches!(status.as_str(), "queued" | "running")).map(|(id, _)| id),
        "latest_job_id": latest.map(|(id, _)| id),
        "observations":plan.seeds.iter().take(5).map(|seed| json!({"reason":seed.reason.kind(),"target":seed.target.wire(),"impact":seed.impact})).collect::<Vec<_>>()
    }))
}

fn coach_response(status: StatusCode, value: Value, epoch: &str) -> Response {
    let mut response = (status, Json(value)).into_response();
    super::private_headers(&mut response, epoch);
    response
}

async fn generate_coach_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CoachPeriod>,
) -> Response {
    use super::application::coach::{SCHEMA, fingerprint};
    if let Err(r) = super::authenticate_owner(&headers, &state).await {
        return r.into_response();
    }
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.trim().is_empty() && v.len() <= 200)
    else {
        return super::error(
            StatusCode::BAD_REQUEST,
            "IDEMPOTENCY_KEY_REQUIRED",
            "Idempotency-Key is required",
        );
    };
    let canonical =
        match fingerprint(&json!({"operation":"generateWeeklyCoachReport","body":request})) {
            Ok(canonical) => canonical,
            Err(error) => return super::command_error(error),
        };
    let epoch = state.workspace.epoch();
    let pool = state.workspace.pool();
    let now = chrono::Utc::now();
    let work = async {
        let mut read = pool.begin().await.map_err(|e| super::command_error(e.into()))?;
        if let Some(receipt) = super::assistant::receipt_response(&mut read, epoch, key, &canonical).await {
            return Ok(receipt);
        }
        let week = coach_week(&mut read, request.period.as_deref(), now).await?;
        let plan = coach_plan(&mut read, &week).await?;
        read.commit().await.map_err(|e| super::command_error(e.into()))?;

        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|e| super::command_error(e.into()))?;
        if let Some(receipt) = super::assistant::receipt_response(&mut tx, epoch, key, &canonical).await {
            return Ok(receipt);
        }
        if coach_plan(&mut tx, &week).await?.dependencies != plan.dependencies {
            return Err(super::error(StatusCode::CONFLICT, "STALE_INPUT", "Refresh the weekly assessment and try again"));
        }
        let cursor: i64 = sqlx::query_scalar("SELECT coalesce(max(cursor),0) FROM event")
            .fetch_one(&mut *tx).await.map_err(|e| super::command_error(e.into()))?;
        let active: Option<String> = sqlx::query_scalar(
            "SELECT id FROM durable_job WHERE epoch=? AND role='coach' AND scope_id=?
             AND dependency_digest=? AND status IN ('queued','running') ORDER BY created_at,id LIMIT 1",
        ).bind(epoch).bind(&week.period).bind(&plan.dependencies).fetch_optional(&mut *tx)
            .await.map_err(|e| super::command_error(e.into()))?;
        let (status, result, cursor) = if let Some(job) = active {
            (StatusCode::ACCEPTED, json!({"job_id":job,"status":"queued"}), cursor)
        } else {
            let occupied: i64 = sqlx::query_scalar(
                "SELECT (SELECT count(*) FROM coach_question WHERE status IN ('open','answer_pending'))+
                    (SELECT count(*) FROM coach_question_hold WHERE epoch=?)",
            ).bind(epoch).fetch_one(&mut *tx).await.map_err(|e| super::command_error(e.into()))?;
            let reserved: Vec<String> = sqlx::query_scalar(
                "SELECT coalesce(semantic_key,question_key) FROM coach_question
                 WHERE status IN ('open','answer_pending','dismissed') OR (status='deferred' AND deferred_until>?)
                 UNION SELECT question_key FROM coach_question_hold WHERE epoch=?",
            ).bind(now_utc()).bind(epoch).fetch_all(&mut *tx).await.map_err(|e| super::command_error(e.into()))?;
            let reserved: std::collections::HashSet<_> = reserved.into_iter().collect();
            let seeds = plan.seeds.iter().filter(|seed| !reserved.contains(&seed.key))
                .take(5_i64.saturating_sub(occupied).max(0) as usize).cloned().collect::<Vec<_>>();
            if seeds.is_empty() {
                (StatusCode::OK, coach_snapshot(&mut tx, epoch, &week, &plan).await?, cursor)
            } else {
                let profile: Option<i64> = sqlx::query_scalar(
                    "SELECT a.profile_revision FROM inference_active_profile a
                     JOIN inference_qualification q ON q.profile_revision=a.profile_revision AND q.mode='structured_role'
                     WHERE a.singleton=1 AND q.status='ready' AND q.checked_at >= ?
                     AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=a.profile_revision)
                     ORDER BY q.checked_at DESC LIMIT 1",
                ).bind(super::now_utc_offset(-300)).fetch_optional(&mut *tx)
                    .await.map_err(|e| super::command_error(e.into()))?;
                let Some(profile) = profile else {
                    return Err(super::error(StatusCode::SERVICE_UNAVAILABLE, "CAPABILITY_UNQUALIFIED",
                        "Qualify the local structured-output model in Settings to generate questions"));
                };
                let live: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM durable_job WHERE status IN ('queued','running')",
                ).fetch_one(&mut *tx).await.map_err(|e| super::command_error(e.into()))?;
                if live >= 31 {
                    let mut response = super::error(StatusCode::TOO_MANY_REQUESTS, "INFERENCE_BUSY", "The local inference queue is full");
                    response.headers_mut().insert("retry-after", axum::http::HeaderValue::from_static("2"));
                    return Err(response);
                }
                let (prompt_version, prompt): (String, String) = sqlx::query_as(
                    "SELECT p.version_id,p.prompt_text FROM agent_prompt_active a
                     JOIN agent_prompt_version p ON p.version_id=a.version_id WHERE a.role='coach'",
                ).fetch_optional(&mut *tx).await.map_err(|e| super::command_error(e.into()))?
                    .unwrap_or_else(|| ("coach-v2".into(), super::application::coach::PROMPT.into()));
                let input = json!({
                    "week":week, "seeds":seeds,"coverage":plan.coverage,"event_cursor":cursor,
                    "prompt_version":prompt_version,"prompt":prompt
                });
                let equivalence = fingerprint(&json!({"period":week.period,"dependencies":plan.dependencies,
                    "profile":profile,"prompt_version":prompt_version,"schema":digest(SCHEMA.as_bytes())}))
                    .map_err(super::command_error)?;
                let job = crate::jobs::enqueue_role_in_tx(
                    &mut tx, "coach", "owner", &week.period, 1, &input, 3, epoch, None,
                    Some(&digest(format!("parley-owner:1:{epoch}").as_bytes())),
                ).await.map_err(super::command_error)?;
                sqlx::query("UPDATE durable_job SET profile_revision=?,equivalence_key=?,dependency_digest=?,eligible_at=?,queue_deadline_at=? WHERE id=?")
                    .bind(profile).bind(equivalence).bind(&plan.dependencies).bind(now_utc())
                    .bind(super::now_utc_offset(300)).bind(&job).execute(&mut *tx).await
                    .map_err(|e| super::command_error(e.into()))?;
                for seed in &seeds {
                    sqlx::query("INSERT INTO coach_question_hold(epoch,question_key,job_id,created_at) VALUES(?,?,?,?)")
                        .bind(epoch).bind(&seed.key).bind(&job).bind(now_utc())
                        .execute(&mut *tx).await.map_err(|e| super::command_error(e.into()))?;
                }
                let cursor = super::assistant::append_event(&mut tx, &job, "coach.report_requested",
                    json!({"period":week.period})).await.map_err(super::command_error)?;
                (StatusCode::ACCEPTED, json!({"job_id":job,"status":"queued"}), cursor)
            }
        };
        super::assistant::write_receipt(&mut tx, epoch, key, &canonical, status.as_u16(), &result, cursor)
            .await.map_err(super::command_error)?;
        tx.commit().await.map_err(|e| super::command_error(e.into()))?;
        if status == StatusCode::ACCEPTED {
            state.jobs.notify_one();
        }
        Ok(coach_response(status, result, epoch))
    }.await;
    match work {
        Ok(response) | Err(response) => response,
    }
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ContextCoachRequest {
    #[serde(flatten)]
    context: super::application::inference_context::ContextRequest,
}

async fn generate_context_coach(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ContextCoachRequest>,
) -> Response {
    if let Err(response) = authenticate(&headers, &state).await {
        return response.into_response();
    }
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.trim().is_empty() && v.len() <= 200)
    else {
        return super::error(
            StatusCode::BAD_REQUEST,
            "IDEMPOTENCY_KEY_REQUIRED",
            "Idempotency-Key is required",
        );
    };
    let mut context = request.context;
    context.evidence.sort_by(|left, right| {
        (&left.claim_id, left.revision).cmp(&(&right.claim_id, right.revision))
    });
    context.resource_ids.sort();
    if context.evidence.len() > 8 || context.resource_ids.len() > 8 {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "CONTEXT_BUDGET",
            "Choose at most eight evidence spans and eight resources",
        );
    }
    let canonical = match super::application::coach::fingerprint(
        &json!({"operation":"generateContextCoachQuestions","body":context}),
    ) {
        Ok(value) => value,
        Err(error) => return super::command_error(error),
    };
    let epoch = state.workspace.epoch();
    let pool = state.workspace.pool();
    let mut read = match pool.begin().await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Err(error) = super::application::inference_context::authorize(&mut read, &context).await
    {
        return match error {
            super::WorkspaceError::NotFound => super::error(
                StatusCode::NOT_FOUND,
                "TARGET_UNAVAILABLE",
                "The selected context is unavailable",
            ),
            other => super::command_error(other),
        };
    }
    if let Some(response) =
        super::assistant::receipt_response(&mut read, epoch, key, &canonical).await
    {
        return response;
    }
    let planned = match super::application::coach::plan_context(&mut read, &context).await {
        Ok(plan) => plan,
        Err(super::WorkspaceError::StaleRevision) => {
            return super::error(
                StatusCode::CONFLICT,
                "STALE_INPUT",
                "Refresh this selected party or opportunity before generating questions",
            );
        }
        Err(super::WorkspaceError::NotFound) => {
            return super::error(
                StatusCode::NOT_FOUND,
                "TARGET_UNAVAILABLE",
                "The selected party, opportunity, segment, evidence or resource is unavailable",
            );
        }
        Err(super::WorkspaceError::Invalid(code)) if code == "CONTEXT_BUDGET" => {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "CONTEXT_BUDGET",
                "Narrow the selected context and try again",
            );
        }
        Err(error) => return super::command_error(error),
    };
    read.commit().await.ok();
    let mut tx = match pool.begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(error) => return super::command_error(error.into()),
    };
    if let Err(error) = super::application::inference_context::authorize(&mut tx, &context).await {
        return match error {
            super::WorkspaceError::NotFound => super::error(
                StatusCode::NOT_FOUND,
                "TARGET_UNAVAILABLE",
                "The selected context is unavailable",
            ),
            other => super::command_error(other),
        };
    }
    if let Some(response) =
        super::assistant::receipt_response(&mut tx, epoch, key, &canonical).await
    {
        return response;
    }
    let current = match super::application::coach::plan_context(&mut tx, &context).await {
        Ok(plan) => plan,
        Err(_) => {
            return super::error(
                StatusCode::CONFLICT,
                "STALE_INPUT",
                "The selected context changed before admission; refresh and retry",
            );
        }
    };
    if current.dependencies != planned.dependencies {
        return super::error(
            StatusCode::CONFLICT,
            "STALE_INPUT",
            "The selected context changed before admission; refresh and retry",
        );
    }
    let scope = &current.projection["scope"];
    let scope_id = match scope["kind"].as_str() {
        Some("party") => scope["id"].as_str().unwrap_or_default().to_owned(),
        _ => format!(
            "{}:{}",
            scope["id"].as_str().unwrap_or_default(),
            scope["recipient_id"].as_str().unwrap_or_default()
        ),
    };
    let active: Option<String> = match sqlx::query_scalar("SELECT id FROM durable_job WHERE epoch=? AND role='coach' AND scope_id=? AND dependency_digest=? AND status IN ('queued','running') ORDER BY created_at,id LIMIT 1")
        .bind(epoch).bind(&scope_id).bind(&current.dependencies).fetch_optional(&mut *tx).await {
            Ok(value)=>value, Err(error)=>return super::command_error(error.into())
        };
    if let Some(job) = active {
        let result = json!({"job_id":job,"status":"queued"});
        let cursor = match super::assistant::append_event(
            &mut tx,
            &job,
            "coach.context_requested",
            json!({"scope":scope}),
        )
        .await
        {
            Ok(cursor) => cursor,
            Err(error) => return super::command_error(error),
        };
        if let Err(error) =
            super::assistant::write_receipt(&mut tx, epoch, key, &canonical, 202, &result, cursor)
                .await
        {
            return super::command_error(error);
        }
        if let Err(error) = tx.commit().await {
            return super::command_error(error.into());
        }
        state.jobs.notify_one();
        return coach_response(StatusCode::ACCEPTED, result, epoch);
    }
    let occupied: i64 = match sqlx::query_scalar("SELECT (SELECT count(*) FROM coach_question WHERE status IN ('open','answer_pending')) + (SELECT count(*) FROM coach_question_hold WHERE epoch=?)")
        .bind(epoch).fetch_one(&mut *tx).await { Ok(value)=>value, Err(error)=>return super::command_error(error.into()) };
    let reserved: Vec<String> = match sqlx::query_scalar("SELECT semantic_key FROM coach_question WHERE (status IN ('open','answer_pending','dismissed') OR (status='deferred' AND deferred_until>?)) AND semantic_key IS NOT NULL UNION SELECT question_key FROM coach_question_hold WHERE epoch=?")
        .bind(now_utc()).bind(epoch).fetch_all(&mut *tx).await { Ok(value)=>value, Err(error)=>return super::command_error(error.into()) };
    let reserved: std::collections::HashSet<_> = reserved.into_iter().collect();
    let seeds = current
        .seeds
        .iter()
        .filter(|seed| !reserved.contains(&seed.key))
        .take(5_i64.saturating_sub(occupied).max(0) as usize)
        .cloned()
        .collect::<Vec<_>>();
    if seeds.is_empty() {
        let visible = match coach_questions_for_scope(&mut tx, scope).await {
            Ok(questions) => questions,
            Err(error) => return super::command_error(error.into()),
        };
        let result = json!({"status":"current","scope":scope,"coverage":current.coverage,"questions":visible});
        let cursor = match super::assistant::append_event(
            &mut tx,
            &scope_id,
            "coach.context_observed",
            result.clone(),
        )
        .await
        {
            Ok(cursor) => cursor,
            Err(error) => return super::command_error(error),
        };
        if let Err(error) =
            super::assistant::write_receipt(&mut tx, epoch, key, &canonical, 200, &result, cursor)
                .await
        {
            return super::command_error(error);
        }
        if let Err(error) = tx.commit().await {
            return super::command_error(error.into());
        }
        return coach_response(StatusCode::OK, result, epoch);
    }
    let profile: Option<i64> = match sqlx::query_scalar("SELECT a.profile_revision FROM inference_active_profile a JOIN inference_qualification q ON q.profile_revision=a.profile_revision AND q.mode='structured_role' WHERE a.singleton=1 AND q.status='ready' AND q.checked_at>=? AND NOT EXISTS(SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision=a.profile_revision) ORDER BY q.checked_at DESC LIMIT 1")
        .bind(super::now_utc_offset(-300)).fetch_optional(&mut *tx).await { Ok(value)=>value, Err(error)=>return super::command_error(error.into()) };
    let Some(profile) = profile else {
        return super::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "CAPABILITY_UNQUALIFIED",
            "Qualify the local structured-output model in Settings to generate questions",
        );
    };
    let live: i64 = match sqlx::query_scalar(
        "SELECT count(*) FROM durable_job WHERE status IN ('queued','running')",
    )
    .fetch_one(&mut *tx)
    .await
    {
        Ok(value) => value,
        Err(error) => return super::command_error(error.into()),
    };
    if live >= 31 {
        return super::error(
            StatusCode::TOO_MANY_REQUESTS,
            "INFERENCE_BUSY",
            "The local inference queue is full",
        );
    }
    let (prompt_version,prompt): (String,String) = match sqlx::query_as("SELECT p.version_id,p.prompt_text FROM agent_prompt_active a JOIN agent_prompt_version p ON p.version_id=a.version_id WHERE a.role='coach'").fetch_optional(&mut *tx).await {
        Ok(Some(value))=>value, Ok(None)=>("coach-v2".into(),super::application::coach::PROMPT.into()), Err(error)=>return super::command_error(error.into())
    };
    let cursor: i64 = match sqlx::query_scalar("SELECT coalesce(max(cursor),0) FROM event")
        .fetch_one(&mut *tx)
        .await
    {
        Ok(value) => value,
        Err(error) => return super::command_error(error.into()),
    };
    let week = super::application::coach::Week {
        period: format!("context:{}", scope_id),
        through: super::now_utc(),
        as_of: super::now_utc(),
        current: true,
    };
    let input = json!({"week":week,"scope":context,"context_projection":current.projection,"dependency_json":current.dependency_json,"seeds":seeds,"coverage":current.coverage,"event_cursor":cursor,"prompt_version":prompt_version,"prompt":prompt});
    let equivalence = match super::application::coach::fingerprint(
        &json!({"scope":scope,"dependencies":current.dependencies,"profile":profile,"prompt_version":prompt_version,"schema":digest(super::application::coach::SCHEMA.as_bytes())}),
    ) {
        Ok(value) => value,
        Err(error) => return super::command_error(error),
    };
    let job = match crate::jobs::enqueue_role_in_tx(
        &mut tx,
        "coach",
        "owner",
        &scope_id,
        1,
        &input,
        3,
        epoch,
        None,
        Some(&digest(format!("parley-owner:1:{epoch}").as_bytes())),
    )
    .await
    {
        Ok(job) => job,
        Err(error) => return super::materials::workspace_error(error),
    };
    let now = now_utc();
    if let Err(error)=sqlx::query("UPDATE durable_job SET profile_revision=?,equivalence_key=?,dependency_digest=?,dependency_json=?,eligible_at=?,queue_deadline_at=? WHERE id=?")
        .bind(profile).bind(equivalence).bind(&current.dependencies).bind(current.dependency_json.to_string()).bind(&now).bind(super::now_utc_offset(300)).bind(&job).execute(&mut *tx).await { return super::command_error(error.into()); }
    for seed in &seeds {
        if let Err(error) = sqlx::query(
            "INSERT INTO coach_question_hold(epoch,question_key,job_id,created_at) VALUES(?,?,?,?)",
        )
        .bind(epoch)
        .bind(&seed.key)
        .bind(&job)
        .bind(&now)
        .execute(&mut *tx)
        .await
        {
            return super::command_error(error.into());
        }
    }
    for dep in current.dependency_json.as_array().into_iter().flatten() {
        if let Err(error) = sqlx::query(
            "INSERT INTO inference_job_dependency(job_id,kind,ref_id,revision) VALUES(?,?,?,?)",
        )
        .bind(&job)
        .bind(dep["kind"].as_str().unwrap_or("scope"))
        .bind(dep["id"].as_str().unwrap_or("scope"))
        .bind(dep["revision"].to_string())
        .execute(&mut *tx)
        .await
        {
            return super::command_error(error.into());
        }
    }
    let result = json!({"job_id":job,"status":"queued"});
    let cursor = match super::assistant::append_event(
        &mut tx,
        &job,
        "coach.context_requested",
        json!({"scope":scope}),
    )
    .await
    {
        Ok(cursor) => cursor,
        Err(error) => return super::command_error(error),
    };
    if let Err(error) =
        super::assistant::write_receipt(&mut tx, epoch, key, &canonical, 202, &result, cursor).await
    {
        return super::command_error(error);
    }
    if let Err(error) = tx.commit().await {
        return super::command_error(error.into());
    }
    state.jobs.notify_one();
    coach_response(StatusCode::ACCEPTED, result, epoch)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TagInput {
    scope: String,
    expected_revision: i64,
    entity_id: Option<String>,
    tag: String,
    claim_id: Option<String>,
    claim_revision: Option<i64>,
}
async fn save_identity_tag(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<TagInput>,
) -> Response {
    if let Err(r) = authenticate(&headers, &state).await {
        return r.into_response();
    }
    let tag = input.tag.trim();
    if tag.is_empty() || tag.len() > 80 {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "IDENTITY_TAG_INVALID",
            "Tag must contain 1 to 80 bytes",
        );
    }
    let now = now_utc();
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
                "A bounded Idempotency-Key header is required",
            );
        }
    };
    let canonical = digest(
        format!(
            "identity-tag\0{}\0{}\0{}\0{:?}\0{:?}\0{:?}",
            input.scope,
            input.expected_revision,
            tag,
            input.entity_id,
            input.claim_id,
            input.claim_revision
        )
        .as_bytes(),
    );
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    if let Some(response) =
        super::assistant::receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    let current: Option<i64> = if input.scope == "owner" {
        match sqlx::query_scalar("SELECT revision FROM owner_identity_revision WHERE id=1")
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(v) => v,
            Err(e) => return super::command_error(e.into()),
        }
    } else if let Some(entity) = input.entity_id.as_deref() {
        match sqlx::query_scalar("SELECT revision FROM entity WHERE id=?")
            .bind(entity)
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(v) => v,
            Err(e) => return super::command_error(e.into()),
        }
    } else {
        None
    };
    if current != Some(input.expected_revision) {
        return super::error(
            StatusCode::CONFLICT,
            "REVISION_CONFLICT",
            "Identity scope changed; reload and retry",
        );
    }
    let result = if input.scope == "owner" {
        if input.entity_id.is_some() || input.claim_id.is_some() || input.claim_revision.is_some() {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "IDENTITY_SCOPE_INVALID",
                "Owner tags cannot bind party claims",
            );
        }
        sqlx::query("INSERT INTO owner_identity_tag(tag,confirmed_at) VALUES(?,?) ON CONFLICT(tag) DO NOTHING").bind(tag).bind(&now).execute(&mut *tx).await
    } else if input.scope == "party" {
        let (Some(entity), Some(claim), Some(revision)) = (
            input.entity_id.as_deref(),
            input.claim_id.as_deref(),
            input.claim_revision,
        ) else {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "IDENTITY_EVIDENCE_REQUIRED",
                "Party tag requires an attributed approved claim",
            );
        };
        let valid:Option<i64>=match sqlx::query_scalar("SELECT 1 FROM claim c JOIN claim_revision r ON r.claim_id=c.id AND r.revision=? WHERE c.id=? AND c.subject_id=? AND r.tier IN (1,3) AND r.approved_wording IS NOT NULL AND r.knowledge_kind<>'hypothesis'").bind(revision).bind(claim).bind(entity).fetch_optional(&mut *tx).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
        if valid.is_none() {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "IDENTITY_EVIDENCE_INVALID",
                "Party tag requires approved tier 1 or 3 evidence",
            );
        }
        sqlx::query("INSERT INTO party_identity_tag(entity_id,tag,claim_id,claim_revision,created_at) VALUES(?,?,?,?,?) ON CONFLICT(entity_id,tag) DO UPDATE SET claim_id=excluded.claim_id,claim_revision=excluded.claim_revision,created_at=excluded.created_at").bind(entity).bind(tag).bind(claim).bind(revision).bind(&now).execute(&mut *tx).await
    } else {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "IDENTITY_SCOPE_INVALID",
            "Scope must be owner or party",
        );
    };
    if let Err(e) = result {
        return super::command_error(e.into());
    }
    let result = json!({"scope":input.scope,"entity_id":input.entity_id,"tag":tag,"claim_id":input.claim_id,"claim_revision":input.claim_revision,"revision":input.expected_revision+1});
    let cursor = match super::assistant::append_event(
        &mut tx,
        "identity-tag",
        "identity_tag.saved",
        result.clone(),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return super::command_error(e),
    };
    if input.scope == "owner" {
        if let Err(e) = sqlx::query(
            "UPDATE owner_identity_revision SET revision=revision+1 WHERE id=1 AND revision=?",
        )
        .bind(input.expected_revision)
        .execute(&mut *tx)
        .await
        {
            return super::command_error(e.into());
        }
    } else if let Some(entity) = input.entity_id.as_deref()
        && let Err(e) = sqlx::query(
            "UPDATE entity SET revision=revision+1,updated_at=? WHERE id=? AND revision=?",
        )
        .bind(now)
        .bind(entity)
        .bind(input.expected_revision)
        .execute(&mut *tx)
        .await
    {
        return super::command_error(e.into());
    }
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
        return super::command_error(e);
    };
    if let Err(e) = tx.commit().await {
        return super::command_error(e.into());
    };
    let mut response = Json(result).into_response();
    super::private_headers(&mut response, state.workspace.epoch());
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HookInput {
    expected_revision: i64,
    published_at: String,
    publicness: String,
}
async fn save_hook(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<HookInput>,
) -> Response {
    if let Err(r) = authenticate(&headers, &state).await {
        return r.into_response();
    }
    if input.publicness != "professional_public"
        || chrono::DateTime::parse_from_rfc3339(&input.published_at).is_err()
    {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "HOOK_METADATA_INVALID",
            "Hook needs RFC3339 publication date and professional_public visibility",
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
                "A bounded Idempotency-Key header is required",
            );
        }
    };
    let canonical = digest(
        format!(
            "hook\0{id}\0{}\0{}\0{}",
            input.expected_revision, input.published_at, input.publicness
        )
        .as_bytes(),
    );
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    if let Some(response) =
        super::assistant::receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return response;
    }
    let valid:Option<i64>=match sqlx::query_scalar("SELECT 1 FROM claim c JOIN claim_revision r ON r.claim_id=c.id AND r.revision=c.current_revision WHERE c.id=? AND c.current_revision=? AND lower(c.predicate) LIKE '%hook%' AND r.tier IN (1,3) AND r.approved_wording IS NOT NULL AND r.knowledge_kind<>'hypothesis'").bind(&id).bind(input.expected_revision).fetch_optional(&mut *tx).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
    if valid.is_none() {
        return super::error(
            StatusCode::CONFLICT,
            "HOOK_CLAIM_INVALID",
            "Hook must reference the current approved non-hypothesis claim",
        );
    };
    let result = json!({"claim_id":id,"claim_revision":input.expected_revision,"published_at":input.published_at,"publicness":input.publicness,"accepted":true});
    if let Err(e)=sqlx::query("INSERT INTO accepted_hook(claim_id,claim_revision,published_at,publicness,accepted_at) VALUES(?,?,?,?,?) ON CONFLICT(claim_id,claim_revision) DO UPDATE SET published_at=excluded.published_at,publicness=excluded.publicness,accepted_at=excluded.accepted_at").bind(&id).bind(input.expected_revision).bind(&input.published_at).bind(&input.publicness).bind(now_utc()).execute(&mut *tx).await{return super::command_error(e.into())}
    let cursor =
        match super::assistant::append_event(&mut tx, &id, "claim.hook_accepted", result.clone())
            .await
        {
            Ok(v) => v,
            Err(e) => return super::command_error(e),
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
        return super::command_error(e);
    };
    if let Err(e) = tx.commit().await {
        return super::command_error(e.into());
    };
    let mut response = Json(result).into_response();
    super::private_headers(&mut response, state.workspace.epoch());
    response
}
