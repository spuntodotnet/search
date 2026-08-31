//! `function_score` et `boosting` : le reglage de la pertinence.
//!
//! # Pourquoi ce module existe
//!
//! Les autres clauses du DSL rendent un **ensemble** de documents, et parfois un
//! **ordre**. Celle-ci rend une **valeur** : le `_score` lui-meme est ce que le
//! client lit, compare et affiche. Une formule recopiee depuis la documentation
//! d'Elastic rend un nombre plausible, et un nombre plausible ne se distingue
//! pas d'un nombre juste par la lecture.
//!
//! Les trois briques ou tout se joue — la decroissance
//! ([`Decroissance`]), le modificateur de `field_value_factor`
//! ([`Modificateur`]) et la combinaison des scores ([`Combinaison`]) — sont donc
//! verrouillees par [`tests/scoring_vs_es.rs`](../tests/scoring_vs_es.rs), qui
//! rejoue 47 000 points **calcules par Elasticsearch lui-meme** : les classes
//! `GaussDecayFunctionBuilder$GaussScoreFunction`,
//! `FieldValueFactorFunction$Modifier` et `CombineFunction`, executees dans le
//! conteneur de reference avec ses jars au classpath
//! (`tests/compat/genere_scoring.py`). C'est le geste de la carte 13 applique a
//! une autre classe, et il evite d'avoir a choisir une tolerance : il n'y a pas
//! d'ecart a tolerer, il y a un `f64` a rendre a l'identique.
//!
//! # Comment
//!
//! Le parcours des documents est **exactement** celui de la sous-requete : on
//! delegue `advance`, `seek` et `doc` (le seul cas ou ce n'est pas vrai est
//! `min_score`, qui saute les documents trop faibles — c'est ce que fait ES).
//! Seul le score est recalcule. Les filtres des `functions[]` sont des `DocSet`
//! qu'on repositionne en avant sur le document courant, comme
//! [`crate::dismax`].
//!
//! # Les incidents
//!
//! Deux situations qu'ES traite en **erreur** ne se decouvrent qu'a l'execution :
//! un document sans valeur pour le champ d'un `field_value_factor` sans
//! `missing`, et un score de fonction negatif. `Scorer::score` ne peut pas
//! echouer ; l'incident est donc pose dans [`Incidents`], partage par la
//! requete, et relu apres la recherche (voir [`crate::search`]). Rendre 0 ou
//! ignorer le document serait un resultat faux presente comme complet.

use std::sync::{Arc, OnceLock};

use axum::http::StatusCode;
use serde_json::json;
use tantivy::columnar::Column;
use tantivy::query::{EnableScoring, Explanation, Query, QueryClone, Scorer, Weight};
use tantivy::{DocId, DocSet, Score, SegmentReader, Term, TERMINATED};

use crate::error::EsError;

// ---------------------------------------------------------------------------
// Les deux minima qui ne sont pas les memes

/// `Math.min` de Java, qui n'est pas `f64::min` de Rust.
///
/// La difference tient a `NaN`, et elle decide de tout ici : Java le
/// **propage**, Rust rend l'autre operande. Un score de fonction `NaN` — un
/// `sqrt` sur une valeur negative, un `log1p` sur moins de -1 — doit donc
/// traverser `min(fonction, max_boost)` intact, pour que le garde-fou final le
/// voie et refuse la requete comme ES le fait. Avec le `min` de Rust il
/// disparaissait dans le plafond, et ferrite rendait **200 avec un score
/// invente** la ou ES rend 500. Trouve par une plage de controle du fuzzer
/// (graines 7300048 et 7300100), pas par la grille : elle ne portait ni `NaN`
/// ni les infinis.
fn min_java(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.min(b)
    }
}

/// `Math.max` de Java — meme histoire que [`min_java`].
fn max_java(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.max(b)
    }
}

// ---------------------------------------------------------------------------
// Les trois briques pures, mesurees contre les classes d'ES

/// Les trois fonctions de decroissance de `function_score`.
///
/// Leurs deux moitiés sont celles d'ES : `processScale` transforme le couple
/// (`scale`, `decay`) en le parametre que la formule utilise, et `evaluate`
/// applique la formule a une **distance** (`max(0, |valeur - origin| - offset)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decroissance {
    Gauss,
    Exp,
    Lineaire,
}

