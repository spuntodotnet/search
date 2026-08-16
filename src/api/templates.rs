//! `_index_template` et `_template` : poser, lire, supprimer un template.
//!
//! Les deux familles partagent tout sauf leur forme de rendu — voir
//! [`crate::templates`] pour ce qu'elles font a la creation d'un index.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use serde_json::{json, Map, Value};

use super::{parse_body, Json, Params, SharedState};
use crate::error::{EsError, EsResult};
use crate::search::glob_match;

// ---------------------------------------------------------------------------
// Composables : `_index_template`
// ---------------------------------------------------------------------------

/// `PUT|POST /_index_template/{nom}`
pub async fn poser_composable(
    State(st): State<SharedState>,
    Path(nom): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let create = lire_parametres(&uri)?;
    valider_nom(&nom)?;
    let tpl = crate::templates::lire_composable(&parse_body(&body)?)?;
    st.catalog.poser_template(&nom, tpl, true, create)?;
    Ok(Json::ok(json!({"acknowledged": true})))
}

/// `GET /_index_template` et `GET /_index_template/{nom}`
pub async fn lire_composables(
    State(st): State<SharedState>,
    nom: Option<Path<String>>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("master_timeout");
    p.opt("local");
    super::refuser_reglages_non_supportes(&mut p, "/_index_template")?;
    p.done()?;

    let motif = nom.map(|Path(n)| n).unwrap_or_else(|| "*".to_string());
    valider_nom(&motif)?;
    let registre = st.catalog.templates();
    let trouves: Vec<Value> = registre
        .composables
        .iter()
        .filter(|(n, _)| glob_match(&motif, n))
        .map(|(n, t)| json!({"name": n, "index_template": t.to_json_composable()}))
        .collect();

    // Un nom litteral absent est un 404 qui le nomme ; un motif sans
    // correspondance rend une liste vide **et** un 404. C'est la reponse d'un
    // vrai ES 8.15, mesuree.
    if trouves.is_empty() {
        if motif.contains('*') {
            return Ok(Json(StatusCode::NOT_FOUND, json!({"index_templates": []})));
        }
        return Err(EsError::new(
            StatusCode::NOT_FOUND,
            "resource_not_found_exception",
            format!("index template matching [{motif}] not found"),
        ));
    }
    Ok(Json::ok(json!({"index_templates": trouves})))
}

/// `HEAD /_index_template/{nom}`
pub async fn existe_composable(
    State(st): State<SharedState>,
    Path(nom): Path<String>,
) -> StatusCode {
    let registre = st.catalog.templates();
    if registre.composables.keys().any(|n| glob_match(&nom, n)) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

/// `DELETE /_index_template/{nom}`
pub async fn supprimer_composable(
    State(st): State<SharedState>,
    Path(nom): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("master_timeout");
    p.opt("timeout");
    p.done()?;
    valider_nom(&nom)?;
    st.catalog.supprimer_template(&nom, true)?;
    Ok(Json::ok(json!({"acknowledged": true})))
}

// ---------------------------------------------------------------------------
// Anciens : `_template`
// ---------------------------------------------------------------------------

/// `PUT|POST /_template/{nom}`
pub async fn poser_ancien(
    State(st): State<SharedState>,
    Path(nom): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let create = lire_parametres(&uri)?;
    valider_nom(&nom)?;
    let tpl = crate::templates::lire_ancien(&parse_body(&body)?)?;
    st.catalog.poser_template(&nom, tpl, false, create)?;
    Ok(Json::ok(json!({"acknowledged": true})))
}

/// `GET /_template` et `GET /_template/{nom}`
///
/// `{nom}` accepte une liste et des motifs, comme chez ES.
pub async fn lire_anciens(
    State(st): State<SharedState>,
    nom: Option<Path<String>>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("master_timeout");
    p.opt("local");
    let plat = p.flag("flat_settings", false)?;
    if p.opt("include_defaults").is_some() {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [include_defaults] sur [/_template] : il ajoute les dizaines \
             de reglages qu'ES a et que ferrite n'a pas",
        ));
    }
    p.done()?;

    let motifs: Vec<String> = nom
        .map(|Path(n)| n.split(',').map(str::trim).map(str::to_string).collect())
        .unwrap_or_else(|| vec!["*".to_string()]);
    let registre = st.catalog.templates();
    let mut out = Map::new();
    for (n, t) in &registre.anciens {
        if !motifs.iter().any(|m| glob_match(m, n)) {
            continue;
        }
        let mut rendu = t.to_json_ancien();
        if plat {
            if let Value::Object(o) = &mut rendu {
                let settings = o.get("settings").cloned().unwrap_or_else(|| json!({}));
                o.insert(
                    "settings".into(),
                    crate::reglages::aplatir_reponse(&settings),
                );
            }
        }
        out.insert(n.clone(), rendu);
    }
    if out.is_empty() {
        // ES rend un corps vide avec un 404, motif ou nom litteral.
        return Ok(Json(StatusCode::NOT_FOUND, json!({})));
    }
    Ok(Json::ok(Value::Object(out)))
}

/// `HEAD /_template/{nom}`
pub async fn existe_ancien(State(st): State<SharedState>, Path(nom): Path<String>) -> StatusCode {
    let registre = st.catalog.templates();
    let motifs: Vec<&str> = nom.split(',').map(str::trim).collect();
    if registre
        .anciens
        .keys()
        .any(|n| motifs.iter().any(|m| glob_match(m, n)))
    {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

/// `DELETE /_template/{nom}`
pub async fn supprimer_ancien(
    State(st): State<SharedState>,
    Path(nom): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("master_timeout");
    p.opt("timeout");
    p.done()?;
    st.catalog.supprimer_template(&nom, false)?;
    Ok(Json::ok(json!({"acknowledged": true})))
}

// ---------------------------------------------------------------------------

fn lire_parametres(uri: &Uri) -> EsResult<bool> {
    let mut p = Params::parse(uri);
    p.opt("master_timeout");
    p.opt("timeout");
    p.opt("order");
    let create = p.flag("create", false)?;
    p.done()?;
    Ok(create)
}

/// ES refuse la virgule dans un nom de template : elle designerait une liste,
/// et poser « deux templates a la fois » n'a pas de sens.
fn valider_nom(nom: &str) -> EsResult<()> {
    if nom.contains(',') {
        return Err(EsError::illegal_argument(
            "template name may not contain ','",
        ));
    }
    Ok(())
}
