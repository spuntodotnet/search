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
    /// Un `date_histogram` : ferrite calcule ses bornes lui-meme et les passe
    /// a tantivy sous forme de `range` (voir [`crate::histodate`]).
    histo: Option<crate::histodate::Histo>,
    /// L'`interval` d'un `histogram` numerique : c'est lui qui donne la borne
    /// haute d'un seau, que le seau rendu ne porte pas.
    intervalle: Option<f64>,
    /// Le `sigma` d'un `extended_stats` : l'ecart des bornes, en ecarts-types.
    /// Son defaut est `2`, chez ES comme ici.
    sigma: f64,
    /// Le type du champ agrege. Il decide de la **forme** de la cle d'un seau
    /// quand il faut la retraduire en clause : un `terms` sur un booleen rend
    /// la cle `1`, que le Query DSL ne relit pas comme `true`.
    genre: Option<FieldKind>,
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
            histo: None,
            intervalle: None,
            sigma: 2.0,
            genre: None,
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
        "extended_stats" => &["field", "missing", "sigma"],
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
            "calendar_interval",
            "time_zone",
            "format",
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

/// Ce qu'une demande d'agregations laisse derriere elle **dans la generation
/// d'un index**, range par chemin d'agregation (`etats>en_retard`).
///
/// Une `Query` tantivy porte des `Field` qui n'ont de sens que dans le schema
/// ou ils ont ete obtenus, et un plan de *fetch* se resout sur un mapping :
/// deux index, meme de mapping identique, en exigent deux. Ces objets voyagent
/// donc avec la cible, pas avec la demande.
#[derive(Default)]
pub struct Prepare {
    /// La requete interne de chaque agregation `filter`.
    pub filtres: HashMap<String, Box<dyn Query>>,
    /// Ce que chaque `top_hits` doit rendre : ses cles de tri et son plan de
    /// lecture, resolus sur **ce** mapping.
    pub tophits: HashMap<String, PlanTopHits>,
}

/// Un `top_hits` resolu sur la generation d'un index.
pub struct PlanTopHits {
    pub from: usize,
    pub size: usize,
    pub sort: Vec<crate::search::SortSpec>,
    pub sort_asc: Vec<bool>,
    pub rendu: crate::search::Rendu,
    pub fetch: crate::fetch::Plan,
}

/// Le separateur des chemins d'agregation.
const SEP: char = '>';

/// Verifie une demande d'agregations de bout en bout, et construit au passage
/// ce qui ne vaut que dans cette generation : les requetes des agregations
/// `filter`, et les plans des `top_hits`.
///
/// `gen` vaut `None` quand la recherche ne vise **aucun** index : la forme se
/// verifie quand meme (c'est ce qui a corrige le seul echec silencieux connu du
/// projet), seul ce qui se resout sur un mapping reste indecidable.
pub fn validate(
    aggs: &Value,
    gen: Option<&Generation>,
    ctx: &crate::dsl::QueryCtx,
    fields_herites: &[crate::fetch::Champ],
) -> EsResult<Prepare> {
    let mut prep = Prepare::default();
    let dehors = Dehors {
        gen,
        ctx,
        fields_herites,
    };
    validate_niveau(aggs, &dehors, "", true, &mut prep)?;
    Ok(prep)
}

/// Ce que la validation d'une agregation lit **en dehors d'elle-meme** : la
/// generation de l'index, le contexte de traduction du Query DSL, et le
/// `fields` de la recherche englobante — dont un `top_hits` herite.
struct Dehors<'a> {
    gen: Option<&'a Generation>,
    ctx: &'a crate::dsl::QueryCtx<'a>,
    fields_herites: &'a [crate::fetch::Champ],
}

fn validate_niveau(
    aggs: &Value,
    dehors: &Dehors<'_>,
    chemin: &str,
    filtre_possible: bool,
    prep: &mut Prepare,
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
        validate_une(nom, &sous_chemin, corps, dehors, filtre_possible, prep)?;
    }
    Ok(())
}

