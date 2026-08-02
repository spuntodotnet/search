//! Le *date math* d'Elasticsearch : `now`, `now-1d/d`, `2026-03-15||+1M`.
//!
//! Une borne de date d'une requete n'est pas seulement une date. ES y accepte
//! une **expression** — `now` resolu par le serveur, une ancre suivie
//! d'operations — et, meme sans expression, il **arrondit la borne selon son
//! cote** : `lte: "2026-03-15"` couvre la journee entiere, `lt: "2026-03-15"`
//! s'arrete a minuit.
//!
//! Les deux comptent autant l'un que l'autre. Sans le premier, un filtre
//! `{"range": {"livraison.fin": {"lt": "now"}}}` — le KPI « en retard » de
//! n'importe quelle application — echoue en 400. Sans le second, il rend
//! silencieusement moins de documents qu'ES : le pire resultat possible ici.
//!
//! Tout ce qui suit est **mesure** contre un ES 8.15.0, pas lu :
//!
//! - l'arrondi depend de la borne : `gte`/`lt` arrondissent vers le bas,
//!   `gt`/`lte` vers le haut (« dernier instant de la periode ») ;
//! - une ancre `2026-03-16||-1d` est lue **vers le bas** meme sous un `lte` :
//!   seul un operateur `/` reintroduit l'arrondi haut, et il l'applique a
//!   **chaque** `/` rencontre (`||/M/d` sous `lte` rend le dernier instant du
//!   mois, pas du jour) ;
//! - une date partielle voit ses champs d'heure manquants remplis au maximum
//!   (`2026-03-15` -> `23:59:59.999`) mais ses champs de **date** manquants
//!   remplis au minimum (`2026-03` -> le 1er, pas le 31) ;
//! - `+1M` sur le 31 janvier donne le 28 fevrier (le jour est ramene au dernier
//!   du mois), `/w` arrondit au **lundi** ;
//! - les messages d'erreur sont ceux d'ES, mot pour mot : un client qui les
//!   affiche montre la meme chose des deux cotes.
//!
//! Le date math ne s'applique **qu'aux requetes**. A l'indexation, ES refuse
//! `{"d": "now"}` (mesure) : le document porterait une date qui depend de
//! l'instant ou il a ete ecrit.

use serde_json::Value;
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use crate::dateformat::DateFormat;
use crate::error::{EsError, EsResult};

/// De quel cote arrondir une borne dont la precision est plus grossiere que la
/// milliseconde.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrondi {
    /// `gte`, `lt` : le premier instant de la periode.
    Bas,
    /// `gt`, `lte` : le dernier instant de la periode.
    Haut,
}

/// Resout une borne de date d'une requete : date math compris, arrondi compris.
///
/// `maintenant` est l'instant du **debut de la requete** : ES resout `now` une
/// fois par recherche, pas une fois par clause, sinon deux bornes de la meme
/// requete ne parleraient pas du meme instant.
pub fn borne(v: &Value, format: &DateFormat, maintenant: i64, sens: Arrondi) -> EsResult<i64> {
    match v {
        Value::String(s) => expression(s, format, maintenant, sens),
        // Un nombre ne peut pas porter d'expression : c'est un timestamp, lu
        // par le format du champ comme a l'indexation.
        autre => {
            let (ms, residu) = format
                .lit_avec_residu(autre)
                .map_err(|_| echec_de_lecture(&autre.to_string(), format))?;
            Ok(applique_residu(ms, residu, sens))
        }
    }
}

/// La meme chose depuis une chaine deja extraite.
pub fn expression(s: &str, format: &DateFormat, maintenant: i64, sens: Arrondi) -> EsResult<i64> {
    if let Some(math) = s.strip_prefix("now") {
        return applique_math(math, maintenant, sens);
    }
    if let Some((ancre, math)) = s.split_once("||") {
        if ancre.is_empty() {
            return Err(parse_exception("cannot parse empty datetime"));
        }
        // L'ancre est toujours lue vers le bas, meme sous un `lte` : mesure
        // contre ES 8.15 (`lte: "2026-03-16||-1d"` s'arrete a minuit le 15).
        let (ms, _) = lit(ancre, format, s)?;
        return applique_math(math, ms, sens);
    }
    let (ms, residu) = lit(s, format, s)?;
    Ok(applique_residu(ms, residu, sens))
}

/// Lit une date litterale avec le format du champ, en rendant l'erreur d'ES.
///
/// `complet` est l'expression telle que le client l'a ecrite : ES cite dans son
/// message la chaine entiere, ancre et operations comprises.
fn lit(s: &str, format: &DateFormat, complet: &str) -> EsResult<(i64, i64)> {
    format
        .lit_avec_residu(&Value::String(s.to_string()))
        .map_err(|_| echec_de_lecture(complet, format))
}

