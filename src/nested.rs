//! `nested` : interroger un tableau d'objets element par element.
//!
//! # Le probleme
//!
//! Un `object` est aplati : `[{ref: A, qte: 5}, {ref: B, qte: 20}]` devient
//! `l.ref = [A, B]` et `l.qte = [5, 20]`. Chercher « une ligne A d'au moins 20 »
//! trouve alors le document, alors qu'aucune de ses lignes ne satisfait les deux
//! conditions. C'est le comportement d'Elasticsearch pour un `object`, et c'est
//! precisement ce que `nested` corrige.
//!
//! # Comment, sans jointure de bloc
//!
//! Lucene resout ca en indexant chaque sous-objet comme un document cache, puis
//! en remontant au parent par une jointure de bloc. tantivy n'a pas cette
//! jointure. ferrite prend l'autre chemin, permis par une propriete mesuree
//! (`tests/spike_nested.rs`) : **l'ordre des valeurs d'un champ multivalue est
//! conserve**. Chaque champ sous un `nested` a donc une colonne jumelle qui dit,
//! pour chaque valeur, de quel element du tableau elle vient :
//!
//! ```text
//! l.ref        ["A", "B"]        l.qte        [5, 20]
//! _elem.l.ref  [ 0,   1 ]        _elem.l.qte  [0,  1 ]
//! ```
//!
//! Une clause `nested` s'evalue alors en deux temps :
//!
//! 1. **pre-filtre** — la requete interne est executee telle quelle, a plat.
//!    Elle rend un sur-ensemble exact des candidats, avec les postings et le
//!    score de tantivy ;
//! 2. **verification** — pour chaque candidat, chaque clause feuille donne
//!    l'ensemble des elements ou elle est vraie ; la structure booleenne les
//!    combine ; si l'intersection finale est vide, le document est ecarte.
//!
//! Aucun faux positif, aucun faux negatif. Ce que ce chemin ne donne pas, c'est
//! le score *par element* (`score_mode`) : les colonnes portent la valeur, pas
//! les postings. Les clauses qui ne se verifient pas sur colonne sont refusees
//! explicitement plutot que rendues approximativement.

use std::collections::BTreeSet;
use std::ops::Bound;

use tantivy::columnar::Column;
use tantivy::query::{EnableScoring, Explanation, Query, QueryClone, Scorer, Weight};
use tantivy::schema::Field;
use tantivy::{DocId, DocSet, Score, SegmentReader, TantivyError, TERMINATED};

use crate::error::EsResult;
use crate::mapping::{FieldKind, MappedField, P_NELEM};

/// Une condition sur un champ, evaluable element par element.
#[derive(Debug, Clone)]
pub enum Predicat {
    /// Egalite exacte (`term`), ou appartenance a un ensemble (`terms`).
    Parmi(Vec<Valeur>),
    /// `range`, sur le type du champ.
    Intervalle(Bound<Valeur>, Bound<Valeur>),
    /// `exists` : l'element porte une valeur pour ce champ.
    Existe,
    /// `prefix` sur un champ textuel non analyse.
    Prefixe(String),
}

/// Une valeur comparable, deja convertie au type du champ.
#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub enum Valeur {
    Str(String),
    I64(i64),
    F64(f64),
    Bool(bool),
}

/// L'arbre de la requete interne, reduit a ce qui se verifie sur colonnes.
#[derive(Debug, Clone)]
pub enum Clause {
    /// Tous les elements.
    Tous,
    /// Aucun.
    Aucun,
    Champ {
        /// Le chemin complet (`l.ref`), pour les messages d'erreur.
        chemin: String,
        champ: MappedField,
        predicat: Predicat,
    },
    Et(Vec<Clause>),
    /// `should` avec son `minimum_should_match` deja resolu.
    Ou(Vec<Clause>, usize),
    Non(Box<Clause>),
}

/// Les colonnes d'un segment, resolues une fois par requete et par segment.
struct Colonnes<'a> {
    reader: &'a SegmentReader,
    racine: &'a str,
}

/// L'ensemble des elements d'un document ou une clause est vraie.
type Elements = BTreeSet<u32>;

