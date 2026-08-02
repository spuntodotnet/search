//! Traduction du Query DSL d'Elasticsearch vers les requetes tantivy.
//!
//! Contrat du module : **tout ce qui n'est pas traduit fidelement est refuse**.
//! Ignorer un `minimum_should_match` ou une clause inconnue produirait des
//! resultats faux presentes comme complets — le pire resultat possible pour ce
//! projet.

use std::ops::Bound;

use serde_json::{Map, Value};
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, ConstScoreQuery, EmptyQuery, ExistsQuery, FuzzyTermQuery,
    Occur, PhraseQuery, Query, RangeQuery, RegexQuery, TermQuery, TermSetQuery,
};
use tantivy::schema::IndexRecordOption;
use tantivy::{Index, Searcher, Term};

use crate::analysis::Analyzer;
use crate::dismax::DisMaxQuery;
use crate::error::{EsError, EsResult};
use crate::mapping::{self, FieldKind, Fields, MappedField, TypedValue};
use crate::nested::{Clause, NestedQuery, Predicat, Valeur};

/// Ce dont la traduction a besoin : le schema resolu et l'index (pour les
/// tokenizers).
pub struct QueryCtx<'a> {
    pub fields: &'a Fields,
    pub index: &'a Index,
    /// La racine `nested` en cours de traduction, s'il y en a une.
    ///
    /// Chez Elasticsearch, les sous-champs d'un `nested` vivent dans des
    /// documents caches : les interroger depuis la racine ne rend **rien**, en
    /// silence. ferrite les indexe sur le parent, il pourrait donc y repondre —
    /// et rendre des documents la ou ES n'en rend aucun. Il refuse a la place,
    /// et ce champ est ce qui distingue « dans une clause `nested` » de
    /// « depuis la racine ».
    pub nested_ouvert: std::cell::RefCell<Vec<String>>,
    /// Un `searcher` sur la meme generation, pour les clauses qui se resolvent
    /// en **deux passes** : `has_child` et `has_parent` executent leur requete
    /// interne a la traduction, materialisent l'ensemble des identifiants
    /// concernes, et rendent une recherche sur ces identifiants. C'est ce que
    /// le mono-shard rend possible sans *global ordinals*.
    pub searcher: &'a Searcher,
    /// Les champs connus d'**un autre** index de la meme recherche.
    ///
    /// Vide sur un index unique : un champ absent est alors une faute de frappe,
    /// et c'est une erreur. En multi-index, un champ que cet index ne mappe pas
    /// mais qu'un autre connait n'est plus une faute : c'est un mapping
    /// heterogene, et la clause ne correspond simplement a rien **ici** — ce que
    /// fait Elasticsearch sur un champ non mappe. Sans ca, ecarter l'index
    /// entier ferait perdre les documents que les *autres* clauses d'un `bool`
    /// auraient trouves.
    pub champs_ailleurs: &'a std::collections::BTreeSet<String>,
    /// `index.query.parse.allow_unmapped_fields` de l'index interroge.
    ///
    /// A `true` (le defaut d'ES), une clause sur un champ que le mapping ne
    /// connait pas ne correspond a rien au lieu d'echouer — voir
    /// [`crate::mapping::Mapping::allow_unmapped_fields`].
    pub champs_inconnus_toleres: bool,
}

/// L'ensemble vide, pour les appels qui ne visent qu'un index.
static AUCUN_AUTRE_CHAMP: std::sync::LazyLock<std::collections::BTreeSet<String>> =
    std::sync::LazyLock::new(std::collections::BTreeSet::new);

impl<'a> QueryCtx<'a> {
    pub fn new(fields: &'a Fields, index: &'a Index, searcher: &'a Searcher) -> Self {
        Self {
            fields,
            index,
            nested_ouvert: std::cell::RefCell::new(Vec::new()),
            searcher,
            champs_ailleurs: &AUCUN_AUTRE_CHAMP,
            // Le defaut d'Elasticsearch. Le reglage de l'index le resserre, via
            // [`QueryCtx::selon_le_mapping`].
            champs_inconnus_toleres: true,
        }
    }

    /// Les champs qu'un autre index de la meme recherche connait.
    pub fn avec_champs_ailleurs(mut self, champs: &'a std::collections::BTreeSet<String>) -> Self {
        self.champs_ailleurs = champs;
        self
    }

    /// Applique les reglages de l'index interroge (`allow_unmapped_fields`).
    pub fn selon_le_mapping(mut self, mapping: &crate::mapping::Mapping) -> Self {
        self.champs_inconnus_toleres = mapping.allow_unmapped_fields;
        self
    }

    /// Cette erreur peut-elle etre rattrapee en « ne correspond a rien » ?
    ///
    /// Deux cas, tous deux mesures sur un vrai ES : le champ est connu d'un
    /// **autre** index de la meme recherche (mapping heterogene), ou l'index
    /// laisse passer les champs non mappes (`allow_unmapped_fields`, le defaut
    /// d'ES).
    fn champ_inconnu_tolere(&self, e: &EsError) -> bool {
        e.champ_inconnu
            .as_deref()
            .is_some_and(|c| self.champs_inconnus_toleres || self.champs_ailleurs.contains(c))
    }
}

impl QueryCtx<'_> {
    fn field(&self, name: &str, clause: &str) -> EsResult<MappedField> {
        if let Some(racine) = self.fields.racine_nested(name) {
            if !self.nested_ouvert.borrow().iter().any(|r| r == racine) {
                return Err(EsError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "query_shard_exception",
                    format!(
                        "[{name}] est sous le champ [nested] [{racine}] : il ne peut etre \
                         interroge que dans une clause [nested] sur ce chemin (clause \
                         [{clause}])"
                    ),
                ));
            }
        }
        self.fields.get(name).ok_or_else(|| {
            // Rattrapee plus haut en « ne correspond a rien » quand l'index
            // tolere les champs non mappes (le defaut d'ES) ou qu'un autre index
            // de la meme recherche connait le champ. Si elle sort, c'est que
            // `allow_unmapped_fields` vaut `false` : le message est celui d'ES
            // dans ce cas, et il nomme le reglage a rebasculer.
            EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "query_shard_exception",
                format!(
                    "No field mapping can be found for the field with name [{name}] (clause \
                     [{clause}]) ; [index.query.parse.allow_unmapped_fields] vaut [false] sur cet \
                     index"
                ),
            )
            .sur_champ_inconnu(name)
        })
    }

    /// Applique l'analyzer du champ `text` a une chaine de requete.
    ///
    /// Rend les positions en plus des termes : `match_phrase` en a besoin, et
    /// elles ne sont pas toujours consecutives (l'analyzer peut jeter un token).
    fn analyze(&self, text: &str, analyzer: Analyzer) -> EsResult<Vec<(usize, String)>> {
        let mut analyzer = self
            .index
            .tokenizers()
            .get(&analyzer.tokenizer())
            .ok_or_else(|| {
                EsError::internal(format!("analyzer [{}] introuvable", analyzer.tokenizer()))
            })?;
        let mut stream = analyzer.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            let token = stream.token();
            out.push((token.position, token.text.clone()));
        }
        Ok(out)
    }
}

