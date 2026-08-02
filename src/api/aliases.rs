//! Les routes d'alias : `_aliases`, `{index}/_alias/{nom}`, `_alias`.
//!
//! Un alias est ce qui permet a un index de changer sans que le code client
//! change : `audits` reste `audits` alors que les index quotidiens qu'il
//! designe apparaissent et disparaissent. C'est aussi la seule facon de basculer
//! une lecture d'un index a un autre **sans interruption** — le geste
//! d'exploitation que la creation d'index seule ne sait pas rendre.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};

use super::{parse_body, selection_options, Json, Params, SharedState};
use crate::alias::{lire_attache, valider_nom, Attache};
use crate::engine::ActionAlias;
use crate::error::{EsError, EsResult};
use crate::search::glob_match;
use crate::selection::{resoudre, Options};

/// `POST /_aliases` — un lot de modifications, tout ou rien.
///
/// C'est la route qui rend le basculement atomique : retirer l'alias de
/// l'ancien index et le poser sur le nouveau dans le meme appel, sans instant
/// ou il ne designe rien.
pub async fn actions(State(st): State<SharedState>, uri: Uri, body: Bytes) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("timeout");
    p.opt("master_timeout");
    p.done()?;

    let body = parse_body(&body)?;
    let obj = body
        .as_object()
        .ok_or_else(|| EsError::parsing("le corps de [_aliases] doit etre un objet"))?;
    super::expect_only(obj, &["actions"], "_aliases")?;
    let liste = obj
        .get("actions")
        .and_then(Value::as_array)
        .ok_or_else(|| EsError::parsing("[_aliases] : [actions] est une liste obligatoire"))?;
    if liste.is_empty() {
        return Err(EsError::illegal_argument(
            "[_aliases] : [actions] ne peut pas etre vide",
        ));
    }

    let mut modifications: Vec<ActionAlias> = Vec::new();
    let mut a_supprimer: Vec<String> = Vec::new();

    for action in liste {
        let o = action
            .as_object()
            .ok_or_else(|| EsError::parsing("[_aliases.actions] : objets attendus"))?;
        if o.len() != 1 {
            return Err(EsError::parsing(
                "[_aliases.actions] : une seule action par entree",
            ));
        }
        let (verbe, corps) = o.iter().next().unwrap();
        let corps = corps.as_object().ok_or_else(|| {
            EsError::parsing(format!("[_aliases.actions.{verbe}] doit etre un objet"))
        })?;

        match verbe.as_str() {
            "add" | "remove" => {
                let quoi = format!("_aliases.actions.{verbe}");
                let index = noms(corps, "index", "indices", &quoi)?;
                let alias = noms(corps, "alias", "aliases", &quoi)?;
                if index.is_empty() || alias.is_empty() {
                    return Err(EsError::illegal_argument(format!(
                        "[{quoi}] : [index] et [alias] sont obligatoires"
                    )));
                }
                let attache = if verbe == "add" {
                    let reste: Map<String, Value> = corps
                        .iter()
                        .filter(|(k, _)| {
                            !matches!(k.as_str(), "index" | "indices" | "alias" | "aliases")
                        })
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    lire_attache(&Value::Object(reste), &quoi)?
                } else {
                    for cle in corps.keys() {
                        if !matches!(cle.as_str(), "index" | "indices" | "alias" | "aliases") {
                            return Err(EsError::unsupported(format!(
                                "ferrite ne supporte pas [{cle}] dans [{quoi}]"
                            )));
                        }
                    }
                    Attache::default()
                };

                for a in &alias {
                    valider_nom(a)?;
                }
                // Un motif d'index est developpe ici : `add` sur `audits-*`
                // pose l'alias sur tout ce qui existe deja.
                for expr in &index {
                    let vises = resoudre(&st.catalog, expr, &Options::default())?;
                    if vises.is_empty() {
                        return Err(EsError::index_not_found(expr));
                    }
                    for idx in vises {
                        for a in &alias {
                            modifications.push(match verbe.as_str() {
                                "add" => ActionAlias::Ajouter {
                                    index: idx.name.clone(),
                                    alias: a.clone(),
                                    attache: attache.clone(),
                                },
                                _ => ActionAlias::Retirer {
                                    index: idx.name.clone(),
                                    alias: a.clone(),
                                },
                            });
                        }
                    }
                }
            }
            "remove_index" => {
                let quoi = "_aliases.actions.remove_index".to_string();
                for cle in corps.keys() {
                    if !matches!(cle.as_str(), "index" | "indices") {
                        return Err(EsError::unsupported(format!(
                            "ferrite ne supporte pas [{cle}] dans [{quoi}]"
                        )));
                    }
                }
                for expr in noms(corps, "index", "indices", &quoi)? {
                    for idx in resoudre(&st.catalog, &expr, &Options::default())? {
                        a_supprimer.push(idx.name.clone());
                    }
                }
            }
            autre => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas l'action [{autre}] dans [_aliases] ; actions \
                     acceptees : add, remove, remove_index"
                )))
            }
        }
    }

    if !modifications.is_empty() {
        st.catalog.modifier_alias(&modifications)?;
    }
    for nom in a_supprimer {
        st.catalog.delete(&nom)?;
    }
    Ok(Json::ok(json!({"acknowledged": true, "errors": false})))
}

