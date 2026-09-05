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
use crate::mapping::{FieldKind, FieldType};

// ---------------------------------------------------------------------------
// Tri
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum SortKey {
    Score,
    Doc,
    Field(Box<SortField>),
}

/// Une cle de tri sur un champ, **resolue dans le mapping d'un index**.
///
/// Deux index vises par la meme recherche n'en produisent donc pas la meme :
/// l'un peut mapper le champ, l'autre ne le connaitre que par `unmapped_type`.
#[derive(Debug, Clone)]
pub struct SortField {
    pub name: String,
    /// Le type retenu pour ce tri : celui du mapping, ou celui d'`unmapped_type`
    /// quand cet index-la ne connait pas le champ.
    pub ty: FieldType,
    /// Cet index mappe-t-il le champ ? Sinon, il n'a pas de colonne a lire et
    /// **tous** ses documents portent la valeur de remplacement.
    pub mappe: bool,
    /// Le `mode` demande. `None` est le defaut d'ES : le minimum en ordre
    /// croissant, le maximum en decroissant.
    pub mode: Option<SortMode>,
    /// Ce qu'un document sans valeur porte, deja typee selon le champ (voir
    /// [`SortValue`]).
    pub absente: SortValue,
}

#[derive(Debug, Clone)]
pub struct SortSpec {
    pub key: SortKey,
    pub asc: bool,
}

/// Quelle valeur d'un champ multivalue sert au tri.
///
/// Le defaut d'ES n'est pas un mode mais une **regle** : le minimum en ordre
/// croissant, le maximum en decroissant (mesure : `[5, 1, 9, 3]` se classe sur 1
/// en `asc` et sur 9 en `desc`). Un `mode` explicite la remplace des deux cotes
/// — `desc` avec `mode: min` classe bien sur le minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    Min,
    Max,
    Sum,
    Avg,
    Median,
}

impl SortMode {
    /// `min` / `max` / `sum` / `avg` / `median`, insensible a la casse (mesure :
    /// ES accepte `MIN`).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "min" => Self::Min,
            "max" => Self::Max,
            "sum" => Self::Sum,
            "avg" => Self::Avg,
            "median" => Self::Median,
            _ => return None,
        })
    }

    /// `sum`, `avg` et `median` n'ont de sens que sur des nombres : ES refuse
    /// les trois sur un `keyword`, avec cette phrase-la.
    pub fn numerique_seulement(self) -> bool {
        matches!(self, Self::Sum | Self::Avg | Self::Median)
    }
}

/// La **famille** de tri d'un champ, telle qu'ES la nomme quand deux shards
/// n'ont pas la meme.
///
/// Ce n'est pas le type du champ : `byte`, `short`, `integer`, `long`, `date` et
/// `boolean` trient tous en `LONG`. Mais `float` et `double` ne trient **pas**
/// dans la meme famille, ce qu'aucune documentation ne dit et qui se mesure :
/// deux index dont l'un mappe `f` en `float` et l'autre en `double` font echouer
/// la recherche entiere.
pub fn famille_de_tri(ty: FieldType) -> &'static str {
    match ty {
        FieldType::Keyword | FieldType::Text => "STRING",
        FieldType::Float => "FLOAT",
        FieldType::Double => "DOUBLE",
        _ => "LONG",
    }
}

/// Une cle de tri, telle qu'elle se compare et telle qu'elle se rend.
///
/// Une seule variante n'est pas une valeur : [`SortValue::Absente`], qui n'existe
/// que sur un `keyword`. ES n'y a pas de sentinelle de chaine — il pose un
/// marqueur qui envoie le document en tete ou en queue **quel que soit le sens
/// du tri**, et rend `null` dans le tableau `sort`.
///
/// Partout ailleurs, une valeur absente est une **vraie valeur** : `i64::MAX` /
/// `i64::MIN` sur un entier, une date ou un booleen, `+inf` / `-inf` sur un
/// flottant. Un document qui porte reellement `9223372036854775807` est donc ex
/// aequo avec un document qui n'a rien, exactement comme chez ES — et une somme
/// de flottants qui deborde rend `"Infinity"` comme une valeur manquante.
#[derive(Debug, Clone, PartialEq)]
pub enum SortValue {
    Absente {
        en_tete: bool,
    },
    I64(i64),
    /// Un flottant, y compris non fini : JSON n'ayant ni l'infini ni `NaN`, ES
    /// les rend en **chaine** (`"Infinity"`, `"-Infinity"`, `"NaN"`).
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
            Self::Absente { .. } => Value::Null,
            Self::I64(n) => json!(n),
            Self::F64(n) if n.is_finite() => json!(n),
            Self::F64(n) if n.is_nan() => json!("NaN"),
            Self::F64(n) => json!(if *n > 0.0 { "Infinity" } else { "-Infinity" }),
            Self::Str(s) => json!(s),
        }
    }
}

