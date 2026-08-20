//! `_delete_by_query` et `_update_by_query` : modifier ou purger **par
//! requete**.
//!
//! Le geste est celui d'ES, et il tient en trois temps : chercher sur
//! l'instantane du moment, relever pour chaque correspondance son `_seq_no`,
//! puis ecrire chaque document **a condition qu'il n'ait pas bouge depuis**.
//! C'est ce dernier point qui produit les `version_conflicts` : ils ne sont pas
//! un detail de comptage, ils sont ce qui empeche une purge de supprimer une
//! version qu'elle n'a jamais vue.
//!
//! `_update_by_query` sans script **reindexe depuis le `_source`**. ferrite
//! sait deja faire ce geste-la : c'est exactement ce que
//! [`crate::engine::FerriteIndex`] fait quand le mapping dynamique decouvre un
//! champ et qu'il reconstruit une generation entiere. La difference est qu'ici
//! chaque document est rejoue **un par un**, avec sa condition de concurrence et
//! son incrementation de `_version` — ce que la reconstruction de generation ne
//! fait pas, puisqu'elle rejoue un etat, pas une ecriture.
//!
//! Tout ce qui suppose Painless (`script`), une tache de fond (`slices`,
//! `wait_for_completion=false`) ou un debit regule (`requests_per_second`) est
//! refuse explicitement par la couche HTTP ([`crate::api::parrequete`]).

use std::sync::Arc;

use serde_json::{json, Value};
use tantivy::collector::{Collector, SegmentCollector};
use tantivy::query::Query;
use tantivy::schema::{TantivyDocument, Value as _};
use tantivy::{DocAddress, DocId, Score, SegmentOrdinal, SegmentReader};

use crate::engine::{FerriteIndex, Generation, WriteOptions};
use crate::error::{EsError, EsResult};

/// La taille de lot par defaut d'ES (`scroll_size`).
pub const LOT_PAR_DEFAUT: usize = 1000;
/// La taille de lot maximale d'ES (`index.max_result_window`).
pub const LOT_MAX: usize = crate::MAX_RESULT_WINDOW;

/// Ce qu'une commande par requete fait des documents qu'elle trouve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Geste {
    /// `_delete_by_query`.
    Supprimer,
    /// `_update_by_query` sans script : reindexer le document depuis son
    /// `_source`.
    Reindexer,
}

/// Un index vise, avec la requete **construite dans sa generation**.
///
/// Meme raison que pour une recherche : les `Field` d'une generation n'ont
/// aucun sens dans une autre (voir [`crate::search::Cible`]).
pub struct Cible {
    pub index: Arc<FerriteIndex>,
    pub gen: Arc<Generation>,
    pub query: Box<dyn Query>,
}

/// Ce que le client a demande, une fois les parametres lus.
#[derive(Debug, Clone, Copy)]
pub struct Demande {
    /// `max_docs` : au-dela, on arrete de traiter (et `total` s'arrete aussi).
    pub max_docs: Option<usize>,
    /// `scroll_size` : la taille d'un lot. Elle ne change pas le resultat, mais
    /// elle change `batches` — et, sous `conflicts=abort`, **ou** la commande
    /// s'arrete.
    pub taille_de_lot: usize,
    /// `conflicts=proceed` : un conflit se compte au lieu d'arreter.
    pub proceder_sur_conflit: bool,
}

impl Default for Demande {
    fn default() -> Self {
        Self {
            max_docs: None,
            taille_de_lot: LOT_PAR_DEFAUT,
            proceder_sur_conflit: false,
        }
    }
}

/// Les compteurs de la reponse d'ES.
#[derive(Debug, Default)]
pub struct Bilan {
    /// Le nombre de documents que la commande **avait** a traiter :
    /// `min(correspondants, max_docs)`. Mesure contre ES 8.15 : il ne diminue
    /// pas quand la commande s'interrompt sur un conflit.
    pub total: usize,
    pub updated: usize,
    pub deleted: usize,
    /// Le nombre de lots reellement traites.
    pub batches: usize,
    pub version_conflicts: usize,
    /// Toujours 0 sans script : ES ne compte un `noop` que quand un script dit
    /// `ctx.op = 'noop'`. Reindexer un document identique compte `updated`.
    pub noops: usize,
    pub failures: Vec<Value>,
}