/// Traduit une requete du DSL. `v` est la valeur de la cle `query`.
pub fn build_query(v: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let q = build_une(v, ctx);
    // Rattrapage au plus pres de la clause : une feuille qui cite un champ que
    // ce mapping ne connait pas devient « ne correspond a rien », et les clauses
    // qui l'entourent (`bool`, `nested`, `dis_max`) continuent de se construire
    // normalement. C'est ce qui fait qu'un `must_not: exists` sur un champ
    // jamais mappe matche tous les documents, comme chez ES, au lieu de faire
    // echouer la recherche entiere.
    match q {
        Err(e) if ctx.champ_inconnu_tolere(&e) => Ok(Box::new(EmptyQuery)),
        autre => autre,
    }
}

fn build_une(v: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(v, "query")?;
    let (name, body) = single_key(obj, "query")?;
    match name {
        "match_all" => match_all(body),
        "match_none" => {
            expect_only(as_object(body, "match_none")?, &[], "match_none")?;
            Ok(Box::new(EmptyQuery))
        }
        "match" => match_query(body, ctx),
        "multi_match" => multi_match_query(body, ctx),
        "match_phrase" => match_phrase_query(body, ctx),
        "match_phrase_prefix" => match_phrase_prefix_query(body, ctx),
        "exists" => exists_query(body, ctx),
        "ids" => ids_query(body, ctx),
        "prefix" => prefix_query(body, ctx),
        "wildcard" => wildcard_query(body, ctx),
        "regexp" => regexp_query(body, ctx),
        "fuzzy" => fuzzy_query(body, ctx),
        "constant_score" => constant_score_query(body, ctx),
        "dis_max" => dis_max_query(body, ctx),
        "term" => term_query(body, ctx),
        "terms" => terms_query(body, ctx),
        "range" => range_query(body, ctx),
        "bool" => bool_query(body, ctx),
        "nested" => nested_query(body, ctx),
        "has_child" => join_query(body, ctx, Sens::VersLeParent),
        "has_parent" => join_query(body, ctx, Sens::VersLEnfant),
        "parent_id" => parent_id_query(body, ctx),
        other => Err(EsError::parsing(format!(
            "unknown query [{other}] : ferrite supporte [match_all, match_none, match, \
             multi_match, match_phrase, match_phrase_prefix, exists, ids, prefix, wildcard, \
             regexp, fuzzy, term, terms, range, bool, constant_score, dis_max, nested, \
             has_child, has_parent, parent_id]"
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

    let (query_value, operator, boost_value) = match spec {
        Value::Object(o) => {
            expect_only(o, &["query", "operator", "boost"], "match")?;
            let q = o.get("query").ok_or_else(|| {
                EsError::parsing(format!(
                    "[match] sur [{field_name}] : cle [query] manquante"
                ))
            })?;
            (
                q.clone(),
                read_operator(o.get("operator"), "match")?,
                o.get("boost").cloned(),
            )
        }
        v => (v.clone(), Occur::Should, None),
    };

    let inner = field_match(field_name, &query_value, operator, "match", ctx)?;
    boost(inner, boost_value.as_ref())
}

/// Le coeur de `match` pour **un** champ : analyse la chaine avec l'analyzer du
/// champ, ou compare la valeur telle quelle si le champ n'est pas analyse.
///
/// Partage avec `multi_match`, qui n'est rien d'autre que ce meme travail
/// repete sur plusieurs champs.
fn field_match(
    field_name: &str,
    value: &Value,
    operator: Occur,
    clause: &str,
    ctx: &QueryCtx,
) -> EsResult<Box<dyn Query>> {
    let MappedField {
        field,
        ty,
        analyzer,
        ..
    } = ctx.field(field_name, clause)?;
    Ok(match ty.kind() {
        FieldKind::Text => {
            let tokens = ctx.analyze(&query_text(field_name, value, clause)?, analyzer)?;
            match tokens.len() {
                0 => Box::new(EmptyQuery),
                1 => Box::new(TermQuery::new(
                    tantivy::Term::from_field_text(field, &tokens[0].1),
                    IndexRecordOption::WithFreqs,
                )),
                _ => {
                    let clauses: Vec<(Occur, Box<dyn Query>)> = tokens
                        .iter()
                        .map(|(_, t)| {
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
        }
        // Sur un champ non analyse, `match` se comporte comme `term` (ES fait
        // pareil : l'analyzer d'un keyword est `keyword`).
        _ => {
            let tv = mapping::coerce_avec(field_name, ty, value, ctx.fields.format_de(field_name))?;
            Box::new(TermQuery::new(tv.to_term(field), IndexRecordOption::Basic))
        }
    })
}

fn query_text(field_name: &str, value: &Value, clause: &str) -> EsResult<String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        v => Err(EsError::illegal_argument(format!(
            "[{clause}] sur [{field_name}] : valeur {v} invalide"
        ))),
    }
}

/// `slop` dans `match_phrase` : seule la phrase exacte est acceptee.
///
/// tantivy et Lucene ne comptent pas les deplacements de la meme facon des que
/// la phrase depasse deux termes : cherchee comme `un deux trois`, la phrase
/// `deux un trois` matche a `slop=2` chez Elasticsearch, et seulement a
/// `slop=3` chez tantivy. Accepter le parametre rendrait donc **moins de
/// documents** qu'ES sur la meme requete, sans que rien ne le signale.
fn read_slop(value: Option<&Value>, clause: &str) -> EsResult<u32> {
    let Some(value) = value else { return Ok(0) };
    let n = value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| EsError::illegal_argument("[slop] : entier positif attendu"))?;
    if n > 0 {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [slop] dans [{clause}] : tantivy et Lucene comptent les \
             deplacements differemment au-dela de deux termes, et le resultat differerait de \
             celui d'Elasticsearch sans que rien ne le signale (voir docs/compat.md). La phrase \
             exacte (slop absent ou 0) est supportee."
        )));
    }
    Ok(0)
}

fn read_operator(value: Option<&Value>, clause: &str) -> EsResult<Occur> {
    match value.and_then(Value::as_str) {
        None => Ok(Occur::Should),
        Some(s) if s.eq_ignore_ascii_case("or") => Ok(Occur::Should),
        Some(s) if s.eq_ignore_ascii_case("and") => Ok(Occur::Must),
        Some(s) => Err(EsError::illegal_argument(format!(
            "[{clause}] : operator [{s}] invalide (or|and)"
        ))),
    }
}

/// `multi_match` : la meme chaine cherchee dans plusieurs champs.
///
/// C'est la clause d'une barre de recherche ordinaire. Deux strategies de
/// score, celles qui couvrent l'usage courant :
///
/// - `best_fields` (defaut chez ES) : le score du meilleur champ l'emporte
///   (`dis_max`), avec un `tie_breaker` optionnel pour tenir compte des autres.
/// - `most_fields` : les scores de tous les champs s'additionnent.
fn multi_match_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "multi_match")?;
    expect_only(
        obj,
        &[
            "query",
            "fields",
            "type",
            "operator",
            "boost",
            "tie_breaker",
        ],
        "multi_match",
    )?;

    let value = obj
        .get("query")
        .ok_or_else(|| EsError::parsing("[multi_match] : cle [query] manquante"))?;
    let fields = obj.get("fields").and_then(Value::as_array).ok_or_else(|| {
        EsError::illegal_argument(
            "[multi_match] : [fields] est obligatoire et doit etre une liste (ferrite n'a pas de \
             champ de recherche par defaut)",
        )
    })?;
    if fields.is_empty() {
        return Err(EsError::illegal_argument(
            "[multi_match] : [fields] ne peut pas etre vide",
        ));
    }
    let operator = read_operator(obj.get("operator"), "multi_match")?;
    let ty = obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("best_fields");
    if !matches!(ty, "best_fields" | "most_fields") {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [type: {ty}] dans [multi_match] ; types acceptes : \
             best_fields, most_fields"
        )));
    }
    let tie_breaker = match obj.get("tie_breaker") {
        None => 0.0f32,
        Some(v) => v
            .as_f64()
            .ok_or_else(|| EsError::illegal_argument("[tie_breaker] : nombre attendu"))?
            as f32,
    };
    if ty == "most_fields" && obj.contains_key("tie_breaker") {
        return Err(EsError::illegal_argument(
            "[tie_breaker] ne s'applique qu'a [type: best_fields]",
        ));
    }

    let mut subs: Vec<Box<dyn Query>> = Vec::with_capacity(fields.len());
    for spec in fields {
        let spec = spec.as_str().ok_or_else(|| {
            EsError::illegal_argument("[multi_match.fields] : liste de chaines attendue")
        })?;
        // Syntaxe d'ES pour ponderer un champ : `titre^3`.
        let (name, field_boost) = match spec.split_once('^') {
            Some((name, b)) => {
                let b = b.trim().parse::<f32>().map_err(|_| {
                    EsError::illegal_argument(format!(
                        "[multi_match.fields] : ponderation invalide dans [{spec}]"
                    ))
                })?;
                (name, Some(b))
            }
            None => (spec, None),
        };
        if name.contains('*') {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas les motifs de champ dans [multi_match.fields] (recu \
                 [{spec}]) : nomme les champs un par un"
            )));
        }
        let mut q = field_match(name, value, operator, "multi_match", ctx)?;
        if let Some(b) = field_boost {
            q = Box::new(BoostQuery::new(q, b));
        }
        subs.push(q);
    }

    let inner: Box<dyn Query> = if ty == "best_fields" {
        Box::new(DisMaxQuery::new(subs, tie_breaker))
    } else {
        Box::new(BooleanQuery::new(
            subs.into_iter().map(|q| (Occur::Should, q)).collect(),
        ))
    };
    boost(inner, obj.get("boost"))
}

