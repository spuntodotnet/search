//! Execution d'une recherche et mise en forme du resultat au format ES.

use std::cmp::Ordering;
use std::sync::Arc;

use serde_json::{json, Map, Value};
use tantivy::collector::{Collector, Count, SegmentCollector, TopDocs};
use tantivy::columnar::{Column, StrColumn};
use tantivy::query::Query;
use tantivy::{DocAddress, DocId, Score, SegmentOrdinal, SegmentReader};

use crate::engine::FerriteIndex;
use crate::error::{EsError, EsResult};
use crate::mapping::FieldKind;

// ---------------------------------------------------------------------------
// Tri
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum SortKey {
    Score,
    Doc,
    Field { name: String, kind: FieldKind },
}

#[derive(Debug, Clone)]
pub struct SortSpec {
    pub key: SortKey,
    pub asc: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum SortValue {
    Missing,
    Bool(bool),
    I64(i64),
    F64(f64),
    Str(String),
}

impl SortValue {
    fn cmp_present(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Bool(a), Self::Bool(b)) => a.cmp(b),
            (Self::I64(a), Self::I64(b)) => a.cmp(b),
            (Self::F64(a), Self::F64(b)) => a.total_cmp(b),
            (Self::Str(a), Self::Str(b)) => a.cmp(b),
            _ => Ordering::Equal,
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Missing => Value::Null,
            Self::Bool(b) => json!(b),
            Self::I64(n) => json!(n),
            Self::F64(n) => json!(n),
            Self::Str(s) => json!(s),
        }
    }
}

#[derive(Debug, Clone)]
struct Hit {
    keys: Vec<SortValue>,
    score: Score,
    seg: SegmentOrdinal,
    doc: DocId,
}

/// Collecteur qui ramasse tous les documents correspondants avec leurs cles de
/// tri, puis les ordonne en memoire.
///
/// Choix assume pour cette iteration : correct pour n'importe quelle
/// combinaison de cles de tri (y compris multi-cles et champs `keyword`, ou le
/// tri par ordinal de terme de tantivy serait faux entre segments), au prix
/// d'une occupation memoire proportionnelle au nombre de hits. Voir
/// `docs/compat.md`.
struct SortCollector {
    specs: Arc<Vec<SortSpec>>,
    needs_score: bool,
}

enum Accessor {
    Score,
    Doc,
    Str(Option<StrColumn>),
    I64(Column<i64>),
    F64(Column<f64>),
    Bool(Column<bool>),
    Date(Column<tantivy::DateTime>),
}

struct SortSegmentCollector {
    seg: SegmentOrdinal,
    accessors: Vec<Accessor>,
    hits: Vec<Hit>,
    buf: Vec<u8>,
}

impl Collector for SortCollector {
    type Fruit = Vec<Hit>;
    type Child = SortSegmentCollector;

