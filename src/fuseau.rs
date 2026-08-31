//! Les regles d'un fuseau horaire : quel decalage a quel instant, et ce que
//! devient une heure locale qui n'existe pas — ou qui existe deux fois.
//!
//! Un `date_histogram` avec `time_zone` ne se calcule pas sans ca : c'est le
//! fuseau qui decide qu'un seau « par jour » dure 23 ou 25 heures, et qu'un
//! seau « par heure » manque a l'appel le dimanche de mars. Le decalage n'est
//! pas une constante par zone, et il ne l'est pas non plus par annee.
//!
//! Ce module lit la table generee par
//! `tests/compat/genere_fuseaux.py` ([`crate::tzdata`]) — dumpee du tzdb du
//! **JDK d'Elasticsearch**, donc des memes regles que l'arbitre applique — et
//! reproduit les trois methodes de `java.time.zone.ZoneRules` dont
//! `org.elasticsearch.common.Rounding` se sert :
//!
//! | Java | ici |
//! |---|---|
//! | `getOffset(Instant)` | [`Fuseau::decalage`] |
//! | `getValidOffsets(LocalDateTime)` | [`Fuseau::decalages_valides`] |
//! | `getTransition(LocalDateTime)` | [`Fuseau::transition_du_trou`] |
//! | `previousTransition(Instant)` | [`Fuseau::transition_precedente`] |
//!
//! Deux details qui ne se devinent pas et que la table porte :
//!
//! - une zone n'a pas qu'un historique, elle a aussi des **regles annuelles**
//!   pour l'avenir (`ZoneOffsetTransitionRule`) : au-dela de la derniere
//!   transition connue, le decalage se **calcule** (« le dernier dimanche de
//!   mars a 01:00 UTC »), il ne se lit pas. Sans elles, tout graphe pose sur
//!   une date future serait faux d'une heure la moitie de l'annee ;
//! - l'heure de la regle est exprimee dans un des trois referentiels
//!   (`UTC`, `WALL`, `STANDARD`), et le choix change l'instant de bascule.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use crate::error::{EsError, EsResult};
use crate::tzdata;

/// Une transition : l'instant ou le decalage change, et les deux decalages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    /// L'instant de la bascule, en millisecondes.
    pub instant_ms: i64,
    /// Le decalage juste avant, en secondes.
    pub avant: i32,
    /// Le decalage juste apres, en secondes.
    pub apres: i32,
}

impl Transition {
    /// La premiere heure locale que la bascule fait disparaitre (un trou) ou
    /// repeter (un recouvrement), en millisecondes d'heure locale.
    pub fn local_avant_ms(&self) -> i64 {
        self.instant_ms + i64::from(self.avant) * 1000
    }

    /// L'heure locale qu'il est juste apres la bascule.
    pub fn local_apres_ms(&self) -> i64 {
        self.instant_ms + i64::from(self.apres) * 1000
    }

    /// Un trou (l'heure avance) plutot qu'un recouvrement.
    pub fn est_un_trou(&self) -> bool {
        self.apres > self.avant
    }
}

/// Une transition **historique**, telle que la table la porte.
#[derive(Debug, Clone, Copy)]
struct Bascule {
    epoch_s: i64,
    avant: i32,
    apres: i32,
}

/// Une regle annuelle : « le dernier dimanche de mars a 01:00 UTC ».
#[derive(Debug, Clone, Copy)]
struct RegleAnnuelle {
    mois: u8,
    /// Le jour du mois, negatif pour « compte depuis la fin ».
    jour: i8,
    /// 0 = aucun, sinon 1 (lundi) a 7 (dimanche).
    jour_semaine: u8,
    seconde_du_jour: i32,
    /// L'heure est celle du **lendemain** a minuit.
    minuit_fin_de_jour: bool,
    /// 0 = UTC, 1 = heure locale, 2 = heure standard (l'ordre de l'enum de
    /// Java, verifie sur la regle europeenne : sa bascule est a 01:00 **UTC**).
    definition: u8,
    standard: i32,
    avant: i32,
    apres: i32,
}

