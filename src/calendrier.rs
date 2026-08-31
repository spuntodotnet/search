//! L'arrondi d'un `date_histogram` : de quel seau releve un instant, et quel
//! est le seau suivant.
//!
//! Un mois n'est pas trente jours et un jour n'est pas toujours vingt-quatre
//! heures. `fixed_interval` ne sait donc pas dire « par mois », et il ne sait
//! pas dire « par jour a Paris » : le dimanche de mars y dure 23 heures, celui
//! d'octobre 25. C'est ce que ce module calcule.
//!
//! **Ce qui est reproduit ici est `org.elasticsearch.common.Rounding`**, pas
//! une idee de l'arrondi calendaire — la meme demarche que le
//! `UnifiedHighlighter` de `src/highlight.rs`. Trois raisons, toutes mesurees :
//!
//! - le seau d'une heure locale **qui n'existe pas** (le trou de mars) n'est
//!   pas devinable : pour un seau qui tombe a minuit, ES prend l'instant de la
//!   bascule (mesure : a Santiago le seau du 2024-09-08 commence a `01:00-03:00`) ;
//!   pour un seau plus court, il repart de **juste avant** le trou ;
//! - une heure locale qui existe **deux fois** (le dimanche d'octobre) n'est
//!   pas tranchee de la meme facon selon que l'unite tombe a minuit ou non ;
//! - un `fixed_interval` avec un fuseau n'est plus fixe du tout : mesure contre
//!   ES 8.15, un seau `3h` pose sur la nuit du changement d'heure a Paris dure
//!   **quatre** heures reelles.
//!
//! L'oracle n'est pas une lecture : `tests/arrondi_vs_es.rs` rejoue
//! `tests/donnees/arrondis.jsonl`, une grille de couples (zone, intervalle,
//! instant) dont les reponses sont celles de **la classe `Rounding` d'ES**
//! elle-meme, executee dans le conteneur de reference
//! (`tests/compat/genere_fuseaux.py --grille`).

use crate::error::{EsError, EsResult};
use crate::fuseau::{div_plancher, Fuseau};

/// Une unite de calendrier — celles que `calendar_interval` accepte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unite {
    Seconde,
    Minute,
    Heure,
    Jour,
    Semaine,
    Mois,
    Trimestre,
    Annee,
}

impl Unite {
    /// L'unite retombe-t-elle sur un minuit local ?
    ///
    /// C'est la ligne de partage d'ES (`unitRoundsToMidnight`) : au-dela de
    /// l'heure, l'arrondi passe par une date locale et non par une division.
    fn vers_minuit(self) -> bool {
        matches!(
            self,
            Self::Jour | Self::Semaine | Self::Mois | Self::Trimestre | Self::Annee
        )
    }

    /// La duree de l'unite, pour celles qui en ont une constante.
    fn duree_ms(self) -> i64 {
        match self {
            Self::Seconde => 1_000,
            Self::Minute => 60_000,
            Self::Heure => 3_600_000,
            Self::Jour => 86_400_000,
            Self::Semaine => 7 * 86_400_000,
            // Sans objet : les unites de mois ne servent jamais de duree.
            Self::Mois | Self::Trimestre | Self::Annee => 0,
        }
    }

    pub fn nom(self) -> &'static str {
        match self {
            Self::Seconde => "second",
            Self::Minute => "minute",
            Self::Heure => "hour",
            Self::Jour => "day",
            Self::Semaine => "week",
            Self::Mois => "month",
            Self::Trimestre => "quarter",
            Self::Annee => "year",
        }
    }
}

/// L'intervalle d'un `date_histogram` : calendaire ou fixe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intervalle {
    Calendaire(Unite),
    /// Une duree en millisecondes.
    Fixe(i64),
}

/// Un arrondi complet : l'intervalle, le fuseau, et le decalage de `offset`.
#[derive(Debug, Clone)]
pub struct Arrondi {
    intervalle: Intervalle,
    fuseau: Fuseau,
    /// `offset` du `date_histogram`, en millisecondes.
    offset_ms: i64,
}

impl Arrondi {
    pub fn new(intervalle: Intervalle, fuseau: Fuseau, offset_ms: i64) -> Self {
        Self {
            intervalle,
            fuseau,
            offset_ms,
        }
    }

    pub fn fuseau(&self) -> &Fuseau {
        &self.fuseau
    }

    pub fn intervalle(&self) -> Intervalle {
        self.intervalle
    }

