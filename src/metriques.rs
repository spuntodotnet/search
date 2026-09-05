//! Les deux metriques qu'`aggs` n'envoie **pas** a tantivy : `percentiles` et
//! `top_hits`.
//!
//! Elles ont en commun de ne pas etre des accumulateurs. Une moyenne se
//! delegue : elle se calcule en une passe, sans rien retenir. Ces deux-la
//! demandent l'ensemble des documents du seau — la liste triee des valeurs
//! pour l'une, les N meilleurs documents pour l'autre — et c'est cette
//! propriete qui decide de tout le reste : ferrite les execute lui-meme, seau
//! par seau, sur la requete croisee du seau (voir [`crate::aggs`]).
//!
//! # `percentiles` : ce que la mesure a dit avant qu'on ecrive une ligne
//!
//! Elasticsearch annonce une approximation (un t-digest), et un nombre approche
//! rendu sous son nom sans le dire est exactement ce que ce depot refuse — c'est
//! le raisonnement qui fait refuser `cardinality`. La question etait donc :
//! **de combien** ES s'ecarte du percentile exact, et la reponse n'etait pas
//! celle qu'on attendait.
//!
//! ES 8.15 n'est approche qu'**au-dela de 2 000 valeurs**. En dessous, son
//! `TDigestState` garde les valeurs telles quelles et son quantile est une
//! interpolation lineaire sur le tableau trie :
//!
//! ```text
//! idx = p/100 * (n - 1) ;  lo = floor(idx)
//! v[lo] + (idx - lo) * (v[lo+1] - v[lo])       (v[0] et v[n-1] aux bords)
//! ```
//!
//! C'est [`quantile`], reproduit au bit pres. La bascule se mesure a la valeur
//! pres — 1 999 valeurs : exact ; 2 000 : plus du tout (jusqu'a 7,3 % d'ecart
//! sur les queues, mesure). Rendre l'exact n'est donc pas « une divergence
//! favorable » : c'est **la reponse d'ES elle-meme** sur tout le regime ou ES
//! est exact, et la seule divergence commence la ou ES cesse de promettre une
//! valeur. Elle est declaree, chiffree, et tenue par
//! [`sonde_metriques.py`](../tests/compat/sonde_metriques.py).
//!
//! Le prix est celui d'une liste : ferrite retient les valeurs du seau pour les
//! trier, la ou ES bascule sur une esquisse de taille bornee. C'est le meme
//! echange que celui du `scroll`, et il est publie.

use serde_json::{json, Map, Value};
use tantivy::collector::{Collector, SegmentCollector};
use tantivy::columnar::Column;
use tantivy::{DocId, Score, SegmentOrdinal, SegmentReader};

use crate::error::{EsError, EsResult};
use crate::mapping::{FieldKind, Fields, TypedValue};

/// Les sept percentiles qu'ES rend quand on ne lui en demande aucun.
const PERCENTS_DEFAUT: &[f64] = &[1.0, 5.0, 25.0, 50.0, 75.0, 95.0, 99.0];

/// Une demande `percentiles`, lue et validee.
#[derive(Debug, Clone)]
pub struct Percentiles {
    pub champ: String,
    /// Les percentiles voulus, **tries croissant** : c'est l'ordre dans lequel
    /// ES les rend, quel que soit celui de la demande (mesure).
    pub percents: Vec<f64>,
    pub keyed: bool,
    /// La valeur de remplacement des documents sans valeur, deja ramenee a
    /// l'echelle rendue (millisecondes sur une date).
    pub missing: Option<f64>,
}

/// Une demande `top_hits`, lue et validee — **avant** toute resolution sur un
/// mapping.
///
/// Le corps est garde tel quel : `sort`, `_source`, `fields`… se resolvent
/// index par index, exactement comme ceux de la recherche englobante, et deux
/// index ne rendent pas les memes champs pour le meme motif.
#[derive(Debug, Clone)]
pub struct TopHits {
    pub from: usize,
    pub size: usize,
    pub sort: Option<Value>,
    pub source: Option<Value>,
    pub fields: Option<Value>,
    pub docvalue: Option<Value>,
    pub stored: Option<Value>,
}

