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
use crate::mapping::{FieldKind, FieldType, Fields, MappedField, TypedValue};

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
    /// Le `shard_size` demande sur un `terms` : au-dela, ES annonce ne pas
    /// savoir borner l'erreur de comptage.
    shard_size: Option<usize>,
    /// Le champ agrege, quand il y en a un.
    champ: Option<String>,
    /// Le champ agrege est-il un `float` ou un `double` ? Les cles d'un `terms`
    /// s'y rendent **avec leur decimale** : ES ecrit `2.0`, tantivy `2`. Une
    /// valeur entiere dans un champ flottant suffit a les separer, et un client
    /// qui type strictement son JSON y lit un entier la ou ES lui donne un
    /// flottant.
    flottant: bool,
    /// Les intervalles demandes sur un `range`.
    ///
    /// tantivy **comble les trous** : il rend un bucket pour chaque intervalle
    /// entre deux bornes demandees, plus un a chaque extremite. ES ne rend que
    /// ce qu'on lui a demande. On garde donc la demande pour ecarter les
    /// buckets que personne n'a reclames.
    ranges: Vec<Borne>,
}

impl Info {
    /// Une agregation dont on ne sait rien : ce que rend une sous-cle qu'on ne
    /// reconnait pas.
    fn vide() -> Self {
        Self {
            type_agg: String::new(),
            date: false,
            format: None,
            size: None,
            ordre: Ordre::CountDesc,
            shard_size: None,
            champ: None,
            flottant: false,
            ranges: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ordre {
    CountDesc,
    CountAsc,
    KeyAsc,
    KeyDesc,
    /// Classer les seaux par la valeur d'une **sous-agregation** metrique.
    ///
    /// `prop` est vide pour une metrique a valeur unique (`avg`, `sum`,
    /// `value_count`...) et nomme la valeur voulue pour une metrique
    /// multi-valuee (`prix.avg` sur un `stats`). `metrique` est le type de la
    /// sous-agregation : c'est lui qui dit ou classer un seau dont la metrique
    /// n'a **aucune** valeur (voir [`valeur_absente`]).
    SousAgg {
        agg: String,
        prop: String,
        metrique: String,
        asc: bool,
    },
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
        "terms" => &[
            "field",
            "size",
            "shard_size",
            "min_doc_count",
            "order",
            "missing",
            "include",
            "exclude",
        ],
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
        _ => return None,
    })
}

/// Les requetes internes des agregations `filter`, rangees par **chemin**
/// d'agregation (`etats>en_retard`).
///
/// Elles sont construites dans la generation de l'index, comme la requete
/// principale : leurs `Field` n'ont de sens que la. Elles voyagent donc avec la
/// cible, pas avec la demande.
pub type Filtres = HashMap<String, Box<dyn Query>>;

/// Le separateur des chemins d'agregation.
const SEP: char = '>';

/// Verifie une demande d'agregations de bout en bout, et construit au passage
/// les requetes des agregations `filter`.
pub fn validate(
    aggs: &Value,
    champs: Option<&Fields>,
    ctx: &crate::dsl::QueryCtx,
) -> EsResult<Filtres> {
    let mut filtres = Filtres::default();
    validate_niveau(aggs, champs, ctx, "", true, &mut filtres)?;
    Ok(filtres)
}

fn validate_niveau(
    aggs: &Value,
    champs: Option<&Fields>,
    ctx: &crate::dsl::QueryCtx,
    chemin: &str,
    filtre_possible: bool,
    filtres: &mut Filtres,
) -> EsResult<()> {
    let obj = aggs
        .as_object()
        .ok_or_else(|| EsError::parsing("[aggs] doit etre un objet"))?;
    if obj.is_empty() {
        return Err(EsError::illegal_argument("[aggs] ne peut pas etre vide"));
    }
    for (nom, corps) in obj {
        let sous_chemin = if chemin.is_empty() {
            nom.clone()
        } else {
            format!("{chemin}{SEP}{nom}")
        };
        validate_une(
            nom,
            &sous_chemin,
            corps,
            champs,
            ctx,
            filtre_possible,
            filtres,
        )?;
    }
    Ok(())
}

fn validate_une(
    nom: &str,
    chemin: &str,
    corps: &Value,
    champs: Option<&Fields>,
    ctx: &crate::dsl::QueryCtx,
    filtre_possible: bool,
    filtres: &mut Filtres,
) -> EsResult<()> {
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

    // `filter` n'est pas une agregation de tantivy : c'est ferrite qui
    // l'execute, en croisant la requete de la recherche avec celle du filtre
    // (voir `run`). Sa requete se construit donc ici, dans la generation de
    // l'index, et se range sous le chemin de l'agregation.
    if type_agg == "filter" {
        if !filtre_possible {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte l'agregation [filter] (ici [aggs.{nom}]) qu'au premier \
                 niveau, ou sous une autre [filter] : sous une agregation de buckets, elle \
                 exigerait de re-executer sa requete bucket par bucket (voir docs/compat.md)"
            )));
        }
        let filtre = crate::dsl::build_query(corps_agg, ctx)?;
        filtres.insert(chemin.to_string(), filtre);
        if let Some(sous) = sous {
            validate_niveau(sous, champs, ctx, chemin, true, filtres)?;
        }
        return Ok(());
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
        verifier_champ(nom, type_agg, champ, champs)?;
    }

    if type_agg == "terms" {
        if let Some(order) = corps_agg.get("order") {
            lire_ordre(order, nom, sous)?;
        }
        let champ = corps_agg.get("field").and_then(Value::as_str).unwrap_or("");
        if let Some(v) = corps_agg.get("missing") {
            verifier_missing(nom, champ, v, champs)?;
        }
        for param in ["include", "exclude"] {
            if let Some(v) = corps_agg.get(param) {
                verifier_filtre_termes(nom, param, champ, v, champs)?;
                // Le seau de remplissage de `missing` disparait des que
                // l'agregation de tantivy filtre les termes : son identifiant
                // de terme n'est pas dans le dictionnaire, donc il n'est dans
                // aucun des ensembles autorises que `include` / `exclude`
                // construisent. Chez ES, ce seau est un terme comme un autre —
                // il reste sous un `exclude` qui ne le vise pas, et il sort
                // sous un `include` qui le nomme (mesure : `include:
                // ["alpha", "(vide)"]` rend deux seaux chez ES, un seul ici).
                // Perdre en silence les documents sans valeur est exactement ce
                // qu'une facette ne doit pas faire.
                if corps_agg.get("missing").is_some() {
                    return Err(EsError::unsupported(format!(
                        "ferrite ne supporte pas [{param}] et [missing] sur la meme agregation \
                         [terms] (agregation [{nom}]) : l'agregation de tantivy filtre les termes \
                         par leur identifiant dans le dictionnaire, que le seau de remplissage \
                         n'a pas — il disparaitrait de la reponse, en 200, alors qu'Elasticsearch \
                         le traite comme un terme ordinaire (voir docs/compat.md)"
                    )));
                }
            }
        }
        // `min_doc_count: 0` demande un bucket pour les valeurs que la
        // recherche n'a **pas** trouvees. tantivy ne le rend pas de facon
        // fiable : zero bucket sur une colonne numerique, zero bucket quand la
        // requete ne ramene rien, et des buckets vides prives de leurs
        // sous-agregations. Trois formes du meme resultat faux, sans un mot —
        // d'ou le refus explicite. Au-dela de 1, il est applique par ferrite
        // lui-meme (voir `mise_en_forme_buckets`).
        // `min_doc_count` ne se reproduit fidelement dans aucune de ses deux
        // moities, et les deux echouent en silence.
        //
        // `0` demande un bucket pour les valeurs que la recherche n'a **pas**
        // trouvees : tantivy en rend zero sur une colonne numerique, zero quand
        // la requete ne ramene rien, et des buckets vides prives de leurs
        // sous-agregations.
        //
        // Au-dela de `1`, c'est `sum_other_doc_count` qui ne suit plus. La regle
        // d'ES a ete cherchee pour de bon : une formule ajustee sur quinze
        // formes d'un corpus en collait quinze, puis s'est effondree sur
        // d'autres corpus (27 ecarts sur 1 450 cas tires au sort). Elle depend
        // de l'ordre demande, de la troncature, et de l'ordre de parcours du
        // dictionnaire de termes — c'est le collecteur d'ES qu'il faudrait
        // reecrire. Annoncer un compte faux serait pire que refuser.
        if let Some(n) = corps_agg
            .get("min_doc_count")
            .and_then(Value::as_u64)
            .filter(|n| *n != 1)
        {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [min_doc_count: {n}] dans [terms] (agregation [{nom}]) ; \
                 seule sa valeur par defaut [1] est reproduite a l'identique : a [0] \
                 l'agregation de tantivy rendrait moins de buckets qu'Elasticsearch, et au-dela \
                 c'est [sum_other_doc_count] qui differerait — dans les deux cas sans le dire \
                 (voir docs/compat.md)"
            )));
        }
    }

    if type_agg == "range" {
        let champ = corps_agg.get("field").and_then(Value::as_str).unwrap_or("");
        let format = champs.and_then(|c| {
            (c.get(champ).map(|m| m.ty.kind()) == Some(FieldKind::Date))
                .then(|| c.format_ou_defaut(champ))
        });
        verifier_ranges(nom, corps_agg, format)?;
    }

    if let Some(sous) = sous {
        if !est_bucket(type_agg) {
            return Err(EsError::illegal_argument(format!(
                "[aggs.{nom}] : l'agregation [{type_agg}] ne peut pas porter de \
                 sous-agregations (ce n'est pas une agregation de buckets)"
            )));
        }
        validate_niveau(sous, champs, ctx, chemin, false, filtres)?;
    }
    Ok(())
}

