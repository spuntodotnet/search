//! Les templates d'index : un mapping, des reglages et des alias appliques a
//! un index **qui n'existe pas encore**.
//!
//! C'est la piece qui manquait a un script d'init : il pose un template, puis
//! ecrit, et l'index quotidien nait avec le bon mapping. Sans elle, la creation
//! implicite donne un index vide dont les champs sont devines document par
//! document — ce qui marche, jusqu'au premier champ devine du mauvais type.
//!
//! Deux familles cohabitent, comme chez Elasticsearch 8 :
//!
//! * les **composables** (`_index_template`), la forme actuelle : au plus un
//!   s'applique, celui de plus forte `priority` ;
//! * les **anciens** (`_template`), deprecies mais toujours servis par ES 8 —
//!   et c'est eux qu'on trouve dans le code d'un projet reste en 7.x, donc eux
//!   qu'il faut servir pour que ce code ne change pas. Tous ceux qui
//!   correspondent s'appliquent, par `order` croissant.
//!
//! Un composable qui correspond **eclipse** les anciens, comme chez ES.
//!
//! ## Ce qui est valide, et quand
//!
//! Tout est valide a la **pose** du template, pas a la creation de l'index :
//! un reglage que ferrite refuse, un type de champ qu'il ne connait pas ou un
//! alias filtre doivent faire echouer `PUT /_index_template`, la ou le client
//! regarde. Les decouvrir six mois plus tard, au premier document ecrit dans
//! `logs-2027.01.01`, serait la meme information rendue inutilisable.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::error::{EsError, EsResult};
use crate::search::glob_match;

const FICHIER: &str = "_templates.json";

/// Un template, sous la forme ou il est rendu et applique.
#[derive(Debug, Clone, Default)]
pub struct Template {
    pub patterns: Vec<String>,
    /// `priority` d'un composable, `order` d'un ancien : le meme role, deux
    /// noms, et deux regles d'application.
    pub rang: i64,
    pub version: Option<i64>,
    pub meta: Option<Value>,
    pub settings: Option<Value>,
    pub mappings: Option<Value>,
    pub aliases: Option<Value>,
}

impl Template {
    /// La forme rendue par `GET /_index_template`.
    pub fn to_json_composable(&self) -> Value {
        let mut t = Map::new();
        if let Some(s) = &self.settings {
            t.insert("settings".into(), s.clone());
        }
        if let Some(m) = &self.mappings {
            t.insert("mappings".into(), m.clone());
        }
        if let Some(a) = &self.aliases {
            t.insert("aliases".into(), a.clone());
        }
        let mut o = Map::new();
        o.insert("index_patterns".into(), json!(self.patterns));
        if !t.is_empty() {
            o.insert("template".into(), Value::Object(t));
        }
        o.insert("composed_of".into(), json!([]));
        o.insert("priority".into(), json!(self.rang));
        if let Some(v) = self.version {
            o.insert("version".into(), json!(v));
        }
        if let Some(m) = &self.meta {
            o.insert("_meta".into(), m.clone());
        }
        Value::Object(o)
    }

    /// La forme rendue par `GET /_template`, dans l'ordre d'ES.
    pub fn to_json_ancien(&self) -> Value {
        let mut o = Map::new();
        o.insert("order".into(), json!(self.rang));
        if let Some(v) = self.version {
            o.insert("version".into(), json!(v));
        }
        o.insert("index_patterns".into(), json!(self.patterns));
        o.insert(
            "settings".into(),
            self.settings.clone().unwrap_or_else(|| json!({})),
        );
        o.insert(
            "mappings".into(),
            self.mappings.clone().unwrap_or_else(|| json!({})),
        );
        o.insert(
            "aliases".into(),
            self.aliases.clone().unwrap_or_else(|| json!({})),
        );
        Value::Object(o)
    }

    fn to_stockage(&self) -> Value {
        let mut o = Map::new();
        o.insert("index_patterns".into(), json!(self.patterns));
        o.insert("rang".into(), json!(self.rang));
        if let Some(v) = self.version {
            o.insert("version".into(), json!(v));
        }
        if let Some(m) = &self.meta {
            o.insert("_meta".into(), m.clone());
        }
        if let Some(s) = &self.settings {
            o.insert("settings".into(), s.clone());
        }
        if let Some(m) = &self.mappings {
            o.insert("mappings".into(), m.clone());
        }
        if let Some(a) = &self.aliases {
            o.insert("aliases".into(), a.clone());
        }
        Value::Object(o)
    }

