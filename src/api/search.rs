//! `_search` : parametres, execution, reponse au format exact d'Elasticsearch.

use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::Uri;
use serde_json::{json, Map, Value};

use super::{elapsed_ms, expect_only, parse_body, selection_options, Json, Params, SharedState};
use crate::dsl::{build_query, QueryCtx};
use crate::engine::Generation;
use crate::error::{EsError, EsResult};
use crate::mapping::FieldKind;
use crate::search::{execute, round_score, Cible, SearchRequest, SortKey, SortSpec, SourceFilter};
use crate::selection::resoudre;
use crate::MAX_RESULT_WINDOW;

const DEFAULT_SIZE: usize = 10;

/// `POST|GET /_search` — sans index dans l'URL, ES cherche partout.
pub async fn search_all(State(st): State<SharedState>, uri: Uri, body: Bytes) -> EsResult<Json> {
    search(State(st), Path("_all".to_string()), uri, body).await
}

/// `POST|GET /{index}/_search`
///
/// `{index}` est une **expression** : un nom, un alias, une liste, un motif.
/// Voir [`crate::selection`].
pub async fn search(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let started = Instant::now();

    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    let param_from = p.number("from")?;
    let param_size = p.number("size")?;
    let param_sort = p.list("sort");
    let param_source = source_filter_opt(&mut p)?;
    // `preference` choisit un shard : ferrite n'en a qu'un, le parametre est
    // donc sans objet — pas ignore, juste sans effet possible.
    p.opt("preference");
    if let Some(v) = p.opt("track_total_hits") {
        check_track_total_hits(&Value::String(v))?;
    }
    if p.opt("q").is_some() {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas la recherche par chaine [q] (query_string) ; utilise le \
             Query DSL",
        ));
    }
    // ES **supporte** ce parametre : le refuser comme un parametre inconnu
    // laisserait croire a une faute de frappe. C'est un refus, et il se nomme.
    if p.opt("rest_total_hits_as_int").is_some() {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [rest_total_hits_as_int] : il change la forme de \
             [hits.total] (un nombre au lieu de {value, relation}) ; voir docs/compat-es7.md",
        ));
    }
    p.done()?;

    let indices = resoudre(&st.catalog, &index, &opts)?;
    // Une seule generation par index pour toute la requete : les `Field` d'une
    // generation n'ont aucun sens dans une autre.
    let generations: Vec<(String, String, std::sync::Arc<Generation>)> = indices
        .iter()
        .map(|i| (i.name.clone(), i.uuid.clone(), i.current()))
        .collect();
    let champs_connus = union_des_champs(&generations);

    let body = parse_body(&body)?;
    let body_obj = match &body {
        Value::Null => Map::new(),
        Value::Object(o) => o.clone(),
        _ => return Err(EsError::parsing("le corps de [_search] doit etre un objet")),
    };
    expect_only(
        &body_obj,
        &[
            "query",
            "from",
            "size",
            "sort",
            "_source",
            "track_total_hits",
            "aggs",
            "aggregations",
        ],
        "_search",
    )?;

    if let Some(v) = body_obj.get("track_total_hits") {
        check_track_total_hits(v)?;
    }

    let from = match param_from {
        Some(v) => v,
        None => body_usize(&body_obj, "from")?.unwrap_or(0),
    };
    let size = match param_size {
        Some(v) => v,
        None => body_usize(&body_obj, "size")?.unwrap_or(DEFAULT_SIZE),
    };

    if from + size > MAX_RESULT_WINDOW {
        return Err(EsError::illegal_argument(format!(
            "Result window is too large, from + size must be less than or equal to: \
             [{MAX_RESULT_WINDOW}] but was [{}].",
            from + size
        )));
    }

    let source = match param_source {
        Some(f) => f,
        None => match body_obj.get("_source") {
            Some(v) => parse_source_body(v)?,
            None => SourceFilter::All,
        },
    };

    // `aggs` et `aggregations` sont deux noms pour la meme chose chez ES.
    let aggs = match (body_obj.get("aggs"), body_obj.get("aggregations")) {
        (Some(_), Some(_)) => {
            return Err(EsError::illegal_argument(
                "[aggs] et [aggregations] sont synonymes : n'en fournis qu'un",
            ))
        }
        (Some(a), None) | (None, Some(a)) => Some(a.clone()),
        (None, None) => None,
    };

    // Une cible par index vise, chacune avec sa requete, ses cles de tri et son
    // verdict sur les agregations.
    let mut cibles: Vec<Cible> = Vec::new();
    let mut ignore: Option<EsError> = None;
    let mut agg_ignore: Option<EsError> = None;
    let mut sort_asc: Vec<bool> = Vec::new();
    // Les index qui n'ont pas pu repondre, au format `_shards.failures` d'ES.
    let mut echecs: Vec<Value> = Vec::new();
    let nb_index = generations.len();

    for (nom, uuid, gen) in generations {
        let sort = match param_sort.as_ref() {
            Some(list) => parse_sort_params(list, &gen),
            None => match body_obj.get("sort") {
                Some(v) => parse_sort_body(v, &gen),
                None => Ok(Vec::new()),
            },
        };
        let sort = match sort {
            Ok(s) => s,
            // Trier sur un champ que cet index ne mappe pas : ES ne fait pas
            // echouer la recherche, il rapporte l'echec **de ce shard** et rend
            // les documents des autres. Ecarter l'index en silence rendrait le
            // meme total qu'ES mais sans dire qu'il manque quelque chose ; le
            // faire echouer entierement rendrait moins de documents qu'ES.
            Err(e) if e.champ_inconnu.is_some() => {
                echecs.push(echec_de_shard(&nom, &uuid, &e, &st.catalog.cluster_uuid));
                continue;
            }
            Err(e) => return Err(e),
        };
        let query = {
            let searcher = gen.searcher();
            let ctx = QueryCtx::new(&gen.fields, &gen.index, &searcher)
                .avec_champs_ailleurs(&champs_connus);
            match body_obj.get("query") {
                Some(v) => build_query(v, &ctx),
                None => Ok(Box::new(tantivy::query::AllQuery) as Box<dyn tantivy::query::Query>),
            }
        };
        let query = match query {
            Ok(q) => q,
            // Ici, le champ n'est connu **d'aucun** index vise : ce n'est plus
            // un mapping heterogene, c'est une faute de frappe. L'erreur est
            // gardee et rendue une fois la boucle finie.
            Err(e) if e.champ_inconnu.is_some() => {
                ignore.get_or_insert(e);
                continue;
            }
            Err(e) => return Err(e),
        };
        // Meme raisonnement pour les agregations : un index qui ne mappe pas le
        // champ agrege n'a aucune valeur a y verser.
        let agrege = match &aggs {
            None => false,
            Some(a) => match crate::aggs::validate(a, &gen) {
                Ok(()) => true,
                Err(e) if e.champ_inconnu.is_some() => {
                    agg_ignore.get_or_insert(e);
                    false
                }
                Err(e) => return Err(e),
            },
        };
        if sort_asc.is_empty() {
            sort_asc = sort.iter().map(|s| s.asc).collect();
        }
        cibles.push(Cible {
            nom,
            gen,
            query,
            sort,
            agrege,
        });
    }

    if cibles.is_empty() {
        // Aucun index n'a su trier : ES groupe alors les echecs de shard en une
        // seule erreur « all shards failed ».
        if !echecs.is_empty() {
            return Err(tous_les_shards_ont_echoue(&echecs));
        }
        if let Some(e) = ignore {
            return Err(e);
        }
    }
    if aggs.is_some() && !cibles.is_empty() && cibles.iter().all(|c| !c.agrege) {
        if let Some(e) = agg_ignore {
            return Err(e);
        }
    }

    let req = SearchRequest {
        aggs,
        from,
        size,
        sort_asc,
        source,
    };

    let outcome = tokio::task::spawn_blocking(move || execute(&cibles, &req))
        .await
        .map_err(|e| EsError::internal(format!("recherche: {e}")))??;

    let mut reponse = Map::new();
    reponse.insert("took".into(), json!(elapsed_ms(started)));
    reponse.insert("timed_out".into(), json!(false));
    // Un index = un shard : ES compte les shards, pas les index.
    let mut shards = json!({
        "total": nb_index,
        "successful": nb_index - echecs.len(),
        "skipped": 0,
        "failed": echecs.len(),
    });
    if !echecs.is_empty() {
        shards["failures"] = Value::Array(echecs);
    }
    reponse.insert("_shards".into(), shards);
    reponse.insert(
        "hits".into(),
        json!({
            // `total` est un objet {value, relation}, pas un entier : un client
            // type le remarque immediatement.
            "total": {"value": outcome.total, "relation": "eq"},
            "max_score": outcome.max_score.map(round_score),
            "hits": outcome.hits,
        }),
    );
    if let Some(aggs) = outcome.aggregations {
        reponse.insert("aggregations".into(), aggs);
    }
    Ok(Json::ok(Value::Object(reponse)))
}