/// Un intervalle demande dans un `range` : ses deux bornes, plus le nom que le
/// client lui a donne.
///
/// `to` est **exclu** et `from` **inclus**, des deux cotes. Garder le nom
/// demande evite d'avoir a deviner, dans le resultat, si la cle rendue par
/// tantivy est celle du client ou une cle generee — et les deux ne se
/// distinguent pas de facon fiable sur un champ date.
#[derive(Debug, Clone, PartialEq)]
struct Borne {
    from: Option<f64>,
    to: Option<f64>,
    nom: Option<String>,
}

/// Les bornes demandees, dans l'ordre du client.
///
/// Sur un champ `date`, une borne n'est pas un nombre : c'est une date, ecrite
/// au format du champ (`"2026-01-03"`) ou en millisecondes. tantivy, lui,
/// compte les dates en **nanosecondes** et n'accepte qu'un flottant : lui
/// passer la borne telle quelle lisait `1767398400000` comme 29 minutes apres
/// l'epoque et rendait des buckets vides **sans rien dire**. Les bornes d'un
/// champ date sont donc lues ici, puis rendues en nanosecondes.
fn lire_ranges(
    corps_agg: &Value,
    format: Option<&crate::dateformat::DateFormat>,
) -> EsResult<Vec<Borne>> {
    let Some(a) = corps_agg.get("ranges").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(a.len());
    for r in a {
        let lire = |nom: &str| -> EsResult<Option<f64>> {
            let Some(v) = r.get(nom) else {
                return Ok(None);
            };
            if v.is_null() {
                return Ok(None);
            }
            match format {
                // Une borne de date se resout comme celle d'un `range` du Query
                // DSL, arrondie **vers le bas** : `to: "2026-01-03"` est le
                // debut de la journee chez ES, pas sa fin.
                Some(f) => {
                    let ms = crate::datemath::borne(
                        v,
                        f,
                        crate::datemath::maintenant(),
                        crate::datemath::Arrondi::Bas,
                    )?;
                    Ok(Some(ms as f64))
                }
                None => v.as_f64().map(Some).ok_or_else(|| {
                    EsError::illegal_argument(format!(
                        "[range.ranges] : borne [{nom}] illisible ({v})"
                    ))
                }),
            }
        };
        out.push(Borne {
            from: lire("from")?,
            to: lire("to")?,
            nom: r.get("key").and_then(Value::as_str).map(str::to_string),
        });
    }
    Ok(out)
}

/// Une borne de date, en nanosecondes : l'unite dans laquelle tantivy compte.
const NANOS: f64 = 1_000_000.0;

/// Les intervalles d'un `range` se **chevauchent**-ils ?
///
/// Elasticsearch les accepte et compte alors un document dans chaque bucket qui
/// le contient. L'agregation de tantivy, elle, partitionne : elle exige des
/// intervalles disjoints et refuse le reste avec un message qui parle de ses
/// propres structures. Le refus est donc prononce ici, explicitement, plutot
/// que de laisser fuir une erreur interne — et il est declare dans
/// `docs/compat.md`.
fn verifier_ranges(
    nom: &str,
    corps_agg: &Value,
    format: Option<&crate::dateformat::DateFormat>,
) -> EsResult<()> {
    let date = format.is_some();
    let mut bornes = lire_ranges(corps_agg, format)?;
    if bornes.is_empty() {
        return Err(EsError::illegal_argument(format!(
            "[aggs.{nom}.range] : [ranges] est obligatoire et ne peut pas etre vide"
        )));
    }
    bornes.sort_by(|a, b| {
        a.from
            .unwrap_or(f64::NEG_INFINITY)
            .total_cmp(&b.from.unwrap_or(f64::NEG_INFINITY))
    });
    for paire in bornes.windows(2) {
        let (fin, debut) = (paire[0].to, paire[1].from);
        let fin = fin.unwrap_or(f64::INFINITY);
        let debut = debut.unwrap_or(f64::NEG_INFINITY);
        if debut < fin {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas des intervalles qui se chevauchent dans \
                 [aggs.{nom}.range] : l'agregation de tantivy partitionne les valeurs, elle ne \
                 peut pas compter un document dans deux buckets (voir docs/compat.md)"
            )));
        }
        // Un **trou** entre deux intervalles se comble chez tantivy, et ferrite
        // ecarte ensuite le bucket de remplissage. Sur un champ date, ou les
        // bornes passent en nanosecondes, ce remplissage avale l'intervalle
        // suivant : le bucket demande n'existe plus, et il manquerait en
        // silence. Sur un numerique, les deux buckets sortent bien, et le
        // filtrage suffit.
        if date && debut > fin {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas un **trou** entre deux intervalles d'un \
                 [aggs.{nom}.range] sur un champ date : l'agregation de tantivy comble les \
                 trous, et sur une date le bucket de remplissage avale l'intervalle demande \
                 (voir docs/compat.md)"
            )));
        }
    }
    Ok(())
}

/// L'ordre d'un `terms` : `_count`, `_key`, ou le chemin d'une **sous-agregation**
/// metrique.
///
/// Le sens se lit sans egard a la casse : ES accepte `"DESC"` autant que
/// `"desc"` (mesure contre ES 8.15), la sienne etant lue par un
/// `equalsIgnoreCase`.
///
/// `sous` porte les sous-agregations declarees sous ce `terms` : c'est la que
/// se resout un chemin comme `prix_moyen` ou `stats_prix.avg`, et c'est la
/// seule facon de distinguer une metrique a valeur unique d'une metrique
/// multi-valuee — ES les traite differemment et refuse chacune sous la forme de
/// l'autre.
fn lire_ordre(order: &Value, nom: &str, sous: Option<&Value>) -> EsResult<Ordre> {
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
    let asc = if sens.eq_ignore_ascii_case("asc") {
        true
    } else if sens.eq_ignore_ascii_case("desc") {
        false
    } else {
        return Err(EsError::illegal_argument(format!(
            "[aggs.{nom}.order] : sens [{sens}] invalide (asc|desc)"
        )));
    };
    match cle.as_str() {
        "_count" => Ok(if asc {
            Ordre::CountAsc
        } else {
            Ordre::CountDesc
        }),
        "_key" => Ok(if asc { Ordre::KeyAsc } else { Ordre::KeyDesc }),
        chemin => lire_ordre_sous_agg(chemin, asc, nom, sous),
    }
}

/// Les proprietes qu'ES accepte au bout du chemin d'un `stats` — les cinq
/// valeurs qu'il rend, et rien d'autre : `s.variance` echoue en 400 (mesure).
const PROPRIETES_STATS: &[&str] = &["count", "min", "max", "avg", "sum"];

/// Les metriques a **valeur unique** : leur chemin d'ordre est leur nom nu.
const METRIQUES_SIMPLES: &[&str] = &["min", "max", "sum", "avg", "value_count"];

