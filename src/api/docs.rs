//! Ingestion : `_doc`, `_create`, `_bulk`.

use std::sync::Arc;
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};

use super::search::source_filter;
use super::{elapsed_ms, parse_body, shards_ok, Json, Params, SharedState};
use crate::engine::{Catalog, WriteOutcome};
use crate::error::{EsError, EsResult};
use crate::util;

/// `PUT|POST /{index}/_doc/{id}`
pub async fn index_doc(
    State(st): State<SharedState>,
    Path((index, id)): Path<(String, String)>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    write_one(st, index, Some(id), uri, body, false).await
}

/// `POST /{index}/_doc` — identifiant genere par le serveur.
pub async fn index_auto_id(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    write_one(st, index, None, uri, body, false).await
}

/// `PUT|POST /{index}/_create/{id}` — echoue si le document existe.
pub async fn create_doc(
    State(st): State<SharedState>,
    Path((index, id)): Path<(String, String)>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    write_one(st, index, Some(id), uri, body, true).await
}

async fn write_one(
    st: SharedState,
    index: String,
    id: Option<String>,
    uri: Uri,
    body: Bytes,
    require_absent: bool,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let refresh = p.refresh()?;
    p.opt("timeout");
    p.opt("op_type");
    p.done()?;

    let source = parse_body(&body)?;
    if !source.is_object() {
        return Err(EsError::mapper_parsing(
            "le corps du document doit etre un objet JSON",
        ));
    }
    let idx = st.catalog.get(&index)?;
    let id = id.unwrap_or_else(util::random_uuid);

    let outcome = {
        let idx = idx.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            let out = idx.index_doc(&id, &source, require_absent)?;
            if refresh {
                idx.refresh()?;
            }
            Ok::<_, EsError>(out)
        })
        .await
        .map_err(|e| EsError::internal(format!("ecriture: {e}")))??
    };

    let status = if outcome.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(Json(status, write_body(&index, &id, &outcome)))
}

fn write_body(index: &str, id: &str, out: &WriteOutcome) -> Value {
    json!({
        "_index": index,
        "_id": id,
        "_version": out.version,
        "result": if out.created { "created" } else { "updated" },
        "_shards": shards_ok(),
        "_seq_no": out.seq_no,
        "_primary_term": 1,
    })
}

/// `GET /{index}/_doc/{id}`
pub async fn get_doc(
    State(st): State<SharedState>,
    Path((index, id)): Path<(String, String)>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let filter = source_filter(&mut p)?;
    p.opt("realtime");
    p.opt("preference");
    p.done()?;

    let idx = st.catalog.get(&index)?;
    let found = {
        let idx = idx.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || idx.get_doc(&id))
            .await
            .map_err(|e| EsError::internal(format!("get: {e}")))??
    };

    match found {
        None => Ok(Json(
            StatusCode::NOT_FOUND,
            json!({"_index": index, "_id": id, "found": false}),
        )),
        Some(res) => {
            let mut o = Map::new();
            o.insert("_index".into(), json!(index));
            o.insert("_id".into(), json!(id));
            o.insert("_version".into(), json!(res.version));
            o.insert("_seq_no".into(), json!(res.seq_no));
            o.insert("_primary_term".into(), json!(1));
            o.insert("found".into(), json!(true));
            if let Some(src) = filter.apply(res.source) {
                o.insert("_source".into(), src);
            }
            Ok(Json::ok(Value::Object(o)))
        }
    }
}

