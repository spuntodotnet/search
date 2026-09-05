//! `date_histogram` : les seaux d'un graphe temporel, calcules par ferrite.
//!
//! tantivy sait grouper par intervalle **fixe**, et c'est tout ce que ferrite
//! servait : `calendar_interval` etait refuse (« un mois n'a pas d'equivalent
//! chez tantivy ») et `time_zone` avec lui. Or « par mois » est la maille d'un
//! graphe temporel, et un graphe « par jour » a Paris n'a pas les memes seaux
//! qu'en UTC.
//!
//! Le pari de la carte etait qu'on pouvait s'en passer : **le seau d'une date
//! est une fonction pure du calendrier**, que ferrite peut appliquer lui-meme —
//! le raisonnement qui a rendu l'agregation `filter` possible. La mesure lui
//! donne raison, a une condition pres qui a decide de tout le reste du module :
//! ce qu'il faut appliquer soi-meme, ce n'est pas *un seau a la fois*, c'est la
//! **liste des bornes**. Une fois cette liste connue, l'agregation devient un
//! `range` — que tantivy execute, avec ses sous-agregations, ses seaux vides et
//! sa fusion multi-index.
//!
//! D'ou la mecanique, en trois temps :
//!
//! 1. une **pre-passe** demande le `min` et le `max` du champ sur la meme
//!    requete. C'est exactement ce qu'ES connait au moment de remplir les
//!    trous : ses seaux vont du premier seau non vide au dernier ;
//! 2. les bornes sont deroulees par [`crate::calendrier`], qui reproduit
//!    l'arrondi d'ES, fuseau compris ;
//! 3. la demande est reecrite en `range` **contigu**, puis le resultat est
//!    remis en forme de `date_histogram`.
//!
//! Trois details que la mesure a imposes :
//!
//! - **le rognage se fait par seau parent.** Sous un `terms`, chaque categorie
//!   a ses propres premier et dernier seaux non vides (mesure contre ES 8.15 :
//!   la categorie `b` commence deux jours apres la categorie `a`). Les bornes,
//!   elles, sont globales : les seaux vides des bords sont donc retires
//!   categorie par categorie ;
//! - **`hard_bounds` et `extended_bounds` sont arrondis** avant d'etre
//!   appliques, et pas du meme cote : la borne haute de `hard_bounds` est
//!   **exclue** (mesure : `max: "2026-03-06"` ne rend pas le seau du 6, et
//!   `max: "2026-03-06T06:00"` non plus — il est arrondi au seau du 6, puis
//!   exclu), celle d'`extended_bounds` est **incluse** ;
//! - **une borne en nanosecondes ne rentre pas toujours dans un `f64`.**
//!   L'agregation `range` de tantivy prend des flottants ; a l'echelle de 2026,
//!   un nanoseconde-flottant ne represente plus les entiers qu'a 256 pres. Une
//!   borne arrondie **vers le haut** rejette dans le seau precedent un document
//!   pose exactement dessus. Ce que la mesure precise — et c'est la moitie qui
//!   compte : une borne a la **seconde pleine** est toujours exacte (un
//!   milliard de nanosecondes est un multiple de 256), donc tous les seaux
//!   d'un `calendar_interval` et de tout `fixed_interval` en secondes le sont
//!   aussi ; ce sont les bornes **sous-seconde** qui debordent, 3 750 fois sur
//!   10 000 millisecondes consecutives. Chaque borne est donc ramenee au
//!   flottant immediatement **inferieur** ou egal, et la sonde le prouve : sans
//!   cette ligne, `fixed_interval: 1ms` et `3ms` divergent d'ES.

use serde_json::{json, Map, Value};

use crate::calendrier::{lit_calendaire, lit_fixe, lit_offset, Arrondi, Intervalle};
use crate::dateformat::DateFormat;
use crate::error::{EsError, EsResult};
use crate::fuseau::Fuseau;

/// Comment l'appelant remet en forme les sous-agregations d'un seau : lui seul
/// connait leurs metadonnees.
pub type MiseEnForme<'a> = dyn Fn(&Map<String, Value>) -> Map<String, Value> + 'a;

/// Les bornes d'un `extended_bounds` ou d'un `hard_bounds`.
#[derive(Debug, Clone, Copy, Default)]
struct Bornes {
    min: Option<i64>,
    max: Option<i64>,
}