/// Les parametres qu'ES accepte dans un `top_hits`, et que ferrite sert.
const TOP_HITS_SERVIS: &[&str] = &[
    "from",
    "size",
    "sort",
    "_source",
    "fields",
    "docvalue_fields",
    "stored_fields",
    "script_fields",
];

/// Ceux qu'ES accepte et que ferrite refuse **en les nommant**, avec la raison.
fn top_hits_refuse(cle: &str) -> Option<&'static str> {
    Some(match cle {
        "highlight" => {
            "le surlignage se resout sur la requete de la recherche et sur le mapping de \
             l'index, pas sur la requete d'un seau — le reproduire ici demanderait de \
             refaire ce chemin par seau"
        }
        "explain" => {
            "l'arbre du score est construit pour la requete de la recherche ; celle d'un \
             seau est croisee avec la contrainte du seau, et son arbre ne serait pas celui \
             qu'Elasticsearch rend"
        }
        "version" => "le hit d'un [top_hits] ne porte pas [_version]",
        "seq_no_primary_term" => "le hit d'un [top_hits] ne porte pas [_seq_no]",
        "track_scores" => {
            "il faudrait noter les documents sous un tri par champ, la ou le collecteur ne \
             demande aucun score"
        }
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Lecture de la demande
// ---------------------------------------------------------------------------

/// Lit et valide un `percentiles`.
///
/// Les quatre refus de forme reproduisent ceux d'ES, mesures un par un : un
/// `percents` **vide**, un doublon, une valeur hors de `[0, 100]` sont tous
/// refuses a la lecture du corps (`x_content_parse_exception`), pas a
/// l'execution.
pub fn lire_percentiles(
    nom: &str,
    corps: &Value,
    champs: Option<&Fields>,
) -> EsResult<Percentiles> {
    let obj = corps
        .as_object()
        .ok_or_else(|| EsError::parsing(format!("[aggs.{nom}.percentiles] doit etre un objet")))?;
    for cle in obj.keys() {
        match cle.as_str() {
            "field" | "percents" | "keyed" | "missing" => {}
            // Les deux parametres qui choisissent l'**algorithme** d'ES. Ils ne
            // sont pas ignorables : ils ne changent pas la forme de la reponse,
            // ils changent le nombre qu'elle porte.
            "tdigest" | "hdr" => {
                // Un objet vide ne demande rien — meme regle que `script_fields`.
                if cle == "tdigest" && obj[cle].as_object().is_some_and(Map::is_empty) {
                    continue;
                }
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas [{cle}] dans [percentiles] (agregation [{nom}]) : \
                     ce parametre regle l'**approximation** d'Elasticsearch, et ferrite ne \
                     s'approxime pas — il rend le percentile exact, celui qu'ES lui-meme rend \
                     tant qu'un seau porte moins de 2 000 valeurs. Le servir en l'ignorant \
                     rendrait un nombre que le client n'a pas demande (voir docs/compat.md)"
                )));
            }
            autre => {
                return Err(EsError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "x_content_parse_exception",
                    format!("[percentiles] unknown field [{autre}]"),
                ))
            }
        }
    }
    let champ = obj
        .get("field")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            EsError::illegal_argument(
                "Required one of fields [field, script], but none were specified. ",
            )
        })?
        .to_string();

    let mut percents: Vec<f64> = match obj.get("percents") {
        None => PERCENTS_DEFAUT.to_vec(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_f64().ok_or_else(percents_illisible))
            .collect::<EsResult<_>>()?,
        Some(_) => return Err(percents_illisible()),
    };
    // ES rend ses percentiles **tries**, quel que soit l'ordre demande, et
    // refuse aussi bien la liste vide que le doublon et la valeur hors bornes —
    // tous trois avec la meme phrase, celle de son constructeur.
    if percents.is_empty() || percents.iter().any(|p| !(0.0..=100.0).contains(p)) {
        return Err(percents_illisible());
    }
    percents.sort_by(f64::total_cmp);
    if percents.windows(2).any(|p| p[0] == p[1]) {
        return Err(percents_illisible());
    }

    let missing = match obj.get("missing") {
        None => None,
        Some(v) => Some(valeur_de_remplacement(nom, &champ, v, champs)?),
    };
    Ok(Percentiles {
        champ,
        percents,
        keyed: obj.get("keyed").and_then(Value::as_bool).unwrap_or(true),
        missing,
    })
}

