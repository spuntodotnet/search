//! `highlight` : les fragments surlignes d'une barre de recherche.
//!
//! Ce que ce module doit reproduire n'est pas « couper le texte autour du mot
//! trouve » : c'est le `UnifiedHighlighter` de Lucene tel qu'Elasticsearch le
//! configure, et sa forme n'etait devinable nulle part. Trois pieces, mesurees
//! une par une contre un ES 8.15 par
//! [`tests/compat/diff_highlight.py`](../tests/compat/diff_highlight.py) :
//!
//! - **ce qui est surligne** : les termes que la requete pose *sur ce
//!   champ-la* (c'est `require_field_match`, vrai par defaut). Une phrase rend
//!   **une seule** marque qui couvre toute la suite, pas une par terme ;
//! - **ou le fragment commence et finit** : le `BoundedBreakIteratorScanner`
//!   d'Elasticsearch. Les phrases (au sens d'UAX#29, voir [`crate::segments`])
//!   sont fusionnees vers l'avant tant que la longueur reste sous
//!   `fragment_size` ; si une seule phrase deborde deja, le fragment est
//!   re-coupe **au mot** autour de la correspondance. Ni « une phrase par
//!   fragment » ni « `fragment_size` caracteres » ne decrivent ce que rend ES ;
//! - **lesquels sont gardes** quand il y en a plus que `number_of_fragments` :
//!   le `PassageScorer` de Lucene (BM25 sur le fragment, `pivot = 87`), puis un
//!   retour a l'ordre du document.
//!
//! Un champ multivalue est traite valeur par valeur — un fragment ne franchit
//! jamais la frontiere entre deux valeurs — mais les fragments de toutes les
//! valeurs sont mis en concurrence ensemble.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::analysis::Analyzer;
use crate::engine::Generation;
use crate::error::{EsError, EsResult};
use crate::mapping::{self, FieldKind, FieldType};
use crate::search::glob_match;

/// Les constantes du `PassageScorer` de Lucene. Elles ne se reglent pas chez
/// ES, et elles decident quels fragments survivent a `number_of_fragments`.
const K1: f64 = 1.2;
const B: f64 = 0.75;
const PIVOT: f64 = 87.0;

// ---------------------------------------------------------------------------
// Ce que le corps demande
// ---------------------------------------------------------------------------

/// Les reglages d'un champ : les valeurs globales, surchargees champ par champ.
#[derive(Debug, Clone, PartialEq)]
pub struct Reglages {
    pub pre: String,
    pub post: String,
    /// `number_of_fragments`. `0` = le champ entier, valeur par valeur.
    pub nb_fragments: usize,
    /// `fragment_size`. `0` = une phrase par fragment, sans borne de longueur.
    pub taille: usize,
    /// `no_match_size` : combien de caracteres rendre quand rien ne correspond.
    pub sans_correspondance: usize,
    /// `require_field_match` : n'utiliser que les termes poses sur ce champ.
    pub champ_exige: bool,
}

impl Default for Reglages {
    fn default() -> Self {
        Self {
            pre: "<em>".into(),
            post: "</em>".into(),
            nb_fragments: 5,
            taille: 100,
            sans_correspondance: 0,
            champ_exige: true,
        }
    }
}

/// Le bloc `highlight` du corps, lu mais pas encore resolu sur un mapping.
#[derive(Debug, Clone)]
pub struct Demande {
    /// Les motifs de champ, dans l'ordre du corps, avec leurs reglages.
    champs: Vec<(String, Reglages)>,
}

/// Lit le bloc `highlight` du corps de `_search`.
///
/// Tout ce qui n'est pas reproduit est refuse **par son nom** : un
/// `type: fvh` accepte en silence rendrait des fragments coupes autrement, et
/// un `order: score` ignore rendrait les bons fragments dans le mauvais ordre.
pub fn lire(v: &Value) -> EsResult<Demande> {
    let obj = v
        .as_object()
        .ok_or_else(|| EsError::parsing("le bloc [highlight] de [_search] doit etre un objet"))?;
    let globales = lire_reglages(obj, &Reglages::default(), "highlight")?;

    let mut champs = Vec::new();
    match obj.get("fields") {
        None | Some(Value::Null) => {}
        Some(Value::Object(o)) => {
            for (motif, spec) in o {
                champs.push((motif.clone(), lire_champ(motif, spec, &globales)?));
            }
        }
        // La forme heritee : une liste d'objets a une seule cle. Les scripts
        // venus de la 1.x l'ecrivent encore, et ES la sert toujours.
        Some(Value::Array(a)) => {
            for entree in a {
                let o = entree.as_object().ok_or_else(|| {
                    EsError::parsing("[highlight.fields] : objet attendu dans la liste")
                })?;
                for (motif, spec) in o {
                    champs.push((motif.clone(), lire_champ(motif, spec, &globales)?));
                }
            }
        }
        Some(_) => {
            return Err(EsError::parsing(
                "[highlight.fields] : un objet ou une liste d'objets est attendu",
            ))
        }
    }
    Ok(Demande { champs })
}

fn lire_champ(motif: &str, spec: &Value, globales: &Reglages) -> EsResult<Reglages> {
    match spec {
        Value::Null => Ok(globales.clone()),
        Value::Object(o) => lire_reglages(o, globales, &format!("highlight.fields.{motif}")),
        _ => Err(EsError::parsing(format!(
            "[highlight.fields.{motif}] : objet attendu"
        ))),
    }
}

