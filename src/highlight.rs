//! `highlight` : les fragments surlignes d'une barre de recherche.
//!
//! Ce que ce module doit reproduire n'est pas « couper le texte autour du mot
//! trouve » : c'est le `UnifiedHighlighter` de Lucene tel qu'Elasticsearch le
//! configure, et sa forme n'etait devinable nulle part. Trois pieces, mesurees
//! une par une contre un ES 8.15 par
//! [`tests/compat/diff_highlight.py`](../tests/compat/diff_highlight.py) :
//!
//! - **ce qui est surligne** : non pas « les termes de la requete » mais ce qui
//!   a fait correspondre **ce document-la**, sur *ce champ-la*
//!   (`require_field_match`, vrai par defaut). C'est la raison pour laquelle
//!   [`Noeud`] garde la forme booleenne de la requete et l'evalue document par
//!   document. Une phrase rend **une seule** marque qui couvre toute la suite,
//!   pas une par terme ;
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
//!
//! Les **bords** sont la quatrieme piece, et celle qui a coute le plus de
//! mesures : un fragment est rogne au sens du `String.trim()` de Java (donc
//! jusqu'a U+0020, pas au sens d'Unicode), il ne l'est pas du tout a
//! `number_of_fragments: 0`, le rognage ne mord jamais sur une marque, et deux
//! marques qui **se chevauchent** n'en font qu'une la ou deux marques qui se
//! **touchent** en restent deux.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::analysis::Analyzer;
use crate::dateformat::DateFormat;
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
}