/// La phrase et le type qu'ES donne a un `percents` mal forme.
///
/// Il le refuse a la **lecture du corps** : le message est celui de son
/// constructeur, pas celui d'une agregation qui aurait commence.
fn percents_illisible() -> EsError {
    EsError::new(
        axum::http::StatusCode::BAD_REQUEST,
        "x_content_parse_exception",
        "Failed to build [percentiles] after last required field arrived",
    )
}

/// `missing` : la valeur que prennent les documents sans valeur, ramenee a
/// l'echelle **rendue** — les millisecondes sur une date, `0` / `1` sur un
/// booleen.
fn valeur_de_remplacement(
    nom: &str,
    champ: &str,
    v: &Value,
    champs: Option<&Fields>,
) -> EsResult<f64> {
    // Sans mapping (recherche qui ne vise aucun index), la forme se verifie
    // quand meme : seule la conversion au type du champ reste indecidable.
    let Some(mf) = champs.and_then(|c| c.get(champ)) else {
        return Ok(v.as_f64().unwrap_or(0.0));
    };
    match crate::mapping::coerce("missing", mf.ty, v)? {
        TypedValue::I64(n) => Ok(n as f64),
        TypedValue::F64(x) => Ok(x),
        TypedValue::Date(ms) => Ok(ms as f64),
        TypedValue::Bool(b) => Ok(f64::from(u8::from(b))),
        TypedValue::Str(_) => Err(EsError::illegal_argument(format!(
            "[aggs.{nom}.percentiles.missing] : [{v}] n'est pas un nombre"
        ))),
    }
}

/// Lit et valide un `top_hits`.
///
/// C'est la seule agregation de ce fichier qui n'a **pas** de `field` : elle
/// rend des documents, pas une statistique sur une colonne.
pub fn lire_top_hits(nom: &str, corps: &Value) -> EsResult<TopHits> {
    let obj = corps
        .as_object()
        .ok_or_else(|| EsError::parsing(format!("[aggs.{nom}.top_hits] doit etre un objet")))?;
    for (cle, valeur) in obj {
        if let Some(raison) = top_hits_refuse(cle) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{cle}] dans [top_hits] (agregation [{nom}]) : \
                 {raison} (voir docs/compat.md)"
            )));
        }
        if !TOP_HITS_SERVIS.contains(&cle.as_str()) {
            // La phrase d'ES, qui nomme le **jeton** qu'il vient de lire.
            return Err(EsError::parsing(format!(
                "Unknown key for a {} in [{cle}].",
                jeton(valeur)
            )));
        }
    }
    // `script_fields` vide ne definit rien : accepte, comme ailleurs dans
    // ferrite. Non vide, il demande un script Painless.
    if let Some(v) = obj.get("script_fields") {
        if !v.as_object().is_some_and(Map::is_empty) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [script_fields] dans [top_hits] (agregation [{nom}]) : \
                 il definit des champs calcules par un script Painless, que ferrite n'execute \
                 pas (seul l'objet vide, qui ne definit rien, est accepte)"
            )));
        }
    }
    let entier = |cle: &str, defaut: usize| -> EsResult<usize> {
        match obj.get(cle) {
            None => Ok(defaut),
            Some(v) => v
                .as_u64()
                .map(|n| n as usize)
                .ok_or_else(|| EsError::parsing(format!("[top_hits.{cle}] doit etre un entier"))),
        }
    };
    // ES rend trois documents quand on ne lui dit rien, et refuse `size: 0`
    // avec la phrase de Lucene (mesure).
    let size = entier("size", 3)?;
    if size == 0 {
        return Err(EsError::illegal_argument(
            "numHits must be > 0; please use TotalHitCountCollectorManager if you just need \
             the total hit count",
        ));
    }
    Ok(TopHits {
        from: entier("from", 0)?,
        size,
        sort: obj.get("sort").cloned(),
        source: obj.get("_source").cloned(),
        fields: obj.get("fields").cloned(),
        docvalue: obj.get("docvalue_fields").cloned(),
        stored: obj.get("stored_fields").cloned(),
    })
}

