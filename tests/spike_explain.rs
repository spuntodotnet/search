//! Spike : les deux hypotheses sur tantivy dont depend l'arbre d'explication.
//!
//! `src/explain.rs` ne reconstruit pas un arbre a cote du scorer — il retourne
//! celui que tantivy produit pour la requete **reellement executee**, dans la
//! forme d'Elasticsearch. Cette traduction s'appuie sur deux choses que tantivy
//! ne documente pas comme des garanties :
//!
//! 1. les **phrases** de ses `Explanation` (`TermQuery, product of...`, `(K1+1)`,
//!    `idf, computed as ...`) — c'est sur elles que la traduction reconnait un
//!    noeud ;
//! 2. la forme du `Debug` d'un `Term`, seule facon d'aller chercher le champ et
//!    le terme d'un noeud de scoring (tantivy ne les met qu'en `context`).
//!
//! Une montee de version qui les change doit casser **ici**, bruyamment, plutot
//! que le jour ou un arbre plausible mais faux sortira en 200.
//!
//! Le troisieme test verrouille une precondition, pas une phrase :
//! `DocSet::seek` exige `target >= doc()`, et un `TermScorer` le verifie par un
//! `debug_assert`. C'est la raison pour laquelle `explain::expliquer` verifie
//! d'abord la correspondance avec son propre curseur.

use serde_json::Value;
use tantivy::query::{BooleanQuery, Occur, Query, QueryClone, TermQuery};
use tantivy::schema::{IndexRecordOption, Schema, STORED, TEXT};
use tantivy::{doc, Index, Term};

fn index() -> (Index, tantivy::Searcher, tantivy::schema::Field) {
    let mut sb = Schema::builder();
    let titre = sb.add_text_field("titre", TEXT | STORED);
    let schema = sb.build();
    let index = Index::create_in_ram(schema);
    let mut w = index.writer(15_000_000).expect("writer");
    w.add_document(doc!(titre => "alpha beta gamma"))
        .expect("doc");
    w.add_document(doc!(titre => "alpha")).expect("doc");
    w.add_document(doc!(titre => "delta")).expect("doc");
    w.commit().expect("commit");
    let searcher = index.reader().expect("reader").searcher();
    (index, searcher, titre)
}

fn arbre(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    fn descend(v: &Value, out: &mut Vec<String>) {
        if let Some(d) = v.get("description").and_then(Value::as_str) {
            out.push(d.to_string());
        }
        if let Some(Value::Array(a)) = v.get("details") {
            for e in a {
                descend(e, out);
            }
        }
    }
    descend(v, &mut out);
    out
}

#[test]
fn les_phrases_des_explications_de_tantivy_n_ont_pas_bouge() {
    let (_index, searcher, titre) = index();
    let q = TermQuery::new(
        Term::from_field_text(titre, "alpha"),
        IndexRecordOption::WithFreqs,
    );
    let e = q
        .explain(&searcher, tantivy::DocAddress::new(0, 0))
        .expect("le document correspond");
    let v = serde_json::to_value(&e).expect("serialisable");
    let phrases = arbre(&v);
    for attendue in [
        "TermQuery, product of...",
        "(K1+1)",
        "idf, computed as log(1 + (N - n + 0.5) / (n + 0.5))",
        "n, number of docs containing this term",
        "N, total number of docs",
        "freq / (freq + k1 * (1 - b + b * dl / avgdl))",
        "freq, occurrences of term within document",
        "k1, term saturation parameter",
        "b, length normalization parameter",
        "dl, length of field",
        "avgdl, average length of field",
    ] {
        assert!(
            phrases.iter().any(|p| p == attendue),
            "tantivy n'ecrit plus [{attendue}] : la traduction de src/explain.rs \
             ne reconnait plus ce noeud, l'arbre rendu serait plausible et faux. \
             Phrases vues : {phrases:?}"
        );
    }

    // Le booleen, l'autre phrase dont depend la traduction.
    let b = BooleanQuery::new(vec![(Occur::Must, q.box_clone())]);
    let e = b
        .explain(&searcher, tantivy::DocAddress::new(0, 0))
        .expect("le document correspond");
    let v = serde_json::to_value(&e).expect("serialisable");
    assert_eq!(arbre(&v)[0], "BooleanClause. sum of ...");
}

#[test]
fn le_debug_d_un_terme_porte_toujours_son_champ_et_sa_valeur() {
    let (_index, searcher, titre) = index();
    let q = TermQuery::new(
        Term::from_field_text(titre, "alpha"),
        IndexRecordOption::WithFreqs,
    );
    let e = q
        .explain(&searcher, tantivy::DocAddress::new(0, 0))
        .expect("le document correspond");
    let v = serde_json::to_value(&e).expect("serialisable");
    let contexte = v
        .get("context")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .expect("tantivy pose le terme en contexte");
    // `Term=Term(field=0, type=Str, "alpha")` : c'est la seule source du champ
    // et du terme, et `src/explain.rs` la lit avec cette forme-la.
    assert!(
        contexte.starts_with("Term=Term(field=0, type=Str, "),
        "la forme du Debug d'un Term a change : {contexte}"
    );
    assert!(contexte.ends_with("\"alpha\")"), "{contexte}");
}

#[test]
fn seek_en_arriere_est_interdit_donc_la_correspondance_se_verifie_avant() {
    let (_index, searcher, titre) = index();
    // « delta » ne correspond qu'au troisieme document : demander l'arbre du
    // premier ferait reculer le curseur. `explain::expliquer` doit rendre
    // `None` sans jamais poser la question a tantivy.
    let q: Box<dyn Query> = Box::new(TermQuery::new(
        Term::from_field_text(titre, "delta"),
        IndexRecordOption::WithFreqs,
    ));
    let schema = searcher.schema().clone();
    let addr = tantivy::DocAddress::new(0, 0);
    assert!(ferrite::explain::correspond(&searcher, &*q, addr).is_none());
    assert!(ferrite::explain::expliquer(&searcher, &*q, addr, &schema).is_none());
    // Et le cas qui a fait paniquer la sonde : un booleen dont le curseur est
    // deja passe. C'est lui qui a montre que le garde-fou ne pouvait pas etre
    // dans tantivy.
    let b: Box<dyn Query> = Box::new(BooleanQuery::new(vec![(
        Occur::Must,
        Box::new(TermQuery::new(
            Term::from_field_text(titre, "delta"),
            IndexRecordOption::WithFreqs,
        )) as Box<dyn Query>,
    )]));
    assert!(ferrite::explain::correspond(&searcher, &*b, addr).is_none());
    assert!(ferrite::explain::expliquer(&searcher, &*b, addr, &schema).is_none());
}
