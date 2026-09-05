//! L'arbre d'explication d'un score : `explain: true` et `GET /{index}/_explain/{id}`.
//!
//! Ce que ce module rend n'est pas un confort de mise au point, c'est un
//! **instrument de mesure**. Le `_score` de ferrite vient du BM25 de tantivy et
//! celui d'ES du BM25 de Lucene : les deux appliquent la meme formule, mais pas
//! sur les memes statistiques (voir plus bas). Constater que deux nombres
//! different ne dit pas **d'ou** vient l'ecart ; un arbre comparable, si.
//!
//! ## Ce qui est reproduit, et ce qui ne l'est pas
//!
//! La `description` d'Elasticsearch est du texte de Lucene. La reproduire mot
//! pour mot serait un decor : elle ne porte aucune information que la structure
//! et les valeurs ne portent pas deja, et la truquer masquerait justement les
//! endroits ou les deux moteurs ne calculent pas la meme chose. Ce qui est donc
//! reproduit est la **forme de l'arbre** et les **valeurs** de chaque noeud ; la
//! phrase reprend celle d'ES **quand la quantite est la meme**, et s'en ecarte
//! quand elle ne l'est pas. Deux exemples, et ce sont exactement les deux
//! endroits ou le score diverge :
//!
//! - `N, total number of documents with field` chez ES (Lucene compte les
//!   documents **qui ont le champ**) contre `N, total number of documents` ici
//!   (tantivy compte tous les documents du segment) ;
//! - `avgdl` se calcule chez Lucene sur ces memes documents, et chez tantivy sur
//!   tous — la phrase est la meme, la valeur non.
//!
//! Un arbre dont la description mentirait sur ce point rendrait ces deux ecarts
//! invisibles, c'est-a-dire retirerait a l'instrument sa seule raison d'etre.
//!
//! ## D'ou viennent les nombres
//!
//! De l'`Explanation` que tantivy produit pour la requete **reellement
//! executee** : un arbre reconstruit a cote du scorer expliquerait un score que
//! personne n'a calcule. Le module ne fait que le **retourner dans la forme
//! d'ES** — c'est-a-dire renommer et regrouper des noeuds. Les phrases de
//! tantivy sur lesquelles cette traduction s'appuie sont figees par
//! [`tests/spike_explain.rs`](../tests/spike_explain.rs) : une montee de version
//! qui les change casse bruyamment, plutot que de rendre un arbre plausible.

use serde_json::{json, Map, Value};
use tantivy::schema::Schema;

/// Un noeud de l'arbre d'explication, au format d'Elasticsearch.
///
/// ES rend toujours les trois cles, `details` comprise et vide sur une feuille.
#[derive(Debug, Clone)]
pub struct Noeud {
    pub valeur: f32,
    pub description: String,
    pub details: Vec<Noeud>,
    /// La valeur est-elle un **compte** ? ES rend alors un entier JSON (`2`) la
    /// ou il rend un flottant partout ailleurs (`2.0`) : `n` et `N` sont des
    /// `long` chez Lucene, et un client qui type strictement son JSON y lit un
    /// entier.
    pub entier: bool,
}

impl Noeud {
    pub fn feuille(description: impl Into<String>, valeur: f32) -> Self {
        Self {
            valeur,
            description: description.into(),
            details: Vec::new(),
            entier: false,
        }
    }

    /// Une feuille dont la valeur est un compte, rendu en entier comme chez ES.
    fn compte(description: impl Into<String>, valeur: f32) -> Self {
        Self {
            entier: true,
            ..Self::feuille(description, valeur)
        }
    }

    pub fn avec(description: impl Into<String>, valeur: f32, details: Vec<Noeud>) -> Self {
        Self {
            valeur,
            description: description.into(),
            details,
            entier: false,
        }
    }

    pub fn json(&self) -> Value {
        json!({
            "value": if self.entier {
                json!(self.valeur as i64)
            } else {
                json!(crate::search::round_score(self.valeur))
            },
            "description": self.description,
            "details": self.details.iter().map(Noeud::json).collect::<Vec<_>>(),
        })
    }
}

/// Ce qu'un document non retenu recoit.
///
/// ES rend ici la raison Lucene de l'echec (`no matching term`, `Failure to meet
/// condition(s) of required/prohibited clause(s)`), reconstruite en descendant
/// dans la requete. ferrite ne la reconstruit pas : le `Weight` de tantivy
/// n'explique que ce qui correspond, et inventer une raison plausible serait la
/// pire des reponses pour un outil de mise au point. Ce qui compte — `matched`
/// — est exact des deux cotes.
pub fn sans_correspondance() -> Noeud {
    Noeud::feuille("ne correspond pas a la requete", 0.0)
}

