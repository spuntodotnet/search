//! `min_score` : le seuil de score d'un `_search`.
//!
//! Il ne se confond pas avec le `min_score` de `function_score`, qui vit
//! **dans** la clause (voir [`crate::fonction_score`]) : celui-ci est pose sur
//! la recherche entiere, et sa place dans la chaine est ce qui fait toute sa
//! valeur. Mesure contre ES 8.15, sur cinq documents dont les scores vont de
//! 0,120 a 0,141 :
//!
//! * il filtre **avant** `from` / `size`, donc il change `hits.total` —
//!   `min_score: 0.135` rend `total: 3`, et `from: 1, size: 1` y rend le
//!   quatrieme document sur trois, pas sur cinq. Un filtrage fait cote client
//!   apres coup ne donne ni le meme compte ni la meme pagination, et c'est
//!   exactement la raison pour laquelle le parametre existe ;
//! * il filtre **avant les agregations** : le meme `terms` rend
//!   `g1: 3, g0: 2` sans seuil et `g1: 2, g0: 1` avec. La question n'etait pas
//!   devinable — les agregations auraient tres bien pu voir l'ensemble complet ;
//! * `max_score` est celui des documents **retenus** ;
//! * il s'applique meme sous un `sort` par champ, ou la reponse ne porte plus
//!   aucun score (`min_score: 0.135` y garde 3 documents sur 5). C'est le piege
//!   deja paye sur le `boost` d'une clause : tantivy laisse tomber le calcul du
//!   score quand le collecteur ne le demande pas, et il faut le rallumer ;
//! * une requete purement filtrante note tout a **0.0** : `min_score: 0.5` sur
//!   un `bool.filter` ne rend donc aucun document, la aussi mesure.

use tantivy::query::{EnableScoring, Explanation, Query, Scorer, Weight};
use tantivy::{DocId, DocSet, Score, SegmentReader, Term, TERMINATED};

/// Une requete qui n'expose que les documents dont le score atteint le seuil.
pub struct Seuil {
    sous: Box<dyn Query>,
    minimum: f32,
}

impl Seuil {
    pub fn new(sous: Box<dyn Query>, minimum: f32) -> Self {
        Self { sous, minimum }
    }
}

impl std::fmt::Debug for Seuil {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Seuil({:?} >= {})", self.sous, self.minimum)
    }
}

impl Clone for Seuil {
    fn clone(&self) -> Self {
        Self {
            sous: self.sous.box_clone(),
            minimum: self.minimum,
        }
    }
}

impl Query for Seuil {
    fn weight(&self, enable_scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        // Le seuil **filtre** : sans score, le total et les agregations
        // seraient faux. On rallume donc le calcul quand le collecteur ne le
        // demande pas — un `Count`, ou un tri par champ.
        let scoring = match enable_scoring.searcher() {
            Some(searcher) => EnableScoring::enabled_from_searcher(searcher),
            None => enable_scoring,
        };
        Ok(Box::new(SeuilWeight {
            sous: self.sous.weight(scoring)?,
            minimum: self.minimum,
        }))
    }

    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        self.sous.query_terms(visitor);
    }
}

struct SeuilWeight {
    sous: Box<dyn Weight>,
    minimum: f32,
}

impl Weight for SeuilWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let mut scorer = SeuilScorer {
            sous: self.sous.scorer(reader, boost)?,
            minimum: self.minimum,
        };
        // Le seuil peut ecarter le premier document : le curseur doit deja etre
        // pose sur un document retenu quand le collecteur le lit.
        if scorer.doc() != TERMINATED && !scorer.retenu() {
            scorer.avancer_jusqu_a_retenu();
        }
        Ok(Box::new(scorer))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut scorer = self.scorer(reader, 1.0)?;
        if scorer.seek(doc) != doc {
            return Err(tantivy::TantivyError::InvalidArgument(format!(
                "document {doc} ne correspond pas a la requete"
            )));
        }
        Ok(Explanation::new("min_score", scorer.score()))
    }
}

struct SeuilScorer {
    sous: Box<dyn Scorer>,
    minimum: f32,
}

impl SeuilScorer {
    fn retenu(&mut self) -> bool {
        self.sous.score() >= self.minimum
    }

    fn avancer_jusqu_a_retenu(&mut self) -> DocId {
        loop {
            if self.sous.advance() == TERMINATED {
                return TERMINATED;
            }
            if self.retenu() {
                return self.sous.doc();
            }
        }
    }
}

impl DocSet for SeuilScorer {
    fn advance(&mut self) -> DocId {
        self.avancer_jusqu_a_retenu()
    }

    fn seek(&mut self, target: DocId) -> DocId {
        let doc = self.sous.seek(target);
        if doc == TERMINATED || self.retenu() {
            return doc;
        }
        self.avancer_jusqu_a_retenu()
    }

    fn doc(&self) -> DocId {
        self.sous.doc()
    }

    fn size_hint(&self) -> u32 {
        self.sous.size_hint()
    }
}

impl Scorer for SeuilScorer {
    fn score(&mut self) -> Score {
        if self.sous.doc() == TERMINATED {
            return 0.0;
        }
        self.sous.score()
    }
}