/// `match_phrase` : les termes dans cet ordre, cote a cote.
fn match_phrase_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "match_phrase")?;
    let (field_name, spec) = single_key(obj, "match_phrase")?;
    let MappedField {
        field,
        ty,
        analyzer,
        ..
    } = ctx.field(field_name, "match_phrase")?;

    let (value, slop, boost_value) = match spec {
        Value::Object(o) => {
            expect_only(o, &["query", "slop", "boost"], "match_phrase")?;
            let q = o.get("query").ok_or_else(|| {
                EsError::parsing(format!(
                    "[match_phrase] sur [{field_name}] : cle [query] manquante"
                ))
            })?;
            (
                q.clone(),
                read_slop(o.get("slop"), "match_phrase")?,
                o.get("boost").cloned(),
            )
        }
        v => (v.clone(), 0, None),
    };

    let inner: Box<dyn Query> = match ty.kind() {
        FieldKind::Text => {
            let tokens = ctx.analyze(&query_text(field_name, &value, "match_phrase")?, analyzer)?;
            match tokens.len() {
                0 => Box::new(EmptyQuery),
                // Une phrase d'un seul terme est un `term` : tantivy exige au
                // moins deux termes pour une PhraseQuery.
                1 => Box::new(TermQuery::new(
                    tantivy::Term::from_field_text(field, &tokens[0].1),
                    IndexRecordOption::WithFreqs,
                )),
                _ => {
                    let terms: Vec<(usize, tantivy::Term)> = tokens
                        .iter()
                        .map(|(pos, t)| (*pos, tantivy::Term::from_field_text(field, t)))
                        .collect();
                    Box::new(PhraseQuery::new_with_offset_and_slop(terms, slop))
                }
            }
        }
        // Sur un champ non analyse, une phrase ne peut etre que la valeur
        // entiere — c'est aussi ce que fait ES.
        _ => {
            if slop != 0 {
                return Err(EsError::illegal_argument(format!(
                    "[match_phrase] : [slop] n'a pas de sens sur le champ non analyse \
                     [{field_name}]"
                )));
            }
            let tv =
                mapping::coerce_avec(field_name, ty, &value, ctx.fields.format_de(field_name))?;
            Box::new(TermQuery::new(tv.to_term(field), IndexRecordOption::Basic))
        }
    };
    boost(inner, boost_value.as_ref())
}

