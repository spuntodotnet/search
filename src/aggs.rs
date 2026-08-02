//! Agregations : validation stricte, execution, mise au format d'Elasticsearch.
//!
//! tantivy a son propre moteur d'agregations, calque sur celui d'ES, et il
//! deserialise deja du JSON de forme elasticsearch. Il serait donc tentant de
//! lui passer la demande du client telle quelle.
//!
//! On ne le fait pas. Serde **ignore en silence** les cles qu'il ne connait
//! pas : un `order`, un `missing` ou un `min_doc_count` non supporte
//! disparaitrait sans un mot et rendrait un resultat faux presente comme
//! complet. Chaque agregation et chaque parametre est donc verifie ici avant
//! d'etre transmis, et la reponse est remise au format exact d'ES.

use std::collections::HashMap;

use serde_json::{json, Map, Value};
use tantivy::aggregation::agg_req::Aggregations;
use tantivy::aggregation::intermediate_agg_result::IntermediateAggregationResults;
use tantivy::aggregation::{
    AggContextParams, AggregationLimitsGuard, DistributedAggregationCollector,
};
use tantivy::query::Query;
use tantivy::Searcher;

use crate::engine::Generation;
use crate::error::{EsError, EsResult};
use crate::mapping::{FieldKind, MappedField};

/// Ce qu'il faut savoir d'une agregation pour remettre son resultat au format
/// d'Elasticsearch.
#[derive(Debug, Clone)]
struct Info {
    type_agg: String,
    /// Le champ agrege est-il une date ? tantivy compte alors en nanosecondes
    /// la ou ES compte en millisecondes.
    date: bool,
    /// Le `format` declare du champ, s'il en a un : ES rend le `*_as_string`
    /// **a ce format**, pas en ISO.
    format: Option<crate::dateformat::DateFormat>,
    /// La `size` demandee par le client, pour tronquer apres re-tri.
    size: Option<usize>,
    /// L'ordre demande sur un `terms`.
    ordre: Ordre,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ordre {
    CountDesc,
    CountAsc,
    KeyAsc,
    KeyDesc,
}

/// Marge de buckets demandes a tantivy en plus de la `size` voulue.
///
/// tantivy rend ses buckets par compte decroissant, sans departager les
/// ex aequo ; ES, lui, departage par cle croissante. Pour rendre **exactement**
/// la meme selection, on demande plus de buckets que necessaire, on applique
/// l'ordre d'ES, puis on tronque.
const MARGE_TERMS: u64 = 500;

/// Nombre maximum de buckets rendus, toutes agregations confondues.
const MAX_BUCKETS: u32 = 65_535;
/// Memoire maximale consommee par une agregation.
const MEMORY_LIMIT: u64 = 500 * 1024 * 1024;

/// Les parametres acceptes, par type d'agregation.
///
/// Une cle absente de cette table est refusee : c'est la seule facon de ne pas
/// laisser serde en avaler une en silence.
fn allowed(agg: &str) -> Option<&'static [&'static str]> {
    Some(match agg {
        "min" | "max" | "sum" | "avg" | "value_count" | "stats" => &["field", "missing"],
        "terms" => &["field", "size", "shard_size", "min_doc_count", "order"],
        "range" => &["field", "ranges", "keyed"],
        "histogram" => &[
            "field",
            "interval",
            "offset",
            "min_doc_count",
            "hard_bounds",
            "extended_bounds",
            "keyed",
        ],
        "date_histogram" => &[
            "field",
            "fixed_interval",
            "offset",
            "min_doc_count",
            "hard_bounds",
            "extended_bounds",
            "keyed",
        ],
        _ => return None,
    })
}

/// Les agregations qui produisent des buckets, et peuvent donc porter des
/// sous-agregations.
fn est_bucket(agg: &str) -> bool {
    matches!(agg, "terms" | "range" | "histogram" | "date_histogram")
}

/// Les agregations d'ES que ferrite refuse **volontairement**, avec la raison.
///
/// Les distinguer des agregations inconnues rend le refus utile : le client
/// sait que ce n'est pas une faute de frappe de sa part.
fn refus_explicite(agg: &str) -> Option<&'static str> {
    Some(match agg {
        "cardinality" => {
            "l'estimation de tantivy differe de celle d'Elasticsearch (mesure : 582 valeurs \
             distinctes annoncees la ou ES en compte 598), y compris sous le seuil ou ES est \
             exact — le nombre rendu ne serait pas celui d'ES"
        }
        "filter" => {
            "l'agregation [filter] de tantivy prend une chaine de requete dans sa propre \
             syntaxe, pas une requete du Query DSL : la traduction serait approximative"
        }
        _ => return None,
    })
}