impl Default for Reglages {
    fn default() -> Self {
        Self {
            pre: "<em>".into(),
            post: "</em>".into(),
            nb_fragments: 5,
            taille: 100,
            sans_correspondance: 0,
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
            // `true` est le defaut : n'utiliser que les termes poses sur ce
            // champ-la. `false` fait chercher chez ES les termes de **toutes**
            // les clauses dans **tous** les champs, par une extraction qui
            // n'est pas celle du mode normal — ES le documente lui-meme comme
            // approximatif, et ferrite n'en reproduit pas tous les cas. Un
            // refus se voit ; un fragment silencieusement different, non.
            "require_field_match" => match v.as_bool() {
                Some(true) => {}
                Some(false) => {
                    return Err(EsError::unsupported(format!(
                        "ferrite ne supporte pas [require_field_match: false] dans [{ou}] : ES y \
                         cherche les termes de toutes les clauses dans tous les champs, par une \
                         extraction dont il dit lui-meme qu'elle est approximative"
                    )))
                }
                None => {
                    return Err(EsError::illegal_argument(format!(
                        "[{ou}.require_field_match] : booleen attendu"
                    )))
                }
            },
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

/// La requete, reduite a ce qu'elle **marque** — et a sa forme booleenne.
///
/// Garder la forme n'est pas un luxe : ES ne surligne que ce qui a vraiment
/// fait correspondre **ce document-la**. Un `should` place dans un `bool` dont
/// le `filter` echoue ne marque rien, et un `bool` porteur d'un
/// `must_not: {match_all}` ne marque jamais rien du tout. Une extraction a plat
/// marquerait les deux, en 200 (trouve par le fuzzer, graines 6 et 106).
#[derive(Debug, Clone)]
enum Noeud {
    /// Une clause sur un champ surlignable.
    Feuille {
        champ: String,
        motifs: Vec<Motif>,
    },
    /// `must` + `filter`, `constant_score` : tout doit tenir.
    Et(Vec<Noeud>),
    /// `should`, `dis_max` : au moins `minimum` doivent tenir.
    Ou {
        enfants: Vec<Noeud>,
        minimum: usize,
    },
    Non(Box<Noeud>),
    /// Une clause que le surlignage ne sait pas trancher depuis le `_source`
    /// (un intervalle de dates, `exists`, `ids`, une jointure). Elle ne marque
    /// rien, et ne fait echouer personne : dans le doute on marque.
    Opaque,
    /// `match_none`, ou un `must_not` qui prend tout.
    Jamais,
    /// `match_all` : vrai partout. Sous un `must_not`, c'est lui qui rend le
    /// `bool` sterile — et ferrite marquait quand meme ses `should`.
    Toujours,
    /// `term` / `terms` sur un champ qui n'est pas surlignable (un nombre, une
    /// date, un booleen) : il ne marque rien, mais il se tranche sur le
    /// `_source`, et un `filter` qui tombe fait taire tout le `bool` — la
    /// meme histoire qu'[`Noeud::Existe`] (trouve par le fuzzer, graine
    /// 6260200).
    Valeurs {
        champ: String,
        ty: FieldType,
        format: Option<DateFormat>,
        attendues: Vec<mapping::TypedValue>,
    },
    /// `range` sur un champ qui n'est pas surlignable : meme role que
    /// [`Noeud::Valeurs`], avec des bornes au lieu d'une liste (fuzzer, graine
    /// 6260176).
    Intervalle {
        champ: String,
        ty: FieldType,
        format: Option<DateFormat>,
        bas: Option<(mapping::TypedValue, bool)>,
        haut: Option<(mapping::TypedValue, bool)>,
    },
    /// `ids` : il ne marque rien, mais il se tranche sur l'`_id` du hit — et un
    /// `must: {ids: …}` qui echoue fait taire tout le `bool` (fuzzer, graine
    /// 5500127).
    Ids(Vec<String>),
    /// `exists` : il ne marque rien, mais il se **tranche** sur le `_source`,
    /// et c'est ce qui compte. Un `must: {exists: b}` qui echoue fait taire
    /// tout le `bool` — le laisser opaque marquait ses voisins (trouve par le
    /// fuzzer, graine 5150006).
    Existe(String),
    /// La clause interne d'un `nested` ou d'une jointure.
    ///
    /// Elle porte sur d'autres documents que celui qu'on surligne : ses termes
    /// marquent bien (ES surligne un sous-champ de `nested` depuis la racine),
    /// mais **rien** de sa structure ne se tranche sur ce `_source`-la. C'est le
    /// pendant manquant de la regle « dans le doute, marquer de trop » : a
    /// plat, un `must_not` vrai pour *un* element faisait taire ce qu'un
    /// *autre* element avait fait correspondre, et le document ressortait sans
    /// aucun fragment (fuzzer, graine 2196237).
    ///
    /// Ne pas couper la-dedans est sur : si le document est un hit, c'est qu'au
    /// moins un element a satisfait la clause interne — le `_source` a plat ne
    /// dit simplement pas lequel.
    Jointure(Box<Noeud>),
}

impl Noeud {
    fn et(enfants: Vec<Noeud>) -> Self {
        match enfants.len() {
            0 => Self::Opaque,
            1 => enfants.into_iter().next().expect("un enfant"),
            _ => Self::Et(enfants),
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
    ///
    /// Une position peut porter **plusieurs** alternatives : un filtre a
    /// n-grammes pose tous les grammes d'un mot au meme endroit, et la phrase
    /// correspond des qu'**une** d'entre elles correspond. C'est la
    /// `SynonymQuery` de Lucene, et sans elle une phrase d'un seul mot ne
    /// marquait rien sur un champ a n-grammes (fuzzer, graine 3141765).
    Phrase(Vec<Vec<Predicat>>),
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

/// Comment lire un champ : ou prendre ses valeurs, et comment les decouper en
/// termes. C'est ce qu'il faut pour **trouver** des marques, que le champ soit
/// surligne ou seulement interroge.
#[derive(Debug, Clone)]
struct Lecture {
    /// Le chemin rendu dans la reponse.
    chemin: String,
    /// Ou lire la valeur dans `_source` (un multi-field lit son parent).
    source: String,
    /// Les chemins qui **copient** dans ce champ (`copy_to`). Leur valeur n'est
    /// pas dans le `_source` de la cible, et ES la surligne quand meme : le
    /// champ sait de quels chemins du document il est fait (meme regle que
    /// pour `fields`, trouvee ici par le fuzzer, graine 89).
    copies: Vec<String>,
    ty: FieldType,
    /// `ignore_above` : une valeur plus longue n'a **pas ete indexee**, donc
    /// elle n'a rien a surligner — et `no_match_size` ne la rend pas non plus.
    /// Meme regle que pour `fields` : lire le `_source` n'est pas lire ce qui
    /// a ete indexe (trouve par le fuzzer, graine 52).
    ignore_above: Option<usize>,
    /// L'analyzer **d'indexation** : c'est lui qui a produit les termes de
    /// l'index, donc lui qui dit ou ils commencent dans le texte.
    analyzer: Analyzer,
}

/// Un champ a surligner, resolu sur **un** mapping.
#[derive(Debug, Clone)]
struct Champ {
    lecture: Lecture,
    reglages: Reglages,
}

/// Ce qu'une recherche surligne, resolu sur un mapping.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    champs: Vec<Champ>,
    /// La requete, gardee sous sa forme booleenne : ce qui marque depend du
    /// document (voir [`Noeud`]).
    arbre: Option<Noeud>,
    /// De quoi lire les champs que la requete cite, meme s'ils ne sont pas
    /// surlignes : sans eux, on ne peut pas savoir si une clause a tenu.
    lectures: BTreeMap<String, Lecture>,
}

impl Plan {
    pub fn est_vide(&self) -> bool {
        self.champs.is_empty()
    }
}

fn lecture_de(chemin: &str, mapped: &mapping::MappedField, gen: &Generation) -> Lecture {
    let source = chemin_source(chemin, gen);
    Lecture {
        copies: gen
            .fields
            .copiants
            .get(source.as_str())
            .cloned()
            .unwrap_or_default(),
        source,
        chemin: chemin.to_string(),
        ty: mapped.ty,
        ignore_above: mapped.ignore_above,
        analyzer: mapped.analyzer,
    }
}

/// Croise la demande avec le mapping d'un index et avec la requete.
///
/// Seuls les champs `text` et `keyword` sont retenus : ES ne surligne rien
/// d'autre, pas meme sous un motif `*` (mesure sur un `integer`, qui ne rend
/// aucune cle).
pub fn resoudre(demande: &Demande, query: Option<&Value>, gen: &Generation) -> EsResult<Plan> {
    let arbre = match query {
        Some(q) => Some(extraire(q, gen)?),
        None => None,
    };

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
            retenus.insert(
                chemin.clone(),
                Champ {
                    lecture: lecture_de(chemin, mapped, gen),
                    reglages: reglages.clone(),
                },
            );
        }
    }

    let mut lectures: BTreeMap<String, Lecture> = BTreeMap::new();
    if let Some(a) = &arbre {
        let mut cites = Vec::new();
        a.champs_cites(&mut cites);
        for nom in cites {
            if let Some(mapped) = gen.fields.mapped.get(&nom) {
                lectures.insert(nom.clone(), lecture_de(&nom, mapped, gen));
            }
        }
    }
    Ok(Plan {
        champs: retenus.into_values().collect(),
        arbre,
        lectures,
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
fn extraire(v: &Value, gen: &Generation) -> EsResult<Noeud> {
    let Some(obj) = v.as_object() else {
        return Ok(Noeud::Opaque);
    };
    let Some((nom, corps)) = obj.iter().next().map(|(k, v)| (k.as_str(), v)) else {
        return Ok(Noeud::Opaque);
    };
    Ok(match nom {
        "match_none" => Noeud::Jamais,
        "match_all" => Noeud::Toujours,
        "ids" => match corps.as_object().and_then(|o| o.get("values")) {
            Some(Value::Array(a)) => Noeud::Ids(
                a.iter()
                    .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                    .collect(),
            ),
            _ => Noeud::Opaque,
        },
        "exists" => match corps.as_object().and_then(|o| o.get("field")) {
            // Le chemin du `_source`, pas celui du champ : un multi-field
            // (`e.keyword`) n'existe pas dans le document, sa valeur est celle
            // de son parent. Lire `e.keyword` y trouvait toujours vide, donc
            // faisait tomber le `bool` qui le portait en `filter` (trouve par
            // le fuzzer, graine 4242005).
            // Seulement sur une **feuille** du mapping : sur la racine d'un
            // `nested` ou d'un `object`, les valeurs vivent ailleurs et le
            // `_source` ne tranche pas. On reste opaque plutot que de repondre
            // « il existe » parce que le `_source` porte un tableau (fuzzer,
            // graine 1234700).
            Some(Value::String(champ)) if gen.fields.mapped.contains_key(champ.as_str()) => {
                Noeud::Existe(chemin_source(champ, gen))
            }
            _ => Noeud::Opaque,
        },
        "bool" => {
            let Some(o) = corps.as_object() else {
                return Ok(Noeud::Opaque);
            };
            let branche = |cle: &str| -> EsResult<Vec<Noeud>> {
                match o.get(cle) {
                    Some(Value::Array(a)) => a.iter().map(|c| extraire(c, gen)).collect(),
                    Some(c @ Value::Object(_)) => Ok(vec![extraire(c, gen)?]),
                    _ => Ok(Vec::new()),
                }
            };
            let mut requis = branche("must")?;
            requis.extend(branche("filter")?);
            let obligatoire = !requis.is_empty();
            for n in branche("must_not")? {
                requis.push(Noeud::Non(Box::new(n)));
            }
            for n in branche(crate::dsl::MUST_NOT_CAMEL)? {
                requis.push(Noeud::Non(Box::new(n)));
            }
            let should = branche("should")?;
            if !should.is_empty() {
                // Le meme calcul que la clause elle-meme : un `should` n'est
                // facultatif que sous une clause obligatoire, et un minimum
                // explicite se resout avant (voir `crate::msm`).
                let defaut = usize::from(!obligatoire);
                let minimum =
                    crate::msm::resoudre(o.get("minimum_should_match"), should.len(), defaut)
                        .unwrap_or(defaut)
                        .max(defaut.min(1));
                requis.push(Noeud::Ou {
                    enfants: should,
                    minimum,
                });
            }
            Noeud::et(requis)
        }
        "constant_score" => match corps.as_object().and_then(|o| o.get("filter")) {
            Some(f) => extraire(f, gen)?,
            None => Noeud::Opaque,
        },
        // `function_score` ne marque que ce que **sa requete** a fait
        // correspondre : ses fonctions changent le score, pas les
        // correspondances. Les `filter` de ses `functions[]` ne marquent donc
        // rien non plus (mesure contre ES 8.15). Sans `query`, c'est un
        // `match_all`, qui ne marque rien.
        //
        // Et un `min_score`, meme a zero, fait **taire tout le sous-arbre** :
        // ES enveloppe alors son scorer d'un `MinScoreScorer`, dont le
        // `Weight` ne sait plus rendre de `Matches` — le surlignage y perd ses
        // termes, et seulement les siens (les clauses soeurs d'un `bool`
        // marquent toujours). Mesure contre ES 8.15, trouvee par le fuzzer.
        "function_score" => match corps.as_object() {
            Some(o) if o.get("min_score").is_some_and(|v| !v.is_null()) => Noeud::Toujours,
            Some(o) => match o.get("query") {
                Some(q) => extraire(q, gen)?,
                None => Noeud::Toujours,
            },
            None => Noeud::Opaque,
        },
        // `boosting` ne marque que son `positive` : `negative` deplace le
        // score, il ne fait correspondre personne. Mesure contre ES 8.15 —
        // `positive: beta, negative: alpha` ne marque que `beta`.
        "boosting" => match corps.as_object().and_then(|o| o.get("positive")) {
            Some(q) => extraire(q, gen)?,
            None => Noeud::Opaque,
        },
        "dis_max" => match corps.as_object().and_then(|o| o.get("queries")) {
            Some(Value::Array(a)) => Noeud::Ou {
                enfants: a
                    .iter()
                    .map(|c| extraire(c, gen))
                    .collect::<EsResult<_>>()?,
                minimum: 1,
            },
            _ => Noeud::Opaque,
        },
        // La clause interne d'un `nested` ou d'une jointure porte sur d'autres
        // documents que celui qu'on surligne : ses termes marquent bien (ES
        // surligne un sous-champ de `nested` depuis la racine), mais son
        // verdict ne se lit pas sur ce `_source`-la.
        "nested" | "has_child" | "has_parent" => {
            match corps.as_object().and_then(|o| o.get("query")) {
                Some(q) => Noeud::Jointure(Box::new(extraire(q, gen)?)),
                None => Noeud::Opaque,
            }
        }
        "match" | "match_phrase" | "match_phrase_prefix" => {
            let Some((champ, spec)) = premiere_cle(corps) else {
                return Ok(Noeud::Opaque);
            };
            let Some(t) = texte_de(spec) else {
                return Ok(Noeud::Opaque);
            };
            pose(gen, champ, false, |mf| motifs_de_texte(gen, mf, nom, &t))?
        }
        "multi_match" => {
            let Some(o) = corps.as_object() else {
                return Ok(Noeud::Opaque);
            };
            let (Some(q), Some(Value::Array(champs))) = (o.get("query"), o.get("fields")) else {
                return Ok(Noeud::Opaque);
            };
            let Some(t) = texte_de(q) else {
                return Ok(Noeud::Opaque);
            };
            let ty = o
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("best_fields");
            let clause = match ty {
                "phrase" => "match_phrase",
                "phrase_prefix" => "match_phrase_prefix",
                _ => "match",
            };
            let mut enfants = Vec::new();
            for spec in champs {
                let Some(spec) = spec.as_str() else { continue };
                let champ = spec.split_once('^').map_or(spec, |(n, _)| n);
                enfants.push(pose(gen, champ, false, |mf| {
                    motifs_de_texte(gen, mf, clause, &t)
                })?);
            }
            // Un `multi_match` correspond des qu'**un** de ses champs
            // correspond, quel que soit son type.
            Noeud::Ou {
                enfants,
                minimum: 1,
            }
        }
        "term" => {
            let Some((champ, spec)) = premiere_cle(corps) else {
                return Ok(Noeud::Opaque);
            };
            match valeur_de(spec) {
                // `term` ne marque **rien** sur un champ non indexe (mesure
                // contre ES 8.15) : le surlignage part des `Matches` de
                // Lucene, et une lecture de colonne n'en produit pas. `terms`,
                // lui, marque — c'est un automate, et Lucene l'extrait de la
                // requete sans passer par l'index. Deux clauses voisines, deux
                // reponses : c'est mesure, pas deduit.
                Some(v) => valeurs_posees(gen, champ, std::slice::from_ref(v), false)?,
                None => Noeud::Opaque,
            }
        }
        "terms" => {
            let Some((champ, Value::Array(vals))) = premiere_cle(corps) else {
                return Ok(Noeud::Opaque);
            };
            valeurs_posees(gen, champ, vals, true)?
        }
        "prefix" | "wildcard" | "regexp" => {
            let Some((champ, spec)) = premiere_cle(corps) else {
                return Ok(Noeud::Opaque);
            };
            let Some(valeur) = valeur_de(spec).map(|v| match v {
                Value::String(s) => s.clone(),
                autre => autre.to_string(),
            }) else {
                return Ok(Noeud::Opaque);
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
            pose(gen, champ, true, |_| {
                Ok(vec![Motif::Simple(Predicat::Motif(re.clone()))])
            })?
        }
        "fuzzy" => {
            let Some((champ, spec)) = premiere_cle(corps) else {
                return Ok(Noeud::Opaque);
            };
            let Some(Value::String(valeur)) = valeur_de(spec) else {
                return Ok(Noeud::Opaque);
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
            pose(gen, champ, true, |_| Ok(vec![Motif::Simple(p.clone())]))?
        }
        "range" => {
            let Some((champ, spec)) = premiere_cle(corps) else {
                return Ok(Noeud::Opaque);
            };
            let Some(o) = spec.as_object() else {
                return Ok(Noeud::Opaque);
            };
            let Some(mf) = gen.fields.get(champ) else {
                return Ok(Noeud::Opaque);
            };
            let format = gen.fields.format_de(champ).cloned();
            let borne = |cles: [&str; 2]| -> Option<(Value, bool)> {
                o.get(cles[0])
                    .map(|v| (v.clone(), true))
                    .or_else(|| o.get(cles[1]).map(|v| (v.clone(), false)))
            };
            let (bas, haut) = (borne(["gte", "gt"]), borne(["lte", "lt"]));
            // Seul un `keyword` a des termes comparables comme des chaines :
            // sur une date ou un nombre, ES ne surligne rien — mais la clause
            // se **tranche** quand meme, et un `filter` qui tombe fait taire
            // tout le `bool` (fuzzer, graine 6260176).
            let typee = |b: &Option<(Value, bool)>| -> Option<(mapping::TypedValue, bool)> {
                let (v, incl) = b.as_ref()?;
                Some((
                    mapping::coerce_avec(champ, mf.ty, v, format.as_ref()).ok()?,
                    *incl,
                ))
            };
            let (tb, th) = (typee(&bas), typee(&haut));
            // Une borne qu'on ne sait pas lire (du date math) rend la clause
            // opaque plutot que fausse.
            if (bas.is_some() && tb.is_none()) || (haut.is_some() && th.is_none()) {
                return Ok(Noeud::Opaque);
            }
            // Non indexe, un `range` ne marque rien non plus : ES construit
            // alors une requete sur la colonne, dont le surlignage ne tire
            // aucun automate (mesure contre 8.15 — c'est la meme regle que le
            // `term` de [`valeurs_posees`], et elle separe `range` de
            // `terms`). La clause se tranche quand meme, par
            // [`Noeud::Intervalle`].
            if mf.ty.kind() == FieldKind::Keyword && mf.indexe {
                let chaine_de = |b: &Option<(mapping::TypedValue, bool)>| match b {
                    Some((mapping::TypedValue::Str(s), incl)) => Some((s.clone(), *incl)),
                    _ => None,
                };
                Noeud::Feuille {
                    champ: champ.to_string(),
                    motifs: vec![Motif::Simple(Predicat::Intervalle {
                        bas: chaine_de(&tb),
                        haut: chaine_de(&th),
                    })],
                }
            } else {
                Noeud::Intervalle {
                    champ: chemin_source(champ, gen),
                    ty: mf.ty,
                    format,
                    bas: tb,
                    haut: th,
                }
            }
        }
        // `match_all`, `exists`, `ids`, `parent_id` : rien a surligner, chez ES
        // non plus — et rien a trancher sur ce `_source`.
        _ => Noeud::Opaque,
    })
}

/// `term` et `terms` : une feuille de marques si le champ est surlignable, une
/// feuille de **valeurs** sinon — elle ne marque rien mais elle se tranche, et
/// c'est ce qui fait taire un `bool` dont le `filter` porte sur une date.
fn valeurs_posees(
    gen: &Generation,
    champ: &str,
    vals: &[Value],
    marque_sans_index: bool,
) -> EsResult<Noeud> {
    let Some(mf) = gen.fields.get(champ) else {
        return Ok(Noeud::Opaque);
    };
    let format = gen.fields.format_de(champ).cloned();
    if matches!(mf.ty.kind(), FieldKind::Text | FieldKind::Keyword)
        && (mf.indexe || marque_sans_index)
    {
        let mut termes: Vec<String> = vals
            .iter()
            .filter_map(|v| chaine(champ, mf.ty, Some(v)))
            .collect();
        // Un `terms` qui cite deux fois la meme valeur ne cherche qu'un terme.
        termes.dedup();
        let motifs: Vec<Motif> = termes
            .into_iter()
            .map(|s| Motif::Simple(Predicat::Terme(s)))
            .collect();
        if motifs.is_empty() {
            return Ok(Noeud::Opaque);
        }
        return Ok(Noeud::Feuille {
            champ: champ.to_string(),
            motifs,
        });
    }
    // Un champ non indexe qui ne marque pas se tranche quand meme sur le
    // `_source`, exactement comme un champ numerique : c'est [`Noeud::Valeurs`].
    let mut attendues: Vec<mapping::TypedValue> = vals
        .iter()
        .filter_map(|v| mapping::coerce_avec(champ, mf.ty, v, format.as_ref()).ok())
        .collect();
    attendues.dedup();
    if attendues.is_empty() {
        return Ok(Noeud::Opaque);
    }
    Ok(Noeud::Valeurs {
        champ: chemin_source(champ, gen),
        ty: mf.ty,
        format,
        attendues,
    })
}

/// Une clause posee sur un champ : une feuille si ce mapping le connait et
/// qu'il est surlignable, `Opaque` sinon.
fn pose(
    gen: &Generation,
    champ: &str,
    marque_sans_index: bool,
    f: impl FnOnce(&mapping::MappedField) -> EsResult<Vec<Motif>>,
) -> EsResult<Noeud> {
    let Some(mf) = gen.fields.get(champ) else {
        return Ok(Noeud::Opaque);
    };
    if !matches!(mf.ty.kind(), FieldKind::Text | FieldKind::Keyword) {
        return Ok(Noeud::Opaque);
    }
    // Sur un champ `index: false`, seule la famille des **automates**
    // (`terms`, `prefix`, `wildcard`, `regexp`, `fuzzy`) marque : Lucene les
    // extrait de la requete et les pose sur le texte, sans rien demander a
    // l'index. Un `term`, un `match` ou un `range` n'y marquent rien, faute de
    // `Matches`. Mesure contre ES 8.15, trouvee par le fuzzer (graines 9310029
    // et 9310045).
    if !mf.indexe && !marque_sans_index {
        return Ok(Noeud::Opaque);
    }
    let motifs = f(&mf)?;
    if motifs.is_empty() {
        return Ok(Noeud::Opaque);
    }
    Ok(Noeud::Feuille {
        champ: champ.to_string(),
        motifs,
    })
}

/// Ce qu'une clause textuelle (`match`, `match_phrase`, `match_phrase_prefix`)
/// cherche dans un champ.
///
/// Un `keyword` **n'est pas analyse** : son terme est la valeur entiere, comme
/// a l'indexation. Le confondre avec un `text` fait chercher `tiret` et `bas`
/// la ou l'index porte `tiret-bas`, donc ne surligne rien — en 200 (trouve par
/// le fuzzer, graine 51).
fn motifs_de_texte(
    gen: &Generation,
    mf: &mapping::MappedField,
    clause: &str,
    texte: &str,
) -> EsResult<Vec<Motif>> {
    if mf.ty.kind() == FieldKind::Keyword {
        return Ok(match clause {
            "match_phrase_prefix" => vec![Motif::Simple(prefixe(texte, false)?)],
            _ => vec![Motif::Simple(Predicat::Terme(texte.to_string()))],
        });
    }
    let positions = analyser(gen, mf.search_analyzer, texte);
    Ok(match clause {
        "match" => positions
            .into_iter()
            .flatten()
            .map(|s| Motif::Simple(Predicat::Terme(s)))
            .collect(),
        "match_phrase" => phrase(positions),
        _ => {
            let mut p = positions;
            let dernier = p.pop();
            let mut suite: Vec<Vec<Predicat>> = p
                .into_iter()
                .map(|alt| alt.into_iter().map(Predicat::Terme).collect())
                .collect();
            if let Some(d) = dernier {
                // La derniere position est un **prefixe**, et chacune de ses
                // alternatives en est un.
                suite.push(
                    d.iter()
                        .map(|t| prefixe(t, false))
                        .collect::<EsResult<Vec<_>>>()?,
                );
            }
            if suite.is_empty() {
                return Ok(Vec::new());
            }
            vec![Motif::Phrase(suite)]
        }
    })
}

fn phrase(positions: Vec<Vec<String>>) -> Vec<Motif> {
    if positions.is_empty() {
        return Vec::new();
    }
    vec![Motif::Phrase(
        positions
            .into_iter()
            .map(|alt| alt.into_iter().map(Predicat::Terme).collect())
            .collect(),
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

/// Les termes d'une chaine de requete, **groupes par position**.
///
/// La distinction compte : un filtre a n-grammes pose tous les grammes d'un mot
/// a la position de ce mot, et une phrase doit alors accepter n'importe lequel
/// a cette position-la — c'est la meme lecture que [`crate::dsl`].
fn analyser(gen: &Generation, analyzer: Analyzer, texte: &str) -> Vec<Vec<String>> {
    let Some(mut ta) = gen.index.tokenizers().get(&analyzer.tokenizer()) else {
        return Vec::new();
    };
    let mut flux = ta.token_stream(texte);
    let mut out: Vec<(usize, Vec<String>)> = Vec::new();
    while flux.advance() {
        let t = flux.token();
        match out.last_mut() {
            Some((pos, alternatives)) if *pos == t.position => {
                alternatives.push(t.text.clone());
            }
            _ => out.push((t.position, vec![t.text.clone()])),
        }
    }
    out.into_iter().map(|(_, a)| a).collect()
}

// ---------------------------------------------------------------------------
// Rendu
// ---------------------------------------------------------------------------

/// Le bloc `highlight` d'un hit, ou `None` s'il n'y a rien a rendre.
///
/// Un champ sans correspondance est **absent** de la reponse (et non une chaine
/// vide), sauf `no_match_size` ; un bloc entierement vide n'apparait pas.
pub fn rendre(plan: &Plan, gen: &Generation, source: &Value, id: &str) -> EsResult<Option<Value>> {
    if plan.est_vide() {
        return Ok(None);
    }
    // Les valeurs de chaque champ que la requete cite ou que le surlignage
    // demande, deja decoupees en termes : il en faut pour marquer, mais aussi
    // pour savoir **quelles clauses ont tenu sur ce document**.
    let mut vues: BTreeMap<&str, Vec<Valeur>> = BTreeMap::new();
    for l in plan
        .lectures
        .values()
        .chain(plan.champs.iter().map(|c| &c.lecture))
    {
        vues.entry(l.chemin.as_str())
            .or_insert_with(|| valeurs_analysees(l, gen, source));
    }

    // Ce que la requete a vraiment trouve dans ce document : ES ne surligne
    // que ca (voir [`Noeud`]).
    let mut par_champ: BTreeMap<&str, Vec<Motif>> = BTreeMap::new();
    if let Some(arbre) = &plan.arbre {
        let verdict = evalue(arbre, &vues, source, id);
        let mut actives = Vec::new();
        collecte(arbre, &verdict, &mut actives);
        for (champ, motifs) in actives {
            par_champ
                .entry(champ)
                .or_default()
                .extend(motifs.iter().cloned());
        }
    }

    let mut bloc = Map::new();
    for champ in &plan.champs {
        let Some(valeurs) = vues.get(champ.lecture.chemin.as_str()) else {
            continue;
        };
        if valeurs.is_empty() {
            continue;
        }
        let motifs = par_champ
            .get(champ.lecture.chemin.as_str())
            .cloned()
            .unwrap_or_default();
        let fragments = fragments_du_champ(champ, &motifs, valeurs);
        if !fragments.is_empty() {
            bloc.insert(
                champ.lecture.chemin.clone(),
                Value::Array(fragments.into_iter().map(Value::String).collect()),
            );
        }
    }
    Ok((!bloc.is_empty()).then(|| Value::Object(bloc)))
}

/// Une valeur du document, prete a etre marquee.
struct Valeur {
    chars: Vec<char>,
    jetons: Vec<Jeton>,
}

/// Les valeurs d'un champ, lues dans le `_source` et decoupees en termes.
fn valeurs_analysees(l: &Lecture, gen: &Generation, source: &Value) -> Vec<Valeur> {
    valeurs_du_source(source, l)
        .into_iter()
        .map(|texte| {
            let chars: Vec<char> = texte.chars().collect();
            let jetons = jetons(l, gen, &texte, &chars);
            Valeur { chars, jetons }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Ce que la requete a trouve dans **ce** document
// ---------------------------------------------------------------------------

impl Noeud {
    /// Les champs que l'arbre cite : ce sont eux qu'il faut lire pour trancher.
    fn champs_cites(&self, out: &mut Vec<String>) {
        match self {
            Self::Feuille { champ, .. } => out.push(champ.clone()),
            Self::Et(enfants) | Self::Ou { enfants, .. } => {
                enfants.iter().for_each(|e| e.champs_cites(out));
            }
            Self::Non(e) | Self::Jointure(e) => e.champs_cites(out),
            Self::Opaque
            | Self::Jamais
            | Self::Toujours
            | Self::Existe(_)
            | Self::Valeurs { .. }
            | Self::Intervalle { .. }
            | Self::Ids(_) => {}
        }
    }
}

/// L'ordre entre deux valeurs typees, quand il en existe un.
///
/// Sert a trancher un `range` sur un champ non surlignable : il ne marque rien,
/// mais un `filter` qui tombe doit faire taire tout le `bool`.
fn ordre(a: &mapping::TypedValue, b: &mapping::TypedValue) -> Option<std::cmp::Ordering> {
    use mapping::TypedValue::{Bool, Date, Str, F64, I64};
    match (a, b) {
        (I64(x), I64(y)) | (Date(x), Date(y)) => Some(x.cmp(y)),
        (F64(x), F64(y)) => Some(x.total_cmp(y)),
        (Bool(x), Bool(y)) => Some(x.cmp(y)),
        (Str(x), Str(y)) => Some(x.cmp(y)),
        (I64(x), F64(y)) => Some((*x as f64).total_cmp(y)),
        (F64(x), I64(y)) => Some(x.total_cmp(&(*y as f64))),
        _ => None,
    }
}

/// Le verdict de chaque noeud, calcule **une fois** par document.
///
/// L'arbre des verdicts a la meme forme que l'arbre des clauses : le calculer
/// a part evite de re-analyser le texte d'un champ a chaque fois qu'on demande
/// si son ancetre tient.
struct Verdict {
    valeur: Option<bool>,
    enfants: Vec<Verdict>,
}

/// Le verdict d'un noeud sur un document. `None` = « on ne sait pas », et dans
/// le doute on laisse passer : mieux vaut marquer de trop que se taire.
fn evalue(n: &Noeud, vues: &BTreeMap<&str, Vec<Valeur>>, source: &Value, id: &str) -> Verdict {
    let enfants: Vec<Verdict> = match n {
        Noeud::Et(e) | Noeud::Ou { enfants: e, .. } => {
            e.iter().map(|x| evalue(x, vues, source, id)).collect()
        }
        Noeud::Non(e) | Noeud::Jointure(e) => vec![evalue(e, vues, source, id)],
        _ => Vec::new(),
    };
    let valeur = verdict_de(n, &enfants, vues, source, id);
    Verdict { valeur, enfants }
}

fn verdict_de(
    n: &Noeud,
    enfants: &[Verdict],
    vues: &BTreeMap<&str, Vec<Valeur>>,
    source: &Value,
    id: &str,
) -> Option<bool> {
    match n {
        Noeud::Ids(valeurs) => Some(valeurs.iter().any(|v| v == id)),
        Noeud::Existe(champ) => {
            let mut vus = Vec::new();
            descendre(source, champ, &mut vus);
            Some(!vus.is_empty())
        }
        Noeud::Intervalle {
            champ,
            ty,
            format,
            bas,
            haut,
        } => {
            let mut vus = Vec::new();
            descendre(source, champ, &mut vus);
            Some(vus.iter().any(|v| {
                mapping::coerce_avec(champ, *ty, v, format.as_ref()).is_ok_and(|t| {
                    let apres = bas.as_ref().is_none_or(|(b, incl)| {
                        ordre(&t, b).is_some_and(|o| o.is_gt() || (*incl && o.is_eq()))
                    });
                    let avant = haut.as_ref().is_none_or(|(h, incl)| {
                        ordre(&t, h).is_some_and(|o| o.is_lt() || (*incl && o.is_eq()))
                    });
                    apres && avant
                })
            }))
        }
        Noeud::Valeurs {
            champ,
            ty,
            format,
            attendues,
        } => {
            let mut vus = Vec::new();
            descendre(source, champ, &mut vus);
            Some(vus.iter().any(|v| {
                mapping::coerce_avec(champ, *ty, v, format.as_ref())
                    .is_ok_and(|t| attendues.contains(&t))
            }))
        }
        Noeud::Feuille { champ, motifs } => Some(
            vues.get(champ.as_str())
                .is_some_and(|vs| vs.iter().any(|v| !marques(motifs, v).is_empty())),
        ),
        Noeud::Opaque => None,
        Noeud::Jamais => Some(false),
        Noeud::Toujours => Some(true),
        Noeud::Et(_) => {
            if enfants.iter().any(|v| v.valeur == Some(false)) {
                Some(false)
            } else if enfants.iter().all(|v| v.valeur == Some(true)) {
                Some(true)
            } else {
                None
            }
        }
        Noeud::Ou { minimum, .. } => {
            let surs = enfants.iter().filter(|v| v.valeur == Some(true)).count();
            if surs >= *minimum {
                Some(true)
            } else if enfants.iter().any(|v| v.valeur.is_none()) {
                None
            } else {
                Some(false)
            }
        }
        // Un `must_not` sur une clause qu'on ne sait pas trancher est suppose
        // **non** satisfait : c'est le sens qui laisse le reste marquer.
        Noeud::Non(_) => Some(!enfants[0].valeur.unwrap_or(false)),
        // Une jointure ne se tranche pas a plat, dans aucun des deux sens : le
        // verdict de son contenu porte sur des elements, pas sur ce document.
        Noeud::Jointure(_) => None,
    }
}

/// Les feuilles qui ont le droit de marquer : celles dont le **contexte** tient
/// sur ce document.
///
/// Une feuille ne s'auto-censure pas : sous `require_field_match: false`, ES
/// applique les termes d'une clause a un **autre** champ que le sien, meme
/// quand elle n'a rien trouve dans le sien (mesure, fuzzer graine 8). Ce qui la
/// fait taire, c'est un `must` ou un `filter` voisin qui echoue — ou un
/// `must_not: {match_all}` qui rend le `bool` sterile (graine 6).
fn collecte<'a>(n: &'a Noeud, verdict: &Verdict, out: &mut Vec<(&'a str, &'a [Motif])>) {
    match n {
        Noeud::Feuille { champ, motifs } => out.push((champ.as_str(), motifs)),
        Noeud::Et(enfants) | Noeud::Ou { enfants, .. } => {
            if verdict.valeur == Some(false) {
                return;
            }
            for (e, v) in enfants.iter().zip(&verdict.enfants) {
                collecte(e, v, out);
            }
        }
        // Sous une jointure, aucun verdict ne coupe : voir [`Noeud::Jointure`].
        Noeud::Jointure(e) => collecte_sans_couper(e, out),
        // Une clause niee ne marque rien, chez ES non plus.
        Noeud::Non(_)
        | Noeud::Opaque
        | Noeud::Jamais
        | Noeud::Toujours
        | Noeud::Existe(_)
        | Noeud::Valeurs { .. }
        | Noeud::Intervalle { .. }
        | Noeud::Ids(_) => {}
    }
}

/// Les feuilles d'un sous-arbre, **sans** consulter aucun verdict.
///
/// C'est la lecture qui vaut sous une jointure : le document est un hit, donc
/// un element a satisfait la clause interne, mais le `_source` a plat ne dit
/// pas lequel — et l'y evaluer fait dire « faux » a un `bool` que l'element
/// gagnant satisfaisait. Seule la negation continue de ne rien marquer : chez
/// ES non plus, un `must_not` ne marque pas.
fn collecte_sans_couper<'a>(n: &'a Noeud, out: &mut Vec<(&'a str, &'a [Motif])>) {
    match n {
        Noeud::Feuille { champ, motifs } => out.push((champ.as_str(), motifs)),
        Noeud::Et(enfants) | Noeud::Ou { enfants, .. } => {
            for e in enfants {
                collecte_sans_couper(e, out);
            }
        }
        Noeud::Jointure(e) => collecte_sans_couper(e, out),
        Noeud::Non(_)
        | Noeud::Opaque
        | Noeud::Jamais
        | Noeud::Toujours
        | Noeud::Existe(_)
        | Noeud::Valeurs { .. }
        | Noeud::Intervalle { .. }
        | Noeud::Ids(_) => {}
    }
}

/// Les valeurs textuelles d'un chemin pointe, tableaux traverses — c'est le
/// meme parcours que `fields`, et il fait tomber les sous-champs d'un `nested`
/// dans l'ordre du document.
fn valeurs_du_source(source: &Value, l: &Lecture) -> Vec<String> {
    let mut brutes = Vec::new();
    descendre(source, &l.source, &mut brutes);
    // Puis ce que les autres champs y ont copie : la valeur d'un `copy_to`
    // n'est nulle part dans le `_source` de sa cible, et ES la surligne quand
    // meme. Meme ordre que pour `fields` : la valeur propre d'abord.
    for copie in &l.copies {
        descendre(source, copie, &mut brutes);
    }
    let (ty, ignore_above) = (l.ty, l.ignore_above);
    brutes
        .into_iter()
        // Une valeur qu'`ignore_above` a ecartee n'est pas dans l'index : ES ne
        // la surligne pas, et `no_match_size` ne la rend pas non plus.
        .filter(|v| match (ignore_above, v.as_str()) {
            (Some(n), Some(s)) => s.chars().count() <= n,
            _ => true,
        })
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

fn fragments_du_champ(champ: &Champ, motifs: &[Motif], valeurs: &[Valeur]) -> Vec<String> {
    // La longueur « du champ » chez ES est celle de ses valeurs mises bout a
    // bout, separateur compris : elle entre dans le score des fragments.
    let longueur: usize =
        valeurs.iter().map(|v| v.chars.len()).sum::<usize>() + valeurs.len().saturating_sub(1);
    // Un champ **sans un caractere** ne rend rien du tout, pas meme une balise
    // vide : ES y coupe avant le surligneur. Une valeur vide **parmi
    // d'autres**, elle, rend bien `<em></em>` — c'est la longueur du champ
    // entier, separateurs compris, qui decide (fuzzer, graine 7370215).
    if longueur == 0 {
        return Vec::new();
    }

    let mut candidats: Vec<Fragment> = Vec::new();
    let mut base = 0usize;
    for valeur in valeurs {
        // Une marque posee **a la fin** du champ n'ouvre pas de fragment : ES
        // s'arrete des que la correspondance commence au-dela du dernier
        // caractere. Ca ne se voit que sur une valeur vide en derniere
        // position — au milieu, la meme valeur rend bien `<em></em>`
        // (fuzzer, graines 7370215 et 8080107).
        let m: Vec<Marque> = marques(motifs, valeur)
            .into_iter()
            .filter(|m| base + m.debut < longueur)
            .collect();
        if !m.is_empty() {
            for f in decouper(champ, &valeur.chars, &m, base, longueur) {
                candidats.push(f);
            }
        }
        base += valeur.chars.len() + 1;
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
fn sans_correspondance(champ: &Champ, valeurs: &[Valeur]) -> Vec<String> {
    let n = champ.reglages.sans_correspondance;
    if n == 0 {
        return Vec::new();
    }
    // La **premiere valeur non vide** : ES concatene les valeurs avec un
    // separateur et saute les separateurs de tete, donc une premiere valeur
    // vide ne lui coute pas le fragment (trouve par le fuzzer, graine 31).
    let Some(premiere) = valeurs.iter().find(|v| !v.chars.is_empty()) else {
        return Vec::new();
    };
    let chars = &premiere.chars;
    let fin = if n < chars.len() {
        crate::segments::suivante(&crate::segments::mots(chars), n)
    } else {
        chars.len()
    };
    let (debut, fin) = if champ.reglages.nb_fragments == 0 {
        (0, fin)
    } else {
        rogner(chars, 0, fin)
    };
    if debut >= fin {
        return Vec::new();
    }
    vec![chars[debut..fin].iter().collect()]
}

/// Les correspondances d'un texte, en indices de `char`, triees et sans
/// doublon.
fn marques(motifs: &[Motif], valeur: &Valeur) -> Vec<Marque> {
    if motifs.is_empty() {
        return Vec::new();
    }
    let jetons = &valeur.jetons;
    let mut out: Vec<Marque> = Vec::new();
    for (rang, motif) in motifs.iter().enumerate() {
        // Les marques d'**une** clause sont fondues entre elles avant tout le
        // reste : chez Lucene une clause est un seul `OffsetsEnum`, et ce qu'il
        // rend d'une position est une marque, pas trois. Sur un champ a
        // n-grammes, un `prefix` attrape `vers`, `versi` et `versio` au meme
        // endroit : c'est `versio` qui ouvre le fragment, pas `vers` (fuzzer,
        // graine 3535137). Deux clauses **distinctes**, elles, restent
        // distinctes — et c'est la plus courte qui ouvre (graine 5500233).
        let mut du_motif: Vec<Marque> = Vec::new();
        let mut pousse = |debut: usize, fin: usize| du_motif.push(Marque { debut, fin, rang });
        match motif {
            Motif::Simple(p) => {
                for j in jetons {
                    if p.matche(&j.texte) {
                        pousse(j.debut, j.fin);
                    }
                }
            }
            Motif::Phrase(positions) => {
                for suite in suites(jetons, positions) {
                    let debut = suite.first().map_or(0, |s| s.0);
                    let fin = suite.iter().map(|s| s.1).max().unwrap_or(0);
                    pousse(debut, fin);
                }
            }
        }
        du_motif.sort_unstable_by_key(|m| (m.debut, m.fin));
        out.extend(
            fondre(&du_motif)
                .into_iter()
                .map(|(debut, fin)| Marque { debut, fin, rang }),
        );
    }
    // Debut croissant puis **fin croissante** : c'est cet ordre-la qui decide
    // quel fragment s'ouvre quand deux marques commencent au meme endroit —
    // la plus **courte** l'emporte, et la plus longue se fait rogner par le
    // fragment qu'elle a ouvert (mesure, fuzzer graine 5500233). Les marques
    // ne sont pas fondues ici : elles le sont une fois rognees au fragment.
    out.sort_unstable_by_key(|m| (m.debut, m.fin, m.rang));
    out.dedup();
    out
}

/// Une correspondance dans une valeur : ses bornes, et **quel** motif l'a
/// trouvee.
///
/// Le motif compte autant que les bornes : le `PassageScorer` de Lucene note un
/// fragment terme par terme, et « le terme » y est une clause, pas un mot
/// trouve. Un `regexp` qui attrape « aluminium » deux fois pese comme un mot vu
/// deux fois, pas comme deux mots (mesure, fuzzer graine 9494099).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Marque {
    debut: usize,
    fin: usize,
    rang: usize,
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
fn jetons(l: &Lecture, gen: &Generation, valeur: &str, chars: &[char]) -> Vec<Jeton> {
    if l.ty.kind() == FieldKind::Keyword {
        return vec![Jeton {
            texte: valeur.to_string(),
            debut: 0,
            fin: chars.len(),
            position: 0,
        }];
    }
    let Some(mut ta) = gen.index.tokenizers().get(&l.analyzer.tokenizer()) else {
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
/// de la phrase, dans l'ordre — chacune rendue **terme par terme**, a charge de
/// l'appelant de les fondre en une seule marque ou non.
fn suites(jetons: &[Jeton], positions: &[Vec<Predicat>]) -> Vec<Vec<(usize, usize)>> {
    let mut out = Vec::new();
    if positions.is_empty() {
        return out;
    }
    let tient =
        |alternatives: &[Predicat], terme: &str| alternatives.iter().any(|p| p.matche(terme));
    for (i, premier) in jetons.iter().enumerate() {
        if !tient(&positions[0], &premier.texte) {
            continue;
        }
        let mut suite = vec![(premier.debut, premier.fin)];
        for (attendue, alternatives) in (premier.position + 1..).zip(positions[1..].iter()) {
            match jetons[i + 1..]
                .iter()
                .find(|j| j.position == attendue && tient(alternatives, &j.texte))
            {
                Some(j) => suite.push((j.debut, j.fin)),
                None => {
                    suite.clear();
                    break;
                }
            }
        }
        if suite.len() == positions.len() {
            out.push(suite);
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
                // A droite : ce que la borne laisse encore — et, quand il ne
                // reste rien, la frontiere de mot qui suit la **fin** de la
                // correspondance. C'est ce qui fait qu'un `match_phrase` de
                // trois mots sort entier au lieu d'etre coupe apres le
                // deuxieme (mesure sur `sur le tapis`, fragment_size 25).
                let restant = max as i64 - (offset as i64 - self.interne.0 as i64);
                self.interne.1 = self.interne.1.min(if restant > 1 {
                    crate::segments::suivante(self.mots, offset + restant as usize)
                } else {
                    // Plus de place : le fragment s'arrete a la fin de la
                    // correspondance — mais sans couper le mot qui la porte.
                    // Un terme qui finit sur une frontiere s'arrete pile
                    // (`<em>l'ascension</em>` et non `<em>l'ascension </em>`,
                    // graine 5500233) ; un gramme au milieu d'un mot va
                    // jusqu'au bout de ce mot (`<b>versio</b>n`, graine
                    // 3535137).
                    crate::segments::a_partir_de(self.mots, arrivee)
                });
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
            // `get` et non l'indexation : sur un texte d'une seule phrase il
            // n'y a rien apres, et `phrases[i + 2..]` y paniquait (trouve par
            // le fuzzer au premier passage, sur un `keyword` d'un seul mot).
            for &b in self.phrases.get(i + 2..).unwrap_or(&[]) {
                if b - debut > max {
                    break;
                }
                fin = b;
            }
        }
        (debut, fin)
    }
}

/// Rogne les deux bords d'un fragment, exactement comme le `String.trim()` de
/// Java : les caracteres **de code inferieur ou egal a U+0020**, et eux seuls.
///
/// Ce n'est ni « les espaces » ni la propriete `White_Space` d'Unicode, et la
/// difference se mesure : ES rogne bien l'espace ordinaire, la tabulation et le
/// saut de ligne, mais **garde** l'espace insecable (U+00A0), l'espace fine
/// (U+2009) et le separateur de ligne (U+2028), tous au-dessus de U+0020.
/// Rogner « les blancs » au sens de Rust en mangeait trois de trop.
fn rogner(chars: &[char], mut debut: usize, mut fin: usize) -> (usize, usize) {
    let blanc = |c: char| (c as u32) <= 0x20;
    while debut < fin && blanc(chars[debut]) {
        debut += 1;
    }
    while fin > debut && blanc(chars[fin - 1]) {
        fin -= 1;
    }
    (debut, fin)
}

fn decouper(
    champ: &Champ,
    chars: &[char],
    marques: &[Marque],
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
    // `number_of_fragments: 0` : la valeur entiere, d'un bloc — et **sans
    // rognage**. ES ne passe alors plus par le decoupeur borne mais par un
    // simple decoupage sur les separateurs de valeurs, et c'est la que vivait
    // le `trim` (mesure : `"  abc def  "` ressort tel quel a `nof: 0`, rogne
    // des `nof: 1` ; fuzzer, graine 5150174).
    let entier = champ.reglages.nb_fragments == 0;

    /// Un fragment en cours : ses bornes, et les marques qu'il porte.
    type Groupe = (usize, usize, Vec<Marque>);
    let mut groupes: Vec<Groupe> = Vec::new();
    for &Marque {
        debut: deb,
        fin,
        rang,
    } in marques
    {
        let ouvrir = match groupes.last() {
            None => true,
            // Un fragment **vide** (une valeur de `keyword` vide) n'en ouvre
            // qu'un : deux clauses qui y posent la meme marque de longueur
            // nulle ne rendent pas deux `<em></em>` (fuzzer, graines 5500001
            // et 2626208).
            Some((d0, f, _)) => deb >= *f && !(deb == *f && d0 == f),
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
        // `deb == g.1` n'est retenu que sur un fragment **vide** : une valeur
        // de `keyword` vide porte un terme vide, et ES la rend bien
        // `<em></em>` (fuzzer, graine 6260221).
        if deb < g.1 || g.0 == g.1 {
            g.2.push(Marque {
                debut: deb.max(g.0),
                fin: fin.min(g.1),
                rang,
            });
        }
    }

    groupes
        .into_iter()
        .filter(|(_, _, m)| !m.is_empty())
        .map(|(deb, fin, m)| {
            let intervalles = fondre(&m);
            // Le score et le rang se calculent sur le fragment **avant**
            // rognage : c'est le `Passage` de Lucene qui est note, et le
            // rognage n'a lieu qu'a la mise en forme. Noter le fragment rogne
            // faisait gagner « cible\t » (5 caracteres une fois rogne) contre
            // « cible\u2009 » (6, que Java ne rogne pas), donc rendait un
            // autre fragment qu'ES.
            let score = note(&m, deb, fin, base, longueur);
            let (mut deb_rendu, mut fin_rendu) = if entier {
                (deb, fin)
            } else {
                rogner(chars, deb, fin)
            };
            // Le rognage ne mord jamais sur une marque : un `keyword` dont la
            // valeur est «   espaces   multiples   » sort chez ES avec ses
            // blancs **dans** le `<em>`, puisque le terme les porte (fuzzer,
            // graines 7370151 et 7370219).
            if let (Some(premiere), Some(derniere)) = (intervalles.first(), intervalles.last()) {
                deb_rendu = deb_rendu.min(premiere.0);
                fin_rendu = fin_rendu.max(derniere.1);
            }
            Fragment {
                depart: base + deb,
                score,
                texte: formater(champ, chars, deb_rendu, fin_rendu, &intervalles),
            }
        })
        .collect()
}

/// Le `PassageScorer` de Lucene : un BM25 dont le « document » est le fragment
/// et le « corpus » le champ, pivote sur 87 caracteres.
fn note(dans: &[Marque], debut: usize, fin: usize, base: usize, longueur: usize) -> f64 {
    let taille = (fin - debut) as f64;
    // Une marque compte par la **clause** qui l'a posee, pas par le mot
    // trouve : c'est un `OffsetsEnum` par clause chez Lucene. Deux
    // occurrences du meme mot pesent donc comme deux occurrences d'une clause,
    // et un `regexp` qui attrape deux mots differents aussi.
    let mut par_terme: BTreeMap<usize, usize> = BTreeMap::new();
    for m in dans {
        *par_terme.entry(m.rang).or_default() += 1;
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

/// Deux marques qui **se chevauchent** n'en font qu'une ; deux marques qui se
/// **touchent** en restent deux.
///
/// La distinction n'est pas cosmetique, et elle se mesure : un analyzer a
/// n-grammes de longueur variable pose des grammes qui se recouvrent, et ES y
/// rend `<em>elevee etendue</em>` d'un bloc (fuzzer, graine 3535187) ; des
/// grammes poses bout a bout, eux, sortent un par un — `<em>t</em><em>i</em>ssu`
/// (graine 6260166). Les fondre toutes donnait le premier resultat dans les
/// deux cas.
fn fondre(marques: &[Marque]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(marques.len());
    for &Marque { debut, fin, .. } in marques {
        match out.last_mut() {
            // Deux clauses qui posent la **meme** marque n'en rendent qu'une :
            // sans ca, un `multi_match` d'une chaine vide sur trois champs
            // rendait `<b></b><b></b>` (fuzzer, graine 2626208).
            Some(precedente) if (debut, fin) == *precedente => {}
            Some(precedente) if debut < precedente.1 && fin > precedente.1 => {
                precedente.1 = fin;
            }
            Some(precedente) if debut < precedente.1 => {}
            _ => out.push((debut, fin)),
        }
    }
    out
}

/// Le formateur de Lucene : le texte du fragment, chaque correspondance
/// encadree.
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
        // Une marque de longueur nulle porte quand meme sa paire de balises :
        // c'est ce qu'ES rend d'une valeur de `keyword` vide, `<em></em>`.
        if f == d && d == pos && debut == fin {
            out.push_str(&champ.reglages.pre);
            out.push_str(&champ.reglages.post);
            continue;
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
        // Plus de place a droite : le fragment s'arrete a la fin de la
        // correspondance, pas a la frontiere de mot suivante.
        assert_eq!(d.autour(20, 25), (11, 25));
        assert_eq!(d.autour(26, 31), (25, 32));
    }

    fn jeton(texte: &str, debut: usize, position: usize) -> Jeton {
        Jeton {
            texte: texte.into(),
            debut,
            fin: debut + texte.chars().count(),
            position,
        }
    }

    fn feuille(champ: &str, terme: &str) -> Noeud {
        Noeud::Feuille {
            champ: champ.into(),
            motifs: vec![Motif::Simple(Predicat::Terme(terme.into()))],
        }
    }

    /// « le chat dort », un terme par position.
    fn vues_du_texte() -> BTreeMap<&'static str, Vec<Valeur>> {
        let mut m = BTreeMap::new();
        m.insert(
            "t",
            vec![Valeur {
                chars: "le chat dort".chars().collect(),
                jetons: vec![jeton("le", 0, 0), jeton("chat", 3, 1), jeton("dort", 8, 2)],
            }],
        );
        m
    }

    /// Ce que la requete pose n'est pas ce qui a fait correspondre le document :
    /// un `should` sous un `filter` qui echoue ne marque rien. Mesure contre
    /// ES 8.15 (fuzzer, graine 106).
    #[test]
    fn un_should_sous_un_filter_qui_echoue_ne_marque_rien() {
        let vues = vues_du_texte();
        let arbre = Noeud::Et(vec![
            feuille("t", "introuvable"),
            Noeud::Ou {
                enfants: vec![feuille("t", "chat")],
                minimum: 1,
            },
        ]);
        let mut out = Vec::new();
        collecte(&arbre, &evalue(&arbre, &vues, &Value::Null, ""), &mut out);
        assert!(out.is_empty(), "{out:?}");

        // Le meme arbre dont le filtre tient : le `should` marque.
        let arbre = Noeud::Et(vec![
            feuille("t", "le"),
            Noeud::Ou {
                enfants: vec![feuille("t", "chat")],
                minimum: 1,
            },
        ]);
        let mut out = Vec::new();
        collecte(&arbre, &evalue(&arbre, &vues, &Value::Null, ""), &mut out);
        assert_eq!(out.len(), 2);
    }

    /// `must_not: {match_all}` fait taire tout le `bool` — Lucene le reecrit en
    /// `MatchNoDocsQuery` (fuzzer, graine 6).
    #[test]
    fn un_must_not_match_all_rend_le_bool_sterile() {
        let vues = vues_du_texte();
        let arbre = Noeud::Et(vec![
            Noeud::Non(Box::new(Noeud::Toujours)),
            Noeud::Ou {
                enfants: vec![feuille("t", "chat")],
                minimum: 1,
            },
        ]);
        let mut out = Vec::new();
        collecte(&arbre, &evalue(&arbre, &vues, &Value::Null, ""), &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    /// Deux marques qui se chevauchent n'en font qu'une : sans la fusion, le
    /// formateur rendrait `<em>le</em><em> chat</em>` (fuzzer, graines 900138
    /// et 3535187).
    #[test]
    fn marques_qui_se_chevauchent() {
        let vues = vues_du_texte();
        let valeur = &vues["t"][0];
        let motifs = vec![
            Motif::Simple(Predicat::Terme("le".into())),
            Motif::Phrase(vec![
                vec![Predicat::Terme("le".into())],
                vec![Predicat::Terme("chat".into())],
            ]),
        ];
        // La plus **courte** d'abord : c'est elle qui ouvrira le fragment.
        let bornes: Vec<(usize, usize)> = marques(&motifs, valeur)
            .iter()
            .map(|m| (m.debut, m.fin))
            .collect();
        assert_eq!(bornes, vec![(0, 2), (0, 7)]);
        // Puis elles n'en font qu'une, une fois rognees au fragment.
        assert_eq!(fondre(&marques(&motifs, valeur)), vec![(0, 7)]);

        // Deux mots separes par une espace restent deux marques.
        let separes = vec![
            Motif::Simple(Predicat::Terme("le".into())),
            Motif::Simple(Predicat::Terme("chat".into())),
        ];
        assert_eq!(fondre(&marques(&separes, valeur)), vec![(0, 2), (3, 7)]);
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