/// Lit une valeur qui peut s'ecrire au singulier (chaine) ou au pluriel (liste).
fn noms(
    corps: &Map<String, Value>,
    singulier: &str,
    pluriel: &str,
    quoi: &str,
) -> EsResult<Vec<String>> {
    let mut out = Vec::new();
    for cle in [singulier, pluriel] {
        match corps.get(cle) {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => out.extend(
                s.split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(str::to_string),
            ),
            Some(Value::Array(a)) => {
                for v in a {
                    let s = v.as_str().ok_or_else(|| {
                        EsError::illegal_argument(format!("[{quoi}.{cle}] : chaines attendues"))
                    })?;
                    out.push(s.to_string());
                }
            }
            Some(_) => {
                return Err(EsError::illegal_argument(format!(
                    "[{quoi}.{cle}] : chaine ou liste attendue"
                )))
            }
        }
    }
    Ok(out)
}

/// `PUT|POST /{index}/_alias/{nom}`
pub async fn poser(
    State(st): State<SharedState>,
    Path((index, nom)): Path<(String, String)>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("timeout");
    p.opt("master_timeout");
    p.done()?;

    let attache = lire_attache(&parse_body(&body)?, "_alias")?;
    let mut modifications = Vec::new();
    for alias in decouper(&nom) {
        valider_nom(&alias)?;
        let vises = resoudre(&st.catalog, &index, &Options::default())?;
        if vises.is_empty() {
            return Err(EsError::index_not_found(&index));
        }
        for idx in vises {
            modifications.push(ActionAlias::Ajouter {
                index: idx.name.clone(),
                alias: alias.clone(),
                attache: attache.clone(),
            });
        }
    }
    st.catalog.modifier_alias(&modifications)?;
    Ok(Json::ok(json!({"acknowledged": true, "errors": false})))
}

/// `DELETE /{index}/_alias/{nom}`
pub async fn retirer(
    State(st): State<SharedState>,
    Path((index, nom)): Path<(String, String)>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("timeout");
    p.opt("master_timeout");
    p.done()?;

    let registre = st.catalog.aliases();
    let mut modifications = Vec::new();
    for expr in decouper(&nom) {
        // Le nom d'alias accepte un motif ici, comme chez ES.
        let vises: Vec<String> = registre
            .keys()
            .filter(|a| glob_match(&expr, a))
            .cloned()
            .collect();
        if vises.is_empty() {
            return Err(EsError::new(
                StatusCode::NOT_FOUND,
                "aliases_not_found_exception",
                format!("aliases [{expr}] missing"),
            ));
        }
        for idx in resoudre(&st.catalog, &index, &Options::default())? {
            for alias in &vises {
                modifications.push(ActionAlias::Retirer {
                    index: idx.name.clone(),
                    alias: alias.clone(),
                });
            }
        }
    }
    st.catalog.modifier_alias(&modifications)?;
    Ok(Json::ok(json!({"acknowledged": true, "errors": false})))
}