/// Un `date_histogram` valide, pret a etre execute.
#[derive(Debug, Clone)]
pub struct Histo {
    pub champ: String,
    arrondi: Arrondi,
    min_doc_count: u64,
    extended: Option<Bornes>,
    hard: Option<Bornes>,
    /// Le `format` de l'agregation, s'il y en a un ; sinon celui du champ.
    format: DateFormat,
    keyed: bool,
    /// Les bornes des seaux, en millisecondes — remplies par la pre-passe.
    /// `n + 1` bornes pour `n` seaux.
    bornes: Vec<i64>,
}

/// Le nombre de seaux qu'un seul `date_histogram` peut produire.
///
/// C'est la limite d'ES (`search.max_buckets`), et elle vaut ici la meme
/// chose : sans elle, un `calendar_interval: minute` pose sur dix ans
/// demanderait cinq millions d'intervalles a tantivy.
const MAX_SEAUX: usize = 65_536;

impl Histo {
    /// Lit et valide un corps de `date_histogram`.
    ///
    /// `format_du_champ` est celui du mapping : c'est lui qui lit les bornes et
    /// rend les dates quand l'agregation ne demande pas de `format`.
    pub fn lire(nom: &str, corps: &Value, format_du_champ: &DateFormat) -> EsResult<Self> {
        let obj = corps.as_object().ok_or_else(|| {
            EsError::parsing(format!("[aggs.{nom}.date_histogram] doit etre un objet"))
        })?;
        let champ = obj
            .get("field")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let intervalle = lit_intervalle(nom, obj)?;
        let fuseau = match obj.get("time_zone") {
            None => Fuseau::utc(),
            Some(Value::String(s)) => Fuseau::parse(s)?,
            Some(autre) => {
                return Err(EsError::illegal_argument(format!(
                    "[aggs.{nom}.date_histogram.time_zone] : une chaine est attendue, pas {autre}"
                )))
            }
        };
        let offset = match obj.get("offset") {
            None => 0,
            Some(Value::String(s)) => lit_offset(s)?,
            Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
            Some(autre) => {
                return Err(EsError::illegal_argument(format!(
                    "[aggs.{nom}.date_histogram.offset] : une duree est attendue, pas {autre}"
                )))
            }
        };
        let format = match obj.get("format") {
            None => format_du_champ.clone(),
            Some(Value::String(s)) => DateFormat::parse(s)?,
            Some(autre) => {
                return Err(EsError::illegal_argument(format!(
                    "[aggs.{nom}.date_histogram.format] : une chaine est attendue, pas {autre}"
                )))
            }
        };
        let min_doc_count = match obj.get("min_doc_count") {
            None => 0,
            Some(v) => v.as_u64().ok_or_else(|| {
                EsError::illegal_argument(format!(
                    "[aggs.{nom}.date_histogram.min_doc_count] : un entier positif est attendu, \
                     pas {v}"
                ))
            })?,
        };
        let keyed = match obj.get("keyed") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(autre) => {
                return Err(EsError::illegal_argument(format!(
                    "[aggs.{nom}.date_histogram.keyed] : un booleen est attendu, pas {autre}"
                )))
            }
        };

        // Les bornes se lisent **dans le fuseau** : mesure contre ES 8.15,
        // `extended_bounds: {min: "2026-02-28T23:30:00"}` avec `Europe/Paris`
        // demande le seau du 28 fevrier, pas celui du 1er mars.
        let extended = lire_bornes(nom, obj, "extended_bounds", &format, &fuseau)?;
        let hard = lire_bornes(nom, obj, "hard_bounds", &format, &fuseau)?;
        if let Some(h) = hard {
            if let (Some(min), Some(max)) = (h.min, h.max) {
                if min > max {
                    return Err(EsError::illegal_argument(format!(
                        "[hard_bounds.min][{min}] cannot be greater than [hard_bounds.max][{max}] \
                         for histogram aggregation [{nom}]"
                    )));
                }
            }
            if let Some(e) = extended {
                let dehors = e.min.is_some_and(|m| h.min.is_some_and(|hm| m < hm))
                    || e.max.is_some_and(|m| h.max.is_some_and(|hm| m > hm));
                if dehors {
                    return Err(EsError::illegal_argument(format!(
                        "Extended bounds have to be inside hard bounds, hard bounds: \
                         [{}--{}], extended bounds: [{}--{}]",
                        borne_texte(h.min),
                        borne_texte(h.max),
                        borne_texte(e.min),
                        borne_texte(e.max)
                    )));
                }
            }
        }