    /// Le debut du seau qui contient cet instant.
    ///
    /// `OffsetRounding` d'ES : le decalage se retire avant l'arrondi et se
    /// remet apres.
    pub fn arrondit(&self, instant_ms: i64) -> i64 {
        self.sans_offset(instant_ms - self.offset_ms) + self.offset_ms
    }

    /// Le debut du seau **suivant**.
    pub fn suivant(&self, instant_ms: i64) -> i64 {
        self.suivant_sans_offset(instant_ms - self.offset_ms) + self.offset_ms
    }

    fn sans_offset(&self, instant_ms: i64) -> i64 {
        match self.intervalle {
            Intervalle::Calendaire(u) if u.vers_minuit() => {
                let local = self.fuseau.vers_local(instant_ms);
                self.premier_instant_du_jour(tronque_local(local, u))
            }
            Intervalle::Calendaire(u) => self.arrondi_court(instant_ms, u),
            Intervalle::Fixe(interval) => self.arrondi_fixe(instant_ms, interval),
        }
    }

    fn suivant_sans_offset(&self, instant_ms: i64) -> i64 {
        match self.intervalle {
            Intervalle::Calendaire(u) if u.vers_minuit() => {
                let local = self.fuseau.vers_local(instant_ms);
                let minuit = tronque_local(local, u);
                self.premier_instant_du_jour(minuit_suivant(minuit, u))
            }
            Intervalle::Calendaire(u) => {
                // `AbstractNotToMidnightRounding.nextRoundingValue` : une unite
                // plus loin, et deux si la premiere n'a pas suffi — ce qui
                // arrive quand l'unite entiere est avalee par une bascule.
                let une = self.arrondi_court(instant_ms + u.duree_ms(), u);
                if instant_ms < une {
                    une
                } else {
                    self.arrondi_court(instant_ms + 2 * u.duree_ms(), u)
                }
            }
            Intervalle::Fixe(interval) => self.suivant_fixe(instant_ms, interval),
        }
    }

    /// `firstTimeOnDay` : ce que vaut un minuit local.
    ///
    /// S'il y a au moins un instant qui porte cette heure locale, c'est le plus
    /// tot des deux ; s'il n'y en a aucun — le jour a commence par une bascule
    /// — c'est l'instant de la bascule.
    fn premier_instant_du_jour(&self, minuit_local: i64) -> i64 {
        let valides = self.fuseau.decalages_valides(minuit_local);
        match valides.first() {
            Some(d) => Fuseau::vers_instant(minuit_local, *d),
            None => self
                .fuseau
                .transition_du_trou(minuit_local)
                .map_or(minuit_local, |t| t.instant_ms),
        }
    }

    /// `JavaTimeNotToMidnightRounding.round` : seconde, minute, heure.
    ///
    /// La boucle n'est pas decorative. Tronquer l'heure locale puis la
    /// reconvertir peut **traverser une bascule** : le resultat serait alors
    /// posterieur a une transition que l'instant d'origine precede. ES revient
    /// alors juste avant la transition et recommence.
    fn arrondi_court(&self, instant_ms: i64, u: Unite) -> i64 {
        let mut instant = instant_ms;
        loop {
            let tronque = self.tronque_en_heure_locale(instant, u);
            let precedente = self.fuseau.transition_precedente(instant);
            let Some(t) = precedente else {
                // Sans transition avant, la troncature ne peut pas avoir echoue.
                return tronque.unwrap_or(instant);
            };
            if let Some(tronque) = tronque {
                if t.instant_ms <= tronque {
                    return tronque;
                }
            }
            instant = t.instant_ms - 1;
        }
    }

    /// `truncateAsLocalTime` : l'heure locale tronquee, ramenee a un instant
    /// **au plus tard** egal a celui d'ou l'on part.
    ///
    /// `None` quand l'heure locale visee n'a pas existe (elle tombe dans un
    /// trou) : l'appelant repart alors d'avant la bascule.
    fn tronque_en_heure_locale(&self, instant_ms: i64, u: Unite) -> Option<i64> {
        let local = self.fuseau.vers_local(instant_ms);
        let tronque = tronque_local(local, u);
        let valides = self.fuseau.decalages_valides(tronque);
        // Le plus tard des instants possibles qui ne depasse pas l'entree.
        valides
            .iter()
            .rev()
            .map(|d| Fuseau::vers_instant(tronque, *d))
            .find(|candidat| *candidat <= instant_ms)
    }

