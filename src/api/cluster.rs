//! Poignee de main et routes de cluster.
//!
//! ferrite est mono-noeud par construction : ces routes repondent de facon
//! credible et constante (un shard, zero replique, toujours `green`) pour que
//! les clients et les outils ne s'etranglent pas.

use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use super::{Json, Params, SharedState};
use crate::error::{EsError, EsResult};
use crate::{
    BUILD_FLAVOR, BUILD_TYPE, ES_VERSION, FERRITE_VERSION, LUCENE_VERSION,
    MIN_INDEX_COMPAT_VERSION, MIN_WIRE_COMPAT_VERSION, TAGLINE,
};

/// `GET /` — la poignee de main que fait tout client officiel au demarrage.
pub async fn root(State(st): State<SharedState>, uri: Uri) -> EsResult<Json> {
    Params::parse(&uri).done()?;
    Ok(Json::ok(json!({
        "name": st.catalog.node_name,
        "cluster_name": st.catalog.cluster_name,
        "cluster_uuid": st.catalog.cluster_uuid,
        "version": {
            "number": ES_VERSION,
            "build_flavor": BUILD_FLAVOR,
            "build_type": BUILD_TYPE,
            "build_hash": format!("ferrite-{FERRITE_VERSION}"),
            "build_date": build_date(),
            "build_snapshot": false,
            "lucene_version": LUCENE_VERSION,
            "minimum_wire_compatibility_version": MIN_WIRE_COMPAT_VERSION,
            "minimum_index_compatibility_version": MIN_INDEX_COMPAT_VERSION,
        },
        "tagline": TAGLINE,
    })))
}

fn build_date() -> &'static str {
    // Fixe : ferrite n'a pas de date de build a l'execution, et aucun client ne
    // s'en sert pour negocier.
    "2026-01-01T00:00:00.000000000Z"
}

pub async fn health(State(st): State<SharedState>, uri: Uri) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    // Ces parametres d'attente sont sans objet : le cluster est deja `green`.
    p.opt("wait_for_status");
    p.opt("timeout");
    p.opt("level");
    p.done()?;
    Ok(Json::ok(health_body(&st, st.catalog.list().len())))
}

pub async fn health_index(
    State(st): State<SharedState>,
    axum::extract::Path(index): axum::extract::Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("wait_for_status");
    p.opt("timeout");
    p.opt("level");
    p.done()?;
    st.catalog.get(&index)?;
    Ok(Json::ok(health_body(&st, 1)))
}

fn health_body(st: &SharedState, shards: usize) -> Value {
    json!({
        "cluster_name": st.catalog.cluster_name,
        "status": "green",
        "timed_out": false,
        "number_of_nodes": 1,
        "number_of_data_nodes": 1,
        "active_primary_shards": shards,
        "active_shards": shards,
        "relocating_shards": 0,
        "initializing_shards": 0,
        "unassigned_shards": 0,
        "delayed_unassigned_shards": 0,
        "number_of_pending_tasks": 0,
        "number_of_in_flight_fetch": 0,
        "task_max_waiting_in_queue_millis": 0,
        "active_shards_percent_as_number": 100.0,
    })
}

pub async fn cat_health(State(st): State<SharedState>, uri: Uri) -> EsResult<Response> {
    let mut p = Params::parse(&uri);
    let format = p.opt("format");
    let verbose = p.flag("v", false)?;
    p.opt("h");
    p.done()?;

    let shards = st.catalog.list().len();
    let row = json!({
        "epoch": crate::util::now_millis() / 1000,
        "timestamp": "00:00:00",
        "cluster": st.catalog.cluster_name,
        "status": "green",
        "node.total": "1",
        "node.data": "1",
        "shards": shards.to_string(),
        "pri": shards.to_string(),
        "relo": "0",
        "init": "0",
        "unassign": "0",
        "pending_tasks": "0",
        "max_task_wait_time": "-",
        "active_shards_percent": "100.0%",
    });
    Ok(cat_response(&[row], format.as_deref(), verbose))
}

