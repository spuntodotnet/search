//! Chercher dans une **colonne**, quand il n'y a pas d'index inverse.
//!
//! C'est ce qu'`index: false` demande, et ce n'est pas ce que le refus d'avant
//! supposait. Un champ declare `index: false` n'est pas hors de portee chez
//! Elasticsearch : il perd son index inverse et **garde ses *doc values***, et
//! Lucene retombe dessus — `term`, `terms`, `range`, `match`, `prefix`,
//! `wildcard`, `regexp`, `fuzzy` y repondent, en balayant la colonne document
//! par document. Ce sont les requetes qu'Elastic appelle *slow queries* dans
//! sa documentation de `index: false`.
//!
//! Deux consequences que la mesure a donnees
//! ([`sonde_index_false.py`](../tests/compat/sonde_index_false.py), contre un
//! ES 8.15.0) :
//!
//! * le **score** n'est plus celui d'un index inverse. Une colonne ne porte ni
//!   frequence de terme ni longueur de champ : ES rend un score **constant de
//!   1.0** la ou un `term` sur un `keyword` indexe rend un BM25 (0.388…). Ce
//!   module est donc toujours a score constant, `boost` compris ;
//! * un `text` n'a **pas** de colonne (ce serait doubler le stockage du texte,
//!   chez Lucene comme ici) : il n'y a rien sur quoi retomber, et la clause est
//!   refusee. C'est [`crate::dsl`] qui prononce ce refus, avec la phrase d'ES.
//!
//! Le prix est celui d'un balayage : la clause visite tous les documents du
//! segment au lieu de sauter par l'index. C'est le prix qu'ES paie aussi, et
//! c'est ce qu'un champ non indexe echange contre la place qu'il ne prend pas.

use std::collections::BTreeSet;
use std::ops::Bound;
use std::sync::Arc;

use tantivy::columnar::{Column, StrColumn};
use tantivy::query::{ConstScorer, EnableScoring, Explanation, Query, Scorer, Weight};
use tantivy::{DocId, DocSet, Score, SegmentReader, TERMINATED};

use crate::mapping::{FieldKind, TypedValue};

/// Ce qu'une clause demande a une colonne.
pub enum Predicat {
    /// Une ou plusieurs valeurs exactes : `term`, `terms`, et `match` /
    /// `match_phrase` sur un champ qui n'est pas du texte.
    Valeurs(Vec<TypedValue>),
    /// Un intervalle : `range`, et la **periode** que designe une date.
    Intervalle {
        bas: Bound<TypedValue>,
        haut: Bound<TypedValue>,
    },
    /// Un automate confronte aux termes du dictionnaire de la colonne :
    /// `prefix`, `wildcard`, `regexp`, `fuzzy`. C'est le **meme** automate que
    /// celui de l'index inverse — sans quoi les deux chemins ne rendraient pas
    /// les memes documents.
    Automate(Automate),
}

/// Les deux automates que les clauses de motif compilent.
pub enum Automate {
    /// `prefix`, `wildcard`, `regexp` : tous trois passent par un motif que
    /// [`crate::regexp`] a deja traduit, et que tantivy compile pareil.
    Regex(tantivy_fst::Regex),
    /// `fuzzy` : l'automate de Levenshtein, construit avec les memes deux
    /// parametres que `FuzzyTermQuery` (distance, transpositions).
    Levenshtein(Box<levenshtein_automata::DFA>),
}

impl Automate {
    fn accepte(&self, terme: &[u8]) -> bool {
        match self {
            Self::Regex(re) => court(re, terme),
            Self::Levenshtein(dfa) => {
                matches!(dfa.eval(terme), levenshtein_automata::Distance::Exact(_))
            }
        }
    }
}

/// Fait courir un automate sur une suite d'octets.
///
/// `Dictionary::search` ferait de meme sur un flux, mais le dictionnaire d'une
/// colonne est deja parcouru entierement ici : le balayage domine, pas la
/// recherche du terme.
fn court<A: tantivy_fst::Automaton>(automate: &A, terme: &[u8]) -> bool {
    let mut etat = automate.start();
    for octet in terme {
        if !automate.can_match(&etat) {
            return false;
        }
        etat = automate.accept(&etat, *octet);
    }
    automate.is_match(&etat)
}

/// Une clause posee sur un champ `index: false`.
#[derive(Clone)]
pub struct ColonneQuery {
    champ: String,
    genre: FieldKind,
    predicat: Arc<Predicat>,
}

impl std::fmt::Debug for ColonneQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ColonneQuery({})", self.champ)
    }
}