/// Le jeu de regles d'une zone.
#[derive(Debug)]
struct Regles {
    /// Le decalage avant la premiere transition connue.
    init: i32,
    /// Les transitions historiques, dans l'ordre.
    bascules: Vec<Bascule>,
    /// Les regles annuelles qui prennent le relais apres la derniere.
    annuelles: Vec<RegleAnnuelle>,
    /// Les decalages distincts de la zone : les candidats d'une heure locale.
    decalages: Vec<i32>,
}

/// Un fuseau resolu : soit un decalage fixe, soit un jeu de regles.
#[derive(Debug, Clone)]
pub struct Fuseau {
    nom: String,
    /// `None` pour une zone a regles.
    fixe: Option<i32>,
    regles: Option<Arc<Regles>>,
}

impl Fuseau {
    /// Le fuseau UTC, celui d'une requete qui n'en demande pas.
    pub fn utc() -> Self {
        Self {
            nom: "UTC".to_string(),
            fixe: Some(0),
            regles: None,
        }
    }

    /// Le fuseau n'est-il qu'un decalage constant ?
    pub fn est_fixe(&self) -> bool {
        self.fixe.is_some()
    }

    pub fn nom(&self) -> &str {
        &self.nom
    }

    /// Lit un `time_zone` du Query DSL : un identifiant de zone
    /// (`Europe/Paris`) ou un decalage (`+05:30`, `-08:00`, `Z`).
    ///
    /// Le message de refus est celui d'Elasticsearch, mot pour mot : un client
    /// qui l'affiche montre la meme chose des deux cotes.
    pub fn parse(s: &str) -> EsResult<Self> {
        if let Some(fixe) = decalage_litteral(s) {
            return Ok(Self {
                nom: s.to_string(),
                fixe: Some(fixe),
                regles: None,
            });
        }
        match charge(s) {
            Some(regles) => Ok(Self {
                nom: s.to_string(),
                fixe: None,
                regles: Some(regles),
            }),
            None => Err(EsError::illegal_argument(format!(
                "Unknown time-zone ID: {s}"
            ))),
        }
    }

    /// `ZoneRules.getOffset(Instant)` : le decalage en vigueur a cet instant.
    pub fn decalage(&self, instant_ms: i64) -> i32 {
        match (&self.fixe, &self.regles) {
            (Some(f), _) => *f,
            (_, Some(r)) => r.decalage(div_plancher(instant_ms, 1000)),
            _ => 0,
        }
    }

    /// L'heure locale correspondant a un instant, en millisecondes « locales »
    /// — c'est-a-dire lues comme si le decalage etait nul, exactement le
    /// `localMillis` d'Elasticsearch.
    pub fn vers_local(&self, instant_ms: i64) -> i64 {
        instant_ms + i64::from(self.decalage(instant_ms)) * 1000
    }

    /// `ZoneRules.getValidOffsets(LocalDateTime)` : les decalages sous lesquels
    /// cette heure locale existe.
    ///
    /// Vide dans un **trou** (l'heure n'a pas existe), deux elements dans un
    /// **recouvrement** (elle a existe deux fois). L'ordre est celui de Java :
    /// l'instant le plus tot d'abord, donc le plus grand decalage.
    pub fn decalages_valides(&self, local_ms: i64) -> Vec<i32> {
        if let Some(f) = self.fixe {
            return vec![f];
        }
        let Some(regles) = &self.regles else {
            return vec![0];
        };
        let mut out: Vec<i32> = Vec::new();
        for &d in &regles.decalages {
            let instant = local_ms - i64::from(d) * 1000;
            if regles.decalage(div_plancher(instant, 1000)) == d && !out.contains(&d) {
                out.push(d);
            }
        }
        // Le plus grand decalage donne l'instant le plus tot.
        out.sort_unstable_by(|a, b| b.cmp(a));
        out
    }