/// `GET|POST /_count` — sans index dans l'URL, ES compte partout.
pub async fn count_all(State(st): State<SharedState>, uri: Uri, body: Bytes) -> EsResult<Json> {
    count(State(st), Path("_all".to_string()), uri, body).await
}

/// `GET|POST /{index}/_count` — combien de documents correspondent.
pub async fn count(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    p.opt("preference");
    if p.opt("q").is_some() {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas la recherche par chaine [q] ; utilise le Query DSL",
        ));
    }
    p.done()?;

    let body = parse_body(&body)?;
    let body_obj = match &body {
        Value::Null => Map::new(),
        Value::Object(o) => o.clone(),
        _ => return Err(EsError::parsing("le corps de [_count] doit etre un objet")),
    };
    expect_only(&body_obj, &["query"], "_count")?;

    let indices = resoudre(&st.catalog, &index, &opts)?;
    let generations: Vec<(String, String, std::sync::Arc<Generation>)> = indices
        .iter()
        .map(|i| (i.name.clone(), i.uuid.clone(), i.current()))
        .collect();
    let champs_connus = union_des_champs(&generations);
    let mut prets: Vec<(std::sync::Arc<Generation>, Box<dyn tantivy::query::Query>)> = Vec::new();
    let mut ignore: Option<EsError> = None;
    for (_, _, gen) in &generations {
        let gen = gen.clone();
        let query = {
            let searcher = gen.searcher();
            let ctx = QueryCtx::new(&gen.fields, &gen.index, &searcher)
                .avec_champs_ailleurs(&champs_connus);
            match body_obj.get("query") {
                Some(v) => build_query(v, &ctx),
                None => Ok(Box::new(tantivy::query::AllQuery) as Box<dyn tantivy::query::Query>),
            }
        };
        match query {
            Ok(q) => prets.push((gen, q)),
            Err(e) if e.champ_inconnu.is_some() => {
                ignore.get_or_insert(e);
            }
            Err(e) => return Err(e),
        }
    }
    if prets.is_empty() {
        if let Some(e) = ignore {
            return Err(e);
        }
    }

    let nb_index = indices.len();
    let total = tokio::task::spawn_blocking(move || {
        let mut total = 0usize;
        for (gen, query) in &prets {
            total += gen.searcher().search(query, &tantivy::collector::Count)?;
        }
        Ok::<_, EsError>(total)
    })
    .await
    .map_err(|e| EsError::internal(format!("count: {e}")))??;

    Ok(Json::ok(json!({
        "count": total,
        "_shards": {"total": nb_index, "successful": nb_index, "skipped": 0, "failed": 0},
    })))
}