fn validate_une(
    nom: &str,
    chemin: &str,
    corps: &Value,
    dehors: &Dehors<'_>,
    filtre_possible: bool,
    prep: &mut Prepare,
) -> EsResult<()> {
    let (gen, ctx) = (dehors.gen, dehors.ctx);
    let champs = gen.map(|g| &g.fields);
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
        prep.filtres.insert(chemin.to_string(), filtre);
        if let Some(sous) = sous {
            validate_niveau(sous, dehors, chemin, true, prep)?;
        }
        return Ok(());
    }

    // `top_hits` n'a pas de `field` : il rend des documents, pas une
    // statistique sur une colonne. Son plan se resout ici, dans la generation
    // de l'index, exactement comme celui de la recherche englobante.
    if type_agg == "top_hits" {
        if sous.is_some() {
            return Err(sous_aggs_interdites(nom, type_agg));
        }
        let demande = crate::metriques::lire_top_hits(nom, corps_agg)?;
        if let Some(gen) = gen {
            let plan = resoudre_top_hits(
                &demande,
                gen,
                ctx.nom_index.unwrap_or(""),
                dehors.fields_herites,
            )?;
            prep.tophits.insert(chemin.to_string(), plan);
        }
        return Ok(());
    }

    if type_agg == "percentiles" {
        if sous.is_some() {
            return Err(sous_aggs_interdites(nom, type_agg));
        }
        let demande = crate::metriques::lire_percentiles(nom, corps_agg, champs)?;
        verifier_champ(
            nom,
            type_agg,
            &demande.champ,
            champs,
            ctx.nom_index.unwrap_or(""),
        )?;
        return Ok(());
    }

    let params = allowed(type_agg).ok_or_else(|| {
        EsError::unsupported(format!(
            "ferrite ne supporte pas l'agregation [{type_agg}] (dans [aggs.{nom}]) ; \
             agregations supportees : min, max, sum, avg, value_count, stats, extended_stats, \
             percentiles, top_hits, terms, range, histogram, date_histogram, filter"
        ))
    })?;

    {
        let corps_obj = corps_agg.as_object().ok_or_else(|| {
            EsError::parsing(format!("[aggs.{nom}.{type_agg}] doit etre un objet"))
        })?;
        for cle in corps_obj.keys() {
            if !params.contains(&cle.as_str()) {
                // Une agregation neuve porte le type et la phrase d'ES : il
                // refuse une cle inconnue a la **lecture du corps**, pas comme
                // un parametre non implemente.
                if type_agg == "extended_stats" {
                    return Err(EsError::new(
                        axum::http::StatusCode::BAD_REQUEST,
                        "x_content_parse_exception",
                        format!("[{type_agg}] unknown field [{cle}]"),
                    ));
                }
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
                if type_agg == "extended_stats" {
                    // La phrase d'ES, pour une agregation neuve (voir
                    // `verifier_champ`).
                    EsError::illegal_argument(
                        "Required one of fields [field, script], but none were specified. ",
                    )
                } else {
                    EsError::illegal_argument(format!(
                        "[aggs.{nom}.{type_agg}] : [field] est obligatoire"
                    ))
                }
            })?;
        verifier_champ(nom, type_agg, champ, champs, ctx.nom_index.unwrap_or(""))?;
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

    // Un `date_histogram` est lu — donc valide — des la validation : c'est ici
    // que se prononcent les refus d'intervalle, de fuseau et de bornes, avant
    // qu'aucun document ne soit lu.
    if type_agg == "date_histogram" {
        let champ = corps_agg.get("field").and_then(Value::as_str).unwrap_or("");
        let defaut = crate::dateformat::DateFormat::default();
        let format = champs.map_or(&defaut, |c| c.format_ou_defaut(champ));
        crate::histodate::Histo::lire(nom, corps_agg, format)?;
    }

    // `sigma` regle l'ecart des bornes ; ES le refuse negatif a la **lecture du
    // corps**, avec le type d'erreur de son parseur et non celui d'une
    // agregation qui aurait commence.
    if type_agg == "extended_stats" {
        if let Some(v) = corps_agg.get("sigma") {
            if v.as_f64().is_none_or(|s| s < 0.0) {
                return Err(EsError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "x_content_parse_exception",
                    "[extended_stats] failed to parse field [sigma]",
                ));
            }
        }
    }

    if let Some(sous) = sous {
        if !est_bucket(type_agg) {
            return Err(sous_aggs_interdites(nom, type_agg));
        }
        validate_niveau(sous, dehors, chemin, false, prep)?;
    }
    Ok(())
}

/// Une metrique ne porte pas de sous-agregations, et ES le dit avec cette
/// phrase-la.
///
/// Elle est reprise mot pour mot : c'est celle qu'une exception de client
/// officiel remonte, et la seule que le code appelant puisse reconnaitre.
fn sous_aggs_interdites(nom: &str, type_agg: &str) -> EsError {
    EsError::illegal_argument(format!(
        "Aggregator [{nom}] of type [{type_agg}] cannot accept sub-aggregations"
    ))
}

/// Resout un `top_hits` sur la generation d'un index : ses cles de tri, et ce
/// que chacun de ses hits transportera.
///
/// C'est le meme chemin que celui de la recherche englobante — `parse_sort_body`
/// et `fetch::resoudre` — et c'est voulu : deux chemins rendraient deux formes
/// de hit, et la seconde ne serait tenue par rien.
fn resoudre_top_hits(
    demande: &crate::metriques::TopHits,
    gen: &Generation,
    index: &str,
    fields_herites: &[crate::fetch::Champ],
) -> EsResult<PlanTopHits> {
    use crate::fetch::Demande;
    use crate::search::SourceFilter;

    let sort = match &demande.sort {
        Some(v) => crate::api::search::parse_sort_body(v, &gen.fields)?,
        None => Vec::new(),
    };
    let mut lecture = Demande::default();
    // `fields` **s'herite** de la recherche englobante, et rien d'autre : un
    // `top_hits` qui n'en declare pas rend le bloc `fields` que le corps de la
    // recherche a demande (mesure contre ES 8.15 — `docvalue_fields` et
    // `stored_fields`, eux, ne s'heritent pas, et un `fields` declare dans le
    // `top_hits` **remplace** celui du dehors au lieu de s'y ajouter). Rien ne
    // le laissait deviner, et ferrite rendait un hit sans son bloc `fields`,
    // en 200 — trouve par une plage de controle du fuzzer (graine 12300029).
    if let Some(v) = &demande.fields {
        lecture.fields = crate::fetch::lire_champs(v, "fields")?;
    } else {
        lecture.fields = fields_herites.to_vec();
    }
    if let Some(v) = &demande.docvalue {
        lecture.docvalue = crate::fetch::lire_champs(v, "docvalue_fields")?;
    }
    if let Some(v) = &demande.stored {
        lecture.stored = crate::fetch::lire_stored(v)?;
    }
    let source = match &demande.source {
        Some(v) => crate::api::search::parse_source_body(v)?,
        None if lecture.retire_le_source() => SourceFilter::None,
        None => SourceFilter::All,
    };
    let avec_id = lecture.avec_id();
    let fetch = crate::fetch::resoudre(&lecture, gen, index)?;
    Ok(PlanTopHits {
        from: demande.from,
        size: demande.size,
        sort_asc: sort.iter().map(|s| s.asc).collect(),
        sort,
        rendu: crate::search::Rendu {
            source,
            avec_id,
            ..crate::search::Rendu::default()
        },
        fetch,
    })
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

/// Les proprietes qu'ES accepte au bout du chemin d'un `extended_stats`.
///
/// C'est son enumeration `InternalStats.Metrics`, moins les six alias de bornes
/// (`std_upper`, `std_lower_sampling`…) : ils designent des valeurs que la
/// reponse range **dans** `std_deviation_bounds`, et ferrite trie les seaux sur
/// ce qu'il a rendu. Une propriete hors de cette liste rend la phrase d'ES,
/// nom de classe Java compris — c'est celle que le client verra.
const PROPRIETES_EXTENDED_STATS: &[&str] = &[
    "count",
    "sum",
    "min",
    "max",
    "avg",
    "sum_of_squares",
    "variance",
    "variance_population",
    "variance_sampling",
    "std_deviation",
    "std_deviation_population",
    "std_deviation_sampling",
];

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
    } else if type_sous == "extended_stats" {
        if prop.is_empty() {
            return Err(EsError::illegal_argument(format!(
                "Invalid aggregation order path [{chemin}]. Missing value key in [null] which \
                 refers to a multi-value metric aggregation (dans [aggs.{nom}])"
            )));
        }
        if !PROPRIETES_EXTENDED_STATS.contains(&prop) {
            return Err(EsError::illegal_argument(format!(
                "Invalid aggregation order path [{chemin}]. No enum constant \
                 org.elasticsearch.search.aggregations.metrics.InternalStats.Metrics.{prop} \
                 (dans [aggs.{nom}])"
            )));
        }
    } else if type_sous == "percentiles" {
        // ES sait classer sur un percentile (`a.50`) parce qu'il le calcule
        // pendant la collecte. ferrite le calcule **apres**, seau par seau, sur
        // la requete du seau : au moment ou l'ordre se decide, la valeur
        // n'existe pas encore. Servir cet ordre demanderait de rejouer une
        // recherche par terme du dictionnaire, ce qui defait la troncature.
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas l'ordre par une agregation [percentiles] [{agg}] (dans \
             [aggs.{nom}.order]) : elle est calculee seau par seau, apres que les seaux ont ete \
             choisis (voir docs/compat.md)"
        )));
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
        ("min", "") | ("stats", "min") | ("extended_stats", "min") => f64::INFINITY,
        ("max", "") | ("stats", "max") | ("extended_stats", "max") => f64::NEG_INFINITY,
        // `extended_stats` accumule comme `stats` : ses trois sommes valent
        // zero sur un seau vide, et tout ce qui divise par le compte rend un
        // `NaN` — que Java classe au-dessus de tout.
        ("extended_stats", "count" | "sum" | "sum_of_squares") => 0.0,
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