impl Decroissance {
    pub fn lit(nom: &str) -> Option<Self> {
        match nom {
            "gauss" => Some(Self::Gauss),
            "exp" => Some(Self::Exp),
            "linear" => Some(Self::Lineaire),
            _ => None,
        }
    }

    pub fn nom(self) -> &'static str {
        match self {
            Self::Gauss => "gauss",
            Self::Exp => "exp",
            Self::Lineaire => "linear",
        }
    }

    /// `processScale` : ce que le couple (`scale`, `decay`) devient.
    ///
    /// `Math.pow(x, 2.0)` de Java est exactement `x * x` (le `pow` de fdlibm
    /// court-circuite l'exposant 2), d'ou la multiplication ici.
    pub fn echelle(self, echelle: f64, decay: f64) -> f64 {
        match self {
            Self::Gauss => 0.5 * (echelle * echelle) / decay.ln(),
            Self::Exp => decay.ln() / echelle,
            Self::Lineaire => echelle / (1.0 - decay),
        }
    }

    /// `evaluate` : la formule, appliquee a une distance deja calculee.
    pub fn evalue(self, distance: f64, echelle: f64) -> f64 {
        match self {
            Self::Gauss => (0.5 * (distance * distance) / echelle).exp(),
            Self::Exp => (echelle * distance).exp(),
            Self::Lineaire => max_java(0.0, (echelle - distance) / echelle),
        }
    }
}

/// Le `modifier` de `field_value_factor`.
///
/// Trois pieges que seule la mesure donne : `ln1p` est `log1p` et non
/// `ln(1 + x)` (ils divergent sous 1e-16), `ln2p` est `log1p(1 + x)`, et
/// `log1p` / `log2p` sont en base **10**.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Modificateur {
    #[default]
    Aucun,
    Log,
    Log1p,
    Log2p,
    Ln,
    Ln1p,
    Ln2p,
    Carre,
    Racine,
    Inverse,
}

impl Modificateur {
    pub fn lit(nom: &str) -> Option<Self> {
        // ES lit le nom par `Modifier.fromString`, qui met en minuscules.
        match nom.to_ascii_lowercase().as_str() {
            "none" => Some(Self::Aucun),
            "log" => Some(Self::Log),
            "log1p" => Some(Self::Log1p),
            "log2p" => Some(Self::Log2p),
            "ln" => Some(Self::Ln),
            "ln1p" => Some(Self::Ln1p),
            "ln2p" => Some(Self::Ln2p),
            "square" => Some(Self::Carre),
            "sqrt" => Some(Self::Racine),
            "reciprocal" => Some(Self::Inverse),
            _ => None,
        }
    }

    pub fn nom(self) -> &'static str {
        match self {
            Self::Aucun => "none",
            Self::Log => "log",
            Self::Log1p => "log1p",
            Self::Log2p => "log2p",
            Self::Ln => "ln",
            Self::Ln1p => "ln1p",
            Self::Ln2p => "ln2p",
            Self::Carre => "square",
            Self::Racine => "sqrt",
            Self::Inverse => "reciprocal",
        }
    }

    pub fn applique(self, n: f64) -> f64 {
        match self {
            Self::Aucun => n,
            Self::Log => n.log10(),
            Self::Log1p => (n + 1.0).log10(),
            Self::Log2p => (n + 2.0).log10(),
            Self::Ln => n.ln(),
            Self::Ln1p => n.ln_1p(),
            Self::Ln2p => (n + 1.0).ln_1p(),
            Self::Carre => n * n,
            Self::Racine => n.sqrt(),
            Self::Inverse => 1.0 / n,
        }
    }
}

/// `boost_mode` : comment le score des fonctions rejoint celui de la requete.
///
/// C'est le seul endroit de la chaine ou ES quitte le `double` — le plafond
/// `max_boost` s'applique **ici**, au score des fonctions et pas au resultat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Combinaison {
    #[default]
    Multiplie,
    Remplace,
    Somme,
    Moyenne,
    Min,
    Max,
}