        Ok(Self {
            champ,
            arrondi: Arrondi::new(intervalle, fuseau, offset),
            min_doc_count,
            extended,
            hard,
            format,
            keyed,
            bornes: Vec::new(),
        })
    }

    /// Deroule les bornes des seaux depuis le `min` et le `max` du champ.
    ///
    /// `None` quand la recherche n'a ramene aucune valeur : ES rend alors
    /// `buckets: []`, sauf si `extended_bounds` demande quand meme des seaux.
    pub fn pose_les_bornes(&mut self, min: Option<i64>, max: Option<i64>) -> EsResult<()> {
        // `extended_bounds` ne s'applique qu'a `min_doc_count: 0` (mesure : a 1
        // il est ignore, sans erreur).
        let etendu = (self.min_doc_count == 0).then_some(self.extended).flatten();
        let arrondi = |v: i64| self.arrondi.arrondit(v);

        let mut debut = match (min, etendu.and_then(|e| e.min)) {
            (Some(d), Some(e)) => Some(arrondi(d).min(arrondi(e))),
            (Some(d), None) => Some(arrondi(d)),
            (None, Some(e)) => Some(arrondi(e)),
            (None, None) => None,
        };
        let mut fin = match (max, etendu.and_then(|e| e.max)) {
            (Some(d), Some(e)) => Some(arrondi(d).max(arrondi(e))),
            (Some(d), None) => Some(arrondi(d)),
            (None, Some(e)) => Some(arrondi(e)),
            (None, None) => None,
        };

        // `hard_bounds` : les deux bornes sont arrondies, la basse est incluse
        // et la haute **exclue** (mesure).
        if let Some(h) = self.hard {
            if let Some(hmin) = h.min.map(arrondi) {
                debut = debut.map(|d| d.max(hmin));
            }
            if let Some(hmax) = h.max.map(arrondi) {
                match fin {
                    Some(f) if f >= hmax => {
                        // Le dernier seau autorise est celui d'avant `hmax`.
                        fin = (hmax > debut.unwrap_or(i64::MIN)).then(|| self.seau_precedent(hmax));
                    }
                    autre => fin = autre,
                }
            }
        }

        let (Some(debut), Some(fin)) = (debut, fin) else {
            self.bornes = Vec::new();
            return Ok(());
        };
        if fin < debut {
            self.bornes = Vec::new();
            return Ok(());
        }

        let mut bornes = vec![debut];
        let mut courante = debut;
        while courante < fin {
            courante = self.arrondi.suivant(courante);
            bornes.push(courante);
            if bornes.len() > MAX_SEAUX {
                return Err(EsError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "too_many_buckets_exception",
                    format!(
                        "Trying to create too many buckets. Must be less than or equal to: \
                         [{MAX_SEAUX}] but this number of buckets was exceeded by the \
                         [date_histogram] aggregation on [{}]",
                        self.champ
                    ),
                ));
            }
        }
        // La borne de fin du dernier seau.
        bornes.push(self.arrondi.suivant(courante));
        self.bornes = bornes;
        Ok(())
    }

    /// Le seau qui precede immediatement une borne de seau.
    fn seau_precedent(&self, seau: i64) -> i64 {
        self.arrondi.arrondit(seau - 1)
    }

    /// La demande `range` equivalente, en nanosecondes — l'unite de tantivy.
    ///
    /// Quand il n'y a aucun seau — pas un document date, et pas
    /// d'`extended_bounds` — la demande porte quand meme un intervalle : celui
    /// d'une nanoseconde a l'epoque, qu'aucun seau ne reclamera. Une agregation
    /// `range` sans intervalle est refusee par tantivy, et retirer l'agregation
    /// de la demande la ferait disparaitre de la reponse, la ou ES rend
    /// `buckets: []`.
    pub fn intervalles_pour_tantivy(&self) -> Value {
        if self.bornes.len() < 2 {
            return json!([{"from": 0.0, "to": 1.0}]);
        }
        Value::Array(
            self.bornes
                .windows(2)
                .map(|paire| {
                    json!({
                        "from": nanos_arrondis_en_bas(paire[0]),
                        "to": nanos_arrondis_en_bas(paire[1]),
                    })
                })
                .collect(),
        )
    }

    /// Le decalage local d'un seau : ce qu'ES ecrit apres la date.
    fn decalage(&self, seau_ms: i64) -> i32 {
        self.arrondi.fuseau().decalage(seau_ms)
    }

    /// La forme lisible d'un seau (`key_as_string`).
    pub fn rend(&self, seau_ms: i64) -> Option<String> {
        self.format
            .rend_avec_decalage(seau_ms, self.decalage(seau_ms))
    }

    pub fn keyed(&self) -> bool {
        self.keyed
    }

    /// La fin d'un seau, en millisecondes : la borne qui suit son debut.
    ///
    /// Elle ne se deduit pas d'une duree : un mois civil n'en a pas de
    /// constante, et un jour a Paris en a deux (23 h et 25 h). Elle se lit donc
    /// dans la liste de bornes que [`Self::pose_les_bornes`] a deroulee — la
    /// meme que celle qui a servi a executer l'agregation, ce qui est la seule
    /// facon de ne pas refaire le calcul autrement.
    pub fn fin_du_seau(&self, debut: i64) -> Option<i64> {
        let i = self.bornes.iter().position(|b| *b == debut)?;
        self.bornes.get(i + 1).copied()
    }

    /// Les seaux d'un `date_histogram`, tires du resultat `range` de tantivy.
    ///
    /// `sous` remet en forme les sous-agregations d'un seau (elles sont rendues
    /// par l'appelant, qui seul connait leurs metadonnees).
    pub fn seaux(&self, buckets: &Value, sous: &MiseEnForme<'_>) -> Value {
        if self.bornes.len() < 2 {
            return if self.keyed {
                Value::Object(Map::new())
            } else {
                Value::Array(Vec::new())
            };
        }
        let liste = match buckets {
            Value::Array(a) => a.as_slice(),
            _ => &[],
        };
        // Chaque seau rendu par tantivy est rattache a **sa** borne : les deux
        // seaux d'extremite qu'il ajoute (avant la premiere borne, apres la
        // derniere) n'en ont pas, et disparaissent.
        let mut seaux: Vec<(i64, u64, Map<String, Value>)> = Vec::new();
        for b in liste {
            let Some(obj) = b.as_object() else { continue };
            let Some(debut) = obj.get("from").and_then(Value::as_f64) else {
                continue;
            };
            let debut = (debut / 1_000_000.0).round() as i64;
            if !self.bornes[..self.bornes.len() - 1].contains(&debut) {
                continue;
            }
            let compte = obj.get("doc_count").and_then(Value::as_u64).unwrap_or(0);
            let restant: Map<String, Value> = obj
                .iter()
                .filter(|(cle, _)| {
                    !matches!(
                        cle.as_str(),
                        "key" | "from" | "to" | "from_as_string" | "to_as_string" | "doc_count"
                    )
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            seaux.push((debut, compte, sous(&restant)));
        }
        seaux.sort_by_key(|(debut, _, _)| *debut);
        let gardes = self.rogne(&seaux);
        let mut sortie: Vec<Value> = Vec::with_capacity(gardes.len());
        for &i in &gardes {
            let (debut, compte, sous) = &seaux[i];
            let mut m = Map::new();
            if let Some(texte) = self.rend(*debut) {
                m.insert("key_as_string".into(), json!(texte));
            }
            m.insert("key".into(), json!(debut));
            m.insert("doc_count".into(), json!(compte));
            for (k, v) in sous {
                m.insert(k.clone(), v.clone());
            }
            sortie.push(Value::Object(m));
        }
        if self.keyed {
            // La cle de la map est la date lisible — et le seau la garde
            // quand meme (mesure : ES ne la retire pas, contrairement a ce
            // qu'il fait du `key` d'un `range` keyed).
            let mut map = Map::new();
            for seau in sortie {
                let cle = seau
                    .get("key_as_string")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| seau["key"].to_string());
                map.insert(cle, seau);
            }
            Value::Object(map)
        } else {
            Value::Array(sortie)
        }
    }

    /// Quels seaux garder : ceux d'`extended_bounds`, plus tout ce qui va du
    /// premier au dernier seau **non vide**, puis le filtre `min_doc_count`.
    ///
    /// C'est ici que se paie le choix d'une liste de bornes globale : sous un
    /// `terms`, chaque categorie a ses propres bords, et ce sont ceux-la qu'ES
    /// rend.
    fn rogne(&self, seaux: &[(i64, u64, Map<String, Value>)]) -> Vec<usize> {
        if self.min_doc_count > 0 {
            return seaux
                .iter()
                .enumerate()
                .filter(|(_, (_, compte, _))| *compte >= self.min_doc_count)
                .map(|(i, _)| i)
                .collect();
        }
        if seaux.is_empty() {
            return Vec::new();
        }
        let plein: Vec<usize> = seaux
            .iter()
            .enumerate()
            .filter(|(_, (_, compte, _))| *compte > 0)
            .map(|(i, _)| i)
            .collect();
        // Ce qu'`extended_bounds` reclame, en indices de seaux — **cote par
        // cote** : une borne absente n'etend rien de ce cote-la. Mesure contre
        // ES 8.15 : `extended_bounds: {min}` sans `max` s'arrete au dernier
        // seau non vide, il ne va pas jusqu'au bout des seaux existants.
        let etendu = self.extended.unwrap_or_default();
        let arrondie = |v: Option<i64>| v.map(|x| self.arrondi.arrondit(x));
        let debut_etendu = arrondie(etendu.min)
            .map(|min| seaux.iter().position(|(d, _, _)| *d >= min).unwrap_or(0));
        let fin_etendue = arrondie(etendu.max).map(|max| {
            seaux
                .iter()
                .rposition(|(d, _, _)| *d <= max)
                .unwrap_or(seaux.len() - 1)
        });
        let (debut, fin) = match (plein.first(), plein.last()) {
            (Some(a), Some(b)) => (
                debut_etendu.map_or(*a, |e| e.min(*a)),
                fin_etendue.map_or(*b, |e| e.max(*b)),
            ),
            // Aucun document ici : seul `extended_bounds` peut encore demander
            // des seaux — et seulement si ses **deux** bornes sont ecrites. Le
            // cas se produit sous un seau parent dont aucun document ne porte
            // la date, alors que les bornes, elles, sont globales ; mesure
            // contre ES 8.15 : avec un `min` seul, il n'y rend aucun seau —
            // il n'a rien pour fermer l'intervalle.
            _ => match (debut_etendu, fin_etendue) {
                (Some(d), Some(f)) => (d, f),
                _ => return Vec::new(),
            },
        };
        (debut..=fin.min(seaux.len() - 1)).collect()
    }
}