fn verifier_champ(
    nom: &str,
    type_agg: &str,
    champ: &str,
    champs: Option<&Fields>,
    index: &str,
) -> EsResult<()> {
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
        // Les deux metriques livrees par la carte 14 portent la phrase d'ES,
        // mot pour mot : elles sont neuves, donc rien n'oblige a garder la
        // formulation maison des anciennes.
        if matches!(type_agg, "extended_stats" | "percentiles") {
            return Err(EsError::illegal_argument(format!(
                "Fielddata is disabled on [{champ}] in [{index}]. Text fields are not optimised \
                 for operations that require per-document field data like aggregations and \
                 sorting, so these operations are disabled by default. Please use a keyword \
                 field instead. Alternatively, set fielddata=true on [{champ}] in order to load \
                 field data by uninverting the inverted index. Note that this can use \
                 significant memory."
            )));
        }
        return Err(EsError::illegal_argument(format!(
            "Fielddata is disabled on [{champ}] : ferrite n'agrege pas sur un champ [text] ; \
             utilise son multi-field [{champ}.keyword] s'il existe"
        )));
    }
    let numerique = matches!(ty.kind(), FieldKind::I64 | FieldKind::F64 | FieldKind::Date);
    match type_agg {
        // Les deux metriques livrees par la carte 14 portent la phrase d'ES,
        // mot pour mot : elles sont neuves, donc rien n'oblige a garder la
        // formulation maison des anciennes. C'est la seule que l'exception d'un
        // client officiel remonte.
        "extended_stats" | "percentiles" if !numerique => {
            Err(EsError::illegal_argument(format!(
                "Field [{champ}] of type [{}] is not supported for aggregation [{type_agg}]",
                ty.name()
            )))
        }
        // `extended_stats` sur une **date** est refuse, et le chiffre est la
        // raison. tantivy accumule la somme des carres sur la valeur en
        // **nanosecondes** ; ramenee en millisecondes carrees, elle a perdu ses
        // bits de poids faible, et la variance qui s'en deduit ne vaut plus
        // rien : sur **un seul** document, ES rend `std_deviation: 0.0` et
        // ferrite rendait `23170.475` (mesure). Un ecart-type invente sur un
        // document unique est exactement le genre de nombre plausible que ce
        // depot refuse de rendre. `stats` sur une date, lui, reste servi : il
        // ne calcule aucun carre.
        "extended_stats" if ty.kind() == FieldKind::Date => Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [extended_stats] sur le champ [date] [{champ}] (agregation \
             [{nom}]) : la somme des carres s'accumule en nanosecondes et ne se ramene pas en \
             millisecondes sans perdre ses bits de poids faible — sur un seul document, \
             Elasticsearch rend [std_deviation: 0.0] et ferrite rendrait [23170.475] (voir \
             docs/compat.md)"
        ))),
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
    /// Le nom de l'index : un hit de `top_hits` porte son `_index`.
    pub nom: &'a str,
    pub gen: &'a Generation,
    pub searcher: &'a Searcher,
    pub query: &'a dyn Query,
    /// Ce que la validation a construit pour **cet** index : les requetes des
    /// agregations `filter`, les plans des `top_hits`.
    pub prep: &'a Prepare,
    /// Les clauses nommees par un `_name`, traduites dans cette generation.
    ///
    /// Un `top_hits` rend des hits, donc il rend aussi leur `matched_queries` :
    /// ES rejoue chaque clause nommee contre chaque document d'un seau
    /// exactement comme contre ceux de la reponse (mesure).
    pub nommees: &'a [(String, Box<dyn Query>)],
}

