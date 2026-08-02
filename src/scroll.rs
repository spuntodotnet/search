//! `scroll` : la pagination par **contexte fige** d'Elasticsearch.
//!
//! Ce n'est pas de la pagination : `from` / `size` rejouent la requete a chaque
//! page, sur un index qui a pu changer entre-temps, et refusent d'aller au-dela
//! de `max_result_window`. Un export, lui, doit voir chaque document une fois et
//! une seule, quoi qu'il arrive a l'index pendant ce temps — c'est ce que
//! `scroll` promet, et c'est ce dont se sert `helpers.scan` du client officiel.
//!
//! ferrite tient cette promesse en figeant deux choses a l'ouverture :
//!
//! - **l'ordre**, en balayant tout ce qui correspond une fois pour toutes
//!   ([`crate::search::balayer`]) : les pages suivantes sont des tranches de ce
//!   tableau, donc la Nieme page ne coute pas N recherches ;
//! - **les documents**, en gardant le `Searcher` tantivy du moment. Un
//!   `Searcher` est un instantane : les ecritures commitees ensuite ne le
//!   changent pas. Sans lui, un commit pendant l'export renumeroterait les
//!   segments et les adresses figees ne designeraient plus les memes documents.
//!
//! Le prix de cette promesse est un contexte vivant cote serveur : d'ou le
//! `keep_alive` (`?scroll=1m`), la purge des contextes expires, et
//! `DELETE /_search/scroll` que tout client bien eleve appelle a la fin.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{EsError, EsResult};
use crate::search::{CibleFigee, HitFige, SourceFilter};

/// `search.max_open_scroll_context` d'ES : au-dela, ouvrir un contexte de plus
/// est refuse. Un contexte retient un instantane de l'index — donc des fichiers
/// que tantivy ne peut pas recycler tant qu'il vit.
pub const MAX_CONTEXTES: usize = 500;

/// `search.max_keep_alive` d'ES.
pub const MAX_KEEP_ALIVE: Duration = Duration::from_secs(24 * 3600);

/// Un contexte de scroll ouvert : le resultat fige, et ou en est la lecture.
pub struct Contexte {
    pub cibles: Vec<CibleFigee>,
    pub hits: Vec<HitFige>,
    pub total: usize,
    pub max_score: Option<f32>,
    pub source: SourceFilter,
    /// Un tri explicite a-t-il ete demande ? (le tableau `sort` du hit, et
    /// `max_score: null`, en dependent — comme dans une recherche normale)
    pub trie: bool,
    pub avec_score: bool,
    /// Taille d'une page, figee a l'ouverture comme chez ES : les appels
    /// suivants ne la renegocient pas.
    pub taille: usize,
    pub position: usize,
    /// De quoi rendre le meme `_shards` a chaque page que la premiere reponse.
    pub nb_index: usize,
    pub echecs: Vec<Value>,
    /// Pose par [`Registre::ouvrir`], repousse a chaque page.
    pub expire: Instant,
}

/// Ce qu'il faut pour rendre la page suivante, extrait sous le verrou pour que
/// la lecture des documents se fasse **sans** le tenir.
pub struct Suite {
    pub cibles: Vec<CibleFigee>,
    pub hits: Vec<HitFige>,
    pub total: usize,
    pub max_score: Option<f32>,
    pub source: SourceFilter,
    pub trie: bool,
    pub avec_score: bool,
    pub nb_index: usize,
    pub echecs: Vec<Value>,
}

#[derive(Default)]
pub struct Registre {
    contextes: Mutex<HashMap<String, Contexte>>,
}

impl Registre {
    /// Ouvre un contexte et rend son identifiant.
    ///
    /// L'identifiant est opaque, comme celui d'ES (qui y encode ses shards) :
    /// un client ne doit rien en deduire, seulement le rendre tel quel.
    pub fn ouvrir(&self, mut ctx: Contexte, keep_alive: Duration) -> EsResult<String> {
        let mut contextes = self.contextes.lock().expect("scroll lock");
        purger(&mut contextes);
        if contextes.len() >= MAX_CONTEXTES {
            return Err(EsError::illegal_argument(format!(
                "Trying to create too many scroll contexts. Must be less than or equal to: \
                 [{MAX_CONTEXTES}]. This limit can be set by changing the \
                 [search.max_open_scroll_context] setting."
            )));
        }
        ctx.expire = Instant::now() + keep_alive;
        let id = crate::util::random_uuid();
        contextes.insert(id.clone(), ctx);
        Ok(id)
    }

    /// Avance d'une page et rend de quoi la construire.
    pub fn avancer(&self, id: &str, keep_alive: Option<Duration>) -> EsResult<Suite> {
        let mut contextes = self.contextes.lock().expect("scroll lock");
        purger(&mut contextes);
        let ctx = contextes.get_mut(id).ok_or_else(|| contexte_absent(id))?;
        if let Some(d) = keep_alive {
            ctx.expire = Instant::now() + d;
        }
        let debut = ctx.position.min(ctx.hits.len());
        let fin = (debut + ctx.taille).min(ctx.hits.len());
        ctx.position = fin;
        Ok(Suite {
            cibles: ctx.cibles.clone(),
            hits: ctx.hits[debut..fin].to_vec(),
            total: ctx.total,
            max_score: ctx.max_score,
            source: ctx.source.clone(),
            trie: ctx.trie,
            avec_score: ctx.avec_score,
            nb_index: ctx.nb_index,
            echecs: ctx.echecs.clone(),
        })
    }

