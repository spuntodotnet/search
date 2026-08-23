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

/// `GET /_cluster/settings`
pub async fn settings_get(State(st): State<SharedState>, uri: Uri) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("master_timeout");
    p.opt("timeout");
    super::refuser_reglages_non_supportes(&mut p, "GET /_cluster/settings")?;
    p.done()?;
    let (persistants, transitoires) = st.catalog.reglages();
    Ok(Json::ok(json!({
        "persistent": arborescence(&persistants),
        "transient": arborescence(&transitoires),
    })))
}

/// `PUT /_cluster/settings`
///
/// Seul `action.destructive_requires_name` est reconnu ; tout le reste est
/// refuse avec le message d'ES (`not recognized`). Un reglage accepte sans etre
/// applique serait pire qu'un refus : le client croirait avoir change quelque
/// chose.
pub async fn settings_put(
    State(st): State<SharedState>,
    uri: Uri,
    body: axum::body::Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("master_timeout");
    p.opt("timeout");
    super::refuser_reglages_non_supportes(&mut p, "PUT /_cluster/settings")?;
    p.done()?;

    let body = super::parse_body(&body)?;
    let obj = body
        .as_object()
        .ok_or_else(|| EsError::parsing("le corps de [_cluster/settings] doit etre un objet"))?;
    super::expect_only(obj, &["persistent", "transient"], "_cluster/settings")?;

    let lire = |cle: &str| -> EsResult<std::collections::BTreeMap<String, Value>> {
        match obj.get(cle) {
            None | Some(Value::Null) => Ok(Default::default()),
            Some(v) => {
                let mut plat = std::collections::BTreeMap::new();
                aplatir(v, String::new(), &mut plat)?;
                Ok(plat)
            }
        }
    };
    let persistants = lire("persistent")?;
    let transitoires = lire("transient")?;
    st.catalog.poser_reglages(&persistants, &transitoires)?;

    // ES ne rend que ce que **cet appel** a change.
    Ok(Json::ok(json!({
        "acknowledged": true,
        "persistent": arborescence(&sans_null(&persistants)),
        "transient": arborescence(&sans_null(&transitoires)),
    })))
}

fn sans_null(
    m: &std::collections::BTreeMap<String, Value>,
) -> std::collections::BTreeMap<String, Value> {
    m.iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Aplatit `{"action": {"destructive_requires_name": false}}` en
/// `{"action.destructive_requires_name": false}` : ES accepte les deux
/// ecritures, indifferemment.
fn aplatir(
    v: &Value,
    prefixe: String,
    out: &mut std::collections::BTreeMap<String, Value>,
) -> EsResult<()> {
    match v {
        Value::Object(o) => {
            for (cle, valeur) in o {
                let chemin = if prefixe.is_empty() {
                    cle.clone()
                } else {
                    format!("{prefixe}.{cle}")
                };
                aplatir(valeur, chemin, out)?;
            }
            Ok(())
        }
        autre => {
            if prefixe.is_empty() {
                return Err(EsError::parsing(
                    "[_cluster/settings] : un objet de reglages est attendu",
                ));
            }
            out.insert(prefixe, autre.clone());
            Ok(())
        }
    }
}

/// Le chemin de retour : `{"action.x": v}` redevient `{"action": {"x": v}}`.
/// ES rend les valeurs en **chaines**, quel que soit leur type d'entree.
fn arborescence(m: &std::collections::BTreeMap<String, Value>) -> Value {
    let mut racine = serde_json::Map::new();
    for (cle, valeur) in m {
        let segments: Vec<&str> = cle.split('.').collect();
        let mut courant = &mut racine;
        for segment in &segments[..segments.len() - 1] {
            courant = courant
                .entry(segment.to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()))
                .as_object_mut()
                .expect("noeud de reglage");
        }
        let texte = match valeur {
            Value::String(s) => s.clone(),
            autre => autre.to_string(),
        };
        courant.insert(segments[segments.len() - 1].to_string(), json!(texte));
    }
    Value::Object(racine)
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
    p.done()?;
    let vises = crate::selection::resoudre(&st.catalog, &index, &Default::default())?;
    Ok(Json::ok(health_body(&st, vises.len())))
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

async fn cat_indices_inner(st: SharedState, uri: Uri, only: Option<String>) -> EsResult<Response> {
    let mut p = Params::parse(&uri);
    let format = p.opt("format");
    let verbose = p.flag("v", false)?;
    let opts = super::selection_options(&mut p)?;
    p.done()?;

    // `_cat/indices/{expr}` accepte une expression : un motif qui ne trouve
    // rien rend une liste vide, pas un 404.
    let vises = match &only {
        Some(expr) => crate::selection::resoudre(&st.catalog, expr, &opts)?,
        None => st.catalog.list(),
    };
    let mut rows = Vec::new();
    for idx in vises {
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
    // La resolution a deja tranche : un nom concret absent a rendu 404, un
    // motif sans correspondance rend une liste vide — comme chez ES.
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
    const UNITS: [(&str, u64); 4] = [
        ("tb", 1 << 40),
        ("gb", 1 << 30),
        ("mb", 1 << 20),
        ("kb", 1 << 10),
    ];
    for (unit, size) in UNITS {
        if n >= size {
            return format!("{:.1}{unit}", n as f64 / size as f64);
        }
    }
    format!("{n}b")
}

/// `GET /_nodes` — le minimum dont se contentent les clients et les outils.
pub async fn nodes_spec(
    State(st): State<SharedState>,
    axum::extract::Path(spec): axum::extract::Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    // Un seul noeud : ces selecteurs le designent forcement. Tout le reste
    // (`stats`, `os`, `jvm`, `hot_threads`...) demande une autre reponse, pas
    // la meme — le confondre avec `/_nodes` serait mentir.
    if !matches!(spec.as_str(), "_all" | "_local" | "_master") && spec != st.catalog.cluster_uuid {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [/_nodes/{spec}] ; selecteurs acceptes : _all, _local, \
             _master, ou l'identifiant du noeud"
        )));
    }
    nodes(State(st), uri).await
}

pub async fn nodes(State(st): State<SharedState>, uri: Uri) -> EsResult<Json> {
    Params::parse(&uri).done()?;

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
                    "max_content_length_in_bytes": super::MAX_CONTENT_LENGTH as i64,
                },
            }
        }
    })))
}
