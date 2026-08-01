//! `_search` : parametres, execution, reponse au format exact d'Elasticsearch.

use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::Uri;
use serde_json::{json, Map, Value};

use super::{elapsed_ms, expect_only, parse_body, Json, Params, SharedState};
use crate::dsl::{build_query, QueryCtx};
use crate::error::{EsError, EsResult};
use crate::mapping::FieldKind;
use crate::search::{execute, round_score, SearchRequest, SortKey, SortSpec, SourceFilter};
use crate::MAX_RESULT_WINDOW;

const DEFAULT_SIZE: usize = 10;

/// `POST|GET /_search` — ferrite n'interroge qu'un index a la fois.
pub async fn search_all(uri: Uri) -> EsError {
    EsError::unsupported(format!(
        "ferrite ne supporte pas la recherche multi-index ([{}]) : precise un index, par exemple \
         [/mon_index/_search]",
        uri.path()
    ))
}

/// `POST|GET /{index}/_search`
pub async fn search(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let started = Instant::now();
    let idx = st.catalog.get(&index)?;
    // Une seule generation pour toute la requete : les `Field` d'une generation
    // n'ont aucun sens dans une autre.
    let gen = idx.current();

    let mut p = Params::parse(&uri);
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
    p.done()?;

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

    let sort = match param_sort {
        Some(list) => parse_sort_params(&list, &gen)?,
        None => match body_obj.get("sort") {
            Some(v) => parse_sort_body(v, &gen)?,
            None => Vec::new(),
        },
    };

    let query = {
        let ctx = QueryCtx::new(&gen.fields, &gen.index);
        match body_obj.get("query") {
            Some(v) => build_query(v, &ctx)?,
            None => Box::new(tantivy::query::AllQuery),
        }
    };

    // `aggs` et `aggregations` sont deux noms pour la meme chose chez ES.
    let aggs = match (body_obj.get("aggs"), body_obj.get("aggregations")) {
        (Some(_), Some(_)) => {
            return Err(EsError::illegal_argument(
                "[aggs] et [aggregations] sont synonymes : n'en fournis qu'un",
            ))
        }
        (Some(a), None) | (None, Some(a)) => {
            crate::aggs::validate(a, &gen)?;
            Some(a.clone())
        }
        (None, None) => None,
    };

    let req = SearchRequest {
        query,
        aggs,
        from,
        size,
        sort,
        source,
    };

    let outcome = tokio::task::spawn_blocking(move || execute(&index, &gen, &req))
        .await
        .map_err(|e| EsError::internal(format!("recherche: {e}")))??;

    let mut reponse = Map::new();
    reponse.insert("took".into(), json!(elapsed_ms(started)));
    reponse.insert("timed_out".into(), json!(false));
    reponse.insert(
        "_shards".into(),
        json!({"total": 1, "successful": 1, "skipped": 0, "failed": 0}),
    );
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

/// `GET|POST /{index}/_count` — combien de documents correspondent.
pub async fn count(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let idx = st.catalog.get(&index)?;
    let gen = idx.current();
    let mut p = Params::parse(&uri);
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

    let query = {
        let ctx = QueryCtx::new(&gen.fields, &gen.index);
        match body_obj.get("query") {
            Some(v) => build_query(v, &ctx)?,
            None => Box::new(tantivy::query::AllQuery),
        }
    };
    let total = tokio::task::spawn_blocking(move || {
        gen.searcher()
            .search(&query, &tantivy::collector::Count)
            .map_err(EsError::from)
    })
    .await
    .map_err(|e| EsError::internal(format!("count: {e}")))??;

    Ok(Json::ok(json!({
        "count": total,
        "_shards": {"total": 1, "successful": 1, "skipped": 0, "failed": 0},
    })))
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
                EsError::illegal_argument(format!(
                    "No mapping found for [{name}] in order to sort on"
                ))
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