    /// `TimeIntervalRounding.JavaTimeRounding.round`.
    fn arrondi_fixe(&self, instant_ms: i64, interval: i64) -> i64 {
        let mut instant = instant_ms;
        // ES s'arrete au bout de 5 000 tours ; le plus qu'il ait observe est
        // 500. La borne est reprise telle quelle.
        for _ in 0..5_000 {
            let local = self.fuseau.vers_local(instant);
            let arrondi_local = cle_de_seau(local, interval) * interval;
            let valides = self.fuseau.decalages_valides(arrondi_local);
            if valides.is_empty() {
                // L'heure locale visee n'a pas existe : le seau commence a la
                // bascule.
                return self
                    .fuseau
                    .transition_du_trou(arrondi_local)
                    .map_or(arrondi_local, |t| t.instant_ms);
            }
            let precedente = self.fuseau.transition_precedente(instant + 1);
            let mut recommence = false;
            for d in valides.iter().rev() {
                let candidat = Fuseau::vers_instant(arrondi_local, *d);
                if let Some(t) = precedente {
                    if candidat < t.instant_ms {
                        // Arrondir a travers la bascule donnerait un seau
                        // faux : on revient a la bascule et on recommence.
                        instant = t.instant_ms - 1;
                        recommence = true;
                        break;
                    }
                }
                if candidat <= instant {
                    return candidat;
                }
            }
            if !recommence {
                return Fuseau::vers_instant(arrondi_local, valides[0]);
            }
        }
        instant
    }

    /// `TimeIntervalRounding.JavaTimeRounding.nextRoundingValue` : la recherche
    /// mi-dichotomique d'ES, reprise telle quelle.
    ///
    /// Elle a l'air d'un bricolage, et c'en est un — celui d'ES. Le seau
    /// suivant d'un intervalle fixe pose dans un fuseau n'est pas
    /// `seau + intervalle` : la nuit du changement d'heure, il est a
    /// `seau + 4 h` pour un intervalle de 3 h.
    fn suivant_fixe(&self, instant_ms: i64, interval: i64) -> i64 {
        let precedent = self.arrondi_fixe(instant_ms, interval);
        let mut increment = interval;
        let mut depuis = precedent;
        for _ in 0..100 {
            depuis += increment;
            let arrondi = self.arrondi_fixe(depuis, interval);
            if arrondi <= precedent {
                if increment < 0 {
                    increment = -increment / 2;
                }
                continue;
            }
            if self.arrondi_fixe(arrondi - 1, interval) > precedent {
                if increment > 0 {
                    increment = -increment / 2;
                }
                continue;
            }
            return arrondi;
        }
        precedent + interval
    }
}

/// `roundKey` d'ES : une division qui tronque vers zero, corrigee sous zero.
fn cle_de_seau(valeur: i64, interval: i64) -> i64 {
    if valeur < 0 {
        (valeur - interval + 1) / interval
    } else {
        valeur / interval
    }
}

/// `truncateLocalDateTime` : l'heure locale ramenee au debut de son unite.
///
/// L'heure locale est comptee comme ES la compte — des millisecondes lues sans
/// decalage — donc les unites courtes sont des divisions, et seules les unites
/// de calendrier passent par une date.
fn tronque_local(local_ms: i64, u: Unite) -> i64 {
    match u {
        Unite::Seconde => div_plancher(local_ms, 1_000) * 1_000,
        Unite::Minute => div_plancher(local_ms, 60_000) * 60_000,
        Unite::Heure => div_plancher(local_ms, 3_600_000) * 3_600_000,
        Unite::Jour => div_plancher(local_ms, 86_400_000) * 86_400_000,
        Unite::Semaine => {
            // La semaine ISO commence le lundi ; le 1er janvier 1970 etait un
            // jeudi.
            let jours = div_plancher(local_ms, 86_400_000);
            let depuis_lundi = (jours + 3).rem_euclid(7);
            (jours - depuis_lundi) * 86_400_000
        }
        Unite::Mois | Unite::Trimestre | Unite::Annee => {
            let (annee, mois, _) = date_locale(local_ms);
            let mois = match u {
                Unite::Mois => mois,
                Unite::Trimestre => (mois - 1) / 3 * 3 + 1,
                _ => 1,
            };
            debut_du_mois(annee, mois)
        }
    }
}