/// Les cles que ferrite sert, et celles qu'il refuse en les nommant.
///
/// `fields` n'est lu qu'au niveau global : sous un champ, c'est une cle
/// inconnue, exactement comme chez ES.
fn lire_reglages(obj: &Map<String, Value>, defauts: &Reglages, ou: &str) -> EsResult<Reglages> {
    let mut r = defauts.clone();
    for (cle, v) in obj {
        match cle.as_str() {
            "fields" if ou == "highlight" => {}
            "pre_tags" | "post_tags" | "tags_schema" => {}
            "number_of_fragments" => r.nb_fragments = lire_taille(v, cle)?.max(0) as usize,
            "fragment_size" => {
                // Une taille negative n'est pas un refus chez ES : elle
                // retombe sur le defaut (mesure contre 8.15, qui rend alors
                // exactement ce que rend `fragment_size: 100`).
                let n = lire_taille(v, cle)?;
                r.taille = if n < 0 {
                    Reglages::default().taille
                } else {
                    n as usize
                };
            }
            "no_match_size" => r.sans_correspondance = lire_taille(v, cle)?.max(0) as usize,
            "require_field_match" => {
                r.champ_exige = v.as_bool().ok_or_else(|| {
                    EsError::illegal_argument(format!(
                        "[{ou}.require_field_match] : booleen attendu"
                    ))
                })?;
            }
            // `none` est le defaut d'ES : les fragments sortent dans l'ordre du
            // document. `score` les trierait par le score du fragment — un
            // classement que ferrite ne sait pas encore rendre identique.
            "order" => match v.as_str() {
                Some("none") => {}
                _ => {
                    return Err(EsError::unsupported(format!(
                        "ferrite ne supporte pas [order] dans [{ou}] : seul l'ordre du document \
                         ([order: none], le defaut) est rendu"
                    )))
                }
            },
            "type"
            | "highlight_query"
            | "matched_fields"
            | "boundary_scanner"
            | "boundary_chars"
            | "boundary_max_scan"
            | "boundary_scanner_locale"
            | "fragmenter"
            | "encoder"
            | "force_source"
            | "phrase_limit"
            | "max_analyzed_offset"
            | "fragment_offset"
            | "options"
            | "max_fragment_length" => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas [{cle}] dans [{ou}] : seul le surligneur par defaut \
                     ([type: unified], balises `<em>`, fragments dans l'ordre du document) est \
                     rendu"
                )))
            }
            autre => {
                return Err(EsError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "x_content_parse_exception",
                    format!("[{ou}] unknown field [{autre}]"),
                ))
            }
        }
    }
    lire_balises(obj, &mut r, ou)?;
    Ok(r)
}

/// `pre_tags` / `post_tags` / `tags_schema`.
///
/// Le surligneur par defaut d'ES n'utilise que la **premiere** balise de chaque
/// liste (mesure : deux paires fournies, une seule employee). Il refuse en
/// revanche une liste sans son pendant, et une liste vide — deux fautes de
/// forme, pas des defauts.
fn lire_balises(obj: &Map<String, Value>, r: &mut Reglages, ou: &str) -> EsResult<()> {
    if let Some(v) = obj.get("tags_schema") {
        match v.as_str() {
            Some("styled") => {
                r.pre = "<em class=\"hlt1\">".into();
                r.post = "</em>".into();
            }
            Some("default") => {
                r.pre = "<em>".into();
                r.post = "</em>".into();
            }
            _ => {
                return Err(EsError::illegal_argument(format!(
                    "[{ou}.tags_schema] : seuls [default] et [styled] existent"
                )))
            }
        }
    }
    let liste = |cle: &str| -> EsResult<Option<Vec<String>>> {
        match obj.get(cle) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(vec![s.clone()])),
            Some(Value::Array(a)) => a
                .iter()
                .map(|x| {
                    x.as_str().map(str::to_string).ok_or_else(|| {
                        EsError::illegal_argument(format!("[{ou}.{cle}] : chaines attendues"))
                    })
                })
                .collect::<EsResult<Vec<_>>>()
                .map(Some),
            Some(_) => Err(EsError::illegal_argument(format!(
                "[{ou}.{cle}] : chaine ou liste de chaines attendue"
            ))),
        }
    };
    let pre = liste("pre_tags")?;
    let post = liste("post_tags")?;
    match (&pre, &post) {
        (None, None) => return Ok(()),
        (Some(_), None) => {
            return Err(EsError::parsing(
                "pre_tags are set but post_tags are not set",
            ))
        }
        (None, Some(_)) => {
            return Err(EsError::parsing(
                "post_tags are set but pre_tags are not set",
            ))
        }
        (Some(a), Some(b)) => {
            if a.is_empty() || b.is_empty() {
                return Err(EsError::parsing("pre_tags or post_tags must not be empty"));
            }
        }
    }
    r.pre = pre.expect("pre present")[0].clone();
    r.post = post.expect("post present")[0].clone();
    Ok(())
}

fn lire_taille(v: &Value, cle: &str) -> EsResult<i64> {
    v.as_i64()
        .ok_or_else(|| EsError::illegal_argument(format!("[highlight.{cle}] : entier attendu")))
}

// ---------------------------------------------------------------------------
// Ce que la requete pose sur un champ
// ---------------------------------------------------------------------------

/// Ce qu'un terme du document doit verifier pour etre surligne.
#[derive(Debug, Clone)]
enum Predicat {
    Terme(String),
    /// `prefix`, `wildcard`, `regexp` : le motif est traduit par
    /// [`crate::regexp`] puis compile par **le meme automate** que celui que
    /// tantivy pose derriere `RegexQuery`. Passer par une autre bibliotheque
    /// d'expressions regulieres ferait surligner autre chose que ce que la
    /// clause a trouve.
    Motif(std::sync::Arc<tantivy_fst::Regex>),
    Approche {
        terme: Vec<char>,
        distance: usize,
        transpositions: bool,
    },
    Intervalle {
        bas: Option<(String, bool)>,
        haut: Option<(String, bool)>,
    },
}

impl Predicat {
    fn matche(&self, terme: &str) -> bool {
        match self {
            Self::Terme(t) => t == terme,
            Self::Motif(re) => accepte(re, terme),
            Self::Approche {
                terme: cible,
                distance,
                transpositions,
            } => {
                distance_edition(cible, &terme.chars().collect::<Vec<_>>(), *transpositions)
                    <= *distance
            }
            Self::Intervalle { bas, haut } => {
                bas.as_ref().is_none_or(|(b, incl)| {
                    if *incl {
                        terme >= b.as_str()
                    } else {
                        terme > b.as_str()
                    }
                }) && haut.as_ref().is_none_or(|(h, incl)| {
                    if *incl {
                        terme <= h.as_str()
                    } else {
                        terme < h.as_str()
                    }
                })
            }
        }
    }
}

/// Ce que la requete cherche dans un champ : un terme isole, ou une suite de
/// termes cote a cote.
#[derive(Debug, Clone)]
enum Motif {
    Simple(Predicat),
    /// Une phrase rend **une seule** marque, du debut du premier terme a la fin
    /// du dernier (mesure : `match_phrase: "le chat"` rend `<em>le chat</em>`,
    /// pas `<em>le</em> <em>chat</em>`).
    Phrase(Vec<Predicat>),
}

