//! Spike : les deux primitives dont `nested` et `join` auraient besoin.
//!
//! `docs/nested-join.md` decrit deux chemins possibles. Chacun repose sur une
//! propriete de tantivy 0.26 qui n'est pas documentee comme une garantie : ce
//! fichier la met a l'epreuve, pour que le choix se fasse sur une mesure et non
//! sur une lecture optimiste du code.
//!
//! 1. **L'ordre des valeurs d'un champ multivalue** — s'il est conserve, la
//!    i-eme valeur de `lignes.ref` et la i-eme de `lignes.qte` decrivent le
//!    meme sous-objet, et `nested` peut se verifier colonne par colonne, sans
//!    jointure de bloc.
//! 2. **La contiguite des documents d'un meme `run()`** — si elle tient, on
//!    dispose de l'equivalent de `IndexWriter.addDocuments()` de Lucene, donc
//!    du bloc parent/enfants sur lequel repose sa jointure.
use std::collections::BTreeMap;

use tantivy::indexer::UserOperation;
use tantivy::schema::{Schema, FAST, STORED, STRING};
use tantivy::{doc, Index, IndexWriter};

#[test]
fn ordre_des_valeurs_multivaluees() {
    let mut sb = Schema::builder();
    // Volontairement a contre-sens de l'ordre alphabetique et numerique : si
    // tantivy triait ou dedupliquait, ca se verrait immediatement.
    let reference = sb.add_text_field("ref", STRING | FAST | STORED);
    let qte = sb.add_i64_field("qte", FAST | STORED);
    let schema = sb.build();

    let index = Index::create_in_ram(schema);
    let mut w: IndexWriter = index.writer(15_000_000).unwrap();
    w.add_document(doc!(
        reference => "zebre", reference => "abeille", reference => "zebre",
        qte => 30i64,          qte => 10i64,           qte => 20i64,
    ))
    .unwrap();
    w.commit().unwrap();

    let reader = index.reader().unwrap();
    let searcher = reader.searcher();
    let seg = searcher.segment_reader(0);
    let ff = seg.fast_fields();

    let qtes: Vec<i64> = ff.i64("qte").unwrap().values_for_doc(0).collect();
    let col = ff.str("ref").unwrap().unwrap();
    let ords: Vec<u64> = col.term_ords(0).collect();
    let mut refs = Vec::new();
    for ord in &ords {
        let mut buf = String::new();
        col.ord_to_str(*ord, &mut buf).unwrap();
        refs.push(buf);
    }

    println!("qte  : {qtes:?}");
    println!("ref  : {refs:?}  (ords {ords:?})");

    assert_eq!(qtes, vec![30, 10, 20], "un champ numerique garde son ordre");
    assert_eq!(
        refs,
        vec!["zebre", "abeille", "zebre"],
        "un champ texte garde son ordre, ses doublons, et n'est pas trie"
    );
}

/// Les documents d'un meme `IndexWriter::run()` atterrissent-ils cote a cote ?
///
/// `run()` empile toutes les operations dans un seul `AddBatch`, et un worker
/// consomme un batch entier dans *son* segment, document par document. C'est
/// l'equivalent d'`addDocuments()` chez Lucene — mais c'est un detail
/// d'implementation, pas un contrat. Avec quatre threads d'indexation et des
/// lots entrelaces, ce test verifie que chaque lot reste d'un seul tenant.
#[test]
fn contiguite_des_documents_d_un_meme_run() {
    let mut sb = Schema::builder();
    let lot = sb.add_u64_field("lot", FAST | STORED);
    let pos = sb.add_u64_field("pos", FAST | STORED);
    let index = Index::create_in_ram(sb.build());
    let w: IndexWriter = index.writer_with_num_threads(4, 60_000_000).unwrap();

    const LOTS: u64 = 40;
    const PAR_LOT: u64 = 5;
    for l in 0..LOTS {
        let ops: Vec<UserOperation> = (0..PAR_LOT)
            .map(|p| UserOperation::Add(doc!(lot => l, pos => p)))
            .collect();
        w.run(ops).unwrap();
    }
    let mut w = w;
    w.commit().unwrap();

    let searcher = index.reader().unwrap().searcher();
    // Pour chaque lot : la suite des (segment, docid, position dans le lot).
    let mut vus: BTreeMap<u64, Vec<(usize, u32, u64)>> = BTreeMap::new();
    for (n, seg) in searcher.segment_readers().iter().enumerate() {
        let ff = seg.fast_fields();
        let (cl, cp) = (ff.u64("lot").unwrap(), ff.u64("pos").unwrap());
        for d in 0..seg.max_doc() {
            vus.entry(cl.first(d).unwrap())
                .or_default()
                .push((n, d, cp.first(d).unwrap()));
        }
    }

    println!(
        "{} segments, {} lots",
        searcher.segment_readers().len(),
        vus.len()
    );
    assert_eq!(vus.len() as u64, LOTS);
    for (l, docs) in &vus {
        let (seg0, doc0, _) = docs[0];
        // Meme segment, docids consecutifs, et dans l'ordre d'insertion : le
        // lot forme bien un bloc, sans qu'aucun autre document s'y glisse.
        let attendu: Vec<(usize, u32, u64)> =
            (0..PAR_LOT).map(|p| (seg0, doc0 + p as u32, p)).collect();
        assert_eq!(docs, &attendu, "le lot {l} ne forme pas un bloc");
    }
}
