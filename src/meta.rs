//! Les champs de **metadonnees** dans une clause de requete.
//!
//! `{"term": {"_id": "mon-id-1"}}` rendait `hits.total = 0` sans erreur pour un
//! document qui existe : le nom n'etait dans aucun mapping, la clause tombait
//! donc dans « champ non mappe » — que le defaut d'ES
//! (`allow_unmapped_fields`) transforme en « ne correspond a rien ». Un vide
//! indiscernable d'un document absent, en 200, c'est-a-dire la pire des trois
//! categories de ce depot.
//!
//! La regle a appliquer n'est pas « faire repondre `_id` », et c'est tout
//! l'objet de ce module. Les 203 questions de
//! [`sonde_meta_champs.py`](../tests/compat/sonde_meta_champs.py) rangent
//! chaque case dans une des **trois** issues qu'un vrai ES 8.15 rend :
//!
//! * il **repond** — `term`, `terms`, `match`, `match_phrase` et `exists` sur
//!   `_id`, a score constant (1.0 x `boost`) ;
//! * il **refuse** — `prefix`, `wildcard`, `regexp`, `fuzzy`,
//!   `match_phrase_prefix` et `range` sur ce meme `_id`, en nommant le type du
//!   champ. Les servir aurait ete une divergence de plus, dans l'autre sens ;
//! * il rend **vide** — `_type`, que rien ne porte depuis la 8.0 : 200 et aucun
//!   document. La parite y est le vide, et en faire une erreur serait une
//!   divergence introduite au nom d'une regle mal citee.
//!
//! Ce que ferrite ne sait pas servir est **refuse en le nommant**, jamais rendu
//! vide : `_routing` (ferrite est mono-shard et refuse `?routing=` a
//! l'indexation, donc aucun document n'en porte), `_seq_no` et `_version` en
//! clause, `_ignored` (ferrite ne tient pas la liste des valeurs qu'un
//! `ignore_above` a ecartees). C'est un cout de perimetre, il est ecrit dans
//! `compat.yaml`, et il se voit.

use serde_json::Value;

use crate::error::{EsError, EsResult};

/// Les champs de metadonnees qu'une clause peut citer, dans l'ordre ou la sonde
/// les pose. Un nom qui n'y est pas suit le chemin ordinaire — donc reste un
/// champ non mappe si le mapping ne le connait pas.
const CHAMPS: &[&str] = &[
    "_id",
    "_index",
    "_routing",
    "_seq_no",
    "_type",
    "_source",
    "_field_names",
    "_version",
    "_ignored",
];

/// La clause qui cite le champ. Chacune a sa propre phrase de refus chez ES :
/// ce n'est pas un detail cosmetique, c'est ce qu'un client lit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clause {
    Term,
    Terms,
    Match,
    MatchPhrase,
    MatchPhrasePrefix,
    Prefix,
    Wildcard,
    Regexp,
    Fuzzy,
    Range,
    Exists,
}

/// Ce que la clause designe, une fois le champ de metadonnees resolu.
///
/// Il n'y a que trois formes possibles parce qu'un champ de metadonnees ne se
/// lit pas dans l'index inverse : `_index` vaut la meme chose pour **tous** les
/// documents d'un index, et `_id` en designe une liste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Tous les documents de cet index.
    Tous,
    /// Aucun — le vide **juste**, celui qu'ES rend lui aussi.
    Aucun,
    /// Les documents dont l'identifiant figure dans la liste.
    Ids(Vec<String>),
}

/// Ce nom est-il celui d'un champ de metadonnees ?
pub fn est_meta(champ: &str) -> bool {
    CHAMPS.contains(&champ)
}