/// La distance d'edition de Levenshtein, avec ou sans transposition — celle que
/// tantivy applique a un `fuzzy`, donc celle qui dit quels termes la clause a
/// vraiment trouves.
fn distance_edition(a: &[char], b: &[char], transpositions: bool) -> usize {
    let (n, m) = (a.len(), b.len());
    let mut precedent_precedent: Vec<usize> = Vec::new();
    let mut precedent: Vec<usize> = (0..=m).collect();
    for i in 1..=n {
        let mut courant = vec![i; m + 1];
        for j in 1..=m {
            let cout = usize::from(a[i - 1] != b[j - 1]);
            courant[j] = (precedent[j] + 1)
                .min(courant[j - 1] + 1)
                .min(precedent[j - 1] + cout);
            if transpositions && i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                courant[j] = courant[j].min(precedent_precedent[j - 2] + 1);
            }
        }
        precedent_precedent = precedent;
        precedent = courant;
    }
    precedent[m]
}

// ---------------------------------------------------------------------------
// Resolution sur un mapping
// ---------------------------------------------------------------------------

/// Un champ a surligner, resolu sur **un** mapping.
#[derive(Debug, Clone)]
struct Champ {
    /// Le chemin rendu dans la reponse.
    chemin: String,
    /// Ou lire la valeur dans `_source` (un multi-field lit son parent).
    source: String,
    ty: FieldType,
    /// L'analyzer **d'indexation** : c'est lui qui a produit les termes de
    /// l'index, donc lui qui dit ou ils commencent dans le texte.
    analyzer: Analyzer,
    reglages: Reglages,
    motifs: Vec<Motif>,
}

/// Ce qu'une recherche surligne, resolu sur un mapping.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    champs: Vec<Champ>,
}

impl Plan {
    pub fn est_vide(&self) -> bool {
        self.champs.is_empty()
    }
}

/// Croise la demande avec le mapping d'un index et avec la requete.
///
/// Seuls les champs `text` et `keyword` sont retenus : ES ne surligne rien
/// d'autre, pas meme sous un motif `*` (mesure sur un `integer`, qui ne rend
/// aucune cle).
pub fn resoudre(demande: &Demande, query: Option<&Value>, gen: &Generation) -> EsResult<Plan> {
    let mut par_champ: BTreeMap<String, Vec<Motif>> = BTreeMap::new();
    let mut tous: Vec<Motif> = Vec::new();
    if let Some(q) = query {
        extraire(q, gen, &mut par_champ)?;
        for motifs in par_champ.values() {
            tous.extend(motifs.iter().cloned());
        }
    }

    // Une table, pas une liste : deux motifs peuvent designer le meme champ, et
    // c'est alors la derniere specification qui gagne — comme pour `fields`.
    let mut retenus: BTreeMap<String, Champ> = BTreeMap::new();
    for (motif, reglages) in &demande.champs {
        let joker = motif.contains('*');
        for (chemin, mapped) in &gen.fields.mapped {
            if !correspond(motif, chemin, joker) {
                continue;
            }
            if !matches!(mapped.ty.kind(), FieldKind::Text | FieldKind::Keyword) {
                continue;
            }
            let motifs = if reglages.champ_exige {
                par_champ.get(chemin).cloned().unwrap_or_default()
            } else {
                tous.clone()
            };
            retenus.insert(
                chemin.clone(),
                Champ {
                    source: chemin_source(chemin, gen),
                    chemin: chemin.clone(),
                    ty: mapped.ty,
                    analyzer: mapped.analyzer,
                    reglages: reglages.clone(),
                    motifs,
                },
            );
        }
    }
    Ok(Plan {
        champs: retenus.into_values().collect(),
    })
}

fn correspond(motif: &str, chemin: &str, joker: bool) -> bool {
    if joker {
        glob_match(motif, chemin)
    } else {
        motif == chemin
    }
}