/// Verifie une demande d'agregations de bout en bout.
pub fn validate(aggs: &Value, gen: &Generation) -> EsResult<()> {
    let obj = aggs
        .as_object()
        .ok_or_else(|| EsError::parsing("[aggs] doit etre un objet"))?;
    if obj.is_empty() {
        return Err(EsError::illegal_argument("[aggs] ne peut pas etre vide"));
    }
    for (nom, corps) in obj {
        validate_une(nom, corps, gen)?;
    }
    Ok(())
}

fn validate_une(nom: &str, corps: &Value, gen: &Generation) -> EsResult<()> {
    let obj = corps
        .as_object()
        .ok_or_else(|| EsError::parsing(format!("[aggs.{nom}] doit etre un objet")))?;

    let mut type_agg: Option<(&str, &Value)> = None;
    let mut sous: Option<&Value> = None;
    for (cle, valeur) in obj {
        match cle.as_str() {
            "aggs" | "aggregations" => sous = Some(valeur),
            autre => {
                if type_agg.is_some() {
                    return Err(EsError::parsing(format!(
                        "[aggs.{nom}] : une seule agregation par nom (deux trouvees)"
                    )));
                }
                type_agg = Some((autre, valeur));
            }
        }
    }

    let (type_agg, corps_agg) = type_agg.ok_or_else(|| {
        EsError::parsing(format!("[aggs.{nom}] : aucun type d'agregation fourni"))
    })?;

    if let Some(raison) = refus_explicite(type_agg) {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas l'agregation [{type_agg}] (dans [aggs.{nom}]) : {raison} \
             (voir docs/compat.md)"
        )));
    }
    let params = allowed(type_agg).ok_or_else(|| {
        EsError::unsupported(format!(
            "ferrite ne supporte pas l'agregation [{type_agg}] (dans [aggs.{nom}]) ; \
             agregations supportees : min, max, sum, avg, value_count, stats, terms, range, \
             histogram, date_histogram"
        ))
    })?;

    {
        let corps_obj = corps_agg.as_object().ok_or_else(|| {
            EsError::parsing(format!("[aggs.{nom}.{type_agg}] doit etre un objet"))
        })?;
        for cle in corps_obj.keys() {
            if !params.contains(&cle.as_str()) {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas [{cle}] dans [{type_agg}] (agregation [{nom}]) ; \
                     parametres acceptes : {params:?}"
                )));
            }
        }
        let champ = corps_obj
            .get("field")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                EsError::illegal_argument(format!(
                    "[aggs.{nom}.{type_agg}] : [field] est obligatoire"
                ))
            })?;
        verifier_champ(nom, type_agg, champ, gen)?;
    }

    if type_agg == "terms" {
        if let Some(order) = corps_agg.get("order") {
            lire_ordre(order, nom)?;
        }
    }

    if let Some(sous) = sous {
        if !est_bucket(type_agg) {
            return Err(EsError::illegal_argument(format!(
                "[aggs.{nom}] : l'agregation [{type_agg}] ne peut pas porter de \
                 sous-agregations (ce n'est pas une agregation de buckets)"
            )));
        }
        validate(sous, gen)?;
    }
    Ok(())
}