    fn depuis_stockage(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        Some(Self {
            patterns: o
                .get("index_patterns")?
                .as_array()?
                .iter()
                .filter_map(|p| p.as_str().map(str::to_string))
                .collect(),
            rang: o.get("rang").and_then(Value::as_i64).unwrap_or(0),
            version: o.get("version").and_then(Value::as_i64),
            meta: o.get("_meta").cloned(),
            settings: o.get("settings").cloned(),
            mappings: o.get("mappings").cloned(),
            aliases: o.get("aliases").cloned(),
        })
    }
}

/// Les deux familles, sous un seul verrou : elles se departagent a la creation
/// d'un index, donc elles se lisent ensemble.
#[derive(Debug, Clone, Default)]
pub struct Registre {
    pub composables: BTreeMap<String, Template>,
    pub anciens: BTreeMap<String, Template>,
}

impl Registre {
    /// Ce qui s'applique a un index de ce nom : au plus un composable, sinon
    /// tous les anciens qui correspondent, fusionnes par `order` croissant.
    pub fn pour(&self, index: &str) -> Option<Template> {
        let mut candidats: Vec<&Template> = self
            .composables
            .values()
            .filter(|t| t.patterns.iter().any(|p| glob_match(p, index)))
            .collect();
        if !candidats.is_empty() {
            candidats.sort_by_key(|t| -t.rang);
            return Some(candidats[0].clone());
        }
        let mut anciens: Vec<&Template> = self
            .anciens
            .values()
            .filter(|t| t.patterns.iter().any(|p| glob_match(p, index)))
            .collect();
        if anciens.is_empty() {
            return None;
        }
        anciens.sort_by_key(|t| t.rang);
        let mut fusion = Template::default();
        for t in anciens {
            fusion.settings = fusionner(fusion.settings.take(), t.settings.clone());
            fusion.mappings = fusionner(fusion.mappings.take(), t.mappings.clone());
            fusion.aliases = fusionner(fusion.aliases.take(), t.aliases.clone());
        }
        Some(fusion)
    }

    fn to_json(&self) -> Value {
        let rendre = |m: &BTreeMap<String, Template>| -> Value {
            Value::Object(
                m.iter()
                    .map(|(k, v)| (k.clone(), v.to_stockage()))
                    .collect(),
            )
        };
        json!({
            "index_templates": rendre(&self.composables),
            "templates": rendre(&self.anciens),
        })
    }
}

/// Fusionne deux blocs, le second l'emportant cle a cle — recursivement sur les
/// objets, ce qui est ce que fait ES d'un `properties` de mapping.
pub fn fusionner(base: Option<Value>, dessus: Option<Value>) -> Option<Value> {
    match (base, dessus) {
        (None, d) => d,
        (b, None) => b,
        (Some(Value::Object(mut b)), Some(Value::Object(d))) => {
            for (cle, valeur) in d {
                let fusionnee = fusionner(b.remove(&cle), Some(valeur));
                if let Some(v) = fusionnee {
                    b.insert(cle, v);
                }
            }
            Some(Value::Object(b))
        }
        (_, d) => d,
    }
}