impl Combinaison {
    pub fn lit(nom: &str) -> Option<Self> {
        match nom.to_ascii_lowercase().as_str() {
            "multiply" => Some(Self::Multiplie),
            "replace" => Some(Self::Remplace),
            "sum" => Some(Self::Somme),
            "avg" => Some(Self::Moyenne),
            "min" => Some(Self::Min),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    pub fn combine(self, score: f64, fonction: f64, plafond: f64) -> f32 {
        let f = min_java(fonction, plafond);
        let r = match self {
            Self::Multiplie => score * f,
            Self::Remplace => f,
            Self::Somme => score + f,
            Self::Moyenne => (f + score) / 2.0,
            Self::Min => min_java(score, f),
            Self::Max => max_java(score, f),
        };
        r as f32
    }
}

/// `score_mode` : comment les fonctions d'un `functions[]` se combinent entre
/// elles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ModeDeScore {
    #[default]
    Multiplie,
    Premiere,
    Moyenne,
    Max,
    Min,
    Somme,
}

impl ModeDeScore {
    pub fn lit(nom: &str) -> Option<Self> {
        match nom.to_ascii_lowercase().as_str() {
            "multiply" => Some(Self::Multiplie),
            "first" => Some(Self::Premiere),
            "avg" => Some(Self::Moyenne),
            "max" => Some(Self::Max),
            "min" => Some(Self::Min),
            "sum" => Some(Self::Somme),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Les incidents : ce qu'ES traite en erreur et qui ne se voit qu'a l'execution

/// Ce qu'une recherche a rencontre et qu'ES rend en erreur.
///
/// `Scorer::score` ne peut pas echouer : l'incident est pose ici, et relu apres
/// la recherche. Il est **enveloppe des maintenant** dans le « all shards
/// failed » d'ES, la seule forme qu'il donne a un echec de shard — et c'est
/// aussi la raison pour laquelle il porte le nom de l'index : la mise en forme
/// a besoin d'informations que le scorer n'a pas.
pub struct Incidents {
    faute: OnceLock<(StatusCode, String, String)>,
    index: String,
    uuid: String,
    node: String,
}

impl Incidents {
    pub fn pour(index: &str, uuid: &str, node: &str) -> Self {
        Self {
            faute: OnceLock::new(),
            index: index.to_string(),
            uuid: uuid.to_string(),
            node: node.to_string(),
        }
    }

    /// Les incidents d'un contexte qui ne sert pas a chercher (validation,
    /// agregations d'un index vide) : ils ne seront jamais relus.
    pub fn anonymes() -> Self {
        Self::pour("", "", "")
    }

    fn signale(&self, status: StatusCode, ty: &str, reason: String) {
        // `OnceLock` : le premier incident gagne, comme le premier shard qui
        // echoue chez ES. Les suivants sont le meme defaut vu ailleurs.
        let _ = self.faute.set((status, ty.to_string(), reason));
    }

    /// L'erreur a rendre, s'il y en a une.
    pub fn erreur(&self) -> Option<EsError> {
        let (status, ty, reason) = self.faute.get()?;
        let cause = json!({
            "type": ty,
            "reason": reason,
            "index_uuid": self.uuid,
            "index": self.index,
        });
        Some(
            EsError::new(
                *status,
                "search_phase_execution_exception",
                "all shards failed",
            )
            .with("phase", json!("query"))
            .with("grouped", json!(true))
            .with(
                "failed_shards",
                json!([{"shard": 0, "index": self.index, "node": self.node, "reason": cause}]),
            )
            .avec_racines(vec![json!({"type": ty, "reason": reason})]),
        )
    }
}

impl Default for Incidents {
    fn default() -> Self {
        Self::anonymes()
    }
}

// ---------------------------------------------------------------------------
// La description d'une fonction, resolue a la traduction

/// De quel genre de colonne une fonction lit ses valeurs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenreNumerique {
    I64,
    F64,
    Date,
    Bool,
}

/// `field_value_factor` : le score est une valeur du document.
#[derive(Clone, Debug)]
pub struct ValeurDeChamp {
    pub champ: String,
    pub genre: Option<GenreNumerique>,
    pub facteur: f64,
    pub modificateur: Modificateur,
    pub manquant: Option<f64>,
}

/// `gauss` / `exp` / `linear` : le score decroit avec la distance a `origin`.
#[derive(Clone, Debug)]
pub struct Attenuation {
    pub champ: String,
    pub genre: Option<GenreNumerique>,
    pub fonction: Decroissance,
    pub origine: f64,
    /// Deja passe par [`Decroissance::echelle`] : c'est le parametre de la
    /// formule, pas le `scale` de la requete.
    pub echelle: f64,
    pub offset: f64,
}

#[derive(Clone, Debug)]
pub enum Calcul {
    /// `weight` seul : le poids **est** le score de la fonction.
    Poids,
    Valeur(ValeurDeChamp),
    Decroit(Attenuation),
}

pub struct Fonction {
    pub filtre: Option<Box<dyn Query>>,
    pub poids: Option<f64>,
    pub calcul: Calcul,
}

impl Clone for Fonction {
    fn clone(&self) -> Self {
        Self {
            filtre: self.filtre.as_ref().map(|q| q.box_clone()),
            poids: self.poids,
            calcul: self.calcul.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// La requete

pub struct FonctionScore {
    sous: Box<dyn Query>,
    fonctions: Vec<Fonction>,
    mode: ModeDeScore,
    combinaison: Combinaison,
    /// `max_boost` : le defaut d'ES est `Float.MAX_VALUE`.
    plafond: f64,
    minimum: Option<f32>,
    /// Le `boost` de la clause.
    ///
    /// Il est porte ici et non par un `BoostQuery` autour, pour une raison qui
    /// ne se voit que sur le **total** : `min_score` compare le score
    /// **boosté** (mesure), et le `Weight::count` de tantivy reconstruit le
    /// scorer avec un boost de 1.0. Un `boost` laisse a l'exterieur ferait donc
    /// compter des documents que la page ne rend pas — 5 au lieu de 6, en 200.
    boost_clause: f32,
    incidents: Arc<Incidents>,
}

impl FonctionScore {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sous: Box<dyn Query>,
        fonctions: Vec<Fonction>,
        mode: ModeDeScore,
        combinaison: Combinaison,
        plafond: f64,
        minimum: Option<f32>,
        boost_clause: f32,
        incidents: Arc<Incidents>,
    ) -> Self {
        Self {
            sous,
            fonctions,
            mode,
            combinaison,
            plafond,
            minimum,
            boost_clause,
            incidents,
        }
    }
}

impl std::fmt::Debug for FonctionScore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "FonctionScore({:?}, {} fonction(s))",
            self.sous,
            self.fonctions.len()
        )
    }
}

impl Clone for FonctionScore {
    fn clone(&self) -> Self {
        Self {
            sous: self.sous.box_clone(),
            fonctions: self.fonctions.clone(),
            mode: self.mode,
            combinaison: self.combinaison,
            plafond: self.plafond,
            minimum: self.minimum,
            boost_clause: self.boost_clause,
            incidents: self.incidents.clone(),
        }
    }
}

impl Query for FonctionScore {
    fn weight(&self, enable_scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        // `min_score` **filtre** des documents : sans score, le total serait
        // faux. On rallume donc le calcul pour la sous-requete quand le
        // collecteur ne le demande pas (un `Count`, un tri par champ).
        let scoring = match (self.minimum, enable_scoring.searcher()) {
            (Some(_), Some(searcher)) => EnableScoring::enabled_from_searcher(searcher),
            _ => enable_scoring,
        };
        Ok(Box::new(FonctionScoreWeight {
            sous: self.sous.weight(scoring)?,
            fonctions: self
                .fonctions
                .iter()
                .map(|f| {
                    Ok(FonctionPesee {
                        filtre: match &f.filtre {
                            // Un filtre ne sert qu'a dire « ce document
                            // compte » : il n'a pas besoin d'un score.
                            Some(q) => Some(q.weight(EnableScoring::Disabled {
                                schema: scoring.schema(),
                                searcher_opt: scoring.searcher(),
                            })?),
                            None => None,
                        },
                        poids: f.poids,
                        calcul: f.calcul.clone(),
                    })
                })
                .collect::<tantivy::Result<Vec<_>>>()?,
            mode: self.mode,
            combinaison: self.combinaison,
            plafond: self.plafond,
            minimum: self.minimum,
            // Le `boost` d'une clause ne s'applique **que si le collecteur
            // demande des scores** : Lucene comme tantivy laissent tomber leur
            // `BoostQuery` quand personne ne lit le score. Ca ne se voit nulle
            // part ailleurs (un facteur constant ne change pas un ensemble de
            // documents) — sauf ici, ou `min_score` en fait un seuil. Mesure
            // contre ES 8.15 : `min_score: 0.25` avec `boost: 2` garde trois
            // documents en recherche libre et **un seul** des qu'un `sort`
            // remplace le score. Trouve par une plage de controle du fuzzer
            // (graine 8810020).
            boost_clause: if enable_scoring.is_scoring_enabled() {
                self.boost_clause
            } else {
                1.0
            },
            incidents: self.incidents.clone(),
        }))
    }

    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        self.sous.query_terms(visitor);
        for f in &self.fonctions {
            if let Some(q) = &f.filtre {
                q.query_terms(visitor);
            }
        }
    }
}

struct FonctionPesee {
    filtre: Option<Box<dyn Weight>>,
    poids: Option<f64>,
    calcul: Calcul,
}

struct FonctionScoreWeight {
    sous: Box<dyn Weight>,
    fonctions: Vec<FonctionPesee>,
    mode: ModeDeScore,
    combinaison: Combinaison,
    plafond: f64,
    minimum: Option<f32>,
    boost_clause: f32,
    incidents: Arc<Incidents>,
}

impl Weight for FonctionScoreWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let mut fonctions = Vec::with_capacity(self.fonctions.len());
        for f in &self.fonctions {
            fonctions.push(FonctionSegment {
                filtre: match &f.filtre {
                    Some(w) => Some(w.scorer(reader, 1.0)?),
                    None => None,
                },
                poids: f.poids,
                lecture: Lecture::ouvrir(&f.calcul, reader)?,
            });
        }
        let mut scorer = FonctionScoreScorer {
            // Le parcours vient de la sous-requete ; on ne fait que le
            // suivre. Elle est scoree a boost 1.0 : ce que la clause combine
            // est le score de la requete **seule**, et le boost s'applique
            // tout a la fin (voir `score_final`).
            sous: self.sous.scorer(reader, 1.0)?,
            fonctions,
            mode: self.mode,
            combinaison: self.combinaison,
            plafond: self.plafond,
            minimum: self.minimum,
            boost: boost * self.boost_clause,
            incidents: self.incidents.clone(),
            cache: None,
        };
        // `min_score` peut ecarter le premier document : le curseur doit deja
        // etre pose sur un document retenu quand le collecteur le lit.
        if scorer.minimum.is_some() && scorer.doc() != TERMINATED && !scorer.retenu() {
            scorer.avancer_jusqu_a_retenu();
        }
        Ok(Box::new(scorer))
    }

    /// L'arbre du score : la valeur de la clause, et **sous elle** celle de la
    /// requete a laquelle les fonctions s'appliquent.
    ///
    /// C'est cette seconde valeur qui compte pour un outil de mise au point :
    /// sans elle, un `function_score` est un nombre opaque, et on ne peut pas
    /// dire si un ecart vient de la fonction ou du BM25 qu'elle multiplie.
    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut scorer = self.scorer(reader, 1.0)?;
        if scorer.seek(doc) != doc {
            return Err(tantivy::TantivyError::InvalidArgument(format!(
                "document {doc} ne correspond pas a la requete"
            )));
        }
        let mut explication = Explanation::new("function score", scorer.score());
        if let Ok(sous) = self.sous.explain(reader, doc) {
            explication.add_detail(sous);
        }
        Ok(explication)
    }
}