/// Le refus qu'ES prononce quand une clause de motif vise un champ dont le type
/// ne la supporte pas. Le type d'un champ de metadonnees porte son propre nom
/// (`[_id] which is of type [_id]`), ce qui rend la phrase lisible telle quelle.
fn refus_type(champ: &str, clause: Clause) -> EsError {
    let phrase = match clause {
        Clause::Prefix => format!(
            "Can only use prefix queries on keyword, text and wildcard fields - not on [{champ}] \
             which is of type [{champ}]"
        ),
        Clause::Wildcard => format!(
            "Can only use wildcard queries on keyword, text and wildcard fields - not on \
             [{champ}] which is of type [{champ}]"
        ),
        Clause::Regexp => format!(
            "Can only use regexp queries on keyword and text fields - not on [{champ}] which is \
             of type [{champ}]"
        ),
        Clause::Fuzzy => format!(
            "failed to create query: Can only use fuzzy queries on keyword and text fields - not \
             on [{champ}] which is of type [{champ}]"
        ),
        Clause::MatchPhrasePrefix => format!(
            "failed to create query: Can only use phrase prefix queries on text fields - not on \
             [{champ}] which is of type [{champ}]"
        ),
        Clause::Range => format!(
            "failed to create query: Field [{champ}] of type [{champ}] does not support range \
             queries"
        ),
        Clause::Match | Clause::MatchPhrase => format!(
            "failed to create query: Field [{champ}] of type [{champ}] does not support match \
             queries"
        ),
        // Les trois qui suivent n'arrivent jamais par ce chemin : `term`,
        // `terms` et `exists` ont chacun leur phrase, plus haut.
        Clause::Term | Clause::Terms | Clause::Exists => {
            format!("Field [{champ}] of type [{champ}] does not support this query")
        }
    };
    query_shard(phrase)
}

fn query_shard(phrase: String) -> EsError {
    EsError::new(
        axum::http::StatusCode::BAD_REQUEST,
        "query_shard_exception",
        phrase,
    )
}

/// Le refus de ferrite la ou ES sait repondre : un cout de perimetre, nomme.
///
/// Il ne porte **pas** la marque « champ inconnu » : c'est elle qui faisait
/// avaler la clause en « ne correspond a rien », et c'est exactement ce qu'on
/// ne veut plus.
fn hors_perimetre(champ: &str, clause: Clause, raison: &str) -> EsError {
    EsError::unsupported(format!(
        "ferrite ne supporte pas le champ de metadonnees [{champ}] dans [{}] : {raison}",
        nom_clause(clause)
    ))
}

fn nom_clause(clause: Clause) -> &'static str {
    match clause {
        Clause::Term => "term",
        Clause::Terms => "terms",
        Clause::Match => "match",
        Clause::MatchPhrase => "match_phrase",
        Clause::MatchPhrasePrefix => "match_phrase_prefix",
        Clause::Prefix => "prefix",
        Clause::Wildcard => "wildcard",
        Clause::Regexp => "regexp",
        Clause::Fuzzy => "fuzzy",
        Clause::Range => "range",
        Clause::Exists => "exists",
    }
}

/// La valeur d'une clause, telle qu'un champ de metadonnees la lit : une chaine.
///
/// ES lit toute **valeur simple** et la rend en texte — c'est le meme piege que
/// `_bulk` avait paye sur un `_id` numerique. Un objet ou un tableau, lui, est
/// refuse en le nommant.
fn texte(champ: &str, clause: Clause, v: &Value) -> EsResult<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(EsError::illegal_argument(format!(
            "[{}] sur [{champ}] : une valeur simple est attendue",
            nom_clause(clause)
        ))),
    }
}

/// Le motif d'un nom d'index : **seule** l'etoile y est un joker.
///
/// C'est `org.elasticsearch.common.regex.Regex.simpleMatch`, celui que le
/// `ConstantFieldType` d'ES applique a `_index` — et il n'a pas la syntaxe de
/// `wildcard`. Mesure contre 8.15 sur deux index nommes `sonde-meta` et
/// `sonde-meta-b` : `sonde-meta*` en rend 3 documents, `*meta*` aussi, mais
/// `sonde-met?` en rend **zero** (le `?` est un caractere ordinaire) et
/// `sonde-meta\*` zero aussi (la barre oblique inverse n'echappe rien).
///
/// La difference n'est pas cosmetique : lire la valeur comme une chaine exacte
/// — ce qu'elle a l'air d'etre — rendait **zero document** la ou ES en rend
/// trois, en 200. C'est la meme famille de defaut que celui de la carte.
fn simple_match(motif: &str, sujet: &str) -> bool {
    match motif.find('*') {
        None => motif == sujet,
        Some(0) if motif.len() == 1 => true,
        Some(i) => {
            let (debut, reste) = (&motif[..i], &motif[i + 1..]);
            if !sujet.starts_with(debut) {
                return false;
            }
            let sujet = &sujet[debut.len()..];
            // Le suffixe apres l'etoile peut commencer n'importe ou : on essaie
            // chaque frontiere de caractere, du plus court au plus long.
            (0..=sujet.len())
                .filter(|k| sujet.is_char_boundary(*k))
                .any(|k| simple_match(reste, &sujet[k..]))
        }
    }
}