/// Un chemin d'ordre qui n'est ni `_count` ni `_key` designe une
/// sous-agregation metrique de **ce** `terms`.
///
/// Quatre des refus reproduisent ceux d'un vrai ES 8.15, mesures un par un
/// (`Invalid aggregation order path [...]`) : une agregation absente, une
/// agregation de **seaux**, une metrique multi-valuee sans propriete, et une
/// propriete que la metrique ne rend pas. Les deux autres — le chemin a
/// plusieurs niveaux et l'agregation **mono-seau** — sont des couts de
/// perimetre : ES les sert. Ils portent donc l'autre type d'erreur, et c'est
/// lui que le rapport de conformance lit.
fn lire_ordre_sous_agg(
    chemin: &str,
    asc: bool,
    nom: &str,
    sous: Option<&Value>,
) -> EsResult<Ordre> {
    // ES sait descendre a travers une agregation mono-seau (`filtre>prix`).
    // ferrite n'a pas d'agregation mono-seau sous un `terms` — `filter` y est
    // deja refusee — donc le chemin ne mene nulle part : le dire vaut mieux que
    // de le lire comme un nom d'agregation qui n'existe pas.
    if chemin.contains(SEP) {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas un chemin d'ordre a plusieurs niveaux [{chemin}] dans \
             [aggs.{nom}.order] : il traverserait une agregation mono-seau, et la seule \
             d'Elasticsearch que ferrite serve ([filter]) est deja refusee sous une agregation \
             de seaux (voir docs/compat.md)"
        )));
    }
    let (agg, prop) = chemin.split_once('.').unwrap_or((chemin, ""));
    let corps = sous
        .and_then(Value::as_object)
        .and_then(|o| o.get(agg))
        .ok_or_else(|| {
            EsError::illegal_argument(format!(
                "Invalid aggregation order path [{chemin}]. Cannot find aggregation named [{agg}] \
                 (dans [aggs.{nom}])"
            ))
        })?;
    let type_sous = type_de(corps).unwrap_or("");
    if METRIQUES_SIMPLES.contains(&type_sous) {
        // Une metrique a valeur unique accepte `value` — le nom de la seule
        // valeur qu'elle rende — et rien d'autre (mesure : `pm.value` passe,
        // `pm.count` echoue en 400).
        if !prop.is_empty() && prop != "value" {
            return Err(EsError::illegal_argument(format!(
                "Invalid aggregation order path [{chemin}]. Unknown value key [{prop}] for \
                 single-value metric aggregation [{agg}]. Either use [value] as key or drop the \
                 key all together (dans [aggs.{nom}])"
            )));
        }
    } else if type_sous == "stats" {
        if prop.is_empty() {
            return Err(EsError::illegal_argument(format!(
                "Invalid aggregation order path [{chemin}]. Missing value key in [null] which \
                 refers to a multi-value metric aggregation (dans [aggs.{nom}])"
            )));
        }
        if !PROPRIETES_STATS.contains(&prop) {
            return Err(EsError::illegal_argument(format!(
                "Invalid aggregation order path [{chemin}]. [{prop}] n'est pas une valeur de \
                 [stats] ; valeurs rendues : {PROPRIETES_STATS:?} (dans [aggs.{nom}])"
            )));
        }
    } else if type_sous == "filter" {
        // Une agregation **mono-seau** est une cle d'ordre valable chez ES : il
        // classe alors sur son `doc_count`, que le chemin le nomme (`h.doc_count`)
        // ou non (`h`) — mesure contre ES 8.15. La seule que ferrite serve,
        // `filter`, est deja refusee sous une agregation de seaux : le chemin
        // ne mene donc nulle part, et c'est un cout de perimetre, pas une
        // demande invalide. Les deux se distinguent par le type de l'erreur,
        // et c'est lui que le rapport de conformance lit.
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas l'ordre par l'agregation [filter] [{agg}] (dans \
             [aggs.{nom}.order]) : Elasticsearch classe alors les seaux sur son [doc_count], \
             mais ferrite refuse deja [filter] sous une agregation de seaux — il faudrait \
             rejouer sa requete seau par seau (voir docs/compat.md)"
        )));
    } else {
        // Une agregation de **seaux** n'est pas une cle d'ordre : ES refuse
        // aussi, avec le meme statut.
        return Err(EsError::illegal_argument(format!(
            "Invalid aggregation order path [{chemin}]. Can't sort a [{type_sous}] aggregation \
             [{agg}] (dans [aggs.{nom}])"
        )));
    }
    Ok(Ordre::SousAgg {
        agg: agg.to_string(),
        prop: prop.to_string(),
        metrique: type_sous.to_string(),
        asc,
    })
}

/// Ou se classe un seau dont la metrique n'a **aucune** valeur.
///
/// Il n'y a pas de reponse unique, et ce n'est pas devinable : ES ne compare
/// pas ce qu'il **affiche** (`null` partout) mais ce que sa metrique rend en
/// interne quand elle est vide. Mesure contre ES 8.15, sur un corpus ou la
/// plupart des categories n'ont aucun prix :
///
/// | metrique | vide vaut | ou elle se classe |
/// |---|---|---|
/// | `avg`, `stats.avg` | `NaN` (`0/0`) | en tete d'un `desc`, en queue d'un `asc` |
/// | `min`, `stats.min` | `+Infinity` | comme ci-dessus |
/// | `max`, `stats.max` | `-Infinity` | **l'inverse** : en queue d'un `desc` |
/// | `sum`, `value_count`, `stats.count`, `stats.sum` | `0` | avec les autres zeros |
///
/// Le `Double.compare` de Java classe `NaN` **au-dessus** de tout, `+Infinity`
/// compris ; le `total_cmp` de Rust fait de meme pour un `NaN` positif. Prendre
/// `null` pour un seul et meme « absent » rendait les seaux vides en tete d'un
/// `order: {stats.max: desc}` la ou ES les met en queue.
fn valeur_absente(metrique: &str, prop: &str) -> f64 {
    // Une metrique a valeur unique s'ecrit `pm` ou `pm.value` : les deux
    // designent la meme valeur, donc le meme « absent ».
    let prop = if METRIQUES_SIMPLES.contains(&metrique) {
        ""
    } else {
        prop
    };
    match (metrique, prop) {
        ("min", "") | ("stats", "min") => f64::INFINITY,
        ("max", "") | ("stats", "max") => f64::NEG_INFINITY,
        _ => f64::NAN,
    }
}

/// L'ordre, reecrit tel que tantivy le lit.
///
/// Deux raisons de ne pas transmettre celui du client tel quel : il accepte
/// `"DESC"` comme ES et tantivy non, et c'est le seul endroit ou la forme
/// envoyee est certaine d'avoir ete validee.
fn ordre_pour_tantivy(ordre: &Ordre) -> Value {
    let (cible, sens) = match ordre {
        Ordre::CountDesc => ("_count".to_string(), "desc"),
        Ordre::CountAsc => ("_count".to_string(), "asc"),
        Ordre::KeyAsc => ("_key".to_string(), "asc"),
        Ordre::KeyDesc => ("_key".to_string(), "desc"),
        Ordre::SousAgg { agg, prop, asc, .. } => (
            if prop.is_empty() {
                agg.clone()
            } else {
                format!("{agg}.{prop}")
            },
            if *asc { "asc" } else { "desc" },
        ),
    };
    json!({ cible: sens })
}

/// `include` / `exclude` sur un `terms` : ne garder (ou ecarter) que les termes
/// qui correspondent a une expression reguliere ou qui figurent dans une liste
/// exacte. C'est ce qui separe une agregation `terms` d'une vraie facette de
/// catalogue.
///
/// Trois formes chez ES, et les trois ne se valent pas ici :
///
/// - **une expression reguliere** (une chaine), dans la syntaxe de Lucene et
///   ancree sur le terme entier (mesure : `include: "a"` ne rend rien sur un
///   terme `alpha`, `^alpha$` non plus puisque `^` et `$` y sont des
///   litteraux). Elle est traduite par [`crate::regexp`], comme celle d'une
///   requete `regexp` — donc avec les memes quatre operateurs refuses
///   (`~`, `&`, `<n-m>`, `#`). ES la refuse sur un champ qui n'est pas textuel,
///   et ferrite reprend son message ;
/// - **une liste exacte de valeurs**. Sur un `keyword`, tantivy la sert ;
/// - **une partition** (`{"partition": n, "num_partitions": m}`), refusee : voir
///   ci-dessous.
///
/// Le refus qui compte est celui du champ **non textuel** : l'agregation de
/// tantivy n'applique `include` / `exclude` qu'a une colonne de chaines, et
/// **ecarte la colonne entiere** quand elle ne l'est pas (`continue` dans
/// `agg_data.rs`). Un `include: [1, 3]` sur un `long` rendrait donc zero seau
/// la ou ES en rend deux — un resultat faux, en 200, sur une demande qu'ES
/// sert.
fn verifier_filtre_termes(
    nom: &str,
    param: &str,
    champ: &str,
    v: &Value,
    champs: Option<&Fields>,
) -> EsResult<()> {
    // `None` quand la recherche ne vise aucun index : la **forme** se verifie
    // quand meme, seul le type du champ reste indecidable.
    let kind = champs.and_then(|c| c.get(champ)).map(|m| m.ty.kind());
    let textuel = kind.is_none_or(|k| k == FieldKind::Keyword);
    match v {
        Value::String(motif) => {
            if !textuel {
                // Le message d'ES, mot pour mot : c'est lui qui dit au client
                // quelle forme employer a la place.
                return Err(EsError::illegal_argument(format!(
                    "Aggregation [{nom}] cannot support regular expression style include/exclude \
                     settings as they can only be applied to string fields. Use an array of \
                     numeric values for include/exclude clauses used to filter numeric fields"
                )));
            }
            // Traduit ici pour que les operateurs refuses le soient au meme
            // endroit que dans une requete `regexp`, et avant toute execution.
            // Le type de l'erreur est conserve : un `~` reste un refus declare,
            // pas un argument illegal.
            crate::regexp::vers_regex(motif, crate::regexp::Flags::default(), false).map_err(
                |mut e| {
                    e.reason = format!("[aggs.{nom}.terms.{param}] : {}", e.reason);
                    e
                },
            )?;
            Ok(())
        }
        Value::Array(valeurs) => {
            for x in valeurs {
                if !matches!(x, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
                    return Err(EsError::illegal_argument(format!(
                        "[aggs.{nom}.terms.{param}] : [{x}] n'est pas une valeur simple ; une \
                         liste d'inclusion ne contient que des valeurs de terme"
                    )));
                }
            }
            if !textuel {
                let ty = champs
                    .and_then(|c| c.get(champ))
                    .map_or("?", |m| m.ty.name());
                return Err(EsError::unsupported(format!(
                    "ferrite ne filtre les termes d'un [terms] que sur un champ de chaines : \
                     [{param}] est ici pose sur [{champ}], de type [{ty}] (agregation [{nom}]). \
                     L'agregation de tantivy **ecarte la colonne entiere** des qu'elle n'est pas \
                     textuelle — elle rendrait zero seau la ou Elasticsearch en rend, en 200 et \
                     sans un mot (voir docs/compat.md)"
                )));
            }
            Ok(())
        }
        Value::Object(o) => {
            if o.contains_key("partition") || o.contains_key("num_partitions") {
                Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas la forme partitionnee de [{param}] (agregation \
                     [{nom}]) : elle retient un terme selon un **hachage** de sa valeur \
                     (murmur3 x86_32, graine 31, mesure contre ES 8.15 et stable a son \
                     redemarrage). Ni le filtre par expression reguliere de tantivy ni sa liste \
                     exacte ne savent l'exprimer : il faudrait enumerer tout le dictionnaire de \
                     termes pour en dresser la liste, ce qui defait la raison meme du parametre \
                     (voir docs/compat.md)"
                )))
            } else {
                Err(EsError::illegal_argument(format!(
                    "[aggs.{nom}.terms.{param}] : un objet n'est accepte que sous la forme \
                     partitionnee ([partition], [num_partitions])"
                )))
            }
        }
        autre => Err(EsError::illegal_argument(format!(
            "[aggs.{nom}.terms.{param}] : [{autre}] illisible ; attendu une expression \
             reguliere, une liste de valeurs, ou une partition"
        ))),
    }
}