/// Lit le corps d'un `PUT /_index_template/{nom}`.
pub fn lire_composable(body: &Value) -> EsResult<Template> {
    let o = body
        .as_object()
        .ok_or_else(|| EsError::parsing("le corps de [PUT /_index_template] doit etre un objet"))?;
    crate::api::expect_only(
        o,
        &[
            "index_patterns",
            "template",
            "priority",
            "version",
            "_meta",
            "composed_of",
            "data_stream",
        ],
        "PUT /_index_template",
    )?;
    if let Some(c) = o.get("composed_of") {
        if !c.as_array().is_some_and(|a| a.is_empty()) {
            return Err(EsError::unsupported(
                "ferrite ne supporte pas [composed_of] : les templates de composants \
                 (`_component_template`) ne sont pas implementes, et appliquer un template qui en \
                 cite un sans le lire donnerait un index sans le mapping demande",
            ));
        }
    }
    if o.contains_key("data_stream") {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas les flux de donnees (`data_stream`)",
        ));
    }
    let patterns = lire_patterns(o.get("index_patterns"))
        .ok_or_else(|| EsError::illegal_argument("Required [index_patterns]"))?;
    let (settings, mappings, aliases) = match o.get("template") {
        None => (None, None, None),
        Some(t) => {
            let t = t.as_object().ok_or_else(|| {
                EsError::parsing("[PUT /_index_template] : [template] doit etre un objet")
            })?;
            crate::api::expect_only(
                t,
                &["settings", "mappings", "aliases", "lifecycle"],
                "PUT /_index_template.template",
            )?;
            (
                t.get("settings").cloned(),
                t.get("mappings").cloned(),
                t.get("aliases").cloned(),
            )
        }
    };
    let tpl = Template {
        patterns,
        rang: o.get("priority").and_then(Value::as_i64).unwrap_or(0),
        version: o.get("version").and_then(Value::as_i64),
        meta: o.get("_meta").cloned(),
        settings: settings.map(normaliser_settings),
        mappings,
        aliases,
    };
    valider(&tpl)?;
    Ok(tpl)
}

/// Lit le corps d'un `PUT /_template/{nom}` (l'ancienne forme, ou `settings`,
/// `mappings` et `aliases` sont a la racine).
pub fn lire_ancien(body: &Value) -> EsResult<Template> {
    let o = body
        .as_object()
        .ok_or_else(|| EsError::parsing("le corps de [PUT /_template] doit etre un objet"))?;
    crate::api::expect_only(
        o,
        &[
            "index_patterns",
            "settings",
            "mappings",
            "aliases",
            "order",
            "version",
        ],
        "PUT /_template",
    )?;
    let patterns = lire_patterns(o.get("index_patterns")).ok_or_else(|| {
        EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "action_request_validation_exception",
            "Validation Failed: 1: index patterns are missing;",
        )
    })?;
    let tpl = Template {
        patterns,
        rang: o.get("order").and_then(Value::as_i64).unwrap_or(0),
        version: o.get("version").and_then(Value::as_i64),
        meta: None,
        settings: o.get("settings").cloned().map(normaliser_settings),
        mappings: o.get("mappings").cloned(),
        aliases: o.get("aliases").cloned(),
    };
    valider(&tpl)?;
    Ok(tpl)
}

/// `index_patterns` : une chaine ou une liste, comme chez ES.
fn lire_patterns(v: Option<&Value>) -> Option<Vec<String>> {
    match v? {
        Value::String(s) => Some(vec![s.clone()]),
        Value::Array(a) if !a.is_empty() => Some(
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
        ),
        _ => None,
    }
}

/// ES range les reglages sous `index` et les rend en chaines : un template pose
/// avec `{"number_of_replicas": 0}` se relit `{"index": {"number_of_replicas":
/// "0"}}`.
fn normaliser_settings(v: Value) -> Value {
    let Value::Object(o) = &v else { return v };
    let mut plats: Vec<(String, Value)> = Vec::new();
    aplatir("", o, &mut plats);
    let mut index = Map::new();
    for (cle, valeur) in plats {
        let court = cle.strip_prefix("index.").unwrap_or(&cle).to_string();
        let texte = match &valeur {
            Value::String(s) => s.clone(),
            autre => autre.to_string(),
        };
        nicher(&mut index, &court, Value::String(texte));
    }
    json!({ "index": Value::Object(index) })
}

fn aplatir(prefixe: &str, o: &Map<String, Value>, out: &mut Vec<(String, Value)>) {
    for (cle, valeur) in o {
        let chemin = if prefixe.is_empty() {
            cle.clone()
        } else {
            format!("{prefixe}.{cle}")
        };
        // `analysis` declare des analyzers : ce n'est pas un reglage scalaire,
        // il garde sa forme.
        if chemin == "analysis" || chemin == "index.analysis" {
            out.push((chemin, valeur.clone()));
            continue;
        }
        match valeur {
            Value::Object(sous) => aplatir(&chemin, sous, out),
            autre => out.push((chemin, autre.clone())),
        }
    }
}

