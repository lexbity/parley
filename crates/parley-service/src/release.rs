//! Recipient-facing issuance: text release and verified file download.
//!
//! This module owns the shared release logic from S03 and extends it to file
//! handoffs (RFC-0002 BQ4). A release authorizes; downloading bytes verifies
//! the complete file into a bounded spool, rechecks grant, gate, current
//! dependency vector and envelope, commits the one-use issuance fact, and only
//! then streams from the exact verified handle. No range requests, no inline
//! rendering, no background delivery.

use super::materials::{event, validate_binding_shape, workspace_error};
use super::{AppState, Workspace, WorkspaceError, digest, new_id, now_utc, private_headers};
use axum::{
    Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::stream::unfold;
use parley_domain::{gate, template};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/materials/{id}/download",
        get(download_material).head(head_not_allowed),
    )
}

/// RFC-0004 IQ2: HEAD-as-GET is disabled. A HEAD probe must never run the
/// download flow (spool verification, writer reservation, issuance event,
/// one-use grant consumption); refuse it outright before any state is
/// touched.
async fn head_not_allowed() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, HeaderValue::from_static("GET"))],
    )
        .into_response()
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct AttachmentInfo {
    pub upload_id: String,
    pub sha256: String,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
}

#[derive(Debug, Deserialize)]
struct DownloadQuery {
    release_id: String,
}

#[derive(Debug)]
pub(crate) struct HeadMaterial {
    pub(crate) id: String,
    pub(crate) opportunity_id: String,
    pub(crate) recipient_id: String,
    pub(crate) recipient_name: String,
    pub(crate) version: i64,
    pub(crate) template_id: String,
    pub(crate) exchange_version: i64,
    pub(crate) body: String,
    pub(crate) slot_fills: Vec<super::materials::SlotFill>,
    pub(crate) bindings: Vec<super::materials::SentenceBinding>,
    pub(crate) hash: String,
    pub(crate) attachment: Option<AttachmentInfo>,
}

