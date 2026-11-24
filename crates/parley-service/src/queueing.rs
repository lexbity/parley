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
        .route("/api/v1/coach/questions/{id}/answer", post(answer_question))
        .route(
            "/api/v1/coach/questions/{id}/dismiss",
            post(dismiss_question),
        )
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
        Err(e) => return super::command_error(e.into()),
    };
    let rows: Vec<(String, String, String)> = match sqlx::query_as(
        "SELECT s.id, coalesce(r.class,'no_outcome'), s.segment_ids_snapshot_json\
         FROM send s LEFT JOIN outcome_revision r ON r.send_id=s.id\
         AND r.revision=(SELECT max(r2.revision) FROM outcome_revision r2 WHERE r2.send_id=s.id)\
         WHERE s.touch_no=1 ORDER BY s.id",
    )
    .fetch_all(&mut *tx)
    .await
    {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    let outcomes: Vec<(String,)> = match sqlx::query_as(
        "SELECT r.class FROM outcome_revision r WHERE r.revision=(SELECT max(r2.revision) FROM outcome_revision r2 WHERE r2.send_id=r.send_id) ORDER BY r.class",
    )
    .fetch_all(&mut *tx)
    .await
    {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
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

async fn questions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = authenticate(&headers, &state).await {
        return r.into_response();
    }
    let rows:Vec<(String,String,String,String,i64,i64)>=match sqlx::query_as("SELECT id,kind,target_json,text,priority,impact FROM coach_question WHERE status='open' ORDER BY priority,impact DESC,id").fetch_all(state.workspace.pool()).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
    let open_count = rows.len();
    let body = json!({"items":rows.into_iter().map(|(id,kind,target,text,priority,impact)|json!({"id":id,"kind":kind,"target":serde_json::from_str::<serde_json::Value>(&target).unwrap_or_default(),"text":text,"priority":priority,"impact":impact})).collect::<Vec<_>>(),"open_count":open_count,"max_open":5});
    let mut r = Json(body).into_response();
    super::private_headers(&mut r, state.workspace.epoch());
    r
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoachDisposition {
    answer: serde_json::Value,
    interviewer_chat_id: Option<String>,
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
    let canonical = digest(format!("coach-answer\\0{id}\\0{}", input.answer).as_bytes());
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    if let Some(r) =
        super::assistant::receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return r;
    }
    let row: Option<(String, String, String)> = match sqlx::query_as(
        "SELECT kind,answer_schema_json,status FROM coach_question WHERE id=?",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    let Some((kind, schema, status)) = row else {
        return super::error(
            StatusCode::NOT_FOUND,
            "QUESTION_NOT_FOUND",
            "Coach question does not exist",
        );
    };
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
        let Some(chat_id) = input.interviewer_chat_id.as_deref() else {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INTERVIEWER_REQUIRED",
                "Choose an owner Interviewer chat",
            );
        };
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
        let chat:Option<(i64,String)>=match sqlx::query_as("SELECT revision,asset_kind FROM agent_chat WHERE id=? AND role='interviewer' AND scope_kind='owner'").bind(chat_id).fetch_optional(&mut *tx).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
        let Some((chat_revision, chat_kind)) = chat else {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INTERVIEWER_SCOPE_INVALID",
                "Choose an owner Interviewer chat",
            );
        };
        if chat_kind != asset_kind {
            return super::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INTERVIEWER_ASSET_KIND_MISMATCH",
                "Interviewer chat must match the Coach question resource kind",
            );
        }
        let turn_no: i64 = match sqlx::query_scalar(
            "SELECT coalesce(max(turn_no),0)+1 FROM agent_turn WHERE chat_id=?",
        )
        .bind(chat_id)
        .fetch_one(&mut *tx)
        .await
        {
            Ok(v) => v,
            Err(e) => return super::command_error(e.into()),
        };
        let next_revision = chat_revision + 1;
        let now = now_utc();
        if let Err(e)=sqlx::query("INSERT INTO agent_turn(id,chat_id,turn_no,speaker,text,created_at) VALUES(?,?,?,'owner',?,?)").bind(super::new_id(chat_id)).bind(chat_id).bind(turn_no).bind(text).bind(&now).execute(&mut *tx).await{return super::command_error(e.into())}
        if let Err(e) =
            sqlx::query("UPDATE agent_chat SET revision=?,updated_at=? WHERE id=? AND revision=?")
                .bind(next_revision)
                .bind(&now)
                .bind(chat_id)
                .bind(chat_revision)
                .execute(&mut *tx)
                .await
        {
            return super::command_error(e.into());
        }
        let turns: Vec<Value> = match sqlx::query_as::<_, (String, String, String)>(
            "SELECT id,speaker,text FROM agent_turn WHERE chat_id=? ORDER BY turn_no",
        )
        .bind(chat_id)
        .fetch_all(&mut *tx)
        .await
        {
            Ok(v) => v
                .into_iter()
                .map(|(id, speaker, text)| json!({"id":id,"speaker":speaker,"text":text}))
                .collect(),
            Err(e) => return super::command_error(e.into()),
        };
        let prompt: Option<String> = match sqlx::query_scalar(
            "SELECT version_id FROM agent_prompt_active WHERE role='interviewer'",
        )
        .fetch_optional(&mut *tx)
        .await
        {
            Ok(v) => v,
            Err(e) => return super::command_error(e.into()),
        };
        let bank:Option<(String,String)>=match sqlx::query_as("SELECT b.version_id,b.content_json FROM interviewer_question_bank_active a JOIN interviewer_question_bank_version b ON b.version_id=a.version_id WHERE a.singleton=1").fetch_optional(&mut *tx).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
        let Some((bank_id, bank_content)) = bank else {
            return super::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "QUESTION_BANK_MISSING",
                "Interviewer question bank is unavailable",
            );
        };
        let existing:Option<(String,String)>=match sqlx::query_as("SELECT fields_json,owner_quotes_json FROM interviewer_candidate WHERE chat_id=? AND state='proposed' ORDER BY created_at DESC LIMIT 1").bind(chat_id).fetch_optional(&mut *tx).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
        let fields = existing
            .as_ref()
            .and_then(|v| serde_json::from_str::<serde_json::Value>(&v.0).ok())
            .unwrap_or(json!({}));
        let quotes = existing
            .as_ref()
            .and_then(|v| serde_json::from_str::<serde_json::Value>(&v.1).ok())
            .unwrap_or(json!({}));
        let input_job = json!({"chat_id":chat_id,"asset_kind":asset_kind,"chat_revision":next_revision,"turns":turns,"current_fields":fields,"current_quotes":quotes,"question_bank_version_id":bank_id,"question_bank":serde_json::from_str::<serde_json::Value>(&bank_content).unwrap_or_default(),"prompt_version_id":prompt,"coach_question_id":id});
        let job = match crate::jobs::enqueue_role_in_tx(
            &mut tx,
            "interviewer",
            "owner",
            chat_id,
            next_revision,
            &input_job,
            3,
            state.workspace.epoch(),
            None,
            None,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return super::materials::workspace_error(e),
        };
        let disposition = json!({"answer":input.answer,"interviewer_chat_id":chat_id,"job_id":job});
        if let Err(e)=sqlx::query("UPDATE coach_question SET status='answered',disposition_json=?,updated_at=? WHERE id=? AND status='open'").bind(disposition.to_string()).bind(&now).bind(&id).execute(&mut *tx).await{return super::command_error(e.into())};
        let result = json!({"id":id,"status":"answered","chat_id":chat_id,"job_id":job,"revision":next_revision});
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
        crate::jobs::wake(&state.jobs, result["job_id"].as_str().unwrap());
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
    if let Err(e)=sqlx::query("UPDATE coach_question SET status='answered',disposition_json=?,updated_at=? WHERE id=? AND status='open'").bind(input.answer.to_string()).bind(&now).bind(&id).execute(&mut *tx).await{return super::command_error(e.into())};
    let result = json!({"id":id,"status":"answered","answer":input.answer,"recorded_at":now});
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
async fn dismiss_question(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
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
    let canonical = digest(format!("coach-dismiss\\0{id}").as_bytes());
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    if let Some(r) =
        super::assistant::receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return r;
    };
    let changed = match sqlx::query(
        "UPDATE coach_question SET status='dismissed',updated_at=? WHERE id=? AND status='open'",
    )
    .bind(now_utc())
    .bind(&id)
    .execute(&mut *tx)
    .await
    {
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
    let result = json!({"id":id,"status":"dismissed"});
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
struct CoachPeriod {
    period: String,
}
async fn get_coach_report(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = authenticate(&headers, &state).await {
        return r.into_response();
    }
    let row:Option<(String,String,String)>=match sqlx::query_as("SELECT report_json,period,input_digest FROM coach_report ORDER BY created_at DESC,id DESC LIMIT 1").fetch_optional(state.workspace.pool()).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
    let body = if let Some((report, period, input_digest)) = row {
        json!({"report":serde_json::from_str::<serde_json::Value>(&report).unwrap_or_default(),"period":period,"input_digest":input_digest})
    } else {
        json!({"report":null,"period":null,"input_digest":null})
    };
    let mut r = Json(body).into_response();
    super::private_headers(&mut r, state.workspace.epoch());
    r
}
async fn generate_coach_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CoachPeriod>,
) -> Response {
    if let Err(r) = authenticate(&headers, &state).await {
        return r.into_response();
    }
    if request.period.trim().is_empty() || request.period.len() > 20 {
        return super::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "PERIOD_INVALID",
            "A bounded weekly period is required",
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
    let canonical = digest(format!("coach-weekly\\0{}", request.period).as_bytes());
    let mut tx = match state.workspace.pool().begin_with("BEGIN IMMEDIATE").await {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    if let Some(r) =
        super::assistant::receipt_response(&mut tx, state.workspace.epoch(), &key, &canonical).await
    {
        return r;
    }
    let cursor: i64 = match sqlx::query_scalar("SELECT coalesce(max(cursor),0) FROM event")
        .fetch_one(&mut *tx)
        .await
    {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    let open: i64 =
        match sqlx::query_scalar("SELECT count(*) FROM coach_question WHERE status='open'")
            .fetch_one(&mut *tx)
            .await
        {
            Ok(v) => v,
            Err(e) => return super::command_error(e.into()),
        };
    if open >= 5 {
        return super::error(
            StatusCode::CONFLICT,
            "COACH_QUESTION_CAP",
            "Resolve an open Coach question before generating a report",
        );
    }
    let segments: Vec<(String, String)> =
        match sqlx::query_as("SELECT id,name FROM segment ORDER BY id")
            .fetch_all(&mut *tx)
            .await
        {
            Ok(v) => v,
            Err(e) => return super::command_error(e.into()),
        };
    let match_rows:Vec<(String,String,Option<String>)>=match sqlx::query_as("SELECT sp.segment_id,op.opportunity_id,m.shortlist_json FROM segment_party sp JOIN opportunity_party op ON op.party_id=sp.party_id JOIN opportunity o ON o.id=op.opportunity_id AND o.state='open' JOIN entity e ON e.id=op.party_id LEFT JOIN match_run m ON m.id=(SELECT m2.id FROM match_run m2 WHERE m2.opportunity_id=o.id AND m2.recipient_id=e.id AND m2.opportunity_revision=o.revision AND m2.recipient_revision=e.revision ORDER BY m2.created_at DESC,m2.id DESC LIMIT 1) ORDER BY sp.segment_id,op.opportunity_id").fetch_all(&mut *tx).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
    let mut coverage: std::collections::HashMap<(String, String), (i64, i64)> =
        std::collections::HashMap::new();
    for (segment, _, shortlist) in match_rows {
        let parsed: serde_json::Value = shortlist
            .as_deref()
            .and_then(|v| serde_json::from_str(v).ok())
            .unwrap_or_default();
        for kind in ["proof", "gift", "artifact"] {
            let matched = parsed
                .get("results")
                .and_then(|v| v.as_array())
                .is_some_and(|items| {
                    items
                        .iter()
                        .any(|i| i["kind"] == kind && i["no_match"] == false)
                });
            let row = coverage
                .entry((segment.clone(), kind.to_owned()))
                .or_insert((0, 0));
            row.0 += 1;
            if matched {
                row.1 += 1
            }
        }
    }
    let mut candidates = Vec::new();
    for (segment, name) in segments {
        for kind in ["proof", "gift", "artifact"] {
            let (total, covered) = coverage
                .get(&(segment.clone(), kind.to_owned()))
                .copied()
                .unwrap_or_default();
            let impact = total - covered;
            if impact > 0 {
                let qkind = format!("missing_{kind}");
                candidates.push(json!({"question_key":format!("{qkind}:{segment}"),"kind":qkind,"target_ref":{"segment_id":segment},"answer_schema":{"type":"object","properties":{kind:{"type":"object"}}},"priority":0,"impact":impact,"why_now":format!("{impact} of {total} open opportunities in {name} have no calibrated {kind} match.")}));
            }
        }
    }
    let unknown_terms: Vec<String> = match sqlx::query_scalar(
        "SELECT term FROM fluency WHERE status='unknown' ORDER BY term COLLATE NOCASE,term",
    )
    .fetch_all(&mut *tx)
    .await
    {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    for term in unknown_terms {
        candidates.push(json!({"question_key":format!("jargon_check:{term}"),"kind":"jargon_check","target_ref":{"term":term},"answer_schema":{"type":"object","required":["fluency"],"properties":{"fluency":{"enum":["learning","fluent"]}}},"priority":0,"impact":1,"why_now":format!("Fluency for {term} is currently unknown.")}));
    }
    let flagged: i64 =
        match sqlx::query_scalar("SELECT count(*) FROM template_version WHERE status='flagged'")
            .fetch_one(&mut *tx)
            .await
        {
            Ok(v) => v,
            Err(e) => return super::command_error(e.into()),
        };
    if flagged > 0 {
        candidates.push(json!({"question_key":"template_review:flagged","kind":"template_review","target_ref":{"status":"flagged"},"answer_schema":{"type":"object","required":["decision"],"properties":{"decision":{"enum":["revise","retire"]}}},"priority":0,"impact":flagged,"why_now":format!("{flagged} template version(s) await review.")}));
    }
    let stale_capacity:i64=match sqlx::query_scalar("SELECT count(*) FROM capacity_ledger WHERE updated_at < strftime('%Y-%m-%dT%H:%M:%fZ','now','-30 days')").fetch_one(&mut *tx).await{Ok(v)=>v,Err(e)=>return super::command_error(e.into())};
    if stale_capacity > 0 {
        candidates.push(json!({"question_key":"capacity_update:stale","kind":"capacity_update","target_ref":{"stale_days":30},"answer_schema":{"type":"object","required":["capacity"],"properties":{"capacity":{"type":"object"}}},"priority":0,"impact":1,"why_now":"Capacity has not been reviewed recently."}));
    }
    let owner_tags: i64 = match sqlx::query_scalar("SELECT count(*) FROM owner_identity_tag")
        .fetch_one(&mut *tx)
        .await
    {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    if owner_tags == 0 {
        candidates.push(json!({"question_key":"missing_identity:owner","kind":"missing_identity","target_ref":{"scope":"owner"},"answer_schema":{"type":"object","required":["identity"],"properties":{"identity":{"type":"string","minLength":1}}},"priority":0,"impact":1,"why_now":"No owner identity tag is available for a verified shared-identity link."}));
    }
    let first_touches: i64 = match sqlx::query_scalar("SELECT count(*) FROM send WHERE touch_no=1")
        .fetch_one(&mut *tx)
        .await
    {
        Ok(v) => v,
        Err(e) => return super::command_error(e.into()),
    };
    if first_touches > 0 && first_touches < 20 {
        candidates.push(json!({"question_key":"insight:small_sample","kind":"insight","target_ref":{"first_touch_count":first_touches},"answer_schema":{"type":"object","required":["note"],"properties":{"note":{"type":"string","minLength":1}}},"priority":0,"impact":1,"why_now":format!("Only {first_touches} first touches exist; comparisons remain inconclusive.")}));
    }
    candidates.sort_by(|a, b| {
        b["impact"]
            .as_i64()
            .cmp(&a["impact"].as_i64())
            .then_with(|| a["kind"].as_str().cmp(&b["kind"].as_str()))
            .then_with(|| a["question_key"].as_str().cmp(&b["question_key"].as_str()))
    });
    for (i, c) in candidates.iter_mut().enumerate() {
        c["priority"] = json!(i)
    }
    candidates.truncate((5 - open) as usize);
    let input = json!({"period":request.period,"event_cursor":cursor.to_string(),"evidence":{"segment_gaps":candidates,"candidate_count":candidates.len()},"candidates":candidates});
    let job =
        match crate::jobs::enqueue_role_in_tx(
            &mut tx,
            "coach",
            "owner",
            "weekly",
            1,
            &input,
            3,
            state.workspace.epoch(),
            None,
            None,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return super::materials::workspace_error(e),
        };
    let event_cursor = match super::assistant::append_event(
        &mut tx,
        &job,
        "coach.report_requested",
        json!({"period":request.period,"event_cursor":cursor}),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return super::materials::workspace_error(e),
    };
    let result = json!({"job_id":job,"period":request.period,"event_cursor":cursor.to_string()});
    if let Err(e) = super::assistant::write_receipt(
        &mut tx,
        state.workspace.epoch(),
        &key,
        &canonical,
        202,
        &result,
        event_cursor,
    )
    .await
    {
        return super::materials::workspace_error(e);
    }
    if let Err(e) = tx.commit().await {
        return super::command_error(e.into());
    }
    crate::jobs::wake(&state.jobs, result["job_id"].as_str().unwrap());
    let mut r = (StatusCode::ACCEPTED, Json(result)).into_response();
    super::private_headers(&mut r, state.workspace.epoch());
    r
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
