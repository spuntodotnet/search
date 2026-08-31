//! `_delete_by_query` / `_update_by_query` : l'ordre du balayage et les
//! conflits, sans Docker.
//!
//! Ces deux propriétés sont celles qui ne se voient pas dans les compteurs :
//!
//! * **quels** documents partent quand `max_docs` est plus petit que le nombre
//!   de correspondances. La règle est l'ordre d'écriture, et elle a été payée :
//!   ferrite triait sur le numéro de document de tantivy, qui **n'est pas**
//!   l'ordre d'écriture (un `_bulk` de 25 documents en ressort mélangé). Il
//!   supprimait donc d'autres documents qu'Elasticsearch, en 200 et sans un
//!   mot — trouvé par `tests/compat/fuzz_vs_es.py` (graine 2727085) ;
//! * un document réécrit **après** le relevé n'est pas touché : c'est le
//!   `version_conflict`, et c'est ce qui empêche une purge de supprimer une
//!   version qu'elle n'a jamais vue.
//!
//! Le reste (les compteurs, les formes de réponse, les refus) se mesure contre
//! un vrai Elasticsearch : `tests/compat/sonde_par_requete.py`.

use std::path::PathBuf;
use std::sync::Arc;

use ferrite::engine::{Catalog, FerriteIndex, Generation, WriteOptions};
use ferrite::mapping::Mapping;
use ferrite::parrequete::{executer, Cible, Demande, Geste};
use serde_json::json;
use tantivy::query::AllQuery;

fn index(nom: &str) -> (Arc<Catalog>, Arc<FerriteIndex>, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "ferrite-test-{nom}-{}-{}",
        std::process::id(),
        ferrite::util::random_uuid()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let cat = Catalog::open(dir.clone(), "ferrite".into(), "ferrite-0".into()).unwrap();
    let mapping = Mapping::parse(&json!({
        "properties": {"n": {"type": "integer"}}
    }))
    .unwrap();
    let idx = cat.create(nom, mapping, Default::default()).unwrap();
    for i in 0..6 {
        idx.index_doc(&format!("d{i}"), &json!({"n": i}), WriteOptions::default())
            .unwrap();
    }
    idx.refresh().unwrap();
    (cat, idx, dir)
}

fn cible(idx: &Arc<FerriteIndex>) -> (Cible, Arc<Generation>) {
    let gen = idx.current();
    (
        Cible {
            index: idx.clone(),
            gen: gen.clone(),
            query: Box::new(AllQuery),
            incidents: Default::default(),
        },
        gen,
    )
}

fn restants(idx: &Arc<FerriteIndex>) -> Vec<String> {
    (0..6)
        .map(|i| format!("d{i}"))
        .filter(|id| idx.get_doc(id).unwrap().is_some())
        .collect()
}

#[test]
fn max_docs_retient_les_premiers_ecrits() {
    let (_cat, idx, dir) = index("max-docs");
    let (c, _gen) = cible(&idx);
    let bilan = executer(
        &[c],
        Geste::Supprimer,
        &Demande {
            max_docs: Some(2),
            ..Demande::default()
        },
    )
    .unwrap();
    assert_eq!((bilan.total, bilan.deleted, bilan.batches), (2, 2, 1));
    // Les deux **premiers écrits**, quel que soit l'ordre interne de tantivy.
    assert_eq!(restants(&idx), vec!["d2", "d3", "d4", "d5"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn un_document_reecrit_apres_le_releve_est_un_conflit() {
    let (_cat, idx, dir) = index("conflit");
    // Le relevé se fait sur cette génération-là : la cible est prise **avant**
    // la réécriture, comme une recherche l'aurait fait.
    let (c, _gen) = cible(&idx);
    idx.index_doc("d3", &json!({"n": 99}), WriteOptions::default())
        .unwrap();

    let bilan = executer(&[c], Geste::Supprimer, &Demande::default()).unwrap();
    assert_eq!(bilan.total, 6);
    assert_eq!(bilan.deleted, 5);
    assert_eq!(bilan.version_conflicts, 1);
    assert_eq!(bilan.failures.len(), 1, "{:?}", bilan.failures);
    let echec = &bilan.failures[0];
    assert_eq!(echec["id"], json!("d3"));
    assert_eq!(echec["status"], json!(409));
    assert_eq!(
        echec["cause"]["type"],
        json!("version_conflict_engine_exception")
    );
    // Le message dit que le document a **bougé**, pas qu'il a disparu : ES a
    // deux phrases pour ces deux cas.
    let raison = echec["cause"]["reason"].as_str().unwrap();
    assert!(raison.contains("current document has seqNo"), "{raison}");
    assert_eq!(restants(&idx), vec!["d3"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conflicts_proceed_compte_sans_remplir_failures() {
    let (_cat, idx, dir) = index("proceed");
    let (c, _gen) = cible(&idx);
    idx.index_doc("d3", &json!({"n": 99}), WriteOptions::default())
        .unwrap();

    let bilan = executer(
        &[c],
        Geste::Supprimer,
        &Demande {
            proceder_sur_conflit: true,
            ..Demande::default()
        },
    )
    .unwrap();
    assert_eq!(bilan.version_conflicts, 1);
    assert!(bilan.failures.is_empty(), "{:?}", bilan.failures);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn abort_va_au_bout_du_lot_puis_s_arrete() {
    let (_cat, idx, dir) = index("abort");
    let (c, _gen) = cible(&idx);
    // Le conflit portera sur d2, c'est-à-dire sur le **deuxième** lot de deux.
    idx.index_doc("d2", &json!({"n": 99}), WriteOptions::default())
        .unwrap();

    let bilan = executer(
        &[c],
        Geste::Supprimer,
        &Demande {
            taille_de_lot: 2,
            ..Demande::default()
        },
    )
    .unwrap();
    // `total` ne diminue pas quand la commande s'interrompt.
    assert_eq!(bilan.total, 6);
    // Deux lots traités : d0, d1, puis d2 (conflit) et d3 quand même.
    assert_eq!(bilan.batches, 2);
    assert_eq!(bilan.deleted, 3);
    assert_eq!(bilan.version_conflicts, 1);
    assert_eq!(restants(&idx), vec!["d2", "d4", "d5"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reindexer_fait_avancer_la_version_sans_changer_le_source() {
    let (_cat, idx, dir) = index("reindex");
    let (c, _gen) = cible(&idx);
    let avant = idx.get_doc("d0").unwrap().unwrap();

    let bilan = executer(&[c], Geste::Reindexer, &Demande::default()).unwrap();
    assert_eq!((bilan.total, bilan.updated, bilan.deleted), (6, 6, 0));
    // Sans script, un document identique compte `updated`, jamais `noop`.
    assert_eq!(bilan.noops, 0);

    let apres = idx.get_doc("d0").unwrap().unwrap();
    assert_eq!(apres.source, avant.source);
    assert_eq!(apres.version, avant.version + 1);
    let _ = std::fs::remove_dir_all(&dir);
}
