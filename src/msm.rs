//! `minimum_should_match` : combien de clauses `should` doivent etre satisfaites.
//!
//! Elasticsearch accepte quatre notations sous ce nom, et elles ne se lisent
//! pas de la meme facon :
//!
//! - un **entier positif** (`3`) : le nombre exige, tel quel ;
//! - un **entier negatif** (`-1`) : le nombre de clauses qu'on accepte de
//!   manquer, compte a partir du total ;
//! - un **pourcentage** (`75%`, `-25%`) : la fraction du total, tronquee ;
//! - une suite de **conditions** separees par une espace (`3<90%`,
//!   `2<-25% 9<-3`) : « tant qu'il y a au plus N clauses, on les exige toutes ;
//!   au-dela, applique cette formule ».
//!
//! Chaque regle ci-dessous vient d'une mesure contre un vrai Elasticsearch
//! 8.15 (`tests/compat/sonde_msm.py`), pas de la documentation, parce que les
//! bords sont exactement ce qu'elle ne dit pas :
//!
//! - l'arrondi est une **troncature vers zero**, pas un plancher : `-99%` de 3
//!   ne vaut pas 3 - 2,97 = 0 mais 3 - 2 = 1... et de 3 clauses il vaut 3,
//!   puisque -0,99 tronque vaut 0. C'est le `(int)` de Java, et il faut donc
//!   regarder le signe du **produit**, pas celui du quotient ;
//! - un minimum **superieur** au nombre de clauses n'est pas ramene a ce
//!   nombre : `150%` ou `5` sur 4 clauses ne rendent aucun document (mesure : ES
//!   ne plafonne pas, contrairement a ce que suggere son propre code
//!   historique) ;
//! - le separateur de la forme combinee est l'**espace**, pas la virgule :
//!   `2<25%,9<3` est une erreur ;
//! - le `%` est suppose etre le **dernier caractere** : `75%x` n'est pas 75%,
//!   c'est une erreur ;
//! - une clause `should` qui porte sur un champ non mappe **compte quand
//!   meme** dans le total : `100%` sur trois champs connus plus un inconnu ne
//!   rend rien.
//!
//! Le pire resultat possible ici serait d'ignorer le parametre : la requete
//! rendrait alors *plus* de documents que prevu, sans rien signaler. Toute
//! notation qui n'est pas comprise est donc refusee en 400.

use serde_json::Value;

use crate::error::{EsError, EsResult};

/// Resout la valeur de `minimum_should_match` en nombre de clauses `should` a
/// satisfaire.
///
/// `should` est le nombre total de clauses `should` de la requete, `defaut` la
/// valeur a rendre quand le parametre est absent (ou `null`, qu'ES traite comme
/// absent).
pub fn resoudre(spec: Option<&Value>, should: usize, defaut: usize) -> EsResult<usize> {
    let texte = match spec {
        None | Some(Value::Null) => return Ok(defaut),
        // ES lit la valeur comme du texte quel que soit le type JSON : un
        // nombre flottant y arrive donc sous la forme « 1.5 » et echoue a la
        // conversion, comme ici.
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => {
            return Err(EsError::illegal_argument(format!(
                "[minimum_should_match] : entier ou chaine attendus (recu {v})"
            )))
        }
    };
    calculer(&texte, should)
}

fn calculer(spec: &str, should: usize) -> EsResult<usize> {
    let spec = spec.trim();
    if !spec.contains('<') {
        return formule(spec, should, spec);
    }
    // Forme combinee. Les conditions se lisent de gauche a droite ; la premiere
    // borne non franchie arrete la lecture et rend ce qui a ete retenu jusque
    // la — donc le total lui-meme si c'est la premiere condition qui bloque.
    let mut retenu = should;
    for condition in spec.split(' ') {
        let (borne, reste) = condition
            .split_once('<')
            .ok_or_else(|| invalide(spec, condition))?;
        if should as i64 <= i64::from(entier(borne, spec)?) {
            return Ok(retenu);
        }
        retenu = formule(reste, should, spec)?;
    }
    Ok(retenu)
}

/// Une formule simple : `3`, `-1`, `75%`, `-25%`.
fn formule(texte: &str, should: usize, spec: &str) -> EsResult<usize> {
    let total = should as i64;
    let brut = if texte.contains('%') {
        let pourcentage = i64::from(entier(sans_le_dernier_caractere(texte), spec)?);
        let produit = total * pourcentage;
        // Java tronque vers zero (`(int)` sur un `float`), et teste le signe
        // avant de tronquer : c'est le produit qui dit si le pourcentage se
        // compte a partir de zero ou a partir du total.
        let quotient = produit / 100;
        if produit < 0 {
            total + quotient
        } else {
            quotient
        }
    } else {
        let n = i64::from(entier(texte, spec)?);
        if n < 0 {
            total + n
        } else {
            n
        }
    };
    // Seul le bas est borne : un minimum superieur au nombre de clauses reste
    // tel quel, et ne rend donc aucun document (mesure).
    Ok(brut.max(0) as usize)
}

fn entier(texte: &str, spec: &str) -> EsResult<i32> {
    texte.parse::<i32>().map_err(|_| invalide(spec, texte))
}

