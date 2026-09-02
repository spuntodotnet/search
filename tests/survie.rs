//! Non-regression : un chemin impossible ne doit pas faire mourir le processus.
//!
//! Le defaut de la carte 42 n'etait pas une erreur 500 mais un `panic!` — et le
//! profil de release porte `panic = "abort"`, donc le processus entier mourait,
//! tous les index avec. Un mapping accepte en 200, un seul document, et le
//! serveur n'existait plus.
//!
//! Ces cas sont des cas de **survie** avant d'etre des cas de compatibilite :
//! ils passent tous par le chemin qui paniquait — `Mapping::to_json`, qui repose
//! les chemins pointes en objets et n'avait plus d'objet ou nicher la feuille.
//! Lances contre le binaire 0.10.0, ils tombent : `cargo test` les rapporte en
//! `panicked at src/mapping.rs:818` (ou, pour deux d'entre eux, en
//! `Field already exists in schema a.b`, depuis tantivy).
//!
//! Les phrases sont celles d'un vrai Elasticsearch 8.15.0, mesurees ; le
//! detail des mesures est dans `tests/compat/sonde_survie.py`.

use std::path::PathBuf;

use ferrite::engine::{Catalog, FerriteIndex, WriteOptions};
use ferrite::mapping::{FieldMapping, FieldType, Mapping};
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

fn cat(nom: &str) -> std::sync::Arc<Catalog> {
    Catalog::open(tmp_dir(nom), "ferrite".into(), "ferrite-0".into()).unwrap()
}

fn index(
    catalog: &std::sync::Arc<Catalog>,
    nom: &str,
    mapping: serde_json::Value,
) -> std::sync::Arc<FerriteIndex> {
    catalog
        .create(nom, Mapping::parse(&mapping).unwrap(), Default::default())
        .unwrap()
}

