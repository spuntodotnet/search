//! L'index survit au redemarrage : mapping, documents, `_version` et `_seq_no`
//! sont relus depuis le disque.

use std::path::{Path, PathBuf};

use ferrite::engine::{Catalog, WriteOptions};
use ferrite::mapping::Mapping;
use serde_json::json;

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ferrite-test-{name}-{}-{}",
        std::process::id(),
        ferrite::util::random_uuid()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn mapping() -> Mapping {
    Mapping::parse(&json!({
        "dynamic": "strict",
        "properties": {
            "titre": {"type": "text"},
            "auteur": {"type": "keyword"},
            "annee": {"type": "integer"},
        }
    }))
    .unwrap()
}

fn mapping_dynamique() -> Mapping {
    Mapping::parse(&json!({"properties": {"titre": {"type": "text"}}})).unwrap()
}

fn catalog(dir: &Path) -> std::sync::Arc<Catalog> {
    Catalog::open(dir.to_path_buf(), "ferrite".into(), "ferrite-0".into()).unwrap()
}

#[test]
fn un_index_survit_au_redemarrage() {
    let dir = tmp_dir("persist");

    {
        let cat = catalog(&dir);
        let idx = cat.create("livres", mapping(), Default::default()).unwrap();
        idx.index_doc(
            "1",
            &json!({"titre": "Le Horla", "auteur": "Maupassant", "annee": 1887}),
            WriteOptions::default(),
        )
        .unwrap();
        // Deux ecritures sur le meme identifiant : `_version` doit valoir 2.
        idx.index_doc(
            "1",
            &json!({"titre": "Le Horla", "auteur": "Maupassant", "annee": 1888}),
            WriteOptions::default(),
        )
        .unwrap();
        idx.index_doc(
            "2",
            &json!({"titre": "Bel-Ami", "auteur": "Maupassant"}),
            WriteOptions::default(),
        )
        .unwrap();
        idx.refresh().unwrap();
    }

    let cat = catalog(&dir);
    let idx = cat.get("livres").expect("l'index doit etre rouvert");
    assert_eq!(idx.doc_count(), 2);
    assert_eq!(idx.mapping().properties.len(), 3);

    let doc = idx.get_doc("1").unwrap().expect("le document doit etre la");
    assert_eq!(doc.version, 2, "la version doit survivre au redemarrage");
    assert_eq!(doc.source["annee"], json!(1888));

    // Les compteurs reprennent la ou ils s'etaient arretes.
    let out = idx
        .index_doc(
            "1",
            &json!({"titre": "Le Horla", "auteur": "M."}),
            WriteOptions::default(),
        )
        .unwrap();
    assert_eq!(out.version, 3);
    assert!(!out.created);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn un_index_supprime_ne_revient_pas() {
    let dir = tmp_dir("delete");
    {
        let cat = catalog(&dir);
        cat.create("livres", mapping(), Default::default()).unwrap();
        cat.delete("livres").unwrap();
    }
    let cat = catalog(&dir);
    assert!(!cat.exists("livres"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn un_champ_absent_du_mapping_est_refuse_en_strict() {
    let dir = tmp_dir("strict");
    let cat = catalog(&dir);
    let idx = cat.create("livres", mapping(), Default::default()).unwrap();
    let err = idx
        .index_doc(
            "1",
            &json!({"titre": "x", "inconnu": 1}),
            WriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(err.ty, "strict_dynamic_mapping_exception");
    assert!(err.reason.contains("inconnu"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Le coeur du mapping dynamique : le schema tantivy est fige, donc ferrite
/// change de generation et rejoue les documents deja indexes.
#[test]
fn un_champ_decouvert_fait_changer_de_generation() {
    let dir = tmp_dir("dyn");
    {
        let cat = catalog(&dir);
        let idx = cat
            .create("livres", mapping_dynamique(), Default::default())
            .unwrap();
        idx.index_doc("1", &json!({"titre": "Bel-Ami"}), WriteOptions::default())
            .unwrap();
        idx.refresh().unwrap();

        // Nouveau champ : le mapping grandit...
        idx.index_doc(
            "2",
            &json!({"titre": "Nana", "annee": 1880}),
            WriteOptions::default(),
        )
        .unwrap();
        idx.refresh().unwrap();
        let m = idx.mapping();
        assert_eq!(m.properties["annee"].ty.name(), "long");
        // ...et une chaine gagne son sous-champ keyword, comme chez ES.
        idx.index_doc(
            "3",
            &json!({"titre": "Germinal", "auteur": "Zola"}),
            WriteOptions::default(),
        )
        .unwrap();
        idx.refresh().unwrap();
        let m = idx.mapping();
        assert_eq!(m.properties["auteur"].ty.name(), "text");
        assert_eq!(
            m.properties["auteur"].fields["keyword"].ty.name(),
            "keyword"
        );

        // Le document indexe AVANT l'evolution est toujours la, entier.
        assert_eq!(idx.doc_count(), 3);
        let doc = idx.get_doc("1").unwrap().unwrap();
        assert_eq!(doc.source["titre"], json!("Bel-Ami"));
        assert_eq!(doc.version, 1);
    }

    // Et tout cela survit au redemarrage.
    let cat = catalog(&dir);
    let idx = cat.get("livres").unwrap();
    assert_eq!(idx.doc_count(), 3);
    assert!(idx.mapping().properties.contains_key("annee"));
    assert_eq!(
        idx.get_doc("2").unwrap().unwrap().source["annee"],
        json!(1880)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `dynamic: false` : le champ reste dans `_source`, sans etre indexe.
#[test]
fn dynamic_false_conserve_sans_indexer() {
    let dir = tmp_dir("dynfalse");
    let cat = catalog(&dir);
    let mapping = Mapping::parse(&json!({
        "dynamic": false,
        "properties": {"titre": {"type": "text"}}
    }))
    .unwrap();
    let idx = cat.create("livres", mapping, Default::default()).unwrap();
    idx.index_doc(
        "1",
        &json!({"titre": "Bel-Ami", "note": 5}),
        WriteOptions::default(),
    )
    .unwrap();
    idx.refresh().unwrap();

    // Le champ n'entre pas dans le mapping...
    assert!(!idx.mapping().properties.contains_key("note"));
    // ...mais il est bien rendu dans le document.
    let doc = idx.get_doc("1").unwrap().unwrap();
    assert_eq!(doc.source["note"], json!(5));
    let _ = std::fs::remove_dir_all(&dir);
}
