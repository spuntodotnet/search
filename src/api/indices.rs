//! Cycle de vie d'un index : creation avec mapping explicite, suppression,
//! existence, mapping, refresh.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use super::{expect_only, parse_body, Json, Params, SharedState};
use crate::error::{EsError, EsResult};
use crate::mapping::Mapping;

/// Reglages d'index acceptes et sans effet : ferrite est mono-shard,
/// zero-replique par construction, donc ces valeurs sont deja ce qu'elles
/// decrivent. Tout autre reglage est refuse.
const NOOP_SETTINGS: &[&str] = &[
    "number_of_shards",
    "number_of_replicas",
    "index.number_of_shards",
    "index.number_of_replicas",
];

/// `PUT /{index}` — creation avec mapping explicite obligatoire.
pub async fn create(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("wait_for_active_shards");
    p.opt("timeout");
    p.opt("master_timeout");
    p.done()?;

    let body = parse_body(&body)?;
    let obj = match &body {
        Value::Null => serde_json::Map::new(),
        Value::Object(o) => o.clone(),
        _ => {
            return Err(EsError::parsing(
                "le corps de [PUT /{index}] doit etre un objet",
            ))
        }
    };
    expect_only(&obj, &["mappings", "settings", "aliases"], "PUT /{index}")?;

    if let Some(aliases) = obj.get("aliases") {
        if !aliases.as_object().map(|o| o.is_empty()).unwrap_or(false) {
            return Err(EsError::unsupported(
                "ferrite ne supporte pas les alias d'index",
            ));
        }
    }
    if let Some(settings) = obj.get("settings") {
        check_settings(settings)?;
    }

    let mappings = obj.get("mappings").ok_or_else(|| {
        EsError::illegal_argument(
            "ferrite exige un mapping explicite a la creation de l'index : [mappings.properties] \
             est obligatoire (le mapping dynamique n'est pas supporte)",
        )
    })?;
    let mapping = Mapping::parse(mappings)?;

    st.catalog.create(&index, mapping)?;
    Ok(Json::ok(json!({
        "acknowledged": true,
        "shards_acknowledged": true,
        "index": index,
    })))
}

fn check_settings(settings: &Value) -> EsResult<()> {
    let obj = settings
        .as_object()
        .ok_or_else(|| EsError::parsing("[settings] doit etre un objet"))?;
    // Forme imbriquee : {"index": {...}}
    let mut flat: Vec<(String, &Value)> = Vec::new();
    for (k, v) in obj {
        if k == "index" {
            if let Some(inner) = v.as_object() {
                for (ik, iv) in inner {
                    flat.push((format!("index.{ik}"), iv));
                }
                continue;
            }
        }
        flat.push((k.clone(), v));
    }
    for (key, _) in flat {
        if !NOOP_SETTINGS.contains(&key.as_str()) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas le reglage d'index [{key}] ; reglages acceptes (et sans \
                 effet, ferrite etant mono-shard) : {NOOP_SETTINGS:?}"
            )));
        }
    }
    Ok(())
}

/// `DELETE /{index}`
pub async fn delete(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    // Operations synchrones et immediates : ces delais n'ont rien a attendre.
    p.opt("timeout");
    p.opt("master_timeout");
    let ignore_unavailable = p.flag("ignore_unavailable", false)?;
    p.done()?;

    match st.catalog.delete(&index) {
        Ok(()) => Ok(Json::ok(json!({"acknowledged": true}))),
        Err(e) if ignore_unavailable && e.ty == "index_not_found_exception" => {
            Ok(Json::ok(json!({"acknowledged": true})))
        }
        Err(e) => Err(e),
    }
}

/// `HEAD /{index}` — 200 ou 404, sans corps.
pub async fn exists(State(st): State<SharedState>, Path(index): Path<String>) -> Response {
    if st.catalog.exists(&index) {
        StatusCode::OK.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

/// `GET /{index}`
pub async fn get_index(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    Params::parse(&uri).done()?;
    let idx = st.catalog.get(&index)?;
    Ok(Json::ok(json!({
        index.as_str(): {
            "aliases": {},
            "mappings": idx.mapping.to_json(),
            "settings": {"index": {
                "number_of_shards": "1",
                "number_of_replicas": "0",
                "uuid": idx.uuid,
                "provided_name": index,
                "creation_date": idx.created_at.to_string(),
                "version": {"created": crate::ES_VERSION},
            }},
        }
    })))
}

/// `GET /{index}/_mapping`
pub async fn get_mapping(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    Params::parse(&uri).done()?;
    let idx = st.catalog.get(&index)?;
    Ok(Json::ok(json!({
        index.as_str(): {"mappings": idx.mapping.to_json()}
    })))
}

/// `PUT /{index}/_mapping` — refuse explicitement : modifier un mapping veut
/// dire modifier le schema tantivy, qui est fige.
pub async fn put_mapping(Path(index): Path<String>) -> EsError {
    EsError::unsupported(format!(
        "ferrite ne supporte pas la modification du mapping d'un index existant (index \
         [{index}]) : le schema est fige a la creation"
    ))
}

/// `POST /{index}/_refresh`
pub async fn refresh(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    Params::parse(&uri).done()?;
    let idx = st.catalog.get(&index)?;
    tokio::task::spawn_blocking(move || idx.refresh())
        .await
        .map_err(|e| EsError::internal(format!("refresh: {e}")))??;
    Ok(Json::ok(json!({"_shards": super::shards_ok()})))
}