    fn for_segment(
        &self,
        seg: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let ff = reader.fast_fields();
        let mut accessors = Vec::with_capacity(self.specs.len());
        for spec in self.specs.iter() {
            accessors.push(match &spec.key {
                SortKey::Score => Accessor::Score,
                SortKey::Doc => Accessor::Doc,
                SortKey::Field { name, kind } => match kind {
                    FieldKind::Keyword | FieldKind::Text => Accessor::Str(ff.str(name)?),
                    FieldKind::I64 => Accessor::I64(ff.i64(name)?),
                    FieldKind::F64 => Accessor::F64(ff.f64(name)?),
                    FieldKind::Bool => Accessor::Bool(ff.bool(name)?),
                    FieldKind::Date => Accessor::Date(ff.date(name)?),
                },
            });
        }
        Ok(SortSegmentCollector {
            seg,
            accessors,
            hits: Vec::new(),
            buf: Vec::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        self.needs_score
    }

    fn merge_fruits(&self, segment_fruits: Vec<Vec<Hit>>) -> tantivy::Result<Vec<Hit>> {
        Ok(segment_fruits.into_iter().flatten().collect())
    }
}

impl SegmentCollector for SortSegmentCollector {
    type Fruit = Vec<Hit>;

    fn collect(&mut self, doc: DocId, score: Score) {
        let mut keys = Vec::with_capacity(self.accessors.len());
        for acc in &self.accessors {
            keys.push(match acc {
                Accessor::Score => SortValue::F64(f64::from(score)),
                Accessor::Doc => SortValue::I64(i64::from(doc)),
                Accessor::Str(col) => match col {
                    Some(c) => match c.term_ords(doc).next() {
                        Some(ord) => {
                            self.buf.clear();
                            if c.ord_to_bytes(ord, &mut self.buf).unwrap_or(false) {
                                SortValue::Str(String::from_utf8_lossy(&self.buf).into_owned())
                            } else {
                                SortValue::Missing
                            }
                        }
                        None => SortValue::Missing,
                    },
                    None => SortValue::Missing,
                },
                Accessor::I64(c) => c.first(doc).map_or(SortValue::Missing, SortValue::I64),
                Accessor::F64(c) => c.first(doc).map_or(SortValue::Missing, SortValue::F64),
                Accessor::Bool(c) => c.first(doc).map_or(SortValue::Missing, SortValue::Bool),
                Accessor::Date(c) => c.first(doc).map_or(SortValue::Missing, |d| {
                    SortValue::I64(d.into_timestamp_millis())
                }),
            });
        }
        self.hits.push(Hit {
            keys,
            score,
            seg: self.seg,
            doc,
        });
    }

    fn harvest(self) -> Vec<Hit> {
        self.hits
    }
}

// ---------------------------------------------------------------------------
// Filtrage de _source
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum SourceFilter {
    All,
    None,
    Filter {
        includes: Vec<String>,
        excludes: Vec<String>,
    },
}

impl SourceFilter {
    pub fn apply(&self, value: Value) -> Option<Value> {
        match self {
            Self::All => Some(value),
            Self::None => None,
            Self::Filter { includes, excludes } => {
                Some(filter_value(&value, "", includes, excludes).unwrap_or(json!({})))
            }
        }
    }
}

fn filter_value(
    value: &Value,
    path: &str,
    includes: &[String],
    excludes: &[String],
) -> Option<Value> {
    if !path.is_empty() && excludes.iter().any(|p| glob_match(p, path)) {
        return None;
    }
    match value {
        Value::Object(o) => {
            let mut out = Map::new();
            for (k, v) in o {
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                if let Some(kept) = filter_value(v, &child, includes, excludes) {
                    out.insert(k.clone(), kept);
                }
            }
            // Un objet vide n'est conserve que s'il correspondait explicitement.
            if out.is_empty() && !path.is_empty() && !matches_include(includes, path) {
                None
            } else {
                Some(Value::Object(out))
            }
        }
        other => {
            if path.is_empty() || matches_include(includes, path) {
                Some(other.clone())
            } else {
                None
            }
        }
    }
}

fn matches_include(includes: &[String], path: &str) -> bool {
    if includes.is_empty() {
        return true;
    }
    includes.iter().any(|p| {
        // « titre » retient aussi « titre.sous_champ ».
        glob_match(p, path) || path.starts_with(&format!("{p}."))
    })
}

/// Comparaison de motif facon ES : `*` remplace n'importe quelle sous-chaine.
fn glob_match(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == text;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !text[pos..].starts_with(part) {
                return false;
            }
            pos += part.len();
        } else {
            match text[pos..].find(part) {
                Some(at) => pos += at + part.len(),
                None => return false,
            }
        }
    }
    if let Some(last) = parts.last() {
        if !last.is_empty() && !text.ends_with(last) {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

pub struct SearchRequest {
    pub query: Box<dyn Query>,
    pub from: usize,
    pub size: usize,
    pub sort: Vec<SortSpec>,
    pub source: SourceFilter,
}

pub struct SearchOutcome {
    pub total: usize,
    pub max_score: Option<f32>,
    pub hits: Vec<Value>,
}

pub fn execute(idx: &FerriteIndex, req: &SearchRequest) -> EsResult<SearchOutcome> {
    let searcher = idx.searcher();

    if req.sort.is_empty() {
        let count = searcher.search(&req.query, &Count)?;
        // `size: 0` ne demande aucun document : ES ne calcule alors pas de score
        // et rend `max_score: null`.
        if req.size == 0 {
            return Ok(SearchOutcome {
                total: count,
                max_score: None,
                hits: Vec::new(),
            });
        }
        let top = searcher.search(
            &req.query,
            &TopDocs::with_limit(req.from + req.size).order_by_score(),
        )?;
        // ES rapporte le meilleur score de la requete, pas de la page courante.
        let max_score = top.first().map(|(score, _)| *score);
        let mut hits = Vec::new();
        for (score, addr) in top.into_iter().skip(req.from) {
            hits.push(build_hit(
                idx,
                &searcher,
                addr,
                Some(score),
                None,
                &req.source,
            )?);
        }
        return Ok(SearchOutcome {
            total: count,
            max_score,
            hits,
        });
    }

    let specs = Arc::new(req.sort.clone());
    let needs_score = specs.iter().any(|s| matches!(s.key, SortKey::Score));
    let collector = SortCollector {
        specs: specs.clone(),
        needs_score,
    };
    let mut all = searcher.search(&req.query, &collector)?;
    all.sort_by(|a, b| compare(&specs, a, b));

    let total = all.len();
    let mut hits = Vec::new();
    for hit in all.into_iter().skip(req.from).take(req.size) {
        let addr = DocAddress::new(hit.seg, hit.doc);
        let sort_values: Vec<Value> = hit.keys.iter().map(SortValue::to_json).collect();
        let score = if needs_score { Some(hit.score) } else { None };
        hits.push(build_hit(
            idx,
            &searcher,
            addr,
            score,
            Some(sort_values),
            &req.source,
        )?);
    }
    Ok(SearchOutcome {
        total,
        max_score: None,
        hits,
    })
}

fn compare(specs: &[SortSpec], a: &Hit, b: &Hit) -> Ordering {
    for (i, spec) in specs.iter().enumerate() {
        let (av, bv) = (&a.keys[i], &b.keys[i]);
        let ord = match (av, bv) {
            (SortValue::Missing, SortValue::Missing) => Ordering::Equal,
            // `missing: _last` — le defaut d'ES, quel que soit le sens du tri.
            (SortValue::Missing, _) => Ordering::Greater,
            (_, SortValue::Missing) => Ordering::Less,
            _ => {
                let c = av.cmp_present(bv);
                if spec.asc {
                    c
                } else {
                    c.reverse()
                }
            }
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    // Departage stable : l'ordre d'indexation, comme ES.
    (a.seg, a.doc).cmp(&(b.seg, b.doc))
}

fn build_hit(
    idx: &FerriteIndex,
    searcher: &tantivy::Searcher,
    addr: DocAddress,
    score: Option<f32>,
    sort_values: Option<Vec<Value>>,
    filter: &SourceFilter,
) -> EsResult<Value> {
    let doc: tantivy::schema::TantivyDocument = searcher.doc(addr)?;
    let id = {
        use tantivy::schema::Value as _;
        doc.get_first(idx.fields.id)
            .and_then(|v| v.as_str().map(str::to_string))
            .ok_or_else(|| EsError::internal("hit sans _id"))?
    };
    let source = {
        use tantivy::schema::Value as _;
        let raw = doc
            .get_first(idx.fields.source)
            .and_then(|v| v.as_str().map(str::to_string))
            .ok_or_else(|| EsError::internal("hit sans _source"))?;
        serde_json::from_str::<Value>(&raw)
            .map_err(|e| EsError::internal(format!("_source illisible: {e}")))?
    };

    let mut hit = Map::new();
    hit.insert("_index".into(), json!(idx.name));
    hit.insert("_id".into(), json!(id));
    hit.insert(
        "_score".into(),
        score.map_or(Value::Null, |s| json!(round_score(s))),
    );
    if let Some(filtered) = filter.apply(source) {
        hit.insert("_source".into(), filtered);
    }
    if let Some(sv) = sort_values {
        hit.insert("sort".into(), Value::Array(sv));
    }
    Ok(Value::Object(hit))
}

/// ES serialise les scores en `float` : on tronque a la meme precision pour ne
/// pas exposer le bruit du `f32 -> f64`.
pub fn round_score(score: f32) -> f64 {
    let v = f64::from(score);
    (v * 1e7).round() / 1e7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob() {
        assert!(glob_match("titre", "titre"));
        assert!(!glob_match("titre", "titres"));
        assert!(glob_match("*", "n_importe"));
        assert!(glob_match("ti*", "titre"));
        assert!(glob_match("*tre", "titre"));
        assert!(!glob_match("ti*x", "titre"));
    }

    #[test]
    fn includes_simple() {
        let f = SourceFilter::Filter {
            includes: vec!["titre".into()],
            excludes: vec![],
        };
        let v = json!({"titre": "Bel-Ami", "auteur": "Maupassant"});
        assert_eq!(f.apply(v).unwrap(), json!({"titre": "Bel-Ami"}));
    }

    #[test]
    fn excludes_simple() {
        let f = SourceFilter::Filter {
            includes: vec![],
            excludes: vec!["auteur".into()],
        };
        let v = json!({"titre": "Bel-Ami", "auteur": "Maupassant"});
        assert_eq!(f.apply(v).unwrap(), json!({"titre": "Bel-Ami"}));
    }

    #[test]
    fn source_false() {
        assert!(SourceFilter::None.apply(json!({"a": 1})).is_none());
    }

    #[test]
    fn tri_valeurs_manquantes_en_dernier() {
        let specs = vec![SortSpec {
            key: SortKey::Field {
                name: "a".into(),
                kind: FieldKind::I64,
            },
            asc: false,
        }];
        let present = Hit {
            keys: vec![SortValue::I64(1)],
            score: 1.0,
            seg: 0,
            doc: 0,
        };
        let missing = Hit {
            keys: vec![SortValue::Missing],
            score: 1.0,
            seg: 0,
            doc: 1,
        };
        assert_eq!(compare(&specs, &present, &missing), Ordering::Less);
        assert_eq!(compare(&specs, &missing, &present), Ordering::Greater);
    }
}
