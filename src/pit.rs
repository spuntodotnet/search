//! Le **point-in-time** : une vue figee de l'index, qu'on interroge autant de
//! fois qu'on veut.
//!
//! C'est ce que la 8.x recommande a la place du `scroll`, et la difference
//! entre les deux n'est pas cosmetique. Un `scroll` fige un **resultat** : la
//! requete est jouee une fois, le classement est calcule une fois, et les pages
//! suivantes sont des tranches de ce tableau — donc il n'y a qu'une requete,
//! qu'un tri, et le curseur vit dans le serveur. Un PIT fige un **lecteur** :
//! il ne connait aucune requete, et chaque recherche qu'on lui pose est une
//! recherche complete sur l'instantane. C'est ce qui permet de changer de
//! requete, de tri, de taille de page entre deux appels, et c'est pour ca que
//! le curseur, lui, vit chez le client (`search_after`).
//!
//! La consequence pratique est qu'un PIT ne retient que trois choses par index
//! vise : son nom, sa generation, et **le `Searcher` tantivy du moment**. Un
//! `Searcher` est un instantane — les ecritures commitees ensuite ne le
//! changent pas, et il empeche tantivy de recycler les fichiers qu'il designe.
//! C'est exactement la propriete dont depend la promesse d'ES, et c'est aussi
//! son prix : d'ou le `keep_alive` obligatoire, la purge, et
//! `DELETE /_pit`.
//!
//! Mesure contre ES 8.15, qui a decide la forme de ce module : ouvrir un PIT,
//! ecrire un document, puis compter **sous** le PIT rend l'ancien total, quand
//! la meme recherche hors PIT rend le nouveau. Un identifiant qu'on rendrait
//! sans rien retenir serait le pire des deux mondes — le client croit paginer
//! sur une vue stable et lit un index qui bouge.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tantivy::Searcher;

use crate::engine::Generation;
use crate::error::{EsError, EsResult};

/// `search.max_open_pit_context` d'ES. Meme raison que pour le scroll : chaque
/// contexte retient un instantane, donc des fichiers que tantivy ne peut pas
/// recycler tant qu'il vit.
pub const MAX_CONTEXTES: usize = 500;

/// Un index tel qu'un PIT le retient.
///
/// L'`uuid` sert aux echecs de shard (ES les nomme par index **et** par uuid),
/// et il est fige avec le reste : un index recree sous le meme nom pendant la
/// vie du PIT n'est pas le meme index.
#[derive(Clone)]
pub struct CibleFigee {
    pub nom: String,
    pub uuid: String,
    pub gen: Arc<Generation>,
    pub searcher: Searcher,
}

pub struct Contexte {
    pub cibles: Vec<CibleFigee>,
    /// Pose par [`Registre::ouvrir`], repousse a chaque recherche qui porte un
    /// `keep_alive`.
    pub expire: Instant,
}

#[derive(Default)]
pub struct Registre {
    contextes: Mutex<HashMap<String, Contexte>>,
}

impl Registre {
    /// Ouvre un contexte et rend son identifiant.
    ///
    /// L'identifiant est **opaque**, comme celui d'ES (qui y encode ses shards
    /// en base64) : un client ne doit rien en deduire, seulement le rendre tel
    /// quel. La consequence se mesure et elle est declaree : un identifiant mal
    /// forme est ici un contexte introuvable (404), la ou ES echoue au decodage
    /// (400).
    pub fn ouvrir(&self, mut ctx: Contexte, keep_alive: Duration) -> EsResult<String> {
        let mut contextes = self.contextes.lock().expect("pit lock");
        purger(&mut contextes);
        if contextes.len() >= MAX_CONTEXTES {
            return Err(EsError::illegal_argument(format!(
                "Trying to create too many point in time contexts. Must be less than or equal to: \
                 [{MAX_CONTEXTES}]. This limit can be set by changing the \
                 [search.max_open_pit_context] setting."
            )));
        }
        ctx.expire = Instant::now() + keep_alive;
        let id = crate::util::random_uuid();
        contextes.insert(id.clone(), ctx);
        Ok(id)
    }

