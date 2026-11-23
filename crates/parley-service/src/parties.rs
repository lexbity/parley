use super::materials::{material_receipt_replay, material_receipt_store};
use super::{
    AppState, Workspace, WorkspaceError, authenticate, canonical_markdown, command_error, digest,
    error, idempotency_key, new_id, now_utc, private_headers,
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use std::collections::{BTreeSet, HashMap};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_ROWS: usize = 1000;
type Tx<'a> = Transaction<'a, Sqlite>;
fn invalid(reason: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Invalid(reason.into())
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/directory", get(directory))
        .route("/api/v1/entities/{id}/usage", get(usage))
        .route("/api/v1/entities/{id}/retire", post(retire))
        .route("/api/v1/party-imports", post(stage))
        .route("/api/v1/party-imports/{id}/preview", post(preview))
        .route("/api/v1/party-imports/{id}/commit", post(commit))
        .route("/api/v1/entities/{id}/attachments", post(attach))
        .route(
            "/api/v1/entities/{id}/attachments/{reference}",
            delete(unlink),
        )
        .route(
            "/api/v1/entities/{id}/attachments/{reference}/content",
            get(content),
        )
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mapping {
    party_type: String,
    name_column: usize,
    focus_column: Option<usize>,
    notes_column: Option<usize>,
    #[serde(default)]
    excluded_records: BTreeSet<usize>,
    #[serde(default)]
    distinct_records: BTreeSet<usize>,
}
#[derive(Serialize, Deserialize)]
struct Approval {
    preview_id: String,
    digest: String,
}
#[derive(Deserialize)]
struct Bind {
    upload_id: String,
}

struct Csv {
    headers: Vec<String>,
    rows: Vec<(usize, Vec<String>)>,
}

fn parse_csv(text: &str) -> Result<Csv, WorkspaceError> {
    if text.len() > MAX_BYTES {
        return Err(invalid(format!(
            "Split your CSV into files of at most {MAX_BYTES} bytes."
        )));
    }
    if text.trim().is_empty() {
        return Err(invalid("CSV needs a header row."));
    }
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    let mut record = 1;
    let mut field = String::new();
    let mut fields = Vec::new();
    let mut headers = None;
    let mut rows = Vec::new();
    let mut quoted = false;
    let mut closed = false;
    loop {
        let ch = chars.next();
        if quoted {
            match ch {
                Some('"') if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                Some('"') => {
                    quoted = false;
                    closed = true;
                }
                Some(c) => field.push(c),
                None => return Err(invalid(format!("Unclosed quote in CSV record {record}."))),
            }
            continue;
        }
        match ch {
            Some('"') if field.is_empty() && !closed => quoted = true,
            Some(',') => {
                fields.push(std::mem::take(&mut field));
                closed = false;
            }
            Some('\n' | '\r') | None => {
                if ch == Some('\r') && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                fields.push(std::mem::take(&mut field));
                if let Some(header) = &headers {
                    let header: &Vec<String> = header;
                    if fields.iter().any(|v| !v.trim().is_empty()) {
                        if fields.len() != header.len() {
                            return Err(invalid(format!(
                                "CSV record {record} has {} columns; expected {}.",
                                fields.len(),
                                header.len()
                            )));
                        }
                        rows.push((record, std::mem::take(&mut fields)));
                        if rows.len() > MAX_ROWS {
                            return Err(invalid(format!(
                                "Split your CSV into files of at most {MAX_ROWS} data rows."
                            )));
                        }
                    }
                } else {
                    headers = Some(std::mem::take(&mut fields));
                }
                fields.clear();
                closed = false;
                record += 1;
                if ch.is_none() {
                    break;
                }
            }
            Some('"') => return Err(invalid(format!("Unexpected quote in CSV record {record}."))),
            Some(_) if closed => {
                return Err(invalid(format!(
                    "Unexpected text after closing quote in CSV record {record}."
                )));
            }
            Some(c) => field.push(c),
        }
    }
    Ok(Csv {
        headers: headers.unwrap_or_default(),
        rows,
    })
}

#[derive(Serialize)]
struct PreviewRow {
    source_record: usize,
    name: String,
    focus: String,
    notes: String,
    duplicates: Vec<String>,
    decision: &'static str,
}

async fn project(
    tx: &mut Tx<'_>,
    csv: &Csv,
    map: &Mapping,
) -> Result<Vec<PreviewRow>, WorkspaceError> {
    let cols: Vec<_> = [Some(map.name_column), map.focus_column, map.notes_column]
        .into_iter()
        .flatten()
        .collect();
    if !matches!(map.party_type.as_str(), "person" | "org")
        || cols.iter().any(|c| *c >= csv.headers.len())
        || cols.iter().collect::<BTreeSet<_>>().len() != cols.len()
    {
        return Err(invalid("Select a party type and distinct valid columns."));
    }
    let records: BTreeSet<_> = csv.rows.iter().map(|r| r.0).collect();
    if !map.excluded_records.is_subset(&records)
        || !map.distinct_records.is_subset(&records)
        || !map.distinct_records.is_disjoint(&map.excluded_records)
    {
        return Err(invalid(
            "Row decisions must identify distinct source records in this preview.",
        ));
    }
    let mut names: HashMap<String, Vec<usize>> = HashMap::new();
    for (n, row) in &csv.rows {
        names
            .entry(row[map.name_column].trim().to_ascii_lowercase())
            .or_default()
            .push(*n);
    }
    let mut result = Vec::new();
    for (n, row) in &csv.rows {
        let name = row[map.name_column].trim().to_owned();
        let focus = map.focus_column.map(|i| row[i].clone()).unwrap_or_default();
        let notes = map
            .notes_column
            .map(|i| canonical_markdown(&row[i]))
            .unwrap_or_default();
        let mut duplicates = Vec::new();
        if !name.is_empty() {
            for num in &names[&name.to_ascii_lowercase()] {
                if n != num {
                    duplicates.push(format!("CSV record {num}"));
                    break;
                }
            }
            let existing:Vec<(String,String,String,String)>=sqlx::query_as("SELECT id,kind,name,focus FROM entity WHERE kind IN ('person','org') AND name=? COLLATE NOCASE ORDER BY id LIMIT 20").bind(&name).fetch_all(&mut **tx).await?;
            duplicates.extend(
                existing
                    .into_iter()
                    .map(|(id, kind, name, focus)| format!("{name} · {kind} · {focus} · {id}")),
            );
        }
        let decision = if map.excluded_records.contains(n) {
            "skip"
        } else if name.is_empty()
            || name.len() > 500
            || focus.chars().count() > 250
            || notes.len() > 512 * 1024
        {
            "error"
        } else if !duplicates.is_empty() && !map.distinct_records.contains(n) {
            "review"
        } else {
            "create"
        };
        result.push(PreviewRow {
            source_record: *n,
            name,
            focus,
            notes,
            duplicates,
            decision,
        });
    }
    Ok(result)
}

enum Command {
    Stage(String, Csv),
    Preview(String, Mapping),
    Commit(String, Approval),
    Attach(String, String),
    Unlink(String, String),
    Retire(String, i64, String),
}

async fn execute(
    state: &AppState,
    headers: &HeaderMap,
    cmd: Command,
    canonical: String,
    status: StatusCode,
) -> Response {
    let key = match idempotency_key(headers) {
        Ok(k) => k,
        Err((s, c, m)) => return error(s, c, m),
    };
    let csv_command = matches!(cmd, Command::Preview(..) | Command::Commit(..));
    let run=async {
        let ws=&state.workspace;
        let mut tx=ws.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(old)=material_receipt_replay(&mut tx,ws.epoch(),&key,&canonical).await? {return Ok(old);}
        let mut transient_stage=None;
        let result=match cmd {
            Command::Stage(raw,csv)=>{
                let id=new_id("party-import");
                transient_stage=Some(id.clone());
                sqlx::query("DELETE FROM party_import_stage WHERE expires_at <= unixepoch() OR epoch<>?").bind(ws.epoch()).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO party_import_stage(id,epoch,source_digest,csv_text,expires_at) VALUES(?,?,?,?,unixepoch()+1800)").bind(&id).bind(ws.epoch()).bind(digest(raw.as_bytes())).bind(raw).execute(&mut *tx).await?;
                json!({"stage_id":id,"headers":csv.headers,"row_count":csv.rows.len(),"max_rows":MAX_ROWS,"max_bytes":MAX_BYTES})
            },
            Command::Preview(id,mapping)=>{
                transient_stage=Some(id.clone());
                let (raw,source)=staged(&mut tx,ws,&id).await?;
                let rows=project(&mut tx,&parse_csv(&raw)?,&mapping).await?;
                let preview_id=new_id(&id);
                let hash=preview_digest(&source,ws.epoch(),&mapping,&rows);
                sqlx::query("UPDATE party_import_stage SET preview_id=?,preview_digest=?,mapping=? WHERE id=?").bind(&preview_id).bind(&hash).bind(json!(mapping).to_string()).bind(id).execute(&mut *tx).await?;
                let count=|d:&str|rows.iter().filter(|r|r.decision==d).count();
                json!({"preview_id":preview_id,"digest":hash,"rows":rows,"create_count":count("create"),"skip_count":count("skip"),"error_count":count("review")+count("error")})
            },
            Command::Commit(id,approval)=>commit_rows(&mut tx,ws,&id,approval).await?,
            Command::Retire(id,revision,etag)=>retire_card(&mut tx,ws,&id,revision,&etag).await?,
            Command::Attach(id,upload)=>bind_upload(&mut tx,&id,&upload).await?,
            Command::Unlink(id,reference)=>{
                let result=sqlx::query("DELETE FROM entity_attachment WHERE id=? AND entity_id=?").bind(reference).bind(&id).execute(&mut *tx).await?;
                if result.rows_affected()!=1{return Err(WorkspaceError::NotFound);}
                emit_event(&mut tx,&id,"entity.attachment_removed").await?;
                Value::Null
            },
        };
        material_receipt_store(&mut tx,ws.epoch(),&key,&canonical,status.as_u16(),&result).await?;
        if let Some(stage)=transient_stage {
            sqlx::query("INSERT INTO party_stage_receipt(stage_id,epoch,key) VALUES(?,?,?)").bind(stage).bind(ws.epoch()).bind(&key).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok::<Value, WorkspaceError>(result)
    }.await;
    match run {
        Ok(value) => {
            let mut r = if status == StatusCode::NO_CONTENT {
                status.into_response()
            } else {
                (status, Json(json!({"data":value}))).into_response()
            };
            private_headers(&mut r, state.workspace.epoch());
            r
        }
        Err(WorkspaceError::StaleRevision) if csv_command => error(
            StatusCode::PRECONDITION_FAILED,
            "PREVIEW_STALE",
            "CSV preview expired or changed. Stage again and review the rows.",
        ),
        Err(e) => command_error(e),
    }
}

async fn staged(
    tx: &mut Tx<'_>,
    ws: &Workspace,
    id: &str,
) -> Result<(String, String), WorkspaceError> {
    sqlx::query_as("SELECT csv_text,source_digest FROM party_import_stage WHERE id=? AND epoch=? AND expires_at>unixepoch()").bind(id).bind(ws.epoch()).fetch_optional(&mut **tx).await?.ok_or(WorkspaceError::StaleRevision)
}
fn preview_digest(source: &str, epoch: &str, mapping: &Mapping, rows: &[PreviewRow]) -> String {
    digest(json!([source, epoch, mapping, rows]).to_string().as_bytes())
}
async fn emit_event(tx: &mut Tx<'_>, id: &str, kind: &str) -> Result<(), WorkspaceError> {
    sqlx::query("INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json) SELECT ?,?,'entity',id,revision,?,'{}' FROM entity WHERE id=?").bind(new_id(id)).bind(now_utc()).bind(kind).bind(id).execute(&mut **tx).await?;
    Ok(())
}

async fn commit_rows(
    tx: &mut Tx<'_>,
    ws: &Workspace,
    id: &str,
    approval: Approval,
) -> Result<Value, WorkspaceError> {
    let (raw, source) = staged(tx, ws, id).await?;
    let mapping: String = sqlx::query_scalar(
        "SELECT mapping FROM party_import_stage WHERE id=? AND preview_id=? AND preview_digest=?",
    )
    .bind(id)
    .bind(&approval.preview_id)
    .bind(&approval.digest)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(WorkspaceError::StaleRevision)?;
    let map: Mapping = serde_json::from_str(&mapping).map_err(|e| invalid(e.to_string()))?;
    let rows = project(tx, &parse_csv(&raw)?, &map).await?;
    if preview_digest(&source, ws.epoch(), &map, &rows) != approval.digest {
        return Err(WorkspaceError::StaleRevision);
    }
    if rows
        .iter()
        .any(|r| !matches!(r.decision, "create" | "skip"))
        || !rows.iter().any(|r| r.decision == "create")
    {
        return Err(invalid(
            "Review duplicates and correct or exclude invalid rows before importing.",
        ));
    }
    let now = now_utc();
    sqlx::query(
        "INSERT INTO party_import_batch(id,source_digest,mapping,created_at) VALUES(?,?,?,?)",
    )
    .bind(id)
    .bind(source)
    .bind(mapping)
    .bind(&now)
    .execute(&mut **tx)
    .await?;
    let mut created = Vec::new();
    let count = rows.len();
    for row in rows.into_iter().filter(|r| r.decision == "create") {
        let party = new_id(&row.name);
        sqlx::query("INSERT INTO entity(id,kind,name,focus,notes_md,revision,created_at,updated_at) VALUES(?,?,?,?,?,1,?,?)").bind(&party).bind(&map.party_type).bind(row.name).bind(row.focus).bind(row.notes).bind(&now).bind(&now).execute(&mut **tx).await?;
        sqlx::query(
            "INSERT INTO party_import_origin(party_id,batch_id,source_record) VALUES(?,?,?)",
        )
        .bind(&party)
        .bind(id)
        .bind(row.source_record as i64)
        .execute(&mut **tx)
        .await?;
        emit_event(tx, &party, "entity.created").await?;
        created.push(json!({"source_record":row.source_record,"party_id":party}));
    }
    sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM party_import_stage WHERE id=?")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(json!({"batch_id":id,"created":created,"skipped_count":count-created.len()}))
}

pub(super) async fn bind_upload(
    tx: &mut Tx<'_>,
    id: &str,
    upload: &str,
) -> Result<Value, WorkspaceError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM graph_card WHERE id=?)")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    if !exists {
        return Err(WorkspaceError::NotFound);
    }
    let attached: Option<String> =
        sqlx::query_scalar("SELECT entity_id FROM entity_attachment WHERE upload_id=?")
            .bind(upload)
            .fetch_optional(&mut **tx)
            .await?;
    if let Some(parent) = attached {
        if parent != id {
            return Err(invalid(
                "Upload the file for this card to create a separate reference.",
            ));
        }
    } else {
        let result=sqlx::query("INSERT INTO entity_attachment(id,entity_id,upload_id,filename,content_type,size,sha256,created_at) SELECT ?,?,id,filename,mime,size,blob_sha256,? FROM blob_upload WHERE id=? AND unixepoch(created_at)>unixepoch()-1800").bind(new_id(upload)).bind(id).bind(now_utc()).bind(upload).execute(&mut **tx).await?;
        if result.rows_affected() != 1 {
            return Err(invalid(
                "Upload expired or unavailable. Upload the file again.",
            ));
        }
        emit_event(tx, id, "entity.attachment_added").await?;
    }
    let (reference,filename,content_type,size,sha256,created_at,shared,reviewed):(String,String,String,i64,String,String,bool,bool)=sqlx::query_as("SELECT a.id,a.filename,a.content_type,a.size,a.sha256,a.created_at,u.share_permission,u.reviewed_by_owner FROM entity_attachment a JOIN blob_upload u ON u.id=a.upload_id WHERE a.entity_id=? AND a.upload_id=?").bind(id).bind(upload).fetch_one(&mut **tx).await?;
    Ok(
        json!({"id":reference,"filename":filename,"content_type":content_type,"size":size,"sha256":sha256,"created_at":created_at,"share_permission":shared,"reviewed_by_owner":reviewed}),
    )
}

