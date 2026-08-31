//! Le scoring de ferrite, confronte a celui d'Elasticsearch — le sien, pas une
//! idee du sien.
//!
//! `tests/donnees/scoring.jsonl` est produit par
//! `tests/compat/genere_scoring.py`, qui appelle **dans le conteneur de
//! reference**, avec les jars d'ES au classpath :
//!
//!   - `GaussDecayFunctionBuilder$GaussScoreFunction` et ses deux soeurs
//!     (`processScale` puis `evaluate`) ;
//!   - `FieldValueFactorFunction$Modifier` (`apply`) ;
//!   - `CombineFunction` (`combine`).
//!
//! Chaque point de la grille est donc la reponse d'ES lui-meme. C'est ce qui
//! evite d'avoir a **choisir** une tolerance sur des flottants : la question
//! n'est pas « est-ce assez proche », c'est « est-ce le meme `f64` ».
//!
//! Une seule tolerance est ecrite, et elle est mesuree : `exp` et `ln` ne sont
//! pas les memes fonctions de bibliotheque des deux cotes (le JDK a les
//! siennes, Rust appelle la libm du systeme), et elles peuvent differer du
//! dernier bit. Le test compte donc les ULP d'ecart et **refuse au-dela de
//! [`ULP_MAX`]**, en imprimant le pire ecart constate — et il exige en plus
//! l'egalite **stricte** apres passage en `f32`, qui est ce que le client lit.
//! Un ecart d'un ULP de `f64` disparait toujours a la conversion : `f32` a 29
//! bits de mantisse en moins.

use std::collections::BTreeMap;

use ferrite::fonction_score::{Combinaison, Decroissance, Modificateur};
use serde_json::Value;

/// L'ecart maximal tolere entre le `double` d'ES et celui de ferrite, en ULP.
///
/// Ce n'est pas un seuil choisi pour que ca passe : c'est la borne de ce que
/// deux implementations correctement arrondies de `exp` / `log` peuvent se
/// permettre. Le test imprime le pire ecart reellement constate, et il echoue
/// si l'egalite en `f32` n'est pas exacte — donc si l'ecart se voyait.
const ULP_MAX: i64 = 1;

/// La distance en ULP entre deux `f64` de meme signe.
fn ulps(a: f64, b: f64) -> i64 {
    if a == b || (a.is_nan() && b.is_nan()) {
        return 0;
    }
    if a.is_nan() || b.is_nan() || a.is_infinite() || b.is_infinite() {
        return i64::MAX;
    }
    let cle = |x: f64| {
        let bits = x.to_bits() as i64;
        if bits < 0 {
            i64::MIN - bits
        } else {
            bits
        }
    };
    (cle(a) - cle(b)).abs()
}

/// Un nombre de la grille.
///
/// Ils y sont tous ecrits en **chaine**, pour deux raisons : JSON n'a ni `NaN`
/// ni les infinis, et surtout le parseur de flottants de `serde_json` se trompe
/// d'un ULP sur `1000000.0000000001` la ou le `str::parse::<f64>` de la
/// bibliotheque standard est exact. Une grille qui perd un bit en la lisant ne
/// mesure plus le bit qu'on veut mesurer.
fn nb(v: &Value) -> f64 {
    let s = v
        .as_str()
        .unwrap_or_else(|| panic!("nombre en chaine attendu, recu {v}"));
    match s {
        "NaN" => f64::NAN,
        "Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        autre => autre
            .parse()
            .unwrap_or_else(|_| panic!("valeur illisible : {autre}")),
    }
}

#[derive(Default)]
struct Compteurs {
    points: usize,
    /// Le pire ecart en ULP, par famille, avec le cas qui l'a produit.
    pire: BTreeMap<String, (i64, String)>,
    ecarts: Vec<String>,
}

impl Compteurs {
    fn compare(&mut self, famille: &str, attendu: f64, obtenu: f64, cas: &str) {
        self.points += 1;
        let d = ulps(attendu, obtenu);
        let e = self
            .pire
            .entry(famille.to_string())
            .or_insert((0, String::new()));
        if d > e.0 {
            *e = (d, cas.to_string());
        }
        // Ce que le client lit est un `float` : l'egalite y est exigee sans
        // tolerance, quel que soit l'ecart en `double`.
        let (fa, fo) = (attendu as f32, obtenu as f32);
        let meme_f32 = fa == fo || (fa.is_nan() && fo.is_nan());
        if (d > ULP_MAX || !meme_f32) && self.ecarts.len() < 20 {
            self.ecarts.push(format!(
                "{cas} : ES rend {attendu:e} ({fa:e} en f32), ferrite {obtenu:e} ({fo:e}) \
                 — {d} ULP"
            ));
        }
    }
}