    /// L'instant correspondant a une heure locale sous un decalage donne.
    pub fn vers_instant(local_ms: i64, decalage: i32) -> i64 {
        local_ms - i64::from(decalage) * 1000
    }

    /// `ZoneRules.getTransition(LocalDateTime)` : la transition qui explique
    /// qu'une heure locale n'existe pas (ou existe deux fois).
    pub fn transition_du_trou(&self, local_ms: i64) -> Option<Transition> {
        let regles = self.regles.as_ref()?;
        regles
            .transitions_autour(div_plancher(local_ms, 1000))
            .into_iter()
            .find(|t| local_ms >= t.local_avant_ms() && local_ms < t.local_apres_ms())
    }

    /// `ZoneRules.previousTransition(Instant)` : la derniere bascule
    /// **strictement** avant cet instant.
    ///
    /// Java raisonne en secondes et **remonte** a la seconde suivante des que
    /// l'instant en porte une fraction (« allow rounding errors ») : un instant
    /// a 500 ms se lit comme la seconde d'apres. Le detail n'est pas cosmetique
    /// — c'est ce que l'arrondi d'un `fixed_interval` interroge a chaque tour.
    pub fn transition_precedente(&self, instant_ms: i64) -> Option<Transition> {
        let regles = self.regles.as_ref()?;
        let mut secondes = div_plancher(instant_ms, 1000);
        if instant_ms.rem_euclid(1000) > 0 {
            secondes += 1;
        }
        regles.transition_precedente(secondes)
    }
}

/// Un decalage ecrit en toutes lettres : `Z`, `UTC`, `+05:30`, `-0800`, `+02`.
///
/// ES lit ces formes-la sans passer par le tzdb ; les noms trois-lettres
/// historiques (`CET`, `EST`) sont, eux, dans le tzdb.
fn decalage_litteral(s: &str) -> Option<i32> {
    if s == "Z" || s == "UTC" || s == "GMT" || s == "UT" {
        return Some(0);
    }
    let reste = s
        .strip_prefix("UTC")
        .or_else(|| s.strip_prefix("GMT"))
        .or_else(|| s.strip_prefix("UT"))
        .unwrap_or(s);
    let (signe, reste) = match reste.as_bytes().first()? {
        b'+' => (1, &reste[1..]),
        b'-' => (-1, &reste[1..]),
        _ => return None,
    };
    let chiffres: String = reste.chars().filter(|c| *c != ':').collect();
    if !chiffres.chars().all(|c| c.is_ascii_digit()) || chiffres.is_empty() {
        return None;
    }
    let (h, m, sec) = match chiffres.len() {
        2 => (&chiffres[0..2], "0", "0"),
        4 => (&chiffres[0..2], &chiffres[2..4], "0"),
        6 => (&chiffres[0..2], &chiffres[2..4], &chiffres[4..6]),
        _ => return None,
    };
    let h: i32 = h.parse().ok()?;
    let m: i32 = m.parse().ok()?;
    let sec: i32 = sec.parse().ok()?;
    if h > 18 || m > 59 || sec > 59 {
        return None;
    }
    Some(signe * (h * 3600 + m * 60 + sec))
}

impl Regles {
    /// Le decalage en vigueur a un instant (en secondes depuis l'epoque).
    ///
    /// La structure est celle de `ZoneRules.getOffset` : l'historique tant
    /// qu'il en reste, les regles annuelles au-dela.
    fn decalage(&self, epoch_s: i64) -> i32 {
        let Some(derniere) = self.bascules.last() else {
            return self.init;
        };
        if !self.annuelles.is_empty() && epoch_s > derniere.epoch_s {
            let annee = annee_de(epoch_s + i64::from(derniere.apres));
            let transitions = self.transitions_de_l_annee(annee);
            for t in &transitions {
                if epoch_s < div_plancher(t.instant_ms, 1000) {
                    return t.avant;
                }
            }
            return transitions.last().map_or(derniere.apres, |t| t.apres);
        }
        match self.bascules.binary_search_by(|b| b.epoch_s.cmp(&epoch_s)) {
            // Pile sur une bascule : le nouveau decalage s'applique.
            Ok(i) => self.bascules[i].apres,
            Err(0) => self.init,
            Err(i) => self.bascules[i - 1].apres,
        }
    }

