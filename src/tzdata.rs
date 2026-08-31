//! La table des fuseaux horaires, **generee** — ne pas editer a la main.
//!
//! Source : le tzdb du JDK qu'embarque le conteneur Elasticsearch elasticsearch:8.15.0
//! (`jdk/lib/tzdb.dat`), c'est-a-dire les regles qu'Elasticsearch lui-meme
//! applique — pas celles du systeme, que son image n'a pas.
//!
//! Version du tzdb : **2024a**. 603 zones, 352 jeux de regles
//! distincts (les liens partagent les leurs), 18078 transitions
//! historiques, 238 regles annuelles pour le futur.
//!
//! Regenerer et verifier : `python3 tests/compat/genere_fuseaux.py [--verifie]`.
//! Le format est decrit dans ce script ; il est lu par [`crate::fuseau`].

/// La version du tzdb dont cette table est tiree.
pub const VERSION_TZDB: &str = "2024a";

/// Le nombre de zones que la table nomme.
pub const NB_ZONES: usize = 603;

/// La table elle-meme (voir `tests/compat/genere_fuseaux.py` pour son format).
pub static TABLE: &[u8] = include_bytes!("tzdata.bin");