/// La colonne d'ou une fonction lit ses valeurs.
enum Colonne {
    I64(Column<i64>),
    F64(Column<f64>),
    Date(Column<tantivy::DateTime>),
    Bool(Column<bool>),
    /// Le champ n'est pas mappe : aucun document n'a de valeur (ES ne s'en
    /// plaint pas non plus — mesure).
    Aucune,
}

impl Colonne {
    fn ouvrir(
        champ: &str,
        genre: Option<GenreNumerique>,
        reader: &SegmentReader,
    ) -> tantivy::Result<Self> {
        let ff = reader.fast_fields();
        Ok(match genre {
            None => Self::Aucune,
            Some(GenreNumerique::I64) => Self::I64(ff.i64(champ)?),
            Some(GenreNumerique::F64) => Self::F64(ff.f64(champ)?),
            Some(GenreNumerique::Date) => Self::Date(ff.date(champ)?),
            Some(GenreNumerique::Bool) => Self::Bool(ff.bool(champ)?),
        })
    }

    /// La **plus petite** valeur du document, celle que lit ES : sa colonne
    /// numerique est triee (`SortedNumericDoubleValues`) et il prend la
    /// premiere.
    fn minimum(&self, doc: DocId) -> Option<f64> {
        match self {
            Self::Aucune => None,
            Self::I64(c) => c.values_for_doc(doc).map(|v| v as f64).reduce(min_java),
            Self::F64(c) => c.values_for_doc(doc).reduce(min_java),
            Self::Date(c) => c
                .values_for_doc(doc)
                .map(|d| d.into_timestamp_millis() as f64)
                .reduce(min_java),
            Self::Bool(c) => c
                .values_for_doc(doc)
                .map(|b| if b { 1.0 } else { 0.0 })
                .reduce(min_java),
        }
    }