pub(crate) async fn load_head(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<HeadMaterial, WorkspaceError> {
    let row=sqlx::query_as::<_,(String,String,String,String,i64,String,i64,String,String,String,String,Option<String>,Option<String>,Option<String>,Option<String>,Option<i64>)>("SELECT m.id,m.opportunity_id,m.recipient_id,e.name,mv.version_no,mv.template_version_id,mv.exchange_version,mv.body,mv.slot_fills_json,mv.sentence_bindings_json,mv.content_sha256,mv.blob_upload_id,mv.blob_sha256,mv.filename,mv.content_type,mv.attachment_size FROM material m JOIN entity e ON e.id=m.recipient_id JOIN material_version mv ON mv.material_id=m.id AND mv.version_no=m.head_version WHERE m.id=?")
        .bind(id).fetch_optional(&mut **tx).await?.ok_or_else(||WorkspaceError::Invalid("material does not exist".into()))?;
    let attachment = match (row.11, row.12, row.13, row.14, row.15) {
        (Some(upload_id), Some(sha256), Some(filename), Some(content_type), Some(size)) => {
            Some(AttachmentInfo {
                upload_id,
                sha256,
                filename,
                content_type,
                size,
            })
        }
        (None, None, None, None, None) => None,
        _ => {
            return Err(WorkspaceError::Invalid(
                "material attachment metadata is incomplete".into(),
            ));
        }
    };
    Ok(HeadMaterial {
        id: row.0,
        opportunity_id: row.1,
        recipient_id: row.2,
        recipient_name: row.3,
        version: row.4,
        template_id: row.5,
        exchange_version: row.6,
        body: row.7,
        slot_fills: serde_json::from_str(&row.8)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?,
        bindings: serde_json::from_str(&row.9)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?,
        hash: row.10,
        attachment,
    })
}

pub(crate) async fn dependency_vector(
    tx: &mut Transaction<'_, Sqlite>,
    material: &HeadMaterial,
) -> Result<BTreeMap<String, String>, WorkspaceError> {
    let mut result = BTreeMap::new();
    result.insert(
        format!("opportunity:{}", material.opportunity_id),
        sqlx::query_scalar::<_, i64>("SELECT revision FROM opportunity WHERE id=?")
            .bind(&material.opportunity_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| WorkspaceError::Invalid("opportunity no longer exists".into()))?
            .to_string(),
    );
    result.insert(
        format!("party:{}", material.recipient_id),
        sqlx::query_scalar::<_, i64>("SELECT revision FROM party_policy WHERE party_id=?")
            .bind(&material.recipient_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| WorkspaceError::Invalid("recipient policy no longer exists".into()))?
            .to_string(),
    );
    result.insert(
        format!("template:{}", material.template_id),
        sqlx::query_scalar::<_, String>(
            "SELECT content_sha256 FROM template_version WHERE id=? AND status='active'",
        )
        .bind(&material.template_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| WorkspaceError::Invalid("template is no longer active".into()))?,
    );
    let (exchange_digest, _, _, _) = current_exchange(
        tx,
        &material.opportunity_id,
        &material.recipient_id,
        material.exchange_version,
    )
    .await?;
    result.insert(
        format!(
            "exchange:{}:{}:{}",
            material.opportunity_id, material.recipient_id, material.exchange_version
        ),
        exchange_digest,
    );
    for fill in &material.slot_fills {
        if let Some(resource_id) = &fill.resource_id {
            let (revision, readiness): (i64, String) =
                sqlx::query_as("SELECT revision,readiness FROM resource WHERE id=?")
                    .bind(resource_id)
                    .fetch_optional(&mut **tx)
                    .await?
                    .ok_or_else(|| {
                        WorkspaceError::Invalid(format!("resource {resource_id} no longer exists"))
                    })?;
            if readiness != "ready" {
                return Err(WorkspaceError::Invalid(format!(
                    "resource {resource_id} is no longer ready"
                )));
            }
            result.insert(format!("resource:{resource_id}"), revision.to_string());
        }
    }
    for binding in &material.bindings {
        if binding.claim_type == "other_fact" {
            let revision: i64 = sqlx::query_scalar("SELECT current_revision FROM claim WHERE id=?")
                .bind(binding.ref_id.as_deref().unwrap_or(""))
                .fetch_optional(&mut **tx)
                .await?
                .ok_or_else(|| WorkspaceError::Invalid("claim reference is missing".into()))?;
            if Some(revision) != binding.ref_revision || binding.approved_wording.is_none() {
                return Err(WorkspaceError::Invalid(
                    "claim was corrected or its owner-approved wording changed".into(),
                ));
            }
            result.insert(
                format!("claim:{}", binding.ref_id.as_deref().unwrap_or("")),
                revision.to_string(),
            );
        }
    }
    if let Some(attachment) = &material.attachment {
        let row: Option<(String, i64)> = sqlx::query_as(
            "SELECT u.blob_sha256,u.revision FROM blob_upload u WHERE u.id=? AND u.reviewed_by_owner=1 AND u.share_permission=1",
        )
        .bind(&attachment.upload_id)
        .fetch_optional(&mut **tx)
        .await?;
        let Some((blob_sha256, revision)) = row else {
            return Err(WorkspaceError::Invalid(
                "attachment upload is no longer reviewed or shareable".into(),
            ));
        };
        if blob_sha256 != attachment.sha256 {
            return Err(WorkspaceError::Invalid(
                "attachment bytes changed after review".into(),
            ));
        }
        let blob_size: Option<i64> = sqlx::query_scalar("SELECT size FROM blob WHERE sha256=?")
            .bind(&attachment.sha256)
            .fetch_optional(&mut **tx)
            .await?;
        if blob_size != Some(attachment.size) {
            return Err(WorkspaceError::Invalid(
                "attachment blob record is missing or its size changed".into(),
            ));
        }
        result.insert(
            format!("blob:{}", attachment.upload_id),
            revision.to_string(),
        );
    }
    Ok(result)
}

pub(crate) async fn evaluate_material(
    tx: &mut Transaction<'_, Sqlite>,
    material: &HeadMaterial,
    overrides: &BTreeMap<String, String>,
    schedule_gates: bool,
) -> Result<gate::GateResult, WorkspaceError> {
    let (exchange_state,they_get,i_get,_,attested,digest_stored)=sqlx::query_as::<_,(String,String,String,i64,i64,Option<String>)>("SELECT o.state,x.they_get,x.i_get,x.version_no,x.fairness_attested,x.attestation_digest FROM opportunity o JOIN exchange_version x ON x.opportunity_id=o.id WHERE x.opportunity_id=? AND x.recipient_id=? AND x.version_no=?")
        .bind(&material.opportunity_id).bind(&material.recipient_id).bind(material.exchange_version).fetch_optional(&mut **tx).await?.ok_or_else(||WorkspaceError::Invalid("exchange version is unavailable".into()))?;
    let interests = exchange_interests(
        tx,
        &material.opportunity_id,
        &material.recipient_id,
        material.exchange_version,
    )
    .await?;
    let current = exchange_digest(
        &material.opportunity_id,
        &material.recipient_id,
        material.exchange_version,
        &they_get,
        &i_get,
        &interests,
    );
    let exchange_complete = !they_get.trim().is_empty()
        && !i_get.trim().is_empty()
        && interests.iter().any(|(_, side, _, _)| side == "them")
        && interests.iter().any(|(_, side, _, _)| side == "me");
    let policy: Option<(String, i64, i64)> = sqlx::query_as(
        "SELECT state,do_not_contact,highest_rung FROM party_policy WHERE party_id=?",
    )
    .bind(&material.recipient_id)
    .fetch_optional(&mut **tx)
    .await?;
    let (party_state, dnc, highest_rung) =
        policy.ok_or_else(|| WorkspaceError::Invalid("recipient has no policy row".into()))?;
    let template_body: String = sqlx::query_scalar("SELECT body FROM template_version WHERE id=?")
        .bind(&material.template_id)
        .fetch_one(&mut **tx)
        .await?;
    let declared_slots = template::validate(&template_body, None).slots;
    let slot_values: Vec<String> = material
        .slot_fills
        .iter()
        .map(|fill| fill.value.clone())
        .collect();
    let sentence_list = split_sentences(&material.body);
    validate_binding_shape(&sentence_list, &material.bindings)?;
    let mut binding_valid = true;
    let mut ask_count = 0usize;
    let mut ask_rung = 0u32;
    let mut used_rules = BTreeSet::new();
    let mut gift = false;
    let mut anchor = false;
    let mut private_artifact = false;
    let mut uses_hook = false;
    let mut sensitive = false;
    let mut identity_valid = true;
    for binding in &material.bindings {
        let sentence = &sentence_list[binding.sentence_index];
        match binding.claim_type.as_str() {
            "generic" => {
                if binding.ref_kind.is_some()
                    || binding.ref_id.is_some()
                    || !generic_sentence_allowed(sentence, &material.recipient_name)
                {
                    binding_valid = false;
                }
            }
            "offer" => {
                let Some(resource_id) = binding
                    .ref_id
                    .as_deref()
                    .filter(|_| binding.ref_kind.as_deref() == Some("resource"))
                else {
                    binding_valid = false;
                    continue;
                };
                let row: Option<(String, String, String, String)> = sqlx::query_as(
                    "SELECT kind,readiness,title,body_json FROM resource WHERE id=?",
                )
                .bind(resource_id)
                .fetch_optional(&mut **tx)
                .await?;
                if let Some((kind, status, _title, body)) = row {
                    let value: Value = serde_json::from_str(&body)
                        .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
                    let what = value.get("what").and_then(Value::as_str).unwrap_or("");
                    if kind != "gift"
                        || status != "ready"
                        || what.is_empty()
                        || !sentence.contains(what)
                        || !tokens_contained(sentence, &body, &material.recipient_name)
                    {
                        binding_valid = false;
                    }
                    gift = true;
                } else {
                    binding_valid = false;
                }
                used_rules.insert("R1");
            }
            "self_fact" => {
                let Some(resource_id) = binding
                    .ref_id
                    .as_deref()
                    .filter(|_| binding.ref_kind.as_deref() == Some("resource"))
                else {
                    binding_valid = false;
                    continue;
                };
                let row: Option<(String, String, String)> =
                    sqlx::query_as("SELECT kind,readiness,body_json FROM resource WHERE id=?")
                        .bind(resource_id)
                        .fetch_optional(&mut **tx)
                        .await?;
                if let Some((kind, status, body)) = row {
                    if status != "ready"
                        || !matches!(kind.as_str(), "proof" | "artifact")
                        || binding.approved_wording.as_deref() != Some(sentence)
                        || !tokens_contained(sentence, &body, &material.recipient_name)
                    {
                        binding_valid = false;
                    }
                    anchor |= kind == "proof";
                    if kind == "proof" {
                        used_rules.insert("R3");
                    }
                    if kind == "artifact" {
                        used_rules.insert("R4");
                    }
                } else {
                    binding_valid = false;
                }
            }
            "other_fact" => {
                let Some(claim_id) = binding
                    .ref_id
                    .as_deref()
                    .filter(|_| binding.ref_kind.as_deref() == Some("claim"))
                else {
                    binding_valid = false;
                    continue;
                };
                let valid:Option<(i64,i64,String,String)>=sqlx::query_as("SELECT c.current_revision,r.tier,r.wording,coalesce(r.approved_wording,'') FROM claim c JOIN claim_revision r ON r.claim_id=c.id AND r.revision=? WHERE c.id=?").bind(binding.ref_revision.unwrap_or_default()).bind(claim_id).fetch_optional(&mut **tx).await?;
                if let Some((current_rev, tier, wording, _approved)) = valid {
                    if current_rev != binding.ref_revision.unwrap_or_default()
                        || !matches!(tier, 1 | 3)
                        || binding.approved_wording.as_deref() != Some(sentence)
                        || !tokens_contained(sentence, &wording, &material.recipient_name)
                    {
                        binding_valid = false;
                    }
                } else {
                    binding_valid = false;
                }
                anchor = true;
                used_rules.insert("R5");
            }
            "question" => {
                let Some(interest_id) = binding
                    .ref_id
                    .as_deref()
                    .filter(|_| binding.ref_kind.as_deref() == Some("interest"))
                else {
                    binding_valid = false;
                    continue;
                };
                let valid: Option<String> = sqlx::query_scalar("SELECT statement FROM opportunity_interest WHERE id=? AND opportunity_id=? AND party_id=? AND status='active'")
                    .bind(interest_id).bind(&material.opportunity_id).bind(&material.recipient_id).fetch_optional(&mut **tx).await?;
                if valid.is_none()
                    || !sentence.trim_end().ends_with('?')
                    || binding.approved_wording.as_deref() != Some(sentence)
                {
                    binding_valid = false;
                }
            }
            "ask" => {
                if binding.ref_kind.as_deref() != Some("ladder")
                    || binding.ask_rung.is_none()
                    || binding.ask_rung.unwrap_or(0) == 0
                    || binding.ask_rung.unwrap_or(6) > 5
                    || !sentence.trim_end().ends_with('?')
                {
                    binding_valid = false;
                }
                ask_count += 1;
                ask_rung = ask_rung.max(binding.ask_rung.unwrap_or_default());
                used_rules.insert("R2");
            }
            "identity" => {
                identity_valid = false;
                anchor = true;
                used_rules.insert("R7");
            }
            "hook" => {
                uses_hook = true;
                anchor = true;
                used_rules.insert("R5");
            }
            _ => binding_valid = false,
        }
        sensitive |= has_sensitive_content(sentence);
    }
    for fill in &material.slot_fills {
        if let Some(resource_id) = &fill.resource_id {
            let row: Option<(String, String)> = sqlx::query_as(
                "SELECT kind,body_json FROM resource WHERE id=? AND readiness='ready'",
            )
            .bind(resource_id)
            .fetch_optional(&mut **tx)
            .await?;
            let Some((kind, body)) = row else {
                binding_valid = false;
                continue;
            };
            if kind == "proof" || kind == "artifact" {
                anchor = true;
            }
            if kind == "gift" {
                gift = true;
            }
            if kind == "proof" {
                used_rules.insert("R3");
            }
            if kind == "artifact" {
                used_rules.insert("R4");
            }
            if kind == "gift" {
                used_rules.insert("R1");
            }
            if kind == "artifact" {
                let v: Value = serde_json::from_str(&body)
                    .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
                private_artifact |= v.get("public").and_then(Value::as_bool) == Some(false);
            }
            let expected_binding = material.bindings.iter().any(|binding| {
                binding.ref_kind.as_deref() == Some("resource")
                    && binding.ref_id.as_deref() == Some(resource_id.as_str())
                    && match kind.as_str() {
                        "gift" => binding.claim_type == "offer",
                        "proof" | "artifact" => binding.claim_type == "self_fact",
                        _ => false,
                    }
            });
            if !expected_binding {
                binding_valid = false;
            }
            sensitive |= has_sensitive_content(&fill.value);
        } else {
            sensitive |= has_sensitive_content(&fill.value);
        }
    }
    let capacity_fill = material
        .slot_fills
        .iter()
        .find(|fill| fill.slot.starts_with("capacity_line:"))
        .map(|fill| fill.value.clone());
    let scarcity_claim_valid = match &capacity_fill {
        None => true,
        Some(value) => capacity_claim_matches_ledger(tx, value).await?,
    };
    let banned = [
        "i hope this finds you well",
        "synergy",
        "circle back",
        "touch base",
        "leverage",
    ];
    let lint_error = banned
        .iter()
        .any(|phrase| material.body.to_lowercase().contains(phrase));
    let char_limit = 1200usize;
    let glossary: Vec<(String, String)> = sqlx::query_as("SELECT term,status FROM fluency")
        .fetch_all(&mut **tx)
        .await?;
    let fluency_ok = glossary
        .iter()
        .all(|(term, status)| status == "fluent" || !contains_glossary_term(&material.body, term));
    let gift_delivered_before = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM send s JOIN send_part sp ON sp.send_id=s.id JOIN resource_use ru ON ru.material_id=sp.material_id AND ru.version_no=sp.version_no JOIN resource r ON r.id=ru.resource_id WHERE s.recipient_id=? AND r.kind='gift'",
    )
    .bind(&material.recipient_id)
    .fetch_one(&mut **tx)
    .await?
        != 0;
    let mut input = gate::GateInput {
        exchange_complete,
        fairness_attested: attested == 1 && digest_stored.as_deref() == Some(current.as_str()),
        factual_sentences_grounded: binding_valid,
        sentence_count: sentence_list.len(),
        bound_sentence_count: material.bindings.len(),
        slot_fills: slot_values,
        body: material.body.clone(),
        scarcity_claim_valid,
        identity_claims_valid: identity_valid,
        ask_count,
        ask_rung,
        highest_rung: highest_rung.max(0) as u32,
        has_sensitive_content: sensitive,
        recipient_open: party_state == "open" && exchange_state == "open",
        recipient_do_not_contact: dnc != 0,
        references_private_artifact: private_artifact,
        gift_in_body: gift,
        gift_delivered_before,
        anchor_used: anchor,
        rule_count: used_rules.len(),
        rule_limit: if sqlx::query_scalar::<_, i64>(
            "SELECT touch_count FROM party_policy WHERE party_id=?",
        )
        .bind(&material.recipient_id)
        .fetch_one(&mut **tx)
        .await?
            == 0
        {
            3
        } else {
            4
        },
        fluency_ok,
        lint_error,
        hook_fresh: !uses_hook,
        cadence_ok: if schedule_gates {
            let pack_id = owner_pack_id(tx).await?;
            let context =
                cadence_context(tx, &material.opportunity_id, &material.recipient_id).await?;
            let cap_ok = context.touch_count < MAX_TOUCHES;
            let spacing_ok = match &context.last_sent_at {
                Some(last) => {
                    let now_date = utc_date_of(&super::now_utc())?;
                    let last_date = utc_date_of(last)?;
                    let needed = followup_days_for_touch(&pack_id, context.touch_count);
                    let earliest = parley_domain::lifecycle::add_business_days(last_date, needed);
                    now_date >= earliest
                }
                None => true,
            };
            cap_ok && spacing_ok
        } else {
            true
        },
        daily_cap_ok: if schedule_gates {
            let pack_id = owner_pack_id(tx).await?;
            sends_today_count(tx).await? < i64::from(pack_daily_cap(&pack_id))
        } else {
            true
        },
        body_char_limit: char_limit,
        brief_reviewed: true,
        override_reasons: overrides.clone(),
    };
    if !declared_slots
        .iter()
        .all(|slot| material.slot_fills.iter().any(|fill| &fill.slot == slot))
    {
        input.factual_sentences_grounded = false;
    }
    Ok(gate::evaluate(&input))
}