/// `match_phrase_prefix` : les termes dans cet ordre, le dernier n'etant qu'un
/// debut de mot.
///
/// C'est la clause d'une barre de recherche qui complete pendant la frappe :
/// `"reduction de bru"` doit trouver `reduction de bruit` avant meme que le mot
/// soit fini.
fn match_phrase_prefix_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "match_phrase_prefix")?;
    let (field_name, spec) = single_key(obj, "match_phrase_prefix")?;
    let MappedField {
        field,
        ty,
        analyzer,
        ..
    } = ctx.field(field_name, "match_phrase_prefix")?;

    let (value, max_expansions, boost_value) = match spec {
        Value::Object(o) => {
            expect_only(
                o,
                &["query", "slop", "boost", "max_expansions", "analyzer"],
                "match_phrase_prefix",
            )?;
            if o.contains_key("analyzer") {
                return Err(EsError::unsupported(
                    "ferrite ne supporte pas [analyzer] dans [match_phrase_prefix] : la chaine \
                     est analysee avec l'analyzer du champ",
                ));
            }
            read_slop(o.get("slop"), "match_phrase_prefix")?;
            let q = o.get("query").ok_or_else(|| {
                EsError::parsing(format!(
                    "[match_phrase_prefix] sur [{field_name}] : cle [query] manquante"
                ))
            })?;
            let max = match o.get("max_expansions") {
                None => 50,
                Some(v) => v
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .ok_or_else(|| {
                        EsError::illegal_argument("[max_expansions] : entier positif attendu")
                    })?,
            };
            (q.clone(), max, o.get("boost").cloned())
        }
        v => (v.clone(), 50, None),
    };

    let inner: Box<dyn Query> = match ty.kind() {
        FieldKind::Text => {
            let tokens = ctx.analyze(
                &query_text(field_name, &value, "match_phrase_prefix")?,
                analyzer,
            )?;
            match tokens.len() {
                0 => Box::new(EmptyQuery),
                // Un seul terme : il n'y a plus de phrase, et Lucene reecrit
                // alors la clause en **disjonction des termes developpes**,
                // chacun score comme un `term` ordinaire. Mesure faite contre
                // ES 8.15 : `match_phrase_prefix [audit]` y rend exactement le
                // score de `match [audit]`. tantivy, lui, donnerait un score
                // constant a ce cas — memes documents, mais dans un ordre qui
                // n'est plus celui d'un moteur de recherche.
                1 => {
                    let clauses: Vec<(Occur, Box<dyn Query>)> =
                        termes_avec_prefixe(ctx, field, &tokens[0].1, max_expansions)?
                            .into_iter()
                            .map(|t| {
                                let q: Box<dyn Query> = Box::new(TermQuery::new(
                                    tantivy::Term::from_field_text(field, &t),
                                    IndexRecordOption::WithFreqs,
                                ));
                                (Occur::Should, q)
                            })
                            .collect();
                    if clauses.is_empty() {
                        Box::new(EmptyQuery)
                    } else {
                        Box::new(BooleanQuery::new(clauses))
                    }
                }
                // Plusieurs termes : une phrase dont le dernier mot est
                // developpe. `PhrasePrefixQuery` de tantivy sait la resoudre,
                // mais **ne la score pas** : son poids BM25 ignore le terme
                // developpe, et rend un score constant (mesure : 1.0 partout,
                // la ou ES classe). Un moteur de recherche qui ne classe pas
                // la clause d'une barre de recherche ne sert a rien, donc le
                // developpement est fait ici : une phrase par terme trouve,
                // scorees comme des phrases — ce qui redonne exactement le
                // score d'ES quand le prefixe ne developpe qu'un terme.
                _ => {
                    let (dernier_pos, dernier) = tokens.last().expect("au moins deux termes");
                    let debut: Vec<(usize, tantivy::Term)> = tokens[..tokens.len() - 1]
                        .iter()
                        .map(|(pos, t)| (*pos, tantivy::Term::from_field_text(field, t)))
                        .collect();
                    let clauses: Vec<(Occur, Box<dyn Query>)> =
                        termes_avec_prefixe(ctx, field, dernier, max_expansions)?
                            .into_iter()
                            .map(|t| {
                                let mut termes = debut.clone();
                                termes.push((
                                    *dernier_pos,
                                    tantivy::Term::from_field_text(field, &t),
                                ));
                                let q: Box<dyn Query> =
                                    Box::new(PhraseQuery::new_with_offset(termes));
                                (Occur::Should, q)
                            })
                            .collect();
                    if clauses.is_empty() {
                        Box::new(EmptyQuery)
                    } else {
                        Box::new(BooleanQuery::new(clauses))
                    }
                }
            }
        }
        // Une phrase suppose des positions, qu'un champ non analyse n'a pas.
        // ES refuse ici, avec ce message : ferrite le reprend mot pour mot,
        // pour qu'un client ne voie pas la difference.
        _ => {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "query_shard_exception",
                format!(
                    "failed to create query: Can only use phrase prefix queries on text fields - \
                     not on [{field_name}] which is of type [{}]",
                    ty.name()
                ),
            ))
        }
    };
    boost(inner, boost_value.as_ref())
}

/// Les termes indexes qui commencent par ce prefixe, dans l'ordre du
/// dictionnaire et bornes par `max`.
///
/// C'est le developpement que fait Lucene avant de scorer : le prendre au
/// dictionnaire de termes, segment par segment, coute le parcours d'une plage
/// et rien de plus.
fn termes_avec_prefixe(
    ctx: &QueryCtx,
    field: tantivy::schema::Field,
    prefixe: &str,
    max: u32,
) -> EsResult<Vec<String>> {
    let mut trouves = std::collections::BTreeSet::new();
    for segment in ctx.searcher.segment_readers() {
        let inverse = segment
            .inverted_index(field)
            .map_err(|e| EsError::internal(format!("index inverse illisible : {e}")))?;
        let mut flux = inverse
            .terms()
            .range()
            .ge(prefixe.as_bytes())
            .into_stream()
            .map_err(|e| EsError::internal(format!("dictionnaire de termes illisible : {e}")))?;
        let mut pris = 0u32;
        while flux.advance() {
            let Ok(terme) = std::str::from_utf8(flux.key()) else {
                continue;
            };
            if !terme.starts_with(prefixe) {
                break;
            }
            trouves.insert(terme.to_string());
            pris += 1;
            // Chaque segment est trie : ses `max` premiers termes suffisent a
            // contenir les `max` premiers de leur union.
            if pris >= max {
                break;
            }
        }
    }
    Ok(trouves.into_iter().take(max as usize).collect())
}

/// `exists` : les documents qui ont au moins une valeur pour ce champ.
fn exists_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "exists")?;
    expect_only(obj, &["field", "boost"], "exists")?;
    let name = obj
        .get("field")
        .and_then(Value::as_str)
        .ok_or_else(|| EsError::illegal_argument("[exists] : cle [field] manquante"))?;
    let MappedField { field, ty, .. } = ctx.field(name, "exists")?;

    let inner: Box<dyn Query> = match ty.kind() {
        // Les champs `text` n'ont pas de fast field (ce serait doubler le
        // stockage du texte) : « avoir une valeur » s'y lit dans l'index
        // inverse, comme « avoir au moins un terme ».
        FieldKind::Text => Box::new(RangeQuery::new(
            Bound::Included(tantivy::Term::from_field_text(field, "")),
            Bound::Unbounded,
        )),
        _ => Box::new(ExistsQuery::new(name.to_string(), false)),
    };
    // ES donne un score constant a `exists`.
    boost(Box::new(ConstScoreQuery::new(inner, 1.0)), obj.get("boost"))
}

fn term_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "term")?;
    let (field_name, spec) = single_key(obj, "term")?;
    let MappedField { field, ty, .. } = ctx.field(field_name, "term")?;

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

    let tv = mapping::coerce_avec(field_name, ty, &value, ctx.fields.format_de(field_name))?;
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
    let MappedField { field, ty, .. } = ctx.field(field_name, "terms")?;
    let list = values.as_array().ok_or_else(|| {
        EsError::illegal_argument(format!(
            "[terms] sur [{field_name}] : une liste de valeurs est attendue (les lookups de \
             termes ne sont pas supportes par ferrite)"
        ))
    })?;

    let clauses: Vec<(Occur, Box<dyn Query>)> = list
        .iter()
        .map(|v| {
            let tv = mapping::coerce_avec(field_name, ty, v, ctx.fields.format_de(field_name))?;
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
    let MappedField { field, ty, .. } = ctx.field(field_name, "range")?;
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
            Some(v) => Ok(Some(
                mapping::coerce_avec(field_name, ty, v, ctx.fields.format_de(field_name))?
                    .to_term(field),
            )),
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

/// `ids` : les documents dont l'identifiant figure dans la liste.
fn ids_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "ids")?;
    expect_only(obj, &["values", "boost"], "ids")?;
    let values = obj
        .get("values")
        .and_then(Value::as_array)
        .ok_or_else(|| EsError::illegal_argument("[ids] : [values] (liste) est obligatoire"))?;

    let clauses: Vec<(Occur, Box<dyn Query>)> = values
        .iter()
        .map(|v| {
            let id = v
                .as_str()
                .ok_or_else(|| EsError::illegal_argument("[ids.values] : chaines attendues"))?;
            let q: Box<dyn Query> = Box::new(TermQuery::new(
                tantivy::Term::from_field_text(ctx.fields.id, id),
                IndexRecordOption::Basic,
            ));
            Ok((Occur::Should, q))
        })
        .collect::<EsResult<_>>()?;
    boost(
        Box::new(ConstScoreQuery::new(
            Box::new(BooleanQuery::new(clauses)),
            1.0,
        )),
        obj.get("boost"),
    )
}