async fn stage(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(e) = authenticate(&headers, &state).await {
        return e.into_response();
    }
    let raw = match std::str::from_utf8(&body) {
        Ok(v) => v,
        Err(_) => {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "CSV_ENCODING",
                "CSV must use UTF-8 encoding.",
            );
        }
    };
    let csv = match parse_csv(raw) {
        Ok(v) => v,
        Err(e) => return command_error(e),
    };
    let hash = digest(format!("party-import:{}", digest(&body)).as_bytes());
    execute(
        &state,
        &headers,
        Command::Stage(raw.to_owned(), csv),
        hash,
        StatusCode::CREATED,
    )
    .await
}
async fn preview(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(m): Json<Mapping>,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    let hash = digest(json!(["preview", id, m]).to_string().as_bytes());
    execute(&s, &h, Command::Preview(id, m), hash, StatusCode::OK).await
}
async fn commit(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(a): Json<Approval>,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    let hash = digest(json!(["commit", id, a]).to_string().as_bytes());
    execute(&s, &h, Command::Commit(id, a), hash, StatusCode::CREATED).await
}
async fn attach(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(b): Json<Bind>,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    let hash = digest(json!(["attach", id, b.upload_id]).to_string().as_bytes());
    execute(
        &s,
        &h,
        Command::Attach(id, b.upload_id),
        hash,
        StatusCode::CREATED,
    )
    .await
}
async fn unlink(
    State(s): State<AppState>,
    Path((id, r)): Path<(String, String)>,
    h: HeaderMap,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    let hash = digest(json!(["unlink", id, r]).to_string().as_bytes());
    execute(&s, &h, Command::Unlink(id, r), hash, StatusCode::NO_CONTENT).await
}
async fn content(
    State(s): State<AppState>,
    Path((id, r)): Path<(String, String)>,
    h: HeaderMap,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    let record: Result<Option<(String, i64)>, _> =
        sqlx::query_as("SELECT sha256,size FROM entity_attachment WHERE id=? AND entity_id=?")
            .bind(r)
            .bind(id)
            .fetch_optional(s.workspace.pool())
            .await;
    let (sha, size) = match record {
        Ok(Some(v)) => v,
        Ok(None) => return command_error(WorkspaceError::NotFound),
        Err(e) => return command_error(e.into()),
    };
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let unavailable = || {
        error(
            StatusCode::GONE,
            "BYTES_UNAVAILABLE",
            "Attached bytes are missing or failed verification.",
        )
    };
    let Ok(mut file) = tokio::fs::File::open(s.workspace.blob_path(&sha)).await else {
        return unavailable();
    };
    let mut hash = Sha256::new();
    let mut read_size = 0i64;
    let mut buf = vec![0u8; 65536];
    loop {
        let Ok(count) = file.read(&mut buf).await else {
            return unavailable();
        };
        if count == 0 {
            break;
        }
        read_size += count as i64;
        if read_size > size {
            return unavailable();
        }
        hash.update(&buf[..count]);
    }
    if read_size != size || hex::encode(hash.finalize()) != sha || file.rewind().await.is_err() {
        return unavailable();
    }
    let stream = futures_util::stream::try_unfold(file, |mut file| async move {
        let mut buf = vec![0u8; 65536];
        let n = file.read(&mut buf).await?;
        buf.truncate(n);
        Ok::<_, std::io::Error>((n > 0).then_some((Bytes::from(buf), file)))
    });
    let mut response = (
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CONTENT_DISPOSITION, "attachment"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response();
    if let Ok(length) = size.to_string().parse() {
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, length);
    }
    private_headers(&mut response, s.workspace.epoch());
    response
}

