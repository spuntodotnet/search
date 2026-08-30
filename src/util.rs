//! Petites briques sans dependance : identifiants facon ES, horloge.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Un identifiant de 22 caracteres base64url, comme les `uuid` d'ES.
///
/// Pas de dependance a un generateur cryptographique : ces identifiants sont
/// cosmetiques (ils apparaissent dans `_cat/indices`, `/`), pas des secrets.
pub fn random_uuid() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut state = nanos ^ (n.wrapping_mul(0x9E37_79B9_7F4A_7C15)) ^ (std::process::id() as u64);
    let mut out = String::with_capacity(22);
    for _ in 0..22 {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let x = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        out.push(B64[(x >> 33) as usize % 64] as char);
    }
    out
}

/// Millisecondes depuis l'epoch.
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Verifie qu'une valeur a la **forme** d'une duree ES (`30s`, `1m`, `-1s`).
///
/// Ne rend pas la duree : elle sert aux parametres qu'ES accepte et que ferrite
/// n'a rien a interrompre (`?timeout=` sur `_search`, mono-shard). Ce qui compte
/// est alors que la valeur soit refusee **exactement** la ou ES la refuse, avec
/// le meme message — sinon un client qui se trompe d'unite recoit un 200 ici et
/// un 400 la-bas. Les trois messages et leurs bords viennent d'une mesure contre
/// un ES 8.15, pas de sa documentation :
///
/// - `0` et `-1` **sans unite** sont valides (ils veulent dire « pas de
///   limite ») ; `1` tout seul ne l'est pas ;
/// - l'unite se lit en minuscules, **sauf `m`** : `1D`, `1H`, `1MS` passent,
///   `1M` non — un `M` majuscule voudrait dire « mois » ailleurs, ES refuse
///   plutot que de trancher ;
/// - `-1s` passe, `-2s` non (« negative durations are not supported ») ;
/// - un nombre a virgule (`1.0s`, mais aussi `1e2s` et tout entier qui deborde
///   un `i64`) donne « fractional time values are not supported », et un nombre
///   illisible (`xs`, `1seconds`) donne « failed to parse [...] » tout court.
///
/// `scroll::duree`, lui, rend la duree, borne la valeur (`search.max_keep_alive`)
/// et refuse le negatif : ce n'est pas la meme question.
pub fn valider_duree(valeur: &str, parametre: &str) -> Result<(), String> {
    let brut = valeur.trim();
    if brut == "0" || brut == "-1" {
        return Ok(());
    }
    let bas = brut.to_ascii_lowercase();
    // L'ordre compte : `micros` et `ms` avant `s`, sinon ils y tombent.
    let suffixe = ["nanos", "micros", "ms", "s", "m", "h", "d"]
        .into_iter()
        // `m` est le seul dont la casse est verifiee sur la valeur d'origine.
        .find(|u| {
            if *u == "m" {
                brut.ends_with('m')
            } else {
                bas.ends_with(u)
            }
        });
    let Some(suffixe) = suffixe else {
        return Err(format!(
            "failed to parse setting [{parametre}] with value [{valeur}] as a time value: unit is \
             missing or unrecognized"
        ));
    };
    let nombre = bas[..bas.len() - suffixe.len()].trim();
    match nombre.trim_start_matches('+').parse::<i64>() {
        Ok(n) if n < -1 => Err(format!(
            "failed to parse setting [{parametre}] with value [{valeur}] as a time value: negative \
             durations are not supported"
        )),
        Ok(_) => Ok(()),
        Err(_) if nombre.parse::<f64>().is_ok() => Err(format!(
            "failed to parse [{valeur}], fractional time values are not supported"
        )),
        Err(_) => Err(format!("failed to parse [{valeur}]")),
    }
}

#[cfg(test)]
mod tests {
    use super::valider_duree;

    /// Chaque ligne est une reponse relevee sur un ES 8.15 (`?timeout=` sur
    /// `_search`), pas une lecture de sa documentation.
    #[test]
    fn les_durees_qu_es_accepte_et_celles_qu_il_refuse() {
        for bonne in [
            "30s", "1m", "0s", "-1s", "500ms", "2d", "1micros", "3nanos", "0", "-1", "+1s", " 1s ",
            "1D", "1H", "1MS", "1NANOS",
        ] {
            assert!(valider_duree(bonne, "timeout").is_ok(), "{bonne}");
        }
        for (mauvaise, attendu) in [
            ("1", "unit is missing"),
            ("abc", "unit is missing"),
            ("", "unit is missing"),
            ("1sec", "unit is missing"),
            ("1M", "unit is missing"),
            ("-2s", "negative durations"),
            ("1.5s", "fractional"),
            ("1e2s", "fractional"),
            ("9223372036854775808s", "fractional"),
            ("1seconds", "failed to parse [1seconds]"),
            ("xs", "failed to parse [xs]"),
        ] {
            let e = valider_duree(mauvaise, "timeout").unwrap_err();
            assert!(e.contains(attendu), "[{mauvaise}] a rendu {e}");
        }
    }
}