/// Lit la forme courte (`{"champ": "valeur"}`) ou longue
/// (`{"champ": {"value": ..., "boost": ...}}`) d'une clause a un champ.
fn valeur_et_boost<'a>(
    obj: &'a Map<String, Value>,
    clause: &str,
    extra: &[&str],
) -> EsResult<(&'a str, String, Option<Value>)> {
    let (champ, spec) = single_key(obj, clause)?;
    match spec {
        Value::Object(o) => {
            let mut permis = vec!["value", "boost"];
            permis.extend_from_slice(extra);
            expect_only(o, &permis, clause)?;
            let v = o.get("value").ok_or_else(|| {
                EsError::parsing(format!("[{clause}] sur [{champ}] : cle [value] manquante"))
            })?;
            Ok((
                champ,
                query_text(champ, v, clause)?,
                o.get("boost").cloned(),
            ))
        }
        v => Ok((champ, query_text(champ, v, clause)?, None)),
    }
}

/// Un champ interrogeable par motif doit etre non analyse : sur un `text`, ES
/// compare au **terme** indexe, ce qui surprend plus souvent que ca n'aide.
fn champ_de_motif(ctx: &QueryCtx, champ: &str, clause: &str) -> EsResult<tantivy::schema::Field> {
    let MappedField { field, ty, .. } = ctx.field(champ, clause)?;
    if ty.kind() != FieldKind::Keyword && ty.kind() != FieldKind::Text {
        return Err(EsError::illegal_argument(format!(
            "[{clause}] ne s'applique qu'a un champ [text] ou [keyword] ; [{champ}] est de type \
             [{}]",
            ty.name()
        )));
    }
    Ok(field)
}

/// `prefix` : les termes qui commencent par cette chaine. Non analysee, comme
/// chez ES.
fn prefix_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "prefix")?;
    let (champ, valeur, boost_value) =
        valeur_et_boost(obj, "prefix", &["case_insensitive", "rewrite"])?;
    refuser_rewrite(obj, "prefix")?;
    let insensible = lire_insensible(obj, "prefix")?;
    let field = champ_de_motif(ctx, champ, "prefix")?;
    let motif = format!(
        "(?s){}(?s:.*)",
        crate::regexp::litteral(&valeur, insensible)
    );
    let q = RegexQuery::from_pattern(&motif, field)
        .map_err(|e| EsError::illegal_argument(format!("[prefix] : {e}")))?;
    boost(
        Box::new(ConstScoreQuery::new(Box::new(q), 1.0)),
        boost_value.as_ref(),
    )
}

/// `wildcard` : `*` remplace n'importe quelle suite, `?` un seul caractere, et
/// `\` echappe le caractere suivant.
fn wildcard_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "wildcard")?;
    let (champ, valeur, boost_value) = valeur_et_boost(
        obj,
        "wildcard",
        &["case_insensitive", "rewrite", "wildcard"],
    )?;
    refuser_rewrite(obj, "wildcard")?;
    let insensible = lire_insensible(obj, "wildcard")?;
    let field = champ_de_motif(ctx, champ, "wildcard")?;

    let motif = format!("(?s){}", crate::regexp::joker(&valeur, insensible));
    let q = RegexQuery::from_pattern(&motif, field)
        .map_err(|e| EsError::illegal_argument(format!("[wildcard] : {e}")))?;
    boost(
        Box::new(ConstScoreQuery::new(Box::new(q), 1.0)),
        boost_value.as_ref(),
    )
}

/// `regexp` : le motif est confronte aux **termes** indexes, ancre des deux
/// cotes.
///
/// C'est la clause qu'un service ecrit pour ses filtres « contient / commence
/// par / finit par », souvent insensibles a la casse. La syntaxe est celle de
/// Lucene, pas celle du crate `regex` : elle est traduite par [`crate::regexp`],
/// qui refuse ce qu'un automate ne sait pas construire plutot que de le prendre
/// pour un litteral (voir le module pour le detail des divergences).
fn regexp_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "regexp")?;
    let (champ, valeur, boost_value) = valeur_et_boost(
        obj,
        "regexp",
        &[
            "case_insensitive",
            "flags",
            "rewrite",
            "max_determinized_states",
        ],
    )?;
    refuser_rewrite(obj, "regexp")?;
    let insensible = lire_insensible(obj, "regexp")?;
    let spec = single_key(obj, "regexp")?.1;
    let params = spec.as_object();

    // `max_determinized_states` borne la taille de l'automate chez Lucene.
    // tantivy a sa propre borne, mais elle ne compte pas la meme chose :
    // l'accepter en le laissant sans effet serait promettre une protection qui
    // n'existe pas.
    if params.is_some_and(|o| o.contains_key("max_determinized_states")) {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [max_determinized_states] dans [regexp] : tantivy borne \
             l'automate autrement, et accepter le parametre sans l'appliquer promettrait une \
             protection qui n'existe pas",
        ));
    }
    let flags = match params.and_then(|o| o.get("flags")) {
        None => crate::regexp::Flags::default(),
        Some(Value::String(s)) => crate::regexp::Flags::lire(s)?,
        Some(_) => {
            return Err(EsError::illegal_argument(
                "[regexp] : [flags] attend une chaine (par exemple \"COMPLEMENT|INTERVAL\")",
            ))
        }
    };

    let field = champ_de_motif(ctx, champ, "regexp")?;
    let motif = crate::regexp::vers_regex(&valeur, flags, insensible)?;
    let q = RegexQuery::from_pattern(&motif, field).map_err(|e| {
        EsError::illegal_argument(format!(
            "[regexp] : motif [{valeur}] refuse par l'automate : {e}"
        ))
    })?;
    // ES rend un score constant sur une requete multi-termes reecrite.
    boost(
        Box::new(ConstScoreQuery::new(Box::new(q), 1.0)),
        boost_value.as_ref(),
    )
}

/// `fuzzy` : les termes a faible distance d'edition.
///
/// `fuzziness` suit la regle `AUTO` d'ES — 0 sous 3 caracteres, 1 jusqu'a 5,
/// 2 au-dela — ou une distance explicite.
fn fuzzy_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "fuzzy")?;
    let (champ, valeur, boost_value) = valeur_et_boost(
        obj,
        "fuzzy",
        &[
            "fuzziness",
            "transpositions",
            "prefix_length",
            "max_expansions",
            "rewrite",
        ],
    )?;
    let spec = single_key(obj, "fuzzy")?.1;
    let params = spec.as_object();

    if params.is_some_and(|o| o.contains_key("prefix_length")) {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [prefix_length] dans [fuzzy]",
        ));
    }
    let distance = match params.and_then(|o| o.get("fuzziness")) {
        None => fuzziness_auto(&valeur),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("auto") => fuzziness_auto(&valeur),
        Some(Value::Number(n)) => n.as_u64().unwrap_or(2).min(2) as u8,
        Some(Value::String(s)) => s.parse::<u8>().map(|d| d.min(2)).map_err(|_| {
            EsError::unsupported(format!(
                "ferrite ne supporte que [AUTO] ou une distance entiere pour [fuzziness] (recu \
                 [{s}])"
            ))
        })?,
        Some(_) => return Err(EsError::illegal_argument("[fuzziness] : valeur invalide")),
    };
    // ES compte une transposition comme une seule operation.
    let transpositions = params
        .and_then(|o| o.get("transpositions"))
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let MappedField { field, .. } = ctx.field(champ, "fuzzy")?;
    let terme = tantivy::Term::from_field_text(field, &valeur);
    let q = FuzzyTermQuery::new(terme, distance, transpositions);
    boost(
        Box::new(ConstScoreQuery::new(Box::new(q), 1.0)),
        boost_value.as_ref(),
    )
}