/// Un motif de nom d'index confronte au nom de l'index interroge.
///
/// Une valeur qui commence par `_` n'est pas un nom d'index valide, et ES la
/// **refuse** au lieu de ne rien lui faire correspondre : `{"term": {"_index":
/// "_doc"}}` y rend `Invalid index name [_doc], must not start with '_'.`. La
/// seule exception est `_all` **exactement**, qui designe tous les index —
/// `_none`, `_all*` et le simple `_` sont refuses. Mesure contre 8.15 ; c'est
/// le fuzzer qui l'a sortie, sur un `terms` que rien d'ecrit a la main n'aurait
/// pose.
fn motif_index(motif: &str, nom: &str) -> EsResult<bool> {
    if motif == "_all" {
        return Ok(true);
    }
    if motif.starts_with('_') {
        // Le type est celui d'ES, releve mot pour mot : `invalid_index_name_exception`
        // en recherche (il l'enveloppe dans « all shards failed »),
        // `failed to create query: …` dans l'explication de `_validate/query`.
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_index_name_exception",
            format!("Invalid index name [{motif}], must not start with '_'."),
        ));
    }
    Ok(simple_match(motif, nom))
}

/// Ce que ferrite fait de cette clause posee sur ce champ.
///
/// `None` quand le nom n'est pas celui d'un champ de metadonnees : la clause
/// suit alors son chemin ordinaire.
///
/// `valeurs` porte ce que la clause cite (une seule pour `term`, la liste pour
/// `terms`, rien pour `exists` et `range`). `nom_index` est celui de l'index
/// interroge — `None` quand la requete est traduite sans qu'aucun index ne soit
/// vise (une validation), ou `_index` ne peut designer personne.
pub fn clause(
    champ: &str,
    clause: Clause,
    valeurs: &[Value],
    nom_index: Option<&str>,
) -> Option<EsResult<Verdict>> {
    if !est_meta(champ) {
        return None;
    }
    Some(resoudre(champ, clause, valeurs, nom_index))
}

