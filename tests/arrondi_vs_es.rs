//! L'arrondi de ferrite, confronte a celui d'Elasticsearch — le sien, pas une
//! idee du sien.
//!
//! `tests/donnees/arrondis.jsonl` est produit par
//! `tests/compat/genere_fuseaux.py --grille`, qui appelle la classe
//! `org.elasticsearch.common.Rounding` **dans le conteneur de reference**, avec
//! les jars d'ES au classpath. Chaque triplet y est donc la reponse d'ES
//! lui-meme, et non celle d'un test ecrit ici avec la meme idee fausse que le
//! code qu'il verifie.
//!
//! La grille ne balaie pas le temps a intervalle regulier : elle vise, zone par
//! zone, **les bascules de cette zone** — la milliseconde avant, celle d'apres,
//! une demi-heure de part et d'autre — plus une bascule de 2044 qu'aucune table
//! ne porte et que seules les regles annuelles savent produire. C'est la, et
//! seulement la, que l'arrondi est difficile.
//!
//! Le generateur verifie au passage que les **deux** chemins d'ES (l'optimise
//! et celui qui passe par `java.time`) rendent la meme chose : sans quoi la
//! grille ne dirait pas de quoi elle est la mesure.

use std::collections::BTreeMap;

use ferrite::calendrier::{lit_calendaire, lit_fixe, Arrondi, Intervalle};
use ferrite::fuseau::Fuseau;
use serde_json::Value;

#[test]
fn la_grille_d_elasticsearch() {
    let brut = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/donnees/arrondis.jsonl"),
    )
    .expect("la grille est commitee");

    let mut cas = 0usize;
    let mut batteries = 0usize;
    let mut zones = std::collections::BTreeSet::new();
    // Les ecarts sont ranges par (zone, intervalle) : un fuseau casse en
    // produit des dizaines, et les lire un par un noierait le premier.
    let mut ecarts: BTreeMap<String, (usize, String)> = BTreeMap::new();

    for ligne in brut.lines() {
        let v: Value = serde_json::from_str(ligne).expect("ligne JSON");
        let Some(zone) = v.get("z").and_then(Value::as_str) else {
            continue; // l'entete
        };
        let nom_intervalle = v["i"].as_str().unwrap();
        let offset = v["o"].as_i64().unwrap();
        let intervalle = match lit_calendaire(nom_intervalle) {
            Some(u) => Intervalle::Calendaire(u),
            None => Intervalle::Fixe(lit_fixe(nom_intervalle).expect("intervalle lisible")),
        };
        let fuseau = Fuseau::parse(zone).unwrap_or_else(|e| panic!("{zone} : {}", e.reason));
        let arrondi = Arrondi::new(intervalle, fuseau, offset);
        batteries += 1;
        zones.insert(zone.to_string());
        if std::env::var("TRACE").is_ok() {
            eprintln!("{zone} [{nom_intervalle}] offset={offset}");
        }

        for triplet in v["c"].as_array().unwrap() {
            let t = triplet.as_array().unwrap();
            let (instant, attendu, suivant_attendu) = (
                t[0].as_i64().unwrap(),
                t[1].as_i64().unwrap(),
                t[2].as_i64().unwrap(),
            );
            cas += 1;
            let obtenu = arrondi.arrondit(instant);
            let suivant = arrondi.suivant(attendu);
            if obtenu != attendu || suivant != suivant_attendu {
                let cle = format!("{zone} [{nom_intervalle}] offset={offset}");
                let e = ecarts.entry(cle).or_insert((0, String::new()));
                e.0 += 1;
                if e.1.is_empty() {
                    e.1 = format!(
                        "instant {instant} : seau {obtenu} (ES : {attendu}), \
                         suivant {suivant} (ES : {suivant_attendu})"
                    );
                }
            }
        }
    }

    assert!(cas > 20_000, "grille trop maigre : {cas} cas");
    assert!(
        zones.len() > 500,
        "grille trop etroite : {} zones",
        zones.len()
    );
    if !ecarts.is_empty() {
        let total: usize = ecarts.values().map(|(n, _)| n).sum();
        let mut message = format!(
            "{total} arrondis sur {cas} different d'Elasticsearch, sur {} batteries :\n",
            ecarts.len()
        );
        for (cle, (n, exemple)) in ecarts.iter().take(20) {
            message.push_str(&format!("  {cle} : {n} ecarts, dont {exemple}\n"));
        }
        panic!("{message}");
    }
    eprintln!(
        "{cas} arrondis d'Elasticsearch reproduits, {batteries} batteries, {} zones",
        zones.len()
    );
}