impl Part<'_> {
    /// La meme part, sur une requete plus etroite : celle d'un seau, ou celle
    /// d'une agregation `filter`.
    fn avec<'b>(&self, query: &'b dyn Query) -> Part<'b>
    where
        Self: 'b,
    {
        Part {
            nom: self.nom,
            gen: self.gen,
            searcher: self.searcher,
            query,
            prep: self.prep,
            nommees: self.nommees,
        }
    }
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
        .filter(|(_, corps)| {
            let t = type_de(corps);
            t != Some("filter") && !t.is_some_and(est_metrique_ferrite)
        })
        .map(|(nom, corps)| (nom.clone(), corps.clone()))
        .collect();
    let (natives, infos) = if natives.is_empty() {
        (Map::new(), HashMap::new())
    } else {
        let (v, infos) = run_natif(parts, &Value::Object(natives), chemin)?;
        match v {
            Value::Object(o) => (o, infos),
            _ => (Map::new(), infos),
        }
    };

    // L'ordre de la reponse est celui de la demande, comme chez ES.
    let mut out = Map::new();
    for (nom, corps) in obj {
        let sous_chemin = if chemin.is_empty() {
            nom.clone()
        } else {
            format!("{chemin}{SEP}{nom}")
        };
        let type_agg = type_de(corps).unwrap_or("");
        if type_agg == "filter" {
            out.insert(nom.clone(), executer_filtre(parts, corps, &sous_chemin)?);
        } else if est_metrique_ferrite(type_agg) {
            out.insert(
                nom.clone(),
                executer_metrique(parts, corps, type_agg, &sous_chemin)?,
            );
        } else if let Some(v) = natives.get(nom) {
            let mut v = v.clone();
            // Ce que tantivy vient de rendre ne porte pas les metriques que
            // ferrite execute lui-meme : elles se posent seau par seau, sur la
            // requete du seau.
            remplir_seaux(parts, corps, &mut v, &sous_chemin, &infos)?;
            out.insert(nom.clone(), v);
        }
    }
    Ok(Value::Object(out))
}

/// Les deux metriques que ferrite execute lui-meme.
///
/// Elles ont en commun de ne pas etre des accumulateurs : l'une veut la liste
/// triee des valeurs du seau, l'autre ses N meilleurs documents. Aucune ne se
/// delegue — celle de tantivy ne rend pas les memes nombres pour la premiere
/// (une esquisse DDSketch la ou ES trie), et pas la meme chose du tout pour la
/// seconde (des valeurs de colonnes, la ou ES rend des hits complets).
fn est_metrique_ferrite(type_agg: &str) -> bool {
    matches!(type_agg, "percentiles" | "top_hits")
}