fn echec_de_lecture(complet: &str, format: &DateFormat) -> EsError {
    parse_exception(format!(
        "failed to parse date field [{complet}] with format [{f}]: [failed to parse date field \
         [{complet}] with format [{f}]]",
        f = format.source
    ))
}

/// Le dernier instant couvert par une date moins precise que la milliseconde.
fn applique_residu(ms: i64, residu: i64, sens: Arrondi) -> i64 {
    match sens {
        Arrondi::Bas => ms,
        Arrondi::Haut => ms + residu,
    }
}

/// Les operations `+1d`, `-2h`, `/d` appliquees dans l'ordre, comme ES.
fn applique_math(math: &str, depart: i64, sens: Arrondi) -> EsResult<i64> {
    let mut dt = instant(depart)?;
    let chars: Vec<char> = math.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let op = chars[i];
        i += 1;
        let arrondir = match op {
            '/' => true,
            '+' | '-' => false,
            _ => {
                return Err(parse_exception(format!(
                    "operator not supported for date math [{math}]"
                )))
            }
        };
        // Le nombre, absent sur un arrondi.
        let mut nombre = String::new();
        if !arrondir {
            while i < chars.len() && chars[i].is_ascii_digit() {
                nombre.push(chars[i]);
                i += 1;
            }
        }
        if i >= chars.len() {
            return Err(parse_exception(format!("truncated date math [{math}]")));
        }
        let unite = chars[i];
        i += 1;
        if arrondir {
            dt = arrondi(dt, unite, math, sens)?;
        } else {
            // ES lit ce nombre dans un entier 32 bits, et rend son erreur de
            // conversion telle quelle.
            let n: i32 = if nombre.is_empty() {
                1
            } else {
                nombre.parse().map_err(|_| {
                    EsError::new(
                        axum::http::StatusCode::BAD_REQUEST,
                        "number_format_exception",
                        format!("For input string: \"{nombre}\""),
                    )
                })?
            };
            let n = if op == '-' {
                -i64::from(n)
            } else {
                i64::from(n)
            };
            dt = ajoute(dt, n, unite, math)?;
        }
    }
    Ok(millis(dt))
}

/// `+1M` sur le 31 janvier donne le 28 fevrier : le jour est ramene au dernier
/// du mois d'arrivee, comme le `plusMonths` de Java.
fn ajoute(dt: OffsetDateTime, n: i64, unite: char, math: &str) -> EsResult<OffsetDateTime> {
    let deborde =
        || EsError::illegal_argument(format!("date math [{math}] hors des dates representables"));
    match unite {
        'y' => decale_mois(dt, n.checked_mul(12).ok_or_else(deborde)?).ok_or_else(deborde),
        'M' => decale_mois(dt, n).ok_or_else(deborde),
        'w' => ajoute_duree(dt, n.checked_mul(7 * 86_400_000).ok_or_else(deborde)?),
        'd' => ajoute_duree(dt, n.checked_mul(86_400_000).ok_or_else(deborde)?),
        'h' | 'H' => ajoute_duree(dt, n.checked_mul(3_600_000).ok_or_else(deborde)?),
        'm' => ajoute_duree(dt, n.checked_mul(60_000).ok_or_else(deborde)?),
        's' => ajoute_duree(dt, n.checked_mul(1_000).ok_or_else(deborde)?),
        autre => Err(parse_exception(format!(
            "unit [{autre}] not supported for date math [{math}]"
        ))),
    }
}

fn ajoute_duree(dt: OffsetDateTime, ms: i64) -> EsResult<OffsetDateTime> {
    instant(
        millis(dt)
            .checked_add(ms)
            .ok_or_else(|| EsError::illegal_argument("date math hors des dates representables"))?,
    )
}

fn decale_mois(dt: OffsetDateTime, n: i64) -> Option<OffsetDateTime> {
    let total = i64::from(dt.year()) * 12 + i64::from(u8::from(dt.month())) - 1 + n;
    let annee = i32::try_from(total.div_euclid(12)).ok()?;
    let mois = Month::try_from(u8::try_from(total.rem_euclid(12)).ok()? + 1).ok()?;
    let jour = dt.day().min(jours_du_mois(annee, mois));
    let date = Date::from_calendar_date(annee, mois, jour).ok()?;
    Some(PrimitiveDateTime::new(date, dt.time()).assume_utc())
}

fn jours_du_mois(annee: i32, mois: Month) -> u8 {
    time::util::days_in_month(mois, annee)
}

