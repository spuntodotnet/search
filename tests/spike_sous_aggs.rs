//! Spike : une sous-agregation de tantivy voit-elle **tous** les documents de
//! son bucket ?
//!
//! Ce fichier existe parce que la reponse a longtemps ete **non**, en silence.
//! tantivy 0.26.1 vidait son cache de sous-agregations tous les 2 048 documents
//! d'un segment en ne recopiant que les buckets au-dessus d'un seuil, puis
//! effacait le cache **entier** : les documents des buckets qu'il n'avait pas
//! recopies etaient perdus. Les `doc_count` restaient justes — donc la reponse
//! avait l'air bonne, en 200. Sur deux millions de documents de la track
//! `geonames`, un bucket de 28 518 documents rendait un `value_count` de 1 692.
//!
//! Le correctif est celui d'amont (tantivy issue #2992), epingle par le
//! `[patch.crates-io]` de `Cargo.toml` — voir `docs/tantivy-patch.md`. Ce
//! spike est ce qui le tient : il ne teste pas ferrite, il teste la
//! **dependance**, exactement comme `spike_nested.rs`. Le jour ou l'epingle
//! saute (montee de version, `cargo update`, retrait du patch), il casse
//! bruyamment ici plutot que de rendre des valeurs fausses en production.
//!
//! Les trois nombres qui suivent ne sont pas des reglages : ils viennent de la
//! mesure (`tests/compat/sonde_sous_aggs.py --seuil`).
//!
//! * le cache se vide tous les **2 048** documents d'un meme segment ;
//! * un bucket est perdu s'il a **au plus `2048 / (2 * nombre de buckets)`**
//!   documents dans la fenetre qui se vide ;
//! * seul le chemin « peu de buckets » est touche : un `terms` de premier
//!   niveau sous **100** valeurs distinctes, et tout `range`.
use std::collections::BTreeMap;

use tantivy::aggregation::agg_req::Aggregations;
use tantivy::aggregation::{AggContextParams, AggregationCollector, AggregationLimitsGuard};
use tantivy::query::AllQuery;
use tantivy::schema::{Schema, FAST, STRING};
use tantivy::{doc, Index, IndexWriter};

/// Le corpus du defaut : un segment, un bucket dominant, des buckets **rares**.
///
/// Les deux moities comptent. Sans plus de 2 048 documents dans le segment, le
/// cache ne se vide jamais avant la fin et rien ne se perd — c'est pour ca que
/// les 600 documents de `diff_aggs.py` ne l'ont jamais vu. Sans bucket rare,
/// chaque bucket recoit sa part de chaque fenetre et depasse toujours le seuil.
fn index_desequilibre(nb_docs: usize, nb_rares: usize) -> (Index, BTreeMap<String, (u64, f64)>) {
    let mut sb = Schema::builder();
    let categorie = sb.add_text_field("categorie", STRING | FAST);
    let note = sb.add_f64_field("note", FAST);
    let schema = sb.build();

    let index = Index::create_in_ram(schema);
    // Un seul thread d'ecriture et un seul commit : un segment, donc un compte
    // de documents par segment qui est celui qu'on croit mesurer.
    let mut w: IndexWriter = index.writer_with_num_threads(1, 50_000_000).unwrap();

    let mut verite: BTreeMap<String, (u64, f64)> = BTreeMap::new();
    for i in 0..nb_docs {
        // Un document sur 25 va dans l'un des `nb_rares` buckets minoritaires,
        // le reste dans le bucket dominant. Les rares sont donc semes dans
        // toutes les fenetres de vidage, pas groupes dans la derniere (que le
        // vidage final recopie de toute facon).
        let cat = if i % 25 == 0 {
            format!("rare_{:02}", (i / 25) % nb_rares)
        } else {
            "dominant".to_string()
        };
        let valeur = (i % 13 + 1) as f64;
        let e = verite.entry(cat.clone()).or_insert((0, 0.0));
        e.0 += 1;
        e.1 += valeur;
        w.add_document(doc!(categorie => cat, note => valeur))
            .unwrap();
    }
    w.commit().unwrap();
    (index, verite)
}

fn agrege(index: &Index, demande: serde_json::Value) -> serde_json::Value {
    let requete: Aggregations = serde_json::from_value(demande).unwrap();
    let limites = AggregationLimitsGuard::new(Some(500_000_000), Some(65_000));
    let contexte = AggContextParams::new(limites, index.tokenizers().clone());
    let collecteur = AggregationCollector::from_aggs(requete, contexte);
    let searcher = index.reader().unwrap().searcher();
    serde_json::to_value(searcher.search(&AllQuery, &collecteur).unwrap()).unwrap()
}

