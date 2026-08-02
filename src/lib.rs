//! ferrite — un moteur de recherche compatible avec l'API Elasticsearch, bati
//! sur [tantivy].
//!
//! Le decoupage en modules suit la couture du produit, et c'est volontaire :
//!
//! - [`mapping`] : le modele de mapping Elasticsearch et sa traduction en
//!   schema tantivy. C'est le point dur (ES accepte des champs a la volee,
//!   tantivy veut un schema fige), donc il vit seul.
//! - [`dsl`] : la traduction Query DSL -> [`tantivy::query::Query`]. Ne connait
//!   ni HTTP ni le stockage.
//! - [`regexp`] : la syntaxe d'expression reguliere de Lucene, traduite vers
//!   celle du crate `regex`. Elles se ressemblent assez pour qu'on croie pouvoir
//!   passer le motif tel quel, et divergent sur des caracteres courants.
//! - [`engine`] : le catalogue d'index, l'ecriture, la lecture, la persistance.
//!   Ne connait pas HTTP.
//! - [`alias`] et [`selection`] : les noms sous lesquels une requete designe des
//!   index — alias, listes, motifs — et leur resolution en index concrets.
//! - [`search`] : l'execution d'une recherche et la mise en forme du resultat au
//!   format ES.
//! - [`api`] : la couche HTTP — routage, parametres, format de reponse et
//!   d'erreur. Ne contient aucune logique de moteur.

pub mod aggs;
pub mod alias;
pub mod analysis;
pub mod api;
pub mod dateformat;
pub mod dismax;
pub mod dsl;
pub mod engine;
pub mod error;
pub mod mapping;
pub mod nested;
pub mod regexp;
pub mod search;
pub mod selection;
pub mod stemmer;
pub mod util;

/// Version d'Elasticsearch annoncee par ferrite.
///
/// Les clients 8.x negocient sur ce numero : il doit etre une version 8.x
/// plausible, et la meme partout ou ES la renvoie (`/`, `_nodes`, ...).
pub const ES_VERSION: &str = "8.15.0";
pub const LUCENE_VERSION: &str = "9.11.1";
pub const MIN_WIRE_COMPAT_VERSION: &str = "7.17.0";
pub const MIN_INDEX_COMPAT_VERSION: &str = "7.0.0";
pub const TAGLINE: &str = "You Know, for Search";
pub const BUILD_FLAVOR: &str = "default";
pub const BUILD_TYPE: &str = "docker";
pub const FERRITE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// `index.max_result_window` : au-dela, ES refuse la pagination profonde.
pub const MAX_RESULT_WINDOW: usize = 10_000;