/// Y a-t-il, quelque part sous cette demande, une metrique que ferrite execute
/// lui-meme ?
///
/// C'est ce qui decide de rejouer une requete par seau : sans metrique de ce
/// genre, le resultat de tantivy est complet et personne ne paie ce prix.
fn contient_metrique_ferrite(aggs: &Value) -> bool {
    let Some(obj) = aggs.as_object() else {
        return false;
    };
    obj.values().any(|corps| {
        type_de(corps).is_some_and(est_metrique_ferrite)
            || corps
                .get("aggs")
                .or_else(|| corps.get("aggregations"))
                .is_some_and(contient_metrique_ferrite)
    })
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
        let filtre = part.prep.filtres.get(chemin).ok_or_else(|| {
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
            .map(|(p, q)| p.avec(&**q))
            .collect();
        if let Value::Object(o) = executer(&sous_parts, &sous, chemin)? {
            out.extend(o);
        }
    }
    Ok(Value::Object(out))
}

// ---------------------------------------------------------------------------
// Les metriques que ferrite execute lui-meme, seau par seau
// ---------------------------------------------------------------------------

/// Pose les metriques de ferrite dans les seaux que tantivy vient de rendre.
///
/// Le principe est celui de l'agregation `filter`, applique a un seau : la
/// requete d'un seau est celle de la recherche **croisee avec la contrainte qui
/// definit le seau** — un terme pour un `terms`, un intervalle pour un `range`,
/// un `histogram` ou un `date_histogram`. Ces contraintes ne sont pas devinees :
/// ce sont exactement celles que l'agregation a appliquees, relues dans le seau
/// qu'elle a rendu (voir [`contrainte_de_seau`]).
///
/// Le prix est une recherche par seau et par index. Il n'est paye que si une
/// metrique de ferrite est demandee quelque part sous ce seau, et il est publie
/// dans `docs/compat.md` : c'est le meme echange que celui de l'agregation
/// `filter`, qui croise deja deux requetes pour compter.
fn remplir_seaux(
    parts: &[Part<'_>],
    corps: &Value,
    valeur: &mut Value,
    chemin: &str,
    infos: &HashMap<String, Info>,
) -> EsResult<()> {
    let Some(sous) = corps
        .get("aggs")
        .or_else(|| corps.get("aggregations"))
        .cloned()
    else {
        return Ok(());
    };
    if !contient_metrique_ferrite(&sous) {
        return Ok(());
    }
    let vide = Info::vide();
    let info = infos.get(chemin).unwrap_or(&vide);
    // La forme `keyed` range les memes seaux dans un objet : les deux se
    // parcourent pareil une fois la liste obtenue.
    let seaux: Vec<&mut Value> = match valeur.get_mut("buckets") {
        Some(Value::Array(a)) => a.iter_mut().collect(),
        Some(Value::Object(o)) => o.values_mut().collect(),
        _ => return Ok(()),
    };
    for seau in seaux {
        let Some(clause) = contrainte_de_seau(info, seau) else {
            return Err(EsError::internal(format!(
                "agregation [{chemin}] : seau sans contrainte exprimable"
            )));
        };
        let mut croisees: Vec<Box<dyn Query>> = Vec::with_capacity(parts.len());
        for part in parts {
            let ctx = crate::dsl::QueryCtx::new(&part.gen.fields, &part.gen.index, part.searcher);
            let contrainte = crate::dsl::build_query(&clause, &ctx)?;
            // La contrainte du seau **filtre**, elle ne note pas. Chez ES, le
            // `top_hits` d'un seau classe sur le score de la requete de la
            // recherche : l'appartenance au seau n'y contribue pas, puisque
            // l'agregateur recoit des documents que la requete a deja notes.
            // Un `Occur::Must` ordinaire additionne les deux scores, et un
            // `top_hits` sans `sort` sous un `terms` rendait alors `2.263` la
            // ou ES rend `1.0` (mesure, `diff_aggs.py`).
            let contrainte: Box<dyn Query> =
                Box::new(tantivy::query::ConstScoreQuery::new(contrainte, 0.0));
            croisees.push(Box::new(tantivy::query::BooleanQuery::new(vec![
                (tantivy::query::Occur::Must, part.query.box_clone()),
                (tantivy::query::Occur::Must, contrainte),
            ])));
        }
        let sous_parts: Vec<Part<'_>> = parts
            .iter()
            .zip(&croisees)
            .map(|(p, q)| p.avec(&**q))
            .collect();
        remplir_niveau(&sous_parts, &sous, seau, chemin, infos)?;
    }
    Ok(())
}

/// Un niveau de sous-agregations, a l'interieur d'un seau : les metriques de
/// ferrite s'y calculent, les seaux plus profonds s'y descendent.
fn remplir_niveau(
    parts: &[Part<'_>],
    aggs: &Value,
    seau: &mut Value,
    chemin: &str,
    infos: &HashMap<String, Info>,
) -> EsResult<()> {
    let Some(obj) = aggs.as_object().cloned() else {
        return Ok(());
    };
    for (nom, corps) in &obj {
        let sous_chemin = format!("{chemin}{SEP}{nom}");
        let type_agg = type_de(corps).unwrap_or("");
        if est_metrique_ferrite(type_agg) {
            let rendu = executer_metrique(parts, corps, type_agg, &sous_chemin)?;
            if let Some(o) = seau.as_object_mut() {
                o.insert(nom.clone(), rendu);
            }
        } else if est_bucket(type_agg) {
            // Les seaux d'un niveau plus profond ont deja ete rendus par
            // tantivy ; il ne reste qu'a y descendre. Leurs `Info` sont dans la
            // meme table — [`preparer`] descend tout l'arbre — et elles sont
            // rangees par le meme chemin absolu.
            if let Some(v) = seau.get_mut(nom) {
                let mut copie = v.take();
                remplir_seaux(parts, corps, &mut copie, &sous_chemin, infos)?;
                *v = copie;
            }
        }
    }
    Ok(())
}

/// La clause du Query DSL qui **definit** un seau.
///
/// Elle est relue dans le seau rendu, pas reconstruite depuis la demande : une
/// cle de `terms` est deja la valeur du terme, et les bornes d'un `range` ou
/// d'un `histogram` sont deja dans l'unite rendue (les millisecondes sur une
/// date). C'est ce qui evite de refaire, une seconde fois et autrement, le
/// calcul de bornes que [`crate::histodate`] a deja fait.
fn contrainte_de_seau(info: &Info, seau: &Value) -> Option<Value> {
    let champ = info.champ.clone()?;
    match info.type_agg.as_str() {
        "terms" => {
            let cle = seau.get("key")?;
            // Sur une date, la cle rendue est un nombre de millisecondes : le
            // Query DSL le lit comme un `epoch_millis`, ce qui est exactement
            // ce que la colonne porte. Sur un **booleen**, en revanche, ES rend
            // la cle `1` et non `true` : passee telle quelle, la clause
            // echouerait sur « valeur 1 non convertible en un booleen ».
            let cle = if info.genre == Some(FieldKind::Bool) {
                json!(cle.as_f64().unwrap_or(0.0) != 0.0)
            } else {
                cle.clone()
            };
            Some(json!({"term": {champ: cle}}))
        }
        "range" | "histogram" | "date_histogram" => {
            let (debut, fin) = bornes_du_seau(seau, info)?;
            let mut b = Map::new();
            if let Some(x) = debut {
                b.insert("gte".into(), json!(x));
            }
            if let Some(x) = fin {
                b.insert("lt".into(), json!(x));
            }
            Some(json!({"range": {champ: Value::Object(b)}}))
        }
        _ => None,
    }
}

/// Les deux bornes d'un seau d'intervalle, dans l'unite **rendue**.
///
/// Trois formes a lire, et une seule est directe : un `range` porte ses bornes
/// dans le seau ; un `histogram` n'y porte que sa cle, donc sa fin vaut cle +
/// intervalle ; un `date_histogram` n'y porte que sa cle aussi, et sa fin est
/// la borne suivante — qu'une duree fixe ne donne pas, un mois civil n'ayant
/// pas de duree constante.
fn bornes_du_seau(seau: &Value, info: &Info) -> Option<(Option<f64>, Option<f64>)> {
    match info.type_agg.as_str() {
        "range" => Some((
            seau.get("from").and_then(Value::as_f64),
            seau.get("to").and_then(Value::as_f64),
        )),
        "histogram" => {
            let cle = seau.get("key")?.as_f64()?;
            Some((Some(cle), Some(cle + info.intervalle?)))
        }
        "date_histogram" => {
            let cle = seau.get("key")?.as_f64()? as i64;
            let fin = info.histo.as_ref()?.fin_du_seau(cle)?;
            Some((Some(cle as f64), Some(fin as f64)))
        }
        _ => None,
    }
}

/// Execute une metrique de ferrite sur la requete courante.
fn executer_metrique(
    parts: &[Part<'_>],
    corps: &Value,
    type_agg: &str,
    chemin: &str,
) -> EsResult<Value> {
    let corps_agg = corps.get(type_agg).unwrap_or(&Value::Null);
    match type_agg {
        "percentiles" => executer_percentiles(parts, corps_agg, chemin),
        "top_hits" => executer_top_hits(parts, chemin),
        _ => Err(EsError::internal(format!(
            "metrique [{type_agg}] inconnue de ferrite"
        ))),
    }
}

/// `percentiles` : les valeurs de la colonne, triees, puis interpolees comme le
/// fait Elasticsearch tant qu'il est exact (voir [`crate::metriques`]).
fn executer_percentiles(parts: &[Part<'_>], corps_agg: &Value, chemin: &str) -> EsResult<Value> {
    let champs = parts.first().map(|p| &p.gen.fields);
    let demande = crate::metriques::lire_percentiles(chemin, corps_agg, champs)?;
    let mut valeurs: Vec<f64> = Vec::new();
    let mut date = false;
    let mut format = None;
    for part in parts {
        let Some(mf) = part.gen.fields.get(&demande.champ) else {
            continue;
        };
        date = mf.ty.kind() == FieldKind::Date;
        if date {
            format = part.gen.fields.format_de(&demande.champ).cloned();
        }
        let collecteur =
            crate::metriques::ValeursColonne::new(&demande.champ, mf.ty.kind(), demande.missing);
        valeurs.extend(
            part.searcher.search(part.query, &collecteur).map_err(|e| {
                EsError::illegal_argument(format!("agregation [percentiles] : {e}"))
            })?,
        );
    }
    // Sur une date, ES pose une cle lisible a cote de chaque percentile.
    let info = Info {
        type_agg: "percentiles".into(),
        date,
        format,
        ..Info::vide()
    };
    let rendre = |ms: f64| -> Option<String> {
        if info.date {
            rend_date(ms, &info)
        } else {
            None
        }
    };
    Ok(crate::metriques::bloc(&demande, valeurs, &rendre))
}

/// `top_hits` : une recherche complete a l'interieur du seau.
fn executer_top_hits(parts: &[Part<'_>], chemin: &str) -> EsResult<Value> {
    let mut cibles = Vec::with_capacity(parts.len());
    let mut reglage: Option<(usize, usize, &[bool], &crate::search::Rendu)> = None;
    for part in parts {
        let plan = part.prep.tophits.get(chemin).ok_or_else(|| {
            EsError::internal(format!("agregation [top_hits] [{chemin}] : plan manquant"))
        })?;
        // `from`, `size` et le sens du tri viennent de la **demande**, donc ils
        // sont les memes partout ; les cles de tri et le plan de lecture, eux,
        // sont resolus par index.
        reglage.get_or_insert((plan.from, plan.size, &plan.sort_asc, &plan.rendu));
        cibles.push(crate::search::CibleTopHits {
            nom: part.nom,
            gen: part.gen,
            searcher: part.searcher,
            query: part.query,
            plan: &plan.fetch,
            sort: &plan.sort,
            nommees: part.nommees,
        });
    }
    let Some((from, size, sort_asc, rendu)) = reglage else {
        return Ok(json!({"hits": {"total": {"value": 0, "relation": "eq"},
                                  "max_score": Value::Null, "hits": []}}));
    };
    crate::search::bloc_top_hits(&cibles, from, size, sort_asc, rendu)
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
fn run_natif(
    parts: &[Part<'_>],
    aggs: &Value,
    chemin: &str,
) -> EsResult<(Value, HashMap<String, Info>)> {
    let mut infos = HashMap::new();
    let Some(premiere) = parts.first() else {
        return Ok((Value::Object(Map::new()), infos));
    };

    // Les metadonnees de mise en forme (champ date, `format`, `size`, ordre) se
    // lisent dans un mapping. Elles sont prises sur le premier index vise : ce
    // sont des proprietes de la **demande**, pas des documents.
    let demande = preparer(aggs, premiere.gen, chemin, &mut infos);

    verifier_cardinalite(parts, &infos)?;

    let demande = calendrier_en_range(parts, demande, chemin, &mut infos)?;
    let brut = collecter(parts, demande.clone(), Cible::Recherche)?;
    let vides = formes_vides(parts, &demande, chemin, &infos)?;
    let rendu = mise_en_forme(
        &brut,
        chemin,
        &Forme {
            infos: &infos,
            vides: &vides,
        },
    );
    Ok((rendu, infos))
}

/// Remplace chaque `date_histogram` de la demande par le `range` **contigu**
/// qui lui correspond, apres avoir mesure ou commencent et ou finissent ses
/// seaux.
///
/// La pre-passe demande le `min` et le `max` de chaque champ agrege, sur la
/// meme requete et sur tous les index vises : c'est exactement ce qu'ES connait
/// au moment de remplir les trous de son histogramme. Un `date_histogram` pose
/// **sous** un autre seau (un `terms`, une `filter`) partage ces bornes, qui
/// sont alors plus larges que les siennes : les seaux vides en trop sont
/// retires seau parent par seau parent a la mise en forme (voir
/// [`crate::histodate::Histo::seaux`]).
fn calendrier_en_range(
    parts: &[Part<'_>],
    demande: Value,
    chemin: &str,
    infos: &mut HashMap<String, Info>,
) -> EsResult<Value> {
    // Un `date_histogram` dont la relecture a echoue **apres** la validation
    // n'existe pas — mais s'il existait, la demande partirait telle quelle chez
    // tantivy, dont le deserialiseur ignore les cles qu'il ne connait pas :
    // `calendar_interval` et `time_zone` disparaitraient en silence, et le
    // graphe serait faux en 200. C'est exactement ce que ce module existe pour
    // empecher, donc le cas est bruyant.
    if let Some((chemin, info)) = infos
        .iter()
        .find(|(_, i)| i.type_agg == "date_histogram" && i.histo.is_none())
    {
        return Err(EsError::internal(format!(
            "[date_histogram] (agregation [{chemin}]) : parametres relus autrement qu'a la \
             validation ({:?})",
            info.champ
        )));
    }
    let mut champs: Vec<String> = infos
        .values()
        .filter(|i| i.type_agg == "date_histogram")
        .filter_map(|i| i.histo.as_ref().map(|h| h.champ.clone()))
        .collect();
    if champs.is_empty() {
        return Ok(demande);
    }
    champs.sort();
    champs.dedup();

    let mut mesure = Map::new();
    for (i, champ) in champs.iter().enumerate() {
        mesure.insert(format!("min{i}"), json!({"min": {"field": champ}}));
        mesure.insert(format!("max{i}"), json!({"max": {"field": champ}}));
    }
    let brut = collecter(parts, Value::Object(mesure), Cible::Recherche)?;
    let borne = |cle: &str| -> Option<i64> {
        brut.get(cle)
            .and_then(|v| v.get("value"))
            .and_then(Value::as_f64)
            // tantivy compte les dates en nanosecondes.
            .map(|nanos| (nanos / NANOS).round() as i64)
    };

    for info in infos.values_mut() {
        let Some(histo) = info.histo.as_mut() else {
            continue;
        };
        let i = champs.iter().position(|c| *c == histo.champ).unwrap_or(0);
        histo.pose_les_bornes(borne(&format!("min{i}")), borne(&format!("max{i}")))?;
    }

    Ok(reecrit_date_histogram(&demande, chemin, infos))
}

/// La reecriture proprement dite, une fois les bornes connues.
fn reecrit_date_histogram(aggs: &Value, chemin: &str, infos: &HashMap<String, Info>) -> Value {
    let Some(obj) = aggs.as_object() else {
        return aggs.clone();
    };
    let mut out = Map::new();
    for (nom, corps) in obj {
        let sous_chemin = if chemin.is_empty() {
            nom.clone()
        } else {
            format!("{chemin}{SEP}{nom}")
        };
        let Some(corps_obj) = corps.as_object() else {
            out.insert(nom.clone(), corps.clone());
            continue;
        };
        let histo = infos.get(&sous_chemin).and_then(|i| i.histo.as_ref());
        let mut nouveau = Map::new();
        for (cle, valeur) in corps_obj {
            if cle == "aggs" || cle == "aggregations" {
                nouveau.insert(
                    cle.clone(),
                    reecrit_date_histogram(valeur, &sous_chemin, infos),
                );
                continue;
            }
            match histo {
                Some(h) if cle == "date_histogram" => {
                    nouveau.insert(
                        "range".into(),
                        json!({
                            "field": h.champ,
                            "ranges": h.intervalles_pour_tantivy(),
                        }),
                    );
                }
                _ => {
                    nouveau.insert(cle.clone(), valeur.clone());
                }
            }
        }
        out.insert(nom.clone(), Value::Object(nouveau));
    }
    Value::Object(out)
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
    chemin: &str,
    infos: &HashMap<String, Info>,
) -> EsResult<HashMap<String, Value>> {
    // Chaque sous-agregation concernee est posee **au premier niveau**, sous le
    // nom de son chemin : c'est ce qui la rend mesurable. Laissee sous son
    // `histogram`, elle ne serait jamais atteinte — sur zero document, le
    // parent ne rend aucun bucket.
    let mut plates = Map::new();
    let mut enfants: Vec<(String, String)> = Vec::new();
    recenser_sous_aggs(demande, chemin, infos, &mut plates, &mut enfants);
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
        // Un `date_histogram` n'est plus concerne : ferrite le fait executer
        // comme un `range`, et tantivy execute bien les sous-agregations des
        // intervalles vides d'un `range` (mesure). Seul l'`histogram`
        // numerique fabrique encore des seaux sans les remplir.
        let fabrique_des_buckets = infos
            .get(&sous_chemin)
            .is_some_and(|i| i.type_agg == "histogram");
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
        // Les metriques que ferrite execute lui-meme ne partent pas chez
        // tantivy : elles seront posees seau par seau (voir [`remplir_seaux`]).
        // Leur laisser une place dans la demande ne changerait pas le resultat
        // — c'est bien pire : le deserialiseur de tantivy ignore les cles qu'il
        // ne connait pas, et un `top_hits` y deviendrait une agregation vide.
        if type_de(corps).is_some_and(est_metrique_ferrite) {
            continue;
        }
        let mut nouveau = Map::new();
        for (cle, valeur) in corps_obj {
            if cle == "aggs" || cle == "aggregations" {
                let sous = preparer(valeur, gen, &sous_chemin, infos);
                // Un seau dont **toutes** les sous-agregations sont des
                // metriques de ferrite n'a plus rien a demander : tantivy
                // refuse une section `aggs` vide.
                if sous.as_object().is_some_and(|o| !o.is_empty()) {
                    nouveau.insert(cle.clone(), sous);
                }
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
            // Un `date_histogram` a deja ete valide ; il est relu ici pour
            // porter son arrondi jusqu'a l'execution.
            let histo = (cle == "date_histogram")
                .then(|| {
                    crate::histodate::Histo::lire(
                        nom,
                        valeur,
                        gen.fields.format_ou_defaut(champ.unwrap_or("")),
                    )
                    .ok()
                })
                .flatten();
            infos.insert(
                sous_chemin.clone(),
                Info {
                    intervalle: (cle == "histogram")
                        .then(|| valeur.get("interval").and_then(Value::as_f64))
                        .flatten(),
                    sigma: valeur.get("sigma").and_then(Value::as_f64).unwrap_or(2.0),
                    genre: champ.and_then(|c| gen.fields.get(c)).map(|m| m.ty.kind()),
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
                    histo,
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

    // Un `date_histogram` : ce que tantivy vient de rendre est le `range`
    // contigu que ferrite lui a demande a sa place. Les seaux sont donc
    // renommes, dates, rognes et filtres ici (voir [`crate::histodate`]).
    if let Some(histo) = &info.histo {
        let sous = |restant: &Map<String, Value>| -> Map<String, Value> {
            restant
                .iter()
                .map(|(cle, v)| {
                    let sous_chemin = format!("{chemin}{SEP}{cle}");
                    let vide = Info::vide();
                    let sous_info = forme.infos.get(&sous_chemin).unwrap_or(&vide);
                    (
                        cle.clone(),
                        mise_en_forme_une(v, &sous_chemin, sous_info, forme),
                    )
                })
                .collect()
        };
        let seaux = histo.seaux(obj.get("buckets").unwrap_or(&Value::Null), &sous);
        out.insert("buckets".into(), seaux);
        return Value::Object(out);
    }

    if info.type_agg == "extended_stats" {
        return extended_stats(obj, info);
    }

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

/// Les onze valeurs qu'`extended_stats` rend a cote de celles de `stats`.
///
/// Elles sont **recalculees ici**, pas reprises de tantivy — et c'est tout
/// l'objet de cette fonction. tantivy accumule sa variance par l'algorithme de
/// Welford, ES par la formule naive `(Σx² - (Σx)²/n) / n` ; les deux sont
/// justes en mathematiques et ne rendent pas le meme `double`. Ce qui est
/// repris de tantivy est ce qu'il accumule **comme ES** : le compte, la somme
/// (compensee par Kahan des deux cotes) et la somme des carres (compensee elle
/// aussi, sous le nom `sum_of_squares`). Le reste se derive de ces trois-la,
/// avec les expressions d'`InternalExtendedStats`.
///
/// Trois bords ne se devinaient pas, et viennent de la mesure contre ES 8.15 :
///
/// * a **zero** document, ES rend `count: 0` et `sum: 0.0`, tout le reste a
///   `null` — y compris un objet `std_deviation_bounds` dont les six valeurs
///   sont nulles. Il ne l'omet pas ;
/// * a **un** document, la variance de population vaut `0.0` mais celle
///   d'echantillon divise par `count - 1`, donc par zero : ES rend la **chaine**
///   `"NaN"`, et la propage dans `std_deviation_sampling` et dans les deux
///   bornes d'echantillon ;
/// * une variance negative (le flottant peut y descendre quand toutes les
///   valeurs sont egales) est ramenee a `0`, et `NaN < 0` etant faux, un `NaN`
///   traverse ce garde-fou intact.
fn extended_stats(obj: &Map<String, Value>, info: &Info) -> Value {
    let lire = |cle: &str| obj.get(cle).and_then(Value::as_f64);
    let count = obj.get("count").and_then(Value::as_u64).unwrap_or(0);
    // Un champ `date` n'arrive pas ici : il est refuse a la validation, parce
    // que la somme des carres s'accumule en nanosecondes (voir
    // `verifier_champ`).
    let sum = lire("sum").unwrap_or(0.0);
    let sumsq = lire("sum_of_squares").unwrap_or(0.0);

    // Un `NaN` n'a pas d'ecriture JSON : ES l'ecrit en toutes lettres, entre
    // guillemets.
    fn nombre(v: Option<f64>) -> Value {
        match v {
            None => Value::Null,
            Some(x) if x.is_nan() => json!("NaN"),
            Some(x) => json!(x),
        }
    }
    let mut out = Map::new();
    if count == 0 {
        // ES rend un vrai `0.0` ici, et rien d'autre : c'est le seul champ
        // qu'une somme vide renseigne.
        out.insert("count".into(), json!(0));
        for cle in ["min", "max", "avg"] {
            out.insert(cle.into(), Value::Null);
        }
        out.insert("sum".into(), json!(0.0));
        for cle in [
            "sum_of_squares",
            "variance",
            "variance_population",
            "variance_sampling",
            "std_deviation",
            "std_deviation_population",
            "std_deviation_sampling",
        ] {
            out.insert(cle.into(), Value::Null);
        }
        let mut bornes = Map::new();
        for cle in [
            "upper",
            "lower",
            "upper_population",
            "lower_population",
            "upper_sampling",
            "lower_sampling",
        ] {
            bornes.insert(cle.into(), Value::Null);
        }
        out.insert("std_deviation_bounds".into(), Value::Object(bornes));
        return Value::Object(out);
    }

    let n = count as f64;
    let min = lire("min");
    let max = lire("max");
    let avg = lire("avg");
    let borne = |v: f64| if v < 0.0 { 0.0 } else { v };
    let ecart = sumsq - (sum * sum) / n;
    let var_pop = borne(ecart / n);
    let var_ech = borne(ecart / (n - 1.0));
    let std_pop = var_pop.sqrt();
    let std_ech = var_ech.sqrt();
    let moyenne = avg.unwrap_or(f64::NAN);
    let sigma = info.sigma;

    out.insert("count".into(), json!(count));
    out.insert("min".into(), nombre(min));
    out.insert("max".into(), nombre(max));
    out.insert("avg".into(), nombre(avg));
    out.insert("sum".into(), nombre(Some(sum)));
    out.insert("sum_of_squares".into(), nombre(Some(sumsq)));
    out.insert("variance".into(), nombre(Some(var_pop)));
    out.insert("variance_population".into(), nombre(Some(var_pop)));
    out.insert("variance_sampling".into(), nombre(Some(var_ech)));
    out.insert("std_deviation".into(), nombre(Some(std_pop)));
    out.insert("std_deviation_population".into(), nombre(Some(std_pop)));
    out.insert("std_deviation_sampling".into(), nombre(Some(std_ech)));
    let mut bornes = Map::new();
    for (cle, v) in [
        ("upper", moyenne + std_pop * sigma),
        ("lower", moyenne - std_pop * sigma),
        ("upper_population", moyenne + std_pop * sigma),
        ("lower_population", moyenne - std_pop * sigma),
        ("upper_sampling", moyenne + std_ech * sigma),
        ("lower_sampling", moyenne - std_ech * sigma),
    ] {
        bornes.insert(cle.into(), nombre(Some(v)));
    }
    out.insert("std_deviation_bounds".into(), Value::Object(bornes));
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