/// `include` / `exclude` sous la forme que tantivy lit : une chaine est un
/// motif traduit en syntaxe `regex`, une liste devient une liste de chaines.
///
/// ES lit chaque element d'une liste **comme du texte** : `include: [1, 2]`
/// cherche les termes `"1"` et `"2"` (mesure). La conversion est donc faite
/// ici, sans quoi tantivy refuserait le nombre a la deserialisation.
fn filtre_pour_tantivy(v: &Value) -> Option<Value> {
    match v {
        Value::String(motif) => {
            crate::regexp::vers_regex(motif, crate::regexp::Flags::default(), false)
                .ok()
                .map(Value::String)
        }
        Value::Array(valeurs) => Some(Value::Array(
            valeurs
                .iter()
                .map(|x| match x {
                    Value::String(s) => json!(s),
                    autre => json!(autre.to_string()),
                })
                .collect(),
        )),
        _ => None,
    }
}

/// Une agregation lit un fast field : tous les types en ont un sauf `text`.
///
/// C'est aussi la regle d'ES, qui refuse d'agreger un `text` sans `fielddata`.
///
/// `champs` vaut `None` quand la recherche ne vise **aucun** index : le type du
/// champ ne se prononce alors pas (ES non plus, qui rend 200), mais le reste de
/// l'agregation — et surtout ses sous-agregations — continue d'etre lu.
/// `missing` sur un `terms` : ranger les documents **sans valeur** sous une cle
/// choisie. C'est ce qu'une facette affiche comme « non renseigne ».
///
/// tantivy sait le faire, et c'est une agregation deleguee de plus : ses bords
/// ne sont pas ceux de son homonyme, et les ecarts mesures contre ES 8.15 sont
/// **silencieux**, pas bruyants.
///
/// | Champ / valeur | ES | tantivy |
/// |---|---|---|
/// | `date`, `missing: "2020-01-01"` | le bucket de cette date | le bucket de **1970-01-01** |
/// | `long`, `missing: "3"` | la cle `3` | la cle `"3"` |
/// | `keyword`, `missing: 42` | la cle `"42"` | la cle `42` |
/// | `double`, `missing: 0` | la cle `0.0` | la cle `0` |
/// | `boolean` | le bucket `false` | une erreur de deserialisation |
///
/// La valeur est donc ramenee au type du champ avant d'etre passee (voir
/// [`normaliser_missing`]), et les deux types que tantivy ne sait pas poser
/// sont refuses. Un bucket de remplissage place au mauvais endroit se lit
/// comme une donnee.
fn verifier_missing(nom: &str, champ: &str, v: &Value, champs: Option<&Fields>) -> EsResult<()> {
    let Some(champs) = champs else {
        return Ok(());
    };
    let Some(MappedField { ty, .. }) = champs.get(champ) else {
        return Ok(());
    };
    let refus = |raison: &str| {
        Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [missing: {v}] sur le champ [{champ}] de type [{}] \
             (agregation [{nom}]) ; {raison} (voir docs/compat.md)",
            ty.name()
        )))
    };
    match ty.kind() {
        // Ceux-la se ramenent au type du champ (voir `normaliser_missing`).
        FieldKind::Keyword | FieldKind::I64 | FieldKind::F64 => match normaliser_missing(ty, v) {
            Some(_) => Ok(()),
            None => refus(
                "la valeur ne se lit pas au type du champ ; Elasticsearch echoue aussi sur cette \
                 demande",
            ),
        },
        FieldKind::Date => refus(
            "tantivy ne lit pas la date et rangerait ces documents sous [1970-01-01], en 200 et \
             sans un mot",
        ),
        FieldKind::Bool | FieldKind::Text => {
            refus("tantivy ne sait pas poser de valeur de remplissage sur ce type de colonne")
        }
    }
}

/// La valeur de `missing` ramenee au type du champ, comme le fait ES.
///
/// `None` quand elle ne s'y lit pas — ES echoue alors aussi.
fn normaliser_missing(ty: FieldType, v: &Value) -> Option<Value> {
    match crate::mapping::coerce("missing", ty, v).ok()? {
        TypedValue::Str(s) => Some(json!(s)),
        TypedValue::I64(n) => Some(json!(n)),
        // Toujours avec sa decimale : ES rend la cle `0.0` sur un `double`, et
        // `json!(0.0_f64)` l'ecrit ainsi la ou `json!(0)` ecrirait `0`.
        TypedValue::F64(n) => Some(json!(n)),
        TypedValue::Bool(_) | TypedValue::Date(_) => None,
    }
}

