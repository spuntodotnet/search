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
            Default::default(),
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
            Default::default(),
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

/// `refresh` est une **garantie** : au retour, ce qui etait ecrit avant l'appel
/// doit etre visible. Or ferrite rafraichit aussi en tache de fond.
///
/// Si le rafraichissement de fond a deja pris le drapeau « il y a du nouveau »
/// et n'a pas fini de commiter, un `refresh` explicite pourrait croire qu'il n'a
/// rien a faire et rendre la main **avant** que le document soit visible. Ce
/// test provoque cette course.
#[test]
fn refresh_garantit_la_visibilite_malgre_le_rafraichissement_de_fond() {
    let dir = tmp_dir("refresh");
    let cat = catalog(&dir);
    let idx = cat
        .create(
            "livres",
            Mapping::parse(&json!({"properties": {"titre": {"type": "text"}}})).unwrap(),
            Default::default(),
        )
        .unwrap();

    // Le rafraichissement de fond, en continu, comme dans le serveur.
    let stop = Arc::new(AtomicBool::new(false));
    let fond = {
        let idx = idx.clone();
        let stop = stop.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = idx.refresh();
            }
        })
    };

    for i in 0..300 {
        let id = format!("doc-{i}");
        idx.index_doc(
            &id,
            &json!({"titre": format!("titre {i}")}),
            WriteOptions::default(),
        )
        .unwrap();
        // Le contrat : apres ce refresh, le document est visible.
        idx.refresh().unwrap();

        let gen = idx.current();
        let searcher = gen.searcher();
        let terme = tantivy::Term::from_field_text(gen.fields.id, &id);
        let trouve = searcher
            .search(
                &tantivy::query::TermQuery::new(terme, tantivy::schema::IndexRecordOption::Basic),
                &tantivy::collector::Count,
            )
            .unwrap();
        assert_eq!(
            trouve, 1,
            "[{id}] devait etre visible apres refresh (tour {i})"
        );
    }

    stop.store(true, Ordering::Relaxed);
    fond.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Un index supprime survit dans la boucle de fond — et son homonyme recree
/// porte **les memes chemins**.
///
/// `refresh_dirty` travaille sur un instantane du catalogue : entre le moment
/// ou elle prend la liste et celui ou elle s'occupe d'un index, un `DELETE`
/// peut avoir retire celui-ci. L'`Arc` qu'elle tient reste vivant, et ses
/// repertoires s'appellent `{index}/index-0`, `{index}/index-1` — exactement
/// ceux qu'un index du meme nom, cree juste apres, vient de s'attribuer. Le
/// vieux commit ecrit alors dans les fichiers du neuf : tantivy y publie son
/// propre `meta.json` et efface les fichiers qu'il ne reference pas. Et le
/// vieux balayage efface le repertoire d'une generation du neuf.
///
/// C'est ce qui a fait battre le cliquet de conformance : `nettoie()` supprime
/// `test1` entre deux cas, le cas suivant le recree aussitot, et une campagne
/// sur treize voyait la boucle de fond passer entre les deux — en 500
/// (« Failed to acquire Lockfile », « FileDoesNotExist(...index-1/....term) »).
/// Rien ne fuyait dans l'API : l'etat verifie entre deux cas etait vide a
/// chaque fois.
#[test]
fn un_index_supprime_ne_touche_plus_aux_fichiers_de_son_homonyme() {
    let dir = tmp_dir("recreation");
    let cat = catalog(&dir);

    // Un premier `t` : `index-0` a la creation, puis `index-1` des qu'un champ
    // inedit force une evolution — et `index-0` part aux generations retirees.
    let ancien = cat.get_or_create("t").unwrap();
    ancien
        .index_doc("1", &json!({"bar": "bar"}), WriteOptions::default())
        .unwrap();
    // Pas de `refresh` : l'index reste « sale », donc la boucle de fond aura
    // quelque chose a commiter apres coup — c'est tout le sujet.

    // La boucle de fond tient cet `Arc` pendant qu'on supprime.
    cat.delete("t").unwrap();

    // Le meme nom, tout de suite apres : memes repertoires. Il repart de
    // `index-0` — celui-la meme que l'ancien garde dans ses generations
    // retirees.
    let neuf = cat.get_or_create("t").unwrap();

    // Ce que fait la boucle de fond sur un index qui n'existe plus.
    let _ = ancien.refresh();
    ancien.balayer_generations_retirees();

    // Le nouvel index doit etre intact : ecrire, rafraichir, relire.
    neuf.index_doc("2", &json!({"bar": "bar"}), WriteOptions::default())
        .expect("ecrire dans le nouvel index");
    neuf.index_doc("3", &json!({"bar": "encore"}), WriteOptions::default())
        .expect("ecrire dans le nouvel index");
    neuf.refresh().expect("rafraichir le nouvel index");
    assert_eq!(
        compte(&neuf),
        2,
        "les documents du nouvel index ne sont pas tous la"
    );

    // La suppression libere le nom par un renommage sous une corbeille, et ce
    // qui n'a pas pu etre efface l'est a l'ouverture suivante : rouvrir le
    // catalogue ne doit ressusciter ni l'index supprime ni sa corbeille.
    drop(neuf);
    drop(ancien);
    drop(cat);
    let rouvert = catalog(&dir);
    let noms: Vec<String> = rouvert.list().iter().map(|i| i.name.clone()).collect();
    assert_eq!(
        noms,
        vec!["t".to_string()],
        "le catalogue rouvert : {noms:?}"
    );
    assert_eq!(compte(&rouvert.get("t").unwrap()), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Les documents que la recherche voit vraiment, generation courante comprise.
fn compte(idx: &Arc<ferrite::engine::FerriteIndex>) -> usize {
    let gen = idx.current();
    gen.searcher()
        .search(&tantivy::query::AllQuery, &tantivy::collector::Count)
        .unwrap()
}