    /// Les transitions que les regles annuelles produisent pour une annee.
    fn transitions_de_l_annee(&self, annee: i32) -> Vec<Transition> {
        let mut out: Vec<Transition> = self
            .annuelles
            .iter()
            .filter_map(|r| r.transition(annee))
            .collect();
        out.sort_by_key(|t| t.instant_ms);
        out
    }

    /// Les transitions autour d'un instant : de quoi decider si une heure
    /// locale tombe dans un trou.
    ///
    /// Deux jours de part et d'autre suffisent — aucun fuseau ne bascule deux
    /// fois dans cette fenetre — mais il faut chercher **des deux cotes** : une
    /// heure locale d'un trou est, par definition, en avance sur l'instant.
    fn transitions_autour(&self, epoch_s: i64) -> Vec<Transition> {
        const FENETRE: i64 = 2 * 86_400;
        let mut out = Vec::new();
        let debut = epoch_s - FENETRE;
        let fin = epoch_s + FENETRE;
        for (i, b) in self.bascules.iter().enumerate() {
            if b.epoch_s >= debut && b.epoch_s <= fin {
                out.push(self.transition(i));
            }
        }
        if !self.annuelles.is_empty() {
            if let Some(derniere) = self.bascules.last() {
                if fin > derniere.epoch_s {
                    let annee = annee_de(epoch_s + i64::from(derniere.apres));
                    for annee in [annee - 1, annee, annee + 1] {
                        for t in self.transitions_de_l_annee(annee) {
                            let s = div_plancher(t.instant_ms, 1000);
                            if s > derniere.epoch_s && s >= debut && s <= fin {
                                out.push(t);
                            }
                        }
                    }
                }
            }
        }
        out.sort_by_key(|t| t.instant_ms);
        out.dedup_by_key(|t| t.instant_ms);
        out
    }

    fn transition(&self, i: usize) -> Transition {
        let b = self.bascules[i];
        Transition {
            instant_ms: b.epoch_s * 1000,
            avant: b.avant,
            apres: b.apres,
        }
    }

    /// `previousTransition` : la derniere bascule strictement avant l'instant.
    fn transition_precedente(&self, epoch_s: i64) -> Option<Transition> {
        let derniere = *self.bascules.last()?;
        if !self.annuelles.is_empty() && epoch_s > derniere.epoch_s {
            let annee = annee_de(epoch_s + i64::from(derniere.apres));
            for annee in [annee, annee - 1] {
                if let Some(t) = self
                    .transitions_de_l_annee(annee)
                    .into_iter()
                    .rev()
                    .find(|t| epoch_s > div_plancher(t.instant_ms, 1000))
                {
                    if div_plancher(t.instant_ms, 1000) > derniere.epoch_s {
                        return Some(t);
                    }
                }
            }
        }
        let i = match self.bascules.binary_search_by(|b| b.epoch_s.cmp(&epoch_s)) {
            Ok(i) => i,
            Err(i) => i,
        };
        (i > 0).then(|| self.transition(i - 1))
    }
}