fn contains_glossary_term(body: &str, term: &str) -> bool {
    let body = body.to_lowercase();
    let term = term.trim().to_lowercase();
    if term.is_empty() {
        return false;
    }
    body.match_indices(&term).any(|(start, matched)| {
        let end = start + matched.len();
        let before = body[..start].chars().next_back();
        let after = body[end..].chars().next();
        let word_edge = |ch: Option<char>| ch.is_none_or(|value| !value.is_alphanumeric());
        word_edge(before) && word_edge(after)
    })
}

/// Maximum touches before a party moves to dormant (RFC-0001 §10.6 CAD-2,
/// initial calibration from the job_search pack).
pub(crate) const MAX_TOUCHES: i64 = 3;

pub(crate) fn pack_daily_cap(pack_id: &str) -> u32 {
    match pack_id {
        "job_search" => 12,
        _ => 15,
    }
}

/// Business-day follow-up interval after `prior_touches` sends
/// (RFC-0001 §10.6 CAD-1, pack cadence defaults).
pub(crate) fn followup_days_for_touch(pack_id: &str, prior_touches: i64) -> i64 {
    let sequence: &[i64] = if pack_id == "job_search" {
        &[4, 8]
    } else {
        &[3, 7]
    };
    let index = (prior_touches - 1).clamp(0, sequence.len() as i64 - 1) as usize;
    sequence[index]
}