fn nicher(cible: &mut Map<String, Value>, chemin: &str, valeur: Value) {
    let parts: Vec<&str> = chemin.split('.').collect();
    let mut courant = cible;
    for (i, part) in parts.iter().enumerate() {
        if i + 1 == parts.len() {
            courant.insert((*part).to_string(), valeur);
            return;
        }
        let entree = courant
            .entry((*part).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let Value::Object(o) = entree else { return };
        courant = o;
    }
}

/// Verifie qu'un template est **applicable**, a la pose.
///
/// C'est la que se joue la regle du projet : un template qu'on accepte et qui
/// fera echouer la creation d'un index six mois plus tard n'a rien signale, il
/// a seulement deplace l'echec la ou personne ne regarde.
fn valider(tpl: &Template) -> EsResult<()> {
    for p in &tpl.patterns {
        if p.is_empty() {
            return Err(EsError::illegal_argument(
                "index_patterns [] : un motif vide ne designe rien",
            ));
        }
    }
    let mut declares = crate::analysis::Analysis::default();
    if let Some(s) = &tpl.settings {
        if let Some(a) = crate::reglages::section_analysis(s) {
            declares = crate::analysis::Analysis::parse(a)?;
        }
        crate::reglages::lire(s)?;
    }
    if let Some(m) = &tpl.mappings {
        crate::mapping::Mapping::parse_avec(m, &declares)?;
    }
    if let Some(a) = &tpl.aliases {
        let o = a
            .as_object()
            .ok_or_else(|| EsError::parsing("[aliases] doit etre un objet"))?;
        for (nom, corps) in o {
            crate::alias::valider_nom(nom)?;
            crate::alias::lire_attache(corps, "template.aliases")?;
        }
    }
    Ok(())
}

/// Deux motifs se recouvrent-ils ?
///
/// Approximation assumee : on ne calcule pas l'intersection de deux jokers, on
/// regarde si l'un decrit l'autre pris pour un nom. Elle attrape le cas qui
/// compte (`logs-*` contre `logs-*-*`) et ne peut que **sur**-detecter, jamais
/// laisser passer deux motifs identiques.
fn se_recouvrent(a: &str, b: &str) -> bool {
    a == b || glob_match(a, b) || glob_match(b, a)
}

/// Refuse un composable qui rendrait la creation d'un index ambigue : deux
/// templates de meme priorite dont les motifs se recouvrent, et plus rien ne
/// dit lequel s'applique.
pub fn verifier_priorite(registre: &Registre, nom: &str, tpl: &Template) -> EsResult<()> {
    for (autre_nom, autre) in &registre.composables {
        if autre_nom == nom || autre.rang != tpl.rang {
            continue;
        }
        for p in &tpl.patterns {
            for q in &autre.patterns {
                if se_recouvrent(p, q) {
                    return Err(EsError::illegal_argument(format!(
                        "index template [{nom}] has index patterns {:?} matching patterns from \
                         existing templates [{autre_nom}] with patterns ({autre_nom} => {:?}) \
                         that have the same priority [{}], multiple index templates may not \
                         match during index creation, please use a different priority",
                        tpl.patterns, autre.patterns, tpl.rang
                    )));
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Persistance
// ---------------------------------------------------------------------------

pub fn charger(racine: &Path) -> Registre {
    let Ok(raw) = std::fs::read(racine.join(FICHIER)) else {
        return Registre::default();
    };
    let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
        return Registre::default();
    };
    let lire = |cle: &str| -> BTreeMap<String, Template> {
        v.get(cle)
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .filter_map(|(k, t)| Template::depuis_stockage(t).map(|t| (k.clone(), t)))
                    .collect()
            })
            .unwrap_or_default()
    };
    Registre {
        composables: lire("index_templates"),
        anciens: lire("templates"),
    }
}

pub fn enregistrer(racine: &Path, registre: &Registre) -> EsResult<()> {
    let tmp = racine.join(format!("{FICHIER}.tmp"));
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&registre.to_json()).unwrap(),
    )
    .map_err(|e| EsError::internal(format!("ecriture des templates: {e}")))?;
    std::fs::rename(&tmp, racine.join(FICHIER))
        .map_err(|e| EsError::internal(format!("bascule des templates: {e}")))?;
    Ok(())
}
