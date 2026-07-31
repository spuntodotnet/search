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

    let mut p = Params::parse(&uri);
    let param_from = p.number("from")?;
    let param_size = p.number("size")?;
    let param_sort = p.list("sort");
    let param_source = source_filter_opt(&mut p)?;
    p.opt("preference");
    p.opt("ignore_unavailable");
    p.opt("allow_no_indices");
    p.opt("expand_wildcards");
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
        &["query", "from", "size", "sort", "_source", "track_total_hits"],
        "_search",
    )?;

    if let Some(v) = body_obj.get("track_total_hits") {
        check_track_total_hits(v)?;
    }

    let from = param_from
        .or_else(|| body_obj.get("from").and_then(Value::as_u64).map(|v| v as usize))
        .unwrap_or(0);
    let size = param_size
        .or_else(|| body_obj.get("size").and_then(Value::as_u64).map(|v| v as usize))
        .unwrap_or(DEFAULT_SIZE);

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
        Some(list) => parse_sort_params(&list, &idx)?,
        None => match body_obj.get("sort") {
            Some(v) => parse_sort_body(v, &idx)?,
            None => Vec::new(),
        },
    };

    let query = {
        let ctx = QueryCtx {
            fields: &idx.fields,
            index: idx.tantivy_index(),
        };
        match body_obj.get("query") {
            Some(v) => build_query(v, &ctx)?,
            None => Box::new(tantivy::query::AllQuery),
        }
    };

    let req = SearchRequest {
        query,
        from,
        size,
        sort,
        source,
    };

    let outcome = tokio::task::spawn_blocking(move || execute(&idx, &req))
        .await
        .map_err(|e| EsError::internal(format!("recherche: {e}")))??;

    Ok(Json::ok(json!({
        "took": elapsed_ms(started),
        "timed_out": false,
        "_shards": {"total": 1, "successful": 1, "skipped": 0, "failed": 0},
        "hits": {
            // `total` est un objet {value, relation}, pas un entier : un client
            // type le remarque immediatement.
            "total": {"value": outcome.total, "relation": "eq"},
            "max_score": outcome.max_score.map(round_score),
            "hits": outcome.hits,
        }
    })))
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
                    x.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| EsError::illegal_argument("[_source] : liste de chaines attendue"))
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

fn parse_sort_body(v: &Value, idx: &crate::engine::FerriteIndex) -> EsResult<Vec<SortSpec>> {
    let entries: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut specs = Vec::new();
    for entry in entries {
        match entry {
            Value::String(s) => specs.push(sort_spec(s, None, idx)?),
            Value::Object(o) => {
                for (field, spec) in o {
                    let order = match spec {
                        Value::String(s) => Some(s.clone()),
                        Value::Object(inner) => {
                            expect_only(inner, &["order"], "sort")?;
                            inner.get("order").and_then(Value::as_str).map(str::to_string)
                        }
                        _ => {
                            return Err(EsError::illegal_argument(
                                "[sort] : chaine ou objet {order} attendu",
                            ))
                        }
                    };
                    specs.push(sort_spec(field, order.as_deref(), idx)?);
                }
            }
            _ => return Err(EsError::illegal_argument("[sort] : entree invalide")),
        }
    }
    Ok(specs)
}

/// `?sort=annee:desc,titre`
fn parse_sort_params(
    list: &[String],
    idx: &crate::engine::FerriteIndex,
) -> EsResult<Vec<SortSpec>> {
    list.iter()
        .map(|entry| match entry.split_once(':') {
            Some((field, order)) => sort_spec(field, Some(order), idx),
            None => sort_spec(entry, None, idx),
        })
        .collect()
}

fn sort_spec(
    field: &str,
    order: Option<&str>,
    idx: &crate::engine::FerriteIndex,
) -> EsResult<SortSpec> {
    let key = match field {
        "_score" => SortKey::Score,
        "_doc" => SortKey::Doc,
        name => {
            let (_, ty) = idx.fields.get(name).ok_or_else(|| {
                EsError::illegal_argument(format!(
                    "No mapping found for [{name}] in order to sort on"
                ))
            })?;
            if ty.kind() == FieldKind::Text {
                return Err(EsError::illegal_argument(format!(
                    "Fielddata is disabled on [{name}] : ferrite ne trie pas sur un champ [text] \
                     ; utilise un champ [keyword]"
                )));
            }
            SortKey::Field {
                name: name.to_string(),
                kind: ty.kind(),
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