/// Un document a traiter, releve sur l'instantane de la recherche.
struct Candidat {
    id: String,
    /// Le `_seq_no` **au moment de la recherche**.
    ///
    /// Il sert deux fois, et c'est heureux :
    ///
    /// * c'est la condition d'ecriture, donc la source des `version_conflicts` ;
    /// * c'est aussi l'**ordre de traitement**. ES balaie dans l'ordre `_doc`,
    ///   le numero interne de Lucene, qui vaut l'ordre d'ecriture sur un shard
    ///   qu'on ne fait qu'alimenter (une reecriture supprime et rajoute a la
    ///   fin, donc avance aussi). Le numero de document de tantivy, lui, **ne
    ///   suit pas l'ordre d'ecriture** — mesure : un `_bulk` de 25 documents
    ///   ressort en `d002, d000, d003, d001, …`. Trier dessus ferait supprimer
    ///   d'autres documents qu'ES sous un `max_docs`, sans rien dire. Le
    ///   `_seq_no`, lui, est attribue sous le verrou d'ecriture : il **est**
    ///   l'ordre d'ecriture. Trouve par `fuzz_vs_es.py` (graine 2727085), pas
    ///   par le raisonnement qui avait ecrit la premiere version.
    seq_no: u64,
    /// Le `_source` stocke, pour le rejeu de `_update_by_query`.
    source: Value,
}

/// Execute la commande sur tous les index vises.
///
/// L'ordre de traitement est celui du `scroll` d'ES trie par `_doc` : les
/// documents de tous les index vises sont **entrelaces par ordre d'ecriture**,
/// et c'est l'index qui departage a rang egal. Ca n'a l'air de rien tant qu'on
/// prend tout — mais avec `max_docs=3` sur deux index, ES supprime deux
/// documents du premier et **un du second**, la ou une simple concatenation en
/// prendrait trois au premier (mesure contre ES 8.15).
///
/// Les lots, eux, se comptent globalement : `scroll_size=3` sur deux index de
/// quatre documents fait trois lots, pas quatre. Le `scroll` d'ES ne connait pas
/// les frontieres d'index.
pub fn executer(cibles: &[Cible], geste: Geste, demande: &Demande) -> EsResult<Bilan> {
    let mut bilan = Bilan::default();

    // 1. Le releve : tout ce qui correspond, sur l'instantane de chaque index.
    let mut candidats: Vec<(usize, Candidat)> = Vec::new();
    for (rang, cible) in cibles.iter().enumerate() {
        for candidat in relever(cible, geste)? {
            candidats.push((rang, candidat));
        }
    }
    candidats.sort_by_key(|(rang, c)| (c.seq_no, *rang));
    candidats.truncate(demande.max_docs.unwrap_or(usize::MAX));
    bilan.total = candidats.len();

    // 2. L'ecriture, lot par lot. Un conflit n'arrete pas le lot en cours : ES
    //    envoie le `_bulk` du lot entier et ne s'arrete qu'apres (mesure : sur
    //    six documents en lots de deux, un conflit au cinquieme laisse le
    //    sixieme supprime).
    for lot in candidats.chunks(demande.taille_de_lot) {
        bilan.batches += 1;
        let avant = bilan.version_conflicts;
        for (rang, candidat) in lot {
            appliquer(&cibles[*rang], candidat, geste, demande, &mut bilan);
        }
        if !demande.proceder_sur_conflit && bilan.version_conflicts > avant {
            break;
        }
    }
    Ok(bilan)
}

/// Ecrit un document, et range le resultat dans le bilan.
///
/// Un conflit n'est pas une erreur de la requete : il est **compte**. Il n'entre
/// dans `failures[]` que sous `conflicts=abort` — avec `proceed`, le client a dit
/// qu'il s'y attendait, et ES rend alors 200 avec un `failures[]` vide et le
/// seul compteur `version_conflicts` (mesure contre ES 8.15).
fn appliquer(
    cible: &Cible,
    candidat: &Candidat,
    geste: Geste,
    demande: &Demande,
    bilan: &mut Bilan,
) {
    let opts = WriteOptions {
        require_absent: false,
        if_seq_no: Some(candidat.seq_no),
        if_primary_term: Some(1),
    };
    let issue = match geste {
        Geste::Supprimer => cible.index.delete_doc(&candidat.id, opts).map(|_| ()),
        Geste::Reindexer => cible
            .index
            .index_doc(&candidat.id, &candidat.source, opts)
            .map(|_| ()),
    };
    match issue {
        Ok(()) => match geste {
            Geste::Supprimer => bilan.deleted += 1,
            Geste::Reindexer => bilan.updated += 1,
        },
        Err(e) if e.status == axum::http::StatusCode::CONFLICT => {
            bilan.version_conflicts += 1;
            if !demande.proceder_sur_conflit {
                bilan
                    .failures
                    .push(echec(&cible.index.name, &candidat.id, &e));
            }
        }
        Err(e) => bilan
            .failures
            .push(echec(&cible.index.name, &candidat.id, &e)),
    }
}