    /// La plus petite **distance** du document a l'origine : c'est le
    /// `multi_value_mode` par defaut d'ES (`min`), et il porte sur la distance,
    /// pas sur la valeur.
    fn distance(&self, doc: DocId, origine: f64, offset: f64) -> Option<f64> {
        let d = |v: f64| max_java(0.0, (v - origine).abs() - offset);
        match self {
            Self::Aucune => None,
            Self::I64(c) => c.values_for_doc(doc).map(|v| d(v as f64)).reduce(min_java),
            Self::F64(c) => c.values_for_doc(doc).map(d).reduce(min_java),
            Self::Date(c) => c
                .values_for_doc(doc)
                .map(|x| d(x.into_timestamp_millis() as f64))
                .reduce(min_java),
            Self::Bool(c) => c
                .values_for_doc(doc)
                .map(|b| d(if b { 1.0 } else { 0.0 }))
                .reduce(min_java),
        }
    }
}

/// Ce qu'une fonction lit dans un segment.
enum Lecture {
    Poids,
    Valeur {
        colonne: Colonne,
        spec: ValeurDeChamp,
    },
    Decroit {
        colonne: Colonne,
        spec: Attenuation,
    },
}

impl Lecture {
    fn ouvrir(calcul: &Calcul, reader: &SegmentReader) -> tantivy::Result<Self> {
        Ok(match calcul {
            Calcul::Poids => Self::Poids,
            Calcul::Valeur(spec) => Self::Valeur {
                colonne: Colonne::ouvrir(&spec.champ, spec.genre, reader)?,
                spec: spec.clone(),
            },
            Calcul::Decroit(spec) => Self::Decroit {
                colonne: Colonne::ouvrir(&spec.champ, spec.genre, reader)?,
                spec: spec.clone(),
            },
        })
    }
}