/// L'arbre d'un document, dans la forme d'Elasticsearch.
///
/// Rend `None` quand le document ne correspond pas a la requete.
pub fn expliquer(
    searcher: &tantivy::Searcher,
    query: &dyn tantivy::query::Query,
    addr: tantivy::DocAddress,
    schema: &Schema,
) -> Option<Noeud> {
    // La correspondance se verifie **avant** de demander l'arbre, et pas avec
    // le meme outil.
    //
    // `Weight::explain` de tantivy commence par `scorer.seek(doc)`, et le
    // contrat de `DocSet::seek` exige `target >= doc()` — un `TermScorer` le
    // verifie par un `debug_assert`. Un document que la requete ne retient pas
    // laisse justement le curseur **au-dela** de lui : demander l'arbre d'un
    // non-correspondant fait donc paniquer en debug, et lire un curseur
    // invalide en release. Or « ce document ne correspond pas » est exactement
    // la question que `_explain` sert a poser.
    correspond(searcher, query, addr)?;
    let brut = query.explain(searcher, addr).ok()?;
    let valeur = serde_json::to_value(&brut).ok()?;
    Some(traduire(&lire(&valeur)?, schema))
}

/// Le score d'un document pour une requete, ou `None` s'il n'y correspond pas.
///
/// N'appelle `seek` que vers l'avant : c'est la seule facon sure de poser la
/// question a une requete quelconque (voir [`expliquer`]).
pub fn correspond(
    searcher: &tantivy::Searcher,
    query: &dyn tantivy::query::Query,
    addr: tantivy::DocAddress,
) -> Option<f32> {
    use tantivy::query::EnableScoring;
    use tantivy::DocSet;
    let weight = query
        .weight(EnableScoring::enabled_from_searcher(searcher))
        .ok()?;
    let reader = searcher.segment_reader(addr.segment_ord);
    let mut scorer = weight.scorer(reader, 1.0).ok()?;
    if scorer.doc() > addr.doc_id {
        return None;
    }
    if scorer.seek(addr.doc_id) != addr.doc_id {
        return None;
    }
    Some(tantivy::query::Scorer::score(&mut *scorer))
}

// ---------------------------------------------------------------------------
// Ce que tantivy ecrit
// ---------------------------------------------------------------------------

/// Un noeud de l'arbre **de tantivy**, tel que sa serialisation le rend.
struct Brut {
    valeur: f32,
    description: String,
    details: Vec<Brut>,
    contexte: Vec<String>,
}

fn lire(v: &Value) -> Option<Brut> {
    let o = v.as_object()?;
    Some(Brut {
        valeur: o.get("value")?.as_f64()? as f32,
        description: o.get("description")?.as_str()?.to_string(),
        details: match o.get("details") {
            Some(Value::Array(a)) => a.iter().filter_map(lire).collect(),
            _ => Vec::new(),
        },
        contexte: match o.get("context") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        },
    })
}

// Les phrases de tantivy dont depend la traduction. Elles sont figees par
// `tests/spike_explain.rs` : ce sont des constantes de la dependance, pas des
// garanties qu'elle documente.
const T_TERME: &str = "TermQuery, product of...";
const T_PHRASE: &str = "Phrase Scorer";
const T_PHRASE_PREFIXE: &str = "Phrase Prefix Scorer";
const T_K1_PLUS_1: &str = "(K1+1)";
const T_BOOL: &str = "BooleanClause. sum of ...";
const T_BOOL_SANS_SCORE: &str = "BooleanQuery with no scoring";
const T_TOUT: &str = "AllQuery";
const T_CONST: &str = "Const";
const T_INTERVALLE: &str = "RangeQuery";
const T_EXISTE: &str = "ExistsQuery";
const T_AUTOMATE: &str = "AutomatonScorer";
const T_IDF: &str = "idf, computed as log(1 + (N - n + 0.5) / (n + 0.5))";
const T_IDF_SOMME: &str = "idf";
const T_N_MINUSCULE: &str = "n, number of docs containing this term";
const T_N_MAJUSCULE: &str = "N, total number of docs";
const T_TF: &str = "freq / (freq + k1 * (1 - b + b * dl / avgdl))";
const T_BOOST: &str = "Boost x";

/// Les phrases sur lesquelles la traduction s'appuie, pour le spike.
pub const PHRASES_TANTIVY: &[&str] = &[
    T_TERME,
    T_PHRASE,
    T_PHRASE_PREFIXE,
    T_K1_PLUS_1,
    T_BOOL,
    T_BOOL_SANS_SCORE,
    T_TOUT,
    T_CONST,
    T_INTERVALLE,
    T_EXISTE,
    T_AUTOMATE,
    T_IDF,
    T_IDF_SOMME,
    T_N_MINUSCULE,
    T_N_MAJUSCULE,
    T_TF,
    T_BOOST,
];