/// Un element de `failures[]`, au format d'ES.
fn echec(index: &str, id: &str, e: &EsError) -> Value {
    json!({
        "index": index,
        "id": id,
        "cause": e.cause(),
        "status": e.status.as_u16(),
    })
}

/// Tout ce qui correspond dans **un** index.
///
/// L'ordre est decide par l'appelant, sur le `_seq_no` : celui des adresses
/// tantivy ne vaut rien ici (voir [`Candidat::seq_no`]).
fn relever(cible: &Cible, geste: Geste) -> EsResult<Vec<Candidat>> {
    let searcher = cible.gen.searcher();
    let adresses = searcher.search(&cible.query, &CollecteurDeDocs)?;

    let mut out = Vec::with_capacity(adresses.len());
    for (seg, doc) in adresses {
        let stocke: TantivyDocument = searcher.doc(DocAddress::new(seg, doc))?;
        let id = stocke
            .get_first(cible.gen.fields.id)
            .and_then(|v| v.as_str().map(str::to_string))
            .ok_or_else(|| EsError::internal("document sans _id stocke"))?;
        let seq_no = stocke
            .get_first(cible.gen.fields.seq_no)
            .and_then(|v| v.as_u64())
            .ok_or_else(|| EsError::internal("document sans _seq_no stocke"))?;
        // Le `_source` n'est lu que si on doit le reecrire : une purge n'a
        // aucune raison de deserialiser ce qu'elle jette.
        let source = match geste {
            Geste::Supprimer => Value::Null,
            Geste::Reindexer => {
                let brut = stocke
                    .get_first(cible.gen.fields.source)
                    .and_then(|v| v.as_str().map(str::to_string))
                    .ok_or_else(|| EsError::internal("document sans _source stocke"))?;
                serde_json::from_str(&brut)
                    .map_err(|e| EsError::internal(format!("_source illisible: {e}")))?
            }
        };
        out.push(Candidat { id, seq_no, source });
    }
    Ok(out)
}

/// Ramasse l'adresse de **tous** les documents qui correspondent, sans score ni
/// tri : une commande par requete ne classe rien, elle ecrit.
struct CollecteurDeDocs;

struct SegmentDeDocs {
    seg: SegmentOrdinal,
    docs: Vec<(SegmentOrdinal, DocId)>,
}

impl Collector for CollecteurDeDocs {
    type Fruit = Vec<(SegmentOrdinal, DocId)>;
    type Child = SegmentDeDocs;

    fn for_segment(
        &self,
        seg: SegmentOrdinal,
        _reader: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        Ok(SegmentDeDocs {
            seg,
            docs: Vec::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, segments: Vec<Self::Fruit>) -> tantivy::Result<Self::Fruit> {
        Ok(segments.into_iter().flatten().collect())
    }
}

impl SegmentCollector for SegmentDeDocs {
    type Fruit = Vec<(SegmentOrdinal, DocId)>;

    fn collect(&mut self, doc: DocId, _score: Score) {
        self.docs.push((self.seg, doc));
    }

    fn harvest(self) -> Self::Fruit {
        self.docs
    }
}

/// La reponse au format d'ES.
///
/// L'ordre des cles est celui d'ES : un humain qui compare deux sorties a la
/// console ne doit pas avoir a les trier. `updated` n'existe **que** pour
/// `_update_by_query` — `_delete_by_query` ne le rend pas du tout, et le rendre
/// a zero serait deja une divergence de forme.
pub fn reponse(bilan: &Bilan, geste: Geste, took: u64) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("took".into(), json!(took));
    o.insert("timed_out".into(), json!(false));
    o.insert("total".into(), json!(bilan.total));
    if geste == Geste::Reindexer {
        o.insert("updated".into(), json!(bilan.updated));
    }
    o.insert("deleted".into(), json!(bilan.deleted));
    o.insert("batches".into(), json!(bilan.batches));
    o.insert("version_conflicts".into(), json!(bilan.version_conflicts));
    o.insert("noops".into(), json!(bilan.noops));
    o.insert("retries".into(), json!({"bulk": 0, "search": 0}));
    o.insert("throttled_millis".into(), json!(0));
    // `requests_per_second: -1.0` est ce qu'ES rend quand rien n'est regule.
    // ferrite refuse le parametre, donc la valeur est toujours celle-la.
    o.insert("requests_per_second".into(), json!(-1.0));
    o.insert("throttled_until_millis".into(), json!(0));
    o.insert("failures".into(), json!(bilan.failures));
    Value::Object(o)
}