struct FonctionSegment {
    filtre: Option<Box<dyn Scorer>>,
    poids: Option<f64>,
    lecture: Lecture,
}

impl FonctionSegment {
    /// Cette fonction s'applique-t-elle a ce document ? (`filter`)
    fn concerne(&mut self, doc: DocId) -> bool {
        match &mut self.filtre {
            None => true,
            Some(f) => {
                // Le parcours de la sous-requete est monotone : un `seek` en
                // avant est toujours licite.
                if f.doc() < doc {
                    f.seek(doc);
                }
                f.doc() == doc
            }
        }
    }

    /// Le score de la fonction pour ce document, ou l'incident qu'ES leve.
    fn score(&self, doc: DocId, incidents: &Incidents) -> f64 {
        let brut = match &self.lecture {
            Lecture::Poids => 1.0,
            Lecture::Valeur { colonne, spec } => {
                let valeur = match colonne.minimum(doc).or(spec.manquant) {
                    Some(v) => v,
                    None => {
                        incidents.signale(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "exception",
                            format!("Missing value for field [{}]", spec.champ),
                        );
                        return 1.0;
                    }
                };
                let r = spec.modificateur.applique(valeur * spec.facteur);
                if r < 0.0 {
                    incidents.signale(
                        StatusCode::BAD_REQUEST,
                        "illegal_argument_exception",
                        format!(
                            "field value function must not produce negative scores, but got: \
                             [{}] for field value: [{}]{}",
                            comme_java(r),
                            comme_java(valeur),
                            if matches!(spec.modificateur, Modificateur::Log | Modificateur::Ln) {
                                "; consider using log1p or log2p instead of log to avoid negative \
                                 scores"
                            } else {
                                ""
                            }
                        ),
                    );
                    return 1.0;
                }
                r
            }
            // Un document sans valeur a une distance **nulle**, donc un score
            // de 1.0 : ES remplace la distance manquante par 0
            // (`FieldData.replaceMissing(…, 0)`), il ne saute pas le document.
            Lecture::Decroit { colonne, spec } => spec.fonction.evalue(
                colonne
                    .distance(doc, spec.origine, spec.offset)
                    .unwrap_or(0.0),
                spec.echelle,
            ),
        };
        match self.poids {
            Some(p) => brut * p,
            None => brut,
        }
    }
}