/// Le flottant de nanosecondes immediatement **inferieur ou egal** a une date
/// en millisecondes.
///
/// A l'echelle de 2026, un `f64` ne represente plus les nanosecondes qu'a 256
/// pres : la conversion naive arrondit au plus proche, donc parfois
/// **au-dessus** de la borne demandee, et un document pose exactement dessus
/// tombe alors dans le seau precedent, en 200 et sans un mot. Une seconde
/// pleine, elle, est toujours exacte — d'ou le fait que seuls les intervalles
/// **sous-seconde** soient concernes (mesure : 3 750 millisecondes sur 10 000
/// debordent, 0 seconde sur 10 000). Aucun document ne peut vivre dans les
/// quelques nanosecondes qu'on retire ici : ferrite indexe des millisecondes.
fn nanos_arrondis_en_bas(ms: i64) -> f64 {
    let exact = i128::from(ms) * 1_000_000;
    let mut approx = exact as f64;
    while (approx as i128) > exact {
        approx = f64_precedent(approx);
    }
    approx
}

fn f64_precedent(x: f64) -> f64 {
    let bits = x.to_bits();
    f64::from_bits(if x > 0.0 { bits - 1 } else { bits + 1 })
}

fn borne_texte(v: Option<i64>) -> String {
    v.map_or_else(|| "null".to_string(), |x| x.to_string())
}