impl Colonnes<'_> {
    /// Le nombre d'elements du `nested` pour ce document.
    fn cardinal(&self, doc: DocId) -> u32 {
        self.reader
            .fast_fields()
            .u64(&format!("{P_NELEM}{}", self.racine))
            .ok()
            .and_then(|c| c.first(doc))
            .unwrap_or(0) as u32
    }

    /// Les elements ou `champ` porte une valeur qui satisfait `predicat`.
    fn evalue(&self, doc: DocId, chemin: &str, champ: &MappedField, p: &Predicat) -> Elements {
        let ff = self.reader.fast_fields();
        let Some(elem_field) = champ.elem else {
            return Elements::new();
        };
        let nom_elem = self.reader.schema().get_field_name(elem_field).to_string();
        let Ok(elems) = ff.u64(&nom_elem) else {
            return Elements::new();
        };
        let elems: Vec<u32> = elems.values_for_doc(doc).map(|v| v as u32).collect();
        if elems.is_empty() {
            return Elements::new();
        }

        let mut out = Elements::new();
        let mut garde = |i: usize, ok: bool| {
            if ok {
                if let Some(e) = elems.get(i) {
                    out.insert(*e);
                }
            }
        };

        match champ.ty.kind() {
            FieldKind::Text | FieldKind::Keyword => {
                let Ok(Some(col)) = ff.str(chemin) else {
                    return out;
                };
                let mut buf = String::new();
                for (i, ord) in col.term_ords(doc).enumerate() {
                    buf.clear();
                    if col.ord_to_str(ord, &mut buf).unwrap_or(false) {
                        garde(i, satisfait_str(&buf, p));
                    }
                }
            }
            FieldKind::I64 => lis(ff.i64(chemin), doc, |i, v| {
                garde(i, satisfait(&Valeur::I64(v), p))
            }),
            FieldKind::Date => lis(ff.date(chemin), doc, |i, v| {
                garde(i, satisfait(&Valeur::I64(v.into_timestamp_millis()), p))
            }),
            FieldKind::F64 => lis(ff.f64(chemin), doc, |i, v| {
                garde(i, satisfait(&Valeur::F64(v), p))
            }),
            FieldKind::Bool => lis(ff.bool(chemin), doc, |i, v| {
                garde(i, satisfait(&Valeur::Bool(v), p))
            }),
        }
        out
    }
}

fn lis<T: tantivy::columnar::HasAssociatedColumnType + PartialOrd + Copy>(
    col: tantivy::Result<Column<T>>,
    doc: DocId,
    mut f: impl FnMut(usize, T),
) {
    if let Ok(col) = col {
        for (i, v) in col.values_for_doc(doc).enumerate() {
            f(i, v);
        }
    }
}

fn satisfait_str(v: &str, p: &Predicat) -> bool {
    match p {
        Predicat::Existe => true,
        Predicat::Prefixe(pre) => v.starts_with(pre.as_str()),
        _ => satisfait(&Valeur::Str(v.to_string()), p),
    }
}

fn satisfait(v: &Valeur, p: &Predicat) -> bool {
    match p {
        Predicat::Existe => true,
        Predicat::Parmi(vals) => vals.contains(v),
        Predicat::Prefixe(_) => false,
        Predicat::Intervalle(bas, haut) => {
            let apres = match bas {
                Bound::Unbounded => true,
                Bound::Included(b) => v >= b,
                Bound::Excluded(b) => v > b,
            };
            let avant = match haut {
                Bound::Unbounded => true,
                Bound::Included(b) => v <= b,
                Bound::Excluded(b) => v < b,
            };
            apres && avant
        }
    }
}