/// L'ordre d'un `terms`.
///
/// Seuls `_count` et `_key` sont acceptes. Ordonner par une sous-agregation est
/// possible chez ES, mais sa regle de departage entre ex aequo n'a pas ete
/// verifiee ici : plutot que de rendre un ordre peut-etre different, on refuse.
fn lire_ordre(order: &Value, nom: &str) -> EsResult<Ordre> {
    let obj = order.as_object().ok_or_else(|| {
        EsError::illegal_argument(format!("[aggs.{nom}.order] doit etre un objet"))
    })?;
    if obj.len() != 1 {
        return Err(EsError::illegal_argument(format!(
            "[aggs.{nom}.order] : une seule cle de tri est acceptee"
        )));
    }
    let (cle, sens) = obj.iter().next().unwrap();
    let sens = sens.as_str().unwrap_or("");
    let asc = match sens {
        "asc" => true,
        "desc" => false,
        autre => {
            return Err(EsError::illegal_argument(format!(
                "[aggs.{nom}.order] : sens [{autre}] invalide (asc|desc)"
            )))
        }
    };
    match cle.as_str() {
        "_count" => Ok(if asc {
            Ordre::CountAsc
        } else {
            Ordre::CountDesc
        }),
        "_key" => Ok(if asc { Ordre::KeyAsc } else { Ordre::KeyDesc }),
        autre => Err(EsError::unsupported(format!(
            "ferrite ne supporte pas l'ordre [{autre}] dans [aggs.{nom}.order] ; cles \
             acceptees : _count, _key (ordonner par une sous-agregation n'est pas supporte)"
        ))),
    }
}

/// Une agregation lit un fast field : tous les types en ont un sauf `text`.
///
/// C'est aussi la regle d'ES, qui refuse d'agreger un `text` sans `fielddata`.
fn verifier_champ(nom: &str, type_agg: &str, champ: &str, gen: &Generation) -> EsResult<()> {
    let MappedField { ty, .. } = gen.fields.get(champ).ok_or_else(|| {
        EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "illegal_argument_exception",
            format!("Invalid aggregation [{nom}] : no mapping found for field [{champ}]"),
        )
        .sur_champ_inconnu(champ)
    })?;

    if ty.kind() == FieldKind::Text {
        return Err(EsError::illegal_argument(format!(
            "Fielddata is disabled on [{champ}] : ferrite n'agrege pas sur un champ [text] ; \
             utilise son multi-field [{champ}.keyword] s'il existe"
        )));
    }
    let numerique = matches!(ty.kind(), FieldKind::I64 | FieldKind::F64 | FieldKind::Date);
    match type_agg {
        "min" | "max" | "sum" | "avg" | "stats" | "histogram" | "range" if !numerique => {
            Err(EsError::illegal_argument(format!(
                "[{type_agg}] (agregation [{nom}]) exige un champ numerique ou date ; [{champ}] \
                 est de type [{}]",
                ty.name()
            )))
        }
        "date_histogram" if ty.kind() != FieldKind::Date => Err(EsError::illegal_argument(
            format!("[date_histogram] (agregation [{nom}]) exige un champ [date] ; [{champ}] est de type [{}]", ty.name()),
        )),
        _ => Ok(()),
    }
}

/// Un index a agreger : sa generation, son `searcher` et la requete construite
/// pour lui.
pub struct Part<'a> {
    pub gen: &'a Generation,
    pub searcher: &'a Searcher,
    pub query: &'a dyn Query,
}