// ---------------------------------------------------------------------------
// La traduction
// ---------------------------------------------------------------------------

fn traduire(b: &Brut, schema: &Schema) -> Noeud {
    traduire_avec_boost(b, schema, 1.0)
}

/// `boost` est le facteur que les `BoostQuery` traversees ont accumule.
///
/// Il n'est pas applique ici mais **porte jusqu'a la feuille de scoring**, ou il
/// se fond dans le `boost` de la formule `boost * idf * tf` — c'est la place que
/// Lucene lui donne. Le laisser en niveau intermediaire donnerait un arbre d'une
/// profondeur de plus qu'ES pour le meme calcul.
fn traduire_avec_boost(b: &Brut, schema: &Schema, boost: f32) -> Noeud {
    let d = b.description.as_str();

    if let Some(reste) = d.strip_prefix(T_BOOST) {
        // « Boost x2 of ... » : un seul enfant, et c'est lui qui porte le score.
        let facteur = reste
            .trim_end_matches(" of ...")
            .parse::<f32>()
            .unwrap_or(1.0);
        if let [enfant] = &b.details[..] {
            return traduire_avec_boost(enfant, schema, boost * facteur);
        }
    }

    if d == T_TERME || d == T_PHRASE || d == T_PHRASE_PREFIXE {
        return scoring(b, schema, boost, d);
    }

    if d == T_BOOL {
        return Noeud::avec(
            "sum of:",
            b.valeur * boost,
            b.details
                .iter()
                .map(|e| traduire_avec_boost(e, schema, boost))
                .collect(),
        );
    }

    if d == T_TOUT {
        return Noeud::feuille("*:*", b.valeur * boost);
    }

    // Une clause de `filter` : ferrite l'enveloppe dans un score constant de
    // zero, exactement pour la raison qu'ES ecrit dans son arbre — elle doit
    // correspondre sans rien ajouter au score.
    if d == T_CONST && b.valeur == 0.0 {
        if let [enfant] = &b.details[..] {
            return Noeud::avec(
                "match on required clause, product of:",
                0.0,
                vec![
                    Noeud::feuille("# clause", 0.0),
                    traduire_avec_boost(enfant, schema, 1.0),
                ],
            );
        }
    }

    // « Tous les documents » est enveloppe d'un score constant de 1.0 pour que
    // tantivy ne laisse pas tomber son score dans un booleen (voir
    // [`crate::dsl::tous_les_documents`]). L'arbre d'ES ne connait que la
    // feuille, et c'est elle qu'il faut rendre : l'enveloppe est un detail
    // d'implementation, pas une etape du calcul.
    if d == T_CONST {
        if let [enfant] = &b.details[..] {
            if enfant.description == T_TOUT {
                return Noeud::feuille("*:*", b.valeur * boost);
            }
        }
    }

    // `Const` porte deux choses chez tantivy : l'enveloppe de `constant_score`
    // (un enfant) et l'intervalle sur colonne (aucun). ES rend une **feuille**
    // dans les deux cas — un score constant n'a rien a expliquer.
    if d == T_CONST || d == T_INTERVALLE || d == T_EXISTE || d == T_AUTOMATE {
        let nom = match d {
            T_CONST if b.details.is_empty() => "RangeQuery",
            T_CONST => "ConstantScore",
            autre => autre,
        };
        return Noeud::feuille(nom, b.valeur * boost);
    }

    if d == T_BOOL_SANS_SCORE {
        return Noeud::feuille("BooleanQuery (sans score)", b.valeur * boost);
    }

    // Tout le reste — les requetes que ferrite ecrit lui-meme (`function_score`,
    // `boosting`, `dis_max`, `nested`) — garde sa phrase et ses enfants. Un
    // noeud inconnu passe donc **visible**, jamais efface.
    Noeud::avec(
        b.description.clone(),
        b.valeur * boost,
        b.details
            .iter()
            .map(|e| traduire_avec_boost(e, schema, boost))
            .collect(),
    )
}

