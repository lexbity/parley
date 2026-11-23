use super::materials::{material_receipt_replay, material_receipt_store};
use super::{
    AppState, Workspace, WorkspaceError, authenticate, canonical_markdown, command_error, digest,
    error, idempotency_key, new_id, now_utc, private_headers,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{patch, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
type Tx<'a> = Transaction<'a, Sqlite>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelatedCard {
    pub kind: String,
    pub name: String,
    pub related_entity_id: String,
    pub verb: String,
    pub occurrence_id: String,
    pub quote: String,
    pub view_scope: String,
    pub view_id: String,
    pub x: f64,
    pub y: f64,
    pub expected_parent_revision: Option<i64>,
    pub direction: Option<Direction>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub staged_upload_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Incoming,
    Outgoing,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Link {
    from_id: String,
    to_id: String,
    verb: String,
    occurrence_id: String,
    quote: String,
    expected_from_revision: i64,
    expected_to_revision: i64,
}
#[derive(Serialize, Deserialize)]
struct Correction {
    expected_revision: i64,
    #[serde(flatten)]
    link: Link,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum EdgeCorrection {
    Owner(super::resource_board::UpdateEdgeInput),
    Sourced(Correction),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Removal {
    expected_revision: i64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ClaimRevision {
    expected_revision: i64,
    occurrence_id: String,
    wording: String,
    speaker_kind: String,
    speaker_entity_id: Option<String>,
}

#[derive(Serialize)]
enum Command {
    ReviseClaim(String, ClaimRevision),
    Related(RelatedCard),
    Link(Link),
    Correct(String, Correction),
    Remove(String, Removal),
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/graph/related", post(related))
        .route("/api/v1/claims/{id}/revisions", post(revise_claim))
        .route("/api/v1/edges", post(link))
        .route("/api/v1/edges/{id}", patch(correct).delete(remove))
}

fn invalid(message: &str) -> WorkspaceError {
    WorkspaceError::Invalid(message.into())
}

fn legal(from: &str, to: &str, verb: &str) -> bool {
    matches!(
        (from, verb, to),
        ("person", "member_of", "org")
            | ("org", "works_with", "project")
            | ("person", "offers_to", "resource")
            | ("org" | "person", "publishes", "source")
            | ("org" | "person", "expresses", "interest")
            | ("person", "maintains", "org" | "project")
            | ("artifact", "could_help", "org" | "person")
    )
}

async fn kind(tx: &mut Tx<'_>, id: &str, expected: Option<i64>) -> Result<String, WorkspaceError> {
    let (kind, rev): (String, i64) =
        sqlx::query_as("SELECT kind,revision FROM graph_card WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(WorkspaceError::NotFound)?;
    if expected.is_some_and(|e| e != rev) {
        return Err(WorkspaceError::StaleRevision);
    }
    Ok(kind)
}

async fn event(
    tx: &mut Tx<'_>,
    ty: &str,
    id: &str,
    rev: i64,
    op: &str,
) -> Result<(), WorkspaceError> {
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) VALUES(?,?,?,?,?,?,'{}')")
        .bind(new_id(id)).bind(now_utc()).bind(ty).bind(id).bind(rev).bind(op).execute(&mut **tx).await?;
    Ok(())
}

async fn add_evidence(tx: &mut Tx<'_>, id: &str, link: &Link) -> Result<bool, WorkspaceError> {
    let duplicate:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM edge_evidence ee JOIN claim_revision cr ON cr.claim_id=ee.claim_id AND cr.revision=ee.claim_revision WHERE ee.edge_id=? AND cr.occurrence_id=? AND cr.wording=?)")
        .bind(id).bind(&link.occurrence_id).bind(&link.quote).fetch_one(&mut **tx).await?;
    if duplicate {
        return Ok(false);
    }
    let (text,variant,speaker,actor):(String,String,String,Option<String>)=sqlx::query_as("SELECT v.raw_text,v.id,o.actor_kind,o.actor_entity_id FROM source_occurrence o JOIN source_variant v ON v.id=o.source_variant_id WHERE o.id=?").bind(&link.occurrence_id).fetch_optional(&mut **tx).await?.ok_or_else(||invalid("Select a saved source occurrence."))?;
    let span = parley_domain::evidence::verify_quote(&text, &link.quote, None)
        .map_err(|_| invalid("Quote is absent or ambiguous in this source."))?;
    let claim = new_id(id);
    let now = now_utc();
    sqlx::query(
        "INSERT INTO claim(id,subject_id,predicate,current_revision,created_at) VALUES(?,?,?,1,?)",
    )
    .bind(&claim)
    .bind(&link.from_id)
    .bind(&link.verb)
    .bind(&now)
    .execute(&mut **tx)
    .await?;
    sqlx::query("INSERT INTO claim_revision(claim_id,revision,wording,knowledge_kind,tier,speaker_kind,speaker_entity_id,occurrence_id,source_variant_id,source_start,source_end,normalized_start,normalized_end,normalization_version,created_at) VALUES(?,1,?,'observation',1,?,?,?,?,?,?,?,?,?,?)")
        .bind(&claim).bind(&link.quote).bind(&speaker).bind(actor).bind(&link.occurrence_id).bind(variant).bind(span.source_start as i64).bind(span.source_end as i64).bind(span.normalized_start as i64).bind(span.normalized_end as i64).bind(parley_domain::evidence::NORMALIZATION_VERSION as i64).bind(now).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO edge_evidence(edge_id,claim_id,claim_revision) VALUES(?,?,1)")
        .bind(id)
        .bind(claim)
        .execute(&mut **tx)
        .await?;
    Ok(true)
}

pub(super) async fn archive(
    tx: &mut Tx<'_>,
    id: &str,
    revision: i64,
) -> Result<(), WorkspaceError> {
    let changed=sqlx::query("INSERT INTO edge_history SELECT id,revision,from_id,to_id,verb,state,(SELECT json_group_array(json_object('claim_id',claim_id,'revision',claim_revision)) FROM edge_evidence WHERE edge_id=edge.id),?,basis FROM edge WHERE id=? AND revision=? AND state='active'")
        .bind(now_utc()).bind(id).bind(revision).execute(&mut **tx).await?;
    if changed.rows_affected() != 1 {
        return Err(WorkspaceError::StaleRevision);
    }
    Ok(())
}

async fn save_edge(
    tx: &mut Tx<'_>,
    link: &Link,
    correction: Option<(&str, i64)>,
) -> Result<Value, WorkspaceError> {
    let from = kind(tx, &link.from_id, Some(link.expected_from_revision)).await?;
    let to = kind(tx, &link.to_id, Some(link.expected_to_revision)).await?;
    if !legal(&from, &to, &link.verb) || link.from_id == link.to_id {
        return Err(invalid(
            "This verb and direction are invalid for the endpoint types.",
        ));
    }
    let existing: Option<(String, i64)> = sqlx::query_as(
        "SELECT id,revision FROM edge WHERE from_id=? AND to_id=? AND verb=? AND state='active'",
    )
    .bind(&link.from_id)
    .bind(&link.to_id)
    .bind(&link.verb)
    .fetch_optional(&mut **tx)
    .await?;
    let (id, revision) = if let Some((id, rev)) = correction {
        if existing.as_ref().is_some_and(|(other, _)| other != id) {
            return Err(invalid(
                "An active relationship already has these endpoints and verb.",
            ));
        }
        let endpoints: (String, String) =
            sqlx::query_as("SELECT from_id,to_id FROM edge WHERE id=?")
                .bind(id)
                .fetch_optional(&mut **tx)
                .await?
                .ok_or(WorkspaceError::NotFound)?;
        if !(endpoints.0 == link.from_id && endpoints.1 == link.to_id
            || endpoints.0 == link.to_id && endpoints.1 == link.from_id)
        {
            return Err(invalid(
                "A correction must keep the relationship endpoints.",
            ));
        }
        archive(tx, id, rev).await?;
        sqlx::query("DELETE FROM edge_evidence WHERE edge_id=?")
            .bind(id)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE edge SET from_id=?,to_id=?,verb=?,revision=revision+1 WHERE id=?")
            .bind(&link.from_id)
            .bind(&link.to_id)
            .bind(&link.verb)
            .bind(id)
            .execute(&mut **tx)
            .await?;
        add_evidence(tx, id, link).await?;
        event(tx, "edge", id, rev + 1, "edge.corrected").await?;
        (id.to_owned(), rev + 1)
    } else if let Some((id, rev)) = existing {
        if add_evidence(tx, &id, link).await? {
            sqlx::query("UPDATE edge SET revision=revision+1 WHERE id=?")
                .bind(&id)
                .execute(&mut **tx)
                .await?;
            event(tx, "edge", &id, rev + 1, "edge.evidence_added").await?;
            (id, rev + 1)
        } else {
            (id, rev)
        }
    } else {
        let id = new_id(&link.verb);
        sqlx::query("INSERT INTO edge(id,from_id,verb,to_id,state,revision,created_at) VALUES(?,?,?,?,'active',1,?)").bind(&id).bind(&link.from_id).bind(&link.verb).bind(&link.to_id).bind(now_utc()).execute(&mut **tx).await?;
        add_evidence(tx, &id, link).await?;
        event(tx, "edge", &id, 1, "edge.created").await?;
        (id, 1)
    };
    Ok(
        json!({"data":{"id":id,"revision":revision,"from_id":link.from_id,"to_id":link.to_id,"verb":link.verb}}),
    )
}

async fn create_card(tx: &mut Tx<'_>, input: RelatedCard) -> Result<Value, WorkspaceError> {
    let name = input.name.trim();
    if name.is_empty()
        || name.len() > 500
        || input.notes.len() > 512 * 1024
        || !input.x.is_finite()
        || !input.y.is_finite()
        || input.staged_upload_ids.len() > 100
    {
        return Err(invalid(
            "Check the card name, notes, placement and attachment count.",
        ));
    }
    let parent = kind(tx, &input.related_entity_id, input.expected_parent_revision).await?;
    let parent_revision = sqlx::query_scalar("SELECT revision FROM entity WHERE id=?")
        .bind(&input.related_entity_id)
        .fetch_one(&mut **tx)
        .await?;
    let incoming = match input.direction {
        Some(Direction::Incoming) => true,
        Some(Direction::Outgoing) => false,
        None => legal(&input.kind, &parent, &input.verb),
    };
    let id = new_id(name);
    let (from_id, to_id, expected_from_revision, expected_to_revision, from, to) = if incoming {
        (
            id.clone(),
            input.related_entity_id,
            1,
            parent_revision,
            &input.kind,
            &parent,
        )
    } else {
        (
            input.related_entity_id,
            id.clone(),
            parent_revision,
            1,
            &parent,
            &input.kind,
        )
    };
    if !legal(from, to, &input.verb) {
        return Err(invalid(
            "This verb and direction are invalid for the endpoint types.",
        ));
    }
    let now = now_utc();
    if input.kind == "artifact" {
        if name.chars().count() > 240 {
            return Err(invalid("Resource title exceeds 240 characters."));
        }
        sqlx::query("INSERT INTO resource(id,kind,title,body_json,readiness,revision,created_at,updated_at) VALUES(?,'artifact',?,'{}','draft',1,?,?)")
            .bind(&id).bind(name).bind(&now).bind(&now).execute(&mut **tx).await?;
        sqlx::query("UPDATE entity SET notes_md=? WHERE id=?")
            .bind(canonical_markdown(&input.notes))
            .bind(&id)
            .execute(&mut **tx)
            .await?;
        event(tx, "resource", &id, 1, "resource.created").await?;
    } else {
        sqlx::query("INSERT INTO entity(id,kind,name,revision,notes_md,created_at,updated_at) VALUES(?,?,?,1,?,?,?)").bind(&id).bind(&input.kind).bind(name).bind(canonical_markdown(&input.notes)).bind(&now).bind(now).execute(&mut **tx).await?;
    }
    event(tx, "entity", &id, 1, "entity.created").await?;
    save_edge(
        tx,
        &Link {
            from_id,
            to_id,
            expected_from_revision,
            expected_to_revision,
            verb: input.verb,
            occurrence_id: input.occurrence_id,
            quote: input.quote,
        },
        None,
    )
    .await?;
    sqlx::query("INSERT INTO view_position(view_scope,view_id,entity_id,x,y,pinned,revision) VALUES(?,?,?,?,?,0,1)").bind(input.view_scope).bind(input.view_id).bind(&id).bind(input.x).bind(input.y).execute(&mut **tx).await?;
    for upload in input.staged_upload_ids {
        super::parties::bind_upload(tx, &id, &upload).await?;
    }
    Ok(json!({"id":id,"kind":input.kind,"name":name,"focus":"","revision":1}))
}

async fn run(
    ws: &Workspace,
    key: &str,
    cmd: Command,
    status: StatusCode,
) -> Result<Value, WorkspaceError> {
    let canonical =
        digest(&serde_json::to_vec(&cmd).map_err(|e| WorkspaceError::Invalid(e.to_string()))?);
    let mut tx = ws.pool().begin_with("BEGIN IMMEDIATE").await?;
    if let Some(saved) = material_receipt_replay(&mut tx, ws.epoch(), key, &canonical).await? {
        return Ok(saved);
    }
    let result = match cmd {
        Command::ReviseClaim(id, input) => save_revision(&mut tx, &id, input).await?,
        Command::Related(input) => create_card(&mut tx, input).await?,
        Command::Link(input) => save_edge(&mut tx, &input, None).await?,
        Command::Correct(id, input) => {
            save_edge(&mut tx, &input.link, Some((&id, input.expected_revision))).await?
        }
        Command::Remove(id, input) => {
            archive(&mut tx, &id, input.expected_revision).await?;
            sqlx::query("UPDATE edge SET revision=revision+1,state='retracted' WHERE id=?")
                .bind(&id)
                .execute(&mut *tx)
                .await?;
            event(
                &mut tx,
                "edge",
                &id,
                input.expected_revision + 1,
                "edge.removed",
            )
            .await?;
            Value::Null
        }
    };
    sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
        .execute(&mut *tx)
        .await?;
    material_receipt_store(
        &mut tx,
        ws.epoch(),
        key,
        &canonical,
        status.as_u16(),
        &result,
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

async fn execute(state: AppState, h: HeaderMap, cmd: Command, status: StatusCode) -> Response {
    if let Err(r) = authenticate(&h, &state).await {
        return r.into_response();
    }
    let key = match idempotency_key(&h) {
        Ok(k) => k,
        Err((s, c, m)) => return error(s, c, m),
    };
    match run(&state.workspace, &key, cmd, status).await {
        Ok(value) => {
            let mut r = if status == StatusCode::NO_CONTENT {
                status.into_response()
            } else {
                (status, Json(value)).into_response()
            };
            private_headers(&mut r, state.workspace.epoch());
            r
        }
        Err(e) => command_error(e),
    }
}
async fn related(State(s): State<AppState>, h: HeaderMap, Json(i): Json<RelatedCard>) -> Response {
    execute(s, h, Command::Related(i), StatusCode::CREATED).await
}
async fn link(State(s): State<AppState>, h: HeaderMap, Json(i): Json<Link>) -> Response {
    execute(s, h, Command::Link(i), StatusCode::CREATED).await
}
async fn correct(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(i): Json<EdgeCorrection>,
) -> Response {
    match i {
        EdgeCorrection::Sourced(input) => {
            execute(s, h, Command::Correct(id, input), StatusCode::OK).await
        }
        EdgeCorrection::Owner(input) => {
            if let Err(reply) = authenticate(&h, &s).await {
                return reply.into_response();
            }
            let key = match idempotency_key(&h) {
                Ok(value) => value,
                Err((status, code, message)) => return error(status, code, message),
            };
            match s.workspace.update_owner_edge(&id, &input, &key).await {
                Ok(result) => {
                    let mut response = Json(json!({"data": result["edge"]})).into_response();
                    private_headers(&mut response, s.workspace.epoch());
                    response
                }
                Err(reason) => command_error(reason),
            }
        }
    }
}
async fn remove(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(i): Json<Removal>,
) -> Response {
    execute(s, h, Command::Remove(id, i), StatusCode::NO_CONTENT).await
}

impl Workspace {
    pub async fn create_related_card(
        &self,
        key: &str,
        input: RelatedCard,
    ) -> Result<super::EntityRecord, WorkspaceError> {
        let value = run(self, key, Command::Related(input), StatusCode::CREATED).await?;
        serde_json::from_value(value).map_err(|e| WorkspaceError::Invalid(e.to_string()))
    }
}

async fn revise_claim(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(i): Json<ClaimRevision>,
) -> Response {
    execute(s, h, Command::ReviseClaim(id, i), StatusCode::CREATED).await
}

async fn save_revision(
    tx: &mut Tx<'_>,
    id: &str,
    i: ClaimRevision,
) -> Result<Value, WorkspaceError> {
    let current: i64 = sqlx::query_scalar("SELECT current_revision FROM claim WHERE id=?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(WorkspaceError::NotFound)?;
    if current != i.expected_revision {
        return Err(WorkspaceError::StaleRevision);
    }
    if !matches!(i.speaker_kind.as_str(), "owner" | "party" | "unknown")
        || (i.speaker_kind == "party") != i.speaker_entity_id.is_some()
    {
        return Err(invalid("Select the speaker explicitly, or choose unknown."));
    }
    if let Some(party) = &i.speaker_entity_id
        && !matches!(kind(tx, party, None).await?.as_str(), "person" | "org")
    {
        return Err(invalid("Select a person or organization as the speaker."));
    }
    let (variant,text):(String,String)=sqlx::query_as("SELECT v.id,v.raw_text FROM source_occurrence o JOIN source_variant v ON v.id=o.source_variant_id WHERE o.id=?").bind(&i.occurrence_id).fetch_optional(&mut **tx).await?.ok_or_else(||invalid("Select a saved source occurrence."))?;
    let span = parley_domain::evidence::verify_quote(&text, &i.wording, None)
        .map_err(|_| invalid("Quote is absent or ambiguous in this source."))?;
    let revision = current + 1;
    sqlx::query("INSERT INTO claim_revision(claim_id,revision,wording,knowledge_kind,tier,speaker_kind,speaker_entity_id,occurrence_id,source_variant_id,source_start,source_end,normalized_start,normalized_end,normalization_version,confirmed_by_owner,created_at) VALUES(?,?,?,'observation',1,?,?,?,?,?,?,?,?,?,1,?)")
        .bind(id).bind(revision).bind(&i.wording).bind(&i.speaker_kind).bind(&i.speaker_entity_id).bind(&i.occurrence_id).bind(variant).bind(span.source_start as i64).bind(span.source_end as i64).bind(span.normalized_start as i64).bind(span.normalized_end as i64).bind(parley_domain::evidence::NORMALIZATION_VERSION as i64).bind(now_utc()).execute(&mut **tx).await?;
    // The existing claim revision trigger advances dependent opportunity-interest
    // revisions. Material gates then compare their frozen dependency vectors.
    sqlx::query("UPDATE claim SET current_revision=? WHERE id=?")
        .bind(revision)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    let edges:Vec<(String,i64)>=sqlx::query_as("SELECT e.id,e.revision FROM edge e JOIN edge_evidence ee ON ee.edge_id=e.id WHERE ee.claim_id=? AND ee.claim_revision=? AND e.state='active'").bind(id).bind(current).fetch_all(&mut **tx).await?;
    for (edge, rev) in edges {
        archive(tx, &edge, rev).await?;
        sqlx::query("UPDATE edge_evidence SET claim_revision=? WHERE edge_id=? AND claim_id=? AND claim_revision=?").bind(revision).bind(&edge).bind(id).bind(current).execute(&mut **tx).await?;
        sqlx::query("UPDATE edge SET revision=revision+1 WHERE id=?")
            .bind(&edge)
            .execute(&mut **tx)
            .await?;
        event(tx, "edge", &edge, rev + 1, "edge.evidence_corrected").await?;
    }
    event(tx, "claim", id, revision, "claim.corrected").await?;
    Ok(
        json!({"data":{"id":id,"revision":revision,"wording":i.wording,"occurrence_id":i.occurrence_id,"speaker_kind":i.speaker_kind,"speaker_entity_id":i.speaker_entity_id}}),
    )
}