fn jeton(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "VALUE_STRING",
        Value::Number(_) => "VALUE_NUMBER",
        Value::Bool(_) => "VALUE_BOOLEAN",
        Value::Array(_) => "START_ARRAY",
        Value::Object(_) => "START_OBJECT",
        Value::Null => "VALUE_NULL",
    }
}

// ---------------------------------------------------------------------------
// `percentiles` : le calcul
// ---------------------------------------------------------------------------

/// Le quantile d'Elasticsearch **quand il est exact**, reproduit tel quel.
///
/// `tri` doit etre trie croissant. Les deux gardes de bord ne sont pas
/// decoratives : sans elles, `p = 100` lirait `v[n]`, et `p = 0` sur un
/// tableau d'un seul element diviserait par zero.
pub fn quantile(tri: &[f64], percent: f64) -> Option<f64> {
    let n = tri.len();
    if n == 0 {
        return None;
    }
    if n == 1 {
        return Some(tri[0]);
    }
    let idx = percent / 100.0 * (n - 1) as f64;
    if idx <= 0.0 {
        return Some(tri[0]);
    }
    if idx >= (n - 1) as f64 {
        return Some(tri[n - 1]);
    }
    let bas = idx.floor();
    let i = bas as usize;
    Some(tri[i] + (idx - bas) * (tri[i + 1] - tri[i]))
}

/// La cle d'un percentile dans la reponse : `"1.0"`, `"33.3"`, `"100.0"`.
///
/// C'est le `Double.toString` de Java, dont la seule particularite lisible ici
/// est qu'il ecrit toujours une decimale.
pub fn cle_percent(p: f64) -> String {
    let mut s = format!("{p}");
    if !s.contains('.') && !s.contains('e') && !s.contains("inf") && !s.contains("NaN") {
        s.push_str(".0");
    }
    s
}

/// Le bloc `percentiles` au format d'Elasticsearch.
///
/// `valeurs` porte toutes les valeurs du seau, dans n'importe quel ordre ; le
/// tri se fait ici. `rendre` met une valeur en forme lisible quand le champ est
/// une date (ES ajoute alors une cle `"50.0_as_string"` a cote de `"50.0"`).
pub fn bloc(
    demande: &Percentiles,
    mut valeurs: Vec<f64>,
    rendre: &dyn Fn(f64) -> Option<String>,
) -> Value {
    valeurs.sort_by(f64::total_cmp);
    let couples: Vec<(f64, Option<f64>)> = demande
        .percents
        .iter()
        .map(|p| (*p, quantile(&valeurs, *p)))
        .collect();
    let valeur_json = |v: Option<f64>| v.map_or(Value::Null, |x| json!(x));
    let contenu = if demande.keyed {
        let mut m = Map::new();
        for (p, v) in &couples {
            m.insert(cle_percent(*p), valeur_json(*v));
            if let Some(texte) = v.and_then(rendre) {
                m.insert(format!("{}_as_string", cle_percent(*p)), json!(texte));
            }
        }
        Value::Object(m)
    } else {
        Value::Array(
            couples
                .iter()
                .map(|(p, v)| {
                    let mut m = Map::new();
                    m.insert("key".into(), json!(p));
                    m.insert("value".into(), valeur_json(*v));
                    if let Some(texte) = v.and_then(rendre) {
                        m.insert("value_as_string".into(), json!(texte));
                    }
                    Value::Object(m)
                })
                .collect(),
        )
    };
    json!({ "values": contenu })
}