/// Tous les noms de champ connus d'au moins un des index vises.
///
/// C'est ce qui distingue « faute de frappe » (personne ne connait ce champ) de
/// « mapping heterogene » (un index quotidien plus recent a un champ de plus).
fn union_des_champs(
    generations: &[(String, String, std::sync::Arc<Generation>)],
) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    // Sur un index unique il n'y a pas d'« ailleurs » : l'ensemble reste vide,
    // et un champ inconnu redevient l'erreur qu'il doit etre.
    if generations.len() < 2 {
        return out;
    }
    for (_, _, gen) in generations {
        out.extend(gen.fields.mapped.keys().cloned());
    }
    out
}

/// L'echec d'un index, au format `_shards.failures[]` d'Elasticsearch.
///
/// Un index = un shard, donc `shard: 0` toujours.
fn echec_de_shard(nom: &str, uuid: &str, e: &EsError, node: &str) -> Value {
    json!({
        "shard": 0,
        "index": nom,
        "node": node,
        "reason": {
            "type": e.ty,
            "reason": e.reason,
            "index_uuid": uuid,
            "index": nom,
        },
    })
}

/// Quand aucun index n'a pu repondre, ES ne rend pas une reponse vide : il rend
/// une erreur qui **groupe** les causes, une par shard.
fn tous_les_shards_ont_echoue(echecs: &[Value]) -> EsError {
    let causes: Vec<Value> = echecs.iter().map(|e| e["reason"].clone()).collect();
    EsError::new(
        axum::http::StatusCode::BAD_REQUEST,
        "search_phase_execution_exception",
        "all shards failed",
    )
    .with("phase", json!("query"))
    .with("grouped", json!(true))
    .with("failed_shards", Value::Array(echecs.to_vec()))
    .avec_racines(causes)
}