struct FonctionScoreScorer {
    sous: Box<dyn Scorer>,
    fonctions: Vec<FonctionSegment>,
    mode: ModeDeScore,
    combinaison: Combinaison,
    plafond: f64,
    minimum: Option<f32>,
    boost: Score,
    incidents: Arc<Incidents>,
    /// Le score du document courant, calcule une seule fois : `min_score` le
    /// demande a l'avance, et le collecteur le redemande ensuite.
    cache: Option<(DocId, Score)>,
}

impl FonctionScoreScorer {
    /// Le score final du document courant, `boost` de la clause **compris**.
    ///
    /// C'est bien celui-la que `min_score` compare, et ce n'est pas ce qu'on
    /// croirait : chez ES le `boost` est un `BoostQuery` qui **enveloppe** la
    /// clause, donc on l'attendrait par-dessus le filtrage. Lucene le fait
    /// descendre dans `createWeight`, et `FunctionScoreQuery` l'applique
    /// **dans** son scorer, que `MinScoreScorer` enveloppe ensuite. Mesure
    /// contre ES 8.15 : `min_score: 3` avec `boost: 10` ne coupe rien la ou le
    /// meme `min_score` sans `boost` coupe tout.
    fn score_final(&mut self) -> Score {
        let doc = self.sous.doc();
        if let Some((d, s)) = self.cache {
            if d == doc {
                return s;
            }
        }
        let sous_score = f64::from(self.sous.score());
        let mut facteur = 1.0f64;
        match self.mode {
            ModeDeScore::Premiere => {
                for f in &mut self.fonctions {
                    if f.concerne(doc) {
                        facteur = f.score(doc, &self.incidents);
                        break;
                    }
                }
            }
            ModeDeScore::Max => {
                let mut max = f64::NEG_INFINITY;
                for f in &mut self.fonctions {
                    if f.concerne(doc) {
                        max = max_java(f.score(doc, &self.incidents), max);
                    }
                }
                if max != f64::NEG_INFINITY {
                    facteur = max;
                }
            }
            ModeDeScore::Min => {
                let mut min = f64::INFINITY;
                for f in &mut self.fonctions {
                    if f.concerne(doc) {
                        min = min_java(f.score(doc, &self.incidents), min);
                    }
                }
                if min != f64::INFINITY {
                    facteur = min;
                }
            }
            ModeDeScore::Multiplie => {
                for f in &mut self.fonctions {
                    if f.concerne(doc) {
                        facteur *= f.score(doc, &self.incidents);
                    }
                }
            }
            // `sum` et `avg` partagent leur boucle chez ES, et `avg` divise par
            // la **somme des poids** — pas par le nombre de fonctions.
            ModeDeScore::Somme | ModeDeScore::Moyenne => {
                let (mut total, mut poids) = (0.0f64, 0.0f64);
                for f in &mut self.fonctions {
                    if f.concerne(doc) {
                        total += f.score(doc, &self.incidents);
                        poids += f.poids.unwrap_or(1.0);
                    }
                }
                if poids != 0.0 {
                    facteur = if matches!(self.mode, ModeDeScore::Moyenne) {
                        total / poids
                    } else {
                        total
                    };
                }
            }
        }
        let score = self.combinaison.combine(sous_score, facteur, self.plafond) * self.boost;
        if score < 0.0 || score.is_nan() {
            self.incidents.signale(
                StatusCode::INTERNAL_SERVER_ERROR,
                "exception",
                format!(
                    "function score query returned an invalid score: {} for doc: {doc}",
                    comme_java(f64::from(score))
                ),
            );
        }
        self.cache = Some((doc, score));
        score
    }