/// Un noeud de scoring BM25 (terme, phrase, phrase a prefixe) dans la forme
/// d'ES : `weight(...)` → `score(...)` → `[boost, idf, tf]`.
fn scoring(b: &Brut, schema: &Schema, boost: f32, genre: &str) -> Noeud {
    let mut facteur = boost;
    let mut idf = None;
    let mut tf = None;
    for e in &b.details {
        match e.description.as_str() {
            T_K1_PLUS_1 => facteur *= e.valeur,
            T_IDF | T_IDF_SOMME => idf = Some(e),
            T_TF => tf = Some(e),
            // Une phrase enveloppe son BM25 dans un noeud intermediaire.
            _ => {
                for f in &e.details {
                    match f.description.as_str() {
                        T_K1_PLUS_1 => facteur *= f.valeur,
                        T_IDF | T_IDF_SOMME => idf = Some(f),
                        T_TF => tf = Some(f),
                        _ => {}
                    }
                }
            }
        }
    }

    let freq = tf.and_then(|t| t.details.first()).map_or(1.0, |f| f.valeur);
    let valeur = b.valeur * boost;

    let mut details = vec![Noeud::feuille("boost", facteur)];
    if let Some(i) = idf {
        details.push(traduire_idf(i));
    }
    if let Some(t) = tf {
        details.push(traduire_tf(t));
    }

    let quoi = terme_lisible(&b.contexte, schema).unwrap_or_else(|| match genre {
        T_PHRASE => "phrase".to_string(),
        T_PHRASE_PREFIXE => "phrase a prefixe".to_string(),
        _ => "terme".to_string(),
    });
    Noeud::avec(
        format!("weight({quoi}), result of:"),
        valeur,
        vec![Noeud::avec(
            format!("score(freq={freq:?}), computed as boost * idf * tf from:"),
            valeur,
            details,
        )],
    )
}

fn traduire_idf(b: &Brut) -> Noeud {
    if b.description == T_IDF_SOMME {
        // La somme des idf d'une phrase : tantivy ne garde pas le detail par
        // terme, ES si. La valeur est la meme, la profondeur non — c'est ecrit
        // dans docs/compat.md plutot que comble par des enfants inventes.
        return Noeud::feuille("idf, sum of:", b.valeur);
    }
    let details = b
        .details
        .iter()
        .map(|e| match e.description.as_str() {
            T_N_MINUSCULE => Noeud::compte("n, number of documents containing term", e.valeur),
            // La phrase d'ES est « N, total number of documents **with field** » :
            // Lucene ne compte que les documents qui portent le champ, tantivy
            // les compte tous. La difference de phrase **est** la difference de
            // valeur ; la gommer rendrait l'ecart invisible.
            T_N_MAJUSCULE => Noeud::compte("N, total number of documents", e.valeur),
            autre => Noeud::feuille(autre, e.valeur),
        })
        .collect();
    Noeud::avec(
        "idf, computed as log(1 + (N - n + 0.5) / (n + 0.5)) from:",
        b.valeur,
        details,
    )
}

fn traduire_tf(b: &Brut) -> Noeud {
    Noeud::avec(
        "tf, computed as freq / (freq + k1 * (1 - b + b * dl / avgdl)) from:",
        b.valeur,
        b.details
            .iter()
            .map(|e| Noeud::feuille(e.description.clone(), e.valeur))
            .collect(),
    )
}

/// `Term(field=3, type=Str, "chat")` → `titre:chat`.
///
/// tantivy ne transporte le terme que dans le `Debug` qu'il pose en contexte :
/// le numero de champ n'a de sens que dans le schema, et c'est ferrite qui l'a.
/// Rendre `weight(...)` sans le terme priverait l'arbre de ce qu'on vient y
/// lire ; la forme du `Debug` est donc figee par le spike.
fn terme_lisible(contexte: &[String], schema: &Schema) -> Option<String> {
    let brut = contexte.iter().find_map(|c| c.strip_prefix("Term="))?;
    let dedans = brut.strip_prefix("Term(")?.strip_suffix(')')?;
    let (champ, reste) = dedans.split_once(", ")?;
    let id: u32 = champ.strip_prefix("field=")?.parse().ok()?;
    let valeur = reste.split_once(", ").map_or(reste, |(_, v)| v);
    let nom = schema.get_field_name(tantivy::schema::Field::from_field_id(id));
    Some(format!("{nom}:{}", valeur.trim_matches('"')))
}

// ---------------------------------------------------------------------------
// L'ordre de `matched_queries`
// ---------------------------------------------------------------------------