/// La phrase du refus, ou le texte du succes.
fn ecrit(idx: &FerriteIndex, id: &str, doc: serde_json::Value) -> String {
    match idx.index_doc(id, &doc, WriteOptions::default()) {
        Ok(_) => "OK".to_string(),
        Err(e) => e.body()["error"]["reason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    }
}

#[test]
fn un_objet_pose_sur_une_feuille_est_refuse() {
    let c = cat("survie-objet");
    let idx = index(&c, "i", json!({"properties": {"a": {"type": "keyword"}}}));

    assert_eq!(
        ecrit(&idx, "1", json!({"a": {"b": "x"}})),
        "failed to parse field [a] of type [keyword] in document with id '1'. \
         Preview of field's value: '{b=x}'"
    );
    // Un objet **vide** aussi : ES le refuse, et ferrite l'acceptait en 201.
    assert_eq!(
        ecrit(&idx, "1", json!({"a": {}})),
        "failed to parse field [a] of type [keyword] in document with id '1'. \
         Preview of field's value: '{}'"
    );
    // L'apercu est le `toString` d'une Map de Java : cles **triees**, valeurs
    // nues, tableaux entre crochets, imbrication conservee.
    assert_eq!(
        ecrit(
            &idx,
            "1",
            json!({"a": {"c": "y", "b": {"z": [1, 2]}, "d": null}})
        ),
        "failed to parse field [a] of type [keyword] in document with id '1'. \
         Preview of field's value: '{b={z=[1, 2]}, c=y, d=null}'"
    );
    // Dans un tableau, c'est le **premier** objet qui sert d'apercu.
    assert_eq!(
        ecrit(&idx, "1", json!({"a": [1, {"b": "x"}, {"b": "y"}]})),
        "failed to parse field [a] of type [keyword] in document with id '1'. \
         Preview of field's value: '{b=x}'"
    );
    // Et le mapping n'a pas bouge : c'est tout l'objet du controle.
    assert!(idx.current().mapping.to_json()["properties"]["a"]["type"] == json!("keyword"));
}

#[test]
fn une_valeur_posee_sur_un_objet_est_refusee() {
    let c = cat("survie-valeur");
    let idx = index(
        &c,
        "i",
        json!({"properties": {"a": {"properties": {"b": {"type": "keyword"}}}}}),
    );
    assert_eq!(
        ecrit(&idx, "1", json!({"a": "x"})),
        "object mapping for [a] tried to parse field [a] as object, but found a concrete value"
    );
    // Dans un tableau, ES n'a plus de nom de champ courant et imprime `[null]`.
    assert_eq!(
        ecrit(&idx, "1", json!({"a": [1, 2]})),
        "object mapping for [a] tried to parse field [null] as object, but found a concrete value"
    );
    // Ce qui doit passer : un objet absent n'est pas un objet mal forme.
    assert_eq!(ecrit(&idx, "1", json!({"a": null})), "OK");
    assert_eq!(ecrit(&idx, "2", json!({"a": []})), "OK");
    assert_eq!(ecrit(&idx, "3", json!({"a": [null]})), "OK");
    assert_eq!(ecrit(&idx, "4", json!({"a": {"z": "y"}})), "OK");
}

#[test]
fn une_copie_vers_le_sous_chemin_d_une_feuille_est_refusee() {
    let c = cat("survie-copie");
    let idx = index(
        &c,
        "i",
        json!({"properties": {
            "a": {"type": "keyword"},
            "s": {"type": "keyword", "copy_to": "a.b.c"},
        }}),
    );
    // Le mapping est accepte — ES l'accepte aussi (mesure) : c'est le document
    // qui est refuse, et seulement s'il porte le champ source.
    assert_eq!(
        ecrit(&idx, "1", json!({"s": "x"})),
        "failed to parse field [a] of type [keyword] in document with id '1'. \
         Preview of field's value: '{b={c=x}}'"
    );
    // Une valeur nulle declenche quand meme le refus ; un tableau vide non.
    assert_eq!(
        ecrit(&idx, "1", json!({"s": null})),
        "failed to parse field [a] of type [keyword] in document with id '1'. \
         Preview of field's value: '{b={c=null}}'"
    );
    assert_eq!(ecrit(&idx, "1", json!({"s": []})), "OK");
    assert_eq!(ecrit(&idx, "2", json!({"a": "z"})), "OK");
}

#[test]
fn une_fusion_de_mapping_qui_ferait_un_chemin_impossible_est_refusee() {
    let c = cat("survie-fusion");
    let feuille = index(&c, "f", json!({"properties": {"a": {"type": "keyword"}}}));
    let err = feuille
        .add_fields([("a.b".to_string(), FieldMapping::new(FieldType::Keyword))].into())
        .unwrap_err();
    assert_eq!(
        err.body()["error"]["reason"],
        json!("can't merge a non object mapping [a] with an object mapping")
    );

    let objet = index(
        &c,
        "o",
        json!({"properties": {"a": {"properties": {"b": {"type": "keyword"}}}}}),
    );
    let err = objet
        .add_fields([("a".to_string(), FieldMapping::new(FieldType::Keyword))].into())
        .unwrap_err();
    assert_eq!(
        err.body()["error"]["reason"],
        json!("can't merge a non object mapping [a] with an object mapping")
    );

    // Un `nested` a sa propre phrase chez ES.
    let imbrique = index(
        &c,
        "n",
        json!({"properties": {
            "a": {"type": "nested", "properties": {"b": {"type": "keyword"}}}}}),
    );
    let err = imbrique
        .add_fields([("a".to_string(), FieldMapping::new(FieldType::Keyword))].into())
        .unwrap_err();
    assert_eq!(
        err.body()["error"]["reason"],
        json!("can't merge a non-nested mapping [a] with a nested mapping")
    );
}

#[test]
fn une_borne_de_date_multi_octets_ne_fait_pas_tomber_le_decoupage() {
    // `+aéb` fait quatre octets et la frontiere de caractere tombe au milieu :
    // le decoupage du decalage, qui compte en octets, paniquait dessus — sur un
    // document comme sur les six routes qui lisent une borne de date.
    let c = cat("survie-date");
    let idx = index(&c, "i", json!({"properties": {"d": {"type": "date"}}}));
    for valeur in [
        "2020-01-01T00:00:00+aéb",
        "2020-01-01T00:00:00+é:00",
        "2020-01-01T00:00:00+éé",
        "2020-01-01T00:00:00-aéb",
        "2020-01-01T00:00:00+\u{10348}",
    ] {
        let vu = ecrit(&idx, "1", json!({"d": valeur}));
        assert!(
            vu.starts_with("failed to parse date field [d]"),
            "{valeur} : {vu}"
        );
    }
    // Et ce qui doit continuer de marcher : les decalages licites.
    for valeur in [
        "2020-01-01T00:00:00+02",
        "2020-01-01T00:00:00+0200",
        "2020-01-01T00:00:00+02:00",
        "2020-01-01T00:00:00Z",
    ] {
        assert_eq!(ecrit(&idx, "1", json!({"d": valeur})), "OK", "{valeur}");
    }
}