pub async fn cat_indices(State(st): State<SharedState>, uri: Uri) -> EsResult<Response> {
    cat_indices_inner(st, uri, None).await
}

pub async fn cat_indices_one(
    State(st): State<SharedState>,
    axum::extract::Path(index): axum::extract::Path<String>,
    uri: Uri,
) -> EsResult<Response> {
    cat_indices_inner(st, uri, Some(index)).await
}

async fn cat_indices_inner(
    st: SharedState,
    uri: Uri,
    only: Option<String>,
) -> EsResult<Response> {
    let mut p = Params::parse(&uri);
    let format = p.opt("format");
    let verbose = p.flag("v", false)?;
    p.opt("h");
    p.opt("s");
    p.opt("bytes");
    p.done()?;

    let mut rows = Vec::new();
    for idx in st.catalog.list() {
        if let Some(name) = &only {
            if &idx.name != name {
                continue;
            }
        }
        rows.push(json!({
            "health": "green",
            "status": "open",
            "index": idx.name,
            "uuid": idx.uuid,
            "pri": "1",
            "rep": "0",
            "docs.count": idx.doc_count().to_string(),
            "docs.deleted": "0",
            "store.size": human_bytes(idx.store_size()),
            "pri.store.size": human_bytes(idx.store_size()),
            "dataset.size": human_bytes(idx.store_size()),
        }));
    }
    if let Some(name) = &only {
        if rows.is_empty() {
            return Err(EsError::index_not_found(name));
        }
    }
    Ok(cat_response(&rows, format.as_deref(), verbose))
}

/// Les routes `_cat` renvoient du texte aligne par defaut, du JSON avec
/// `?format=json`.
fn cat_response(rows: &[Value], format: Option<&str>, verbose: bool) -> Response {
    match format {
        Some("json") => Json::ok(json!(rows)).into_response(),
        _ => {
            let mut out = String::new();
            let columns: Vec<String> = rows
                .first()
                .and_then(Value::as_object)
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
            if verbose && !columns.is_empty() {
                out.push_str(&columns.join(" "));
                out.push('\n');
            }
            for row in rows {
                let cells: Vec<String> = columns
                    .iter()
                    .map(|c| row[c].as_str().unwrap_or("").to_string())
                    .collect();
                out.push_str(&cells.join(" "));
                out.push('\n');
            }
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; charset=UTF-8")],
                out,
            )
                .into_response()
        }
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [(&str, u64); 4] = [("tb", 1 << 40), ("gb", 1 << 30), ("mb", 1 << 20), ("kb", 1 << 10)];
    for (unit, size) in UNITS {
        if n >= size {
            return format!("{:.1}{unit}", n as f64 / size as f64);
        }
    }
    format!("{n}b")
}

/// `GET /_nodes` — le minimum dont se contentent les clients et les outils.
pub async fn nodes(State(st): State<SharedState>, uri: Uri) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("flat_settings");
    p.done()?;

    let node_id = &st.catalog.cluster_uuid;
    Ok(Json::ok(json!({
        "_nodes": {"total": 1, "successful": 1, "failed": 0},
        "cluster_name": st.catalog.cluster_name,
        "nodes": {
            node_id.as_str(): {
                "name": st.catalog.node_name,
                "transport_address": "127.0.0.1:9300",
                "host": "127.0.0.1",
                "ip": "127.0.0.1",
                "version": ES_VERSION,
                "build_flavor": BUILD_FLAVOR,
                "build_type": BUILD_TYPE,
                "build_hash": format!("ferrite-{FERRITE_VERSION}"),
                "roles": ["data", "data_content", "ingest", "master"],
                "attributes": {},
                "http": {
                    "bound_address": ["0.0.0.0:9200"],
                    "publish_address": "127.0.0.1:9200",
                    "max_content_length_in_bytes": 104_857_600i64,
                },
            }
        }
    })))
}