fn resoudre(
    champ: &str,
    cl: Clause,
    valeurs: &[Value],
    nom_index: Option<&str>,
) -> EsResult<Verdict> {
    use Clause::*;
    match champ {
        // `_id` : ES le sert en `term`, `terms`, `match` et `match_phrase` — et
        // sans analyser la valeur, mesure a l'appui (`match _id "a b"` rend
        // zero document, la ou un `match` analyse en aurait trouve deux).
        "_id" => match cl {
            Term | Terms | Match | MatchPhrase => {
                let mut ids = Vec::with_capacity(valeurs.len());
                for v in valeurs {
                    ids.push(texte(champ, cl, v)?);
                }
                Ok(Verdict::Ids(ids))
            }
            Exists => Ok(Verdict::Tous),
            _ => Err(refus_type(champ, cl)),
        },
        // `_index` vaut la meme chose pour tous les documents d'un index : la
        // clause est donc un predicat sur un nom, et son resultat est « tous »
        // ou « aucun ». C'est son seul usage reel — filtrer un index dans une
        // recherche qui en vise plusieurs.
        "_index" => {
            let Some(nom) = nom_index else {
                // Aucun index vise : la clause se valide sans designer personne.
                return Ok(Verdict::Aucun);
            };
            match cl {
                // La valeur n'est pas une chaine exacte mais une **expression
                // de nom d'index** : `{"term": {"_index": "logs-*"}}` designe
                // tous les index qui commencent par `logs-`, et c'est mesure.
                Term | Terms | Match | MatchPhrase | Wildcard => {
                    for v in valeurs {
                        if motif_index(&texte(champ, cl, v)?, nom)? {
                            return Ok(Verdict::Tous);
                        }
                    }
                    Ok(Verdict::Aucun)
                }
                // `prefix` est le meme motif, une etoile en plus au bout — d'ou
                // le fait qu'une valeur qui en porte deja une marche aussi, et
                // que le refus d'un nom invalide cite le motif **avec** son
                // etoile (mesure : `prefix: "_all"` rend « Invalid index name
                // [_all*] »).
                Prefix => {
                    let p = texte(champ, cl, valeurs.first().unwrap_or(&Value::Null))?;
                    Ok(si(motif_index(&format!("{p}*"), nom)?))
                }
                Exists => Ok(Verdict::Tous),
                _ => Err(refus_type(champ, cl)),
            }
        }
        // `_type` n'existe plus depuis la 8.0 : ES rend 200 et **aucun**
        // document sur chacune de ces clauses. Le vide y est la parite.
        "_type" => Ok(Verdict::Aucun),
        // `_source` n'est pas cherchable, et ES le dit avec deux phrases
        // differentes selon la clause.
        "_source" => match cl {
            Term | Terms | Exists => Err(query_shard("The _source field is not searchable".into())),
            _ => Err(refus_type(champ, cl)),
        },
        // `_version` non plus — sauf `exists`, qui rend tous les documents
        // puisque chacun en porte une.
        "_version" => match cl {
            Term | Terms => Err(query_shard("The _version field is not searchable".into())),
            Exists => Ok(Verdict::Tous),
            _ => Err(refus_type(champ, cl)),
        },
        // `_field_names` ne porte plus que les champs sans colonne ni norme :
        // les clauses de terme y rendent vide, et `exists` y est refuse par une
        // phrase a lui.
        "_field_names" => match cl {
            Term | Terms | Match | MatchPhrase => Ok(Verdict::Aucun),
            Exists => Err(query_shard(
                "failed to create query: Cannot run exists query on _field_names".into(),
            )),
            _ => Err(refus_type(champ, cl)),
        },
        // Les trois que ferrite ne sait pas servir. ES repond sur chacun ; le
        // refus est donc un cout de perimetre, et il est nomme.
        "_routing" => Err(hors_perimetre(
            champ,
            cl,
            "ferrite est mono-shard et refuse [?routing=] a l'indexation, donc aucun document \
             n'en porte ; rendre un resultat vide serait indiscernable d'un document sans \
             routage",
        )),
        "_seq_no" => match cl {
            // Chaque document en porte un : `exists` est vrai partout, comme
            // chez ES.
            Exists => Ok(Verdict::Tous),
            Prefix | Wildcard | Regexp | Fuzzy | MatchPhrasePrefix => Err(refus_type(champ, cl)),
            _ => Err(hors_perimetre(
                champ,
                cl,
                "le numero de sequence est tenu par ferrite mais n'est pas interrogeable en \
                 clause (ES y repond)",
            )),
        },
        "_ignored" => Err(hors_perimetre(
            champ,
            cl,
            "ferrite ne tient pas la liste des valeurs qu'un [ignore_above] a ecartees (ES y \
             repond)",
        )),
        // `est_meta` a deja filtre : ce bras est inatteignable.
        autre => Err(EsError::internal(format!(
            "champ de metadonnees [{autre}] non traite"
        ))),
    }
}

