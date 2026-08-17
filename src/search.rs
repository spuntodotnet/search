//! Execution d'une recherche et mise en forme du resultat au format ES.

use std::cmp::Ordering;
use std::sync::Arc;

use serde_json::{json, Map, Value};
use tantivy::collector::{Collector, Count, SegmentCollector, TopDocs};
use tantivy::columnar::{Column, StrColumn};
use tantivy::query::Query;
use tantivy::{DocAddress, DocId, Score, Searcher, SegmentOrdinal, SegmentReader};

use crate::engine::Generation;
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
    /// Une valeur absente qui n'a **pas** de valeur de remplacement chez ES :
    /// le document part en dernier, quel que soit le sens du tri, et son
    /// tableau `sort` porte `rendu`.
    ///
    /// C'est le cas d'un `keyword` (`null`) et d'un flottant (`"Infinity"` /
    /// `"-Infinity"` — des **chaines**, JSON n'ayant pas l'infini). Un entier,
    /// un booleen et une date, eux, ne passent pas par la : ES leur substitue
    /// une vraie valeur (`i64::MAX` en croissant, `i64::MIN` en decroissant),
    /// donc ferrite en fait un `I64` — et un document qui porte *reellement*
    /// `9223372036854775807` se retrouve alors ex aequo avec un document qui
    /// n'a rien, exactement comme chez ES. Mesure par
    /// `tests/compat/fuzz_vs_es.py`.
    Missing(Value),
    I64(i64),
    F64(f64),
    Str(String),
}

impl SortValue {
    fn cmp_present(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::I64(a), Self::I64(b)) => a.cmp(b),
            (Self::F64(a), Self::F64(b)) => a.total_cmp(b),
            (Self::Str(a), Self::Str(b)) => a.cmp(b),
            _ => Ordering::Equal,
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Missing(rendu) => rendu.clone(),
            Self::I64(n) => json!(n),
            Self::F64(n) => json!(n),
            Self::Str(s) => json!(s),
        }
    }
}

/// Ce qu'ES met a la place d'une valeur de tri absente, selon le type de la
/// colonne et le sens du tri.
///
/// Sur un entier, un booleen ou une date, c'est une **vraie valeur** : le
/// document se compare comme s'il la portait. Sur un `keyword` ou un flottant,
/// c'est un rendu sans valeur : le document part en dernier.
fn sentinelle(kind: FieldKind, asc: bool) -> SortValue {
    match kind {
        FieldKind::Keyword | FieldKind::Text => SortValue::Missing(Value::Null),
        FieldKind::F64 => SortValue::Missing(json!(if asc { "Infinity" } else { "-Infinity" })),
        _ => SortValue::I64(if asc { i64::MAX } else { i64::MIN }),
    }
}

/// Le minimum (tri croissant) ou le maximum (tri decroissant) des valeurs d'un
/// champ multivalue.
///
/// C'est le `mode` par defaut d'ES, et il n'est pas anodin : prendre la
/// **premiere** valeur — ce que faisait ferrite — classe `[5, 1, 9]` sur 5 la
/// ou ES le classe sur 1 en croissant et sur 9 en decroissant. Un ordre faux,
/// sans le moindre message.
fn extremum<T: PartialOrd>(valeurs: impl Iterator<Item = T>, asc: bool) -> Option<T> {
    valeurs.fold(None, |acc, v| match acc {
        None => Some(v),
        Some(a) => Some(if (v < a) == asc { v } else { a }),
    })
}