fn utc_date_of(timestamp: &str) -> Result<chrono::NaiveDate, WorkspaceError> {
    chrono::NaiveDate::parse_from_str(&timestamp[..10], "%Y-%m-%d")
        .map_err(|_| WorkspaceError::Invalid("stored timestamp has an unexpected date".into()))
}

async fn owner_pack_id(tx: &mut Transaction<'_, Sqlite>) -> Result<String, WorkspaceError> {
    Ok(
        sqlx::query_scalar("SELECT pack_id FROM owner_profile WHERE id=1")
            .fetch_one(&mut **tx)
            .await?,
    )
}

struct CadenceContext {
    last_sent_at: Option<String>,
    touch_count: i64,
}

async fn cadence_context(
    tx: &mut Transaction<'_, Sqlite>,
    opportunity_id: &str,
    recipient_id: &str,
) -> Result<CadenceContext, WorkspaceError> {
    let row = sqlx::query_as::<_, (Option<String>, i64)>(
        "SELECT ct.sent_at, pp.touch_count FROM contact_thread ct JOIN party_policy pp ON pp.party_id=ct.recipient_id WHERE ct.opportunity_id=? AND ct.recipient_id=?",
    )
    .bind(opportunity_id)
    .bind(recipient_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(match row {
        Some((last_sent_at, touch_count)) => CadenceContext {
            last_sent_at,
            touch_count,
        },
        None => CadenceContext {
            last_sent_at: None,
            touch_count: 0,
        },
    })
}

async fn sends_today_count(tx: &mut Transaction<'_, Sqlite>) -> Result<i64, WorkspaceError> {
    let today = &super::now_utc()[..10];
    let start = format!("{today}T00:00:00.000Z");
    let next_day = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d")
        .map_err(|_| WorkspaceError::Invalid("today has an unexpected date".into()))?
        .succ_opt()
        .expect("tomorrow exists");
    let end = format!("{}T00:00:00.000Z", next_day.format("%Y-%m-%d"));
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM send WHERE sent_at >= ? AND sent_at < ?")
            .bind(start)
            .bind(end)
            .fetch_one(&mut **tx)
            .await?,
    )
}
fn split_sentences(body: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0;
    for (index, ch) in body.char_indices() {
        let end = index + ch.len_utf8();
        if matches!(ch, '.' | '!' | '?' | '\n') {
            let value = body[start..end].trim();
            if !value.is_empty() {
                result.push(value.to_owned());
            }
            start = end;
        }
    }
    let rest = body[start..].trim();
    if !rest.is_empty() {
        result.push(rest.to_owned());
    }
    result
}

