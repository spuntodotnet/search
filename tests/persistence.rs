//! L'index survit au redemarrage : mapping, documents, `_version` et `_seq_no`
//! sont relus depuis le disque.

use std::path::{Path, PathBuf};

use ferrite::engine::Catalog;
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
        "properties": {
            "titre": {"type": "text"},
            "auteur": {"type": "keyword"},
            "annee": {"type": "integer"},
        }
    }))
    .unwrap()
}

fn catalog(dir: &Path) -> std::sync::Arc<Catalog> {
    Catalog::open(dir.to_path_buf(), "ferrite".into(), "ferrite-0".into()).unwrap()
}

#[test]
fn un_index_survit_au_redemarrage() {
    let dir = tmp_dir("persist");

    {
        let cat = catalog(&dir);
        let idx = cat.create("livres", mapping()).unwrap();
        idx.index_doc(
            "1",
            &json!({"titre": "Le Horla", "auteur": "Maupassant", "annee": 1887}),
            false,
        )
        .unwrap();
        // Deux ecritures sur le meme identifiant : `_version` doit valoir 2.
        idx.index_doc(
            "1",
            &json!({"titre": "Le Horla", "auteur": "Maupassant", "annee": 1888}),
            false,
        )
        .unwrap();
        idx.index_doc(
            "2",
            &json!({"titre": "Bel-Ami", "auteur": "Maupassant"}),
            false,
        )
        .unwrap();
        idx.refresh().unwrap();
    }

    let cat = catalog(&dir);
    let idx = cat.get("livres").expect("l'index doit etre rouvert");
    assert_eq!(idx.doc_count(), 2);
    assert_eq!(idx.mapping.properties.len(), 3);

    let doc = idx.get_doc("1").unwrap().expect("le document doit etre la");
    assert_eq!(doc.version, 2, "la version doit survivre au redemarrage");
    assert_eq!(doc.source["annee"], json!(1888));

    // Les compteurs reprennent la ou ils s'etaient arretes.
    let out = idx
        .index_doc("1", &json!({"titre": "Le Horla", "auteur": "M."}), false)
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
        cat.create("livres", mapping()).unwrap();
        cat.delete("livres").unwrap();
    }
    let cat = catalog(&dir);
    assert!(!cat.exists("livres"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn un_champ_absent_du_mapping_est_refuse() {
    let dir = tmp_dir("strict");
    let cat = catalog(&dir);
    let idx = cat.create("livres", mapping()).unwrap();
    let err = idx
        .index_doc("1", &json!({"titre": "x", "inconnu": 1}), false)
        .unwrap_err();
    assert_eq!(err.ty, "strict_dynamic_mapping_exception");
    assert!(err.reason.contains("inconnu"));
    let _ = std::fs::remove_dir_all(&dir);
}