/// L'ordre dans lequel Elasticsearch rend les noms de `matched_queries`.
///
/// Ce n'est ni l'ordre de la requete ni l'ordre alphabetique : ES range ses
/// clauses nommees dans une `HashMap<String, Query>` de Java et rend l'ordre
/// d'iteration de cette table. Il est **reproductible** — deux JVM differentes
/// rendent le meme (mesure contre deux conteneurs) — parce que le seau d'une
/// chaine ne depend que de son `hashCode` : `h ^ (h >>> 16)`, puis modulo la
/// taille de la table, qui vaut 16 tant qu'il y a au plus 12 noms et double
/// ensuite.
///
/// Ce que ferrite ne reproduit pas est l'ordre **a l'interieur d'un seau** :
/// chez ES il depend de l'historique des deux tables chainees que le nom
/// traverse (l'une remplie a l'analyse, l'autre a la restitution) et de leurs
/// redimensionnements. ferrite departage par ordre d'apparition dans la requete.
/// C'est donc identique a ES tant que deux noms ne tombent pas dans le meme
/// seau, et l'ecart est chiffre par `tests/compat/sonde_explain.py`.
pub fn ordre_es(noms: &[String]) -> Vec<usize> {
    let mut capacite: u32 = 16;
    while noms.len() as f32 > 0.75 * capacite as f32 {
        capacite *= 2;
    }
    let mut rangs: Vec<usize> = (0..noms.len()).collect();
    rangs.sort_by_key(|&i| (seau(&noms[i], capacite), i));
    rangs
}

/// Le seau de Java : `String.hashCode`, l'etalement de `HashMap`, puis le
/// modulo (une puissance de deux, donc un `&`).
fn seau(nom: &str, capacite: u32) -> u32 {
    let mut h: u32 = 0;
    for c in nom.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u32::from(c));
    }
    (h ^ (h >> 16)) & (capacite - 1)
}

/// Le bloc `matched_queries` d'un hit.
///
/// `avec_score` est `include_named_queries_score` : ES rend alors un objet
/// `{nom: score}` au lieu d'une liste de noms.
pub fn matched_queries(
    searcher: &tantivy::Searcher,
    nommees: &[(String, Box<dyn tantivy::query::Query>)],
    addr: tantivy::DocAddress,
    avec_score: bool,
) -> Option<Value> {
    let mut noms = Vec::new();
    let mut scores = Vec::new();
    for (nom, q) in nommees {
        // La clause est rejouee **seule** contre ce document-la : c'est la
        // question qu'ES pose, et elle rend le score avec la reponse.
        if let Some(score) = correspond(searcher, q.as_ref(), addr) {
            noms.push(nom.clone());
            scores.push(score);
        }
    }
    if noms.is_empty() {
        return None;
    }
    let rangs = ordre_es(&noms);
    if avec_score {
        let mut o = Map::new();
        for i in rangs {
            o.insert(
                noms[i].clone(),
                json!(crate::search::round_score(scores[i])),
            );
        }
        Some(Value::Object(o))
    } else {
        Some(Value::Array(
            rangs.into_iter().map(|i| json!(noms[i])).collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seau_de_java() {
        // Mesure contre un ES 8.15 : ces cinq noms, poses dans cet ordre, sont
        // rendus dans celui-la.
        let noms: Vec<String> = ["zzz", "aaa", "mmm", "kkk", "bbb"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ordre: Vec<&str> = ordre_es(&noms).iter().map(|&i| noms[i].as_str()).collect();
        assert_eq!(ordre, ["aaa", "bbb", "kkk", "zzz", "mmm"]);
    }

    #[test]
    fn seau_de_java_deux_lettres() {
        // Les quatre autres batteries mesurees, chacune sur une clause
        // differente : le seau ne depend que du nom, pas de la clause.
        for (poses, attendu) in [
            (vec!["B", "in"], vec!["B", "in"]),
            (vec!["P", "NG", "BO"], vec!["P", "NG", "BO"]),
            (vec!["DM", "D1"], vec!["DM", "D1"]),
            (vec!["CS", "CSF"], vec!["CS", "CSF"]),
            (vec!["FQ", "FS"], vec!["FQ", "FS"]),
        ] {
            let noms: Vec<String> = poses.iter().map(|s| s.to_string()).collect();
            let ordre: Vec<&str> = ordre_es(&noms).iter().map(|&i| noms[i].as_str()).collect();
            assert_eq!(ordre, attendu);
        }
    }

    #[test]
    fn la_table_double_apres_douze_noms() {
        let noms: Vec<String> = (0..13).map(|i| format!("q{i:03}")).collect();
        // Mesure contre ES : avec treize noms la table vaut 32, et `q010`
        // (seau 10) passe devant `q000` (seau 11).
        let ordre: Vec<&str> = ordre_es(&noms).iter().map(|&i| noms[i].as_str()).collect();
        assert_eq!(ordre[0], "q010");
        assert_eq!(ordre[1], "q000");
        assert_eq!(ordre[2], "q011");
    }
}