impl ColonneQuery {
    pub fn new(champ: &str, genre: FieldKind, predicat: Predicat) -> Self {
        Self {
            champ: champ.to_string(),
            genre,
            predicat: Arc::new(predicat),
        }
    }
}

impl Query for ColonneQuery {
    fn weight(&self, _enable_scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(ColonneWeight {
            champ: self.champ.clone(),
            genre: self.genre,
            predicat: self.predicat.clone(),
        }))
    }
}

struct ColonneWeight {
    champ: String,
    genre: FieldKind,
    predicat: Arc<Predicat>,
}

impl ColonneWeight {
    /// Les documents du segment que le predicat retient.
    ///
    /// Un document multivalue correspond des qu'**une** de ses valeurs
    /// correspond — comme un index inverse, ou comme Lucene sur une colonne.
    fn documents(&self, reader: &SegmentReader) -> tantivy::Result<Vec<DocId>> {
        let colonnes = reader.fast_fields();
        let max_doc = reader.max_doc();
        match self.genre {
            // Un `text` n'a pas de colonne : la clause n'aurait pas du arriver
            // jusqu'ici (voir [`crate::dsl`]), et si elle y arrivait, ne rien
            // rendre vaut mieux que rendre n'importe quoi.
            FieldKind::Text => Ok(Vec::new()),
            FieldKind::Keyword => {
                let Some(colonne) = colonnes.str(&self.champ)? else {
                    return Ok(Vec::new());
                };
                let ords = self.ordinaux(&colonne)?;
                if ords.is_empty() {
                    return Ok(Vec::new());
                }
                Ok((0..max_doc)
                    .filter(|doc| colonne.term_ords(*doc).any(|ord| ords.contains(&ord)))
                    .collect())
            }
            FieldKind::I64 => {
                let colonne = colonnes.column_opt::<i64>(&self.champ)?;
                self.balaye(colonne, max_doc, TypedValue::I64)
            }
            FieldKind::F64 => {
                let colonne = colonnes.column_opt::<f64>(&self.champ)?;
                self.balaye(colonne, max_doc, TypedValue::F64)
            }
            FieldKind::Bool => {
                let colonne = colonnes.column_opt::<bool>(&self.champ)?;
                self.balaye(colonne, max_doc, TypedValue::Bool)
            }
            FieldKind::Date => {
                let colonne = colonnes.column_opt::<tantivy::DateTime>(&self.champ)?;
                self.balaye(colonne, max_doc, |d: tantivy::DateTime| {
                    TypedValue::Date(d.into_timestamp_millis())
                })
            }
        }
    }

    /// Les ordinaux du dictionnaire que le predicat retient.
    ///
    /// Passer par les ordinaux plutot que par les chaines est ce qui rend le
    /// balayage lisible : le dictionnaire est parcouru **une** fois, les
    /// documents ensuite.
    fn ordinaux(&self, colonne: &StrColumn) -> tantivy::Result<BTreeSet<u64>> {
        let mut out = BTreeSet::new();
        match &*self.predicat {
            // Une valeur exacte se demande au dictionnaire, sans le parcourir.
            Predicat::Valeurs(valeurs) => {
                for v in valeurs {
                    if let TypedValue::Str(s) = v {
                        if let Some(ord) = colonne.dictionary().term_ord(s.as_bytes())? {
                            out.insert(ord);
                        }
                    }
                }
            }
            Predicat::Intervalle { bas, haut } => {
                let mut flux = colonne.dictionary().stream()?;
                while flux.advance() {
                    if dans_intervalle(flux.key(), bas, haut) {
                        out.insert(flux.term_ord());
                    }
                }
            }
            Predicat::Automate(automate) => {
                let mut flux = colonne.dictionary().stream()?;
                while flux.advance() {
                    if automate.accepte(flux.key()) {
                        out.insert(flux.term_ord());
                    }
                }
            }
        }
        Ok(out)
    }

    /// Le balayage d'une colonne numerique (entiers, flottants, booleens,
    /// dates) : chaque valeur est ramenee au [`TypedValue`] du mapping, donc
    /// comparee exactement comme la clause l'a lue.
    fn balaye<T, F>(
        &self,
        colonne: Option<Column<T>>,
        max_doc: DocId,
        vers: F,
    ) -> tantivy::Result<Vec<DocId>>
    where
        T: PartialOrd + Copy + std::fmt::Debug + Send + Sync + 'static,
        F: Fn(T) -> TypedValue,
    {
        let Some(colonne) = colonne else {
            return Ok(Vec::new());
        };
        let retient = |valeur: TypedValue| match &*self.predicat {
            Predicat::Valeurs(valeurs) => valeurs.contains(&valeur),
            Predicat::Intervalle { bas, haut } => {
                borne_basse(&valeur, bas) && borne_haute(&valeur, haut)
            }
            // Un automate ne se pose pas sur un nombre : la clause est refusee
            // en amont (`fuzzy` sur un numerique rend deja 400 chez ES).
            Predicat::Automate(_) => false,
        };
        Ok((0..max_doc)
            .filter(|doc| colonne.values_for_doc(*doc).any(|v| retient(vers(v))))
            .collect())
    }
}