#[derive(Deserialize)]
struct DirectoryQuery {
    kind: Option<String>,
    q: Option<String>,
    batch_id: Option<String>,
    sort: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

async fn directory(
    State(s): State<AppState>,
    h: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<DirectoryQuery>,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    let limit = q.limit.unwrap_or(50);
    let sort = match q.sort.as_deref() {
        None | Some("name") => "name",
        Some("type") => "kind",
        Some("focus") => "focus",
        _ => return command_error(invalid("Unsupported directory sort.")),
    };
    if !(1..=200).contains(&limit)
        || q.q.as_ref().is_some_and(|t| t.len() > 500)
        || q.kind.as_deref().is_some_and(|k| {
            !matches!(
                k,
                "person" | "org" | "resource" | "project" | "source" | "interest" | "artifact"
            )
        })
    {
        return command_error(invalid("Select a supported kind and a page size of 1–200."));
    }
    let scope = digest(
        json!([q.kind, q.q, q.batch_id, sort])
            .to_string()
            .as_bytes(),
    );
    let after = match q.cursor {
        Some(token) => match hex::decode(token)
            .ok()
            .and_then(|v| serde_json::from_slice::<(String, String, String)>(&v).ok())
        {
            Some((binding, val, id)) if binding == scope => Some((val, id)),
            _ => {
                return command_error(invalid(
                    "The directory cursor does not belong to this query.",
                ));
            }
        },
        None => None,
    };
    let run=async {
        let mut tx=s.workspace.pool().begin().await?;
        let cursor:i64=sqlx::query_scalar("SELECT COALESCE(MAX(cursor),0) FROM event").fetch_one(&mut *tx).await?;
        // sort is a closed enum above; all user-provided predicates are bound.
        let mut query=sqlx::QueryBuilder::new("SELECT e.id,e.kind,e.name,e.focus,e.revision FROM graph_card e WHERE ");
        if let Some(k)=q.kind {query.push("e.kind=").push_bind(k);}else{query.push("e.kind IN ('person','org')");}
        if let Some(t)=q.q.filter(|v|!v.is_empty()){query.push(" AND (instr(lower(e.name),lower(").push_bind(t.clone()).push("))>0 OR instr(lower(e.focus),lower(").push_bind(t).push("))>0)");}
        if let Some(b)=q.batch_id{query.push(" AND EXISTS(SELECT 1 FROM party_import_origin o WHERE o.party_id=e.id AND o.batch_id=").push_bind(b).push(")");}
        if let Some((val,id))=after {query.push(format!(" AND (e.{sort} COLLATE NOCASE,e.id) > (")).push_bind(val).push(",").push_bind(id).push(")");}
        query.push(format!(" ORDER BY e.{sort} COLLATE NOCASE,e.id LIMIT ")).push_bind((limit+1) as i64);
        let mut rows:Vec<(String,String,String,String,i64)>=query.build_query_as().fetch_all(&mut *tx).await?;
        let more=rows.len()>limit;
        rows.truncate(limit);
        let next_cursor=if more {rows.last().map(|r|hex::encode(json!([scope,match sort{"kind"=>&r.1,"focus"=>&r.3,_=>&r.2},r.0]).to_string().as_bytes()))}else{None};
        let data:Vec<_>=rows.into_iter().map(|(id,kind,name,focus,revision)|json!({"id":id,"kind":kind,"name":name,"focus":focus,"revision":revision,"etag":super::entity_etag(s.workspace.epoch(),&id,revision)})).collect();
        tx.commit().await?;
        Ok::<_,WorkspaceError>(json!({"data":data,"next_cursor":next_cursor,"cursor":cursor.to_string()}))
    }.await;
    match run {
        Ok(v) => {
            let mut r = Json(v).into_response();
            private_headers(&mut r, s.workspace.epoch());
            r
        }
        Err(e) => command_error(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Retirement {
    expected_revision: i64,
}

async fn usage(State(s): State<AppState>, Path(id): Path<String>, h: HeaderMap) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    match card_usage(&s.workspace, &id).await {
        Ok(value) => {
            let mut r = Json(json!({"data":value})).into_response();
            private_headers(&mut r, s.workspace.epoch());
            r
        }
        Err(e) => command_error(e),
    }
}

async fn card_usage(ws: &Workspace, id: &str) -> Result<Value, WorkspaceError> {
    let mut tx = ws.pool().begin().await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM entity WHERE id=?)")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if !exists {
        return Err(WorkspaceError::NotFound);
    }
    let links:Vec<(String,String,String,String,i64)>=sqlx::query_as("SELECT id,verb,from_id,to_id,count(*) OVER() FROM edge WHERE state='active' AND (from_id=? OR to_id=?) ORDER BY id LIMIT 60").bind(id).bind(id).fetch_all(&mut *tx).await?;
    let opportunities:Vec<(String,String,i64)>=sqlx::query_as("SELECT o.id,o.title,count(*) OVER() FROM opportunity_party p JOIN opportunity o ON o.id=p.opportunity_id WHERE party_id=? ORDER BY o.id LIMIT 60").bind(id).fetch_all(&mut *tx).await?;
    let materials:Vec<(String,String,i64)>=sqlx::query_as("SELECT id,kind,count(*) OVER() FROM material WHERE recipient_id=? OR id IN (SELECT material_id FROM resource_use WHERE resource_id=?) ORDER BY id LIMIT 60").bind(id).bind(id).fetch_all(&mut *tx).await?;
    let recommendations:i64=sqlx::query_scalar("SELECT count(*) FROM match_run WHERE recipient_id=? OR EXISTS(SELECT 1 FROM json_tree(shortlist_json) WHERE type='text' AND atom=?)").bind(id).bind(id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    let counts = json!({"links":links.first().map_or(0,|v|v.4),"opportunities":opportunities.first().map_or(0,|v|v.2),"materials":materials.first().map_or(0,|v|v.2),"recommendations":recommendations});
    Ok(
        json!({"links":links.into_iter().map(|(id,verb,from,to,_)|json!({"id":id,"verb":verb,"from_id":from,"to_id":to})).collect::<Vec<_>>(),"opportunities":opportunities.into_iter().map(|(id,title,_)|json!({"id":id,"title":title})).collect::<Vec<_>>(),"materials":materials.into_iter().map(|(id,kind,_)|json!({"id":id,"kind":kind})).collect::<Vec<_>>(),"counts":counts}),
    )
}

async fn retire(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(i): Json<Retirement>,
) -> Response {
    if let Err(e) = authenticate(&h, &s).await {
        return e.into_response();
    }
    let Some(tag) = h
        .get(header::IF_MATCH)
        .and_then(|h| h.to_str().ok())
        .filter(|v| super::is_strong_etag(v))
    else {
        return error(
            StatusCode::PRECONDITION_REQUIRED,
            "IF_MATCH_REQUIRED",
            "A current entity ETag is required.",
        );
    };
    let hash = digest(
        json!(["retire", id, tag, i.expected_revision])
            .to_string()
            .as_bytes(),
    );
    execute(
        &s,
        &h,
        Command::Retire(id, i.expected_revision, tag.to_owned()),
        hash,
        StatusCode::OK,
    )
    .await
}

async fn retire_card(
    tx: &mut Tx<'_>,
    ws: &Workspace,
    id: &str,
    expected: i64,
    tag: &str,
) -> Result<Value, WorkspaceError> {
    let (kind, revision): (String, i64) =
        sqlx::query_as("SELECT kind,revision FROM graph_card WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(WorkspaceError::NotFound)?;
    if revision != expected || tag != super::entity_etag(ws.epoch(), id, revision) {
        return Err(WorkspaceError::StaleRevision);
    }
    let now = now_utc();
    sqlx::query(
        "UPDATE resource SET readiness='retired',revision=revision+1,updated_at=? WHERE id=?",
    )
    .bind(&now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE entity SET retired_at=?,updated_at=?,revision=? WHERE id=?")
        .bind(&now)
        .bind(&now)
        .bind(revision + 1)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if matches!(kind.as_str(), "person" | "org") {
        sqlx::query("INSERT INTO party_policy(party_id,state) VALUES(?,'closed') ON CONFLICT(party_id) DO UPDATE SET state='closed',revision=revision+1").bind(id).execute(&mut **tx).await?;
    }
    emit_event(tx, id, "entity.retired").await?;
    sqlx::query("UPDATE workspace_meta SET revision=revision+1 WHERE singleton=1")
        .execute(&mut **tx)
        .await?;
    Ok(json!({"id":id,"revision":revision+1,"retired_at":now}))
}
