//! `post_filter` : filtrer les hits **sans** toucher aux agregations.
//!
//! Le parametre n'est pas une clause de plus dans le `bool` : c'est la
//! mecanique meme d'une page a facettes, ou choisir « rouge » ne doit pas faire
//! disparaitre les autres couleurs du panneau de gauche. Sa place dans la
//! chaine **est** sa definition, et chaque moitie se mesure contre un ES 8.15 :
//!
//! * il filtre les hits, donc `hits.total` et la pagination — `post_filter` sur
//!   deux documents d'un resultat de quatre rend bien `total: 2` ;
//! * il ne filtre **pas** les agregations : le meme `terms` rend les memes
//!   seaux avec et sans lui. C'est tout l'objet du parametre ;
//! * il ne **note** rien. Un `boost: 10` pose dedans ne deplace pas le `_score`
//!   d'un hit — d'ou ce module plutot qu'une intersection booleenne, dont le
//!   score serait une somme ;
//! * il n'apparait pas dans l'arbre d'`explain` et ne surligne rien : les deux
//!   lisent la requete **sans** lui ;
//! * ses clauses nommees sortent quand meme dans `matched_queries`, dans la
//!   meme liste que celles de la requete (`["q", "pf"]`, mesure).

use std::cmp::Ordering;

use tantivy::query::{EnableScoring, Explanation, Query, Scorer, Weight};
use tantivy::{DocId, DocSet, Score, SegmentReader, Term, TERMINATED};

/// Une requete qui ne rend que les documents d'une seconde, **avec le score de
/// la premiere**.
pub struct Filtre {
    sous: Box<dyn Query>,
    filtre: Box<dyn Query>,
}

impl Filtre {
    pub fn new(sous: Box<dyn Query>, filtre: Box<dyn Query>) -> Self {
        Self { sous, filtre }
    }
}

impl std::fmt::Debug for Filtre {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Filtre({:?} apres {:?})", self.sous, self.filtre)
    }
}

impl Clone for Filtre {
    fn clone(&self) -> Self {
        Self {
            sous: self.sous.box_clone(),
            filtre: self.filtre.box_clone(),
        }
    }
}

impl Query for Filtre {
    fn weight(&self, enable_scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(FiltreWeight {
            sous: self.sous.weight(enable_scoring)?,
            filtre: self.filtre.weight(enable_scoring)?,
        }))
    }

    /// Les termes du **filtre** ne sont pas ceux de la requete : le surlignage
    /// lit cette liste, et ES ne marque pas ce qu'un `post_filter` a trouve
    /// (mesure). Seule la sous-requete se declare.
    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        self.sous.query_terms(visitor);
    }
}

struct FiltreWeight {
    sous: Box<dyn Weight>,
    filtre: Box<dyn Weight>,
}

impl Weight for FiltreWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        // Le `boost` ne va qu'a la sous-requete : le filtre ne note rien, donc
        // rien ne peut le multiplier.
        let mut scorer = FiltreScorer {
            sous: self.sous.scorer(reader, boost)?,
            filtre: self.filtre.scorer(reader, 1.0)?,
        };
        scorer.aligner();
        Ok(Box::new(scorer))
    }

    /// L'arbre d'explication est celui de la sous-requete : c'est ce que rend
    /// ES, dont l'arbre ne porte aucune trace du `post_filter`. La route
    /// `_explain` et `explain: true` passent d'ailleurs par la requete nue,
    /// jamais par celle-ci — mais un `Weight` doit savoir repondre.
    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        self.sous.explain(reader, doc)
    }
}

struct FiltreScorer {
    sous: Box<dyn Scorer>,
    filtre: Box<dyn Scorer>,
}

impl FiltreScorer {
    /// Avance les deux curseurs jusqu'a ce qu'ils designent le meme document.
    ///
    /// Le contrat de `DocSet::seek` exige `target >= doc()` : on ne fait donc
    /// jamais reculer un curseur, on avance toujours celui qui est en retard.
    fn aligner(&mut self) -> DocId {
        loop {
            let a = self.sous.doc();
            if a == TERMINATED {
                return TERMINATED;
            }
            let b = self.filtre.doc();
            if b == TERMINATED {
                return self.epuiser();
            }
            match a.cmp(&b) {
                Ordering::Equal => return a,
                Ordering::Less => {
                    if self.sous.seek(b) == TERMINATED {
                        return TERMINATED;
                    }
                }
                Ordering::Greater => {
                    if self.filtre.seek(a) == TERMINATED {
                        return self.epuiser();
                    }
                }
            }
        }
    }

    /// Le filtre n'a plus rien : la requete non plus. Pousser le curseur au
    /// bout plutot que de mentir sur `doc()` — un collecteur lit `doc()`, pas
    /// la valeur qu'`advance` a rendue.
    fn epuiser(&mut self) -> DocId {
        if self.sous.doc() != TERMINATED {
            self.sous.seek(TERMINATED);
        }
        TERMINATED
    }
}

impl DocSet for FiltreScorer {
    fn advance(&mut self) -> DocId {
        if self.sous.advance() == TERMINATED {
            return TERMINATED;
        }
        self.aligner()
    }

    fn seek(&mut self, target: DocId) -> DocId {
        if self.sous.seek(target) == TERMINATED {
            return TERMINATED;
        }
        self.aligner()
    }

    fn doc(&self) -> DocId {
        self.sous.doc()
    }

    /// L'intersection ne peut pas rendre plus que le plus petit des deux.
    fn size_hint(&self) -> u32 {
        self.sous.size_hint().min(self.filtre.size_hint())
    }
}

impl Scorer for FiltreScorer {
    /// **Le** point du module : le score est celui de la sous-requete, au bit
    /// pres. Une intersection booleenne y ajouterait celui du filtre.
    fn score(&mut self) -> Score {
        self.sous.score()
    }
}
