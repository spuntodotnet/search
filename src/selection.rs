//! Resoudre une **expression d'index** en index concrets.
//!
//! Une expression, c'est ce que le client ecrit entre les slashes de l'URL (ou
//! dans le tableau `index=[…]` d'un client officiel, que celui-ci recolle avec
//! des virgules) :
//!
//! | Expression | Ce qu'elle designe |
//! |---|---|
//! | `catalogue` | l'index de ce nom, ou l'alias de ce nom |
//! | `produits,marques` | les deux |
//! | `audits-2026.08.*` | tous les index dont le nom correspond |
//! | `_all`, `*` | tous les index |
//! | `audits-*,-audits-2026.07.*` | les premiers, moins les seconds |
//!
//! La resolution est **le seul endroit** ou ces formes sont interpretees : les
//! routes, elles, recoivent une liste d'index et n'ont plus a se poser la
//! question. C'est aussi ce qui garantit qu'un motif veut dire la meme chose
//! pour `_search`, pour `_refresh` et pour `DELETE`.

use std::sync::Arc;

use crate::engine::{Catalog, FerriteIndex};
use crate::error::{EsError, EsResult};
use crate::search::glob_match;

/// Les tolerances d'une resolution, telles qu'ES les nomme dans sa query
/// string.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Un nom concret absent est-il ignore, ou une erreur ?
    pub ignore_unavailable: bool,
    /// Un motif qui ne correspond a rien est-il licite ?
    pub allow_no_indices: bool,
    /// Les motifs designent-ils quelque chose ? (`expand_wildcards` d'ES : un
    /// client qui ne demande que les index **fermes** ne vise rien ici, ferrite
    /// n'en ayant aucun.)
    pub expansion: bool,
}

impl Default for Options {
    /// Les defauts d'Elasticsearch.
    fn default() -> Self {
        Self {
            ignore_unavailable: false,
            allow_no_indices: true,
            expansion: true,
        }
    }
}

/// Le resultat d'une resolution : les index vises, tries par nom.
///
/// L'ordre est stable **par construction**, et c'est une propriete dont la
/// recherche depend : c'est lui qui departage deux documents de meme score
/// venus d'index differents.
pub fn resoudre(
    catalog: &Catalog,
    expression: &str,
    opts: &Options,
) -> EsResult<Vec<Arc<FerriteIndex>>> {
    let items: Vec<&str> = expression
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if items.is_empty() {
        return Err(EsError::illegal_argument(
            "aucun index n'est designe : l'expression d'index est vide",
        ));
    }

    let mut retenus: Vec<String> = Vec::new();
    let mut a_utilise_un_motif = false;

    for item in items {
        // Un nom d'index ne peut pas commencer par `-` : un `-` en tete est
        // donc toujours une exclusion, jamais un nom.
        let (exclusion, motif) = match item.strip_prefix('-') {
            Some(reste) => (true, reste),
            None => (false, item),
        };
        if motif.is_empty() {
            return Err(EsError::illegal_argument(format!(
                "[{item}] : exclusion sans nom ni motif"
            )));
        }

        // Un nom reserve n'est pas un index absent : ES distingue les deux, et
        // c'est utile — `GET /_route_inconnue` doit dire « nom invalide », pas
        // laisser croire qu'il manque un index de ce nom.
        if !motif.contains('*') && motif != "_all" && motif.starts_with('_') {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "invalid_index_name_exception",
                format!("Invalid index name [{motif}], must not start with '_'."),
            )
            .with("index_uuid", serde_json::json!("_na_"))
            .with("index", serde_json::json!(motif)));
        }

        let designes = designer(catalog, motif, opts, &mut a_utilise_un_motif);
        let designes = match designes {
            Some(v) => v,
            None => {
                // Nom concret inconnu. A l'exclusion, il n'y a rien a retirer :
                // ce n'est pas une erreur, meme chez ES.
                if exclusion || opts.ignore_unavailable {
                    Vec::new()
                } else {
                    return Err(EsError::index_not_found(motif));
                }
            }
        };

        if exclusion {
            retenus.retain(|n| !designes.contains(n));
        } else {
            for nom in designes {
                if !retenus.contains(&nom) {
                    retenus.push(nom);
                }
            }
        }
    }

    if retenus.is_empty() && a_utilise_un_motif && !opts.allow_no_indices {
        return Err(EsError::index_not_found(expression));
    }

    retenus.sort();
    Ok(retenus.iter().filter_map(|n| catalog.brut(n)).collect())
}