#[test]
fn la_grille_de_scoring_d_elasticsearch() {
    let brut = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/donnees/scoring.jsonl"),
    )
    .expect("la grille est commitee");

    let mut c = Compteurs::default();
    let mut batteries = 0usize;
    let mut familles: BTreeMap<String, usize> = BTreeMap::new();

    for ligne in brut.lines() {
        let v: Value = serde_json::from_str(ligne).expect("ligne JSON");
        let Some(t) = v.get("t").and_then(Value::as_str) else {
            continue; // l'entete
        };
        batteries += 1;
        *familles.entry(t.to_string()).or_default() += 1;
        match t {
            "decroissance" => {
                let nom = v["f"].as_str().unwrap();
                let f = Decroissance::lit(nom).unwrap_or_else(|| panic!("fonction {nom}"));
                let (origine, s, d, offset) = (nb(&v["o"]), nb(&v["s"]), nb(&v["d"]), nb(&v["e"]));
                let echelle = f.echelle(s, d);
                c.compare(
                    "processScale",
                    nb(&v["p"]),
                    echelle,
                    &format!("{nom} processScale(scale={s}, decay={d})"),
                );
                for point in v["c"].as_array().unwrap() {
                    let p = point.as_array().unwrap();
                    let (valeur, distance, attendu) = (nb(&p[0]), nb(&p[1]), nb(&p[2]));
                    // La distance est aussi de ferrite : c'est la formule que
                    // le scorer applique, `max(0, |valeur - origin| - offset)`.
                    let notre = f64::max(0.0, (valeur - origine).abs() - offset);
                    c.compare(
                        "distance",
                        distance,
                        notre,
                        &format!("distance(valeur={valeur}, origin={origine}, offset={offset})"),
                    );
                    c.compare(
                        "evaluate",
                        attendu,
                        f.evalue(notre, echelle),
                        &format!(
                            "{nom} evaluate(distance={notre}, scale={s}, decay={d}, \
                             offset={offset}, origin={origine}, valeur={valeur})"
                        ),
                    );
                }
            }
            "modificateur" => {
                let nom = v["m"].as_str().unwrap();
                let m = Modificateur::lit(nom).unwrap_or_else(|| panic!("modifier {nom}"));
                for point in v["c"].as_array().unwrap() {
                    let p = point.as_array().unwrap();
                    let (valeur, attendu) = (nb(&p[0]), nb(&p[1]));
                    c.compare(
                        "modifier",
                        attendu,
                        m.applique(valeur),
                        &format!("{nom}({valeur:e})"),
                    );
                }
            }
            "combinaison" => {
                let nom = v["b"].as_str().unwrap();
                let b = Combinaison::lit(nom).unwrap_or_else(|| panic!("boost_mode {nom}"));
                for point in v["c"].as_array().unwrap() {
                    let p = point.as_array().unwrap();
                    let (score, facteur, plafond, attendu) =
                        (nb(&p[0]), nb(&p[1]), nb(&p[2]), nb(&p[3]));
                    c.compare(
                        "combine",
                        attendu,
                        f64::from(b.combine(score, facteur, plafond)),
                        &format!("{nom}.combine({score:e}, {facteur:e}, {plafond:e})"),
                    );
                }
            }
            autre => panic!("famille inconnue : {autre}"),
        }
    }

    eprintln!(
        "grille : {batteries} batteries, {} points ({})",
        c.points,
        familles
            .iter()
            .map(|(k, n)| format!("{n} {k}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for (famille, (d, cas)) in &c.pire {
        eprintln!("  pire ecart {famille} : {d} ULP  [{cas}]");
    }

    assert!(c.points > 40_000, "la grille doit etre entiere");
    assert!(
        c.ecarts.is_empty(),
        "{} ecart(s) avec la grille d'Elasticsearch :\n{}",
        c.ecarts.len(),
        c.ecarts.join("\n")
    );
}