    fn retenu(&mut self) -> bool {
        match self.minimum {
            None => true,
            Some(m) => self.score_final() >= m,
        }
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

impl DocSet for FonctionScoreScorer {
    fn advance(&mut self) -> DocId {
        if self.minimum.is_none() {
            return self.sous.advance();
        }
        self.avancer_jusqu_a_retenu()
    }

    fn seek(&mut self, target: DocId) -> DocId {
        let doc = self.sous.seek(target);
        if self.minimum.is_none() || doc == TERMINATED || self.retenu() {
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

impl Scorer for FonctionScoreScorer {
    fn score(&mut self) -> Score {
        if self.sous.doc() == TERMINATED {
            return 0.0;
        }
        self.score_final()
    }
}

// ---------------------------------------------------------------------------
// `boosting` : la demotion sans exclusion

/// `boosting` : les documents de `positive`, ceux qui matchent aussi `negative`
/// voyant leur score multiplie par `negative_boost`.
///
/// L'ensemble rendu est **exactement** celui de `positive` : `negative` ne
/// retire rien, il repousse.
pub struct Retrograde {
    positive: Box<dyn Query>,
    negative: Box<dyn Query>,
    poids: f32,
}

impl Retrograde {
    pub fn new(positive: Box<dyn Query>, negative: Box<dyn Query>, poids: f32) -> Self {
        Self {
            positive,
            negative,
            poids,
        }
    }
}

impl std::fmt::Debug for Retrograde {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Retrograde({:?}, {:?}, {})",
            self.positive, self.negative, self.poids
        )
    }
}

impl Clone for Retrograde {
    fn clone(&self) -> Self {
        Self {
            positive: self.positive.box_clone(),
            negative: self.negative.box_clone(),
            poids: self.poids,
        }
    }
}

impl Query for Retrograde {
    fn weight(&self, enable_scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(RetrogradeWeight {
            positive: self.positive.weight(enable_scoring)?,
            negative: self.negative.weight(EnableScoring::Disabled {
                schema: enable_scoring.schema(),
                searcher_opt: enable_scoring.searcher(),
            })?,
            poids: self.poids,
        }))
    }

    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        self.positive.query_terms(visitor);
        self.negative.query_terms(visitor);
    }
}

struct RetrogradeWeight {
    positive: Box<dyn Weight>,
    negative: Box<dyn Weight>,
    poids: f32,
}

impl Weight for RetrogradeWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        Ok(Box::new(RetrogradeScorer {
            positive: self.positive.scorer(reader, boost)?,
            negative: self.negative.scorer(reader, 1.0)?,
            poids: self.poids,
        }))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut scorer = self.scorer(reader, 1.0)?;
        if scorer.seek(doc) != doc {
            return Err(tantivy::TantivyError::InvalidArgument(format!(
                "document {doc} ne correspond pas a la requete"
            )));
        }
        let mut explication = Explanation::new("boosting", scorer.score());
        if let Ok(sous) = self.positive.explain(reader, doc) {
            explication.add_detail(sous);
        }
        // La clause negative n'apparait que si elle a joue : c'est ce qu'on
        // vient lire, et le document qui n'y correspond pas n'a rien perdu.
        if let Ok(sous) = self.negative.explain(reader, doc) {
            explication.add_detail(sous);
        }
        Ok(explication)
    }

    fn count(&self, reader: &SegmentReader) -> tantivy::Result<u32> {
        self.positive.count(reader)
    }
}

struct RetrogradeScorer {
    positive: Box<dyn Scorer>,
    negative: Box<dyn Scorer>,
    poids: f32,
}

impl DocSet for RetrogradeScorer {
    fn advance(&mut self) -> DocId {
        self.positive.advance()
    }

    fn seek(&mut self, target: DocId) -> DocId {
        self.positive.seek(target)
    }

    fn doc(&self) -> DocId {
        self.positive.doc()
    }

    fn size_hint(&self) -> u32 {
        self.positive.size_hint()
    }
}

impl Scorer for RetrogradeScorer {
    fn score(&mut self) -> Score {
        let doc = self.positive.doc();
        if doc == TERMINATED {
            return 0.0;
        }
        let score = self.positive.score();
        if self.negative.doc() < doc {
            self.negative.seek(doc);
        }
        if self.negative.doc() == doc {
            (f64::from(score) * f64::from(self.poids)) as f32
        } else {
            score
        }
    }
}

// ---------------------------------------------------------------------------

/// Un `double` ecrit comme Java l'ecrit.
///
/// Les messages d'erreur d'ES citent la valeur fautive, et ils la citent avec
/// `Double.toString` : `-Infinity`, `0.0`, `-5.0`. Le `{}` de Rust rendrait
/// `-inf`, `0` et `-5` — trois messages qui ne sont plus celui d'ES.
fn comme_java(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    let s = format!("{x}");
    if s.contains(['.', 'e', 'E']) {
        s
    } else {
        format!("{s}.0")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_nombres_s_ecrivent_comme_en_java() {
        assert_eq!(comme_java(0.0), "0.0");
        assert_eq!(comme_java(-5.0), "-5.0");
        assert_eq!(comme_java(f64::NEG_INFINITY), "-Infinity");
        assert_eq!(comme_java(1.5), "1.5");
    }
}