/// Le chemin « peu de buckets » d'un `terms` de premier niveau : celui qui
/// perdait les documents. 90 valeurs distinctes, donc sous les 100 au-dessus
/// desquelles tantivy prend l'autre cache.
#[test]
fn terms_peu_de_buckets_ne_perd_aucun_document() {
    let (index, verite) = index_desequilibre(5_000, 89);
    let res = agrege(
        &index,
        serde_json::json!({
            "b": {"terms": {"field": "categorie", "size": 200},
                  "aggs": {"n": {"sum": {"field": "note"}}}}
        }),
    );

    let buckets = res["b"]["buckets"].as_array().unwrap();
    assert_eq!(
        buckets.len(),
        verite.len(),
        "90 valeurs distinctes attendues, `size` est assez grand pour toutes"
    );
    for b in buckets {
        let cle = b["key"].as_str().unwrap();
        let (compte, somme) = verite[cle];
        assert_eq!(
            b["doc_count"].as_u64().unwrap(),
            compte,
            "doc_count de [{cle}] : c'est la moitie qui restait juste quand la \
             sous-agregation, elle, etait fausse"
        );
        assert_eq!(
            b["n"]["value"].as_f64().unwrap(),
            somme,
            "somme de la sous-agregation de [{cle}] : tantivy 0.26.1 non corrige \
             rend ici la seule derniere fenetre de vidage. Si cette ligne casse, \
             l'epingle de `Cargo.toml` a saute — voir docs/tantivy-patch.md"
        );
    }
}

/// L'autre chemin fautif : un `range`, qui prend toujours ce cache-la, quel que
/// soit son nombre d'intervalles.
#[test]
fn range_ne_perd_aucun_document() {
    let (index, _) = index_desequilibre(5_000, 1);
    let res = agrege(
        &index,
        serde_json::json!({
            "b": {"range": {"field": "note", "ranges": [{"to": 13.0}, {"from": 13.0}]},
                  "aggs": {"n": {"value_count": {"field": "note"}}}}
        }),
    );

    // L'invariant qu'Elasticsearch tient toujours : chaque document du bucket
    // porte une valeur, donc `value_count` vaut exactement `doc_count`.
    for b in res["b"]["buckets"].as_array().unwrap() {
        assert_eq!(
            b["n"]["value"].as_f64().unwrap(),
            b["doc_count"].as_u64().unwrap() as f64,
            "bucket [{}] : la sous-agregation ne compte pas tous ses documents",
            b["key"].as_str().unwrap()
        );
    }
}

/// Le seuil, mesure plutot que suppose : sous 2 048 documents dans le segment,
/// le defaut n'apparait **pas**. C'est ce qui explique qu'il ait survecu a tous
/// les corpus ecrits a la main — et c'est ce que ce cas fige, pour que la borne
/// publiee dans `docs/compat.md` reste une mesure.
#[test]
fn le_seuil_est_bien_de_2048_documents_par_segment() {
    for nb_docs in [2_047, 2_048] {
        // Un unique document rare, place en tete : il tombe donc dans la
        // premiere fenetre. A 2 047 documents aucune fenetre ne se vide avant
        // la fin ; a 2 048, une se vide, et c'est la que le document se perdait.
        let mut sb = Schema::builder();
        let categorie = sb.add_text_field("categorie", STRING | FAST);
        let note = sb.add_f64_field("note", FAST);
        let index = Index::create_in_ram(sb.build());
        let mut w: IndexWriter = index.writer_with_num_threads(1, 50_000_000).unwrap();
        for i in 0..nb_docs {
            let cat = if i == 0 { "rare" } else { "c0" };
            w.add_document(doc!(categorie => cat, note => 1.0f64))
                .unwrap();
        }
        w.commit().unwrap();

        let res = agrege(
            &index,
            serde_json::json!({
                "b": {"terms": {"field": "categorie", "size": 10},
                      "aggs": {"n": {"value_count": {"field": "note"}}}}
            }),
        );
        let rare = res["b"]["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["key"] == "rare")
            .expect("le bucket rare doit exister : son doc_count a toujours ete juste");
        assert_eq!(rare["doc_count"].as_u64().unwrap(), 1);
        assert_eq!(
            rare["n"]["value"].as_f64().unwrap(),
            1.0,
            "a {nb_docs} documents dans le segment, le document du bucket rare a \
             disparu de sa sous-agregation"
        );
    }
}
