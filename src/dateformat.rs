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
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

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
        /// Ce qu'il reste de la periode que le motif ne sait pas exprimer, en
        /// millisecondes — voir [`DateFormat::lit_avec_residu`].
        residu: i64,
    },
}

/// Le residu d'une date qui s'arrete a la seconde, a la minute, a l'heure, au
/// jour : ce qu'il faut ajouter pour obtenir le **dernier** instant couvert.
const RESIDU_MS: i64 = 0;
const RESIDU_SECONDE: i64 = 999;
const RESIDU_MINUTE: i64 = 59_999;
const RESIDU_HEURE: i64 = 3_599_999;
const RESIDU_JOUR: i64 = 86_399_999;

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
        self.lit_champ(champ, v).map(|(ms, _)| ms)
    }

    /// Comme [`DateFormat::lit`], plus le **residu** de la valeur lue.
    ///
    /// Une date est un instant, mais une date ecrite couvre souvent une periode :
    /// `2026-03-15` designe une journee entiere. ES le sait, et une borne haute
    /// (`lte`, `gt`) prend le **dernier** instant de cette periode — sinon
    /// `lte: "2026-03-15"` perdrait tout ce qui s'est passe apres minuit
    /// (mesure contre ES 8.15). Le residu est cette largeur, en millisecondes :
    /// les champs d'heure absents remplis au maximum, les champs de date absents
    /// laisses au minimum (`2026-03` -> le 1er a 23:59:59.999, pas le 31).
    pub fn lit_avec_residu(&self, v: &Value) -> EsResult<(i64, i64)> {
        self.lit_champ("", v)
    }

    fn lit_champ(&self, champ: &str, v: &Value) -> EsResult<(i64, i64)> {
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
                        Forme::EpochSecond => {
                            return Ok(((brut * 1000.0) as i64, RESIDU_SECONDE));
                        }
                        Forme::EpochMillis | Forme::DateOptionalTime => {
                            return Ok((brut as i64, RESIDU_MS));
                        }
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
                    if let Some(lu) = forme.lit(s) {
                        return Ok(lu);
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

    /// La valeur porte-t-elle **son propre** instant ?
    ///
    /// La question n'a de sens que sous un `time_zone` : une date ecrite sans
    /// decalage (`2026-03-29T02:00:00`) est une heure **locale**, que le fuseau
    /// place sur l'axe du temps ; une date qui porte un decalage
    /// (`...T02:00:00+02:00`, ou un `Z`) et un nombre d'epoque designent deja
    /// un instant, et ES les laisse alors tranquilles.
    pub fn est_absolue(&self, v: &Value) -> bool {
        match v {
            Value::Number(_) => true,
            Value::String(s) => {
                let s = s.trim();
                self.formes
                    .iter()
                    .find(|f| f.lit(s).is_some())
                    .is_some_and(|f| f.absolue(s))
            }
            _ => false,
        }
    }

    /// Rend une date au premier format declare, comme le `*_as_string` d'ES.
    pub fn rend(&self, millis: i64) -> Option<String> {
        self.formes.first()?.rend(millis)
    }

    /// La meme chose **dans un fuseau** : ES ecrit alors l'heure locale, et le
    /// decalage a la place du `Z` (mesure : `2024-02-29T00:00:00.000+01:00`).
    ///
    /// Un decalage nul rend le `Z` — c'est ce qu'ES ecrit meme pour un
    /// `time_zone: "+00:00"` explicite (mesure).
    pub fn rend_avec_decalage(&self, millis: i64, decalage_s: i32) -> Option<String> {
        self.formes.first()?.rend_avec_decalage(millis, decalage_s)
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

    /// La date lue, et le residu de sa periode (voir
    /// [`DateFormat::lit_avec_residu`]).
    fn lit(&self, s: &str) -> Option<(i64, i64)> {
        match self {
            Self::EpochMillis => s.parse::<i64>().ok().map(|n| (n, RESIDU_MS)),
            Self::EpochSecond => s.parse::<i64>().ok().map(|n| (n * 1000, RESIDU_SECONDE)),
            Self::DateOptionalTime => lit_iso(s),
            Self::Motif {
                items,
                heure,
                offset,
                residu,
            } => {
                let ms = if *offset {
                    OffsetDateTime::parse(s, items)
                        .ok()
                        .map(|dt| (dt.unix_timestamp_nanos() / 1_000_000) as i64)
                } else if *heure {
                    PrimitiveDateTime::parse(s, items)
                        .ok()
                        .map(|dt| (dt.assume_utc().unix_timestamp_nanos() / 1_000_000) as i64)
                } else {
                    Date::parse(s, items)
                        .ok()
                        .map(|d| d.midnight().assume_utc().unix_timestamp() * 1000)
                };
                ms.map(|ms| (ms, *residu))
            }
        }
    }

    /// Cette ecriture-la porte-t-elle son instant ? (voir
    /// [`DateFormat::est_absolue`])
    fn absolue(&self, s: &str) -> bool {
        match self {
            Self::EpochMillis | Self::EpochSecond => true,
            Self::Motif { offset, .. } => *offset,
            // Le format par defaut accepte les deux ecritures : c'est la valeur
            // qui dit laquelle.
            Self::DateOptionalTime => {
                s.ends_with('Z')
                    || s.find('T')
                        .is_some_and(|t| s[t..].contains('+') || s[t..].contains('-'))
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

    fn rend_avec_decalage(&self, millis: i64, decalage_s: i32) -> Option<String> {
        if decalage_s == 0 {
            return self.rend(millis);
        }
        // Un epoch est un instant : il ne change pas de fuseau (mesure :
        // `format: "epoch_millis"` avec un `time_zone` rend le meme nombre).
        if matches!(self, Self::EpochMillis | Self::EpochSecond) {
            return self.rend(millis);
        }
        let decalage = time::UtcOffset::from_whole_seconds(decalage_s).ok()?;
        let dt = OffsetDateTime::from_unix_timestamp_nanos(i128::from(millis) * 1_000_000)
            .ok()?
            .to_offset(decalage);
        match self {
            Self::DateOptionalTime => {
                let f = time::macros::format_description!(
                    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]\
                     [offset_hour sign:mandatory]:[offset_minute]"
                );
                dt.format(f).ok()
            }
            // Un motif qui porte un decalage le rend tel quel ; les autres
            // rendent l'heure locale, et c'est aussi ce que fait ES.
            _ => self.rend_motif(dt),
        }
    }

    fn rend_motif(&self, dt: OffsetDateTime) -> Option<String> {
        match self {
            Self::Motif { items, .. } => dt.format(items).ok(),
            _ => None,
        }
    }
}

/// ISO-8601 tolerant : c'est le `strict_date_optional_time` d'ES.
///
/// Ecrit a la main plutot que delegue au crate `time`, parce qu'il faut savoir
/// **jusqu'ou** la date etait precise : `2026-03-15T12` couvre une heure,
/// `2026-03-15` une journee, et une borne haute prend le dernier instant de
/// cette periode (voir [`DateFormat::lit_avec_residu`]). Un parseur qui rend un
/// instant a perdu cette information.
///
/// Strict comme celui d'ES : chaque champ a sa largeur (`2026-3-5` est refuse).
///
/// Un entier en chaine (`"1614852000000"`) n'est **pas** de son ressort : c'est
/// `epoch_millis`, une autre alternative du format par defaut. Les confondre
/// ferait lire `"2026"` comme 2,026 secondes apres 1970 (mesure : ES y lit
/// l'annee 2026).
fn lit_iso(s: &str) -> Option<(i64, i64)> {
    // Le decalage final, s'il y en a un. Le chercher apres le `T` seulement :
    // les `-` d'une date ne sont pas des signes.
    let (corps, decalage_min) = coupe_decalage(s)?;
    let (date, heure) = match corps.split_once('T') {
        Some((d, h)) if !h.is_empty() => (d, Some(h)),
        Some(_) => return None,
        None => (corps, None),
    };

    let mut champs = date.split('-');
    let annee: i32 = nombre(champs.next()?, 4)?;
    let mois: u8 = match champs.next() {
        Some(m) => nombre(m, 2)?,
        None => 1,
    };
    let jour: u8 = match champs.next() {
        Some(j) => nombre(j, 2)?,
        None => 1,
    };
    if champs.next().is_some() {
        return None;
    }

    let (mut h, mut min, mut sec, mut milli) = (0u8, 0u8, 0u8, 0u32);
    let mut residu = RESIDU_JOUR;
    if let Some(heure) = heure {
        let mut champs = heure.split(':');
        h = nombre(champs.next()?, 2)?;
        residu = RESIDU_HEURE;
        if let Some(m) = champs.next() {
            min = nombre(m, 2)?;
            residu = RESIDU_MINUTE;
        }
        if let Some(s) = champs.next() {
            let (entier, fraction) = match s.split_once('.') {
                Some((e, f)) => (e, Some(f)),
                None => (s, None),
            };
            sec = nombre(entier, 2)?;
            residu = RESIDU_SECONDE;
            if let Some(f) = fraction {
                if f.is_empty() || f.len() > 9 || !f.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                // Les chiffres au-dela de la milliseconde sont tronques, comme
                // le fait tantivy, qui indexe les dates a la milliseconde.
                let mut millis: u32 = 0;
                for (i, c) in f.chars().take(3).enumerate() {
                    millis += c.to_digit(10)? * 10u32.pow(2 - i as u32);
                }
                milli = millis;
                residu = RESIDU_MS;
            }
        }
        if champs.next().is_some() {
            return None;
        }
    }

    let date = Date::from_calendar_date(annee, Month::try_from(mois).ok()?, jour).ok()?;
    let t = Time::from_hms_milli(h, min, sec, u16::try_from(milli).ok()?).ok()?;
    let ms = (PrimitiveDateTime::new(date, t)
        .assume_utc()
        .unix_timestamp_nanos()
        / 1_000_000) as i64
        - i64::from(decalage_min) * 60_000;
    Some((ms, residu))
}

/// Detache un `Z` ou un `+HH:mm` final, et rend le decalage en minutes.
fn coupe_decalage(s: &str) -> Option<(&str, i32)> {
    if let Some(corps) = s.strip_suffix('Z') {
        return Some((corps, 0));
    }
    // Une partie horaire ne contient ni `+` ni `-` : le premier des deux apres
    // le `T` ouvre forcement le decalage.
    let Some(pos_t) = s.find('T') else {
        return Some((s, 0));
    };
    let Some(pos) = s[pos_t..].find(['+', '-']).map(|p| p + pos_t) else {
        return Some((s, 0));
    };
    let signe = if s.as_bytes()[pos] == b'-' { -1 } else { 1 };
    let brut = &s[pos + 1..];
    let (hh, mm) = match brut.len() {
        2 => (brut, "00"),
        4 => (&brut[..2], &brut[2..]),
        5 if brut.as_bytes()[2] == b':' => (&brut[..2], &brut[3..]),
        _ => return None,
    };
    let h: i32 = nombre(hh, 2)?;
    let m: i32 = nombre(mm, 2)?;
    Some((&s[..pos], signe * (h * 60 + m)))
}

/// Un champ de date, de largeur imposee (le `strict_` d'ES) et sans signe.
fn nombre<T: std::str::FromStr>(s: &str, largeur: usize) -> Option<T> {
    if s.len() != largeur || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Traduit un motif Java vers la description du crate `time`.
///
/// Tout ce qui n'est pas dans cette table est **refuse** : accepter une lettre
/// sans savoir ce qu'elle veut dire indexerait des dates fausses en silence.
fn traduis(motif: &str, nom_origine: &str) -> EsResult<Forme> {
    let mut sortie = String::new();
    let mut heure = false;
    let mut offset = false;
    // La plus fine unite d'heure que le motif exprime : ce qu'il ne dit pas est
    // le residu de la periode (`yyyy-MM-dd` couvre une journee).
    let mut residu = RESIDU_JOUR;
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
                residu = residu.min(RESIDU_HEURE);
                "[hour]"
            }
            "hh" => {
                heure = true;
                residu = residu.min(RESIDU_HEURE);
                "[hour repr:12]"
            }
            "mm" => {
                heure = true;
                residu = residu.min(RESIDU_MINUTE);
                "[minute]"
            }
            "ss" => {
                heure = true;
                residu = residu.min(RESIDU_SECONDE);
                "[second]"
            }
            "SSS" => {
                residu = RESIDU_MS;
                "[subsecond digits:3]"
            }
            "S" => {
                residu = RESIDU_MS;
                "[subsecond digits:1]"
            }
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
        residu,
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
