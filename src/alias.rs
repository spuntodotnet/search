//! Les alias d'index : un nom de plus pour un ou plusieurs index.
//!
//! C'est ce qui rend une famille d'index quotidiens (`audits-2026.08.01`,
//! `audits-2026.08.02`, …) interrogeable sous un nom stable. Le code client
//! n'ecrit et ne lit que `audits` : le decoupage est une affaire
//! d'exploitation, pas d'application. Sans alias, changer de decoupage oblige a
//! changer le code — c'est precisement ce que ferrite promet d'eviter.
//!
//! Le registre est persiste a la racine des donnees (`_aliases.json`) : un nom
//! commencant par `_` ne peut pas etre un index, il n'y a donc pas de
//! collision possible avec un repertoire d'index.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::error::{EsError, EsResult};

pub const FICHIER: &str = "_aliases.json";

/// Ce qu'un alias ajoute a un index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attache {
    /// Designe l'index qui recoit les ecritures quand l'alias en couvre
    /// plusieurs. `None` : non precise.
    pub is_write_index: Option<bool>,
}

impl Attache {
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        if let Some(w) = self.is_write_index {
            o.insert("is_write_index".into(), json!(w));
        }
        Value::Object(o)
    }
}

/// `alias -> index -> attache`. Les deux niveaux sont ordonnes : la reponse
/// d'ES l'est aussi, et un ordre stable rend les diffs comparables.
pub type Registre = BTreeMap<String, BTreeMap<String, Attache>>;

/// Les proprietes acceptees dans le corps d'une declaration d'alias.
///
/// `filter`, `routing`, `index_routing` et `search_routing` sont refuses
/// explicitement : les accepter sans les appliquer rendrait des documents que
/// l'alias est justement cense cacher.
pub fn lire_attache(v: &Value, quoi: &str) -> EsResult<Attache> {
    let obj = match v {
        Value::Null => return Ok(Attache::default()),
        Value::Object(o) => o,
        _ => {
            return Err(EsError::parsing(format!(
                "[{quoi}] : la declaration d'un alias doit etre un objet"
            )))
        }
    };
    for cle in obj.keys() {
        match cle.as_str() {
            "is_write_index" => {}
            "filter" => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas [filter] sur un alias (dans [{quoi}]) : un alias \
                     filtre qui n'appliquerait pas son filtre rendrait les documents qu'il est \
                     cense cacher (voir docs/compat.md)"
                )))
            }
            "routing" | "index_routing" | "search_routing" => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas [{cle}] sur un alias (dans [{quoi}]) : ferrite est \
                     mono-shard, le routage n'a rien a choisir"
                )))
            }
            autre => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas [{autre}] dans la declaration d'un alias (dans \
                     [{quoi}]) ; cles acceptees : is_write_index"
                )))
            }
        }
    }
    let is_write_index = match obj.get("is_write_index") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => {
            return Err(EsError::illegal_argument(format!(
                "[{quoi}.is_write_index] : booleen attendu"
            )))
        }
    };
    Ok(Attache { is_write_index })
}

/// Les regles de nommage d'un alias. Elles sont celles d'un index, moins la
/// contrainte de repertoire : un alias n'existe que dans le registre.
pub fn valider_nom(nom: &str) -> EsResult<()> {
    let invalide = |raison: &str| {
        Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_alias_name_exception",
            format!("Invalid alias name [{nom}], {raison}"),
        )
        .with("index_uuid", json!("_na_"))
        .with("index", json!(nom)))
    };
    if nom.is_empty() {
        return invalide("must not be empty");
    }
    if nom == "." || nom == ".." {
        return invalide("must not be '.' or '..'");
    }
    if nom.starts_with('_') {
        return invalide("must not start with '_'.");
    }
    const INTERDITS: &[char] = &[
        '\\', '/', '*', '?', '"', '<', '>', '|', ' ', ',', '#', ':', '-', '+',
    ];
    if let Some(c) = nom.chars().find(|c| INTERDITS.contains(c)) {
        // `-` et `+` ne sont interdits qu'en tete chez ES ; ailleurs ils sont
        // licites, et les noms d'alias en contiennent souvent.
        if (c == '-' || c == '+') && !nom.starts_with(c) {
            return Ok(());
        }
        return invalide(&format!("must not contain the following characters [{c}]"));
    }
    Ok(())
}

pub fn charger(racine: &Path) -> Registre {
    let Ok(raw) = fs::read(racine.join(FICHIER)) else {
        return Registre::new();
    };
    let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
        return Registre::new();
    };
    depuis_json(&v)
}

pub fn depuis_json(v: &Value) -> Registre {
    let mut registre = Registre::new();
    let Some(obj) = v.as_object() else {
        return registre;
    };
    for (alias, cibles) in obj {
        let Some(cibles) = cibles.as_object() else {
            continue;
        };
        let mut entree = BTreeMap::new();
        for (index, attache) in cibles {
            entree.insert(
                index.clone(),
                Attache {
                    is_write_index: attache.get("is_write_index").and_then(Value::as_bool),
                },
            );
        }
        if !entree.is_empty() {
            registre.insert(alias.clone(), entree);
        }
    }
    registre
}

pub fn vers_json(registre: &Registre) -> Value {
    let mut out = Map::new();
    for (alias, cibles) in registre {
        let mut o = Map::new();
        for (index, attache) in cibles {
            o.insert(index.clone(), attache.to_json());
        }
        out.insert(alias.clone(), Value::Object(o));
    }
    Value::Object(out)
}

/// Ecrit le registre de facon atomique : il fait foi au redemarrage, donc il ne
/// doit jamais etre a moitie ecrit.
pub fn enregistrer(racine: &Path, registre: &Registre) -> EsResult<()> {
    let tmp = racine.join(format!("{FICHIER}.tmp"));
    fs::write(
        &tmp,
        serde_json::to_vec_pretty(&vers_json(registre)).unwrap(),
    )
    .map_err(|e| EsError::internal(format!("ecriture des alias: {e}")))?;
    fs::rename(&tmp, racine.join(FICHIER))
        .map_err(|e| EsError::internal(format!("bascule des alias: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aller_retour_json() {
        let mut r = Registre::new();
        let mut cibles = BTreeMap::new();
        cibles.insert("audits-2026.08.01".to_string(), Attache::default());
        cibles.insert(
            "audits-2026.08.02".to_string(),
            Attache {
                is_write_index: Some(true),
            },
        );
        r.insert("audits".to_string(), cibles);
        assert_eq!(depuis_json(&vers_json(&r)), r);
    }

    #[test]
    fn un_alias_filtre_est_refuse_plutot_qu_ignore() {
        let e = lire_attache(&json!({"filter": {"term": {"a": 1}}}), "test").unwrap_err();
        assert_eq!(e.ty, crate::error::UNSUPPORTED);
    }

    #[test]
    fn is_write_index_est_lu() {
        let a = lire_attache(&json!({"is_write_index": true}), "test").unwrap();
        assert_eq!(a.is_write_index, Some(true));
    }

    #[test]
    fn un_tiret_interne_est_licite() {
        assert!(valider_nom("mes-audits").is_ok());
        assert!(valider_nom("-audits").is_err());
        assert!(valider_nom("audits*").is_err());
    }
}