// ---------------------------------------------------------------------------
// La collecte des valeurs d'une colonne
// ---------------------------------------------------------------------------

/// Un collecteur qui ne retient rien d'autre que **les valeurs d'une colonne**,
/// pour les documents qui correspondent.
///
/// Un document multivalue verse **toutes** ses valeurs, doublons compris : ES
/// fait de meme (mesure — `[1, 1, 2]` y compte trois valeurs), et c'est ce qui
/// separe un percentile d'un `terms`, qui compte des documents.
pub struct ValeursColonne {
    champ: String,
    genre: FieldKind,
    missing: Option<f64>,
}

impl ValeursColonne {
    pub fn new(champ: &str, genre: FieldKind, missing: Option<f64>) -> Self {
        Self {
            champ: champ.to_string(),
            genre,
            missing,
        }
    }
}

enum Lecture {
    I64(Column<i64>),
    F64(Column<f64>),
    Bool(Column<bool>),
    Date(Column<tantivy::DateTime>),
    Rien,
}

pub struct SegmentValeurs {
    lecture: Lecture,
    missing: Option<f64>,
    out: Vec<f64>,
}

impl Collector for ValeursColonne {
    type Fruit = Vec<f64>;
    type Child = SegmentValeurs;

    fn for_segment(
        &self,
        _ord: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let colonnes = reader.fast_fields();
        let lecture = match self.genre {
            FieldKind::I64 => colonnes
                .column_opt::<i64>(&self.champ)?
                .map_or(Lecture::Rien, Lecture::I64),
            FieldKind::F64 => colonnes
                .column_opt::<f64>(&self.champ)?
                .map_or(Lecture::Rien, Lecture::F64),
            FieldKind::Bool => colonnes
                .column_opt::<bool>(&self.champ)?
                .map_or(Lecture::Rien, Lecture::Bool),
            FieldKind::Date => colonnes
                .column_opt::<tantivy::DateTime>(&self.champ)?
                .map_or(Lecture::Rien, Lecture::Date),
            // Un `text` ni un `keyword` n'ont de valeur numerique : la clause
            // est refusee en amont, avec la phrase d'ES.
            FieldKind::Keyword | FieldKind::Text => Lecture::Rien,
        };
        Ok(SegmentValeurs {
            lecture,
            missing: self.missing,
            out: Vec::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, enfants: Vec<Vec<f64>>) -> tantivy::Result<Vec<f64>> {
        Ok(enfants.into_iter().flatten().collect())
    }
}

impl SegmentCollector for SegmentValeurs {
    type Fruit = Vec<f64>;

    fn collect(&mut self, doc: DocId, _score: Score) {
        let avant = self.out.len();
        match &self.lecture {
            Lecture::I64(c) => self.out.extend(c.values_for_doc(doc).map(|v| v as f64)),
            Lecture::F64(c) => self.out.extend(c.values_for_doc(doc)),
            Lecture::Bool(c) => self
                .out
                .extend(c.values_for_doc(doc).map(|v| f64::from(u8::from(v)))),
            // ES compte les dates en **millisecondes** ; tantivy en
            // nanosecondes. La conversion se fait ici, une fois, plutot que sur
            // chaque quantile.
            Lecture::Date(c) => self.out.extend(
                c.values_for_doc(doc)
                    .map(|v| v.into_timestamp_millis() as f64),
            ),
            Lecture::Rien => {}
        }
        // Un document sans valeur ne compte pas — sauf sous `missing`, qui lui
        // en donne une.
        if self.out.len() == avant {
            if let Some(m) = self.missing {
                self.out.push(m);
            }
        }
    }

    fn harvest(self) -> Vec<f64> {
        self.out
    }
}