impl RegleAnnuelle {
    /// `ZoneOffsetTransitionRule.createTransition(year)`.
    fn transition(&self, annee: i32) -> Option<Transition> {
        use time::{Date, Month};

        let mois = Month::try_from(self.mois).ok()?;
        let mut date = if self.jour < 0 {
            let dernier = time::util::days_in_month(mois, annee);
            let jour = i32::from(dernier) + 1 + i32::from(self.jour);
            let date = Date::from_calendar_date(annee, mois, u8::try_from(jour).ok()?).ok()?;
            match self.jour_semaine {
                0 => date,
                dow => recule_jusqu_a(date, dow),
            }
        } else {
            let date = Date::from_calendar_date(annee, mois, u8::try_from(self.jour).ok()?).ok()?;
            match self.jour_semaine {
                0 => date,
                dow => avance_jusqu_a(date, dow),
            }
        };
        if self.minuit_fin_de_jour {
            date = date.next_day()?;
        }
        let local = jour_en_ms(date) + i64::from(self.seconde_du_jour) * 1000;
        // Les trois referentiels de l'heure de bascule : la regle europeenne
        // est ecrite en UTC, celle des Etats-Unis en heure locale.
        let local_mur = match self.definition {
            0 => local + i64::from(self.avant) * 1000,
            2 => local + i64::from(self.avant - self.standard) * 1000,
            _ => local,
        };
        Some(Transition {
            instant_ms: local_mur - i64::from(self.avant) * 1000,
            avant: self.avant,
            apres: self.apres,
        })
    }
}

fn avance_jusqu_a(date: time::Date, jour_semaine: u8) -> time::Date {
    let mut date = date;
    while date.weekday().number_from_monday() != jour_semaine {
        date = date.next_day().unwrap_or(date);
    }
    date
}

fn recule_jusqu_a(date: time::Date, jour_semaine: u8) -> time::Date {
    let mut date = date;
    while date.weekday().number_from_monday() != jour_semaine {
        date = date.previous_day().unwrap_or(date);
    }
    date
}

fn jour_en_ms(date: time::Date) -> i64 {
    time::PrimitiveDateTime::new(date, time::Time::MIDNIGHT)
        .assume_utc()
        .unix_timestamp()
        * 1000
}

/// L'annee d'un instant lu dans un decalage donne — le `findYear` de Java.
fn annee_de(epoch_s: i64) -> i32 {
    time::OffsetDateTime::from_unix_timestamp(epoch_s.clamp(-62_135_596_800, 253_402_300_799))
        .map_or(1970, |d| d.year())
}

/// La division qui arrondit vers le bas, y compris pour un instant negatif :
/// `-1 ms` est dans la seconde `-1`, pas dans la seconde `0`.
pub fn div_plancher(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}

// ---------------------------------------------------------------------------
// La lecture de la table generee

/// L'index `nom de zone -> decalage du jeu de regles dans la table`, lu une
/// fois.
fn index() -> &'static HashMap<String, u32> {
    static INDEX: OnceLock<HashMap<String, u32>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut lecteur = Lecteur::new(tzdata::TABLE);
        if lecteur.octets(4) != b"FTZ1" {
            return HashMap::new();
        }
        let n = lecteur.u8() as usize;
        lecteur.octets(n); // la version du tzdb, deja publiee par `tzdata`
        let nb_zones = lecteur.varint() as usize;
        let mut zones = Vec::with_capacity(nb_zones);
        for _ in 0..nb_zones {
            let n = lecteur.u8() as usize;
            let nom = String::from_utf8_lossy(lecteur.octets(n)).into_owned();
            zones.push((nom, lecteur.varint() as usize));
        }
        let nb_jeux = lecteur.varint() as usize;
        let mut table = Vec::with_capacity(nb_jeux);
        for _ in 0..nb_jeux {
            let mut quatre = [0u8; 4];
            quatre.copy_from_slice(lecteur.octets(4));
            table.push(u32::from_le_bytes(quatre));
        }
        zones
            .into_iter()
            .filter_map(|(nom, jeu)| table.get(jeu).map(|d| (nom, *d)))
            .collect()
    })
}