fn generic_sentence_allowed(sentence: &str, recipient_name: &str) -> bool {
    if sentence.chars().any(|ch| ch.is_ascii_digit()) {
        return false;
    }
    let recipient_words: BTreeSet<String> = recipient_name
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let mut first = true;
    for token in sentence.split_whitespace() {
        let word = token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '\'');
        if word.is_empty() {
            continue;
        }
        let allowed = first || word == "I" || recipient_words.contains(&word.to_lowercase());
        if !allowed && word.chars().next().is_some_and(char::is_uppercase) {
            return false;
        }
        first = false;
    }
    true
}

fn tokens_contained(sentence: &str, source: &str, recipient_name: &str) -> bool {
    let recipient_lower = recipient_name.to_lowercase();
    let source_lower = source.to_lowercase();
    for token in sentence.split_whitespace() {
        let token = token
            .trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '%' && ch != '$' && ch != '.');
        if token.is_empty() {
            continue;
        }
        let numeric = token.chars().any(|ch| ch.is_ascii_digit());
        let named = token.chars().next().is_some_and(char::is_uppercase) && token.len() > 1;
        if (numeric || named)
            && !source_lower.contains(&token.to_lowercase())
            && !recipient_lower.contains(&token.to_lowercase())
        {
            return false;
        }
    }
    true
}

fn has_sensitive_content(text: &str) -> bool {
    const TERMS: &[&str] = &[
        "health",
        "disability",
        "medical",
        "family",
        "wife",
        "husband",
        "child",
        "children",
        "sexual orientation",
        "religion",
        "politics",
        "political",
        "race",
        "ethnicity",
        "immigration",
        "union membership",
        "financial hardship",
        "home address",
        "home location",
        "pregnant",
        "diagnosis",
    ];
    let normalized = text.to_lowercase();
    TERMS.iter().any(|term| normalized.contains(term))
}

