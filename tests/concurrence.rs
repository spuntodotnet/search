//! Ecritures, recherches et evolutions de schema en meme temps.
//!
//! Le mapping dynamique fait basculer l'index d'une generation a l'autre en
//! plein service. Ces tests verifient qu'aucun document ne se perd pendant une
//! bascule — c'est le risque principal de la mecanique : un ecrivain qui tient
//! l'ancienne generation pendant qu'elle est remplacee.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use ferrite::engine::{Catalog, WriteOptions};
use ferrite::mapping::Mapping;
use serde_json::json;

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ferrite-conc-{name}-{}-{}",
        std::process::id(),
        ferrite::util::random_uuid()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn catalog(dir: &Path) -> Arc<Catalog> {
    Catalog::open(dir.to_path_buf(), "ferrite".into(), "ferrite-0".into()).unwrap()
}

/// Le cas qui fait mal : plusieurs ecrivains introduisent des champs inedits en
/// meme temps, donc plusieurs evolutions se disputent l'index, pendant que des
/// lecteurs cherchent sans arret.
#[test]
fn aucune_perte_pendant_les_evolutions_concurrentes() {
    const ECRIVAINS: usize = 8;
    const PAR_ECRIVAIN: usize = 60;
    const LECTEURS: usize = 4;

    let dir = tmp_dir("evolutions");
    let cat = catalog(&dir);
    let idx = cat
        .create(
            "livres",
            Mapping::parse(&json!({"properties": {"titre": {"type": "text"}}})).unwrap(),
        )
        .unwrap();

    let barriere = Arc::new(Barrier::new(ECRIVAINS + LECTEURS));
    let stop = Arc::new(AtomicBool::new(false));
    let recherches = Arc::new(AtomicUsize::new(0));
    let mut threads = Vec::new();

    for w in 0..ECRIVAINS {
        let idx = idx.clone();
        let barriere = barriere.clone();
        threads.push(thread::spawn(move || {
            barriere.wait();
            for i in 0..PAR_ECRIVAIN {
                // Un champ inedit tous les dix documents : les evolutions se
                // chevauchent entre ecrivains.
                let mut doc = json!({"titre": format!("document {w}-{i}")});
                if i % 10 == 0 {
                    doc[format!("champ_{w}_{i}")] = json!(i as i64);
                }
                idx.index_doc(&format!("{w}-{i}"), &doc, WriteOptions::default())
                    .unwrap_or_else(|e| panic!("ecriture {w}-{i} : {e}"));
            }
        }));
    }

    for _ in 0..LECTEURS {
        let idx = idx.clone();
        let barriere = barriere.clone();
        let stop = stop.clone();
        let recherches = recherches.clone();
        threads.push(thread::spawn(move || {
            barriere.wait();
            while !stop.load(Ordering::Relaxed) {
                // Une generation coherente du debut a la fin de la recherche.
                let gen = idx.current();
                let searcher = gen.searcher();
                let top = searcher
                    .search(
                        &tantivy::query::AllQuery,
                        &tantivy::collector::TopDocs::with_limit(5).order_by_score(),
                    )
                    .expect("recherche");
                // Lire les documents, pas seulement les compter : c'est la que
                // tantivy ouvre le fichier de stockage, parfois paresseusement.
                // Une generation effacee trop tot se voit ici.
                for (_, addr) in top {
                    let doc: tantivy::schema::TantivyDocument =
                        searcher.doc(addr).expect("lecture du document");
                    let _ = doc.field_values().count();
                }
                recherches.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for t in threads.drain(..ECRIVAINS) {
        t.join().expect("un ecrivain a panique");
    }
    stop.store(true, Ordering::Relaxed);
    for t in threads {
        t.join().expect("un lecteur a panique");
    }

    idx.refresh().unwrap();

    let attendus = ECRIVAINS * PAR_ECRIVAIN;
    assert_eq!(
        idx.doc_count(),
        attendus,
        "des documents ont disparu pendant les bascules de generation"
    );

    // Chaque document doit etre relisible, avec son contenu intact.
    for w in 0..ECRIVAINS {
        for i in 0..PAR_ECRIVAIN {
            let id = format!("{w}-{i}");
            let doc = idx
                .get_doc(&id)
                .unwrap()
                .unwrap_or_else(|| panic!("document {id} introuvable"));
            assert_eq!(doc.source["titre"], json!(format!("document {w}-{i}")));
        }
    }

    // Et tous les champs decouverts doivent etre dans le mapping final.
    let mapping = idx.mapping();
    for w in 0..ECRIVAINS {
        for i in (0..PAR_ECRIVAIN).step_by(10) {
            let champ = format!("champ_{w}_{i}");
            assert!(
                mapping.properties.contains_key(&champ),
                "[{champ}] manque au mapping final"
            );
        }
    }
    assert!(
        recherches.load(Ordering::Relaxed) > 0,
        "aucune recherche jouee"
    );

    // Le contenu doit aussi survivre a un redemarrage.
    drop(idx);
    drop(cat);
    let cat = catalog(&dir);
    let idx = cat.get("livres").unwrap();
    assert_eq!(idx.doc_count(), attendus);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Ecritures et suppressions concurrentes sur les memes identifiants : les
/// compteurs `_version` doivent rester coherents.
#[test]
fn ecritures_et_suppressions_concurrentes() {
    const THREADS: usize = 8;
    const TOURS: usize = 40;

    let dir = tmp_dir("ecritures");
    let cat = catalog(&dir);
    let idx = cat
        .create(
            "livres",
            Mapping::parse(&json!({"properties": {"n": {"type": "long"}}})).unwrap(),
        )
        .unwrap();

    let barriere = Arc::new(Barrier::new(THREADS));
    let mut threads = Vec::new();
    for t in 0..THREADS {
        let idx = idx.clone();
        let barriere = barriere.clone();
        threads.push(thread::spawn(move || {
            barriere.wait();
            for i in 0..TOURS {
                // Les threads se disputent les memes dix identifiants.
                let id = format!("doc-{}", i % 10);
                if (t + i) % 4 == 3 {
                    idx.delete_doc(&id, WriteOptions::default()).unwrap();
                } else {
                    idx.index_doc(&id, &json!({"n": i as i64}), WriteOptions::default())
                        .unwrap();
                }
            }
        }));
    }
    for t in threads {
        t.join().expect("un thread a panique");
    }
    idx.refresh().unwrap();

    // Aucun identifiant ne doit exister en double, et l'index doit rester
    // lisible.
    assert!(idx.doc_count() <= 10);
    for i in 0..10 {
        let id = format!("doc-{i}");
        if let Some(doc) = idx.get_doc(&id).unwrap() {
            assert!(doc.version >= 1);
            assert!(doc.source["n"].is_number());
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