/// Lit un entier positif du corps. Une valeur invalide est refusee plutot que
/// remplacee par le defaut : `size: -1` doit se voir, pas devenir `10`.
fn body_usize(obj: &Map<String, Value>, key: &str) -> EsResult<Option<usize>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| {
                EsError::illegal_argument(format!("[{key}] : entier positif attendu, recu {v}"))
            }),
    }
}

/// ferrite compte toujours les hits exactement — `relation` vaut donc toujours
/// `eq`. Seul `false` (ne pas compter) n'a pas d'equivalent.
fn check_track_total_hits(v: &Value) -> EsResult<()> {
    let refused = match v {
        Value::Bool(b) => !*b,
        Value::String(s) => s == "false",
        Value::Number(_) => false,
        _ => true,
    };
    if refused {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [track_total_hits: false] : le total est toujours exact",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// _source
// ---------------------------------------------------------------------------

/// Lit `_source` / `_source_includes` / `_source_excludes` depuis la query
/// string. Renvoie [`SourceFilter::All`] si rien n'est precise.
pub fn source_filter(p: &mut Params) -> EsResult<SourceFilter> {
    Ok(source_filter_opt(p)?.unwrap_or(SourceFilter::All))
}

fn source_filter_opt(p: &mut Params) -> EsResult<Option<SourceFilter>> {
    let includes = p.list("_source_includes").unwrap_or_default();
    let excludes = p.list("_source_excludes").unwrap_or_default();
    let source = p.opt("_source");

    match (source.as_deref(), includes.is_empty(), excludes.is_empty()) {
        (None, true, true) => Ok(None),
        (Some("false"), _, _) => Ok(Some(SourceFilter::None)),
        (Some("true") | None, _, _) => {
            if includes.is_empty() && excludes.is_empty() {
                Ok(Some(SourceFilter::All))
            } else {
                Ok(Some(SourceFilter::Filter { includes, excludes }))
            }
        }
        (Some(list), _, _) => {
            let mut includes = includes;
            includes.extend(list.split(',').map(str::trim).map(str::to_string));
            Ok(Some(SourceFilter::Filter { includes, excludes }))
        }
    }
}

fn parse_source_body(v: &Value) -> EsResult<SourceFilter> {
    match v {
        Value::Bool(true) => Ok(SourceFilter::All),
        Value::Bool(false) => Ok(SourceFilter::None),
        Value::String(s) => Ok(SourceFilter::Filter {
            includes: vec![s.clone()],
            excludes: vec![],
        }),
        Value::Array(a) => Ok(SourceFilter::Filter {
            includes: a
                .iter()
                .map(|x| {
                    x.as_str().map(str::to_string).ok_or_else(|| {
                        EsError::illegal_argument("[_source] : liste de chaines attendue")
                    })
                })
                .collect::<EsResult<_>>()?,
            excludes: vec![],
        }),
        Value::Object(o) => {
            expect_only(o, &["includes", "excludes"], "_source")?;
            let read = |key: &str| -> EsResult<Vec<String>> {
                match o.get(key) {
                    None => Ok(vec![]),
                    Some(Value::String(s)) => Ok(vec![s.clone()]),
                    Some(Value::Array(a)) => a
                        .iter()
                        .map(|x| {
                            x.as_str().map(str::to_string).ok_or_else(|| {
                                EsError::illegal_argument(format!(
                                    "[_source.{key}] : liste de chaines attendue"
                                ))
                            })
                        })
                        .collect(),
                    Some(_) => Err(EsError::illegal_argument(format!(
                        "[_source.{key}] : chaine ou liste attendue"
                    ))),
                }
            };
            Ok(SourceFilter::Filter {
                includes: read("includes")?,
                excludes: read("excludes")?,
            })
        }
        _ => Err(EsError::illegal_argument("[_source] : valeur invalide")),
    }
}

// ---------------------------------------------------------------------------
// sort
// ---------------------------------------------------------------------------

fn parse_sort_body(v: &Value, gen: &crate::engine::Generation) -> EsResult<Vec<SortSpec>> {
    let entries: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut specs = Vec::new();
    for entry in entries {
        match entry {
            Value::String(s) => specs.push(sort_spec(s, None, gen)?),
            Value::Object(o) => {
                for (field, spec) in o {
                    let order = match spec {
                        Value::String(s) => Some(s.clone()),
                        Value::Object(inner) => {
                            expect_only(inner, &["order"], "sort")?;
                            inner
                                .get("order")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        }
                        _ => {
                            return Err(EsError::illegal_argument(
                                "[sort] : chaine ou objet {order} attendu",
                            ))
                        }
                    };
                    specs.push(sort_spec(field, order.as_deref(), gen)?);
                }
            }
            _ => return Err(EsError::illegal_argument("[sort] : entree invalide")),
        }
    }
    Ok(specs)
}

/// `?sort=annee:desc,titre`
fn parse_sort_params(list: &[String], gen: &crate::engine::Generation) -> EsResult<Vec<SortSpec>> {
    list.iter()
        .map(|entry| match entry.split_once(':') {
            Some((field, order)) => sort_spec(field, Some(order), gen),
            None => sort_spec(entry, None, gen),
        })
        .collect()
}

fn sort_spec(
    field: &str,
    order: Option<&str>,
    gen: &crate::engine::Generation,
) -> EsResult<SortSpec> {
    let key = match field {
        "_score" => SortKey::Score,
        "_doc" => SortKey::Doc,
        name => {
            let mapped = gen.fields.get(name).ok_or_else(|| {
                // Le type d'ES, et le marqueur qui permet a une recherche
                // multi-index de n'echouer que sur **cet** index.
                EsError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "query_shard_exception",
                    format!("No mapping found for [{name}] in order to sort on"),
                )
                .sur_champ_inconnu(name)
            })?;
            if mapped.ty.kind() == FieldKind::Text {
                return Err(EsError::illegal_argument(format!(
                    "Fielddata is disabled on [{name}] : ferrite ne trie pas sur un champ [text] \
                     ; utilise un champ [keyword]"
                )));
            }
            SortKey::Field {
                name: name.to_string(),
                kind: mapped.ty.kind(),
            }
        }
    };
    // Defaut d'ES : `desc` sur `_score`, `asc` partout ailleurs.
    let default_asc = !matches!(key, SortKey::Score);
    let asc = match order {
        None => default_asc,
        Some(s) if s.eq_ignore_ascii_case("asc") => true,
        Some(s) if s.eq_ignore_ascii_case("desc") => false,
        Some(s) => {
            return Err(EsError::illegal_argument(format!(
                "[sort] : ordre [{s}] invalide (asc|desc)"
            )))
        }
    };
    Ok(SortSpec { key, asc })
}