/// Les index qu'un seul terme d'expression designe.
///
/// `None` signale un **nom concret inconnu** — le seul cas ou l'appelant a une
/// decision a prendre. Un motif qui ne correspond a rien rend `Some(vec![])` :
/// il a bien ete compris, il ne trouve simplement rien.
fn designer(
    catalog: &Catalog,
    motif: &str,
    opts: &Options,
    a_utilise_un_motif: &mut bool,
) -> Option<Vec<String>> {
    if motif == "_all" || motif == "*" {
        *a_utilise_un_motif = true;
        if !opts.expansion {
            return Some(Vec::new());
        }
        return Some(catalog.list().iter().map(|i| i.name.clone()).collect());
    }
    if motif.contains('*') {
        *a_utilise_un_motif = true;
        if !opts.expansion {
            return Some(Vec::new());
        }
        let mut noms: Vec<String> = catalog
            .list()
            .iter()
            .filter(|i| glob_match(motif, &i.name))
            .map(|i| i.name.clone())
            .collect();
        // Un motif correspond aussi aux **alias** : `audits-*` doit attraper un
        // alias `audits-chauds` comme ES le fait.
        for (alias, cibles) in catalog.aliases() {
            if glob_match(motif, &alias) {
                noms.extend(cibles.keys().cloned());
            }
        }
        return Some(noms);
    }
    // Nom concret : un index d'abord, un alias ensuite. Les deux ne peuvent pas
    // porter le meme nom (le catalogue le refuse a la creation).
    if let Some(idx) = catalog.brut(motif) {
        return Some(vec![idx.name.clone()]);
    }
    catalog
        .cibles_alias(motif)
        .map(|v| v.iter().map(|i| i.name.clone()).collect())
}

/// L'index unique designe par une expression, pour une operation qui n'en
/// accepte qu'un : `GET /{index}/_doc/{id}`, `_update`, `_delete`.
///
/// Un alias est suivi ; un motif ou une liste sont refuses, comme chez ES —
/// ecrire ou lire « quelque part parmi ces index » n'a pas de sens.
pub fn index_unique(catalog: &Catalog, expression: &str) -> EsResult<Arc<FerriteIndex>> {
    if expression.contains(',') || expression.contains('*') {
        return Err(EsError::illegal_argument(format!(
            "[{expression}] : cette operation porte sur un seul index ; les motifs et les listes \
             ne sont acceptes qu'a la recherche et sur les routes d'administration"
        )));
    }
    if let Some(idx) = catalog.brut(expression) {
        return Ok(idx);
    }
    match catalog.cibles_alias(expression) {
        None => catalog.get(expression),
        Some(cibles) if cibles.len() == 1 => Ok(cibles.into_iter().next().unwrap()),
        Some(cibles) => Err(EsError::illegal_argument(format!(
            "alias [{expression}] has more than one index associated with it [{}], can't execute a \
             single index op",
            cibles
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ))),
    }
}

