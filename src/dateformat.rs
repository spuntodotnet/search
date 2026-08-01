//! Les `format` de date d'Elasticsearch, lus et rendus.
//!
//! Un mapping venu d'une instance existante declare presque toujours un
//! `format` sur ses dates (`"yyyy-MM-dd HH:mm:ss"`). Sans lui, les documents ne
//! s'indexent pas : c'etait le premier obstacle restant sur le chemin d'une
//! migration reelle, mesure par `tests/compat/diff_es7.py`.
//!
//! Elasticsearch accepte deux choses sous ce nom :
//!
//! - des **motifs** a la Java (`yyyy`, `MM`, `dd`, `HH`, `mm`, `ss`, `SSS`, du
//!   texte entre apostrophes, des separateurs) ;
//! - des **noms** predefinis (`strict_date_optional_time`, `epoch_millis`,
//!   `basic_date`...), et plusieurs alternatives separees par `||`.
//!
//! Ce module traduit tout ca vers le crate `time`, et **refuse explicitement**
//! ce qu'il ne sait pas traduire : un motif accepte mais mal interprete
//! indexerait des dates fausses sans que rien ne le signale.

use serde_json::Value;
use time::format_description::{self, OwnedFormatItem};
use time::{Date, OffsetDateTime, PrimitiveDateTime};

use crate::error::{EsError, EsResult};

/// Le format par defaut d'Elasticsearch pour un champ `date`.
pub const DEFAUT: &str = "strict_date_optional_time||epoch_millis";

/// Une alternative d'un `format`, dans l'ordre ou ES les essaie.
#[derive(Debug, Clone)]
enum Forme {
    /// ISO-8601 tolerant : avec ou sans heure, avec ou sans fuseau.
    DateOptionalTime,
    EpochMillis,
    EpochSecond,
    /// Un motif traduit.
    Motif {
        items: Vec<OwnedFormatItem>,
        /// Le motif porte-t-il une heure ? Sinon, minuit.
        heure: bool,
        /// Et un decalage explicite ?
        offset: bool,
    },
}

#[derive(Debug, Clone)]
pub struct DateFormat {
    /// Le `format` tel que declare, rendu tel quel par `_mapping`.
    pub source: String,
    formes: Vec<Forme>,
}

impl Default for DateFormat {
    fn default() -> Self {
        Self::parse(DEFAUT).expect("le format par defaut est valide")
    }
}

impl DateFormat {
    /// Traduit un `format` d'Elasticsearch, ou dit pourquoi il ne sait pas.
    pub fn parse(source: &str) -> EsResult<Self> {
        let mut formes = Vec::new();
        for morceau in source.split("||") {
            let morceau = morceau.trim();
            if morceau.is_empty() {
                return Err(EsError::mapper_parsing(format!(
                    "[format] : alternative vide dans [{source}]"
                )));
            }
            formes.push(Forme::parse(morceau)?);
        }
        Ok(Self {
            source: source.to_string(),
            formes,
        })
    }

    /// Est-ce le format par defaut ? (`_mapping` ne le rend pas.)
    pub fn est_defaut(&self) -> bool {
        self.source == DEFAUT
    }

    /// Lit une valeur JSON en millisecondes depuis l'epoch.
    pub fn lit(&self, champ: &str, v: &Value) -> EsResult<i64> {
        match v {
            // Un nombre n'est un timestamp que si le format le prevoit. Avec
            // `format: "yyyy-MM-dd"`, ES refuse `1614852000000` — l'accepter
            // ferait entrer des documents qu'une instance reelle rejette.
            Value::Number(n) => {
                let brut = n.as_f64().ok_or_else(|| {
                    EsError::mapper_parsing(format!("[{champ}] : date [{n}] invalide"))
                })?;
                for forme in &self.formes {
                    match forme {
                        Forme::EpochSecond => return Ok((brut * 1000.0) as i64),
                        Forme::EpochMillis | Forme::DateOptionalTime => return Ok(brut as i64),
                        Forme::Motif { .. } => continue,
                    }
                }
                Err(EsError::mapper_parsing(format!(
                    "failed to parse date field [{champ}] with value [{n}] : formats acceptes = \
                     {}",
                    self.source
                )))
            }
            Value::String(s) => {
                let s = s.trim();
                for forme in &self.formes {
                    if let Some(ms) = forme.lit(s) {
                        return Ok(ms);
                    }
                }
                Err(EsError::mapper_parsing(format!(
                    "failed to parse date field [{champ}] with value [{s}] : formats acceptes = \
                     {}",
                    self.source
                )))
            }
            _ => Err(EsError::mapper_parsing(format!(
                "failed to parse date field [{champ}] : valeur {v} invalide"
            ))),
        }
    }

    /// Rend une date au premier format declare, comme le `*_as_string` d'ES.
    pub fn rend(&self, millis: i64) -> Option<String> {
        self.formes.first()?.rend(millis)
    }
}