/// Ou lire la valeur d'un champ dans `_source` — meme regle que pour `fields` :
/// un multi-field n'existe pas dans le document, sa valeur est celle du parent.
fn chemin_source(chemin: &str, gen: &Generation) -> String {
    if gen.mapping.properties.contains_key(chemin) {
        return chemin.to_string();
    }
    match chemin.rsplit_once('.') {
        Some((parent, _)) if gen.mapping.properties.contains_key(parent) => parent.to_string(),
        _ => chemin.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Extraction des motifs depuis le Query DSL
// ---------------------------------------------------------------------------

/// Parcourt la requete et note, champ par champ, ce qu'elle y cherche.
///
/// Le parcours refait celui de [`crate::dsl`] plutot que de relire la requete
/// tantivy : une `RegexQuery` ou une `FuzzyTermQuery` ne rend pas ses termes, et
/// un surlignage qui les oublierait serait muet la ou ES marque.
///
/// Ce qui **ne** contribue pas est aussi mesure : `match_all`, `match_none`,
/// `exists`, `ids`, `parent_id` et la branche `must_not` d'un `bool` ne
/// surlignent rien chez ES non plus.
fn extraire(v: &Value, gen: &Generation, out: &mut BTreeMap<String, Vec<Motif>>) -> EsResult<()> {
    let Some(obj) = v.as_object() else {
        return Ok(());
    };
    let Some((nom, corps)) = obj.iter().next().map(|(k, v)| (k.as_str(), v)) else {
        return Ok(());
    };
    match nom {
        "bool" => {
            if let Some(o) = corps.as_object() {
                for cle in ["must", "should", "filter"] {
                    match o.get(cle) {
                        Some(Value::Array(a)) => {
                            for c in a {
                                extraire(c, gen, out)?;
                            }
                        }
                        Some(c @ Value::Object(_)) => extraire(c, gen, out)?,
                        _ => {}
                    }
                }
            }
        }
        "constant_score" => {
            if let Some(f) = corps.as_object().and_then(|o| o.get("filter")) {
                extraire(f, gen, out)?;
            }
        }
        "dis_max" => {
            if let Some(Value::Array(a)) = corps.as_object().and_then(|o| o.get("queries")) {
                for c in a {
                    extraire(c, gen, out)?;
                }
            }
        }
        "nested" | "has_child" | "has_parent" => {
            if let Some(q) = corps.as_object().and_then(|o| o.get("query")) {
                extraire(q, gen, out)?;
            }
        }
        "match" | "match_phrase" | "match_phrase_prefix" => {
            let Some((champ, spec)) = premiere_cle(corps) else {
                return Ok(());
            };
            let texte = texte_de(spec);
            if let Some(t) = texte {
                pose(gen, champ, out, |mf| {
                    let termes = analyser(gen, mf.search_analyzer, &t);
                    Ok(match nom {
                        "match" => termes
                            .into_iter()
                            .map(|s| Motif::Simple(Predicat::Terme(s)))
                            .collect(),
                        "match_phrase" => phrase(termes, None),
                        _ => {
                            let mut p: Vec<String> = termes;
                            let dernier = p.pop();
                            let mut positions: Vec<Predicat> =
                                p.into_iter().map(Predicat::Terme).collect();
                            if let Some(d) = dernier {
                                positions.push(prefixe(&d, false)?);
                            }
                            vec![Motif::Phrase(positions)]
                        }
                    })
                })?;
            }
        }
        "multi_match" => {
            let Some(o) = corps.as_object() else {
                return Ok(());
            };
            let (Some(q), Some(Value::Array(champs))) = (o.get("query"), o.get("fields")) else {
                return Ok(());
            };
            let Some(t) = texte_de(q) else { return Ok(()) };
            let ty = o
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("best_fields");
            for spec in champs {
                let Some(spec) = spec.as_str() else { continue };
                let champ = spec.split_once('^').map_or(spec, |(n, _)| n);
                pose(gen, champ, out, |mf| {
                    let termes = analyser(gen, mf.search_analyzer, &t);
                    Ok(match ty {
                        "phrase" => phrase(termes, None),
                        "phrase_prefix" => {
                            let mut p = termes;
                            let dernier = p.pop();
                            let mut positions: Vec<Predicat> =
                                p.into_iter().map(Predicat::Terme).collect();
                            if let Some(d) = dernier {
                                positions.push(prefixe(&d, false)?);
                            }
                            vec![Motif::Phrase(positions)]
                        }
                        _ => termes
                            .into_iter()
                            .map(|s| Motif::Simple(Predicat::Terme(s)))
                            .collect(),
                    })
                })?;
            }
        }
        "term" => {
            let Some((champ, spec)) = premiere_cle(corps) else {
                return Ok(());
            };
            let valeur = valeur_de(spec);
            pose(gen, champ, out, |mf| {
                Ok(chaine(champ, mf.ty, valeur)
                    .map(|s| vec![Motif::Simple(Predicat::Terme(s))])
                    .unwrap_or_default())
            })?;
        }
        "terms" => {
            let Some((champ, Value::Array(vals))) = premiere_cle(corps) else {
                return Ok(());
            };
            pose(gen, champ, out, |mf| {
                Ok(vals
                    .iter()
                    .filter_map(|v| chaine(champ, mf.ty, Some(v)))
                    .map(|s| Motif::Simple(Predicat::Terme(s)))
                    .collect())
            })?;
        }
        "prefix" | "wildcard" | "regexp" => {
            let Some((champ, spec)) = premiere_cle(corps) else {
                return Ok(());
            };
            let Some(valeur) = valeur_de(spec).map(|v| match v {
                Value::String(s) => s.clone(),
                autre => autre.to_string(),
            }) else {
                return Ok(());
            };
            let insensible = spec
                .as_object()
                .and_then(|o| o.get("case_insensitive"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let flags = spec
                .as_object()
                .and_then(|o| o.get("flags"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let brut = match nom {
                "prefix" => format!(
                    "(?s){}(?s:.*)",
                    crate::regexp::litteral(&valeur, insensible)
                ),
                "wildcard" => format!("(?s){}", crate::regexp::joker(&valeur, insensible)),
                _ => crate::regexp::vers_regex(
                    &valeur,
                    match flags.as_deref() {
                        Some(f) => crate::regexp::Flags::lire(f)?,
                        None => crate::regexp::Flags::default(),
                    },
                    insensible,
                )?,
            };
            let re = compile(&brut)?;
            pose(gen, champ, out, |_| {
                Ok(vec![Motif::Simple(Predicat::Motif(re.clone()))])
            })?;
        }
        "fuzzy" => {
            let Some((champ, spec)) = premiere_cle(corps) else {
                return Ok(());
            };
            let Some(Value::String(valeur)) = valeur_de(spec) else {
                return Ok(());
            };
            let o = spec.as_object();
            let distance = match o.and_then(|o| o.get("fuzziness")) {
                Some(Value::Number(n)) => n.as_u64().unwrap_or(2).min(2) as usize,
                Some(Value::String(s)) if !s.eq_ignore_ascii_case("auto") => {
                    s.parse::<usize>().unwrap_or(2).min(2)
                }
                _ => match valeur.chars().count() {
                    0..=2 => 0,
                    3..=5 => 1,
                    _ => 2,
                },
            };
            let transpositions = o
                .and_then(|o| o.get("transpositions"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let p = Predicat::Approche {
                terme: valeur.chars().collect(),
                distance,
                transpositions,
            };
            pose(gen, champ, out, |_| Ok(vec![Motif::Simple(p.clone())]))?;
        }
        "range" => {
            let Some((champ, spec)) = premiere_cle(corps) else {
                return Ok(());
            };
            let Some(o) = spec.as_object() else {
                return Ok(());
            };
            pose(gen, champ, out, |mf| {
                // Seul un `keyword` a des termes comparables comme des chaines
                // ; sur une date ou un nombre, ES ne surligne rien (mesure).
                if mf.ty.kind() != FieldKind::Keyword {
                    return Ok(Vec::new());
                }
                let borne = |cles: [&str; 2]| -> Option<(String, bool)> {
                    o.get(cles[0])
                        .and_then(|v| chaine(champ, mf.ty, Some(v)))
                        .map(|s| (s, true))
                        .or_else(|| {
                            o.get(cles[1])
                                .and_then(|v| chaine(champ, mf.ty, Some(v)))
                                .map(|s| (s, false))
                        })
                };
                Ok(vec![Motif::Simple(Predicat::Intervalle {
                    bas: borne(["gte", "gt"]),
                    haut: borne(["lte", "lt"]),
                })])
            })?;
        }
        // `match_all`, `match_none`, `exists`, `ids`, `parent_id` : rien a
        // surligner, chez ES non plus.
        _ => {}
    }
    Ok(())
}

/// Ajoute les motifs d'une clause au champ vise, si ce mapping le connait et
/// qu'il est surlignable.
fn pose(
    gen: &Generation,
    champ: &str,
    out: &mut BTreeMap<String, Vec<Motif>>,
    f: impl FnOnce(&mapping::MappedField) -> EsResult<Vec<Motif>>,
) -> EsResult<()> {
    let Some(mf) = gen.fields.get(champ) else {
        return Ok(());
    };
    if !matches!(mf.ty.kind(), FieldKind::Text | FieldKind::Keyword) {
        return Ok(());
    }
    let motifs = f(&mf)?;
    if !motifs.is_empty() {
        out.entry(champ.to_string()).or_default().extend(motifs);
    }
    Ok(())
}

fn phrase(termes: Vec<String>, _slop: Option<u32>) -> Vec<Motif> {
    if termes.is_empty() {
        return Vec::new();
    }
    vec![Motif::Phrase(
        termes.into_iter().map(Predicat::Terme).collect(),
    )]
}

fn prefixe(valeur: &str, insensible: bool) -> EsResult<Predicat> {
    let brut = format!("(?s){}(?s:.*)", crate::regexp::litteral(valeur, insensible));
    Ok(Predicat::Motif(compile(&brut)?))
}

/// Le motif d'un terme est **ancre** : l'automate de tantivy-fst l'est par
/// construction — il accepte ou non le terme **entier**, il ne cherche pas
/// dedans.
fn compile(brut: &str) -> EsResult<std::sync::Arc<tantivy_fst::Regex>> {
    tantivy_fst::Regex::new(brut)
        .map(std::sync::Arc::new)
        .map_err(|e| EsError::internal(format!("motif de surlignage illisible : {e}")))
}

/// Fait courir l'automate sur un terme, exactement comme le fait tantivy quand
/// il parcourt le dictionnaire.
fn accepte(re: &tantivy_fst::Regex, terme: &str) -> bool {
    use tantivy_fst::Automaton;
    let mut etat = re.start();
    for b in terme.as_bytes() {
        etat = re.accept(&etat, *b);
        if !re.can_match(&etat) {
            return false;
        }
    }
    re.is_match(&etat)
}

fn premiere_cle(v: &Value) -> Option<(&str, &Value)> {
    v.as_object()
        .and_then(|o| o.iter().next())
        .map(|(k, v)| (k.as_str(), v))
}

/// La valeur d'une clause `{champ: valeur}` ou `{champ: {value: …}}`.
fn valeur_de(spec: &Value) -> Option<&Value> {
    match spec {
        Value::Object(o) => o.get("value").or_else(|| o.get("query")),
        autre => Some(autre),
    }
}

fn texte_de(spec: &Value) -> Option<String> {
    let v = valeur_de(spec)?;
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
        autre => Some(autre.to_string()),
    }
}

/// La forme **indexee** d'une valeur sur un champ `keyword` ou `text`.
fn chaine(champ: &str, ty: FieldType, v: Option<&Value>) -> Option<String> {
    match mapping::coerce(champ, ty, v?).ok()? {
        mapping::TypedValue::Str(s) => Some(s),
        _ => None,
    }
}

fn analyser(gen: &Generation, analyzer: Analyzer, texte: &str) -> Vec<String> {
    let Some(mut ta) = gen.index.tokenizers().get(&analyzer.tokenizer()) else {
        return Vec::new();
    };
    let mut flux = ta.token_stream(texte);
    let mut out = Vec::new();
    while flux.advance() {
        out.push(flux.token().text.clone());
    }
    out
}

// ---------------------------------------------------------------------------
// Rendu
// ---------------------------------------------------------------------------

/// Le bloc `highlight` d'un hit, ou `None` s'il n'y a rien a rendre.
///
/// Un champ sans correspondance est **absent** de la reponse (et non une chaine
/// vide), sauf `no_match_size` ; un bloc entierement vide n'apparait pas.
pub fn rendre(plan: &Plan, gen: &Generation, source: &Value) -> EsResult<Option<Value>> {
    if plan.est_vide() {
        return Ok(None);
    }
    let mut bloc = Map::new();
    for champ in &plan.champs {
        let valeurs = valeurs_du_source(source, &champ.source, champ.ty);
        if valeurs.is_empty() {
            continue;
        }
        let fragments = fragments_du_champ(champ, gen, &valeurs);
        if !fragments.is_empty() {
            bloc.insert(
                champ.chemin.clone(),
                Value::Array(fragments.into_iter().map(Value::String).collect()),
            );
        }
    }
    Ok((!bloc.is_empty()).then(|| Value::Object(bloc)))
}

/// Les valeurs textuelles d'un chemin pointe, tableaux traverses — c'est le
/// meme parcours que `fields`, et il fait tomber les sous-champs d'un `nested`
/// dans l'ordre du document.
fn valeurs_du_source(source: &Value, chemin: &str, ty: FieldType) -> Vec<String> {
    let mut brutes = Vec::new();
    descendre(source, chemin, &mut brutes);
    brutes
        .into_iter()
        .filter_map(|v| match v {
            Value::String(s) => Some(s.clone()),
            // ES surligne aussi ce qu'il a converti en texte a l'indexation.
            Value::Number(_) | Value::Bool(_) => match mapping::coerce("", ty, v).ok()? {
                mapping::TypedValue::Str(s) => Some(s),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn descendre<'a>(v: &'a Value, chemin: &str, out: &mut Vec<&'a Value>) {
    if chemin.is_empty() {
        return;
    }
    match v {
        Value::Array(a) => a.iter().for_each(|e| descendre(e, chemin, out)),
        Value::Object(o) => match chemin.split_once('.') {
            Some((tete, reste)) => {
                if let Some(sous) = o.get(tete) {
                    descendre(sous, reste, out);
                }
            }
            None => {
                if let Some(x) = o.get(chemin) {
                    aplatir(x, out);
                }
            }
        },
        _ => {}
    }
}

fn aplatir<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    match v {
        Value::Array(a) => a.iter().for_each(|e| aplatir(e, out)),
        Value::Null => {}
        autre => out.push(autre),
    }
}

/// Un fragment candidat, avant selection.
struct Fragment {
    /// Le decalage global (valeurs concatenees) — il sert au classement.
    depart: usize,
    score: f64,
    texte: String,
}

fn fragments_du_champ(champ: &Champ, gen: &Generation, valeurs: &[String]) -> Vec<String> {
    // La longueur « du champ » chez ES est celle de ses valeurs mises bout a
    // bout, separateur compris : elle entre dans le score des fragments.
    let longueur: usize =
        valeurs.iter().map(|v| v.chars().count()).sum::<usize>() + valeurs.len().saturating_sub(1);

    let mut candidats: Vec<Fragment> = Vec::new();
    let mut base = 0usize;
    for valeur in valeurs {
        let chars: Vec<char> = valeur.chars().collect();
        let marques = marques(champ, gen, valeur, &chars);
        if !marques.is_empty() {
            for f in decouper(champ, &chars, &marques, base, longueur) {
                candidats.push(f);
            }
        }
        base += chars.len() + 1;
    }

    if candidats.is_empty() {
        return sans_correspondance(champ, valeurs);
    }
    // Plus de fragments que demande : Lucene garde les mieux notes, puis les
    // remet dans l'ordre du document.
    if champ.reglages.nb_fragments > 0 && candidats.len() > champ.reglages.nb_fragments {
        candidats.sort_by(|a, b| {
            a.score
                .total_cmp(&b.score)
                .then_with(|| a.depart.cmp(&b.depart))
        });
        candidats.drain(..candidats.len() - champ.reglages.nb_fragments);
        candidats.sort_by_key(|f| f.depart);
    }
    candidats.into_iter().map(|f| f.texte).collect()
}

/// `no_match_size` : le debut de la **premiere** valeur, etendu a la frontiere
/// de mot qui suit.
fn sans_correspondance(champ: &Champ, valeurs: &[String]) -> Vec<String> {
    let n = champ.reglages.sans_correspondance;
    if n == 0 {
        return Vec::new();
    }
    let Some(premiere) = valeurs.first() else {
        return Vec::new();
    };
    let chars: Vec<char> = premiere.chars().collect();
    let fin = if n < chars.len() {
        crate::segments::suivante(&crate::segments::mots(&chars), n)
    } else {
        chars.len()
    };
    let (debut, fin) = rogner(&chars, 0, fin);
    if debut >= fin {
        return Vec::new();
    }
    vec![chars[debut..fin].iter().collect()]
}

/// Les correspondances d'un texte, en indices de `char`, triees et sans
/// doublon.
fn marques(champ: &Champ, gen: &Generation, valeur: &str, chars: &[char]) -> Vec<(usize, usize)> {
    if champ.motifs.is_empty() {
        return Vec::new();
    }
    let jetons = jetons(champ, gen, valeur, chars);
    let mut out: Vec<(usize, usize)> = Vec::new();
    for motif in &champ.motifs {
        match motif {
            Motif::Simple(p) => {
                for j in &jetons {
                    if p.matche(&j.texte) {
                        out.push((j.debut, j.fin));
                    }
                }
            }
            Motif::Phrase(positions) => {
                for suite in suites(&jetons, positions) {
                    out.push(suite);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    // Deux marques qui se chevauchent ne sont pas fusionnees par Lucene : le
    // formateur avance sur `pos` et n'en rend que ce qui depasse. Le tri suffit.
    out
}

struct Jeton {
    texte: String,
    debut: usize,
    fin: usize,
    position: usize,
}

/// Les termes du texte, avec leurs bornes en `char`.
///
/// Un `keyword` n'est pas analyse : sa valeur entiere est le terme, et ES la
/// surligne d'un bloc.
fn jetons(champ: &Champ, gen: &Generation, valeur: &str, chars: &[char]) -> Vec<Jeton> {
    if champ.ty.kind() == FieldKind::Keyword {
        return vec![Jeton {
            texte: valeur.to_string(),
            debut: 0,
            fin: chars.len(),
            position: 0,
        }];
    }
    let Some(mut ta) = gen.index.tokenizers().get(&champ.analyzer.tokenizer()) else {
        return Vec::new();
    };
    // Les offsets de tantivy sont en octets ; ceux de Lucene en caracteres.
    let mut octet_vers_char = vec![0usize; valeur.len() + 1];
    for (i, (o, _)) in valeur.char_indices().enumerate() {
        octet_vers_char[o] = i;
    }
    octet_vers_char[valeur.len()] = chars.len();
    for i in 1..=valeur.len() {
        if octet_vers_char[i] == 0 && i != valeur.len() {
            octet_vers_char[i] = octet_vers_char[i - 1];
        }
    }

    let mut flux = ta.token_stream(valeur);
    let mut out = Vec::new();
    while flux.advance() {
        let t = flux.token();
        out.push(Jeton {
            texte: t.text.clone(),
            debut: octet_vers_char[t.offset_from.min(valeur.len())],
            fin: octet_vers_char[t.offset_to.min(valeur.len())],
            position: t.position,
        });
    }
    out
}

/// Les suites de jetons a positions consecutives qui verifient chaque predicat
/// de la phrase, dans l'ordre.
fn suites(jetons: &[Jeton], positions: &[Predicat]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if positions.is_empty() {
        return out;
    }
    for (i, premier) in jetons.iter().enumerate() {
        if !positions[0].matche(&premier.texte) {
            continue;
        }
        let mut fin = premier.fin;
        let mut ok = true;
        for (attendue, p) in (premier.position + 1..).zip(positions[1..].iter()) {
            match jetons[i + 1..]
                .iter()
                .find(|j| j.position == attendue && p.matche(&j.texte))
            {
                Some(j) => fin = fin.max(j.fin),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            out.push((premier.debut, fin));
        }
    }
    out
}

/// Le decoupeur : celui d'Elasticsearch, etat compris.
///
/// Il est appele une fois par correspondance qui ouvre un nouveau fragment, et
/// **garde son etat** entre deux appels : c'est ce qui fait qu'un second
/// fragment de la meme phrase reprend la ou le precedent s'est arrete.
struct Decoupeur<'a> {
    phrases: &'a [usize],
    mots: &'a [usize],
    /// `None` = `fragment_size: 0`, une phrase entiere par fragment.
    borne: Option<usize>,
    fenetre: (usize, usize),
    interne: (usize, usize),
    ouvert: bool,
}

impl Decoupeur<'_> {
    /// Rend `(debut, fin)` du fragment qui accueille une correspondance
    /// commencant a `depart`.
    fn autour(&mut self, depart: usize, arrivee: usize) -> (usize, usize) {
        // Le fragment se centre sur le **milieu** de la correspondance, pas sur
        // son debut : sur un mot isole les deux se confondent, sur une phrase
        // non — et c'est ce qui decale tout le fragment d'ES d'une phrase de
        // quatre mots. Mesure : la meme phrase a 1, 2, 3 puis 4 mots, avec
        // `fragment_size` de 1 a 39, donne un bord gauche qui suit
        // `(debut + fin) / 2 - fragment_size`, jamais `debut`.
        let offset = (depart + arrivee) / 2;
        if self.ouvert && offset > self.fenetre.0 && offset < self.fenetre.1 {
            self.interne = (self.interne.1, self.fenetre.1);
        } else {
            let f = self.fenetre_de(offset);
            self.fenetre = f;
            self.interne = f;
            self.ouvert = true;
        }
        if let Some(max) = self.borne {
            if self.interne.1 - self.interne.0 > max {
                // A gauche : la frontiere de mot qui precede la place restante.
                if offset > max && offset - max > self.interne.0 {
                    self.interne.0 = self
                        .interne
                        .0
                        .max(crate::segments::precedente(self.mots, offset - max));
                }
                // A droite : ce que la borne laisse encore.
                let restant = max as i64 - (offset as i64 - self.interne.0 as i64);
                let cible = if restant > 1 {
                    offset + restant as usize
                } else {
                    offset
                };
                self.interne.1 = self
                    .interne
                    .1
                    .min(crate::segments::suivante(self.mots, cible));
            }
        }
        // Un fragment contient toujours la correspondance entiere, meme quand
        // la borne est plus courte qu'elle : `fragment_size: 1` sur une phrase
        // de quatre mots rend la phrase, pas son milieu.
        self.interne.0 = self.interne.0.min(depart);
        self.interne.1 = self.interne.1.max(arrivee);
        self.interne
    }

    /// La fenetre : la phrase qui contient `offset`, puis les suivantes tant
    /// que la longueur totale tient sous la borne.
    fn fenetre_de(&self, offset: usize) -> (usize, usize) {
        let i = match self.phrases.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let debut = self.phrases[i.min(self.phrases.len() - 1)];
        let mut fin = self.phrases[(i + 1).min(self.phrases.len() - 1)];
        if let Some(max) = self.borne {
            for &b in &self.phrases[i + 2..] {
                if b - debut > max {
                    break;
                }
                fin = b;
            }
        }
        (debut, fin)
    }
}

/// Rogne les blancs des deux bords : ES ne rend jamais un fragment qui
/// commence ou finit par une espace, alors que les frontieres de phrase, elles,
/// les emportent.
fn rogner(chars: &[char], mut debut: usize, mut fin: usize) -> (usize, usize) {
    while debut < fin && chars[debut].is_whitespace() {
        debut += 1;
    }
    while fin > debut && chars[fin - 1].is_whitespace() {
        fin -= 1;
    }
    (debut, fin)
}

fn decouper(
    champ: &Champ,
    chars: &[char],
    marques: &[(usize, usize)],
    base: usize,
    longueur: usize,
) -> Vec<Fragment> {
    let phrases = crate::segments::phrases(chars);
    let mots = crate::segments::mots(chars);
    let mut d = Decoupeur {
        phrases: &phrases,
        mots: &mots,
        borne: (champ.reglages.taille > 0).then_some(champ.reglages.taille),
        fenetre: (0, 0),
        interne: (0, 0),
        ouvert: false,
    };
    // `number_of_fragments: 0` : la valeur entiere, d'un bloc.
    let entier = champ.reglages.nb_fragments == 0;

    /// Un fragment en cours : ses bornes, et les marques qu'il porte.
    type Groupe = (usize, usize, Vec<(usize, usize)>);
    let mut groupes: Vec<Groupe> = Vec::new();
    for &(deb, fin) in marques {
        let ouvrir = match groupes.last() {
            None => true,
            Some((_, f, _)) => deb >= *f,
        };
        if ouvrir {
            let bornes = if entier {
                (0, chars.len())
            } else {
                d.autour(deb, fin)
            };
            groupes.push((bornes.0, bornes.1, Vec::new()));
        }
        let g = groupes.last_mut().expect("un groupe vient d'etre ouvert");
        if deb < g.1 {
            g.2.push((deb.max(g.0), fin.min(g.1)));
        }
    }

    groupes
        .into_iter()
        .filter(|(_, _, m)| !m.is_empty())
        .map(|(deb, fin, m)| {
            let (deb, fin) = rogner(chars, deb, fin);
            Fragment {
                depart: base + deb,
                score: note(&m, chars, deb, fin, base, longueur),
                texte: formater(champ, chars, deb, fin, &m),
            }
        })
        .collect()
}

/// Le `PassageScorer` de Lucene : un BM25 dont le « document » est le fragment
/// et le « corpus » le champ, pivote sur 87 caracteres.
fn note(
    dans: &[(usize, usize)],
    chars: &[char],
    debut: usize,
    fin: usize,
    base: usize,
    longueur: usize,
) -> f64 {
    let taille = (fin - debut) as f64;
    // Une marque compte par le **terme** qu'elle porte : deux occurrences du
    // meme mot dans un fragment pesent moins que deux mots differents.
    let mut par_terme: BTreeMap<String, usize> = BTreeMap::new();
    for (d, f) in dans {
        *par_terme.entry(chars[*d..*f].iter().collect()).or_default() += 1;
    }
    // Le nombre d'occurrences du terme dans le champ **vaut 1** : c'est ce que
    // rend `OffsetsEnum.freq()` quand le surligneur travaille sur les
    // `Matches` de Lucene, qui ne les comptent pas. Le poids est donc le meme
    // pour tous les fragments, et ce qui les separe est le reste. Prendre le
    // vrai compte le rendrait **negatif** des qu'un terme apparait plus de
    // trois fois — et un score negatif inverse le classement, donc rend les
    // derniers fragments a la place des premiers (mesure : `number_of_fragments:
    // 5` sur un texte a sept passages rendait les passages 2 a 6 au lieu de 0 a 4).
    let nb_docs = 1.0 + (longueur as f64 / PIVOT).floor();
    let poids = (K1 + 1.0) * (1.0 + (nb_docs - 1.0 + 0.5) / 1.5).ln();
    let norme = K1 * ((1.0 - B) + B * taille / PIVOT);
    let mut score = 0.0;
    for tf in par_terme.into_values() {
        score += poids * (tf as f64 / (tf as f64 + norme));
    }
    score * (1.0 + 1.0 / ((PIVOT + (base + debut) as f64).ln()))
}

/// Le formateur de Lucene : le texte du fragment, chaque correspondance
/// encadree. Deux marques qui se chevauchent n'en font qu'une.
fn formater(
    champ: &Champ,
    chars: &[char],
    debut: usize,
    fin: usize,
    marques: &[(usize, usize)],
) -> String {
    let mut out = String::new();
    let mut pos = debut;
    for &(d, f) in marques {
        if d > pos {
            out.extend(&chars[pos..d.min(fin)]);
        }
        if f > pos {
            out.push_str(&champ.reglages.pre);
            out.extend(&chars[pos.max(d).min(fin)..f.min(fin)]);
            out.push_str(&champ.reglages.post);
            pos = f;
        }
    }
    if pos < fin {
        out.extend(&chars[pos..fin]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defauts_du_bloc() {
        let d = lire(&json!({"fields": {"corps": {}}})).unwrap();
        assert_eq!(d.champs.len(), 1);
        assert_eq!(d.champs[0].0, "corps");
        assert_eq!(d.champs[0].1, Reglages::default());
    }

    #[test]
    fn surcharge_par_champ() {
        let d = lire(&json!({
            "pre_tags": ["<b>"], "post_tags": ["</b>"], "fragment_size": 30,
            "fields": {"a": {"fragment_size": 10}, "b": {}}
        }))
        .unwrap();
        let par_nom: BTreeMap<_, _> = d.champs.iter().cloned().collect();
        assert_eq!(par_nom["a"].taille, 10);
        assert_eq!(par_nom["b"].taille, 30);
        assert_eq!(par_nom["a"].pre, "<b>");
    }

    /// La forme heritee : `fields` en liste d'objets a une cle.
    #[test]
    fn fields_en_liste() {
        let d = lire(&json!({"fields": [{"corps": {}}, {"titre": {}}]})).unwrap();
        assert_eq!(d.champs.len(), 2);
    }

    #[test]
    fn balises_incompletes_refusees() {
        let e = lire(&json!({"pre_tags": ["<b>"], "fields": {}})).unwrap_err();
        assert_eq!(e.reason, "pre_tags are set but post_tags are not set");
        let e = lire(&json!({"pre_tags": [], "post_tags": [], "fields": {}})).unwrap_err();
        assert_eq!(e.reason, "pre_tags or post_tags must not be empty");
    }

    #[test]
    fn ce_qui_est_refuse_se_nomme() {
        for cle in [
            "type",
            "highlight_query",
            "matched_fields",
            "boundary_scanner",
        ] {
            let e = lire(&json!({ cle: "x", "fields": {} })).unwrap_err();
            assert!(e.reason.contains(cle), "{cle} : {}", e.reason);
        }
        let e = lire(&json!({"order": "score", "fields": {}})).unwrap_err();
        assert!(e.reason.contains("[order]"), "{}", e.reason);
        let e = lire(&json!({"nawak": 1, "fields": {}})).unwrap_err();
        assert_eq!(e.reason, "[highlight] unknown field [nawak]");
    }

    /// Une taille de fragment negative retombe sur le defaut, comme chez ES.
    #[test]
    fn taille_negative_vaut_le_defaut() {
        let d = lire(&json!({"fragment_size": -1, "fields": {"a": {}}})).unwrap();
        assert_eq!(d.champs[0].1.taille, 100);
    }

    #[test]
    fn distance_avec_et_sans_transposition() {
        let ab: Vec<char> = "ab".chars().collect();
        let ba: Vec<char> = "ba".chars().collect();
        assert_eq!(distance_edition(&ab, &ba, true), 1);
        assert_eq!(distance_edition(&ab, &ba, false), 2);
    }

    /// Le cas qui a fixe la formule du decoupeur : trois correspondances dans
    /// **une seule** phrase (le point suivi d'une minuscule ne coupe pas), avec
    /// `fragment_size: 10`. Mesure contre ES 8.15.
    #[test]
    fn trois_fragments_dans_une_phrase() {
        let texte = "zzz cible. aaa. bbb cible cible.";
        let chars: Vec<char> = texte.chars().collect();
        let phrases = crate::segments::phrases(&chars);
        let mots = crate::segments::mots(&chars);
        let mut d = Decoupeur {
            phrases: &phrases,
            mots: &mots,
            borne: Some(10),
            fenetre: (0, 0),
            interne: (0, 0),
            ouvert: false,
        };
        assert_eq!(d.autour(4, 9), (0, 11));
        assert_eq!(d.autour(20, 25), (11, 25));
        assert_eq!(d.autour(26, 31), (25, 32));
    }

    /// La borne gauche : la sonde qui a fixe `offset - maxLen + 1`.
    #[test]
    fn borne_gauche_au_mot() {
        let texte = "aaaaaaaaaa bbbb cible cccc dddddddd";
        let chars: Vec<char> = texte.chars().collect();
        let phrases = crate::segments::phrases(&chars);
        let mots = crate::segments::mots(&chars);
        let coupe = |max: usize| {
            let mut d = Decoupeur {
                phrases: &phrases,
                mots: &mots,
                borne: Some(max),
                fenetre: (0, 0),
                interne: (0, 0),
                ouvert: false,
            };
            let (a, b) = d.autour(16, 21);
            let (a, b) = rogner(&chars, a, b);
            chars[a..b].iter().collect::<String>()
        };
        assert_eq!(coupe(1), "cible");
        assert_eq!(coupe(2), "cible");
        assert_eq!(coupe(3), "bbbb cible");
        assert_eq!(coupe(7), "bbbb cible");
        assert_eq!(coupe(8), "aaaaaaaaaa bbbb cible");
        assert_eq!(coupe(22), "aaaaaaaaaa bbbb cible cccc");
    }
}