/// La regle `AUTO` d'Elasticsearch.
fn fuzziness_auto(terme: &str) -> u8 {
    match terme.chars().count() {
        0..=2 => 0,
        3..=5 => 1,
        _ => 2,
    }
}

/// `case_insensitive` : le repliement de casse **ASCII** d'Elasticsearch.
fn lire_insensible(obj: &Map<String, Value>, clause: &str) -> EsResult<bool> {
    let (_, spec) = single_key(obj, clause)?;
    match spec.as_object().and_then(|o| o.get("case_insensitive")) {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(EsError::illegal_argument(format!(
            "[{clause}] : [case_insensitive] attend un booleen"
        ))),
    }
}

/// `rewrite` choisit la strategie de reecriture d'une requete multi-termes chez
/// Lucene, et avec elle le mode de scoring. ferrite n'en a qu'une (score
/// constant) : l'accepter sans effet changerait les scores en silence.
fn refuser_rewrite(obj: &Map<String, Value>, clause: &str) -> EsResult<()> {
    let (_, spec) = single_key(obj, clause)?;
    if spec.as_object().is_some_and(|o| o.contains_key("rewrite")) {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [rewrite] dans [{clause}] : il ne reecrit une requete \
             multi-termes que d'une facon, a score constant"
        )));
    }
    Ok(())
}

/// `constant_score` : le filtre decide, le score est fixe.
fn constant_score_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "constant_score")?;
    expect_only(obj, &["filter", "boost"], "constant_score")?;
    let filtre = obj
        .get("filter")
        .ok_or_else(|| EsError::illegal_argument("[constant_score] : [filter] est obligatoire"))?;
    let inner = build_query(filtre, ctx)?;
    let score = match obj.get("boost") {
        None => 1.0,
        Some(v) => v
            .as_f64()
            .ok_or_else(|| EsError::illegal_argument("[boost] : nombre attendu"))?
            as f32,
    };
    Ok(Box::new(ConstScoreQuery::new(inner, score)))
}

/// `dis_max` : le meilleur score l'emporte (voir [`crate::dismax`]).
fn dis_max_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "dis_max")?;
    expect_only(obj, &["queries", "tie_breaker", "boost"], "dis_max")?;
    let queries = obj
        .get("queries")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            EsError::illegal_argument("[dis_max] : [queries] (liste) est obligatoire")
        })?;
    if queries.is_empty() {
        return Err(EsError::illegal_argument(
            "[dis_max.queries] ne peut pas etre vide",
        ));
    }
    let sous: Vec<Box<dyn Query>> = queries
        .iter()
        .map(|q| build_query(q, ctx))
        .collect::<EsResult<_>>()?;
    let tie = match obj.get("tie_breaker") {
        None => 0.0f32,
        Some(v) => v
            .as_f64()
            .ok_or_else(|| EsError::illegal_argument("[tie_breaker] : nombre attendu"))?
            as f32,
    };
    boost(Box::new(DisMaxQuery::new(sous, tie)), obj.get("boost"))
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

// ---------------------------------------------------------------------------
// nested
// ---------------------------------------------------------------------------

/// `{"nested": {"path": "lignes", "query": {...}}}`.
///
/// La requete interne sert deux fois : telle quelle comme **pre-filtre** (elle
/// rend un sur-ensemble exact, avec les postings et le score de tantivy), et
/// traduite en [`Clause`] pour la **verification** element par element. Voir
/// [`crate::nested`] pour le pourquoi de ce double emploi.
fn nested_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "nested")?;
    expect_only(
        obj,
        &[
            "path",
            "query",
            "score_mode",
            "boost",
            "inner_hits",
            "ignore_unmapped",
        ],
        "nested",
    )?;
    for refuse in ["inner_hits", "ignore_unmapped"] {
        if obj.contains_key(refuse) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{refuse}] dans [nested]"
            )));
        }
    }
    // Le score d'une clause `nested` est celui du pre-filtre, calcule a plat :
    // ferrite n'a pas de document par element, donc pas de score par element.
    // Le dire plutot que de rendre un classement qui n'est pas celui demande.
    if let Some(mode) = obj.get("score_mode").and_then(Value::as_str) {
        if mode != "none" && mode != "avg" {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [score_mode: {mode}] dans [nested] ; le score est celui \
                 de la requete interne evaluee a plat (voir docs/compat.md)"
            )));
        }
    }

    let path = obj
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| EsError::parsing("[nested] : cle [path] manquante"))?;
    if !ctx.fields.nested.contains(path) {
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "query_shard_exception",
            format!("[nested] : [{path}] n'est pas un champ de type [nested] dans ce mapping"),
        ));
    }
    let interne = obj
        .get("query")
        .ok_or_else(|| EsError::parsing("[nested] : cle [query] manquante"))?;

    ctx.nested_ouvert.borrow_mut().push(path.to_string());
    let construit = (|| {
        // Le pre-filtre doit etre un **sur-ensemble** des documents attendus.
        // Une negation ne l'est pas : `must_not: ref=vis` ecarterait a plat un
        // document dont une *autre* ligne satisfait la clause. On la retire
        // donc du pre-filtre — la verification par element, elle, la garde.
        let prefiltre = build_query(&sans_negations(interne), ctx)?;
        let clause = clause_nested(interne, ctx, path)?;
        EsResult::Ok((prefiltre, clause))
    })();
    ctx.nested_ouvert.borrow_mut().pop();
    let (prefiltre, clause) = construit?;
    let q: Box<dyn Query> = Box::new(NestedQuery::new(path.to_string(), prefiltre, clause)?);
    boost(q, obj.get("boost"))
}

