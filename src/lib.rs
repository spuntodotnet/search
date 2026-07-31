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
//! - [`engine`] : le catalogue d'index, l'ecriture, la lecture, la persistance.
//!   Ne connait pas HTTP.
//! - [`search`] : l'execution d'une recherche et la mise en forme du resultat au
//!   format ES.
//! - [`api`] : la couche HTTP — routage, parametres, format de reponse et
//!   d'erreur. Ne contient aucune logique de moteur.

pub mod api;
pub mod dismax;
pub mod dsl;
pub mod engine;
pub mod error;
pub mod mapping;
pub mod search;
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