/// Execute les agregations sur un ou plusieurs index et rend le resultat au
/// format d'Elasticsearch.
///
/// Chaque index est un index tantivy distinct : il faut donc l'agreger a part,
/// puis **fusionner**. On ne fusionne pas les resultats finaux — un `avg` final
/// ne porte plus le compte qui permettrait de le repondererer, et faire la
/// moyenne des moyennes rendrait un nombre faux. On collecte donc les resultats
/// **intermediaires** (`DistributedAggregationCollector`, prevu pour ca chez
/// tantivy), on les fusionne, et on ne finalise qu'une fois — exactement la
/// mecanique qu'ES applique entre ses shards.
pub fn run(parts: &[Part<'_>], aggs: &Value) -> EsResult<Value> {
    let Some(premiere) = parts.first() else {
        return Ok(Value::Object(Map::new()));
    };

    // Les metadonnees de mise en forme (champ date, `format`, `size`, ordre) se
    // lisent dans un mapping. Elles sont prises sur le premier index vise : ce
    // sont des proprietes de la **demande**, pas des documents.
    let mut infos = HashMap::new();
    let demande = preparer(aggs, premiere.gen, &mut infos);

    let requete: Aggregations = serde_json::from_value(demande)
        .map_err(|e| EsError::parsing(format!("[aggs] illisible : {e}")))?;
    let limites = AggregationLimitsGuard::new(Some(MEMORY_LIMIT), Some(MAX_BUCKETS));

    let mut cumul: Option<IntermediateAggregationResults> = None;
    for part in parts {
        let contexte = AggContextParams::new(limites.clone(), part.gen.index.tokenizers().clone());
        let collecteur = DistributedAggregationCollector::from_aggs(requete.clone(), contexte);
        let partiel = part
            .searcher
            .search(part.query, &collecteur)
            .map_err(|e| EsError::illegal_argument(format!("agregation : {e}")))?;
        match &mut cumul {
            None => cumul = Some(partiel),
            Some(total) => total
                .merge_fruits(partiel)
                .map_err(|e| EsError::internal(format!("fusion d'agregations : {e}")))?,
        }
    }

    let resultat = cumul
        .unwrap_or_default()
        .into_final_result(requete, limites)
        .map_err(|e| EsError::illegal_argument(format!("agregation : {e}")))?;

    let brut = serde_json::to_value(resultat)
        .map_err(|e| EsError::internal(format!("resultat d'agregation illisible : {e}")))?;
    Ok(mise_en_forme(&brut, &infos))
}

/// Recense ce qu'il faut savoir de chaque agregation, et prepare la demande
/// envoyee a tantivy (voir [`MARGE_TERMS`]).
fn preparer(aggs: &Value, gen: &Generation, infos: &mut HashMap<String, Info>) -> Value {
    let Some(obj) = aggs.as_object() else {
        return aggs.clone();
    };
    let mut out = Map::new();
    for (nom, corps) in obj {
        let Some(corps_obj) = corps.as_object() else {
            out.insert(nom.clone(), corps.clone());
            continue;
        };
        let mut nouveau = Map::new();
        for (cle, valeur) in corps_obj {
            if cle == "aggs" || cle == "aggregations" {
                nouveau.insert(cle.clone(), preparer(valeur, gen, infos));
                continue;
            }
            let champ = valeur.get("field").and_then(Value::as_str);
            let date = champ
                .and_then(|c| gen.fields.get(c))
                .is_some_and(|m| m.ty.kind() == FieldKind::Date);
            let format = champ.and_then(|c| gen.fields.format_de(c)).cloned();
            let ordre = valeur
                .get("order")
                .and_then(|o| lire_ordre(o, nom).ok())
                .unwrap_or(Ordre::CountDesc);
            let size = valeur
                .get("size")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .or(if cle == "terms" { Some(10) } else { None });

            let mut corps_agg = valeur.clone();
            if cle == "terms" {
                // On demande plus large pour pouvoir appliquer l'ordre d'ES.
                if let Some(o) = corps_agg.as_object_mut() {
                    let voulu = size.unwrap_or(10) as u64;
                    o.insert("size".into(), json!(voulu + MARGE_TERMS));
                }
            }
            infos.insert(
                nom.clone(),
                Info {
                    type_agg: cle.clone(),
                    date,
                    format,
                    size,
                    ordre,
                },
            );
            nouveau.insert(cle.clone(), corps_agg);
        }
        out.insert(nom.clone(), Value::Object(nouveau));
    }
    Value::Object(out)
}

/// La forme lisible d'une date : celle du `format` declare du champ, sinon
/// l'ISO d'Elasticsearch.
fn rend_date(millis: f64, info: &Info) -> Option<String> {
    match &info.format {
        Some(f) => f.rend(millis as i64),
        None => format_date(millis),
    }
}

/// `2023-01-01T00:00:00.000Z`, le format de sortie d'Elasticsearch.
fn format_date(millis: f64) -> Option<String> {
    use time::format_description::BorrowedFormatItem;
    use time::macros::format_description;

    const FORMAT: &[BorrowedFormatItem<'_>] =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");
    let nanos = (millis * 1_000_000.0) as i128;
    let dt = time::OffsetDateTime::from_unix_timestamp_nanos(nanos).ok()?;
    let rendu = dt.format(&FORMAT).ok()?;
    // ES prefixe d'un `+` les annees a plus de quatre chiffres — le format de
    // `time` le fait deja quand il y en a besoin, on ne le double pas.
    Some(if dt.year() > 9999 && !rendu.starts_with('+') {
        format!("+{rendu}")
    } else {
        rendu
    })
}

/// Remet le resultat de tantivy au format exact d'ES.
///
/// Quatre ecarts constates par `tests/compat/diff_aggs.py` sont corriges ici,
/// et tous sont documentes dans `docs/compat.md` :
///
/// 1. tantivy compte les dates en **nanosecondes**, ES en millisecondes ;
/// 2. ES ajoute un `*_as_string` a cote de chaque metrique de date ;
/// 3. ES departage les buckets `terms` ex aequo par **cle croissante** ;
/// 4. ES formate les bornes d'un `range` en flottants (`100.0`), meme sur un
///    champ entier, et rend la cle d'un `date_histogram` en entier.
fn mise_en_forme(brut: &Value, infos: &HashMap<String, Info>) -> Value {
    let Some(obj) = brut.as_object() else {
        return brut.clone();
    };
    let mut out = Map::new();
    for (nom, valeur) in obj {
        let vide = Info {
            type_agg: String::new(),
            date: false,
            format: None,
            size: None,
            ordre: Ordre::CountDesc,
        };
        let info = infos.get(nom).unwrap_or(&vide);
        out.insert(nom.clone(), mise_en_forme_une(valeur, info, infos));
    }
    Value::Object(out)
}

/// Les metriques dont la valeur est une date a convertir.
const METRIQUES_DATE: &[&str] = &["value", "min", "max", "avg", "sum"];

fn mise_en_forme_une(valeur: &Value, info: &Info, infos: &HashMap<String, Info>) -> Value {
    let Some(obj) = valeur.as_object() else {
        return valeur.clone();
    };
    let mut out = Map::new();

    // Les buckets d'abord : la troncature d'un `terms` dit combien de documents
    // partent avec les buckets ecartes, et ce compte doit rejoindre
    // `sum_other_doc_count`.
    let mut ecartes = 0u64;
    if let Some(buckets) = obj.get("buckets") {
        let (rendus, perdus) = mise_en_forme_buckets(buckets, info, infos);
        ecartes = perdus;
        out.insert("buckets".into(), rendus);
    }

    for (cle, v) in obj {
        if cle == "buckets" {
            continue;
        }
        if cle == "sum_other_doc_count" {
            out.insert(cle.clone(), json!(v.as_u64().unwrap_or(0) + ecartes));
            continue;
        }
        // Metrique sur un champ date : tantivy rend des nanosecondes, ES des
        // millisecondes, et y ajoute la forme lisible.
        if info.date && METRIQUES_DATE.contains(&cle.as_str()) {
            if let Some(nanos) = v.as_f64() {
                let millis = nanos / 1_000_000.0;
                out.insert(cle.clone(), json!(millis));
                if let Some(texte) = rend_date(millis, info) {
                    out.insert(format!("{cle}_as_string"), json!(texte));
                }
                continue;
            }
        }
        out.insert(cle.clone(), v.clone());
    }

    if info.type_agg == "terms" {
        out.entry("doc_count_error_upper_bound".to_string())
            .or_insert_with(|| json!(0));
        out.entry("sum_other_doc_count".to_string())
            .or_insert_with(|| json!(ecartes));
    }
    Value::Object(out)
}

/// Rend les buckets mis en forme, et le nombre de documents portes par ceux que
/// la troncature a ecartes.
fn mise_en_forme_buckets(
    buckets: &Value,
    info: &Info,
    infos: &HashMap<String, Info>,
) -> (Value, u64) {
    match buckets {
        Value::Array(a) => {
            let mut liste: Vec<Value> = a
                .iter()
                .map(|b| mise_en_forme_bucket(b, info, infos))
                .collect();
            let mut ecartes = 0u64;
            if info.type_agg == "terms" {
                trier_terms(&mut liste, info.ordre);
                if let Some(size) = info.size {
                    ecartes = liste
                        .iter()
                        .skip(size)
                        .filter_map(|b| b.get("doc_count").and_then(Value::as_u64))
                        .sum();
                    liste.truncate(size);
                }
            }
            (Value::Array(liste), ecartes)
        }
        // Forme `keyed` : un objet de buckets, dont l'ordre n'a pas de sens.
        Value::Object(o) => (
            Value::Object(
                o.iter()
                    .map(|(k, b)| (k.clone(), mise_en_forme_bucket(b, info, infos)))
                    .collect(),
            ),
            0,
        ),
        autre => (autre.clone(), 0),
    }
}

/// L'ordre d'ES : le critere demande, puis **la cle croissante** pour
/// departager les ex aequo. tantivy ne departage pas, d'ou des selections
/// differentes au bord de la troncature.
fn trier_terms(buckets: &mut [Value], ordre: Ordre) {
    fn compte(b: &Value) -> u64 {
        b.get("doc_count").and_then(Value::as_u64).unwrap_or(0)
    }
    fn cle(b: &Value) -> (u8, f64, String) {
        match b.get("key") {
            Some(Value::String(s)) => (1, 0.0, s.clone()),
            Some(Value::Number(n)) => (0, n.as_f64().unwrap_or(0.0), String::new()),
            Some(Value::Bool(x)) => (0, f64::from(u8::from(*x)), String::new()),
            _ => (2, 0.0, String::new()),
        }
    }
    fn cmp_cle(a: &Value, b: &Value) -> std::cmp::Ordering {
        let (ta, na, sa) = cle(a);
        let (tb, nb, sb) = cle(b);
        ta.cmp(&tb).then(na.total_cmp(&nb)).then(sa.cmp(&sb))
    }
    buckets.sort_by(|a, b| match ordre {
        Ordre::CountDesc => compte(b).cmp(&compte(a)).then(cmp_cle(a, b)),
        Ordre::CountAsc => compte(a).cmp(&compte(b)).then(cmp_cle(a, b)),
        Ordre::KeyAsc => cmp_cle(a, b),
        Ordre::KeyDesc => cmp_cle(b, a),
    });
}

fn mise_en_forme_bucket(bucket: &Value, info: &Info, infos: &HashMap<String, Info>) -> Value {
    let Some(obj) = bucket.as_object() else {
        return bucket.clone();
    };
    let mut out = Map::new();
    for (cle, v) in obj {
        match cle.as_str() {
            "key" if info.type_agg == "date_histogram" => {
                // ES rend un entier de millisecondes, tantivy un flottant.
                let millis = v.as_f64().unwrap_or(0.0);
                out.insert("key".into(), json!(millis as i64));
                if let Some(texte) = rend_date(millis, info) {
                    out.insert("key_as_string".into(), json!(texte));
                }
            }
            // Deja pose juste au-dessus, au bon format.
            "key_as_string" if info.type_agg == "date_histogram" => {}
            "key" if info.type_agg == "range" => {
                out.insert("key".into(), json!(cle_de_range(bucket, v)));
            }
            "doc_count" | "key" | "key_as_string" | "from" | "to" | "from_as_string"
            | "to_as_string" => {
                out.insert(cle.clone(), v.clone());
            }
            // Tout le reste est une sous-agregation.
            autre => {
                let vide = Info {
                    type_agg: String::new(),
                    date: false,
                    format: None,
                    size: None,
                    ordre: Ordre::CountDesc,
                };
                let sous = infos.get(autre).unwrap_or(&vide);
                out.insert(cle.clone(), mise_en_forme_une(v, sous, infos));
            }
        }
    }
    Value::Object(out)
}

/// ES nomme les buckets d'un `range` avec des bornes flottantes : `*-100.0`,
/// `100.0-500.0`, `500.0-*`. tantivy rend `*-100`. Une cle explicite fournie
/// par le client est laissee telle quelle.
fn cle_de_range(bucket: &Value, cle_tantivy: &Value) -> String {
    let borne = |nom: &str| -> Option<f64> { bucket.get(nom).and_then(Value::as_f64) };
    let (from, to) = (borne("from"), borne("to"));
    if from.is_none() && to.is_none() {
        return cle_tantivy.as_str().unwrap_or_default().to_string();
    }
    // Une cle nommee ne contient ni `-` genere ni `*` : on la reconnait au fait
    // qu'elle ne correspond pas a la forme generee par tantivy.
    let generee = format!(
        "{}-{}",
        from.map(trim_zero).unwrap_or_else(|| "*".into()),
        to.map(trim_zero).unwrap_or_else(|| "*".into())
    );
    if cle_tantivy.as_str() != Some(generee.as_str()) {
        return cle_tantivy.as_str().unwrap_or_default().to_string();
    }
    format!(
        "{}-{}",
        from.map(|f| format!("{f:?}")).unwrap_or_else(|| "*".into()),
        to.map(|f| format!("{f:?}")).unwrap_or_else(|| "*".into())
    )
}

/// La forme que tantivy donne a une borne : entiere quand elle est ronde.
fn trim_zero(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}