/// Le minuit local suivant, pour l'unite donnee — `nextRelevantMidnight`.
fn minuit_suivant(minuit_local: i64, u: Unite) -> i64 {
    match u {
        Unite::Jour => minuit_local + 86_400_000,
        Unite::Semaine => minuit_local + 7 * 86_400_000,
        Unite::Mois => decale_mois(minuit_local, 1),
        Unite::Trimestre => decale_mois(minuit_local, 3),
        Unite::Annee => decale_mois(minuit_local, 12),
        // Sans objet : ces unites ne tombent pas a minuit.
        _ => minuit_local,
    }
}

fn date_locale(local_ms: i64) -> (i32, u8, u8) {
    match time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(local_ms) * 1_000_000) {
        Ok(dt) => (dt.year(), u8::from(dt.month()), dt.day()),
        Err(_) => (1970, 1, 1),
    }
}

fn debut_du_mois(annee: i32, mois: u8) -> i64 {
    let mois = time::Month::try_from(mois).unwrap_or(time::Month::January);
    match time::Date::from_calendar_date(annee, mois, 1) {
        Ok(d) => d.midnight().assume_utc().unix_timestamp() * 1000,
        Err(_) => 0,
    }
}

/// Un decalage de `n` mois sur un debut de mois local.
fn decale_mois(minuit_local: i64, n: i32) -> i64 {
    let (annee, mois, _) = date_locale(minuit_local);
    let total = i64::from(annee) * 12 + i64::from(mois) - 1 + i64::from(n);
    let annee = i32::try_from(total.div_euclid(12)).unwrap_or(1970);
    let mois = u8::try_from(total.rem_euclid(12) + 1).unwrap_or(1);
    debut_du_mois(annee, mois)
}

// ---------------------------------------------------------------------------
// La lecture des parametres

/// `calendar_interval` : une unite, et **une seule** — `2d` est refuse par ES.
///
/// Mesure contre ES 8.15 : `1d`, `day`, `1w`, `week`, `1M`, `month`, `1q`,
/// `quarter`, `1y`, `year`, `1h`, `hour`, `1m`, `minute`, `1s`, `second` sont
/// acceptes ; `2d`, `90m`, `1.5h`, `0d`, `1H` et `MONTH` sont refuses.
pub fn lit_calendaire(v: &str) -> Option<Unite> {
    Some(match v {
        "1s" | "second" => Unite::Seconde,
        "1m" | "minute" => Unite::Minute,
        "1h" | "hour" => Unite::Heure,
        "1d" | "day" => Unite::Jour,
        "1w" | "week" => Unite::Semaine,
        "1M" | "month" => Unite::Mois,
        "1q" | "quarter" => Unite::Trimestre,
        "1y" | "year" => Unite::Annee,
        _ => return None,
    })
}