impl Clause {
    /// Les elements du document ou cette clause est vraie.
    fn elements(&self, doc: DocId, cols: &Colonnes, tous: &Elements) -> Elements {
        match self {
            Self::Tous => tous.clone(),
            Self::Aucun => Elements::new(),
            Self::Champ {
                chemin,
                champ,
                predicat,
            } => cols.evalue(doc, chemin, champ, predicat),
            Self::Et(sous) => {
                let mut it = sous.iter();
                let Some(premier) = it.next() else {
                    return tous.clone();
                };
                let mut acc = premier.elements(doc, cols, tous);
                for c in it {
                    if acc.is_empty() {
                        return acc;
                    }
                    let autre = c.elements(doc, cols, tous);
                    acc.retain(|e| autre.contains(e));
                }
                acc
            }
            Self::Ou(sous, minimum) => {
                // `minimum_should_match` se compte par element, comme ES le
                // compte par document cache.
                let mut compte: Vec<(u32, usize)> = Vec::new();
                for c in sous {
                    for e in c.elements(doc, cols, tous) {
                        match compte.iter_mut().find(|(x, _)| *x == e) {
                            Some((_, n)) => *n += 1,
                            None => compte.push((e, 1)),
                        }
                    }
                }
                compte
                    .into_iter()
                    .filter(|(_, n)| *n >= (*minimum).max(1))
                    .map(|(e, _)| e)
                    .collect()
            }
            Self::Non(sous) => {
                let interdits = sous.elements(doc, cols, tous);
                tous.iter()
                    .filter(|e| !interdits.contains(e))
                    .copied()
                    .collect()
            }
        }
    }
}

/// La requete `nested` : un pre-filtre en postings, puis la verification.
pub struct NestedQuery {
    racine: String,
    prefiltre: Box<dyn Query>,
    clause: Clause,
    /// Les champs consultes, pour verifier a l'ouverture du segment que les
    /// colonnes existent (un index vide n'en a aucune).
    nelem: Option<Field>,
}

impl NestedQuery {
    pub fn new(racine: String, prefiltre: Box<dyn Query>, clause: Clause) -> EsResult<Self> {
        Ok(Self {
            racine,
            prefiltre,
            clause,
            nelem: None,
        })
    }
}

impl Clone for NestedQuery {
    fn clone(&self) -> Self {
        Self {
            racine: self.racine.clone(),
            prefiltre: self.prefiltre.box_clone(),
            clause: self.clause.clone(),
            nelem: self.nelem,
        }
    }
}

impl std::fmt::Debug for NestedQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NestedQuery({})", self.racine)
    }
}

impl Query for NestedQuery {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(NestedWeight {
            racine: self.racine.clone(),
            interne: self.prefiltre.weight(scoring)?,
            clause: self.clause.clone(),
        }))
    }
}

struct NestedWeight {
    racine: String,
    interne: Box<dyn Weight>,
    clause: Clause,
}

impl Weight for NestedWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let mut interne = self.interne.scorer(reader, boost)?;
        let cols = Colonnes {
            reader,
            racine: &self.racine,
        };
        // La verification est faite une fois, a la construction : elle lit des
        // colonnes, pas des postings, et le pre-filtre a deja reduit le champ.
        let mut retenus: Vec<(DocId, Score)> = Vec::new();
        let mut doc = interne.doc();
        while doc != TERMINATED {
            let n = cols.cardinal(doc);
            let tous: Elements = (0..n).collect();
            if !self.clause.elements(doc, &cols, &tous).is_empty() {
                retenus.push((doc, interne.score()));
            }
            doc = interne.advance();
        }
        Ok(Box::new(VerifieScorer { retenus, pos: 0 }))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut scorer = self.scorer(reader, 1.0)?;
        if scorer.seek(doc) != doc {
            return Err(TantivyError::InvalidArgument(format!(
                "document {doc} hors de la requete nested"
            )));
        }
        Ok(Explanation::new("NestedQuery", scorer.score()))
    }
}

/// Le `DocSet` des documents dont au moins un element satisfait la clause.
struct VerifieScorer {
    retenus: Vec<(DocId, Score)>,
    pos: usize,
}

impl DocSet for VerifieScorer {
    fn advance(&mut self) -> DocId {
        self.pos += 1;
        self.doc()
    }

    fn doc(&self) -> DocId {
        self.retenus.get(self.pos).map_or(TERMINATED, |(d, _)| *d)
    }

    fn size_hint(&self) -> u32 {
        self.retenus.len() as u32
    }
}

impl Scorer for VerifieScorer {
    fn score(&mut self) -> Score {
        self.retenus.get(self.pos).map_or(0.0, |(_, s)| *s)
    }
}