/// `/d` : le debut de la periode, ou son dernier instant sous une borne haute.
fn arrondi(dt: OffsetDateTime, unite: char, math: &str, sens: Arrondi) -> EsResult<OffsetDateTime> {
    let bas = match unite {
        'y' => jour(Date::from_calendar_date(dt.year(), Month::January, 1)),
        'M' => jour(Date::from_calendar_date(dt.year(), dt.month(), 1)),
        // ES arrondit a la semaine ISO, donc au lundi (mesure).
        'w' => {
            let recul = i64::from(dt.weekday().number_days_from_monday());
            instant(millis(jour(Ok(dt.date()))?) - recul * 86_400_000)
        }
        'd' => jour(Ok(dt.date())),
        'h' | 'H' => tronque(dt, Time::from_hms(dt.hour(), 0, 0)),
        'm' => tronque(dt, Time::from_hms(dt.hour(), dt.minute(), 0)),
        's' => tronque(dt, Time::from_hms(dt.hour(), dt.minute(), dt.second())),
        autre => {
            return Err(parse_exception(format!(
                "unit [{autre}] not supported for date math [{math}]"
            )))
        }
    }?;
    match sens {
        Arrondi::Bas => Ok(bas),
        // « le dernier instant de la periode » : ES avance d'une unite et
        // retire une milliseconde.
        Arrondi::Haut => ajoute_duree(ajoute(bas, 1, unite, math)?, -1),
    }
}

fn jour(date: Result<Date, time::error::ComponentRange>) -> EsResult<OffsetDateTime> {
    let date = date.map_err(|e| EsError::internal(format!("date hors bornes : {e}")))?;
    Ok(PrimitiveDateTime::new(date, Time::MIDNIGHT).assume_utc())
}

fn tronque(
    dt: OffsetDateTime,
    t: Result<Time, time::error::ComponentRange>,
) -> EsResult<OffsetDateTime> {
    let t = t.map_err(|e| EsError::internal(format!("heure hors bornes : {e}")))?;
    Ok(PrimitiveDateTime::new(dt.date(), t).assume_utc())
}

fn instant(ms: i64) -> EsResult<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .map(|dt| dt.to_offset(UtcOffset::UTC))
        .map_err(|_| {
            EsError::illegal_argument(format!("date [{ms}] hors des dates representables"))
        })
}

fn millis(dt: OffsetDateTime) -> i64 {
    (dt.unix_timestamp_nanos() / 1_000_000) as i64
}

fn parse_exception(reason: impl Into<String>) -> EsError {
    EsError::new(
        axum::http::StatusCode::BAD_REQUEST,
        "parse_exception",
        reason,
    )
}