impl Forme {
    fn parse(nom: &str) -> EsResult<Self> {
        // Les noms predefinis les plus courants, reduits a leur motif. Les
        // variantes `strict_` ne different que par la tolerance aux zeros
        // manquants, que le crate `time` n'accepte de toute facon pas.
        let motif = match nom.trim_start_matches("strict_") {
            "date_optional_time" | "dateOptionalTime" => return Ok(Self::DateOptionalTime),
            "epoch_millis" => return Ok(Self::EpochMillis),
            "epoch_second" => return Ok(Self::EpochSecond),
            "date" => "yyyy-MM-dd",
            "date_time" => "yyyy-MM-dd'T'HH:mm:ss.SSSZ",
            "date_time_no_millis" => "yyyy-MM-dd'T'HH:mm:ssZ",
            "date_hour_minute_second" | "date_hour_minute_second_millis" => "yyyy-MM-dd'T'HH:mm:ss",
            "date_hour_minute" => "yyyy-MM-dd'T'HH:mm",
            "date_hour" => "yyyy-MM-dd'T'HH",
            "basic_date" => "yyyyMMdd",
            "basic_date_time_no_millis" => "yyyyMMdd'T'HHmmssZ",
            "hour_minute_second" => "HH:mm:ss",
            "hour_minute" => "HH:mm",
            "year_month_day" => "yyyy-MM-dd",
            "year_month" => "yyyy-MM",
            "year" => "yyyy",
            autre => autre,
        };
        traduis(motif, nom)
    }

    fn lit(&self, s: &str) -> Option<i64> {
        match self {
            Self::EpochMillis => s.parse::<i64>().ok(),
            Self::EpochSecond => s.parse::<i64>().ok().map(|n| n * 1000),
            Self::DateOptionalTime => lit_iso(s),
            Self::Motif {
                items,
                heure,
                offset,
                ..
            } => {
                if *offset {
                    return OffsetDateTime::parse(s, items)
                        .ok()
                        .map(|dt| (dt.unix_timestamp_nanos() / 1_000_000) as i64);
                }
                if *heure {
                    return PrimitiveDateTime::parse(s, items)
                        .ok()
                        .map(|dt| (dt.assume_utc().unix_timestamp_nanos() / 1_000_000) as i64);
                }
                Date::parse(s, items)
                    .ok()
                    .map(|d| d.midnight().assume_utc().unix_timestamp() * 1000)
            }
        }
    }

    fn rend(&self, millis: i64) -> Option<String> {
        let dt = OffsetDateTime::from_unix_timestamp_nanos(i128::from(millis) * 1_000_000).ok()?;
        match self {
            Self::EpochMillis => Some(millis.to_string()),
            Self::EpochSecond => Some((millis / 1000).to_string()),
            Self::DateOptionalTime => {
                let f = time::macros::format_description!(
                    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
                );
                dt.format(f).ok()
            }
            Self::Motif { items, .. } => dt.format(items).ok(),
        }
    }
}

/// ISO-8601 tolerant : c'est le `strict_date_optional_time` d'ES.
fn lit_iso(s: &str) -> Option<i64> {
    use time::format_description::well_known::Rfc3339;
    if let Ok(dt) = OffsetDateTime::parse(s, &Rfc3339) {
        return Some((dt.unix_timestamp_nanos() / 1_000_000) as i64);
    }
    let naive = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second][optional [.[subsecond]]]"
    );
    if let Ok(dt) = PrimitiveDateTime::parse(s, naive) {
        return Some((dt.assume_utc().unix_timestamp_nanos() / 1_000_000) as i64);
    }
    let jour = time::macros::format_description!("[year]-[month]-[day]");
    if let Ok(d) = Date::parse(s, jour) {
        return Some(d.midnight().assume_utc().unix_timestamp() * 1000);
    }
    // ES accepte aussi un entier en chaine sous `epoch_millis`.
    s.parse::<i64>().ok()
}