/// Les jeux de regles deja decodes.
///
/// Le decodage est fait a la demande, zone par zone : decoder la table entiere
/// couterait 18 078 transitions en memoire pour un serveur qui n'en emploiera
/// jamais qu'une poignee — et le RSS au repos est un chiffre publie.
fn cache() -> &'static RwLock<HashMap<String, Arc<Regles>>> {
    static CACHE: OnceLock<RwLock<HashMap<String, Arc<Regles>>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn charge(nom: &str) -> Option<Arc<Regles>> {
    if let Some(deja) = cache().read().ok()?.get(nom) {
        return Some(deja.clone());
    }
    let decalage = *index().get(nom)?;
    let regles = Arc::new(decode_jeu(tzdata::TABLE, decalage as usize));
    if let Ok(mut c) = cache().write() {
        c.insert(nom.to_string(), regles.clone());
    }
    Some(regles)
}

fn decode_jeu(table: &'static [u8], debut: usize) -> Regles {
    let mut l = Lecteur::new(&table[debut..]);
    let nb_offsets = l.varint() as usize;
    let offsets: Vec<i32> = (0..nb_offsets).map(|_| l.zigzag() as i32).collect();
    let lit = |i: usize| offsets.get(i).copied().unwrap_or(0);
    let init = lit(l.varint() as usize);
    let nb = l.varint() as usize;
    let mut bascules = Vec::with_capacity(nb);
    let mut epoch = 0i64;
    let mut avant = init;
    for _ in 0..nb {
        epoch += l.zigzag();
        let apres = lit(l.varint() as usize);
        bascules.push(Bascule {
            epoch_s: epoch,
            avant,
            apres,
        });
        avant = apres;
    }
    let nb = l.varint() as usize;
    let mut annuelles = Vec::with_capacity(nb);
    for _ in 0..nb {
        let mois = l.u8();
        let jour = l.zigzag() as i8;
        let jour_semaine = l.u8();
        let minuit_fin_de_jour = l.u8() != 0;
        let definition = l.u8();
        let seconde_du_jour = l.varint() as i32;
        annuelles.push(RegleAnnuelle {
            mois,
            jour,
            jour_semaine,
            seconde_du_jour,
            minuit_fin_de_jour,
            definition,
            standard: lit(l.varint() as usize),
            avant: lit(l.varint() as usize),
            apres: lit(l.varint() as usize),
        });
    }
    let mut decalages: Vec<i32> = offsets;
    decalages.sort_unstable();
    decalages.dedup();
    Regles {
        init,
        bascules,
        annuelles,
        decalages,
    }
}

struct Lecteur<'a> {
    octets: &'a [u8],
    pos: usize,
}

impl<'a> Lecteur<'a> {
    fn new(octets: &'a [u8]) -> Self {
        Self { octets, pos: 0 }
    }

    fn u8(&mut self) -> u8 {
        let v = self.octets.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        v
    }