/// Identify a capacity claim in the slot fills: a `capacity_line` typed slot.
pub(crate) fn capacity_line_fill(fills: &[super::materials::SlotFill]) -> Option<String> {
    fills
        .iter()
        .find(|fill| fill.slot.starts_with("capacity_line:"))
        .map(|fill| fill.value.clone())
}

pub(crate) async fn capacity_claim_matches_ledger(
    tx: &mut Transaction<'_, Sqlite>,
    value: &str,
) -> Result<bool, WorkspaceError> {
    let row: Option<(f64, f64)> =
        sqlx::query_as("SELECT total,committed FROM capacity_ledger WHERE period=? AND unit=?")
            .bind(parley_domain::lifecycle::DEFAULT_CAPACITY_PERIOD)
            .bind(parley_domain::lifecycle::DEFAULT_CAPACITY_UNIT)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((total, committed)) = row else {
        return Ok(false);
    };
    let remaining = (total - committed).round() as i64;
    Ok(extract_number(value) == Some(remaining))
}

pub(crate) fn extract_number(text: &str) -> Option<i64> {
    text.split(|ch: char| !ch.is_ascii_digit())
        .find(|part| !part.is_empty())
        .and_then(|part| part.parse().ok())
}

pub(crate) async fn exchange_interests(
    tx: &mut Transaction<'_, Sqlite>,
    opportunity: &str,
    recipient: &str,
    version: i64,
) -> Result<Vec<(String, String, String, i64)>, WorkspaceError> {
    Ok(sqlx::query_as("SELECT i.id,i.side,i.statement,i.revision FROM exchange_interest_link l JOIN opportunity_interest i ON i.id=l.interest_id WHERE l.opportunity_id=? AND l.recipient_id=? AND l.exchange_version=? AND i.status='active' ORDER BY i.id")
        .bind(opportunity).bind(recipient).bind(version).fetch_all(&mut **tx).await?)
}

async fn current_exchange(
    tx: &mut Transaction<'_, Sqlite>,
    opportunity: &str,
    recipient: &str,
    version: i64,
) -> Result<(String, String, String, Vec<(String, String, String, i64)>), WorkspaceError> {
    let (they_get,i_get):(String,String)=sqlx::query_as("SELECT they_get,i_get FROM exchange_version WHERE opportunity_id=? AND recipient_id=? AND version_no=?").bind(opportunity).bind(recipient).bind(version).fetch_optional(&mut **tx).await?.ok_or_else(||WorkspaceError::Invalid("exchange version does not exist".into()))?;
    let interests = exchange_interests(tx, opportunity, recipient, version).await?;
    let digest = exchange_digest(
        opportunity,
        recipient,
        version,
        &they_get,
        &i_get,
        &interests,
    );
    Ok((digest, they_get, i_get, interests))
}

pub(crate) fn exchange_digest(
    opportunity: &str,
    recipient: &str,
    version: i64,
    they_get: &str,
    i_get: &str,
    interests: &[(String, String, String, i64)],
) -> String {
    digest(
        &serde_json::to_vec(&(opportunity, recipient, version, they_get, i_get, interests))
            .unwrap_or_default(),
    )
}