/// Traduit un motif Java vers la description du crate `time`.
///
/// Tout ce qui n'est pas dans cette table est **refuse** : accepter une lettre
/// sans savoir ce qu'elle veut dire indexerait des dates fausses en silence.
fn traduis(motif: &str, nom_origine: &str) -> EsResult<Forme> {
    let mut sortie = String::new();
    let mut heure = false;
    let mut offset = false;
    let octets: Vec<char> = motif.chars().collect();
    let mut i = 0;
    while i < octets.len() {
        let c = octets[i];
        if c == '\'' {
            // Texte litteral entre apostrophes : `'T'`.
            let mut j = i + 1;
            while j < octets.len() && octets[j] != '\'' {
                sortie.push(echappe(octets[j]));
                j += 1;
            }
            if j >= octets.len() {
                return Err(EsError::mapper_parsing(format!(
                    "[format] : apostrophe non fermee dans [{nom_origine}]"
                )));
            }
            i = j + 1;
            continue;
        }
        if !c.is_ascii_alphabetic() {
            sortie.push(echappe(c));
            i += 1;
            continue;
        }
        let mut n = 0;
        while i + n < octets.len() && octets[i + n] == c {
            n += 1;
        }
        let lettre: String = std::iter::repeat_n(c, n).collect();
        let remplacement = match lettre.as_str() {
            "yyyy" | "uuuu" => "[year]",
            "yy" => "[year repr:last_two]",
            "MM" => "[month]",
            "dd" => "[day]",
            "HH" => {
                heure = true;
                "[hour]"
            }
            "hh" => {
                heure = true;
                "[hour repr:12]"
            }
            "mm" => {
                heure = true;
                "[minute]"
            }
            "ss" => {
                heure = true;
                "[second]"
            }
            "SSS" => "[subsecond digits:3]",
            "S" => "[subsecond digits:1]",
            "a" => "[period]",
            "Z" | "ZZ" | "X" | "XX" | "XXX" => {
                offset = true;
                "[offset_hour sign:mandatory][offset_minute]"
            }
            autre => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne sait pas traduire [{autre}] dans le format de date \
                     [{nom_origine}] ; motifs compris : yyyy, yy, MM, dd, HH, hh, mm, ss, SSS, \
                     a, Z, du texte entre apostrophes, et les noms predefinis (date, date_time, \
                     epoch_millis, epoch_second, strict_date_optional_time...)"
                )))
            }
        };
        sortie.push_str(remplacement);
        i += n;
    }

    let items = format_description::parse_owned::<2>(&sortie).map_err(|e| {
        EsError::mapper_parsing(format!(
            "[format] : [{nom_origine}] n'est pas exploitable ({e})"
        ))
    })?;
    Ok(Forme::Motif {
        items: vec![items],
        heure,
        offset,
    })
}

/// Un caractere litteral du motif, tel quel — la syntaxe du crate `time`
/// n'utilise que les crochets, qu'un motif de date ne contient pas.
fn echappe(c: char) -> char {
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ms(format: &str, v: &str) -> i64 {
        DateFormat::parse(format)
            .unwrap()
            .lit("d", &json!(v))
            .unwrap()
    }

    #[test]
    fn motifs_java_courants() {
        // Le motif qui bloquait la reprise d'une instance 7.10.2.
        assert_eq!(
            ms("yyyy-MM-dd HH:mm:ss", "2021-03-04 10:00:00"),
            1614852000000
        );
        assert_eq!(ms("yyyy-MM-dd", "2021-03-04"), 1614816000000);
        assert_eq!(ms("yyyyMMdd", "20210304"), 1614816000000);
        assert_eq!(ms("dd/MM/yyyy", "04/03/2021"), 1614816000000);
        assert_eq!(
            ms("yyyy-MM-dd'T'HH:mm:ss.SSS", "2021-03-04T10:00:00.123"),
            1614852000123
        );
        // Le defaut reste accepte tel quel.
        assert_eq!(ms(DEFAUT, "2021-03-04T10:00:00Z"), 1614852000000);
        assert_eq!(ms(DEFAUT, "1614852000000"), 1614852000000);
    }

    #[test]
    fn alternatives_et_rendu() {
        let f = DateFormat::parse("yyyy-MM-dd HH:mm:ss||yyyy-MM-dd").unwrap();
        assert_eq!(
            f.lit("d", &json!("2021-03-04 10:00:00")).unwrap(),
            1614852000000
        );
        assert_eq!(f.lit("d", &json!("2021-03-04")).unwrap(), 1614816000000);
        // Le rendu suit la premiere alternative, comme chez ES.
        assert_eq!(f.rend(1614852000000).unwrap(), "2021-03-04 10:00:00");
        // Un nombre n'est un timestamp que si le format le prevoit : verifie
        // contre un vrai ES 7.10.2, qui refuse aussi.
        assert!(f.lit("d", &json!(1614852000000i64)).is_err());
        let avec_epoch = DateFormat::parse("yyyy-MM-dd||epoch_millis").unwrap();
        assert_eq!(
            avec_epoch.lit("d", &json!(1614852000000i64)).unwrap(),
            1614852000000
        );
    }

    #[test]
    fn une_valeur_hors_format_est_refusee() {
        let f = DateFormat::parse("yyyy-MM-dd").unwrap();
        let e = f.lit("d", &json!("04/03/2021")).unwrap_err();
        assert!(
            e.reason.contains("failed to parse date field"),
            "{}",
            e.reason
        );
    }

    #[test]
    fn un_motif_inconnu_est_refuse_plutot_que_devine() {
        // `G` (l'ere) n'a pas d'equivalent : mieux vaut le dire que de
        // l'ignorer et indexer une date fausse.
        let e = DateFormat::parse("GGGG yyyy").unwrap_err();
        assert!(e.reason.contains("ne sait pas traduire"), "{}", e.reason);
    }
}
