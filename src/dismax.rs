//! `dis_max` : le score du meilleur sous-requete l'emporte.
//!
//! # Pourquoi ce module existe
//!
//! tantivy 0.26 expose [`tantivy::query::DisjunctionMaxQuery`], mais elle ne
//! fait pas ce que son nom dit : quel que soit le `tie_breaker`, elle rend la
//! **somme** des scores, pas leur maximum. Verifie a la main sur deux documents
//! (`titre` = 0.1823, `corps` = 0.6489, resultat 0.8312 = la somme, la ou le max
//! vaut 0.6489), et confirme par lecture du code : la specialisation
//! `SpecializedScorer::TermUnion` court-circuite le `DisjunctionMaxCombiner`.
//! Le constructeur qui applique correctement le combineur
//! (`BufferedUnionScorer::build`) est `pub(crate)`, donc hors de portee.
//!
//! Or `best_fields` est le type **par defaut** de `multi_match` chez
//! Elasticsearch. S'en remettre a la version cassee reviendrait a rendre
//! silencieusement un classement `most_fields` a qui demande `best_fields` —
//! exactement le resultat faux presente comme complet que ce projet refuse.
//!
//! # Comment
//!
//! On n'ecrit pas d'union : l'ensemble des documents qui correspondent est
//! **exactement** celui d'un `BooleanQuery` en `should`, dont on delegue
//! integralement le parcours (`advance`, `seek`, `doc`). Seul le score est
//! recalcule, en repositionnant chaque sous-requete sur le document courant.
//! Aucune logique de `DocSet` maison, donc aucun risque d'ensemble de resultats
//! errone : au pire un score faux, et le test [`tests`] plus bas le verrouille.

use tantivy::query::{
    BooleanQuery, EnableScoring, Explanation, Occur, Query, QueryClone, Scorer, Weight,
};
use tantivy::{DocId, DocSet, Score, SegmentReader, Term, TERMINATED};

/// Combine plusieurs sous-requetes en gardant le meilleur score, plus
/// `tie_breaker x` la somme des autres — la formule d'Elasticsearch.
pub struct DisMaxQuery {
    disjuncts: Vec<Box<dyn Query>>,
    tie_breaker: Score,
}

impl DisMaxQuery {
    pub fn new(disjuncts: Vec<Box<dyn Query>>, tie_breaker: Score) -> Self {
        Self {
            disjuncts,
            tie_breaker,
        }
    }

    fn union(&self) -> BooleanQuery {
        BooleanQuery::new(
            self.disjuncts
                .iter()
                .map(|q| (Occur::Should, q.box_clone()))
                .collect(),
        )
    }
}

impl std::fmt::Debug for DisMaxQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DisMaxQuery(tie_breaker={}, {:?})",
            self.tie_breaker, self.disjuncts
        )
    }
}

impl Clone for DisMaxQuery {
    fn clone(&self) -> Self {
        Self {
            disjuncts: self.disjuncts.iter().map(|q| q.box_clone()).collect(),
            tie_breaker: self.tie_breaker,
        }
    }
}

impl Query for DisMaxQuery {
    fn weight(&self, enable_scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        let parts = self
            .disjuncts
            .iter()
            .map(|q| q.weight(enable_scoring))
            .collect::<tantivy::Result<Vec<_>>>()?;
        Ok(Box::new(DisMaxWeight {
            union: self.union().weight(enable_scoring)?,
            parts,
            tie_breaker: self.tie_breaker,
        }))
    }

    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        for disjunct in &self.disjuncts {
            disjunct.query_terms(visitor);
        }
    }
}

struct DisMaxWeight {
    union: Box<dyn Weight>,
    parts: Vec<Box<dyn Weight>>,
    tie_breaker: Score,
}

impl Weight for DisMaxWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        Ok(Box::new(DisMaxScorer {
            // Le parcours vient de tantivy ; on ne fait que le suivre.
            docs: self.union.scorer(reader, 1.0)?,
            parts: self
                .parts
                .iter()
                .map(|w| w.scorer(reader, boost))
                .collect::<tantivy::Result<Vec<_>>>()?,
            tie_breaker: self.tie_breaker,
        }))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut scorer = self.scorer(reader, 1.0)?;
        if scorer.seek(doc) != doc {
            return Err(tantivy::TantivyError::InvalidArgument(format!(
                "document {doc} ne correspond pas a la requete"
            )));
        }
        let mut explanation = Explanation::new("DisMax (best_fields)", scorer.score());
        for (i, part) in self.parts.iter().enumerate() {
            if let Ok(sub) = part.explain(reader, doc) {
                let _ = i;
                explanation.add_detail(sub);
            }
        }
        Ok(explanation)
    }

    fn count(&self, reader: &SegmentReader) -> tantivy::Result<u32> {
        self.union.count(reader)
    }
}