    fn octets(&mut self, n: usize) -> &'a [u8] {
        let fin = (self.pos + n).min(self.octets.len());
        let out = &self.octets[self.pos.min(fin)..fin];
        self.pos += n;
        out
    }

    fn varint(&mut self) -> u64 {
        let mut n = 0u64;
        let mut decalage = 0;
        loop {
            let octet = self.u8();
            n |= u64::from(octet & 0x7F) << decalage;
            if octet & 0x80 == 0 {
                return n;
            }
            decalage += 7;
        }
    }

    fn zigzag(&mut self) -> i64 {
        let n = self.varint();
        if n & 1 == 1 {
            -(((n + 1) >> 1) as i64)
        } else {
            (n >> 1) as i64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str) -> i64 {
        let f = crate::dateformat::DateFormat::default();
        f.lit_avec_residu(&serde_json::json!(s)).unwrap().0
    }

    #[test]
    fn la_table_porte_les_zones_d_elasticsearch() {
        assert_eq!(index().len(), tzdata::NB_ZONES);
        for nom in ["Europe/Paris", "America/New_York", "Asia/Kolkata", "UTC"] {
            assert!(index().contains_key(nom), "{nom} manque");
        }
    }

    #[test]
    fn decalage_selon_l_instant() {
        let paris = Fuseau::parse("Europe/Paris").unwrap();
        // Heure d'hiver / heure d'ete, de part et d'autre de la bascule de
        // 2026 (le dernier dimanche de mars a 01:00 UTC).
        assert_eq!(paris.decalage(ms("2026-01-15T12:00:00Z")), 3600);
        assert_eq!(paris.decalage(ms("2026-03-29T00:59:59Z")), 3600);
        assert_eq!(paris.decalage(ms("2026-03-29T01:00:00Z")), 7200);
        assert_eq!(paris.decalage(ms("2026-10-25T00:59:59Z")), 7200);
        assert_eq!(paris.decalage(ms("2026-10-25T01:00:00Z")), 3600);
        // Une annee que la table ne porte pas : la regle annuelle prend le
        // relais (2044 est bien au-dela de la derniere transition connue).
        assert_eq!(paris.decalage(ms("2044-07-01T12:00:00Z")), 7200);
        assert_eq!(paris.decalage(ms("2044-01-01T12:00:00Z")), 3600);
    }

    #[test]
    fn une_heure_locale_qui_n_existe_pas() {
        let paris = Fuseau::parse("Europe/Paris").unwrap();
        // 2026-03-29, 02:30 heure locale : le trou.
        let trou = ms("2026-03-29T02:30:00Z");
        assert!(paris.decalages_valides(trou).is_empty());
        let t = paris.transition_du_trou(trou).unwrap();
        assert_eq!(t.instant_ms, ms("2026-03-29T01:00:00Z"));
        assert!(t.est_un_trou());
        // 2026-10-25, 02:30 locale : elle existe deux fois, la plus tot
        // d'abord.
        let double = ms("2026-10-25T02:30:00Z");
        assert_eq!(paris.decalages_valides(double), vec![7200, 3600]);
        // Une heure ordinaire n'a qu'une lecture.
        assert_eq!(
            paris.decalages_valides(ms("2026-06-01T12:00:00Z")),
            vec![7200]
        );
    }

    #[test]
    fn transition_precedente() {
        let paris = Fuseau::parse("Europe/Paris").unwrap();
        let t = paris
            .transition_precedente(ms("2026-06-01T00:00:00Z"))
            .unwrap();
        assert_eq!(t.instant_ms, ms("2026-03-29T01:00:00Z"));
        // Strictement avant : l'instant de la bascule elle-meme rend la
        // precedente, pas elle.
        let t = paris
            .transition_precedente(ms("2026-03-29T01:00:00Z"))
            .unwrap();
        assert_eq!(t.instant_ms, ms("2025-10-26T01:00:00Z"));
    }

    #[test]
    fn les_decalages_litteraux() {
        assert_eq!(Fuseau::parse("+05:30").unwrap().decalage(0), 19800);
        assert_eq!(Fuseau::parse("-08:00").unwrap().decalage(0), -28800);
        assert_eq!(Fuseau::parse("+02").unwrap().decalage(0), 7200);
        assert_eq!(Fuseau::parse("-0130").unwrap().decalage(0), -5400);
        assert_eq!(Fuseau::parse("Z").unwrap().decalage(0), 0);
        assert_eq!(Fuseau::parse("UTC").unwrap().decalage(0), 0);
        assert_eq!(Fuseau::parse("UTC+01:00").unwrap().decalage(0), 3600);
        assert!(Fuseau::parse("Europe/Nulle_Part").is_err());
        assert!(Fuseau::parse("+99:00").is_err());
    }

    #[test]
    fn une_zone_sans_heure_d_ete() {
        let kolkata = Fuseau::parse("Asia/Kolkata").unwrap();
        assert_eq!(kolkata.decalage(ms("2026-01-15T12:00:00Z")), 19800);
        assert_eq!(kolkata.decalage(ms("2026-07-15T12:00:00Z")), 19800);
        let phoenix = Fuseau::parse("America/Phoenix").unwrap();
        assert_eq!(phoenix.decalage(ms("2026-07-15T12:00:00Z")), -25200);
    }
}