/// `HEAD /{index}/_doc/{id}`
pub async fn head_doc(
    State(st): State<SharedState>,
    Path((index, id)): Path<(String, String)>,
) -> Response {
    let Ok(idx) = st.catalog.get(&index) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match tokio::task::spawn_blocking(move || idx.get_doc(&id)).await {
        Ok(Ok(Some(_))) => StatusCode::OK.into_response(),
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `DELETE /{index}/_doc/{id}`
pub async fn delete_doc(
    State(st): State<SharedState>,
    Path((index, id)): Path<(String, String)>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let refresh = p.refresh()?;
    p.opt("timeout");
    p.done()?;

    let idx = st.catalog.get(&index)?;
    let outcome = {
        let idx = idx.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            let out = idx.delete_doc(&id)?;
            if refresh {
                idx.refresh()?;
            }
            Ok::<_, EsError>(out)
        })
        .await
        .map_err(|e| EsError::internal(format!("suppression: {e}")))??
    };

    match outcome {
        Some(out) => Ok(Json::ok(json!({
            "_index": index,
            "_id": id,
            "_version": out.version,
            "result": "deleted",
            "_shards": shards_ok(),
            "_seq_no": out.seq_no,
            "_primary_term": 1,
        }))),
        None => Ok(Json(
            StatusCode::NOT_FOUND,
            json!({
                "_index": index,
                "_id": id,
                "_version": 1,
                "result": "not_found",
                "_shards": shards_ok(),
                "_seq_no": 0,
                "_primary_term": 1,
            }),
        )),
    }
}

// ---------------------------------------------------------------------------
// _bulk
// ---------------------------------------------------------------------------

/// `POST /_bulk`
pub async fn bulk(State(st): State<SharedState>, uri: Uri, body: Bytes) -> EsResult<Json> {
    bulk_inner(st, None, uri, body).await
}

/// `POST /{index}/_bulk`
pub async fn bulk_index(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    bulk_inner(st, Some(index), uri, body).await
}

async fn bulk_inner(
    st: SharedState,
    default_index: Option<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let started = Instant::now();
    let mut p = Params::parse(&uri);
    let refresh = p.refresh()?;
    p.opt("timeout");
    p.done()?;

    let catalog = st.catalog.clone();
    let (items, touched) = tokio::task::spawn_blocking(move || {
        run_bulk(&catalog, default_index.as_deref(), &body, refresh)
    })
    .await
    .map_err(|e| EsError::internal(format!("bulk: {e}")))??;

    if refresh {
        for name in touched {
            if let Ok(idx) = st.catalog.get(&name) {
                let _ = tokio::task::spawn_blocking(move || idx.refresh()).await;
            }
        }
    }

    let errors = items
        .iter()
        .any(|it| it.as_object().and_then(|o| o.values().next()).is_some_and(is_error_item));

    Ok(Json::ok(json!({
        "errors": errors,
        "took": elapsed_ms(started),
        "items": items,
    })))
}

fn is_error_item(v: &Value) -> bool {
    v.get("error").is_some()
}

/// Une action `_bulk` decodee.
struct Action {
    op: String,
    index: String,
    id: Option<String>,
}

fn run_bulk(
    catalog: &Arc<Catalog>,
    default_index: Option<&str>,
    body: &Bytes,
    forced_refresh: bool,
) -> EsResult<(Vec<Value>, Vec<String>)> {
    let text = std::str::from_utf8(body)
        .map_err(|_| EsError::parsing("le corps de [_bulk] doit etre de l'UTF-8"))?;

    let mut items: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    let mut lines = text.split('\n').enumerate().peekable();

    while let Some((lineno, raw)) = lines.next() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line).map_err(|e| {
            EsError::parsing(format!(
                "[_bulk] ligne {} : action illisible ({e})",
                lineno + 1
            ))
        })?;

        let action = match parse_action(&value, default_index, lineno + 1) {
            Ok(a) => a,
            Err(e) => return Err(e),
        };

        // Les actions `index`/`create` sont suivies du document ; `delete` non.
        let source = if action.op == "delete" {
            None
        } else {
            let Some((slineno, sraw)) = lines.next() else {
                return Err(EsError::parsing(format!(
                    "[_bulk] ligne {} : action [{}] sans document",
                    lineno + 1,
                    action.op
                )));
            };
            let doc: Value = serde_json::from_str(sraw.trim()).map_err(|e| {
                EsError::parsing(format!(
                    "[_bulk] ligne {} : document illisible ({e})",
                    slineno + 1
                ))
            })?;
            Some(doc)
        };

        if !touched.contains(&action.index) {
            touched.push(action.index.clone());
        }
        items.push(execute_action(catalog, &action, source, forced_refresh));
    }

    Ok((items, touched))
}

