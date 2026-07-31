//! Traduction du Query DSL d'Elasticsearch vers les requetes tantivy.
//!
//! Contrat du module : **tout ce qui n'est pas traduit fidelement est refuse**.
//! Ignorer un `minimum_should_match` ou une clause inconnue produirait des
//! resultats faux presentes comme complets — le pire resultat possible pour ce
//! projet.

use std::ops::Bound;

use serde_json::{Map, Value};
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, ConstScoreQuery, EmptyQuery, Occur, Query, RangeQuery,
    TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::Index;

use crate::error::{EsError, EsResult};
use crate::mapping::{self, FieldKind, FieldType, Fields, TEXT_TOKENIZER};

/// Ce dont la traduction a besoin : le schema resolu et l'index (pour les
/// tokenizers).
pub struct QueryCtx<'a> {
    pub fields: &'a Fields,
    pub index: &'a Index,
}

impl QueryCtx<'_> {
    fn field(&self, name: &str, clause: &str) -> EsResult<(Field, FieldType)> {
        self.fields.get(name).ok_or_else(|| {
            // ES cherche le champ dans le mapping dynamique ; ferrite n'en a pas,
            // donc un champ inconnu est une erreur explicite et non « 0 hit ».
            EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "query_shard_exception",
                format!(
                    "no mapping found for field [{name}] (clause [{clause}]) ; ferrite exige un \
                     mapping explicite"
                ),
            )
        })
    }

    /// Applique l'analyzer du champ `text` a une chaine de requete.
    fn analyze(&self, text: &str) -> EsResult<Vec<String>> {
        let mut analyzer = self.index.tokenizers().get(TEXT_TOKENIZER).ok_or_else(|| {
            EsError::internal(format!("tokenizer [{TEXT_TOKENIZER}] introuvable"))
        })?;
        let mut stream = analyzer.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            out.push(stream.token().text.clone());
        }
        Ok(out)
    }
}

/// Traduit une requete du DSL. `v` est la valeur de la cle `query`.
pub fn build_query(v: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(v, "query")?;
    let (name, body) = single_key(obj, "query")?;
    match name {
        "match_all" => match_all(body),
        "match_none" => {
            expect_only(as_object(body, "match_none")?, &[], "match_none")?;
            Ok(Box::new(EmptyQuery))
        }
        "match" => match_query(body, ctx),
        "term" => term_query(body, ctx),
        "terms" => terms_query(body, ctx),
        "range" => range_query(body, ctx),
        "bool" => bool_query(body, ctx),
        other => Err(EsError::parsing(format!(
            "unknown query [{other}] : ferrite supporte [match_all, match_none, match, term, \
             terms, range, bool]"
        ))),
    }
}

fn match_all(body: &Value) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "match_all")?;
    expect_only(obj, &["boost"], "match_all")?;
    boost(Box::new(AllQuery), obj.get("boost"))
}

fn match_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "match")?;
    let (field_name, spec) = single_key(obj, "match")?;
    let (field, ty) = ctx.field(field_name, "match")?;

    let (query_value, operator, boost_value) = match spec {
        Value::Object(o) => {
            expect_only(o, &["query", "operator", "boost"], "match")?;
            let q = o.get("query").ok_or_else(|| {
                EsError::parsing(format!(
                    "[match] sur [{field_name}] : cle [query] manquante"
                ))
            })?;
            let op = match o.get("operator").and_then(Value::as_str) {
                None => Occur::Should,
                Some(s) if s.eq_ignore_ascii_case("or") => Occur::Should,
                Some(s) if s.eq_ignore_ascii_case("and") => Occur::Must,
                Some(s) => {
                    return Err(EsError::illegal_argument(format!(
                        "[match] : operator [{s}] invalide (or|and)"
                    )))
                }
            };
            (q.clone(), op, o.get("boost").cloned())
        }
        v => (v.clone(), Occur::Should, None),
    };

    let inner: Box<dyn Query> = match ty.kind() {
        FieldKind::Text => {
            let text = match &query_value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                v => {
                    return Err(EsError::illegal_argument(format!(
                        "[match] sur [{field_name}] : valeur {v} invalide"
                    )))
                }
            };
            let tokens = ctx.analyze(&text)?;
            if tokens.is_empty() {
                Box::new(EmptyQuery)
            } else if tokens.len() == 1 {
                Box::new(TermQuery::new(
                    tantivy::Term::from_field_text(field, &tokens[0]),
                    IndexRecordOption::WithFreqs,
                ))
            } else {
                let clauses: Vec<(Occur, Box<dyn Query>)> = tokens
                    .iter()
                    .map(|t| {
                        let q: Box<dyn Query> = Box::new(TermQuery::new(
                            tantivy::Term::from_field_text(field, t),
                            IndexRecordOption::WithFreqs,
                        ));
                        (operator, q)
                    })
                    .collect();
                Box::new(BooleanQuery::new(clauses))
            }
        }
        // Sur un champ non analyse, `match` se comporte comme `term` (ES fait
        // pareil : l'analyzer d'un keyword est `keyword`).
        _ => {
            let tv = mapping::coerce(field_name, ty, &query_value)?;
            Box::new(TermQuery::new(tv.to_term(field), IndexRecordOption::Basic))
        }
    };
    boost(inner, boost_value.as_ref())
}

