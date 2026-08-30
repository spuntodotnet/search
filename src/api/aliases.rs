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
        // La phrase est celle d'ES : un client qui teste ses erreurs la lit.
        return Err(EsError::illegal_argument("No action specified"));
    }

    let mut modifications: Vec<ActionAlias> = Vec::new();
    let mut a_supprimer: Vec<String> = Vec::new();
    // Tout ce que la requete a **ecrit** comme nom d'alias, dans l'ordre : c'est
    // ce que nomme le 404 global quand elle finit sans rien faire.
    let mut alias_ecrits: Vec<String> = Vec::new();
    // Le registre tel qu'il etait **avant** la requete : c'est contre lui qu'ES
    // resout les `remove`, pas contre l'etat qu'une action precedente laisse.
    // Lu une seule fois, et seulement si un `remove` le demande.
    let mut registre: Option<crate::alias::Registre> = None;

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
                let (attache, must_exist) = if verbe == "add" {
                    let reste: Map<String, Value> = corps
                        .iter()
                        .filter(|(k, _)| {
                            !matches!(k.as_str(), "index" | "indices" | "alias" | "aliases")
                        })
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    (lire_attache(&Value::Object(reste), &quoi)?, None)
                } else {
                    let mut must_exist = None;
                    for (cle, v) in corps {
                        match cle.as_str() {
                            "index" | "indices" | "alias" | "aliases" => {}
                            "must_exist" => {
                                must_exist = Some(v.as_bool().ok_or_else(|| {
                                    EsError::illegal_argument(format!(
                                        "[{quoi}.must_exist] : Failed to parse value [{v}] as \
                                         only [true] or [false] are allowed."
                                    ))
                                })?)
                            }
                            autre => {
                                return Err(EsError::unsupported(format!(
                                    "ferrite ne supporte pas [{autre}] dans [{quoi}]"
                                )))
                            }
                        }
                    }
                    (Attache::default(), must_exist)
                };
                for a in &alias {
                    alias_ecrits.push(a.clone());
                }

                // Un motif d'index est developpe ici : `add` sur `audits-*`
                // pose l'alias sur tout ce qui existe deja.
                let mut vises = Vec::new();
                for expr in &index {
                    let trouves = resoudre(&st.catalog, expr, &Options::default())?;
                    if trouves.is_empty() {
                        return Err(EsError::index_not_found(expr));
                    }
                    vises.extend(trouves.into_iter().map(|i| i.name.clone()));
                }

                if verbe == "add" {
                    for a in &alias {
                        valider_nom(a)?;
                    }
                    for idx in &vises {
                        for a in &alias {
                            modifications.push(ActionAlias::Ajouter {
                                index: idx.clone(),
                                alias: a.clone(),
                                attache: attache.clone(),
                            });
                        }
                    }
                } else {
                    // Un `remove` ne nomme pas des alias, il les **designe** :
                    // `test_alias*` et `_all` y sont des motifs, et un nom qui
                    // ne correspond a rien n'est pas une erreur en soi. Les
                    // valider comme des noms d'alias rendait 400 sur le joker
                    // que la suite d'OpenSearch pose.
                    let registre = registre.get_or_insert_with(|| st.catalog.aliases());
                    for idx in &vises {
                        let trouves = alias_de_l_index(registre, idx, &alias);
                        // `must_exist: true` se verifie **par index visé**, pas
                        // par requete : mesure contre ES 8.15, ou
                        // `remove {index: "wz*", alias: "ex1"}` rend 404 des
                        // qu'un seul des index ne porte pas l'alias, meme si un
                        // autre le porte. Le 404 par defaut, lui, est global —
                        // voir plus bas.
                        // ES ne nomme alors que la **premiere** expression
                        // ecrite, pas toutes : `[ab1, ab2]` rend
                        // « aliases [ab1] missing » et `[ab2, ab1] `
                        // « aliases [ab2] missing ». Releve, pas deduit.
                        if trouves.is_empty() && must_exist == Some(true) {
                            return Err(aliases_manquants(&alias[..1.min(alias.len())]));
                        }
                        for a in trouves {
                            modifications.push(ActionAlias::Retirer {
                                index: idx.clone(),
                                alias: a,
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

    // Une requete qui n'a rien a faire est une requete dont tous les `remove`
    // ont porte a cote : ES la refuse en 404, en nommant les alias ecrits — et
    // ce verdict est **global**, pas par action. `remove` d'un alias absent
    // **plus** un `add` valide rend 200 chez lui (mesure), parce que le second
    // a produit quelque chose.
    if modifications.is_empty() && a_supprimer.is_empty() {
        let mut vus = std::collections::BTreeSet::new();
        let noms: Vec<String> = alias_ecrits
            .into_iter()
            .filter(|a| vus.insert(a.clone()))
            .collect();
        return Err(aliases_manquants(&noms));
    }
    if !modifications.is_empty() {
        st.catalog.modifier_alias(&modifications)?;
    }
    for nom in a_supprimer {
        st.catalog.delete(&nom)?;
    }
    Ok(Json::ok(json!({"acknowledged": true, "errors": false})))
}

/// Le 404 d'ES quand un `remove` ne designe aucun alias existant.
///
/// Le corps porte `resource.type` et `resource.id` — une **chaine** quand il n'y
/// a qu'un nom, une **liste** au-dela. C'est la forme d'ES 8.15, relevee.
fn aliases_manquants(noms: &[String]) -> EsError {
    let id = match noms {
        [seul] => json!(seul),
        plusieurs => json!(plusieurs),
    };
    EsError::new(
        StatusCode::NOT_FOUND,
        "aliases_not_found_exception",
        format!("aliases [{}] missing", noms.join(", ")),
    )
    .with("resource.type", json!("aliases"))
    .with("resource.id", id)
}

/// Les alias attaches a `index` que designe une des expressions `exprs`.
///
/// `_all` et `*` y designent tous les alias de l'index, comme chez ES ; les
/// autres termes sont des motifs (`test_alias*`).
fn alias_de_l_index(
    registre: &crate::alias::Registre,
    index: &str,
    exprs: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    for (alias, cibles) in registre {
        if !cibles.contains_key(index) {
            continue;
        }
        if exprs
            .iter()
            .any(|e| e == "_all" || e == "*" || glob_match(e, alias))
        {
            out.push(alias.clone());
        }
    }
    out
}

/// Lit une valeur qui peut s'ecrire au singulier (chaine) ou au pluriel (liste).
///
/// Les messages sont ceux d'ES 8.15, mesures un par un : ils distinguent la cle
/// **absente** (« One of [alias] or [aliases] is required ») de la cle
/// **vide** (« [aliases] can't be empty », « [alias] can't be empty string »).
/// La suite d'OpenSearch grep le deuxieme.
fn noms(
    corps: &Map<String, Value>,
    singulier: &str,
    pluriel: &str,
    quoi: &str,
) -> EsResult<Vec<String>> {
    let mut out = Vec::new();
    let mut ecrit = false;
    for cle in [singulier, pluriel] {
        match corps.get(cle) {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => {
                ecrit = true;
                if s.trim().is_empty() {
                    return Err(EsError::illegal_argument(format!(
                        "[{quoi}] : [{cle}] can't be empty string"
                    )));
                }
                out.extend(
                    s.split(',')
                        .map(str::trim)
                        .filter(|x| !x.is_empty())
                        .map(str::to_string),
                );
            }
            Some(Value::Array(a)) => {
                ecrit = true;
                if a.is_empty() {
                    return Err(EsError::illegal_argument(format!(
                        "[{quoi}] : [{cle}] can't be empty"
                    )));
                }
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
    if !ecrit || out.is_empty() {
        return Err(EsError::illegal_argument(format!(
            "[{quoi}] : One of [{singulier}] or [{pluriel}] is required"
        )));
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
    poser_impl(st, Some(index), Some(nom), uri, body).await
}

/// `PUT /{index}/_alias` — le nom de l'alias est dans le corps.
pub async fn poser_sans_nom(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    poser_impl(st, Some(index), None, uri, body).await
}

/// `PUT|POST /_alias/{nom}` — l'index est dans le corps.
pub async fn poser_sans_index(
    State(st): State<SharedState>,
    Path(nom): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    poser_impl(st, None, Some(nom), uri, body).await
}

/// `PUT /_alias` — les deux sont dans le corps.
pub async fn poser_par_le_corps(
    State(st): State<SharedState>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    poser_impl(st, None, None, uri, body).await
}

/// Les sept URL de `put_alias`, ramenees a une seule lecture.
///
/// Le corps peut porter `index` et `alias`, et il **remplace** alors ce que
/// l'URL dit (mesure : `PUT /inconnu/_alias/a` avec `{"index": "reel"}` pose
/// l'alias sur `reel` et rend 200). C'est ce qui rend les formes sans nom
/// d'alias dans l'URL utilisables — et ce sont elles que la suite de conformance
/// d'OpenSearch exerce, la suite d'Elastic etant figee avant leur arrivee.
///
/// Deux ecarts assumes, tous deux du cote du refus :
///
/// - ES ne lit dans ce corps que `index` et `alias` au **singulier** — un
///   `indices`/`aliases` y est ignore, et sort en « [indices] can't be empty ».
///   ferrite les refuse en les nommant plutot que de laisser croire qu'ils ont
///   servi ;
/// - une **liste** JSON (`{"index": ["a", "b"]}`) n'en garde chez ES que le
///   dernier element, en 200. Recopier ca poserait l'alias ailleurs que la ou le
///   corps le demande, sans un mot : c'est refuse.
async fn poser_impl(
    st: SharedState,
    index_url: Option<String>,
    nom_url: Option<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("timeout");
    p.opt("master_timeout");
    p.done()?;

    let corps = parse_body(&body)?;
    let obj = match &corps {
        Value::Null => Map::new(),
        Value::Object(o) => o.clone(),
        _ => {
            return Err(EsError::parsing(
                "[_alias] : la declaration d'un alias doit etre un objet",
            ))
        }
    };
    for pluriel in ["indices", "aliases"] {
        if obj.contains_key(pluriel) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{pluriel}] dans le corps de [_alias] : ES n'y lit que \
                 [index] et [alias] au singulier, et rend [indices] can't be empty sans le dire"
            )));
        }
    }
    let du_corps = |cle: &str| -> EsResult<Option<String>> {
        match obj.get(cle) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(EsError::unsupported(format!(
                "ferrite ne supporte pas une liste dans [{cle}] du corps de [_alias] : ES n'en \
                 garde que le dernier element, en 200"
            ))),
        }
    };

    // `index` est une **liste** d'expressions separees par des virgules, dans
    // l'URL comme dans le corps ; `alias` est un nom, et un seul — une virgule y
    // rend « Invalid alias name » chez ES, aussi bien depuis l'URL que depuis le
    // corps (mesure).
    let index = du_corps("index")?.or(index_url).unwrap_or_default();
    let index = decouper(&index);
    if index.is_empty() {
        return Err(EsError::illegal_argument("[indices] can't be empty"));
    }
    let alias = du_corps("alias")?.or(nom_url).unwrap_or_default();
    if alias.trim().is_empty() {
        return Err(EsError::illegal_argument("[alias] can't be empty string"));
    }
    valider_nom(&alias)?;

    let reste: Map<String, Value> = obj
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "index" | "alias"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let attache = lire_attache(&Value::Object(reste), "_alias")?;

    let mut modifications = Vec::new();
    for expr in &index {
        let vises = resoudre(&st.catalog, expr, &Options::default())?;
        if vises.is_empty() {
            return Err(EsError::index_not_found(expr));
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

    // `{nom}` est ici une **liste** de motifs (`ex1,absent`, `e*`, `_all`) —
    // c'est ce que dit la spec d'ES, et c'est ce qui separe cette route de
    // `PUT`, ou le meme `{nom}` est un nom unique.
    let exprs = decouper(&nom);
    let registre = st.catalog.aliases();
    let mut modifications = Vec::new();
    for idx in resoudre(&st.catalog, &index, &Options::default())? {
        for alias in alias_de_l_index(&registre, &idx.name, &exprs) {
            modifications.push(ActionAlias::Retirer {
                index: idx.name.clone(),
                alias,
            });
        }
    }
    // Meme regle globale que `POST /_aliases` : le 404 tombe quand la commande
    // n'a rien a faire, pas quand un terme porte a cote. `DELETE
    // /a,b/_alias/x` rend 200 des que `a` porte `x` (mesure).
    if modifications.is_empty() {
        return Err(aliases_manquants(&exprs));
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

    let mut rendus: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut out = Map::new();
    for (idx, aliases) in vus {
        let mut o = Map::new();
        for (nom, attache) in aliases {
            rendus.insert(nom.clone());
            o.insert(nom, attache.to_json());
        }
        out.insert(idx, json!({"aliases": Value::Object(o)}));
    }

    // ES rend 404 quand un alias explicitement demande n'est pas dans la
    // reponse — et, sur cette route seulement, avec un `error` qui est une
    // **chaine** et non l'objet habituel. Un client type le remarque. Le corps
    // porte quand meme les alias trouves : le 404 dit « il en manque », pas
    // « il n'y a rien ».
    if let Some(f) = &filtre {
        let manquants = manquants(&st.catalog.aliases().keys().cloned().collect(), f, &rendus);
        if !manquants.is_empty() {
            let mot = if manquants.len() == 1 {
                "alias"
            } else {
                "aliases"
            };
            out.insert(
                "error".into(),
                Value::String(format!("{mot} [{}] missing", manquants.join(","))),
            );
            out.insert("status".into(), json!(404));
            return Ok(Json(StatusCode::NOT_FOUND, Value::Object(out)));
        }
    }
    Ok(Json::ok(Value::Object(out)))
}

/// Les alias que designe une expression `a,b*,-c`.
///
/// L'expression se lit de gauche a droite : un terme ajoute ce qu'il designe,
/// un terme prefixe de `-` retire de ce qui a deja ete retenu. Le tiret n'est
/// une exclusion qu'a partir du **deuxieme** terme — en premiere position il
/// fait partie du nom, comme partout ailleurs chez ES. C'est ce qui fait de
/// `-test_alias_1,test_alias*,-test_alias_2` un 404 sur un nom qui n'existe pas
/// plutot qu'une exclusion.
fn selectionne(
    tous: &std::collections::BTreeSet<String>,
    expr: &str,
) -> std::collections::BTreeSet<String> {
    let mut retenus = std::collections::BTreeSet::new();
    if designe_tout(expr) {
        return tous.clone();
    }
    for (i, terme) in decouper(expr).iter().enumerate() {
        let exclusion = i > 0 && terme.starts_with('-');
        let motif = if exclusion {
            &terme[1..]
        } else {
            terme.as_str()
        };
        for alias in tous.iter().filter(|a| glob_match(motif, a)) {
            if exclusion {
                retenus.remove(alias);
            } else {
                retenus.insert(alias.clone());
            }
        }
    }
    retenus
}

/// `_all` et `*` designent tous les alias : ES ne cherche alors aucun manquant.
fn designe_tout(expr: &str) -> bool {
    expr == "_all" || expr == "*"
}

/// Les noms d'alias qu'ES declare manquants pour l'expression `expr`.
///
/// Mesure, pas deduction : `test_alias_1,-test` rend 404 sur `-test` alors que
/// `test_blias_2,test_alias*,-test_alias_1` rend 200 — la meme exclusion d'un
/// alias qui existe. Ce qui les separe est le **joker**. Tant qu'aucun terme
/// n'est un motif, ES compare la liste **ecrite** a ce qu'il rend : une
/// exclusion y figure telle quelle, tiret compris, et manque forcement. Des
/// qu'un motif apparait, la liste ecrite est remplacee par une liste
/// **resolue** — les termes qui precedent le motif y entrent, les suivants s'y
/// ajoutent ou s'en retirent — et ne contient donc plus que ce qui a survecu.
///
/// C'est ce basculement, et lui seul, qui explique les 21 reponses relevees sur
/// ES 7.10.2 comme sur ES 8.15.0 (`tests/compat/sonde_alias.py`).
fn manquants(
    tous: &std::collections::BTreeSet<String>,
    expr: &str,
    rendus: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    if designe_tout(expr) {
        return Vec::new();
    }
    let termes = decouper(expr);
    let mut resolue: Option<std::collections::BTreeSet<String>> = None;
    for (i, terme) in termes.iter().enumerate() {
        let exclusion = i > 0 && terme.starts_with('-');
        let motif = if exclusion {
            &terme[1..]
        } else {
            terme.as_str()
        };
        if !motif.contains('*') {
            // Un nom simple avant le premier motif reste dans la liste ecrite :
            // il n'y a pas encore de liste resolue ou l'inscrire.
            if let Some(r) = resolue.as_mut() {
                if exclusion {
                    r.remove(motif);
                } else {
                    r.insert(motif.to_string());
                }
            }
            continue;
        }
        let r = resolue.get_or_insert_with(|| termes[..i].iter().cloned().collect());
        for alias in tous.iter().filter(|a| glob_match(motif, a)) {
            if exclusion {
                r.remove(alias);
            } else {
                r.insert(alias.clone());
            }
        }
    }
    let resolue = resolue.unwrap_or_else(|| termes.iter().cloned().collect());
    resolue.difference(rendus).cloned().collect()
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
    let tous: std::collections::BTreeSet<String> = registre.keys().cloned().collect();
    let retenus: Option<std::collections::BTreeSet<String>> = filtre.map(|f| selectionne(&tous, f));

    let mut out = std::collections::BTreeMap::new();
    for idx in indices {
        let mut aliases = std::collections::BTreeMap::new();
        for (alias, cibles) in &registre {
            let Some(attache) = cibles.get(&idx.name) else {
                continue;
            };
            let retenu = match &retenus {
                None => true,
                Some(r) => r.contains(alias),
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