/// Un document candidat, avant la fusion entre index.
///
/// `cible` est le rang de l'index dont il vient : c'est lui qui departage deux
/// documents que tout le reste laisse ex aequo, et il est stable parce que les
/// index arrivent tries par nom.
#[derive(Debug, Clone)]
struct Hit {
    keys: Vec<SortValue>,
    score: Score,
    cible: usize,
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
    cible: usize,
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

/// Un accesseur, plus ce qu'il faut pour lire un champ **multivalue** et pour
/// rendre une valeur absente comme ES la rend : le sens du tri, et la
/// sentinelle de son type.
struct Cle {
    acc: Accessor,
    asc: bool,
    absente: SortValue,
}

struct SortSegmentCollector {
    seg: SegmentOrdinal,
    cible: usize,
    accessors: Vec<Cle>,
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
            let (acc, absente) = match &spec.key {
                SortKey::Score => (Accessor::Score, SortValue::Missing(Value::Null)),
                SortKey::Doc => (Accessor::Doc, SortValue::Missing(Value::Null)),
                SortKey::Field { name, kind } => (
                    match kind {
                        FieldKind::Keyword | FieldKind::Text => Accessor::Str(ff.str(name)?),
                        FieldKind::I64 => Accessor::I64(ff.i64(name)?),
                        FieldKind::F64 => Accessor::F64(ff.f64(name)?),
                        FieldKind::Bool => Accessor::Bool(ff.bool(name)?),
                        FieldKind::Date => Accessor::Date(ff.date(name)?),
                    },
                    sentinelle(*kind, spec.asc),
                ),
            };
            accessors.push(Cle {
                acc,
                asc: spec.asc,
                absente,
            });
        }
        Ok(SortSegmentCollector {
            seg,
            cible: self.cible,
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
        for cle in &self.accessors {
            // Un champ multivalue se trie sur son minimum en croissant et sur
            // son maximum en decroissant : c'est le `mode` par defaut d'ES.
            let asc = cle.asc;
            let absente = || cle.absente.clone();
            keys.push(match &cle.acc {
                Accessor::Score => SortValue::F64(f64::from(score)),
                Accessor::Doc => SortValue::I64(i64::from(doc)),
                Accessor::Str(col) => match col {
                    // Les ordinaux d'un dictionnaire tantivy suivent l'ordre
                    // lexicographique : le plus petit ordinal est la plus
                    // petite chaine.
                    Some(c) => match extremum(c.term_ords(doc), asc) {
                        Some(ord) => {
                            self.buf.clear();
                            if c.ord_to_bytes(ord, &mut self.buf).unwrap_or(false) {
                                SortValue::Str(String::from_utf8_lossy(&self.buf).into_owned())
                            } else {
                                absente()
                            }
                        }
                        None => absente(),
                    },
                    None => absente(),
                },
                Accessor::I64(c) => {
                    extremum(c.values_for_doc(doc), asc).map_or_else(absente, SortValue::I64)
                }
                Accessor::F64(c) => {
                    extremum(c.values_for_doc(doc), asc).map_or_else(absente, SortValue::F64)
                }
                // ES rend un booleen de tri en entier (`1`, non `true`), et le
                // compare a la sentinelle des entiers : il est donc un entier
                // de bout en bout.
                Accessor::Bool(c) => extremum(c.values_for_doc(doc), asc)
                    .map_or_else(absente, |b| SortValue::I64(i64::from(b))),
                Accessor::Date(c) => extremum(
                    c.values_for_doc(doc).map(|d| d.into_timestamp_millis()),
                    asc,
                )
                .map_or_else(absente, SortValue::I64),
            });
        }
        self.hits.push(Hit {
            keys,
            score,
            cible: self.cible,
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
///
/// Sert au filtrage de `_source` comme a la resolution des noms d'index
/// ([`crate::selection`]) : c'est le meme joker des deux cotes.
pub fn glob_match(pattern: &str, text: &str) -> bool {
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

/// Un index a interroger : sa generation, et la requete **construite dans
/// cette generation**.
///
/// Une `Query` tantivy porte des `Field` qui n'ont de sens que dans le schema
/// ou ils ont ete obtenus : deux index, meme de mapping identique, exigent donc
/// deux requetes distinctes. C'est la raison pour laquelle une cible transporte
/// sa requete plutot que la recherche n'en construise une seule.
pub struct Cible {
    pub nom: String,
    pub gen: Arc<Generation>,
    /// Ce que le hit transporte au-dela du `_source`, resolu **sur ce
    /// mapping** : deux index ne rendent pas les memes champs pour le meme
    /// motif.
    pub plan: Arc<crate::fetch::Plan>,
    pub query: Box<dyn Query>,
    /// Les cles de tri resolues dans **cette** generation.
    pub sort: Vec<SortSpec>,
    /// Les agregations sont-elles collectees sur cet index ? (`false` quand il
    /// ignore un des champs agreges : il n'a alors aucune valeur a apporter.)
    pub agrege: bool,
    /// Les requetes des agregations [`filter`], construites **dans cette
    /// generation** comme la requete principale, et rangees par chemin
    /// d'agregation (voir [`crate::aggs`]).
    pub filtres: crate::aggs::Filtres,
}

pub struct SearchRequest {
    /// Les agregations demandees, deja validees.
    pub aggs: Option<Value>,
    pub from: usize,
    pub size: usize,
    /// Le sens de chaque cle de tri. Vide : tri par score.
    pub sort_asc: Vec<bool>,
    pub source: SourceFilter,
    /// `_id` est-il rendu ? Seul `stored_fields: "_none_"` le retire (mesure
    /// contre ES 8.15).
    pub avec_id: bool,
}

pub struct SearchOutcome {
    pub total: usize,
    pub max_score: Option<f32>,
    pub hits: Vec<Value>,
    pub aggregations: Option<Value>,
}

/// Un candidat **fige** : de quoi retrouver un document et le rendre plus tard,
/// sans garder la requete qui l'a trouve.
///
/// C'est ce qu'un contexte de `scroll` conserve, une fois l'ordre final calcule.
#[derive(Debug, Clone)]
pub struct HitFige {
    /// Le rang de l'index d'ou vient le document, dans [`Balayage::cibles`].
    pub cible: usize,
    pub seg: SegmentOrdinal,
    pub doc: DocId,
    pub score: Score,
    /// Les valeurs de tri, deja mises au format JSON (le tableau `sort` du hit).
    pub sort: Vec<Value>,
}

/// Un index tel qu'un contexte de `scroll` le retient : son nom, sa generation,
/// et **le `searcher` du moment ou le scroll a ete ouvert**.
///
/// Garder le `searcher` n'est pas un detail : une ecriture commitee pendant le
/// balayage fait recharger le reader, donc changer les numeros de segment. Les
/// adresses figees ne designeraient plus les memes documents. Un `Searcher`
/// tantivy est un instantane — le retenir, c'est exactement le « point in
/// time » que scroll promet chez Elasticsearch.
#[derive(Clone)]
pub struct CibleFigee {
    pub nom: String,
    pub gen: Arc<Generation>,
    pub searcher: Searcher,
    pub plan: Arc<crate::fetch::Plan>,
}

/// Tous les documents qui correspondent, dans l'ordre final.
pub struct Balayage {
    pub total: usize,
    pub max_score: Option<f32>,
    pub hits: Vec<HitFige>,
    pub aggregations: Option<Value>,
    pub cibles: Vec<CibleFigee>,
    /// Un tri explicite a-t-il ete demande ? (il remplace le score, comme chez
    /// ES : `sort` dans chaque hit, `max_score: null`)
    pub trie: bool,
    /// Le hit porte-t-il un `_score` ?
    pub avec_score: bool,
}

/// Balaye **tout** ce qui correspond, une fois pour toutes.
///
/// La recherche paginee ne remonte que `from + size` documents par index ;
/// `scroll`, lui, promet de rendre l'integralite du resultat dans un ordre
/// stable, y compris pendant que l'index change. On collecte donc l'ensemble
/// des correspondances **une seule fois**, on les ordonne, et les pages
/// suivantes ne sont plus qu'une tranche de ce tableau : chaque document est vu
/// une fois et une seule, et la Nieme page ne coute pas N recherches.
///
/// Le prix est la memoire : un candidat par document correspondant (une adresse
/// et ses cles de tri). C'est le meme choix que le collecteur de tri, et il est
/// note dans `docs/compat.md`.
pub fn balayer(cibles: Vec<Cible>, req: &SearchRequest) -> EsResult<Balayage> {
    let searchers: Vec<Searcher> = cibles.iter().map(|c| c.gen.searcher()).collect();

    let aggregations = match &req.aggs {
        Some(aggs) => Some(crate::aggs::run(
            &parts_d_agregation(&cibles, &searchers),
            aggs,
        )?),
        None => None,
    };

    let trie = !req.sort_asc.is_empty();
    let needs_score = !trie
        || cibles
            .iter()
            .any(|c| c.sort.iter().any(|s| matches!(s.key, SortKey::Score)));

    let mut total = 0usize;
    let mut max_score: Option<f32> = None;
    let mut candidats: Vec<Hit> = Vec::new();
    for (rang, (cible, searcher)) in cibles.iter().zip(&searchers).enumerate() {
        let collector = SortCollector {
            specs: Arc::new(cible.sort.clone()),
            needs_score,
            cible: rang,
        };
        let locaux = searcher.search(&cible.query, &collector)?;
        total += locaux.len();
        if !trie {
            for h in &locaux {
                max_score = Some(max_score.map_or(h.score, |m: f32| m.max(h.score)));
            }
        }
        candidats.extend(locaux);
    }
    candidats.sort_by(|a, b| compare(&req.sort_asc, a, b));

    let hits = candidats
        .into_iter()
        .map(|h| HitFige {
            cible: h.cible,
            seg: h.seg,
            doc: h.doc,
            score: h.score,
            sort: h.keys.iter().map(SortValue::to_json).collect(),
        })
        .collect();

    let cibles = cibles
        .into_iter()
        .zip(searchers)
        .map(|(c, s)| CibleFigee {
            nom: c.nom,
            gen: c.gen,
            searcher: s,
            plan: c.plan,
        })
        .collect();

    Ok(Balayage {
        total,
        max_score: if trie { None } else { max_score },
        hits,
        aggregations,
        cibles,
        trie,
        avec_score: !trie || needs_score,
    })
}

/// Rend une tranche de candidats deja ordonnes au format `hits.hits[]`.
///
/// Sert aux pages d'un `scroll` : l'ordre est deja decide, il ne reste qu'a
/// aller chercher les documents dans le `searcher` fige.
pub fn rendre_page(
    cibles: &[CibleFigee],
    hits: &[HitFige],
    rendu: &Rendu,
    trie: bool,
    avec_score: bool,
) -> EsResult<Vec<Value>> {
    let mut out = Vec::with_capacity(hits.len());
    for hit in hits {
        let cible = &cibles[hit.cible];
        out.push(build_hit(
            &cible.nom,
            &cible.gen,
            &cible.plan,
            &cible.searcher,
            DocAddress::new(hit.seg, hit.doc),
            avec_score.then_some(hit.score),
            trie.then(|| hit.sort.clone()),
            rendu,
        )?);
    }
    Ok(out)
}

/// Ce qu'un hit transporte au-dela des documents eux-memes.
///
/// `stored_fields` ne rend aucun champ chez ferrite (aucun n'est stocke, voir
/// [`crate::fetch`]) mais change bel et bien la reponse : il retire `_source`,
/// et `_none_` retire aussi `_id`.
#[derive(Debug, Clone)]
pub struct Rendu {
    pub source: SourceFilter,
    pub avec_id: bool,
}

/// Les index sur lesquels les agregations se collectent : ceux qui mappent tous
/// les champs agreges.
fn parts_d_agregation<'a>(
    cibles: &'a [Cible],
    searchers: &'a [Searcher],
) -> Vec<crate::aggs::Part<'a>> {
    cibles
        .iter()
        .zip(searchers)
        .filter(|(c, _)| c.agrege)
        .map(|(c, s)| crate::aggs::Part {
            gen: &c.gen,
            searcher: s,
            query: &*c.query,
            filtres: &c.filtres,
        })
        .collect()
}

/// Execute la recherche sur chaque index vise, puis fusionne.
///
/// C'est le schema `query_then_fetch` d'Elasticsearch, applique a des index
/// mono-shard : chaque index classe ses propres documents avec **ses** IDF, on
/// ne rassemble que les meilleurs de chacun, et le classement final se fait sur
/// ces candidats. Les scores ne sont donc pas comparables terme a terme entre
/// index — ils ne le sont pas davantage entre shards chez ES, qui fait
/// exactement ce calcul par defaut.
pub fn execute(cibles: &[Cible], req: &SearchRequest) -> EsResult<SearchOutcome> {
    let searchers: Vec<Searcher> = cibles.iter().map(|c| c.gen.searcher()).collect();

    // Les agregations portent sur tous les documents qui correspondent, pas sur
    // la page rendue : elles se calculent a part, et se fusionnent a part.
    let aggregations = match &req.aggs {
        Some(aggs) => Some(crate::aggs::run(
            &parts_d_agregation(cibles, &searchers),
            aggs,
        )?),
        None => None,
    };

    let trie = !req.sort_asc.is_empty();
    let needs_score = !trie
        || cibles
            .iter()
            .any(|c| c.sort.iter().any(|s| matches!(s.key, SortKey::Score)));
    // Combien de documents chaque index doit remonter pour que la page finale
    // soit exacte : les `from` premiers peuvent tous venir du meme index.
    let fenetre = req.from + req.size;

    let mut total = 0usize;
    let mut max_score: Option<f32> = None;
    let mut candidats: Vec<Hit> = Vec::new();

    for (rang, (cible, searcher)) in cibles.iter().zip(&searchers).enumerate() {
        if trie {
            let specs = Arc::new(cible.sort.clone());
            let collector = SortCollector {
                specs: specs.clone(),
                needs_score,
                cible: rang,
            };
            let mut locaux = searcher.search(&cible.query, &collector)?;
            total += locaux.len();
            locaux.sort_by(|a, b| compare(&req.sort_asc, a, b));
            locaux.truncate(fenetre);
            candidats.extend(locaux);
        } else {
            total += searcher.search(&cible.query, &Count)?;
            if fenetre == 0 {
                continue;
            }
            let top =
                searcher.search(&cible.query, &TopDocs::with_limit(fenetre).order_by_score())?;
            // ES rapporte le meilleur score de la requete, pas de la page.
            if let Some((score, _)) = top.first() {
                max_score = Some(max_score.map_or(*score, |m: f32| m.max(*score)));
            }
            candidats.extend(top.into_iter().map(|(score, addr)| Hit {
                keys: Vec::new(),
                score,
                cible: rang,
                seg: addr.segment_ord,
                doc: addr.doc_id,
            }));
        }
    }

    // `size: 0` ne demande aucun document : ES ne calcule alors pas de score et
    // rend `max_score: null`.
    if req.size == 0 {
        return Ok(SearchOutcome {
            total,
            max_score: None,
            hits: Vec::new(),
            aggregations,
        });
    }

    candidats.sort_by(|a, b| compare(&req.sort_asc, a, b));

    let rendu = Rendu {
        source: req.source.clone(),
        avec_id: req.avec_id,
    };
    let mut hits = Vec::new();
    for hit in candidats.into_iter().skip(req.from).take(req.size) {
        let cible = &cibles[hit.cible];
        let addr = DocAddress::new(hit.seg, hit.doc);
        let sort_values = trie.then(|| hit.keys.iter().map(SortValue::to_json).collect());
        let score = (!trie || needs_score).then_some(hit.score);
        hits.push(build_hit(
            &cible.nom,
            &cible.gen,
            &cible.plan,
            &searchers[hit.cible],
            addr,
            score,
            sort_values,
            &rendu,
        )?);
    }
    Ok(SearchOutcome {
        total,
        // Un tri explicite remplace le score : ES rend alors `max_score: null`.
        max_score: if trie { None } else { max_score },
        hits,
        aggregations,
    })
}

/// L'ordre entre deux candidats, quel que soit l'index d'ou ils viennent.
///
/// `sort_asc` vide signifie « par score decroissant » : c'est le classement par
/// defaut d'ES.
fn compare(sort_asc: &[bool], a: &Hit, b: &Hit) -> Ordering {
    if sort_asc.is_empty() {
        let ord = b.score.total_cmp(&a.score);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    for (i, asc) in sort_asc.iter().enumerate() {
        let (av, bv) = (&a.keys[i], &b.keys[i]);
        let ord = match (av, bv) {
            (SortValue::Missing(_), SortValue::Missing(_)) => Ordering::Equal,
            // `missing: _last` — le defaut d'ES, quel que soit le sens du tri.
            (SortValue::Missing(_), _) => Ordering::Greater,
            (_, SortValue::Missing(_)) => Ordering::Less,
            _ => {
                let c = av.cmp_present(bv);
                if *asc {
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
    // Departage stable : l'index vise, puis l'ordre d'indexation, comme ES
    // departage par shard puis par document.
    (a.cible, a.seg, a.doc).cmp(&(b.cible, b.seg, b.doc))
}

#[allow(clippy::too_many_arguments)]
fn build_hit(
    index_name: &str,
    gen: &Generation,
    plan: &crate::fetch::Plan,
    searcher: &tantivy::Searcher,
    addr: DocAddress,
    score: Option<f32>,
    sort_values: Option<Vec<Value>>,
    rendu: &Rendu,
) -> EsResult<Value> {
    let doc: tantivy::schema::TantivyDocument = searcher.doc(addr)?;
    let id = {
        use tantivy::schema::Value as _;
        doc.get_first(gen.fields.id)
            .and_then(|v| v.as_str().map(str::to_string))
            .ok_or_else(|| EsError::internal("hit sans _id"))?
    };
    let source = {
        use tantivy::schema::Value as _;
        let raw = doc
            .get_first(gen.fields.source)
            .and_then(|v| v.as_str().map(str::to_string))
            .ok_or_else(|| EsError::internal("hit sans _source"))?;
        serde_json::from_str::<Value>(&raw)
            .map_err(|e| EsError::internal(format!("_source illisible: {e}")))?
    };
    let version = {
        use tantivy::schema::Value as _;
        doc.get_first(gen.fields.version).and_then(|v| v.as_u64())
    };

    let mut hit = Map::new();
    hit.insert("_index".into(), json!(index_name));
    if rendu.avec_id {
        hit.insert("_id".into(), json!(id));
    }
    hit.insert(
        "_score".into(),
        score.map_or(Value::Null, |s| json!(round_score(s))),
    );
    // `fields` lit le `_source` **complet**, pas celui que `_source` a filtre :
    // les deux se demandent ensemble et ne repondent pas a la meme question.
    let blocs = crate::fetch::rendre(
        plan,
        gen,
        searcher,
        addr,
        &crate::fetch::Document {
            source: &source,
            index: index_name,
            id: &id,
            version,
        },
    )?;
    if let Some(filtered) = rendu.source.apply(source) {
        hit.insert("_source".into(), filtered);
    }
    if let Some(b) = blocs.fields {
        hit.insert("fields".into(), b);
    }
    if let Some(b) = blocs.ignores {
        hit.insert("ignored_field_values".into(), b);
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
        let present = Hit {
            keys: vec![SortValue::I64(1)],
            score: 1.0,
            cible: 0,
            seg: 0,
            doc: 0,
        };
        let missing = Hit {
            keys: vec![SortValue::Missing(json!(i64::MIN))],
            score: 1.0,
            cible: 0,
            seg: 0,
            doc: 1,
        };
        assert_eq!(compare(&[false], &present, &missing), Ordering::Less);
        assert_eq!(compare(&[false], &missing, &present), Ordering::Greater);
    }

    /// Ce qu'ES rend a la place d'une valeur de tri absente : pas `null`, sauf
    /// sur un `keyword`. Mesure contre un ES 8.15 (`fuzz_vs_es.py`).
    #[test]
    fn sentinelles_des_valeurs_absentes() {
        assert_eq!(sentinelle(FieldKind::I64, true).to_json(), json!(i64::MAX));
        assert_eq!(sentinelle(FieldKind::I64, false).to_json(), json!(i64::MIN));
        assert_eq!(sentinelle(FieldKind::Bool, true).to_json(), json!(i64::MAX));
        assert_eq!(
            sentinelle(FieldKind::Date, false).to_json(),
            json!(i64::MIN)
        );
        assert_eq!(
            sentinelle(FieldKind::F64, true).to_json(),
            json!("Infinity")
        );
        assert_eq!(
            sentinelle(FieldKind::F64, false).to_json(),
            json!("-Infinity")
        );
        assert_eq!(sentinelle(FieldKind::Keyword, true).to_json(), Value::Null);

        // Sur un entier, la sentinelle est une **vraie valeur** : un document
        // qui porte i64::MAX est ex aequo avec un document qui n'a rien, et
        // c'est la cle suivante qui les departage — comme chez ES.
        let cle_suivante = |v: SortValue, id: u32| Hit {
            keys: vec![v, SortValue::Str(format!("d{id}"))],
            score: 1.0,
            cible: 0,
            seg: 0,
            doc: id,
        };
        let vide = cle_suivante(sentinelle(FieldKind::I64, true), 4);
        let plein = cle_suivante(SortValue::I64(i64::MAX), 8);
        assert_eq!(compare(&[true, true], &vide, &plein), Ordering::Less);
    }

    /// Le `mode` par defaut d'ES sur un champ multivalue : minimum en
    /// croissant, maximum en decroissant.
    #[test]
    fn extremum_selon_le_sens_du_tri() {
        assert_eq!(extremum([5i64, 1, 9].into_iter(), true), Some(1));
        assert_eq!(extremum([5i64, 1, 9].into_iter(), false), Some(9));
        assert_eq!(extremum(std::iter::empty::<i64>(), true), None);
    }

    /// Sans cle de tri, deux documents de meme score sont departages par
    /// l'index d'ou ils viennent — et les index arrivent tries par nom, donc
    /// l'ordre rendu ne depend pas de celui ou la recherche les a parcourus.
    #[test]
    fn ex_aequo_departages_par_index() {
        let a = Hit {
            keys: vec![],
            score: 2.0,
            cible: 1,
            seg: 0,
            doc: 0,
        };
        let b = Hit {
            keys: vec![],
            score: 2.0,
            cible: 0,
            seg: 0,
            doc: 9,
        };
        assert_eq!(compare(&[], &b, &a), Ordering::Less);
        // Le score reste prioritaire sur l'index.
        let meilleur = Hit {
            score: 3.0,
            ..a.clone()
        };
        assert_eq!(compare(&[], &meilleur, &b), Ordering::Less);
    }
}