fn term_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "term")?;
    let (field_name, spec) = single_key(obj, "term")?;
    let (field, ty) = ctx.field(field_name, "term")?;

    let (value, boost_value) = match spec {
        Value::Object(o) => {
            expect_only(o, &["value", "boost", "case_insensitive"], "term")?;
            if o.contains_key("case_insensitive") {
                return Err(EsError::unsupported(
                    "ferrite ne supporte pas [case_insensitive] sur [term]",
                ));
            }
            let v = o.get("value").ok_or_else(|| {
                EsError::parsing(format!("[term] sur [{field_name}] : cle [value] manquante"))
            })?;
            (v.clone(), o.get("boost").cloned())
        }
        v => (v.clone(), None),
    };

    let tv = mapping::coerce(field_name, ty, &value)?;
    let record = if ty.kind() == FieldKind::Text {
        IndexRecordOption::WithFreqs
    } else {
        IndexRecordOption::Basic
    };
    boost(
        Box::new(TermQuery::new(tv.to_term(field), record)),
        boost_value.as_ref(),
    )
}

fn terms_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "terms")?;
    let mut boost_value = None;
    let mut entry: Option<(&String, &Value)> = None;
    for (k, v) in obj {
        if k == "boost" {
            boost_value = Some(v.clone());
        } else if entry.is_some() {
            return Err(EsError::parsing(
                "[terms] n'accepte qu'un seul champ par clause",
            ));
        } else {
            entry = Some((k, v));
        }
    }
    let (field_name, values) =
        entry.ok_or_else(|| EsError::parsing("[terms] : aucun champ fourni"))?;
    let (field, ty) = ctx.field(field_name, "terms")?;
    let list = values.as_array().ok_or_else(|| {
        EsError::illegal_argument(format!(
            "[terms] sur [{field_name}] : une liste de valeurs est attendue (les lookups de \
             termes ne sont pas supportes par ferrite)"
        ))
    })?;

    let clauses: Vec<(Occur, Box<dyn Query>)> = list
        .iter()
        .map(|v| {
            let tv = mapping::coerce(field_name, ty, v)?;
            let q: Box<dyn Query> =
                Box::new(TermQuery::new(tv.to_term(field), IndexRecordOption::Basic));
            Ok((Occur::Should, q))
        })
        .collect::<EsResult<_>>()?;

    // ES donne un score constant aux clauses `terms`.
    let inner: Box<dyn Query> = Box::new(ConstScoreQuery::new(
        Box::new(BooleanQuery::new(clauses)),
        1.0,
    ));
    boost(inner, boost_value.as_ref())
}

fn range_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "range")?;
    let (field_name, spec) = single_key(obj, "range")?;
    let (field, ty) = ctx.field(field_name, "range")?;
    let spec = as_object(spec, "range")?;
    expect_only(spec, &["gte", "gt", "lte", "lt", "boost"], "range")?;

    if ty.kind() == FieldKind::Text {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [range] sur un champ [text] (champ [{field_name}]) ; \
             utilise un champ [keyword]"
        )));
    }

    let to_term = |key: &str| -> EsResult<Option<tantivy::Term>> {
        match spec.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => Ok(Some(mapping::coerce(field_name, ty, v)?.to_term(field))),
        }
    };

    let lower = match (to_term("gte")?, to_term("gt")?) {
        (Some(_), Some(_)) => {
            return Err(EsError::illegal_argument(
                "[range] : [gte] et [gt] sont mutuellement exclusifs",
            ))
        }
        (Some(t), None) => Bound::Included(t),
        (None, Some(t)) => Bound::Excluded(t),
        (None, None) => Bound::Unbounded,
    };
    let upper = match (to_term("lte")?, to_term("lt")?) {
        (Some(_), Some(_)) => {
            return Err(EsError::illegal_argument(
                "[range] : [lte] et [lt] sont mutuellement exclusifs",
            ))
        }
        (Some(t), None) => Bound::Included(t),
        (None, Some(t)) => Bound::Excluded(t),
        (None, None) => Bound::Unbounded,
    };
    if matches!(lower, Bound::Unbounded) && matches!(upper, Bound::Unbounded) {
        return Err(EsError::illegal_argument(format!(
            "[range] sur [{field_name}] : au moins une borne (gte/gt/lte/lt) est requise"
        )));
    }

    let inner: Box<dyn Query> = Box::new(ConstScoreQuery::new(
        Box::new(RangeQuery::new(lower, upper)),
        1.0,
    ));
    boost(inner, spec.get("boost"))
}