/// L'index qui recoit une **ecriture** adressee a cette expression.
///
/// Contrairement a la lecture, l'ecriture cree l'index s'il n'existe pas encore
/// (`action.auto_create_index` d'ES). Un alias, lui, ne cree rien : il designe
/// son index d'ecriture, ou refuse s'il n'y en a pas un seul.
pub fn index_d_ecriture(catalog: &Catalog, expression: &str) -> EsResult<Arc<FerriteIndex>> {
    if expression.contains(',') || expression.contains('*') {
        return Err(EsError::illegal_argument(format!(
            "[{expression}] : une ecriture porte sur un seul index ; ni motif ni liste"
        )));
    }
    if catalog.est_alias(expression) {
        let nom = catalog.index_d_ecriture(expression)?;
        return catalog.get(&nom);
    }
    catalog.get_or_create(expression)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alias::Attache;
    use crate::engine::ActionAlias;
    use crate::mapping::Mapping;

    /// Un repertoire de donnees jetable, efface a la fin du test.
    struct Jetable(std::path::PathBuf);

    impl Drop for Jetable {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn catalogue(noms: &[&str]) -> (Jetable, Arc<Catalog>) {
        let dir = std::env::temp_dir().join(format!(
            "ferrite-selection-{}-{}",
            std::process::id(),
            crate::util::random_uuid()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let cat = Catalog::open(dir.clone(), "test".into(), "n1".into()).unwrap();
        for nom in noms {
            cat.create(nom, Mapping::default()).unwrap();
        }
        (Jetable(dir), cat)
    }

    fn noms(v: &[Arc<FerriteIndex>]) -> Vec<String> {
        v.iter().map(|i| i.name.clone()).collect()
    }

    /// `Result::unwrap_err` exige un `Debug` sur le succes ; un index n'en a
    /// pas, et lui en donner un pour un test serait la queue qui remue le chien.
    fn erreur(r: EsResult<Vec<Arc<FerriteIndex>>>) -> EsError {
        match r {
            Ok(v) => panic!("succes inattendu : {:?}", noms(&v)),
            Err(e) => e,
        }
    }

    #[test]
    fn liste_explicite() {
        let (_d, cat) = catalogue(&["produits", "marques", "clients"]);
        let r = resoudre(&cat, "produits,marques", &Options::default()).unwrap();
        assert_eq!(noms(&r), ["marques", "produits"]);
    }

    #[test]
    fn un_index_absent_de_la_liste_est_une_erreur() {
        let (_d, cat) = catalogue(&["produits"]);
        let e = erreur(resoudre(&cat, "produits,fantome", &Options::default()));
        assert_eq!(e.ty, "index_not_found_exception");
        let ok = resoudre(
            &cat,
            "produits,fantome",
            &Options {
                ignore_unavailable: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(noms(&ok), ["produits"]);
    }

    #[test]
    fn motif_et_exclusion() {
        let (_d, cat) = catalogue(&["audits-01", "audits-02", "audits-03", "produits"]);
        let r = resoudre(&cat, "audits-*,-audits-02", &Options::default()).unwrap();
        assert_eq!(noms(&r), ["audits-01", "audits-03"]);
    }

    #[test]
    fn un_motif_sans_correspondance_ne_trouve_rien_sans_echouer() {
        let (_d, cat) = catalogue(&["produits"]);
        let r = resoudre(&cat, "fantome-*", &Options::default()).unwrap();
        assert!(r.is_empty());
        let e = erreur(resoudre(
            &cat,
            "fantome-*",
            &Options {
                allow_no_indices: false,
                ..Options::default()
            },
        ));
        assert_eq!(e.ty, "index_not_found_exception");
    }

    #[test]
    fn un_alias_designe_ses_index() {
        let (_d, cat) = catalogue(&["audits-01", "audits-02", "produits"]);
        cat.modifier_alias(&[
            ActionAlias::Ajouter {
                index: "audits-01".into(),
                alias: "audits".into(),
                attache: Attache::default(),
            },
            ActionAlias::Ajouter {
                index: "audits-02".into(),
                alias: "audits".into(),
                attache: Attache::default(),
            },
        ])
        .unwrap();
        let r = resoudre(&cat, "audits", &Options::default()).unwrap();
        assert_eq!(noms(&r), ["audits-01", "audits-02"]);
        // Une operation a index unique refuse un alias qui en couvre deux.
        assert!(index_unique(&cat, "audits").is_err());
        // ... et l'ecriture aussi, tant qu'aucun index d'ecriture n'est designe.
        assert!(index_d_ecriture(&cat, "audits").is_err());
    }

    #[test]
    fn is_write_index_tranche_l_ecriture() {
        let (_d, cat) = catalogue(&["audits-01", "audits-02"]);
        cat.modifier_alias(&[
            ActionAlias::Ajouter {
                index: "audits-01".into(),
                alias: "audits".into(),
                attache: Attache::default(),
            },
            ActionAlias::Ajouter {
                index: "audits-02".into(),
                alias: "audits".into(),
                attache: Attache {
                    is_write_index: Some(true),
                },
            },
        ])
        .unwrap();
        assert_eq!(index_d_ecriture(&cat, "audits").unwrap().name, "audits-02");
    }

    #[test]
    fn supprimer_un_index_le_retire_de_ses_alias() {
        let (_d, cat) = catalogue(&["audits-01", "audits-02"]);
        for nom in ["audits-01", "audits-02"] {
            cat.modifier_alias(&[ActionAlias::Ajouter {
                index: nom.into(),
                alias: "audits".into(),
                attache: Attache::default(),
            }])
            .unwrap();
        }
        cat.delete("audits-01").unwrap();
        let r = resoudre(&cat, "audits", &Options::default()).unwrap();
        assert_eq!(noms(&r), ["audits-02"]);
        cat.delete("audits-02").unwrap();
        // Plus aucune cible : l'alias lui-meme a disparu.
        assert!(!cat.est_alias("audits"));
    }
}