/// `calendar_interval` ou `fixed_interval` — l'un des deux, jamais les deux.
///
/// Les trois refus sont ceux d'ES, mesures un par un : les deux ensemble, aucun
/// des deux, et un multiple sur `calendar_interval` (`2d`).
fn lit_intervalle(nom: &str, obj: &Map<String, Value>) -> EsResult<Intervalle> {
    let calendaire = obj.get("calendar_interval").and_then(Value::as_str);
    let fixe = obj.get("fixed_interval").and_then(Value::as_str);
    match (calendaire, fixe) {
        (Some(_), Some(_)) => Err(EsError::illegal_argument(format!(
            "[aggs.{nom}.date_histogram] : [calendar_interval] et [fixed_interval] ne peuvent pas \
             etre demandes ensemble"
        ))),
        (Some(c), None) => lit_calendaire(c)
            .map(Intervalle::Calendaire)
            .ok_or_else(|| {
                EsError::illegal_argument(format!(
                "[aggs.{nom}.date_histogram] : [calendar_interval: {c}] n'est pas un intervalle de \
                 calendrier ; une unite et une seule est acceptee (second/1s, minute/1m, hour/1h, \
                 day/1d, week/1w, month/1M, quarter/1q, year/1y) — un multiple comme [2d] se \
                 demande en [fixed_interval]"
            ))
            }),
        (None, Some(f)) => {
            let ms = lit_fixe(f).ok_or_else(|| {
                EsError::illegal_argument(format!(
                    "[aggs.{nom}.date_histogram] : [fixed_interval: {f}] ne se lit pas comme une \
                     duree ; unites acceptees : ms, s, m, h, d — les unites de calendrier (w, M, \
                     q, y) se demandent en [calendar_interval]"
                ))
            })?;
            if ms < 1 {
                return Err(EsError::illegal_argument(
                    "Zero or negative time interval not supported",
                ));
            }
            Ok(Intervalle::Fixe(ms))
        }
        (None, None) => Err(EsError::illegal_argument(
            "Invalid interval specified, must be non-null and non-empty",
        )),
    }
}