/// Ces deux predicats sont publics : le `range` sur un `boolean` d'un champ
/// **indexe** enumere lui aussi les valeurs que les bornes laissent passer, et
/// il doit le faire avec la meme regle.
///
/// L'ordre des valeurs d'un meme type, tel que le mapping les a lues.
///
/// Deux valeurs de types differents ne se comparent pas : le predicat est
/// construit au type du champ, donc le cas ne se presente pas — et rendre
/// `false` y est le choix sur : un document ne correspond pas.
fn compare(a: &TypedValue, b: &TypedValue) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (TypedValue::Str(x), TypedValue::Str(y)) => Some(x.cmp(y)),
        (TypedValue::I64(x), TypedValue::I64(y)) => Some(x.cmp(y)),
        (TypedValue::F64(x), TypedValue::F64(y)) => x.partial_cmp(y),
        (TypedValue::Bool(x), TypedValue::Bool(y)) => Some(x.cmp(y)),
        (TypedValue::Date(x), TypedValue::Date(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

pub fn borne_basse(valeur: &TypedValue, bas: &Bound<TypedValue>) -> bool {
    match bas {
        Bound::Unbounded => true,
        Bound::Included(b) => matches!(
            compare(valeur, b),
            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
        ),
        Bound::Excluded(b) => matches!(compare(valeur, b), Some(std::cmp::Ordering::Greater)),
    }
}

pub fn borne_haute(valeur: &TypedValue, haut: &Bound<TypedValue>) -> bool {
    match haut {
        Bound::Unbounded => true,
        Bound::Included(b) => matches!(
            compare(valeur, b),
            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
        ),
        Bound::Excluded(b) => matches!(compare(valeur, b), Some(std::cmp::Ordering::Less)),
    }
}

/// Le meme intervalle, sur les octets d'un terme du dictionnaire.
fn dans_intervalle(terme: &[u8], bas: &Bound<TypedValue>, haut: &Bound<TypedValue>) -> bool {
    let Ok(texte) = std::str::from_utf8(terme) else {
        return false;
    };
    let valeur = TypedValue::Str(texte.to_string());
    borne_basse(&valeur, bas) && borne_haute(&valeur, haut)
}

impl Weight for ColonneWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let docs = self.documents(reader)?;
        Ok(Box::new(ConstScorer::new(ListeDocs::new(docs), boost)))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        if !self.documents(reader)?.contains(&doc) {
            return Err(tantivy::TantivyError::InvalidArgument(format!(
                "document {doc} ne correspond pas a la requete"
            )));
        }
        Ok(Explanation::new(
            "ColonneQuery : lu dans la colonne, score constant",
            1.0,
        ))
    }

    // Pas de `count` a ecrire : celui par defaut construit le meme parcours et
    // le compte **contre le bitset des vivants**, ce qu'un comptage direct de
    // la liste oublierait — un `_count` rendrait alors plus que la recherche
    // qui le suit.
}

/// La liste des documents retenus par un balayage.
///
/// tantivy a bien un `VecDocSet`, mais il n'est compile que pour ses propres
/// tests : dix lignes ici valent mieux qu'une dependance sur ce qui n'est pas
/// publie.
struct ListeDocs {
    docs: Vec<DocId>,
    curseur: usize,
}

impl ListeDocs {
    fn new(docs: Vec<DocId>) -> Self {
        Self { docs, curseur: 0 }
    }

    fn courant(&self) -> DocId {
        self.docs.get(self.curseur).copied().unwrap_or(TERMINATED)
    }
}

impl DocSet for ListeDocs {
    fn advance(&mut self) -> DocId {
        self.curseur += 1;
        self.courant()
    }

    fn seek(&mut self, cible: DocId) -> DocId {
        // Les documents sont ranges dans l'ordre : la position se trouve par
        // dichotomie, et un `seek` ne recule jamais.
        let depart = self.curseur;
        self.curseur =
            depart + self.docs[depart.min(self.docs.len())..].partition_point(|d| *d < cible);
        self.courant()
    }

    fn doc(&self) -> DocId {
        self.courant()
    }

    fn size_hint(&self) -> u32 {
        self.docs.len() as u32
    }
}