fn decouper(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// Lecture
// ---------------------------------------------------------------------------

/// `GET /_alias`
pub async fn lister_tout(State(st): State<SharedState>, uri: Uri) -> EsResult<Json> {
    lire(st, None, None, uri).await
}

/// `GET /_alias/{nom}`
pub async fn lister_par_alias(
    State(st): State<SharedState>,
    Path(nom): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    lire(st, None, Some(nom), uri).await
}

/// `GET /{index}/_alias`
pub async fn lister_par_index(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    lire(st, Some(index), None, uri).await
}

/// `GET /{index}/_alias/{nom}`
pub async fn lister(
    State(st): State<SharedState>,
    Path((index, nom)): Path<(String, String)>,
    uri: Uri,
) -> EsResult<Json> {
    lire(st, Some(index), Some(nom), uri).await
}

async fn lire(
    st: SharedState,
    index: Option<String>,
    filtre: Option<String>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    p.opt("master_timeout");
    p.opt("local");
    p.done()?;

    let vus = collecter(&st, index.as_deref(), filtre.as_deref(), &opts)?;
    // ES rend 404 quand un alias explicitement demande n'existe nulle part —
    // et, sur cette route seulement, avec un `error` qui est une **chaine** et
    // non l'objet habituel. Un client type le remarque.
    if let Some(f) = &filtre {
        if vus.values().all(|a| a.is_empty()) {
            return Ok(Json(
                StatusCode::NOT_FOUND,
                json!({"error": format!("alias [{f}] missing"), "status": 404}),
            ));
        }
    }

    let mut out = Map::new();
    for (idx, aliases) in vus {
        let mut o = Map::new();
        for (nom, attache) in aliases {
            o.insert(nom, attache.to_json());
        }
        out.insert(idx, json!({"aliases": Value::Object(o)}));
    }
    Ok(Json::ok(Value::Object(out)))
}

/// `HEAD /_alias/{nom}` et `HEAD /{index}/_alias/{nom}` — 200 ou 404.
pub async fn exister(State(st): State<SharedState>, Path(nom): Path<String>, uri: Uri) -> Response {
    tete(st, None, nom, uri)
}

pub async fn exister_dans(
    State(st): State<SharedState>,
    Path((index, nom)): Path<(String, String)>,
    uri: Uri,
) -> Response {
    tete(st, Some(index), nom, uri)
}

fn tete(st: SharedState, index: Option<String>, nom: String, uri: Uri) -> Response {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p).unwrap_or_default();
    match collecter(&st, index.as_deref(), Some(&nom), &opts) {
        Ok(vus) if vus.values().any(|a| !a.is_empty()) => StatusCode::OK.into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `index -> alias -> attache`, restreint aux index et aux alias demandes.
#[allow(clippy::type_complexity)]
fn collecter(
    st: &SharedState,
    index: Option<&str>,
    filtre: Option<&str>,
    opts: &Options,
) -> EsResult<std::collections::BTreeMap<String, std::collections::BTreeMap<String, Attache>>> {
    let registre = st.catalog.aliases();
    let indices = match index {
        Some(expr) => resoudre(&st.catalog, expr, opts)?,
        None => st.catalog.list(),
    };
    let motifs: Option<Vec<String>> = filtre.map(decouper);

    let mut out = std::collections::BTreeMap::new();
    for idx in indices {
        let mut aliases = std::collections::BTreeMap::new();
        for (alias, cibles) in &registre {
            let Some(attache) = cibles.get(&idx.name) else {
                continue;
            };
            let retenu = match &motifs {
                None => true,
                Some(m) => m.iter().any(|p| glob_match(p, alias)),
            };
            if retenu {
                aliases.insert(alias.clone(), attache.clone());
            }
        }
        out.insert(idx.name.clone(), aliases);
    }
    // `GET /_alias/{nom}` ne rend que les index concernes ; `GET /_alias` les
    // rend tous, meme sans alias, comme ES.
    if filtre.is_some() {
        out.retain(|_, a| !a.is_empty());
    }
    Ok(out)
}