/// Validate the attachment eligibility for a release-ready head: the upload is
/// reviewed, shareable and its bytes still exist and match. Shared by approval
/// and release so a revoked or corrupt attachment blocks before the gate.
async fn verify_attachment_eligible(
    tx: &mut Transaction<'_, Sqlite>,
    material: &HeadMaterial,
) -> Result<(), WorkspaceError> {
    let Some(attachment) = &material.attachment else {
        return Ok(());
    };
    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT u.blob_sha256,u.size FROM blob_upload u JOIN blob b ON b.sha256=u.blob_sha256 WHERE u.id=? AND u.reviewed_by_owner=1 AND u.share_permission=1",
    )
    .bind(&attachment.upload_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((blob_sha256, size)) = row else {
        return Err(WorkspaceError::Invalid(
            "attachment requires a reviewed, shareable upload".into(),
        ));
    };
    if blob_sha256 != attachment.sha256 || size != attachment.size {
        return Err(WorkspaceError::Invalid(
            "attachment bytes changed after review".into(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct ApprovalRow {
    pub(crate) id: String,
    pub(crate) hash: String,
    pub(crate) dependency_json: String,
    pub(crate) overrides_json: String,
}

pub(crate) async fn load_approval(
    tx: &mut Transaction<'_, Sqlite>,
    material: &HeadMaterial,
) -> Result<ApprovalRow, WorkspaceError> {
    let (id, hash, dependency_json, overrides_json): (String, String, String, String) =
        sqlx::query_as("SELECT id,content_sha256,dependency_vector_json,overrides_json FROM approval WHERE material_id=? AND version_no=?")
            .bind(&material.id)
            .bind(material.version)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| {
                WorkspaceError::Invalid("material version must be approved before release".into())
            })?;
    Ok(ApprovalRow {
        id,
        hash,
        dependency_json,
        overrides_json,
    })
}

impl Workspace {
    /// Release an approved material version. Text materials return the exact
    /// approved bytes in the response; attachment materials authorize the
    /// version and the download endpoint streams the verified file bytes
    /// against the one-use issuance grant.
    pub async fn release_material(&self, id: &str) -> Result<Value, WorkspaceError> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let material = load_head(&mut tx, id).await?;
        let approval = load_approval(&mut tx, &material).await?;
        if approval.hash != material.hash {
            return Err(WorkspaceError::Invalid(
                "approved bytes do not match the immutable material version".into(),
            ));
        }
        let current = dependency_vector(&mut tx, &material).await?;
        let current_json =
            serde_json::to_string(&current).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        if approval.dependency_json != current_json {
            return Err(WorkspaceError::Invalid(
                "material dependencies changed after approval; create and review a new version"
                    .into(),
            ));
        }
        verify_attachment_eligible(&mut tx, &material).await?;
        let overrides: BTreeMap<String, String> = serde_json::from_str(&approval.overrides_json)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let checked = evaluate_material(&mut tx, &material, &overrides, false).await?;
        if !checked.copy_enabled {
            return Err(WorkspaceError::Invalid(
                "current release gate blocks this material".into(),
            ));
        }
        if let Some(attachment) = &material.attachment {
            self.verify_blob_file(&attachment.sha256, attachment.size as u64)
                .await?;
        }
        let release_id = new_id(&approval.id);
        let now = now_utc();
        sqlx::query("INSERT INTO release_record(id,approval_id,material_id,version_no,content_sha256,issued_at) VALUES(?,?,?,?,?,?)").bind(&release_id).bind(&approval.id).bind(&material.id).bind(material.version).bind(&material.hash).bind(&now).execute(&mut *tx).await?;
        event(&mut tx,"material",&material.id,material.version,"material.release_issued",json!({"release_id":release_id,"approval_id":approval.id,"content_sha256":material.hash})).await?;
        let mut result = json!({"release_id":release_id,"material_id":material.id,"version":material.version,"recipient_id":material.recipient_id,"body":material.body,"content_sha256":material.hash,"issued_at":now});
        if let Some(attachment) = &material.attachment {
            result["attachment"] = serde_json::to_value(attachment)
                .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        }
        tx.commit().await?;
        Ok(result)
    }

    pub async fn acknowledge_release(
        &self,
        release_id: &str,
        succeeded: bool,
    ) -> Result<(), WorkspaceError> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let exists: Option<String> =
            sqlx::query_scalar("SELECT material_id FROM release_record WHERE id=?")
                .bind(release_id)
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(WorkspaceError::Invalid(
                "release record does not exist".into(),
            ));
        }
        let previous: Option<i64> =
            sqlx::query_scalar("SELECT succeeded FROM release_ack WHERE release_id=?")
                .bind(release_id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(previous) = previous {
            if previous != i64::from(succeeded) {
                return Err(WorkspaceError::Invalid(
                    "release acknowledgement cannot be changed after it is recorded".into(),
                ));
            }
            tx.commit().await?;
            return Ok(());
        }
        let now = now_utc();
        sqlx::query("INSERT INTO release_ack(release_id,succeeded,acknowledged_at) VALUES(?,?,?)")
            .bind(release_id)
            .bind(succeeded)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        let (material_id, version): (String, i64) =
            sqlx::query_as("SELECT material_id,version_no FROM release_record WHERE id=?")
                .bind(release_id)
                .fetch_one(&mut *tx)
                .await?;
        event(
            &mut tx,
            "material",
            &material_id,
            version,
            "material.release_acknowledged",
            json!({"release_id":release_id,"succeeded":succeeded}),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Begin a verified download. The complete file is verified into a bounded
    /// spool first; the writer then rechecks the one-use grant, the current
    /// dependency vector and the gate, and commits the issuance fact before
    /// any byte is streamed.
    pub async fn start_download_material(
        &self,
        material_id: &str,
        release_id: &str,
    ) -> Result<DownloadGrant, WorkspaceError> {
        let material = {
            let mut tx = self.pool().begin().await?;
            load_head(&mut tx, material_id).await?
        };
        let Some(attachment) = &material.attachment else {
            return Err(WorkspaceError::Invalid(
                "download requires an attachment material version".into(),
            ));
        };
        // Preflight outside the writer: the release grant must exist, match
        // the head version and stay unconsumed before paying for spool
        // verification.
        {
            let mut tx = self.pool().begin().await?;
            let release: Option<(i64, String, Option<String>)> = sqlx::query_as(
                "SELECT version_no,content_sha256,bytes_issued_at FROM release_record WHERE id=? AND material_id=?",
            )
            .bind(release_id)
            .bind(material_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some((version, hash, issued)) = release else {
                return Err(WorkspaceError::Invalid(
                    "release record does not exist for this material".into(),
                ));
            };
            if version != material.version || hash != material.hash {
                return Err(WorkspaceError::Invalid(
                    "release grant does not match the current material version".into(),
                ));
            }
            if issued.is_some() {
                return Err(WorkspaceError::Invalid(
                    "release grant was already consumed by a download".into(),
                ));
            }
        }
        let token = self
            .spool()
            .verify(
                self.blob_path(&attachment.sha256),
                &attachment.sha256,
                attachment.size as u64,
            )
            .await?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let material = load_head(&mut tx, material_id).await?;
        let Some(attachment) = &material.attachment else {
            return Err(WorkspaceError::Invalid(
                "download requires an attachment material version".into(),
            ));
        };
        let release: Option<(i64, String, Option<String>)> = sqlx::query_as(
            "SELECT version_no,content_sha256,bytes_issued_at FROM release_record WHERE id=? AND material_id=?",
        )
        .bind(release_id)
        .bind(material_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((version, hash, issued)) = release else {
            return Err(WorkspaceError::Invalid(
                "release record does not exist for this material".into(),
            ));
        };
        if version != material.version || hash != material.hash || issued.is_some() {
            return Err(WorkspaceError::Invalid(
                "release grant is stale or already consumed".into(),
            ));
        }
        let approval = load_approval(&mut tx, &material).await?;
        if approval.hash != material.hash {
            return Err(WorkspaceError::Invalid(
                "approved bytes do not match the immutable material version".into(),
            ));
        }
        let current = dependency_vector(&mut tx, &material).await?;
        let current_json =
            serde_json::to_string(&current).map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        if approval.dependency_json != current_json {
            return Err(WorkspaceError::Invalid(
                "material dependencies changed after approval; create and review a new version"
                    .into(),
            ));
        }
        verify_attachment_eligible(&mut tx, &material).await?;
        let overrides: BTreeMap<String, String> = serde_json::from_str(&approval.overrides_json)
            .map_err(|e| WorkspaceError::Invalid(e.to_string()))?;
        let checked = evaluate_material(&mut tx, &material, &overrides, false).await?;
        if !checked.copy_enabled {
            return Err(WorkspaceError::Invalid(
                "current release gate blocks this download".into(),
            ));
        }
        let now = now_utc();
        let updated = sqlx::query("UPDATE release_record SET bytes_issued_at=? WHERE id=? AND material_id=? AND bytes_issued_at IS NULL")
            .bind(&now)
            .bind(release_id)
            .bind(material_id)
            .execute(&mut *tx)
            .await?;
        if updated.rows_affected() != 1 {
            return Err(WorkspaceError::Invalid(
                "release grant was already consumed by a download".into(),
            ));
        }
        event(
            &mut tx,
            "material",
            material_id,
            material.version,
            "material.download_issued",
            json!({"release_id":release_id,"content_sha256":material.hash}),
        )
        .await?;
        tx.commit().await?;
        Ok(DownloadGrant {
            token,
            material_id: material.id,
            version: material.version,
            recipient_id: material.recipient_id,
            filename: attachment.filename.clone(),
            mime: attachment.content_type.clone(),
            size: attachment.size as u64,
            content_sha256: attachment.sha256.clone(),
        })
    }

    /// Best-effort record of an observable incomplete transfer (broken pipe).
    pub async fn record_incomplete_transfer(&self, release_id: &str) {
        let Ok(mut tx) = self.pool().begin_with("BEGIN IMMEDIATE").await else {
            return;
        };
        let Ok(rows) = sqlx::query(
            "UPDATE release_record SET transfer_incomplete=1 WHERE id=? AND bytes_issued_at IS NOT NULL AND transfer_incomplete=0",
        )
        .bind(release_id)
        .execute(&mut *tx)
        .await
        else {
            return;
        };
        if rows.rows_affected() == 1 {
            let _ = event(
                &mut tx,
                "material",
                "",
                0,
                "material.download_incomplete",
                json!({"release_id":release_id}),
            )
            .await;
        }
        let _ = tx.commit().await;
    }
}

#[derive(Debug)]
pub struct DownloadGrant {
    pub token: super::blob::SpoolToken,
    pub material_id: String,
    pub version: i64,
    pub recipient_id: String,
    pub filename: String,
    pub mime: String,
    pub size: u64,
    pub content_sha256: String,
}

fn safe_filename(filename: &str) -> String {
    filename
        .chars()
        .map(|ch| {
            if ch == '"' || ch == '\\' || ch == '\r' || ch == '\n' {
                '_'
            } else {
                ch
            }
        })
        .collect()
}

async fn download_material(
    State(state): State<AppState>,
    Path(material_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<DownloadQuery>,
) -> Response {
    if let Err(error) = super::authenticate(&headers, &state).await {
        return (error.0, error.1).into_response();
    }
    let granted = match state
        .workspace
        .start_download_material(&material_id, &query.release_id)
        .await
    {
        Ok(value) => value,
        Err(error) => return workspace_error(error),
    };
    let release_id = query.release_id.clone();
    let workspace = state.workspace.clone();
    let file = match state.workspace.spool().take_file(&granted.token).await {
        Ok(value) => value,
        Err(error) => return workspace_error(error),
    };
    let stream = unfold(Some(file), move |state| {
        let workspace = workspace.clone();
        let release_id = release_id.clone();
        async move {
            let mut file = state?;
            let mut buf = vec![0u8; 65_536];
            match tokio::io::AsyncReadExt::read(&mut file, &mut buf).await {
                Ok(0) => None,
                Ok(read) => {
                    buf.truncate(read);
                    Some((Ok::<_, std::io::Error>(Bytes::from(buf)), Some(file)))
                }
                Err(error) => {
                    tokio::spawn(async move {
                        workspace.record_incomplete_transfer(&release_id).await;
                    });
                    Some((Err(error), None))
                }
            }
        }
    });
    let mut response = axum::body::Body::from_stream(stream).into_response();
    *response.status_mut() = StatusCode::OK;
    if let Ok(value) = HeaderValue::from_str(&granted.mime) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!(
        "attachment; filename=\"{}\"",
        safe_filename(&granted.filename)
    )) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    if let Ok(value) = HeaderValue::from_str(&granted.size.to_string()) {
        response.headers_mut().insert(header::CONTENT_LENGTH, value);
    }
    private_headers(&mut response, state.workspace.epoch());
    response
}