/// `spec.substring(0, spec.length() - 1)` d'ES : le `%` est suppose etre le
/// dernier caractere, et tout ce qui l'entoure part avec lui.
fn sans_le_dernier_caractere(texte: &str) -> &str {
    let mut chars = texte.chars();
    chars.next_back();
    chars.as_str()
}

fn invalide(spec: &str, fautif: &str) -> EsError {
    EsError::illegal_argument(format!(
        "[minimum_should_match] : [{spec}] n'est pas une valeur valide (a [{fautif}]) ; formes \
         acceptees : un entier ([3], [-1]), un pourcentage ([75%], [-25%]), ou des conditions \
         separees par une espace ([3<90%])"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn r(spec: Value, should: usize) -> EsResult<usize> {
        resoudre(Some(&spec), should, 0)
    }

    /// Chaque attendu de ce test a ete constate sur un ES 8.15
    /// (`tests/compat/sonde_msm.py`).
    #[test]
    fn pourcentages_mesures_contre_es() {
        assert_eq!(r(json!("75%"), 4).unwrap(), 3);
        assert_eq!(r(json!("70%"), 4).unwrap(), 2); // 2,8 tronque
        assert_eq!(r(json!("76%"), 4).unwrap(), 3);
        assert_eq!(r(json!("100%"), 4).unwrap(), 4);
        assert_eq!(r(json!("0%"), 4).unwrap(), 0);
        assert_eq!(r(json!("50%"), 3).unwrap(), 1); // 1,5 tronque
        assert_eq!(r(json!("66%"), 3).unwrap(), 1);
        assert_eq!(r(json!("67%"), 3).unwrap(), 2);
        assert_eq!(r(json!("+75%"), 4).unwrap(), 3);
        // Au-dela du total : garde tel quel, donc aucun document.
        assert_eq!(r(json!("150%"), 4).unwrap(), 6);
    }

    #[test]
    fn negatifs_comptes_a_partir_du_total() {
        assert_eq!(r(json!("-25%"), 4).unwrap(), 3);
        assert_eq!(r(json!("-50%"), 4).unwrap(), 2);
        // -3,96 tronque vaut -3, pas -4 : le plancher serait faux.
        assert_eq!(r(json!("-99%"), 4).unwrap(), 1);
        // -0,99 tronque vaut 0 : les trois clauses restent exigees.
        assert_eq!(r(json!("-33%"), 3).unwrap(), 3);
        assert_eq!(r(json!(-1), 4).unwrap(), 3);
        assert_eq!(r(json!(-2), 4).unwrap(), 2);
        // Le bas est borne a zero.
        assert_eq!(r(json!("-200%"), 4).unwrap(), 0);
        assert_eq!(r(json!(-9), 4).unwrap(), 0);
    }

    #[test]
    fn entiers() {
        assert_eq!(r(json!(2), 4).unwrap(), 2);
        assert_eq!(r(json!(0), 4).unwrap(), 0);
        assert_eq!(r(json!("75"), 4).unwrap(), 75);
        assert_eq!(r(json!(5), 4).unwrap(), 5);
    }

    #[test]
    fn conditions_combinees() {
        // 4 clauses : la borne 3 est franchie, donc 90% de 4 = 3,6 -> 3.
        assert_eq!(r(json!("3<90%"), 4).unwrap(), 3);
        // 3 clauses : la borne n'est pas franchie, donc tout est exige.
        assert_eq!(r(json!("3<90%"), 3).unwrap(), 3);
        assert_eq!(r(json!("2<90%"), 3).unwrap(), 2);
        assert_eq!(r(json!("1<2"), 4).unwrap(), 2);
        assert_eq!(r(json!("2<-25% 9<-3"), 4).unwrap(), 3);
        assert_eq!(r(json!("5<2 8<3"), 3).unwrap(), 3);
        assert_eq!(r(json!("3<-1"), 3).unwrap(), 3);
    }

    #[test]
    fn absent_ou_null_rend_le_defaut() {
        assert_eq!(resoudre(None, 4, 1).unwrap(), 1);
        assert_eq!(resoudre(Some(&Value::Null), 4, 1).unwrap(), 1);
    }

    #[test]
    fn notations_refusees_plutot_que_mal_lues() {
        for spec in [
            json!("abc"),
            json!(""),
            json!("75.5%"),
            json!(1.5),
            json!(true),
            json!(["75%"]),
            json!("2<25%,9<3"),    // la virgule n'est pas un separateur
            json!("1<70%  3<50%"), // espace double : une condition vide
            json!("1<"),
            json!("<2"),
            json!("75%x"), // le % doit etre le dernier caractere
            json!(" 75 % "),
        ] {
            let e = r(spec.clone(), 4).unwrap_err();
            assert_eq!(e.status, axum::http::StatusCode::BAD_REQUEST, "{spec}");
            assert!(e.reason.contains("minimum_should_match"), "{spec}");
        }
    }

    #[test]
    fn le_pourcentage_ne_panique_pas_sur_de_l_unicode() {
        assert!(r(json!("%é"), 4).is_err());
        assert!(r(json!("é%"), 4).is_err());
    }
}