/// `fixed_interval` : un nombre et une unite de duree (`ms`, `s`, `m`, `h`,
/// `d`), y compris un multiple. `w`, `M`, `q` et `y` y sont refuses par ES —
/// ce sont des unites de calendrier, dont la duree n'est pas constante.
pub fn lit_fixe(v: &str) -> Option<i64> {
    let fin = v.len()
        - v.chars()
            .rev()
            .take_while(|c| c.is_ascii_alphabetic())
            .count();
    let (nombre, unite) = v.split_at(fin);
    let n: i64 = nombre.trim().parse().ok()?;
    let ms = match unite {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" | "H" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    n.checked_mul(ms)
}

/// `offset` d'un `date_histogram` : `+6h`, `-2d`, `1h`.
///
/// La forme est celle d'une duree signee ; ES accepte les memes unites que
/// `fixed_interval`.
pub fn lit_offset(v: &str) -> EsResult<i64> {
    let refus = || {
        EsError::illegal_argument(format!(
            "failed to parse setting [offset] with value [{v}] as a time value"
        ))
    };
    let (signe, reste) = match v.as_bytes().first() {
        Some(b'+') => (1, &v[1..]),
        Some(b'-') => (-1, &v[1..]),
        _ => (1, v),
    };
    lit_fixe(reste).map(|ms| signe * ms).ok_or_else(refus)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str) -> i64 {
        crate::dateformat::DateFormat::default()
            .lit_avec_residu(&serde_json::json!(s))
            .unwrap()
            .0
    }

    fn arrondi(intervalle: Intervalle, zone: &str) -> Arrondi {
        Arrondi::new(intervalle, Fuseau::parse(zone).unwrap(), 0)
    }

    #[test]
    fn un_jour_de_paris_dure_parfois_vingt_trois_heures() {
        let a = arrondi(Intervalle::Calendaire(Unite::Jour), "Europe/Paris");
        let debut = a.arrondit(ms("2026-03-29T05:00:00Z"));
        assert_eq!(debut, ms("2026-03-28T23:00:00Z"));
        let suivant = a.suivant(debut);
        assert_eq!(suivant, ms("2026-03-29T22:00:00Z"));
        assert_eq!(suivant - debut, 23 * 3_600_000);
        // Et vingt-cinq en octobre.
        let debut = a.arrondit(ms("2026-10-25T05:00:00Z"));
        assert_eq!(a.suivant(debut) - debut, 25 * 3_600_000);
    }

    #[test]
    fn un_mois_civil_n_est_pas_trente_jours() {
        let a = arrondi(Intervalle::Calendaire(Unite::Mois), "UTC");
        assert_eq!(
            a.arrondit(ms("2024-02-29T12:00:00Z")),
            ms("2024-02-01T00:00:00Z")
        );
        assert_eq!(
            a.suivant(ms("2024-02-01T00:00:00Z")),
            ms("2024-03-01T00:00:00Z")
        );
        // Fevrier 2024 : 29 jours.
        assert_eq!(
            a.suivant(ms("2024-02-01T00:00:00Z")) - ms("2024-02-01T00:00:00Z"),
            29 * 86_400_000
        );
    }

    #[test]
    fn le_seau_d_un_minuit_qui_n_existe_pas() {
        // A Santiago, le 8 septembre 2024 commence a 01:00 : minuit n'a pas eu
        // lieu. ES pose le seau a l'instant de la bascule (mesure).
        let a = arrondi(Intervalle::Calendaire(Unite::Jour), "America/Santiago");
        assert_eq!(
            a.arrondit(ms("2024-09-08T12:00:00Z")),
            ms("2024-09-08T04:00:00Z")
        );
    }

    #[test]
    fn un_intervalle_fixe_ne_l_est_plus_dans_un_fuseau() {
        // Mesure contre ES 8.15 : dans la nuit du retour a l'heure d'hiver, le
        // seau de 3 h qui commence a 00:00+02:00 dure quatre heures reelles.
        let a = arrondi(Intervalle::Fixe(3 * 3_600_000), "Europe/Paris");
        let debut = a.arrondit(ms("2026-10-24T23:00:00Z"));
        assert_eq!(debut, ms("2026-10-24T22:00:00Z"));
        assert_eq!(a.suivant(debut), ms("2026-10-25T02:00:00Z"));
    }

    #[test]
    fn les_semaines_commencent_le_lundi() {
        let a = arrondi(Intervalle::Calendaire(Unite::Semaine), "UTC");
        // Le 15 mars 2026 est un dimanche : sa semaine commence le 9.
        assert_eq!(
            a.arrondit(ms("2026-03-15T10:00:00Z")),
            ms("2026-03-09T00:00:00Z")
        );
    }

    #[test]
    fn l_offset_deplace_les_bornes() {
        let a = Arrondi::new(
            Intervalle::Calendaire(Unite::Jour),
            Fuseau::utc(),
            6 * 3_600_000,
        );
        assert_eq!(
            a.arrondit(ms("2026-03-15T03:00:00Z")),
            ms("2026-03-14T06:00:00Z")
        );
        assert_eq!(
            a.arrondit(ms("2026-03-15T07:00:00Z")),
            ms("2026-03-15T06:00:00Z")
        );
    }

    #[test]
    fn les_intervalles_se_lisent_comme_chez_es() {
        assert_eq!(lit_calendaire("month"), Some(Unite::Mois));
        assert_eq!(lit_calendaire("1M"), Some(Unite::Mois));
        assert_eq!(lit_calendaire("2d"), None);
        assert_eq!(lit_calendaire("90m"), None);
        assert_eq!(lit_calendaire("1H"), None);
        assert_eq!(lit_fixe("90m"), Some(90 * 60_000));
        assert_eq!(lit_fixe("7d"), Some(7 * 86_400_000));
        assert_eq!(lit_fixe("1ms"), Some(1));
        assert_eq!(lit_fixe("1w"), None);
        assert_eq!(lit_fixe("1M"), None);
        assert_eq!(lit_offset("+6h").unwrap(), 6 * 3_600_000);
        assert_eq!(lit_offset("-1d").unwrap(), -86_400_000);
        assert!(lit_offset("1w").is_err());
    }
}