/// Traduit la requete interne d'un `nested` en conditions verifiables sur les
/// colonnes. Tout ce qui ne s'y reduit pas est refuse explicitement.
fn clause_nested(v: &Value, ctx: &QueryCtx, racine: &str) -> EsResult<Clause> {
    let obj = as_object(v, "nested.query")?;
    let (name, body) = single_key(obj, "nested.query")?;
    match name {
        "match_all" => Ok(Clause::Tous),
        "match_none" => Ok(Clause::Aucun),
        "term" | "match" => {
            let o = as_object(body, name)?;
            let (champ, spec) = single_key(o, name)?;
            let valeur = match spec {
                Value::Object(sub) => sub
                    .get("value")
                    .or_else(|| sub.get("query"))
                    .cloned()
                    .ok_or_else(|| EsError::parsing(format!("[{name}] : valeur manquante")))?,
                v => v.clone(),
            };
            let mf = champ_nested(ctx, champ, racine, name)?;
            Ok(Clause::Champ {
                chemin: champ.to_string(),
                champ: mf,
                predicat: Predicat::Parmi(vec![valeur_de(ctx, champ, mf, &valeur)?]),
            })
        }
        "terms" => {
            let o = as_object(body, "terms")?;
            let (champ, spec) = single_key(o, "terms")?;
            let liste = spec
                .as_array()
                .ok_or_else(|| EsError::parsing("[terms] attend un tableau"))?;
            let mf = champ_nested(ctx, champ, racine, "terms")?;
            let vals = liste
                .iter()
                .map(|v| valeur_de(ctx, champ, mf, v))
                .collect::<EsResult<Vec<_>>>()?;
            Ok(Clause::Champ {
                chemin: champ.to_string(),
                champ: mf,
                predicat: Predicat::Parmi(vals),
            })
        }
        "range" => {
            let o = as_object(body, "range")?;
            let (champ, spec) = single_key(o, "range")?;
            let spec = as_object(spec, "range")?;
            let mf = champ_nested(ctx, champ, racine, "range")?;
            let borne = |cle: &str| -> EsResult<Option<Valeur>> {
                match spec.get(cle) {
                    None | Some(Value::Null) => Ok(None),
                    Some(v) => Ok(Some(valeur_de(ctx, champ, mf, v)?)),
                }
            };
            let bas = match (borne("gte")?, borne("gt")?) {
                (Some(v), None) => Bound::Included(v),
                (None, Some(v)) => Bound::Excluded(v),
                (None, None) => Bound::Unbounded,
                (Some(_), Some(_)) => {
                    return Err(EsError::illegal_argument(
                        "[range] : [gte] et [gt] sont mutuellement exclusifs",
                    ))
                }
            };
            let haut = match (borne("lte")?, borne("lt")?) {
                (Some(v), None) => Bound::Included(v),
                (None, Some(v)) => Bound::Excluded(v),
                (None, None) => Bound::Unbounded,
                (Some(_), Some(_)) => {
                    return Err(EsError::illegal_argument(
                        "[range] : [lte] et [lt] sont mutuellement exclusifs",
                    ))
                }
            };
            Ok(Clause::Champ {
                chemin: champ.to_string(),
                champ: mf,
                predicat: Predicat::Intervalle(bas, haut),
            })
        }
        "exists" => {
            let o = as_object(body, "exists")?;
            let champ = o
                .get("field")
                .and_then(Value::as_str)
                .ok_or_else(|| EsError::parsing("[exists] : cle [field] manquante"))?;
            let mf = champ_nested(ctx, champ, racine, "exists")?;
            Ok(Clause::Champ {
                chemin: champ.to_string(),
                champ: mf,
                predicat: Predicat::Existe,
            })
        }
        "prefix" => {
            let o = as_object(body, "prefix")?;
            let (champ, spec) = single_key(o, "prefix")?;
            let valeur = match spec {
                Value::Object(sub) => sub.get("value").cloned().unwrap_or(Value::Null),
                v => v.clone(),
            };
            let prefixe = valeur
                .as_str()
                .ok_or_else(|| EsError::parsing("[prefix] attend une chaine"))?;
            let mf = champ_nested(ctx, champ, racine, "prefix")?;
            Ok(Clause::Champ {
                chemin: champ.to_string(),
                champ: mf,
                predicat: Predicat::Prefixe(prefixe.to_string()),
            })
        }
        "bool" => {
            let o = as_object(body, "bool")?;
            expect_only(
                o,
                &[
                    "must",
                    "filter",
                    "should",
                    "must_not",
                    "minimum_should_match",
                    "boost",
                ],
                "nested.bool",
            )?;
            let liste = |cle: &str| -> EsResult<Vec<Clause>> {
                match o.get(cle) {
                    None => Ok(Vec::new()),
                    Some(Value::Array(a)) => a
                        .iter()
                        .map(|c| clause_nested(c, ctx, racine))
                        .collect::<EsResult<Vec<_>>>(),
                    Some(v) => Ok(vec![clause_nested(v, ctx, racine)?]),
                }
            };
            let mut et = liste("must")?;
            et.extend(liste("filter")?);
            let should = liste("should")?;
            let must_not = liste("must_not")?;
            if !should.is_empty() {
                // Meme regle que chez ES : `should` seul exige un match, `should`
                // accompagne d'un `must` est facultatif sauf minimum explicite.
                let defaut = usize::from(et.is_empty() && must_not.is_empty());
                let minimum = match o.get("minimum_should_match") {
                    None => defaut,
                    Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as usize,
                    Some(_) => {
                        return Err(EsError::unsupported(
                            "ferrite ne supporte que [minimum_should_match] entier dans un \
                             [nested]",
                        ))
                    }
                };
                if minimum > 0 {
                    et.push(Clause::Ou(should, minimum));
                }
            }
            for c in must_not {
                et.push(Clause::Non(Box::new(c)));
            }
            Ok(if et.is_empty() {
                Clause::Tous
            } else {
                Clause::Et(et)
            })
        }
        autre => Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [{autre}] dans une clause [nested] ; clauses verifiables \
             element par element : match_all, match_none, term, terms, match, range, exists, \
             prefix, bool"
        ))),
    }
}

/// Le champ vise par une clause interne : il doit vivre sous la racine
/// `nested`, sinon la correlation par element n'a pas de sens.
fn champ_nested(ctx: &QueryCtx, champ: &str, racine: &str, clause: &str) -> EsResult<MappedField> {
    if !mapping::est_sous_chemin(champ, racine) {
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "query_shard_exception",
            format!(
                "[nested] sur [{racine}] : le champ [{champ}] n'est pas sous ce chemin (clause \
                 [{clause}])"
            ),
        ));
    }
    let mf = ctx.field(champ, clause)?;
    if mf.ty.kind() == FieldKind::Text {
        return Err(EsError::unsupported(format!(
            "ferrite ne verifie pas un champ [text] element par element (champ [{champ}] dans un \
             [nested]) : les colonnes portent la valeur, pas les termes analyses. Interroge son \
             multi-field [keyword], ou sors la clause du [nested]"
        )));
    }
    if mf.elem.is_none() {
        return Err(EsError::internal(format!(
            "[{champ}] n'a pas de colonne d'element : index construit avant le support [nested] ?"
        )));
    }
    Ok(mf)
}

/// Convertit une valeur JSON au type du champ, puis en [`Valeur`] comparable.
fn valeur_de(ctx: &QueryCtx, champ: &str, mf: MappedField, v: &Value) -> EsResult<Valeur> {
    Ok(
        match mapping::coerce_avec(champ, mf.ty, v, ctx.fields.format_de(champ))? {
            TypedValue::Str(s) => Valeur::Str(s),
            TypedValue::I64(n) => Valeur::I64(n),
            TypedValue::F64(n) => Valeur::F64(n),
            TypedValue::Bool(b) => Valeur::Bool(b),
            TypedValue::Date(ms) => Valeur::I64(ms),
        },
    )
}