/// L'instant courant en millisecondes, pris une fois par requete.
pub fn maintenant() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-03-15T10:20:30.500Z
    const ANCRE: i64 = 1773570030500;

    fn r(expr: &str, sens: Arrondi) -> String {
        let f = DateFormat::default();
        let ms = expression(expr, &f, ANCRE, sens).unwrap_or_else(|e| panic!("{expr} : {e}"));
        let dt = instant(ms).unwrap();
        dt.format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    }

    fn bas(expr: &str) -> String {
        r(expr, Arrondi::Bas)
    }
    fn haut(expr: &str) -> String {
        r(expr, Arrondi::Haut)
    }

    fn err(expr: &str) -> String {
        let f = DateFormat::default();
        expression(expr, &f, ANCRE, Arrondi::Bas)
            .unwrap_err()
            .reason
    }

    #[test]
    fn now_et_ses_operations() {
        assert_eq!(bas("now"), "2026-03-15T10:20:30.5Z");
        assert_eq!(bas("now-1d"), "2026-03-14T10:20:30.5Z");
        assert_eq!(bas("now+1h"), "2026-03-15T11:20:30.5Z");
        assert_eq!(bas("now-90m"), "2026-03-15T08:50:30.5Z");
        assert_eq!(bas("now-2H"), "2026-03-15T08:20:30.5Z");
        assert_eq!(bas("now/d"), "2026-03-15T00:00:00Z");
        assert_eq!(bas("now-1d/d"), "2026-03-14T00:00:00Z");
        assert_eq!(bas("now/M"), "2026-03-01T00:00:00Z");
        assert_eq!(bas("now/y"), "2026-01-01T00:00:00Z");
        assert_eq!(bas("now/h"), "2026-03-15T10:00:00Z");
        assert_eq!(bas("now/m"), "2026-03-15T10:20:00Z");
        assert_eq!(bas("now/s"), "2026-03-15T10:20:30Z");
        // Le 15 mars 2026 est un dimanche : la semaine ISO commence le 9.
        assert_eq!(bas("now/w"), "2026-03-09T00:00:00Z");
        assert_eq!(bas("now+1d+1d+1d"), "2026-03-18T10:20:30.5Z");
    }

    #[test]
    fn ancre_puis_operations() {
        assert_eq!(bas("2026-03-15||+1d"), "2026-03-16T00:00:00Z");
        assert_eq!(bas("2026-03-15T10:20:30.123||/d"), "2026-03-15T00:00:00Z");
        assert_eq!(bas("2026-03-15||-1M/M"), "2026-02-01T00:00:00Z");
        assert_eq!(bas("2026-03-15T10:20:30Z||+1h"), "2026-03-15T11:20:30Z");
        assert_eq!(bas("1773504000000||+1d"), "2026-03-15T16:00:00Z");
        // Mesure contre ES : le jour est ramene au dernier du mois d'arrivee.
        assert_eq!(bas("2026-01-31||+1M"), "2026-02-28T00:00:00Z");
        assert_eq!(bas("2026-03-31||-1M"), "2026-02-28T00:00:00Z");
        assert_eq!(bas("2024-02-29||+1y"), "2025-02-28T00:00:00Z");
        assert_eq!(bas("2026-08-02T12:00:00Z||+1w/w"), "2026-08-03T00:00:00Z");
    }

    #[test]
    fn arrondi_selon_la_borne() {
        // Une date partielle : les champs d'heure manquants au maximum.
        assert_eq!(bas("2026-03-15"), "2026-03-15T00:00:00Z");
        assert_eq!(haut("2026-03-15"), "2026-03-15T23:59:59.999Z");
        assert_eq!(haut("2026-03-15T12"), "2026-03-15T12:59:59.999Z");
        assert_eq!(haut("2026-03-15T12:00"), "2026-03-15T12:00:59.999Z");
        assert_eq!(haut("2026-03-15T12:00:00"), "2026-03-15T12:00:00.999Z");
        assert_eq!(haut("2026-03-15T12:00:00.123Z"), "2026-03-15T12:00:00.123Z");
        // ... mais les champs de date manquants au minimum : le 1er, pas le 31.
        assert_eq!(haut("2026-03"), "2026-03-01T23:59:59.999Z");
        assert_eq!(haut("2026"), "2026-01-01T23:59:59.999Z");
        // Un arrondi explicite sous une borne haute : dernier instant.
        assert_eq!(haut("2026-03-15||/d"), "2026-03-15T23:59:59.999Z");
        assert_eq!(haut("2026-03-15||/M"), "2026-03-31T23:59:59.999Z");
        // Chaque `/` reapplique l'arrondi haut, y compris le premier.
        assert_eq!(
            haut("2026-03-15T10:00:00Z||/M/d"),
            "2026-03-31T23:59:59.999Z"
        );
        // Sans operateur d'arrondi, une borne haute ne gagne rien.
        assert_eq!(haut("2026-03-14||+1d"), "2026-03-15T00:00:00Z");
        assert_eq!(haut("2026-03-16||-1d"), "2026-03-15T00:00:00Z");
        assert_eq!(haut("now"), "2026-03-15T10:20:30.5Z");
        // Un arrondi haut suivi d'un decalage garde son residu.
        assert_eq!(
            haut("2026-03-16T10:00:00Z||/d-1d"),
            "2026-03-15T23:59:59.999Z"
        );
    }

    #[test]
    fn les_messages_sont_ceux_d_elasticsearch() {
        assert_eq!(err("now-1q"), "unit [q] not supported for date math [-1q]");
        assert_eq!(err("now/D"), "unit [D] not supported for date math [/D]");
        assert_eq!(err("now-1"), "truncated date math [-1]");
        assert_eq!(err("now/"), "truncated date math [/]");
        assert_eq!(err("now-"), "truncated date math [-]");
        assert_eq!(err("now+1d/"), "truncated date math [+1d/]");
        assert_eq!(err("nowX"), "operator not supported for date math [X]");
        assert_eq!(err("now1d"), "operator not supported for date math [1d]");
        assert_eq!(err("now/dd"), "operator not supported for date math [/dd]");
        assert_eq!(
            err("now-1.5d"),
            "unit [.] not supported for date math [-1.5d]"
        );
        assert_eq!(
            err("2026-03-15||+1d||+1d"),
            "operator not supported for date math [+1d||+1d]"
        );
        assert_eq!(err("||+1d"), "cannot parse empty datetime");
        assert_eq!(
            err("now-99999999999999d"),
            "For input string: \"99999999999999\""
        );
        assert!(err("NOW").starts_with("failed to parse date field [NOW] with format ["));
        assert!(err("2026-03-15+1d").starts_with("failed to parse date field [2026-03-15+1d]"));
    }
}