fn bool_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "bool")?;
    expect_only(
        obj,
        &[
            "must",
            "should",
            "filter",
            "must_not",
            "minimum_should_match",
            "boost",
        ],
        "bool",
    )?;

    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
    let mut should_count = 0usize;
    let mut has_required = false;

    for (key, occur) in [
        ("must", Occur::Must),
        ("filter", Occur::Must),
        ("should", Occur::Should),
        ("must_not", Occur::MustNot),
    ] {
        let Some(v) = obj.get(key) else { continue };
        let list: Vec<&Value> = match v {
            Value::Array(a) => a.iter().collect(),
            Value::Null => continue,
            other => vec![other],
        };
        for sub in list {
            let mut q = build_query(sub, ctx)?;
            if key == "filter" {
                // Contexte filtre : le score ne doit pas bouger.
                q = Box::new(ConstScoreQuery::new(q, 0.0));
            }
            if occur == Occur::Should {
                should_count += 1;
            } else if occur == Occur::Must {
                has_required = true;
            }
            clauses.push((occur, q));
        }
    }

    if clauses.is_empty() {
        return boost(Box::new(AllQuery), obj.get("boost"));
    }

    // Un `bool` qui n'a que des `must_not` ne matche rien chez tantivy, alors
    // qu'ES l'interprete comme « tous les documents, sauf ceux-la ». On pose la
    // clause positive implicite.
    if !has_required && should_count == 0 {
        clauses.insert(0, (Occur::Must, Box::new(AllQuery)));
        has_required = true;
    }

    // Semantique ES : sans clause obligatoire, au moins un `should` doit matcher.
    let min_should = match obj.get("minimum_should_match") {
        None => {
            if has_required || should_count == 0 {
                0
            } else {
                1
            }
        }
        Some(Value::Number(n)) => n
            .as_i64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| {
                EsError::illegal_argument("[minimum_should_match] : entier positif attendu")
            })?,
        Some(Value::String(s)) => s.trim().parse::<usize>().map_err(|_| {
            EsError::unsupported(format!(
                "ferrite ne supporte que la forme entiere de [minimum_should_match] (recu \
                 [{s}])"
            ))
        })?,
        Some(v) => {
            return Err(EsError::illegal_argument(format!(
                "[minimum_should_match] : valeur {v} invalide"
            )))
        }
    };
    if min_should > should_count {
        return Ok(Box::new(EmptyQuery));
    }

    let inner: Box<dyn Query> = if min_should > 0 {
        Box::new(BooleanQuery::with_minimum_required_clauses(
            clauses, min_should,
        ))
    } else {
        Box::new(BooleanQuery::new(clauses))
    };
    boost(inner, obj.get("boost"))
}

fn boost(query: Box<dyn Query>, value: Option<&Value>) -> EsResult<Box<dyn Query>> {
    match value {
        None | Some(Value::Null) => Ok(query),
        Some(v) => {
            let b = v
                .as_f64()
                .ok_or_else(|| EsError::illegal_argument("[boost] : nombre attendu"))?;
            Ok(Box::new(BoostQuery::new(query, b as f32)))
        }
    }
}

fn as_object<'a>(v: &'a Value, clause: &str) -> EsResult<&'a Map<String, Value>> {
    v.as_object()
        .ok_or_else(|| EsError::parsing(format!("[{clause}] doit etre un objet JSON")))
}

fn single_key<'a>(obj: &'a Map<String, Value>, clause: &str) -> EsResult<(&'a str, &'a Value)> {
    let mut it = obj.iter();
    let Some((k, v)) = it.next() else {
        return Err(EsError::parsing(format!(
            "[{clause}] : objet vide, une clause est attendue"
        )));
    };
    if it.next().is_some() {
        return Err(EsError::parsing(format!(
            "[{clause}] n'accepte qu'une seule cle, {} fournies",
            obj.len()
        )));
    }
    Ok((k.as_str(), v))
}

fn expect_only(obj: &Map<String, Value>, allowed: &[&str], clause: &str) -> EsResult<()> {
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{key}] dans [{clause}] ; parametres acceptes : {allowed:?}"
            )));
        }
    }
    Ok(())
}
