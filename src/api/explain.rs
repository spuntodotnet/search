//! `GET|POST /{index}/_explain/{id}` : pourquoi **ce** document, avec ce score.
//!
//! La route ne cherche pas : elle prend un document nomme et lui pose une
//! requete. Deux choses la separent d'une recherche a un document pres, et ce
//! sont les deux qu'un client lit :
//!
//! * `matched` — le document correspond-il ? C'est un booleen, il est exact des
//!   deux cotes, et c'est lui qui repond a « pourquoi mon document ne sort
//!   pas » ;
//! * `explanation` — l'arbre du score, quand il correspond. Ce que ferrite y
//!   rend et ce qu'il ne rend pas est ecrit dans [`crate::explain`].
//!
//! Un document absent rend **404 avec un corps** (`{_index, _id, matched:
//! false}`, sans `explanation`) : mesure contre ES 8.15, et la difference
//! compte, un client qui leve sur 404 ne lit pas le meme cas qu'un `matched:
//! false` en 200.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use serde_json::{json, Map, Value};

use super::{expect_only, parse_body, Json, Params, SharedState};
use crate::dsl::{build_query, QueryCtx};
use crate::error::{EsError, EsResult};
use crate::selection::index_unique;

/// `GET|POST /{index}/_explain/{id}`
pub async fn explain(
    State(st): State<SharedState>,
    Path((index, id)): Path<(String, String)>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    // Un seul shard, un seul noeud : ces trois-la designent forcement le meme
    // endroit. Ils sont acceptes et sans objet, comme sur `_search`.
    p.opt("preference");
    p.opt("routing");
    p.opt("realtime");
    if p.opt("q").is_some() {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas la recherche par chaine [q] (query_string) sur [_explain] ; \
             utilise le Query DSL dans le corps",
        ));
    }
    for param in [
        "df",
        "default_operator",
        "analyzer",
        "analyze_wildcard",
        "lenient",
        "_source",
        "_source_includes",
        "_source_excludes",
        "stored_fields",
    ] {
        if p.opt(param).is_some() {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{param}] sur [_explain] : la route rend l'arbre du \
                 score, pas le document (voir docs/compat.md)"
            )));
        }
    }
    p.done()?;

    let body = parse_body(&body)?;
    let corps = match &body {
        Value::Null => Map::new(),
        Value::Object(o) => o.clone(),
        _ => {
            return Err(EsError::parsing(
                "le corps de [_explain] doit etre un objet",
            ))
        }
    };
    expect_only(&corps, &["query"], "_explain")?;
    // ES valide la demande **avant** d'aller chercher le document : sans
    // `query`, il rend 400 meme sur un identifiant qui n'existe pas.
    requete_obligatoire(&corps)?;

    let idx = index_unique(&st.catalog, &index)?;
    let nom_index = idx.name.clone();

    let trouve = {
        let idx = idx.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || idx.adresse_de(&id))
            .await
            .map_err(|e| EsError::internal(format!("_explain: {e}")))??
    };

    let Some((gen, searcher, addr)) = trouve else {
        return Ok(Json(
            StatusCode::NOT_FOUND,
            json!({"_index": nom_index, "_id": id, "matched": false}),
        ));
    };

    // Les clauses nommees sont retirees comme sur `_search` — ES accepte
    // `_name` ici aussi, et le perdre en silence serait le meme defaut.
    let requete = crate::dsl::extraire_noms(requete_obligatoire(&corps)?)?.0;

    let incidents = std::sync::Arc::new(crate::fonction_score::Incidents::pour(
        &nom_index,
        &idx.uuid,
        &st.catalog.cluster_uuid,
    ));
    let ctx = QueryCtx::new(&gen.fields, &gen.index, &searcher)
        .selon_le_mapping(&gen.mapping)
        .avec_incidents(incidents.clone());
    let query = build_query(&requete, &ctx)?;

    let arbre = crate::explain::expliquer(&searcher, &*query, addr, &gen.index.schema());
    if let Some(e) = incidents.erreur() {
        return Err(e);
    }

    let mut o = Map::new();
    o.insert("_index".into(), json!(nom_index));
    o.insert("_id".into(), json!(id));
    o.insert("matched".into(), json!(arbre.is_some()));
    o.insert(
        "explanation".into(),
        arbre
            .unwrap_or_else(crate::explain::sans_correspondance)
            .json(),
    );
    Ok(Json::ok(Value::Object(o)))
}

/// `_explain` sans requete : ES refuse, et il a raison — la route ne rend pas le
/// document, elle rend ce qu'une requete en dit. Le message est le sien.
fn requete_obligatoire(corps: &Map<String, Value>) -> EsResult<&Value> {
    match corps.get("query") {
        Some(v) if !v.is_null() => Ok(v),
        _ => Err(EsError::new(
            StatusCode::BAD_REQUEST,
            "action_request_validation_exception",
            "Validation Failed: 1: query is missing;",
        )),
    }
}