    /// Ferme les contextes nommes ; rend combien l'etaient vraiment.
    pub fn fermer(&self, ids: &[String]) -> usize {
        let mut contextes = self.contextes.lock().expect("scroll lock");
        if ids.iter().any(|id| id == "_all") {
            let n = contextes.len();
            contextes.clear();
            return n;
        }
        ids.iter()
            .filter(|id| contextes.remove(*id).is_some())
            .count()
    }

    /// Oublie les contextes expires. Appele a chaque operation de scroll, et
    /// periodiquement par le serveur : un contexte retient un instantane de
    /// l'index, personne ne doit pouvoir l'oublier ouvert indefiniment.
    pub fn purger(&self) {
        purger(&mut self.contextes.lock().expect("scroll lock"));
    }

    pub fn ouverts(&self) -> usize {
        self.contextes.lock().expect("scroll lock").len()
    }
}

fn purger(contextes: &mut HashMap<String, Contexte>) {
    let maintenant = Instant::now();
    contextes.retain(|_, c| c.expire > maintenant);
}

/// L'erreur d'ES quand le contexte n'existe plus : expire, deja ferme, ou jamais
/// ouvert. Statut **404**, et le meme empilement de causes que chez lui — c'est
/// ce que les clients reconnaissent pour dire « ton scroll a expire », plutot
/// que « ta requete est invalide ».
fn contexte_absent(id: &str) -> EsError {
    let cause = json!({
        "type": "search_context_missing_exception",
        "reason": format!("No search context found for id [{id}]"),
    });
    EsError::new(
        axum::http::StatusCode::NOT_FOUND,
        "search_phase_execution_exception",
        "all shards failed",
    )
    .with("phase", json!("query"))
    .with("grouped", json!(true))
    .with(
        "failed_shards",
        json!([{"shard": -1, "index": Value::Null, "reason": cause.clone()}]),
    )
    .with("caused_by", cause.clone())
    .avec_racines(vec![cause])
}

/// Lit une duree au format d'ES (`30s`, `1m`, `2h`, `500ms`...).
///
/// Le message d'erreur est celui d'ES, mot pour mot : un client qui l'affiche
/// doit dire la meme chose des deux cotes.
pub fn duree(valeur: &str, parametre: &str) -> EsResult<Duration> {
    let invalide = || {
        EsError::illegal_argument(format!(
            "failed to parse setting [{parametre}] with value [{valeur}] as a time value: unit is \
             missing or unrecognized"
        ))
    };
    let brut = valeur.trim();
    let coupe = brut.len()
        - brut
            .trim_end_matches(|c: char| c.is_ascii_alphabetic())
            .len();
    let (nombre, unite) = brut.split_at(brut.len() - coupe);
    let nombre: u64 = nombre.trim().parse().map_err(|_| invalide())?;
    let d = match unite {
        "d" => Duration::from_secs(nombre.saturating_mul(86_400)),
        "h" => Duration::from_secs(nombre.saturating_mul(3_600)),
        "m" => Duration::from_secs(nombre.saturating_mul(60)),
        "s" => Duration::from_secs(nombre),
        "ms" => Duration::from_millis(nombre),
        "micros" => Duration::from_micros(nombre),
        "nanos" => Duration::from_nanos(nombre),
        _ => return Err(invalide()),
    };
    if d > MAX_KEEP_ALIVE {
        return Err(EsError::illegal_argument(format!(
            "Keep alive for request ({valeur}) is too large. It must be less than ({}). This limit \
             can be set by changing the [search.max_keep_alive] cluster level setting.",
            "1d"
        )));
    }
    Ok(d)
}

/// L'identifiant de scroll d'un corps de requete, sous ses deux ecritures :
/// une chaine, ou une liste (le client officiel envoie une liste a
/// `clear_scroll`).
pub fn ids_du_corps(v: &Value, cle: &str) -> EsResult<Vec<String>> {
    match v {
        Value::String(s) => Ok(vec![s.clone()]),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                x.as_str().map(str::to_string).ok_or_else(|| {
                    EsError::illegal_argument(format!("[{cle}] : chaines attendues"))
                })
            })
            .collect(),
        _ => Err(EsError::illegal_argument(format!(
            "[{cle}] : chaine ou liste de chaines attendue"
        ))),
    }
}

/// Le `Registre` partage par le serveur.
pub type RegistrePartage = Arc<Registre>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durees_au_format_es() {
        assert_eq!(duree("1m", "scroll").unwrap(), Duration::from_secs(60));
        assert_eq!(duree("30s", "scroll").unwrap(), Duration::from_secs(30));
        assert_eq!(duree("2h", "scroll").unwrap(), Duration::from_secs(7200));
        assert_eq!(
            duree("500ms", "scroll").unwrap(),
            Duration::from_millis(500)
        );
        assert_eq!(duree("1d", "scroll").unwrap(), MAX_KEEP_ALIVE);
    }

    /// Une duree sans unite est le piege classique (`scroll=1`) : ES la refuse,
    /// et le message le dit.
    #[test]
    fn duree_sans_unite_refusee() {
        for mauvais in ["1", "xx", "", "1y", "m"] {
            let e = duree(mauvais, "scroll").unwrap_err();
            assert!(e.reason.contains("unit is missing"), "{}", e.reason);
        }
    }

    #[test]
    fn duree_trop_longue_refusee() {
        let e = duree("2d", "scroll").unwrap_err();
        assert!(e.reason.contains("too large"), "{}", e.reason);
    }

    #[test]
    fn contexte_absent_est_un_404() {
        let e = contexte_absent("abc");
        assert_eq!(e.status, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(e.ty, "search_phase_execution_exception");
        let body = e.body();
        assert_eq!(
            body["error"]["root_cause"][0]["type"],
            "search_context_missing_exception"
        );
    }
}