    /// Les index figes d'un contexte, et le repoussement de son expiration.
    ///
    /// Le `Searcher` est clone : la recherche se fait **sans** tenir le verrou,
    /// comme pour une page de scroll.
    pub fn lire(&self, id: &str, keep_alive: Option<Duration>) -> EsResult<Vec<CibleFigee>> {
        let mut contextes = self.contextes.lock().expect("pit lock");
        purger(&mut contextes);
        let ctx = contextes.get_mut(id).ok_or_else(|| contexte_absent(id))?;
        if let Some(d) = keep_alive {
            ctx.expire = Instant::now() + d;
        }
        Ok(ctx.cibles.clone())
    }

    /// Ferme le contexte nomme ; rend `true` s'il existait encore.
    pub fn fermer(&self, id: &str) -> bool {
        self.contextes
            .lock()
            .expect("pit lock")
            .remove(id)
            .is_some()
    }

    /// Oublie les contextes expires. Appele a chaque operation, et
    /// periodiquement par le serveur.
    pub fn purger(&self) {
        purger(&mut self.contextes.lock().expect("pit lock"));
    }

    pub fn ouverts(&self) -> usize {
        self.contextes.lock().expect("pit lock").len()
    }
}

fn purger(contextes: &mut HashMap<String, Contexte>) {
    let maintenant = Instant::now();
    contextes.retain(|_, c| c.expire > maintenant);
}

/// L'erreur d'ES quand le contexte n'existe plus : expire, deja ferme, ou jamais
/// ouvert.
///
/// C'est **le meme** empilement que pour un scroll expire, et c'est voulu : les
/// clients reconnaissent `search_context_missing_exception` pour dire « ta vue a
/// expire, recommence », plutot que « ta requete est invalide ».
pub fn contexte_absent(id: &str) -> EsError {
    let cause = serde_json::json!({
        "type": "search_context_missing_exception",
        "reason": format!("No search context found for id [{id}]"),
    });
    EsError::new(
        axum::http::StatusCode::NOT_FOUND,
        "search_phase_execution_exception",
        "all shards failed",
    )
    .with("phase", serde_json::json!("query"))
    .with("grouped", serde_json::json!(true))
    .with(
        "failed_shards",
        serde_json::json!([{"shard": -1, "index": serde_json::Value::Null, "reason": cause.clone()}]),
    )
    .with("caused_by", cause.clone())
    .avec_racines(vec![cause])
}

/// Le `Registre` partage par le serveur.
pub type RegistrePartage = Arc<Registre>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contexte_absent_est_un_404_de_la_meme_forme_qu_un_scroll() {
        let e = contexte_absent("abc");
        assert_eq!(e.status, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(e.ty, "search_phase_execution_exception");
        let body = e.body();
        assert_eq!(
            body["error"]["root_cause"][0]["type"],
            "search_context_missing_exception"
        );
    }

    #[test]
    fn un_contexte_expire_ne_se_lit_plus() {
        let r = Registre::default();
        let ctx = Contexte {
            cibles: Vec::new(),
            expire: Instant::now(),
        };
        let id = r.ouvrir(ctx, Duration::from_millis(0)).unwrap();
        // `ouvrir` pose l'expiration a `maintenant + 0` : elle est deja passee.
        assert!(r.lire(&id, None).is_err());
        assert_eq!(r.ouverts(), 0);
    }

    #[test]
    fn fermer_deux_fois_ne_libere_qu_une_fois() {
        let r = Registre::default();
        let id = r
            .ouvrir(
                Contexte {
                    cibles: Vec::new(),
                    expire: Instant::now(),
                },
                Duration::from_secs(60),
            )
            .unwrap();
        assert!(r.fermer(&id));
        assert!(!r.fermer(&id));
    }
}