/// `extended_bounds` / `hard_bounds` : deux bornes de date, lues au format de
/// l'agregation (ou du champ), date math comprise.
fn lire_bornes(
    nom: &str,
    obj: &Map<String, Value>,
    cle: &str,
    format: &DateFormat,
    fuseau: &Fuseau,
) -> EsResult<Option<Bornes>> {
    let Some(v) = obj.get(cle) else {
        return Ok(None);
    };
    let o = v.as_object().ok_or_else(|| {
        EsError::illegal_argument(format!(
            "[aggs.{nom}.date_histogram.{cle}] : un objet [min]/[max] est attendu, pas {v}"
        ))
    })?;
    for k in o.keys() {
        if k != "min" && k != "max" {
            return Err(EsError::illegal_argument(format!(
                "[aggs.{nom}.date_histogram.{cle}] : [{k}] n'est ni [min] ni [max]"
            )));
        }
    }
    let lit = |k: &str| -> EsResult<Option<i64>> {
        match o.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => crate::datemath::borne_dans(
                v,
                format,
                crate::datemath::maintenant(),
                crate::datemath::Arrondi::Bas,
                fuseau,
            )
            .map(Some),
        }
    };
    Ok(Some(Bornes {
        min: lit("min")?,
        max: lit("max")?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str) -> i64 {
        DateFormat::default()
            .lit_avec_residu(&serde_json::json!(s))
            .unwrap()
            .0
    }

    fn histo(corps: Value) -> Histo {
        Histo::lire("h", &corps, &DateFormat::default()).unwrap()
    }

    #[test]
    fn les_bornes_vont_du_premier_au_dernier_seau() {
        let mut h = histo(json!({"field": "d", "calendar_interval": "day"}));
        h.pose_les_bornes(
            Some(ms("2026-03-01T05:00:00Z")),
            Some(ms("2026-03-03T09:00:00Z")),
        )
        .unwrap();
        assert_eq!(
            h.bornes,
            vec![
                ms("2026-03-01T00:00:00Z"),
                ms("2026-03-02T00:00:00Z"),
                ms("2026-03-03T00:00:00Z"),
                ms("2026-03-04T00:00:00Z"),
            ]
        );
    }

    #[test]
    fn un_mois_a_paris_commence_a_l_heure_locale() {
        let mut h = histo(json!({
            "field": "d", "calendar_interval": "month", "time_zone": "Europe/Paris"
        }));
        h.pose_les_bornes(
            Some(ms("2026-03-15T00:00:00Z")),
            Some(ms("2026-04-15T00:00:00Z")),
        )
        .unwrap();
        assert_eq!(h.bornes[0], ms("2026-02-28T23:00:00Z"));
        // Avril commence en heure d'ete : le seau de mars dure une heure de
        // moins qu'un mois de 31 jours.
        assert_eq!(h.bornes[1], ms("2026-03-31T22:00:00Z"));
        assert_eq!(
            h.rend(h.bornes[0]).unwrap(),
            "2026-03-01T00:00:00.000+01:00"
        );
        assert_eq!(
            h.rend(h.bornes[1]).unwrap(),
            "2026-04-01T00:00:00.000+02:00"
        );
    }

    #[test]
    fn hard_bounds_exclut_sa_borne_haute() {
        let mut h = histo(json!({
            "field": "d", "calendar_interval": "day",
            "hard_bounds": {"min": "2026-03-02", "max": "2026-03-06"}
        }));
        h.pose_les_bornes(
            Some(ms("2026-03-01T00:00:00Z")),
            Some(ms("2026-03-06T12:00:00Z")),
        )
        .unwrap();
        assert_eq!(h.bornes.first(), Some(&ms("2026-03-02T00:00:00Z")));
        // Le seau du 6 est exclu : la derniere borne ferme celui du 5.
        assert_eq!(h.bornes.last(), Some(&ms("2026-03-06T00:00:00Z")));
    }

    #[test]
    fn extended_bounds_etend_des_deux_cotes() {
        let mut h = histo(json!({
            "field": "d", "calendar_interval": "day",
            "extended_bounds": {"min": "2026-02-27T13:00:00", "max": "2026-03-08T09:00:00"}
        }));
        h.pose_les_bornes(
            Some(ms("2026-03-01T00:00:00Z")),
            Some(ms("2026-03-03T00:00:00Z")),
        )
        .unwrap();
        assert_eq!(h.bornes.first(), Some(&ms("2026-02-27T00:00:00Z")));
        assert_eq!(h.bornes.last(), Some(&ms("2026-03-09T00:00:00Z")));
    }

    #[test]
    fn les_refus_sont_ceux_d_elasticsearch() {
        let f = DateFormat::default();
        let err = |c: Value| Histo::lire("h", &c, &f).unwrap_err().reason;
        assert_eq!(
            err(json!({"field": "d"})),
            "Invalid interval specified, must be non-null and non-empty"
        );
        assert_eq!(
            err(json!({"field": "d", "fixed_interval": "0s"})),
            "Zero or negative time interval not supported"
        );
        assert!(err(json!({"field": "d", "calendar_interval": "2d"})).contains("[2d]"));
        assert!(err(json!({"field": "d", "fixed_interval": "1M"})).contains("calendar_interval"));
        assert!(err(json!({"field": "d", "calendar_interval": "day",
                           "time_zone": "Europe/Nulle_Part"}))
        .contains("Unknown time-zone ID"));
        assert!(err(json!({"field": "d", "calendar_interval": "day",
                           "hard_bounds": {"min": "2026-03-05", "max": "2026-03-02"}}))
        .contains("cannot be greater than"));
        assert!(err(json!({"field": "d", "calendar_interval": "day",
                           "hard_bounds": {"min": "2026-03-02", "max": "2026-03-05"},
                           "extended_bounds": {"min": "2026-02-27", "max": "2026-03-08"}}))
        .contains("Extended bounds have to be inside hard bounds"));
    }

    #[test]
    fn une_borne_ne_depasse_jamais_sa_milliseconde() {
        // Le flottant de nanosecondes doit rester **sous** la borne exacte,
        // sinon un document pose a minuit tombe dans le seau d'avant.
        for ms in [1774738800000i64, 1772323200000, 2350944000000, -86400000] {
            let f = nanos_arrondis_en_bas(ms);
            let exact = i128::from(ms) * 1_000_000;
            assert!(f as i128 <= exact, "{ms} : {f} > {exact}");
            assert!(exact - (f as i128) < 1_000_000, "{ms} : trop bas");
        }
    }
}