fn parse_action(value: &Value, default_index: Option<&str>, lineno: usize) -> EsResult<Action> {
    let obj = value.as_object().ok_or_else(|| {
        EsError::parsing(format!("[_bulk] ligne {lineno} : l'action doit etre un objet"))
    })?;
    if obj.len() != 1 {
        return Err(EsError::parsing(format!(
            "[_bulk] ligne {lineno} : une action et une seule est attendue par ligne"
        )));
    }
    let (op, meta) = obj.iter().next().unwrap();
    if op == "update" {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas l'action [update] dans [_bulk] (voir docs/compat.md)",
        ));
    }
    if !matches!(op.as_str(), "index" | "create" | "delete") {
        return Err(EsError::parsing(format!(
            "[_bulk] ligne {lineno} : action inconnue [{op}] ; actions supportees : index, \
             create, delete"
        )));
    }
    let meta = meta.as_object().ok_or_else(|| {
        EsError::parsing(format!(
            "[_bulk] ligne {lineno} : les metadonnees de [{op}] doivent etre un objet"
        ))
    })?;
    for key in meta.keys() {
        if !matches!(key.as_str(), "_index" | "_id") {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas la metadonnee [_bulk] [{key}] (ligne {lineno}) ; cles \
                 acceptees : _index, _id"
            )));
        }
    }
    let index = meta
        .get("_index")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| default_index.map(str::to_string))
        .ok_or_else(|| {
            EsError::illegal_argument(format!(
                "[_bulk] ligne {lineno} : [_index] est requis quand l'URL n'en fournit pas"
            ))
        })?;
    let id = meta.get("_id").and_then(Value::as_str).map(str::to_string);

    if op == "delete" && id.is_none() {
        return Err(EsError::illegal_argument(format!(
            "[_bulk] ligne {lineno} : [_id] est requis pour l'action [delete]"
        )));
    }
    Ok(Action {
        op: op.clone(),
        index,
        id,
    })
}

fn execute_action(
    catalog: &Arc<Catalog>,
    action: &Action,
    source: Option<Value>,
    forced_refresh: bool,
) -> Value {
    let id = action
        .id
        .clone()
        .unwrap_or_else(util::random_uuid);

    let result = (|| -> EsResult<(StatusCode, &'static str, WriteOutcome)> {
        let idx = catalog.get(&action.index)?;
        match action.op.as_str() {
            "delete" => match idx.delete_doc(&id)? {
                Some(out) => Ok((StatusCode::OK, "deleted", out)),
                None => Ok((
                    StatusCode::NOT_FOUND,
                    "not_found",
                    WriteOutcome {
                        version: 1,
                        seq_no: 0,
                        created: false,
                    },
                )),
            },
            op => {
                let doc = source.as_ref().ok_or_else(|| {
                    EsError::parsing(format!("[_bulk] action [{op}] sans document"))
                })?;
                if !doc.is_object() {
                    return Err(EsError::mapper_parsing(
                        "le document doit etre un objet JSON",
                    ));
                }
                let out = idx.index_doc(&id, doc, op == "create")?;
                let status = if out.created {
                    StatusCode::CREATED
                } else {
                    StatusCode::OK
                };
                Ok((status, if out.created { "created" } else { "updated" }, out))
            }
        }
    })();

    let mut item = Map::new();
    item.insert("_index".into(), json!(action.index));
    item.insert("_id".into(), json!(id));

    match result {
        Ok((status, result_name, out)) => {
            item.insert("_version".into(), json!(out.version));
            item.insert("result".into(), json!(result_name));
            if forced_refresh {
                item.insert("forced_refresh".into(), json!(true));
            }
            item.insert("_shards".into(), shards_ok());
            item.insert("_seq_no".into(), json!(out.seq_no));
            item.insert("_primary_term".into(), json!(1));
            item.insert("status".into(), json!(status.as_u16()));
        }
        Err(e) => {
            item.insert("status".into(), json!(e.status.as_u16()));
            item.insert("error".into(), e.cause());
        }
    }

    json!({ action.op.as_str(): Value::Object(item) })
}