fn si(vrai: bool) -> Verdict {
    if vrai {
        Verdict::Tous
    } else {
        Verdict::Aucun
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v(s: &str) -> Vec<Value> {
        vec![json!(s)]
    }

    #[test]
    fn id_est_servi_par_term_et_refuse_par_prefix() {
        assert_eq!(
            clause("_id", Clause::Term, &v("a"), Some("i"))
                .unwrap()
                .unwrap(),
            Verdict::Ids(vec!["a".into()])
        );
        let e = clause("_id", Clause::Prefix, &v("a"), Some("i"))
            .unwrap()
            .unwrap_err();
        assert!(e.to_string().contains("which is of type [_id]"), "{e:?}");
    }

    #[test]
    fn id_numerique_devient_une_chaine() {
        assert_eq!(
            clause("_id", Clause::Term, &[json!(42)], Some("i"))
                .unwrap()
                .unwrap(),
            Verdict::Ids(vec!["42".into()])
        );
    }

    #[test]
    fn index_est_un_predicat_sur_le_nom() {
        let t =
            |cl, val: &str, nom: &str| clause("_index", cl, &v(val), Some(nom)).unwrap().unwrap();
        assert_eq!(t(Clause::Term, "logs", "logs"), Verdict::Tous);
        assert_eq!(t(Clause::Term, "logs", "autre"), Verdict::Aucun);
        // Un `term` sur `_index` lit une **expression de nom d'index**, pas une
        // chaine exacte : mesure contre ES 8.15.
        assert_eq!(t(Clause::Term, "log*", "logs"), Verdict::Tous);
        assert_eq!(t(Clause::Wildcard, "log*", "logs"), Verdict::Tous);
        // ... mais seule l'etoile y est un joker.
        assert_eq!(t(Clause::Term, "log?", "logs"), Verdict::Aucun);
        assert_eq!(t(Clause::Prefix, "log", "logs"), Verdict::Tous);
        assert_eq!(t(Clause::Prefix, "logs-", "logs"), Verdict::Aucun);
    }

    #[test]
    fn type_rend_vide_comme_es() {
        for cl in [Clause::Term, Clause::Regexp, Clause::Range, Clause::Exists] {
            assert_eq!(
                clause("_type", cl, &v("_doc"), Some("i")).unwrap().unwrap(),
                Verdict::Aucun
            );
        }
    }

    #[test]
    fn ce_que_ferrite_ne_sert_pas_est_refuse_pas_vide() {
        for champ in ["_routing", "_ignored"] {
            let e = clause(champ, Clause::Term, &v("x"), Some("i"))
                .unwrap()
                .unwrap_err();
            assert!(e.to_string().contains(champ), "{e:?}");
        }
        // `_seq_no` : `exists` passe, le reste est refuse.
        assert_eq!(
            clause("_seq_no", Clause::Exists, &[], Some("i"))
                .unwrap()
                .unwrap(),
            Verdict::Tous
        );
        assert!(clause("_seq_no", Clause::Term, &[json!(0)], Some("i"))
            .unwrap()
            .is_err());
    }

    #[test]
    fn un_champ_ordinaire_ne_passe_pas_par_ici() {
        assert!(clause("titre", Clause::Term, &v("x"), Some("i")).is_none());
        assert!(clause("_all_text", Clause::Term, &v("x"), Some("i")).is_none());
    }

    #[test]
    fn seule_l_etoile_est_un_joker() {
        assert!(simple_match("*-b", "sonde-meta-b"));
        assert!(simple_match("sonde-*-b", "sonde-meta-b"));
        assert!(!simple_match("sonde-*-c", "sonde-meta-b"));
        assert!(simple_match("*meta*", "sonde-meta-b"));
        assert!(simple_match("**", "sonde-meta-b"));
        assert!(simple_match("*", "n'importe quoi"));
        assert!(simple_match("sonde-meta-b", "sonde-meta-b"));
        // Les deux mesures qui separent `simpleMatch` de `wildcard`.
        assert!(!simple_match("sonde-met?", "sonde-meta"));
        assert!(!simple_match("sonde-meta\\*", "sonde-meta-b"));
        // Le motif s'applique au **nom entier**, pas a un morceau.
        assert!(!simple_match("meta", "sonde-meta"));
        // Un sujet non ASCII ne doit pas couper un caractere en deux.
        assert!(simple_match("é*é", "ééé"));
        assert!(!simple_match("é*z", "ééé"));
    }
}