struct DisMaxScorer {
    docs: Box<dyn Scorer>,
    parts: Vec<Box<dyn Scorer>>,
    tie_breaker: Score,
}

impl DocSet for DisMaxScorer {
    fn advance(&mut self) -> DocId {
        self.docs.advance()
    }

    fn seek(&mut self, target: DocId) -> DocId {
        self.docs.seek(target)
    }

    fn doc(&self) -> DocId {
        self.docs.doc()
    }

    fn size_hint(&self) -> u32 {
        self.docs.size_hint()
    }
}

impl Scorer for DisMaxScorer {
    fn score(&mut self) -> Score {
        let doc = self.docs.doc();
        if doc == TERMINATED {
            return 0.0;
        }
        let (mut max, mut sum) = (0.0f32, 0.0f32);
        for part in &mut self.parts {
            // Les sous-requetes ne reculent jamais : le parcours de l'union est
            // monotone, donc `seek` en avant est toujours licite.
            if part.doc() < doc {
                part.seek(doc);
            }
            if part.doc() == doc {
                let score = part.score();
                max = max.max(score);
                sum += score;
            }
        }
        max + (sum - max) * self.tie_breaker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::collector::TopDocs;
    use tantivy::query::TermQuery;
    use tantivy::schema::{IndexRecordOption, Schema, TEXT};
    use tantivy::{doc, Index, IndexWriter};

    /// Verrouille la seule chose qui compte : le score est le **maximum**, pas
    /// la somme.
    ///
    /// Ce test echoue si une future version de tantivy change le comportement
    /// sur lequel ce module s'appuie — c'est le but. Sans lui, une regression
    /// de pertinence passerait inapercue.
    #[test]
    fn le_score_est_le_maximum_pas_la_somme() {
        let mut b = Schema::builder();
        let titre = b.add_text_field("titre", TEXT);
        let corps = b.add_text_field("corps", TEXT);
        let index = Index::create_in_ram(b.build());
        {
            let mut w: IndexWriter = index.writer(15_000_000).unwrap();
            w.add_document(doc!(
                titre => "tres bon",
                corps => "tres bon appareil pour un usage courant"
            ))
            .unwrap();
            w.add_document(doc!(
                titre => "tres bon",
                corps => "appareil pour un usage courant"
            ))
            .unwrap();
            w.commit().unwrap();
        }
        let searcher = index.reader().unwrap().searcher();
        let terme = |field| -> Box<dyn Query> {
            Box::new(TermQuery::new(
                Term::from_field_text(field, "tres"),
                IndexRecordOption::WithFreqs,
            ))
        };
        let score_seul = |field| {
            searcher
                .search(&terme(field), &TopDocs::with_limit(2).order_by_score())
                .unwrap()[0]
                .0
        };
        let (s_titre, s_corps) = (score_seul(titre), score_seul(corps));
        assert!(
            s_corps > s_titre,
            "le fixture doit avoir deux scores distincts"
        );

        let dismax = DisMaxQuery::new(vec![terme(titre), terme(corps)], 0.0);
        let top = searcher
            .search(&dismax, &TopDocs::with_limit(2).order_by_score())
            .unwrap();
        let meilleur = top[0].0;

        assert!(
            (meilleur - s_corps).abs() < 1e-5,
            "dis_max doit rendre le max ({s_corps}), pas {meilleur}"
        );
        assert!(
            (meilleur - (s_titre + s_corps)).abs() > 1e-5,
            "dis_max ne doit pas rendre la somme"
        );
    }

    #[test]
    fn le_tie_breaker_ajoute_une_fraction_des_autres() {
        let mut b = Schema::builder();
        let titre = b.add_text_field("titre", TEXT);
        let corps = b.add_text_field("corps", TEXT);
        let index = Index::create_in_ram(b.build());
        {
            let mut w: IndexWriter = index.writer(15_000_000).unwrap();
            w.add_document(doc!(titre => "tres bon", corps => "tres bon appareil pour un usage"))
                .unwrap();
            w.commit().unwrap();
        }
        let searcher = index.reader().unwrap().searcher();
        let terme = |field| -> Box<dyn Query> {
            Box::new(TermQuery::new(
                Term::from_field_text(field, "tres"),
                IndexRecordOption::WithFreqs,
            ))
        };
        let score = |tie| {
            searcher
                .search(
                    &DisMaxQuery::new(vec![terme(titre), terme(corps)], tie),
                    &TopDocs::with_limit(1).order_by_score(),
                )
                .unwrap()[0]
                .0
        };
        let (max, avec_tie) = (score(0.0), score(1.0));
        assert!(
            avec_tie > max,
            "tie_breaker=1.0 doit valoir la somme ({avec_tie} > {max})"
        );
    }
}