fn verifier_champ(nom: &str, type_agg: &str, champ: &str, champs: Option<&Fields>) -> EsResult<()> {
    let Some(champs) = champs else {
        return Ok(());
    };

    // Un sous-champ de `nested` agrege depuis la racine.
    //
    // Chez ES, ces valeurs vivent dans des documents caches : au niveau racine
    // il n'en voit aucune et rend le resultat vide de l'agregation (`null` pour
    // un `avg`, `0.0` pour un `sum`, `buckets: []` pour un `terms` — mesure
    // contre ES 8.15). ferrite les indexe sur le document parent : il agregeait
    // donc a plat et rendait un **autre nombre**, en 200 (mesure : `avg` de
    // `7.0` la ou ES rend `null`, `sum` de `21.0` la ou ES rend `0.0`).
    //
    // Un chiffre plausible et faux est le pire des resultats. Et rendre celui
    // d'ES demanderait de savoir agreger *dans* le contexte `nested` pour ne
    // pas se contenter d'un zero — l'agregation `nested` n'est pas supportee.
    // Le refus est donc explicite, comme il l'est deja pour la **requete**
    // equivalente (divergence assumee n° 10 de docs/compat.md).
    if let Some(racine) = champs.racine_nested(champ) {
        return Err(EsError::unsupported(format!(
            "[{champ}] est sous le champ [nested] [{racine}] : ferrite ne l'agrege pas depuis la \
             racine (agregation [{nom}.{type_agg}]). Elasticsearch n'y voit aucun document et \
             rend un resultat vide ; ferrite porte ces valeurs sur le document parent et \
             rendrait un autre nombre, sans le dire. L'agregation [nested] n'est pas encore \
             supportee (voir docs/compat.md)"
        )));
    }

    let MappedField { ty, .. } = champs.get(champ).ok_or_else(|| {
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
    /// Les requetes des agregations `filter`, construites pour **cet** index.
    pub filtres: &'a Filtres,
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
    executer(parts, aggs, "")
}

/// Le type declare d'une agregation (`terms`, `filter`, ...).
fn type_de(corps: &Value) -> Option<&str> {
    corps.as_object()?.keys().find_map(|c| match c.as_str() {
        "aggs" | "aggregations" => None,
        autre => Some(autre),
    })
}

/// Execute un niveau d'agregations : les `filter` a part, tout le reste en un
/// seul passage tantivy.
///
/// `filter` n'existe pas cote tantivy sous une forme utilisable — la sienne
/// prend une chaine dans **sa** syntaxe de requete, pas une requete du Query
/// DSL. Mais elle n'a rien de mysterieux : compter les documents qui
/// correspondent a la fois a la recherche et au filtre, c'est executer
/// l'intersection des deux requetes. C'est exactement ce qu'on fait ici, avec
/// la requete que le Query DSL de ferrite a deja traduite — donc sans
/// approximation, et avec toutes les clauses que ferrite sait traduire.
/// Les sous-agregations, elles, tournent sur cette requete croisee : c'est la
/// meme definition que chez Elasticsearch.
fn executer(parts: &[Part<'_>], aggs: &Value, chemin: &str) -> EsResult<Value> {
    let Some(obj) = aggs.as_object() else {
        return Ok(Value::Object(Map::new()));
    };
    let natives: Map<String, Value> = obj
        .iter()
        .filter(|(_, corps)| type_de(corps) != Some("filter"))
        .map(|(nom, corps)| (nom.clone(), corps.clone()))
        .collect();
    let natives = if natives.is_empty() {
        Map::new()
    } else {
        match run_natif(parts, &Value::Object(natives))? {
            Value::Object(o) => o,
            _ => Map::new(),
        }
    };

    // L'ordre de la reponse est celui de la demande, comme chez ES.
    let mut out = Map::new();
    for (nom, corps) in obj {
        if type_de(corps) == Some("filter") {
            let sous_chemin = if chemin.is_empty() {
                nom.clone()
            } else {
                format!("{chemin}{SEP}{nom}")
            };
            out.insert(nom.clone(), executer_filtre(parts, corps, &sous_chemin)?);
        } else if let Some(v) = natives.get(nom) {
            out.insert(nom.clone(), v.clone());
        }
    }
    Ok(Value::Object(out))
}

/// Une agregation `filter` : le compte des documents qui correspondent aux deux
/// requetes, puis ses sous-agregations sur ce meme croisement.
fn executer_filtre(parts: &[Part<'_>], corps: &Value, chemin: &str) -> EsResult<Value> {
    let sous = corps
        .get("aggs")
        .or_else(|| corps.get("aggregations"))
        .cloned();

    let mut doc_count = 0usize;
    let mut croisees: Vec<Box<dyn Query>> = Vec::with_capacity(parts.len());
    for part in parts {
        let filtre = part.filtres.get(chemin).ok_or_else(|| {
            EsError::internal(format!(
                "agregation [filter] [{chemin}] : requete manquante"
            ))
        })?;
        let croisee: Box<dyn Query> = Box::new(tantivy::query::BooleanQuery::new(vec![
            (tantivy::query::Occur::Must, part.query.box_clone()),
            (tantivy::query::Occur::Must, filtre.box_clone()),
        ]));
        doc_count += part
            .searcher
            .search(&croisee, &tantivy::collector::Count)
            .map_err(|e| EsError::illegal_argument(format!("agregation [filter] : {e}")))?;
        croisees.push(croisee);
    }

    let mut out = Map::new();
    out.insert("doc_count".into(), json!(doc_count));
    if let Some(sous) = sous {
        let sous_parts: Vec<Part<'_>> = parts
            .iter()
            .zip(&croisees)
            .map(|(p, q)| Part {
                gen: p.gen,
                searcher: p.searcher,
                query: &**q,
                filtres: p.filtres,
            })
            .collect();
        if let Value::Object(o) = executer(&sous_parts, &sous, chemin)? {
            out.extend(o);
        }
    }
    Ok(Value::Object(out))
}

/// Un `histogram`, un `date_histogram` ou un `range` sur un champ **multivalue**
/// compte les valeurs, la ou ES compte les documents.
///
/// Un document dont le champ vaut `[1, 2, 3]` tombe trois fois dans le bucket
/// qui les contient toutes chez tantivy, une seule fois chez ES. Mesure :
/// `doc_count` de 4 la ou ES en compte 2. C'est un resultat faux, et rien ne le
/// signale — d'ou ce refus explicite, prononce **seulement** quand la colonne
/// est reellement multivaluee. Le cas courant (une valeur par document) reste
/// exact et reste servi.
///
/// `terms`, `value_count` et `stats` ne sont pas concernes : leurs comptes
/// coincident avec ceux d'ES, mesure a l'appui.
fn verifier_cardinalite(parts: &[Part<'_>], infos: &HashMap<String, Info>) -> EsResult<()> {
    use tantivy::columnar::Cardinality;

    for (chemin, info) in infos {
        if !matches!(
            info.type_agg.as_str(),
            "histogram" | "date_histogram" | "range"
        ) {
            continue;
        }
        let Some(champ) = info.champ.as_deref() else {
            continue;
        };
        for part in parts {
            let Some(mf) = part.gen.fields.get(champ) else {
                continue;
            };
            for lecteur in part.searcher.segment_readers() {
                let ff = lecteur.fast_fields();
                let multi = match mf.ty.kind() {
                    FieldKind::I64 => ff.i64(champ).map(|c| c.get_cardinality()),
                    FieldKind::F64 => ff.f64(champ).map(|c| c.get_cardinality()),
                    FieldKind::Date => ff.date(champ).map(|c| c.get_cardinality()),
                    _ => continue,
                };
                if matches!(multi, Ok(Cardinality::Multivalued)) {
                    return Err(EsError::unsupported(format!(
                        "ferrite ne supporte pas [{}] (agregation [{chemin}]) sur le champ \
                         multivalue [{champ}] : l'agregation de tantivy compte les **valeurs**, \
                         Elasticsearch compte les **documents** — un document dont le champ vaut \
                         [1, 2, 3] tomberait trois fois dans le meme bucket (voir docs/compat.md)",
                        info.type_agg
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Les agregations que tantivy execute lui-meme, en un seul passage.
fn run_natif(parts: &[Part<'_>], aggs: &Value) -> EsResult<Value> {
    let Some(premiere) = parts.first() else {
        return Ok(Value::Object(Map::new()));
    };

    // Les metadonnees de mise en forme (champ date, `format`, `size`, ordre) se
    // lisent dans un mapping. Elles sont prises sur le premier index vise : ce
    // sont des proprietes de la **demande**, pas des documents.
    let mut infos = HashMap::new();
    let demande = preparer(aggs, premiere.gen, "", &mut infos);

    verifier_cardinalite(parts, &infos)?;

    let brut = collecter(parts, demande.clone(), Cible::Recherche)?;
    let vides = formes_vides(parts, &demande, &infos)?;
    Ok(mise_en_forme(
        &brut,
        "",
        &Forme {
            infos: &infos,
            vides: &vides,
        },
    ))
}

/// Sur quoi une passe d'agregation est collectee.
#[derive(Clone, Copy)]
enum Cible {
    /// La requete de la recherche, index par index.
    Recherche,
    /// Aucun document — la forme « zero document » de la demande.
    Rien,
}

/// Collecte une demande d'agregations sur tous les index vises et rend le JSON
/// **brut** de tantivy.
///
/// Chaque index est un index tantivy distinct : il faut donc l'agreger a part,
/// puis fusionner les resultats **intermediaires** avant de finaliser une seule
/// fois (voir [`run`]).
fn collecter(parts: &[Part<'_>], demande: Value, cible: Cible) -> EsResult<Value> {
    let requete: Aggregations = serde_json::from_value(demande)
        .map_err(|e| EsError::parsing(format!("[aggs] illisible : {e}")))?;
    let limites = AggregationLimitsGuard::new(Some(MEMORY_LIMIT), Some(MAX_BUCKETS));
    let rien = tantivy::query::EmptyQuery;

    let mut cumul: Option<IntermediateAggregationResults> = None;
    for part in parts {
        let contexte = AggContextParams::new(limites.clone(), part.gen.index.tokenizers().clone());
        let collecteur = DistributedAggregationCollector::from_aggs(requete.clone(), contexte);
        let query: &dyn Query = match cible {
            Cible::Recherche => part.query,
            Cible::Rien => &rien,
        };
        let partiel = part
            .searcher
            .search(query, &collecteur)
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

    serde_json::to_value(resultat)
        .map_err(|e| EsError::internal(format!("resultat d'agregation illisible : {e}")))
}

/// La forme « zero document » des sous-agregations d'un `histogram`, **mesuree**
/// plutot qu'ecrite.
///
/// tantivy comble les trous entre deux buckets d'un `histogram` ou d'un
/// `date_histogram` — c'est ce que fait ES aussi — mais il n'execute pas ce
/// qu'il y a **dessous** : dans un de ces buckets de remplissage, une
/// sous-agregation `range` rend `buckets: []` la ou ES rend ses intervalles a
/// `doc_count: 0`. Mesure contre ES 8.15 : sur un `histogram` d'intervalle 10
/// dont les prix sautent de 5 a 30, les buckets `10.0` et `20.0` rendent zero
/// intervalle ici, trois chez ES. Un graphe qui empile deux niveaux perd donc
/// ses categories sur les periodes creuses, en 200 et sans un mot.
///
/// Ecrire a la main ce que chaque agregation rend sur zero document remettrait
/// dans le code l'idee qu'on s'en fait. On le **mesure** : les sous-agregations
/// de chaque `histogram` sont rejouees telles quelles sur une requete qui ne
/// ramene aucun document, et un bucket a `doc_count: 0` prend cette
/// reponse-la — puisque c'est exactement ce qu'il contient. Le meme jeu de
/// requetes pose sur une recherche sans resultat est deja mesure identique a ES
/// (`diff_aggs.py`), y compris sur deux niveaux de `range` : la forme rendue
/// n'est donc pas une supposition de plus.
///
/// Le tirage est restreint aux `histogram` et `date_histogram` : ce sont les
/// seules agregations dont tantivy **fabrique** des buckets sans les executer.
/// Un bucket vide de `range` ou de `terms`, lui, est bien passe par le
/// collecteur, et porte deja les bonnes sous-agregations (mesure).
fn formes_vides(
    parts: &[Part<'_>],
    demande: &Value,
    infos: &HashMap<String, Info>,
) -> EsResult<HashMap<String, Value>> {
    // Chaque sous-agregation concernee est posee **au premier niveau**, sous le
    // nom de son chemin : c'est ce qui la rend mesurable. Laissee sous son
    // `histogram`, elle ne serait jamais atteinte — sur zero document, le
    // parent ne rend aucun bucket.
    let mut plates = Map::new();
    let mut enfants: Vec<(String, String)> = Vec::new();
    recenser_sous_aggs(demande, "", infos, &mut plates, &mut enfants);
    if plates.is_empty() {
        return Ok(HashMap::new());
    }
    let brut = collecter(parts, Value::Object(plates), Cible::Rien)?;
    let obj = match brut.as_object() {
        Some(o) => o,
        None => return Ok(HashMap::new()),
    };
    let sans = HashMap::new();
    let forme = Forme {
        infos,
        vides: &sans,
    };
    let mut out: HashMap<String, Value> = HashMap::new();
    for (parent, nom) in enfants {
        let chemin = format!("{parent}{SEP}{nom}");
        let Some(valeur) = obj.get(&chemin) else {
            continue;
        };
        let vide = Info::vide();
        let info = infos.get(&chemin).unwrap_or(&vide);
        let rendu = mise_en_forme_une(valeur, &chemin, info, &forme);
        match out
            .entry(parent)
            .or_insert_with(|| Value::Object(Map::new()))
        {
            Value::Object(o) => {
                o.insert(nom, rendu);
            }
            _ => unreachable!(),
        }
    }
    Ok(out)
}

/// Met a plat les sous-agregations de chaque `histogram` ou `date_histogram` de
/// la demande deja preparee : `plates` en est la demande (une entree par
/// sous-agregation, nommee par son chemin), `enfants` le lien (parent, nom) qui
/// permettra de les remettre en place.
fn recenser_sous_aggs(
    aggs: &Value,
    chemin: &str,
    infos: &HashMap<String, Info>,
    plates: &mut Map<String, Value>,
    enfants: &mut Vec<(String, String)>,
) {
    let Some(obj) = aggs.as_object() else {
        return;
    };
    for (nom, corps) in obj {
        let sous_chemin = if chemin.is_empty() {
            nom.clone()
        } else {
            format!("{chemin}{SEP}{nom}")
        };
        let Some(sous) = corps
            .get("aggs")
            .or_else(|| corps.get("aggregations"))
            .and_then(Value::as_object)
            .filter(|o| !o.is_empty())
        else {
            continue;
        };
        let fabrique_des_buckets = infos
            .get(&sous_chemin)
            .is_some_and(|i| matches!(i.type_agg.as_str(), "histogram" | "date_histogram"));
        if fabrique_des_buckets {
            for (fils, corps_fils) in sous {
                plates.insert(format!("{sous_chemin}{SEP}{fils}"), corps_fils.clone());
                enfants.push((sous_chemin.clone(), fils.clone()));
            }
        }
        recenser_sous_aggs(
            &Value::Object(sous.clone()),
            &sous_chemin,
            infos,
            plates,
            enfants,
        );
    }
}

/// Recense ce qu'il faut savoir de chaque agregation, et prepare la demande
/// envoyee a tantivy (voir [`MARGE_TERMS`]).
fn preparer(
    aggs: &Value,
    gen: &Generation,
    chemin: &str,
    infos: &mut HashMap<String, Info>,
) -> Value {
    let Some(obj) = aggs.as_object() else {
        return aggs.clone();
    };
    let mut out = Map::new();
    for (nom, corps) in obj {
        // Les infos sont rangees par **chemin**, comme les filtres : deux
        // agregations peuvent porter le meme nom a deux niveaux differents, et
        // une table a plat leur ferait echanger leur mise en forme.
        let sous_chemin = if chemin.is_empty() {
            nom.clone()
        } else {
            format!("{chemin}{SEP}{nom}")
        };
        let Some(corps_obj) = corps.as_object() else {
            out.insert(nom.clone(), corps.clone());
            continue;
        };
        let mut nouveau = Map::new();
        for (cle, valeur) in corps_obj {
            if cle == "aggs" || cle == "aggregations" {
                nouveau.insert(cle.clone(), preparer(valeur, gen, &sous_chemin, infos));
                continue;
            }
            let champ = valeur.get("field").and_then(Value::as_str);
            let date = champ
                .and_then(|c| gen.fields.get(c))
                .is_some_and(|m| m.ty.kind() == FieldKind::Date);
            let flottant = champ
                .and_then(|c| gen.fields.get(c))
                .is_some_and(|m| m.ty.kind() == FieldKind::F64);
            let format = champ.and_then(|c| gen.fields.format_de(c)).cloned();
            let sous_aggs = corps_obj
                .get("aggs")
                .or_else(|| corps_obj.get("aggregations"));
            let ordre = valeur
                .get("order")
                .and_then(|o| lire_ordre(o, nom, sous_aggs).ok())
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
                    if matches!(ordre, Ordre::SousAgg { .. }) {
                        // Un ordre par sous-agregation ne se delegue pas a
                        // moitie. tantivy classe une metrique **absente** a
                        // `f64::MIN` ; ES la classe comme un `NaN` de Java,
                        // c'est-a-dire la plus **grande** de toutes (mesure :
                        // un `avg` nul part en tete en `desc` et en queue en
                        // `asc`, dans les deux cas la ou tantivy le met a
                        // l'oppose). Les deux troncatures ne gardent donc pas
                        // les memes seaux, et une marge n'y change rien.
                        //
                        // tantivy ne coupe deja plus au niveau du segment quand
                        // l'ordre vise une sous-agregation (`cut_off_buckets`
                        // n'est pas appele) : tous les termes remontent de
                        // toute facon. On lui demande donc de n'en ecarter
                        // aucun, et c'est ferrite qui choisit — sur l'ensemble
                        // des seaux, avec le comparateur d'ES.
                        o.insert("size".into(), json!(MAX_BUCKETS));
                        o.remove("shard_size");
                    } else {
                        o.insert("size".into(), json!(voulu + MARGE_TERMS));
                    }
                    if o.contains_key("order") {
                        o.insert("order".into(), ordre_pour_tantivy(&ordre));
                    }
                    // `missing` est pose **au type du champ**, comme le fait ES
                    // : `missing: 0` sur un `keyword` y devient la cle `"0"`.
                    // tantivy, lui, poserait la valeur telle quelle et rendrait
                    // la cle `0` — un bucket qui n'a pas le type de sa colonne.
                    if let Some(m) = o.get("missing").cloned() {
                        if let Some(t) = champ.and_then(|c| gen.fields.get(c)) {
                            if let Some(norme) = normaliser_missing(t.ty, &m) {
                                o.insert("missing".into(), norme);
                            }
                        }
                    }
                    // `include` / `exclude` : un motif de Lucene devient un
                    // motif `regex`, une liste devient une liste de chaines.
                    for param in ["include", "exclude"] {
                        if let Some(v) = o.get(param).and_then(filtre_pour_tantivy) {
                            o.insert(param.to_string(), v);
                        }
                    }
                }
            }
            // Les bornes d'un `range` : lues en millisecondes (elles ont deja
            // ete validees), puis rendues a tantivy en nanosecondes s'il s'agit
            // d'un champ date. C'est aussi cette liste qui sert a ecarter les
            // buckets que tantivy ajoute pour combler les trous.
            let mut ranges = Vec::new();
            if cle == "range" {
                let fmt = date.then(|| gen.fields.format_ou_defaut(champ.unwrap_or("")));
                // `ranges` reste dans l'unite **rendue** (millisecondes sur une
                // date) : c'est elle qui sert a reconnaitre un bucket une fois
                // mis en forme. Seule la demande envoyee a tantivy passe en
                // nanosecondes.
                ranges = lire_ranges(valeur, fmt).unwrap_or_default();
                let echelle = if date { NANOS } else { 1.0 };
                if let Some(o) = corps_agg.as_object_mut() {
                    o.insert(
                        "ranges".into(),
                        Value::Array(
                            ranges
                                .iter()
                                .map(|b| {
                                    let mut m = Map::new();
                                    if let Some(x) = b.from {
                                        m.insert("from".into(), json!(x * echelle));
                                    }
                                    if let Some(x) = b.to {
                                        m.insert("to".into(), json!(x * echelle));
                                    }
                                    Value::Object(m)
                                })
                                .collect(),
                        ),
                    );
                }
            }
            infos.insert(
                sous_chemin.clone(),
                Info {
                    type_agg: cle.clone(),
                    date,
                    format,
                    size,
                    ordre,
                    shard_size: valeur
                        .get("shard_size")
                        .and_then(Value::as_u64)
                        .map(|n| n as usize),
                    champ: champ.map(str::to_string),
                    flottant,
                    ranges,
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
///
/// La moyenne de deux dates tombe entre deux millisecondes. ES **tronque**
/// alors vers zero pour l'afficher — mesure : une moyenne de `0.5` s'affiche
/// `"0"`, une moyenne de `1.5` s'affiche `"1"`.
fn rend_date(millis: f64, info: &Info) -> Option<String> {
    match &info.format {
        Some(f) => f.rend(millis as i64),
        None => format_date(millis.trunc()),
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

/// Ce dont la mise en forme a besoin : les metadonnees de chaque agregation,
/// et la forme « zero document » des sous-agregations d'un `histogram` (voir
/// [`formes_vides`]), rangee par chemin de l'agregation qui les porte.
struct Forme<'a> {
    infos: &'a HashMap<String, Info>,
    vides: &'a HashMap<String, Value>,
}

/// Remet le resultat de tantivy au format exact d'ES.
///
/// Cinq ecarts constates par `tests/compat/diff_aggs.py` et par le fuzzer
/// differentiel sont corriges ici, et tous sont documentes dans
/// `docs/compat.md` :
///
/// 1. tantivy compte les dates en **nanosecondes**, ES en millisecondes ;
/// 2. ES ajoute un `*_as_string` a cote de chaque metrique de date ;
/// 3. ES departage les buckets `terms` ex aequo par **cle croissante** ;
/// 4. ES formate les bornes d'un `range` en flottants (`100.0`), meme sur un
///    champ entier, et rend la cle d'un `date_histogram` en entier ;
/// 5. un bucket vide de `histogram` garde ses sous-agregations chez ES ;
///    tantivy comble le trou sans les executer (voir [`formes_vides`]).
fn mise_en_forme(brut: &Value, chemin: &str, forme: &Forme<'_>) -> Value {
    let Some(obj) = brut.as_object() else {
        return brut.clone();
    };
    let mut out = Map::new();
    for (nom, valeur) in obj {
        let sous_chemin = if chemin.is_empty() {
            nom.clone()
        } else {
            format!("{chemin}{SEP}{nom}")
        };
        let vide = Info::vide();
        let info = forme.infos.get(&sous_chemin).unwrap_or(&vide);
        out.insert(
            nom.clone(),
            mise_en_forme_une(valeur, &sous_chemin, info, forme),
        );
    }
    Value::Object(out)
}

/// Les metriques dont la valeur est une date a convertir.
const METRIQUES_DATE: &[&str] = &["value", "min", "max", "avg", "sum"];

fn mise_en_forme_une(valeur: &Value, chemin: &str, info: &Info, forme: &Forme<'_>) -> Value {
    let Some(obj) = valeur.as_object() else {
        return valeur.clone();
    };
    let mut out = Map::new();

    // Un `stats` sur un bucket vide : ES rend `sum: 0.0` mais **pas** de
    // `sum_as_string`. Une somme de zero date n'est pas l'epoque Unix, c'est
    // rien du tout — et ferrite l'annoncait comme « 1970-01-01 ».
    let vide = obj.get("count").and_then(Value::as_u64) == Some(0);

    // Les buckets d'abord : la troncature d'un `terms` dit combien de documents
    // partent avec les buckets ecartes, et ce compte doit rejoindre
    // `sum_other_doc_count`.
    let mut ecartes = 0u64;
    let mut distincts = 0usize;
    if let Some(buckets) = obj.get("buckets") {
        distincts = buckets.as_array().map_or(0, Vec::len);
        let (rendus, perdus) = mise_en_forme_buckets(buckets, chemin, info, forme);
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
        // `value_count` porte lui aussi sa reponse sous `value`, mais c'est un
        // **compte**, pas une date : le convertir rendait « 3 documents » en
        // `3e-06`, avec un `value_as_string` a l'epoque Unix.
        // `value_count` compte : ES le rend en **entier**, tantivy en flottant.
        if info.type_agg == "value_count" && cle == "value" {
            if let Some(n) = v.as_f64() {
                out.insert(cle.clone(), json!(n as u64));
                continue;
            }
        }
        if info.date && info.type_agg != "value_count" && METRIQUES_DATE.contains(&cle.as_str()) {
            if let Some(nanos) = v.as_f64() {
                let millis = nanos / 1_000_000.0;
                out.insert(cle.clone(), json!(millis));
                if !vide {
                    if let Some(texte) = rend_date(millis, info) {
                        out.insert(format!("{cle}_as_string"), json!(texte));
                    }
                }
                continue;
            }
        }
        out.insert(cle.clone(), v.clone());
    }

    if info.type_agg == "terms" {
        out.insert(
            "doc_count_error_upper_bound".to_string(),
            json!(erreur_de_comptage(info, distincts)),
        );
        out.entry("sum_other_doc_count".to_string())
            .or_insert_with(|| json!(ecartes));
    }
    Value::Object(out)
}

/// `doc_count_error_upper_bound` : la borne d'erreur qu'ES annonce sur un
/// `terms`.
///
/// Elle vaut `-1` — « je ne sais pas la borner » — quand deux conditions sont
/// reunies : l'ordre demande **ne classe pas les seaux par compte decroissant**
/// (donc `_count` croissant, ou une sous-agregation), et le nombre de termes
/// distincts **atteint** ce que le shard collecte (`shard_size`, par defaut
/// `size * 1.5 + 10`). Elle vaut `0` partout ailleurs — `_key` compris, dans les
/// deux sens.
///
/// Mesure contre un ES 8.15, `size: 3` donc `shard_size` de 14 : `0` a 13 termes
/// distincts, `-1` a 14. Et sur 800 termes distincts : `-1` en `_count asc`
/// comme en `order: {prix: desc}` a `size: 5` (`shard_size` de 17), `0` des que
/// `shard_size` passe au-dessus de 800 — la borne est bien celle du
/// `shard_size`, pas celle de l'ordre.
///
/// C'est la seconde moitie de ce chiffre qui a demande la mesure : l'ordre par
/// sous-agregation rendait `0` sur un petit index (8 termes, `shard_size` de
/// 14) et `-1` des que l'index en portait plus que le shard n'en collecte. Lire
/// le premier seul aurait fige la mauvaise regle.
fn erreur_de_comptage(info: &Info, distincts: usize) -> i64 {
    let size = info.size.unwrap_or(10);
    let defaut = (size as f64 * 1.5 + 10.0) as usize;
    let shard_size = info.shard_size.unwrap_or(defaut);
    let borne_inconnue = matches!(info.ordre, Ordre::CountAsc | Ordre::SousAgg { .. });
    if borne_inconnue && distincts >= shard_size {
        -1
    } else {
        0
    }
}

/// Rend les buckets mis en forme, et le nombre de documents portes par ceux que
/// la troncature a ecartes.
fn mise_en_forme_buckets(
    buckets: &Value,
    chemin: &str,
    info: &Info,
    forme: &Forme<'_>,
) -> (Value, u64) {
    match buckets {
        Value::Array(a) => {
            let mut liste: Vec<Value> = a
                .iter()
                .map(|b| mise_en_forme_bucket(b, chemin, info, forme))
                .collect();
            let mut ecartes = 0u64;
            if info.type_agg == "terms" {
                trier_terms(&mut liste, &info.ordre);
                if let Some(size) = info.size {
                    ecartes = liste
                        .iter()
                        .skip(size)
                        .filter_map(|b| b.get("doc_count").and_then(Value::as_u64))
                        .sum::<u64>();
                    liste.truncate(size);
                }
            }
            if info.type_agg == "range" {
                liste.retain(|b| demande(b, &info.ranges, 1.0).is_some());
            }
            (Value::Array(liste), ecartes)
        }
        // Forme `keyed` : un objet de buckets, dont l'ordre n'a pas de sens —
        // mais dont la **cle** n'est pas la meme chez tantivy et chez ES.
        Value::Object(o) => {
            let mut map = Map::new();
            for (_, b) in o {
                let bucket = mise_en_forme_bucket(b, chemin, info, forme);
                if info.type_agg == "range" && demande(&bucket, &info.ranges, 1.0).is_none() {
                    continue;
                }
                let (cle, bucket) = cle_keyed(bucket, info);
                map.insert(cle, bucket);
            }
            (Value::Object(map), 0)
        }
        autre => (autre.clone(), 0),
    }
}

/// Ce bucket de `range` a-t-il ete demande ?
///
/// tantivy comble les trous entre deux intervalles demandes : il rend un bucket
/// `10.0-1000.0` que personne n'a reclame quand le client a demande
/// `*--100`, `-100-10` et `1000-*`. ES ne rend que ce qu'on lui demande — et un
/// bucket de plus n'est pas anodin : un client qui lit ses buckets par indice
/// lit alors le mauvais.
fn demande<'a>(bucket: &Value, ranges: &'a [Borne], echelle: f64) -> Option<&'a Borne> {
    let borne = |nom: &str| bucket.get(nom).and_then(Value::as_f64).map(|x| x / echelle);
    let (from, to) = (borne("from"), borne("to"));
    let egal = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => x == y,
        _ => false,
    };
    ranges.iter().find(|b| egal(from, b.from) && egal(to, b.to))
}

/// La cle d'un bucket dans la forme `keyed`, telle qu'ES la nomme.
///
/// Elle n'est **pas** celle de tantivy, et pas la meme selon l'agregation :
/// un `range` est nomme par sa cle de bucket (et perd alors son champ `key`,
/// qu'ES ne repete pas), un `date_histogram` par sa date lisible, un
/// `histogram` par sa borne rendue en flottant (`-1000.0`, non `-1000`).
fn cle_keyed(bucket: Value, info: &Info) -> (String, Value) {
    let cle = bucket.get("key").cloned();
    match info.type_agg.as_str() {
        "range" => {
            let nom = cle
                .as_ref()
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mut b = bucket;
            if let Some(o) = b.as_object_mut() {
                o.remove("key");
            }
            (nom, b)
        }
        "date_histogram" => (
            bucket
                .get("key_as_string")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            bucket,
        ),
        _ => {
            let nom = match cle {
                Some(Value::Number(n)) => n
                    .as_f64()
                    .map_or_else(|| n.to_string(), |f| format!("{f:?}")),
                Some(Value::String(s)) => s,
                autre => autre.map(|v| v.to_string()).unwrap_or_default(),
            };
            (nom, bucket)
        }
    }
}

/// L'ordre d'ES : le critere demande, puis **la cle croissante** pour
/// departager les ex aequo. tantivy ne departage pas, d'ou des selections
/// differentes au bord de la troncature.
///
/// La cle croissante departage dans **les deux sens**, y compris sur un ordre
/// `desc` — mesure contre ES 8.15 : trois categories de meme moyenne (20,5)
/// sortent d'un `order: {prix: desc}` dans l'ordre `Epsilon`, `alpha`, `delta`,
/// c'est-a-dire par octets croissants.
///
/// Une metrique **absente** (`avg` d'un seau ou le champ n'est nulle part) vaut
/// `NaN`, et le `Double.compare` de Java le classe **au-dessus** de tout : en
/// tete d'un `desc`, en queue d'un `asc` (mesure). Le `total_cmp` de Rust range
/// le `NaN` positif exactement au meme endroit.
fn trier_terms(buckets: &mut [Value], ordre: &Ordre) {
    fn compte(b: &Value) -> u64 {
        b.get("doc_count").and_then(Value::as_u64).unwrap_or(0)
    }
    fn lire_metrique(b: &Value, agg: &str, prop: &str, absente: f64) -> f64 {
        b.get(agg)
            .and_then(|m| m.get(if prop.is_empty() { "value" } else { prop }))
            .and_then(Value::as_f64)
            .unwrap_or(absente)
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
        Ordre::SousAgg {
            agg,
            prop,
            metrique,
            asc,
        } => {
            let absente = valeur_absente(metrique, prop);
            let va = lire_metrique(a, agg, prop, absente);
            let vb = lire_metrique(b, agg, prop, absente);
            if *asc {
                va.total_cmp(&vb)
            } else {
                vb.total_cmp(&va)
            }
            .then(cmp_cle(a, b))
        }
    });
}

fn mise_en_forme_bucket(bucket: &Value, chemin: &str, info: &Info, forme: &Forme<'_>) -> Value {
    let Some(obj) = bucket.as_object() else {
        return bucket.clone();
    };
    let mut out = Map::new();

    // Un bucket a `doc_count: 0` ne contient aucun document : ses
    // sous-agregations sont donc, mot pour mot, celles d'une recherche qui ne
    // ramene rien. C'est ce que [`formes_vides`] a mesure, et c'est ce qui
    // remplace ici ce que tantivy a rendu — lui ne les execute pas dans les
    // buckets qu'il fabrique pour combler un trou.
    let zero = obj.get("doc_count").and_then(Value::as_u64) == Some(0);
    let remplacement = if zero {
        forme.vides.get(chemin).and_then(Value::as_object)
    } else {
        None
    };
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
            // Un `terms` sur un champ date : ES rend la cle en millisecondes et
            // ajoute sa forme lisible ; tantivy rend une chaine ISO.
            "key" if info.type_agg == "terms" && info.date => {
                let millis = millis_de_cle(v);
                out.insert("key".into(), json!(millis as i64));
                if let Some(texte) = rend_date(millis, info) {
                    out.insert("key_as_string".into(), json!(texte));
                }
            }
            // Une cle entiere sur un champ flottant : ES ecrit `2.0`, tantivy
            // `2`. Elle se rend donc avec sa decimale — sinon deux serveurs qui
            // ont indexe la meme valeur ne rendent pas le meme type JSON.
            "key" if info.type_agg == "terms" && info.flottant => {
                let x = v.as_f64().unwrap_or(0.0);
                out.insert("key".into(), json!(x));
            }
            "key" if info.type_agg == "range" => {
                out.insert("key".into(), json!(cle_de_range(bucket, info)));
            }
            // Les bornes d'un `range` sur un champ date : tantivy les rend en
            // nanosecondes, ES en millisecondes, et il ajoute leur forme
            // lisible a cote.
            "from" | "to" if info.type_agg == "range" && info.date => {
                let millis = v.as_f64().unwrap_or(0.0) / NANOS;
                out.insert(cle.clone(), json!(millis));
                if let Some(texte) = rend_date(millis, info) {
                    out.insert(format!("{cle}_as_string"), json!(texte));
                }
            }
            // tantivy pose lui aussi un `*_as_string`, en RFC 3339 sans les
            // millisecondes et sans egard pour le `format` du champ : celui
            // qu'on vient d'ecrire est le bon, on ne le laisse pas ecraser.
            "from_as_string" | "to_as_string" if info.type_agg == "range" && info.date => {}
            "doc_count" | "key" | "key_as_string" | "from" | "to" | "from_as_string"
            | "to_as_string" => {
                out.insert(cle.clone(), v.clone());
            }
            // Tout le reste est une sous-agregation.
            autre => {
                let sous_chemin = format!("{chemin}{SEP}{autre}");
                let vide = Info::vide();
                let sous = forme.infos.get(&sous_chemin).unwrap_or(&vide);
                out.insert(cle.clone(), mise_en_forme_une(v, &sous_chemin, sous, forme));
            }
        }
    }
    // La forme « zero document » l'emporte, et elle ajoute au besoin les
    // sous-agregations que tantivy n'a pas rendues du tout.
    for (cle, v) in remplacement.into_iter().flatten() {
        out.insert(cle.clone(), v.clone());
    }
    Value::Object(out)
}

/// La cle d'un terme de `terms` sur un champ date, en millisecondes.
///
/// tantivy la rend en chaine ISO (`2026-01-05T00:00:00Z`) ; ES en
/// millisecondes. La chaine est relue plutot que devinee.
fn millis_de_cle(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0) / NANOS,
        Value::String(s) => {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                .map(|d| (d.unix_timestamp_nanos() / 1_000_000) as f64)
                .unwrap_or(0.0)
        }
        _ => 0.0,
    }
}

/// ES nomme les buckets d'un `range` : `*-100.0`, `100.0-500.0`, `500.0-*` sur
/// un champ numerique, et la **date lisible** sur un champ date
/// (`*-2026-01-03T00:00:00.000Z`, ou au `format` du champ).
///
/// Le nom demande par le client l'emporte, et il est repris de la **demande**,
/// pas devine dans la reponse : sur un champ date, la cle generee par tantivy
/// est elle-meme une date, donc rien ne la distingue d'un nom choisi.
fn cle_de_range(bucket: &Value, info: &Info) -> String {
    let echelle = if info.date { NANOS } else { 1.0 };
    if let Some(nom) = demande(bucket, &info.ranges, echelle).and_then(|b| b.nom.clone()) {
        return nom;
    }
    let brut = |nom: &str| bucket.get(nom).and_then(Value::as_f64);
    let rendu = |b: Option<f64>| match b {
        None => "*".to_string(),
        Some(f) if info.date => {
            rend_date(f / NANOS, info).unwrap_or_else(|| format!("{:?}", f / NANOS))
        }
        Some(f) => format!("{f:?}"),
    };
    format!("{}-{}", rendu(brut("from")), rendu(brut("to")))
}