/// Ce qu'ES met a la place d'une valeur de tri absente : le bout du tri ou le
/// document doit partir.
///
/// `_last` est le defaut, et il ne depend pas du sens : en `asc` c'est la borne
/// haute (`i64::MAX`, `+inf`), en `desc` la borne basse. `_first` est l'inverse.
pub fn sentinelle(ty: FieldType, asc: bool, en_tete: bool) -> SortValue {
    // La sentinelle est **haute** quand elle doit partir en queue d'un tri
    // croissant ou en tete d'un tri decroissant.
    let haute = asc != en_tete;
    match ty {
        FieldType::Keyword | FieldType::Text => SortValue::Absente { en_tete },
        FieldType::Float | FieldType::Double => SortValue::F64(if haute {
            f64::INFINITY
        } else {
            f64::NEG_INFINITY
        }),
        _ => SortValue::I64(if haute { i64::MAX } else { i64::MIN }),
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

/// `Math.round(double)` de Java, qui n'est ni `f64::round` ni `floor`.
///
/// Java arrondit **vers le haut** a la demie (`floor(x + 0.5)`), la ou Rust
/// s'ecarte de zero : `-2,5` vaut `-2` chez Java et `-3` chez Rust. C'est
/// exactement ce que fait `MultiValueMode.AVG` sur une colonne d'entiers, donc
/// ce qui decide de l'ordre.
///
/// Le calcul ne passe **pas** par `x + 0.5` : cette addition arrondit, et fait
/// franchir un entier a une valeur qui ne l'atteignait pas
/// (`0.49999999999999994` y donnerait 1 au lieu de 0). C'est le defaut que le
/// JDK corrige depuis sa 7. La partie fractionnaire, elle, se soustrait sans
/// perte — elle tient dans la mantisse.
fn arrondi_java(x: f64) -> i64 {
    let plancher = x.floor();
    let arrondi = if x - plancher >= 0.5 {
        plancher + 1.0
    } else {
        plancher
    };
    // `as` sature aux bornes et rend 0 sur `NaN`, comme `Math.round`.
    arrondi as i64
}

/// La somme d'une colonne d'entiers, **qui deborde en silence**.
///
/// C'est ce que fait ES, parce que c'est ce que fait `long` en Java :
/// `[1, i64::MAX]` se classe sur `i64::MIN`. Saturer serait plus raisonnable et
/// rendrait un autre ordre que le sien.
fn somme_i64(vals: &[i64]) -> i64 {
    vals.iter().fold(0i64, |a, v| a.wrapping_add(*v))
}

/// La valeur qu'un document multivalue porte, selon `mode`.
///
/// `vals` arrive dans l'ordre de la colonne, qui est **triee croissante**
/// (voir [`crate::engine`] : `pose`) — comme les `SortedNumericDocValues` de
/// Lucene, et c'est ce que `median` exige. `min` et `max` n'en dependent pas :
/// ils balaient.
fn choisir_i64(vals: &[i64], mode: SortMode) -> i64 {
    let n = vals.len();
    match mode {
        SortMode::Min => vals.iter().copied().min().unwrap_or(0),
        SortMode::Max => vals.iter().copied().max().unwrap_or(0),
        SortMode::Sum => somme_i64(vals),
        // `count > 1 ? Math.round(total / count) : total` : a une seule valeur,
        // ES ne passe pas par le flottant, donc ne perd rien au-dela de 2^53.
        SortMode::Avg if n == 1 => vals[0],
        SortMode::Avg => arrondi_java(somme_i64(vals) as f64 / n as f64),
        SortMode::Median => {
            let i = (n - 1) / 2;
            if n % 2 == 0 {
                arrondi_java((vals[i] as f64 + vals[i + 1] as f64) / 2.0)
            } else {
                vals[i]
            }
        }
    }
}

/// La meme chose sur une colonne de flottants — sans arrondi : `avg` y rend
/// `8,5 / 3 = 2,8333333333333335`, mesure comprise.
fn choisir_f64(vals: &[f64], mode: SortMode) -> f64 {
    let n = vals.len();
    match mode {
        SortMode::Min => extremum(vals.iter().copied(), true).unwrap_or(0.0),
        SortMode::Max => extremum(vals.iter().copied(), false).unwrap_or(0.0),
        SortMode::Sum => vals.iter().sum(),
        SortMode::Avg => vals.iter().sum::<f64>() / n as f64,
        SortMode::Median => {
            let i = (n - 1) / 2;
            if n % 2 == 0 {
                (vals[i] + vals[i + 1]) / 2.0
            } else {
                vals[i]
            }
        }
    }
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

/// Le `Count` de tantivy, mais qui **demande les scores**.
///
/// Les deux comptent la meme chose partout ailleurs : un score ne change pas
/// l'ensemble des documents qui correspondent. Sauf sous un `min_score`, qui en
/// fait un seuil — et le regime de score decide alors du total (voir
/// [`crate::fonction_score`]).
struct CompteAvecScore;

struct CompteAvecScoreSegment(usize);

impl Collector for CompteAvecScore {
    type Fruit = usize;
    type Child = CompteAvecScoreSegment;

    fn for_segment(&self, _: SegmentOrdinal, _: &SegmentReader) -> tantivy::Result<Self::Child> {
        Ok(CompteAvecScoreSegment(0))
    }

    fn requires_scoring(&self) -> bool {
        true
    }

    fn merge_fruits(&self, fruits: Vec<usize>) -> tantivy::Result<usize> {
        Ok(fruits.into_iter().sum())
    }
}

impl SegmentCollector for CompteAvecScoreSegment {
    type Fruit = usize;

    fn collect(&mut self, _doc: DocId, _score: Score) {
        self.0 += 1;
    }

    fn harvest(self) -> usize {
        self.0
    }
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
    /// Le champ n'est pas mappe par cet index : c'est l'echappatoire
    /// `unmapped_type`, ou chaque document porte la valeur de remplacement.
    Aucune,
    Str(Option<StrColumn>),
    I64(Column<i64>),
    F64(Column<f64>),
    Bool(Column<bool>),
    Date(Column<tantivy::DateTime>),
}

/// Un accesseur, plus ce qu'il faut pour lire un champ **multivalue** et pour
/// rendre une valeur absente comme ES la rend : le sens du tri, le `mode`, et la
/// sentinelle de son type.
struct Cle {
    acc: Accessor,
    asc: bool,
    mode: Option<SortMode>,
    absente: SortValue,
}

struct SortSegmentCollector {
    seg: SegmentOrdinal,
    cible: usize,
    accessors: Vec<Cle>,
    hits: Vec<Hit>,
    buf: Vec<u8>,
    /// Les valeurs d'un document, relues telles que la colonne les porte quand
    /// un `mode` en demande plus que le minimum ou le maximum. Reutilise d'un
    /// document a l'autre : `sum`, `avg` et `median` n'allouent pas par hit.
    nums: Vec<i64>,
    reels: Vec<f64>,
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
            let (acc, mode, absente) = match &spec.key {
                SortKey::Score => (Accessor::Score, None, SortValue::Absente { en_tete: false }),
                SortKey::Doc => (Accessor::Doc, None, SortValue::Absente { en_tete: false }),
                // Un `unmapped_type` sur un index qui ignore le champ : il n'y a
                // pas de colonne a ouvrir, tous ses documents sont « sans
                // valeur ».
                SortKey::Field(f) if !f.mappe => (Accessor::Aucune, f.mode, f.absente.clone()),
                SortKey::Field(f) => (
                    match f.ty.kind() {
                        FieldKind::Keyword | FieldKind::Text => Accessor::Str(ff.str(&f.name)?),
                        FieldKind::I64 => Accessor::I64(ff.i64(&f.name)?),
                        FieldKind::F64 => Accessor::F64(ff.f64(&f.name)?),
                        FieldKind::Bool => Accessor::Bool(ff.bool(&f.name)?),
                        FieldKind::Date => Accessor::Date(ff.date(&f.name)?),
                    },
                    f.mode,
                    f.absente.clone(),
                ),
            };
            accessors.push(Cle {
                acc,
                asc: spec.asc,
                mode,
                absente,
            });
        }
        Ok(SortSegmentCollector {
            seg,
            cible: self.cible,
            accessors,
            hits: Vec::new(),
            buf: Vec::new(),
            nums: Vec::new(),
            reels: Vec::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        self.needs_score
    }

    fn merge_fruits(&self, segment_fruits: Vec<Vec<Hit>>) -> tantivy::Result<Vec<Hit>> {
        Ok(segment_fruits.into_iter().flatten().collect())
    }
}

/// La cle de tri d'une colonne d'entiers — la meme fonction pour un `long`, une
/// date et un booleen, qui se comparent tous les trois comme des entiers.
///
/// Sans `mode`, elle ne touche pas au tampon : le cas courant reste un simple
/// balayage. Avec, elle le remplit une fois par document et le reutilise —
/// `sum`, `avg` et `median` n'allouent donc pas par hit.
fn cle_entiere<I: Iterator<Item = i64>>(
    valeurs: I,
    mode: Option<SortMode>,
    asc: bool,
    tampon: &mut Vec<i64>,
) -> Option<SortValue> {
    match mode {
        None => extremum(valeurs, asc).map(SortValue::I64),
        Some(m) => {
            tampon.clear();
            tampon.extend(valeurs);
            (!tampon.is_empty()).then(|| SortValue::I64(choisir_i64(tampon, m)))
        }
    }
}

impl SegmentCollector for SortSegmentCollector {
    type Fruit = Vec<Hit>;

    fn collect(&mut self, doc: DocId, score: Score) {
        let mut keys = Vec::with_capacity(self.accessors.len());
        for cle in &self.accessors {
            // Sans `mode`, un champ multivalue se trie sur son minimum en
            // croissant et sur son maximum en decroissant : c'est le defaut
            // d'ES, et il depend donc du sens du tri. Avec, c'est le `mode` qui
            // decide, et le sens n'entre plus en compte.
            let asc = cle.asc;
            let absente = || cle.absente.clone();
            let nums = &mut self.nums;
            let reels = &mut self.reels;
            keys.push(match &cle.acc {
                Accessor::Score => SortValue::F64(f64::from(score)),
                Accessor::Doc => SortValue::I64(i64::from(doc)),
                Accessor::Aucune => absente(),
                Accessor::Str(col) => match col {
                    // Les ordinaux d'un dictionnaire tantivy suivent l'ordre
                    // lexicographique : le plus petit ordinal est la plus
                    // petite chaine. `sum`, `avg` et `median` ne viennent pas
                    // jusqu'ici — ES les refuse sur un `keyword`.
                    Some(c) => match extremum(
                        c.term_ords(doc),
                        match cle.mode {
                            Some(SortMode::Max) => false,
                            Some(SortMode::Min) => true,
                            _ => asc,
                        },
                    ) {
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
                    cle_entiere(c.values_for_doc(doc), cle.mode, asc, nums).unwrap_or_else(absente)
                }
                Accessor::F64(c) => match cle.mode {
                    None => {
                        extremum(c.values_for_doc(doc), asc).map_or_else(absente, SortValue::F64)
                    }
                    Some(m) => {
                        reels.clear();
                        reels.extend(c.values_for_doc(doc));
                        if reels.is_empty() {
                            absente()
                        } else {
                            SortValue::F64(choisir_f64(reels, m))
                        }
                    }
                },
                // ES rend un booleen de tri en entier (`1`, non `true`), et le
                // compare a la sentinelle des entiers : il est donc un entier
                // de bout en bout — `mode: sum` sur `[true, false]` vaut 1.
                Accessor::Bool(c) => {
                    cle_entiere(c.values_for_doc(doc).map(i64::from), cle.mode, asc, nums)
                        .unwrap_or_else(absente)
                }
                Accessor::Date(c) => cle_entiere(
                    c.values_for_doc(doc).map(|d| d.into_timestamp_millis()),
                    cle.mode,
                    asc,
                    nums,
                )
                .unwrap_or_else(absente),
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

#[derive(Debug, Clone, Default)]
pub enum SourceFilter {
    #[default]
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
    /// Ce que le hit surligne, resolu **sur ce mapping et sur cette requete**.
    pub hl: Arc<crate::highlight::Plan>,
    pub query: Box<dyn Query>,
    /// Les cles de tri resolues dans **cette** generation.
    pub sort: Vec<SortSpec>,
    /// Les agregations sont-elles collectees sur cet index ? (`false` quand il
    /// ignore un des champs agreges : il n'a alors aucune valeur a apporter.)
    pub agrege: bool,
    /// Ce que les agregations laissent dans **cette** generation : les requetes
    /// des agregations [`filter`] et les plans des `top_hits`, construits ici
    /// comme la requete principale et ranges par chemin d'agregation (voir
    /// [`crate::aggs`]).
    pub prep: crate::aggs::Prepare,
    /// Ce que l'execution a rencontre et qu'ES traite en erreur (voir
    /// [`crate::fonction_score::Incidents`]). Relu **apres** chaque recherche :
    /// un `Scorer` ne peut pas echouer, mais son verdict ne doit pas se perdre.
    pub incidents: Arc<crate::fonction_score::Incidents>,
    /// Les clauses nommees par un `_name`, traduites **dans cette generation**
    /// comme la requete principale.
    ///
    /// Chacune est rejouee seule contre chaque document rendu : c'est ce que
    /// fait ES, et c'est ce qui explique qu'un `should` place sous un `must_not`
    /// ne se nomme pas dans un hit qui, lui, correspond (voir
    /// [`crate::dsl::extraire_noms`]).
    pub nommees: Arc<Vec<(String, Box<dyn Query>)>>,
}

pub struct SearchRequest {
    /// Les agregations demandees, deja validees.
    pub aggs: Option<Value>,
    pub from: usize,
    pub size: usize,
    /// Le sens de chaque cle de tri. Vide : tri par score.
    pub sort_asc: Vec<bool>,
    /// Ce que chaque hit transporte : `_source`, `_id`, `_explanation`,
    /// `matched_queries`.
    pub rendu: Rendu,
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
    pub hl: Arc<crate::highlight::Plan>,
    /// La requete elle-meme : `explain` et `matched_queries` la rejouent
    /// document par document, bien apres que le classement soit decide.
    pub query: Arc<dyn Query>,
    pub nommees: Arc<Vec<(String, Box<dyn Query>)>>,
    /// Rejouer une clause nommee **note** le document, donc peut declencher un
    /// garde-fou de `function_score` — et une page de `scroll` est rendue bien
    /// apres la recherche qui l'a ouverte (voir [`build_hit`]).
    pub incidents: Arc<crate::fonction_score::Incidents>,
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
    let mut apporte = vec![false; cibles.len()];
    for (rang, (cible, searcher)) in cibles.iter().zip(&searchers).enumerate() {
        let collector = SortCollector {
            specs: Arc::new(cible.sort.clone()),
            needs_score,
            cible: rang,
        };
        let locaux = searcher.search(&cible.query, &collector)?;
        if let Some(e) = cible.incidents.erreur() {
            return Err(e);
        }
        total += locaux.len();
        apporte[rang] = !locaux.is_empty();
        if !trie {
            for h in &locaux {
                max_score = Some(max_score.map_or(h.score, |m: f32| m.max(h.score)));
            }
        }
        candidats.extend(locaux);
    }
    if trie {
        if let Some(e) = conflit_de_familles(&cibles, &apporte) {
            return Err(e);
        }
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
            hl: c.hl,
            query: Arc::from(c.query),
            nommees: c.nommees,
            incidents: c.incidents,
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
            &cible.hl,
            &cible.searcher,
            DocAddress::new(hit.seg, hit.doc),
            avec_score.then_some(hit.score),
            trie.then(|| hit.sort.clone()),
            rendu,
            &*cible.query,
            &cible.nommees,
            &cible.incidents,
        )?);
    }
    Ok(out)
}

/// Ce qu'un hit transporte au-dela des documents eux-memes.
///
/// `stored_fields` ne rend aucun champ chez ferrite (aucun n'est stocke, voir
/// [`crate::fetch`]) mais change bel et bien la reponse : il retire `_source`,
/// et `_none_` retire aussi `_id`.
#[derive(Debug, Clone, Default)]
pub struct Rendu {
    pub source: SourceFilter,
    pub avec_id: bool,
    /// `explain: true` : le hit porte alors `_explanation`, et — comme chez ES —
    /// `_shard` et `_node`, qu'il ne porte pas autrement.
    pub explique: bool,
    /// `include_named_queries_score` : `matched_queries` devient un objet
    /// `{nom: score}` au lieu d'une liste de noms.
    pub noms_avec_score: bool,
    /// L'identifiant du noeud, pour `_node`. Un seul noeud ici.
    pub noeud: String,
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
            nom: &c.nom,
            gen: &c.gen,
            searcher: s,
            query: &*c.query,
            prep: &c.prep,
            nommees: &c.nommees,
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
    // Quels index ont **apporte un document** : c'est ce qui decide si un
    // conflit de familles de tri se leve (voir [`conflit_de_familles`]).
    let mut apporte = vec![false; cibles.len()];

    for (rang, (cible, searcher)) in cibles.iter().zip(&searchers).enumerate() {
        if trie {
            let specs = Arc::new(cible.sort.clone());
            let collector = SortCollector {
                specs: specs.clone(),
                needs_score,
                cible: rang,
            };
            let mut locaux = searcher.search(&cible.query, &collector)?;
            if let Some(e) = cible.incidents.erreur() {
                return Err(e);
            }
            total += locaux.len();
            locaux.sort_by(|a, b| compare(&req.sort_asc, a, b));
            locaux.truncate(fenetre);
            apporte[rang] = !locaux.is_empty();
            candidats.extend(locaux);
        } else {
            // Le total se compte avec le meme regime de score que la page :
            // `min_score` fait de la valeur du score un **seuil**, et le
            // `boost` d'une clause ne s'applique que si quelqu'un lit le score
            // (voir [`crate::fonction_score`]). Compter sans score la ou la
            // page en demande rendrait un total plus petit que le nombre de
            // hits — en 200. A `size: 0` personne ne lit de score, et c'est
            // aussi ce que fait ES.
            total += if fenetre == 0 {
                searcher.search(&cible.query, &Count)?
            } else {
                searcher.search(&cible.query, &CompteAvecScore)?
            };
            if let Some(e) = cible.incidents.erreur() {
                return Err(e);
            }
            if fenetre == 0 {
                continue;
            }
            let top =
                searcher.search(&cible.query, &TopDocs::with_limit(fenetre).order_by_score())?;
            if let Some(e) = cible.incidents.erreur() {
                return Err(e);
            }
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

    if trie {
        if let Some(e) = conflit_de_familles(cibles, &apporte) {
            return Err(e);
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

    let rendu = req.rendu.clone();
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
            &cible.hl,
            &searchers[hit.cible],
            addr,
            score,
            sort_values,
            &rendu,
            &*cible.query,
            &cible.nommees,
            &cible.incidents,
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

/// Un index sur lequel un `top_hits` va chercher les documents **d'un seau**.
///
/// C'est une [`Cible`] amputee de tout ce qui n'a pas de sens ici : le
/// surlignage et les clauses nommees se resolvent sur la requete de la
/// recherche, pas sur celle d'un seau, et les deux sont refuses en les nommant
/// (voir [`crate::metriques`]).
pub(crate) struct CibleTopHits<'a> {
    pub nom: &'a str,
    pub gen: &'a Generation,
    pub searcher: &'a Searcher,
    /// La requete de la recherche **croisee avec la contrainte du seau**.
    pub query: &'a dyn Query,
    pub plan: &'a crate::fetch::Plan,
    /// Les cles de tri du `top_hits`, resolues dans cette generation.
    pub sort: &'a [SortSpec],
    /// Les clauses nommees de la recherche : ES les rejoue contre les hits d'un
    /// `top_hits` comme contre ceux de la reponse, et le hit porte alors son
    /// `matched_queries` (mesure contre ES 8.15). ferrite ne les rendait pas —
    /// trouve par une plage de controle du fuzzer.
    pub nommees: &'a [(String, Box<dyn Query>)],
}

/// Le bloc `hits` d'un `top_hits` : une recherche complete a l'interieur d'un
/// seau.
///
/// C'est le meme `query_then_fetch` que [`execute`], sur le meme collecteur de
/// tri et le meme `build_hit` — sans quoi deux chemins rendraient deux formes
/// de hit. Ce qui change est ce qu'il y a autour : la requete est celle du
/// seau, et le resultat est un objet `{total, max_score, hits}` a poser dans
/// une agregation plutot qu'a la racine de la reponse.
pub(crate) fn bloc_top_hits(
    cibles: &[CibleTopHits<'_>],
    from: usize,
    size: usize,
    sort_asc: &[bool],
    rendu: &Rendu,
) -> EsResult<Value> {
    let trie = !sort_asc.is_empty();
    let needs_score = !trie
        || cibles
            .iter()
            .any(|c| c.sort.iter().any(|s| matches!(s.key, SortKey::Score)));
    let fenetre = from + size;

    let mut total = 0usize;
    let mut max_score: Option<f32> = None;
    let mut candidats: Vec<Hit> = Vec::new();
    for (rang, cible) in cibles.iter().enumerate() {
        if trie {
            let collector = SortCollector {
                specs: Arc::new(cible.sort.to_vec()),
                needs_score,
                cible: rang,
            };
            let mut locaux = cible.searcher.search(cible.query, &collector)?;
            total += locaux.len();
            locaux.sort_by(|a, b| compare(sort_asc, a, b));
            locaux.truncate(fenetre);
            candidats.extend(locaux);
        } else {
            total += cible.searcher.search(cible.query, &CompteAvecScore)?;
            let top = cible
                .searcher
                .search(cible.query, &TopDocs::with_limit(fenetre).order_by_score())?;
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
    candidats.sort_by(|a, b| compare(sort_asc, a, b));

    let hl = crate::highlight::Plan::default();
    let incidents = crate::fonction_score::Incidents::anonymes();
    let mut hits = Vec::new();
    for hit in candidats.into_iter().skip(from).take(size) {
        let cible = &cibles[hit.cible];
        hits.push(build_hit(
            cible.nom,
            cible.gen,
            cible.plan,
            &hl,
            cible.searcher,
            DocAddress::new(hit.seg, hit.doc),
            (!trie || needs_score).then_some(hit.score),
            trie.then(|| hit.keys.iter().map(SortValue::to_json).collect()),
            rendu,
            cible.query,
            cible.nommees,
            &incidents,
        )?);
    }
    // Rejouer une clause nommee **note** le document, donc peut declencher un
    // garde-fou de `function_score` — et ici, a la restitution. Le verdict ne
    // doit pas se perdre parce qu'il est tombe dans un seau (meme piege que
    // celui de la carte 43, une agregation plus loin).
    if let Some(e) = incidents.erreur() {
        return Err(e);
    }
    Ok(json!({
        "hits": {
            // ES ne tronque pas ce total : un `top_hits` porte le compte du
            // seau, pas celui de la page qu'il rend.
            "total": {"value": total, "relation": "eq"},
            "max_score": if trie { Value::Null } else {
                max_score.map_or(Value::Null, |s| json!(round_score(s)))
            },
            "hits": hits,
        }
    }))
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
            // Sur un `keyword`, ES n'a pas de sentinelle de chaine : le
            // document part en tete ou en queue **quel que soit le sens du
            // tri** (`missing: _first` en `desc` le met bien en premier).
            (SortValue::Absente { .. }, SortValue::Absente { .. }) => Ordering::Equal,
            (SortValue::Absente { en_tete }, _) => bout(*en_tete),
            (_, SortValue::Absente { en_tete }) => bout(*en_tete).reverse(),
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

fn bout(en_tete: bool) -> Ordering {
    if en_tete {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

/// Deux index qui ne trient pas dans la **meme famille** ne se fusionnent pas.
///
/// C'est le garde-fou d'ES, et il n'a rien d'evident : `unmapped_type` est
/// justement fait pour trier sur un champ qu'un index ne mappe pas, mais le type
/// choisi doit se comparer a celui des autres. `unmapped_type: keyword` sur un
/// champ ailleurs `long` rend donc 400, pas un ordre. Sans ce controle, ferrite
/// comparait un `I64` a un `Str` en les declarant ex aequo : un ordre faux, en
/// 200, exactement ce que ce projet refuse.
///
/// Deux details mesures contre ES 8.15, tous deux visibles dans le message :
/// l'erreur nomme le champ tel que le **second** index le voit — donc
/// `__anonymous_` quand c'est lui qui porte l'`unmapped_type` — et les deux
/// familles sortent dans l'ordre des index. Et elle ne tombe que si les deux
/// index **ont apporte un document** : une recherche qui n'en ramene aucun (ou
/// un `size: 0`) rend 200 malgre le conflit.
fn conflit_de_familles(cibles: &[Cible], apporte: &[bool]) -> Option<EsError> {
    let nb_cles = cibles.first().map_or(0, |c| c.sort.len());
    for j in 0..nb_cles {
        let mut vue: Option<&SortField> = None;
        for (rang, cible) in cibles.iter().enumerate() {
            if !apporte.get(rang).copied().unwrap_or(false) {
                continue;
            }
            let SortKey::Field(f) = &cible.sort[j].key else {
                continue;
            };
            match vue {
                None => vue = Some(f.as_ref()),
                Some(premier) if famille_de_tri(premier.ty) != famille_de_tri(f.ty) => {
                    return Some(tri_incompatible(premier, f.as_ref()));
                }
                Some(_) => {}
            }
        }
    }
    None
}

/// Le nom sous lequel ES designe le champ d'un `unmapped_type` : le mapper
/// anonyme qu'il fabrique pour l'occasion n'a pas d'autre nom.
const ANONYME: &str = "__anonymous_";

fn tri_incompatible(premier: &SortField, second: &SortField) -> EsError {
    let nom = if second.mappe {
        second.name.as_str()
    } else {
        ANONYME
    };
    let cause = EsError::illegal_argument(format!(
        "Can't sort on field [{nom}]; the field has incompatible sort types: [{}] and [{}] across \
         shards!",
        famille_de_tri(premier.ty),
        famille_de_tri(second.ty)
    ));
    // L'enveloppe est celle d'ES, `root_cause` vide et `reason` vide comprises :
    // c'est une erreur de la phase de fusion, pas d'un shard, et un client qui
    // journalise le corps doit lire la meme chose des deux cotes.
    EsError::new(
        axum::http::StatusCode::BAD_REQUEST,
        "search_phase_execution_exception",
        "",
    )
    .with("phase", json!("rank-feature"))
    .with("grouped", json!(true))
    .with("failed_shards", json!([]))
    .with("caused_by", cause.cause())
    .avec_racines(Vec::new())
}

#[allow(clippy::too_many_arguments)]
fn build_hit(
    index_name: &str,
    gen: &Generation,
    plan: &crate::fetch::Plan,
    hl: &crate::highlight::Plan,
    searcher: &tantivy::Searcher,
    addr: DocAddress,
    score: Option<f32>,
    sort_values: Option<Vec<Value>>,
    rendu: &Rendu,
    query: &dyn Query,
    nommees: &[(String, Box<dyn Query>)],
    incidents: &crate::fonction_score::Incidents,
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
    // ES ne pose `_shard` et `_node` que quand `explain` est demande : ils
    // disent de quel shard vient l'arbre qui suit. ferrite n'a qu'un shard et
    // qu'un noeud, mais la reponse change quand meme de forme.
    if rendu.explique {
        hit.insert("_shard".into(), json!(format!("[{index_name}][0]")));
        hit.insert("_node".into(), json!(rendu.noeud));
    }
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
    // Le surlignage lit le `_source` **complet** lui aussi, et pour la meme
    // raison : ES rend les fragments d'un champ que le filtre `_source` a
    // retire. Il se calcule donc avant que le filtre ne consomme la valeur.
    let fragments = crate::highlight::rendre(hl, gen, &source, &id)?;
    if let Some(filtered) = rendu.source.apply(source) {
        hit.insert("_source".into(), filtered);
    }
    if let Some(b) = blocs.fields {
        hit.insert("fields".into(), b);
    }
    if let Some(b) = blocs.ignores {
        hit.insert("ignored_field_values".into(), b);
    }
    if let Some(b) = fragments {
        hit.insert("highlight".into(), b);
    }
    if let Some(sv) = sort_values {
        hit.insert("sort".into(), Value::Array(sv));
    }
    // Une clause nommee est **rejouee, et notee** : elle peut donc rencontrer ce
    // qu'ES traite en erreur — un `field_value_factor` a score negatif, une
    // valeur manquante — alors que la recherche principale ne l'a pas
    // rencontre. C'est exactement ce qui arrive sous un `sort` : personne n'y
    // demande de score, la requete principale ne calcule rien, et c'est le
    // `_name` qui rallume le calcul (mesure : sans `sort` les deux serveurs
    // refusent, avec `sort` les deux repondent, avec `sort` **et** `_name` seul
    // ES refusait).
    //
    // L'incident se relit donc **ici**, et pas apres la recherche : a ce
    // moment-la il n'existait pas encore. Le laisser tomber rendait 200 la ou
    // ES rend 400 — un silence, trouve par une plage de controle du fuzzer
    // (graine 9610018) apres le rebase sur la carte 40.
    let noms = crate::explain::matched_queries(searcher, nommees, addr, rendu.noms_avec_score);
    if let Some(e) = incidents.erreur() {
        return Err(e);
    }
    if let Some(m) = noms {
        hit.insert("matched_queries".into(), m);
    }
    if rendu.explique {
        let arbre = crate::explain::expliquer(searcher, query, addr, &gen.index.schema())
            .unwrap_or_else(crate::explain::sans_correspondance);
        hit.insert("_explanation".into(), arbre.json());
    }
    Ok(Value::Object(hit))
}

/// Le `_score` tel qu'ES l'ecrit.
///
/// ES serialise un `float`, et Java comme Rust en rendent la **plus courte
/// ecriture decimale qui le represente sans perte** : le score n'a donc qu'a
/// faire l'aller-retour par cette ecriture-la pour tomber sur le meme nombre.
///
/// Ce qui etait fait avant — arrondir a la septieme decimale — visait la bonne
/// chose (ne pas exposer le bruit du `f32 -> f64`) et falsifiait la valeur : un
/// score de `1e-9` sortait a **`0.0`**, et `12345.6789` a `12345.6787109` la ou
/// ES ecrit `12345.679`. C'est sans consequence tant qu'un score vaut quelques
/// unites ; une decroissance `gauss` sur des dates en rend couramment en 1e-26,
/// et le client lisait alors zero pour chacun de ses documents.
pub fn round_score(score: f32) -> f64 {
    // `f32::to_string` est la plus courte ecriture qui round-trip ; le `f64`
    // qu'elle donne se reserialise a l'identique.
    score
        .to_string()
        .parse()
        .unwrap_or_else(|_| f64::from(score))
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
            keys: vec![SortValue::Absente { en_tete: false }],
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
        let last = |ty, asc| sentinelle(ty, asc, false).to_json();
        assert_eq!(last(FieldType::Long, true), json!(i64::MAX));
        assert_eq!(last(FieldType::Long, false), json!(i64::MIN));
        assert_eq!(last(FieldType::Boolean, true), json!(i64::MAX));
        assert_eq!(last(FieldType::Date, false), json!(i64::MIN));
        assert_eq!(last(FieldType::Double, true), json!("Infinity"));
        assert_eq!(last(FieldType::Double, false), json!("-Infinity"));
        assert_eq!(last(FieldType::Keyword, true), Value::Null);

        // `missing: _first` prend l'autre bout, et il ne se lit pas non plus
        // dans le tableau `sort` d'un `keyword` : la meme cle `null` y sort en
        // tete ou en queue selon ce qui a ete demande.
        let first = |ty, asc| sentinelle(ty, asc, true);
        assert_eq!(first(FieldType::Long, true).to_json(), json!(i64::MIN));
        assert_eq!(first(FieldType::Long, false).to_json(), json!(i64::MAX));
        assert_eq!(first(FieldType::Double, true).to_json(), json!("-Infinity"));
        assert_eq!(first(FieldType::Keyword, false).to_json(), Value::Null);

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
        let vide = cle_suivante(sentinelle(FieldType::Long, true, false), 4);
        let plein = cle_suivante(SortValue::I64(i64::MAX), 8);
        assert_eq!(compare(&[true, true], &vide, &plein), Ordering::Less);

        // Sur un `keyword` en revanche, `_first` l'emporte sur le sens du tri :
        // le marqueur part en tete meme en `desc` (mesure contre ES 8.15).
        let absent = cle_suivante(SortValue::Absente { en_tete: true }, 1);
        let mot = cle_suivante(SortValue::Str("zoulou".into()), 2);
        assert_eq!(compare(&[false, false], &absent, &mot), Ordering::Less);
    }

    /// Le `mode` par defaut d'ES sur un champ multivalue : minimum en
    /// croissant, maximum en decroissant.
    #[test]
    fn extremum_selon_le_sens_du_tri() {
        assert_eq!(extremum([5i64, 1, 9].into_iter(), true), Some(1));
        assert_eq!(extremum([5i64, 1, 9].into_iter(), false), Some(9));
        assert_eq!(extremum(std::iter::empty::<i64>(), true), None);
    }

    /// Les bords de `mode`, tous mesures contre un ES 8.15 : ce sont ceux de
    /// `MultiValueMode` de Lucene, et aucun n'etait devinable.
    #[test]
    fn modes_sur_une_colonne_d_entiers() {
        let v = [1i64, 3, 5, 9];
        assert_eq!(choisir_i64(&v, SortMode::Min), 1);
        assert_eq!(choisir_i64(&v, SortMode::Max), 9);
        assert_eq!(choisir_i64(&v, SortMode::Sum), 18);
        // 18 / 4 = 4,5 : `Math.round` arrondit **vers le haut**, donc 5.
        assert_eq!(choisir_i64(&v, SortMode::Avg), 5);
        // Un nombre pair de valeurs moyenne les deux du milieu : (3 + 5) / 2.
        assert_eq!(choisir_i64(&v, SortMode::Median), 4);
        assert_eq!(choisir_i64(&[1i64, 2, 3, 4, 5], SortMode::Median), 3);
        assert_eq!(choisir_i64(&[2i64, 3], SortMode::Median), 3);

        // La somme **deborde**, comme un `long` de Java : c'est ce qui decide
        // de l'ordre, et saturer rendrait un autre classement que celui d'ES.
        assert_eq!(choisir_i64(&[1i64, i64::MAX], SortMode::Sum), i64::MIN);
        assert_eq!(choisir_i64(&[i64::MIN, -1], SortMode::Sum), i64::MAX);
        // Et `avg` part de cette somme debordee.
        assert_eq!(
            choisir_i64(&[1i64, i64::MAX], SortMode::Avg),
            -4611686018427387904
        );
        // `median`, lui, passe par les flottants sans deborder.
        assert_eq!(
            choisir_i64(&[i64::MIN, -1], SortMode::Median),
            -4611686018427387904
        );
        // A une seule valeur, ES ne passe pas par le flottant du tout.
        assert_eq!(choisir_i64(&[i64::MAX], SortMode::Avg), i64::MAX);
    }

    #[test]
    fn modes_sur_une_colonne_de_flottants() {
        let v = [-1.0f64, 2.0, 7.5];
        assert_eq!(choisir_f64(&v, SortMode::Sum), 8.5);
        assert_eq!(choisir_f64(&v, SortMode::Avg), 2.833_333_333_333_333_5);
        assert_eq!(choisir_f64(&v, SortMode::Median), 2.0);
        assert_eq!(
            choisir_f64(&[0.1f64, 0.2], SortMode::Median),
            0.150_000_000_000_000_02
        );
        // Une somme qui deborde rend `Infinity`, et ES la rend en chaine —
        // exactement comme une valeur absente.
        assert_eq!(
            SortValue::F64(choisir_f64(&[1e308f64, 1e308], SortMode::Sum)).to_json(),
            json!("Infinity")
        );
    }

    /// `Math.round(double)` de Java, qui n'est pas celui de Rust.
    #[test]
    fn arrondi_a_la_java() {
        assert_eq!(arrondi_java(2.5), 3);
        // Rust rendrait -3 : il s'ecarte de zero, Java arrondit vers le haut.
        assert_eq!(arrondi_java(-2.5), -2);
        assert_eq!(arrondi_java(2.4), 2);
        // Le cas que le JDK corrige depuis sa 7 : `x + 0.5` franchit 1.
        assert_eq!(arrondi_java(0.499_999_999_999_999_94), 0);
    }

    /// Les familles de tri d'ES : ni le type du champ, ni son `FieldKind`.
    /// `float` et `double` n'y sont pas ensemble, et c'est ce qui fait echouer
    /// une recherche sur deux index qui ne les mappent pas pareil.
    #[test]
    fn familles_de_tri() {
        for ty in [
            FieldType::Byte,
            FieldType::Short,
            FieldType::Integer,
            FieldType::Long,
            FieldType::Date,
            FieldType::Boolean,
        ] {
            assert_eq!(famille_de_tri(ty), "LONG");
        }
        assert_eq!(famille_de_tri(FieldType::Float), "FLOAT");
        assert_eq!(famille_de_tri(FieldType::Double), "DOUBLE");
        assert_eq!(famille_de_tri(FieldType::Keyword), "STRING");
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