/// La requete privee de ses `must_not`, a tous les niveaux.
///
/// Le resultat est **plus large** que l'original : c'est exactement ce qu'un
/// pre-filtre doit etre.
fn sans_negations(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(k, _)| k.as_str() != "must_not")
                .map(|(k, sous)| (k.clone(), sans_negations(sous)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(sans_negations).collect()),
        autre => autre.clone(),
    }
}

// ---------------------------------------------------------------------------
// join : has_child / has_parent / parent_id
// ---------------------------------------------------------------------------

/// De quel cote de la relation la requete interne s'applique.
#[derive(Clone, Copy, PartialEq)]
enum Sens {
    /// `has_child` : la requete porte sur les enfants, on rend les parents.
    VersLeParent,
    /// `has_parent` : la requete porte sur les parents, on rend les enfants.
    VersLEnfant,
}

/// `has_child` / `has_parent`, en deux passes.
///
/// 1. la requete interne est executee, restreinte a la relation visee ;
/// 2. les identifiants qui en sortent (celui du parent pour un enfant, le sien
///    pour un parent) deviennent une recherche sur `_id` ou sur la colonne du
///    parent.
///
/// Exact, et borne par le nombre d'identifiants distincts. Elasticsearch ne
/// peut pas se le permettre — distribue, il lui faut des *global ordinals* —
/// mais mono-shard, parent et enfant sont forcement au meme endroit.
fn join_query(body: &Value, ctx: &QueryCtx, sens: Sens) -> EsResult<Box<dyn Query>> {
    let nom_clause = if sens == Sens::VersLeParent {
        "has_child"
    } else {
        "has_parent"
    };
    let obj = as_object(body, nom_clause)?;
    let cle_type = if sens == Sens::VersLeParent {
        "type"
    } else {
        "parent_type"
    };
    expect_only(
        obj,
        &[
            cle_type,
            "query",
            "score_mode",
            "boost",
            "inner_hits",
            "ignore_unmapped",
            "min_children",
            "max_children",
        ],
        nom_clause,
    )?;
    for refuse in [
        "inner_hits",
        "ignore_unmapped",
        "min_children",
        "max_children",
    ] {
        if obj.contains_key(refuse) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{refuse}] dans [{nom_clause}]"
            )));
        }
    }
    if let Some(mode) = obj.get("score_mode").and_then(Value::as_str) {
        if mode != "none" {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [score_mode: {mode}] dans [{nom_clause}] : la jointure \
                 rend un score constant (voir docs/compat.md)"
            )));
        }
    }

    let (join, f_nom, f_parent) = infos_join(ctx, nom_clause)?;
    let relation = obj
        .get(cle_type)
        .and_then(Value::as_str)
        .ok_or_else(|| EsError::parsing(format!("[{nom_clause}] : cle [{cle_type}] manquante")))?;
    let attendu_enfant = sens == Sens::VersLeParent;
    let est_enfant = join.parent_de(relation).is_some();
    if !join.connait(relation) || est_enfant != attendu_enfant {
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "query_shard_exception",
            format!(
                "[{nom_clause}] : [{relation}] n'est pas {} declare dans [{}] ; relations : {:?}",
                if attendu_enfant {
                    "un enfant"
                } else {
                    "un parent"
                },
                join.champ,
                join.noms()
            ),
        ));
    }
    let interne = obj
        .get("query")
        .ok_or_else(|| EsError::parsing(format!("[{nom_clause}] : cle [query] manquante")))?;

    // Passe 1 : les documents du cote interroge, restreints a la relation.
    let cible = BooleanQuery::new(vec![
        (Occur::Must, build_query(interne, ctx)?),
        (
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(f_nom, relation),
                IndexRecordOption::Basic,
            )),
        ),
    ]);

    // Passe 2 : de ces documents aux identifiants qui les relient.
    let (lu, vise) = if sens == Sens::VersLeParent {
        (f_parent, ctx.fields.id) // l'enfant porte l'id du parent
    } else {
        (ctx.fields.id, f_parent) // le parent porte le sien
    };
    let ids = collecte_termes(ctx, &cible, lu)?;
    if ids.is_empty() {
        return Ok(Box::new(EmptyQuery));
    }
    let termes: Vec<Term> = ids
        .iter()
        .map(|id| Term::from_field_text(vise, id))
        .collect();
    let inner: Box<dyn Query> = Box::new(ConstScoreQuery::new(
        Box::new(TermSetQuery::new(termes)),
        1.0,
    ));
    boost(inner, obj.get("boost"))
}

/// `{"parent_id": {"type": "ligne", "id": "1"}}` : les enfants d'un parent.
fn parent_id_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "parent_id")?;
    expect_only(
        obj,
        &["type", "id", "boost", "ignore_unmapped"],
        "parent_id",
    )?;
    if obj.contains_key("ignore_unmapped") {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [ignore_unmapped] dans [parent_id]",
        ));
    }
    let (join, f_nom, f_parent) = infos_join(ctx, "parent_id")?;
    let relation = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| EsError::parsing("[parent_id] : cle [type] manquante"))?;
    if join.parent_de(relation).is_none() {
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "query_shard_exception",
            format!(
                "[parent_id] : [{relation}] n'est pas un enfant declare dans [{}]",
                join.champ
            ),
        ));
    }
    let id = match obj.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err(EsError::parsing("[parent_id] : cle [id] manquante")),
    };
    let inner: Box<dyn Query> = Box::new(ConstScoreQuery::new(
        Box::new(BooleanQuery::new(vec![
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(f_nom, relation),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>,
            ),
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(f_parent, &id),
                    IndexRecordOption::Basic,
                )),
            ),
        ])),
        1.0,
    ));
    boost(inner, obj.get("boost"))
}

fn infos_join<'a>(
    ctx: &'a QueryCtx,
    clause: &str,
) -> EsResult<(
    &'a mapping::Join,
    tantivy::schema::Field,
    tantivy::schema::Field,
)> {
    match (
        ctx.fields.join.as_ref(),
        ctx.fields.join_name,
        ctx.fields.join_parent,
    ) {
        (Some(j), Some(n), Some(p)) => Ok((j, n, p)),
        _ => Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "query_shard_exception",
            format!("[{clause}] : cet index n'a pas de champ [join]"),
        )),
    }
}

/// Les valeurs distinctes d'une colonne `keyword`, pour les documents qui
/// correspondent a `q`.
fn collecte_termes(
    ctx: &QueryCtx,
    q: &dyn Query,
    champ: tantivy::schema::Field,
) -> EsResult<Vec<String>> {
    use std::collections::BTreeSet;
    let nom = ctx.searcher.schema().get_field_name(champ).to_string();
    let mut out = BTreeSet::new();
    let poids = q.weight(tantivy::query::EnableScoring::disabled_from_searcher(
        ctx.searcher,
    ))?;
    for reader in ctx.searcher.segment_readers() {
        let Ok(Some(col)) = reader.fast_fields().str(&nom) else {
            continue;
        };
        let mut docset = poids.scorer(reader, 1.0)?;
        let mut buf = String::new();
        let mut doc = docset.doc();
        while doc != tantivy::TERMINATED {
            for ord in col.term_ords(doc) {
                buf.clear();
                if col.ord_to_str(ord, &mut buf).unwrap_or(false) {
                    out.insert(buf.clone());
                }
            }
            doc = docset.advance();
        }
    }
    Ok(out.into_iter().collect())
}
