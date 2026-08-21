//! Les reglages d'index : ce que ferrite exploite, ce qu'il accepte sans
//! effet, et ce qu'il refuse.
//!
//! Un seul endroit pour les trois, parce qu'ils sont lus depuis trois routes
//! (`PUT /{index}`, `PUT /{index}/_settings`, un template applique a la
//! creation) et qu'un reglage accepte a un endroit et refuse a l'autre est un
//! piege qu'on ne trouve qu'en production.
//!
//! Trois familles, et la frontiere entre les deux dernieres est le coeur du
//! sujet :
//!
//! * **exploite** — `index.query.parse.allow_unmapped_fields`, le seul reglage
//!   qui change ce que ferrite repond ;
//! * **inerte** — ES l'accepte, ferrite le garde et le rend, et il ne change
//!   rien parce qu'il decrit deja ce que ferrite est (mono-shard, sans
//!   replique). Le refuser ferait echouer un script d'init entier sur une ligne
//!   qui ne changerait rien ;
//! * **refuse** — tout le reste. Un reglage accepte puis ignore alors qu'il
//!   changerait le comportement (`index.blocks.read_only`,
//!   `index.max_result_window`) est exactement l'echec silencieux que ce projet
//!   refuse.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::error::{EsError, EsResult};

/// Le seul reglage d'index que ferrite exploite (voir
/// [`crate::mapping::Mapping::allow_unmapped_fields`]).
pub const ALLOW_UNMAPPED: &str = "index.query.parse.allow_unmapped_fields";

/// L'ecart maximal entre `max_gram` et `min_gram` d'un `ngram` (defaut 1).
///
/// Il n'est pas inerte : il **valide** ce que `settings.analysis` declare, et
/// c'est le seul reglage d'index dans ce cas. Il est stocke avec les inertes
/// parce qu'ES le rend dans `GET /_settings` comme les autres.
pub const MAX_NGRAM_DIFF: &str = "index.max_ngram_diff";

/// Les reglages acceptes, gardes et rendus — sans effet sur les reponses.
///
/// `number_of_shards` et `number_of_replicas` decrivent deja ce que ferrite
/// est. `refresh_interval` merite un mot : ferrite rafraichit toutes les
/// secondes, donc une valeur **positive** est toujours honoree au sens ou ES la
/// definit (« les ecritures sont visibles au plus tard apres ce delai ») ;
/// seule la valeur `-1`, qui demande a ne **pas** rafraichir, change quelque
/// chose — et celle-la est vraiment appliquee (voir
/// [`crate::engine::Catalog::refresh_dirty`]).
pub const INERTES: &[&str] = &[
    "index.number_of_shards",
    "index.number_of_replicas",
    "index.auto_expand_replicas",
    "index.refresh_interval",
];

/// Les reglages qu'ES fige a la creation : les modifier est une erreur, chez
/// lui comme ici.
const FIGES: &[&str] = &["index.number_of_shards"];

/// Ce qu'un bloc `settings` a dit.
#[derive(Debug, Default, Clone)]
pub struct Reglages {
    /// `allow_unmapped_fields`, s'il a ete pose.
    pub allow_unmapped: Option<bool>,
    /// Les reglages inertes, cle complete (`index.…`) et valeur en chaine,
    /// comme ES les stocke et les rend.
    pub inertes: BTreeMap<String, String>,
}

/// Lit un bloc `settings`, refuse ce qui n'est ni exploite ni inerte.
///
/// Un reglage d'ES s'ecrit aussi bien a plat (`"index.number_of_shards": 1`)
/// qu'imbrique (`{"index": {"number_of_shards": 1}}`), et les clients melangent
/// les deux : on aplatit tout avant de comparer, sinon la meme demande serait
/// acceptee sous une forme et refusee sous l'autre.
pub fn lire(settings: &Value) -> EsResult<Reglages> {
    let obj = settings
        .as_object()
        .ok_or_else(|| EsError::parsing("[settings] doit etre un objet"))?;
    let mut plats: Vec<(String, &Value)> = Vec::new();
    aplatir("", obj, &mut plats);

    let mut out = Reglages::default();
    for (cle, valeur) in plats {
        let complete = complete(&cle);
        if complete == ALLOW_UNMAPPED {
            out.allow_unmapped = Some(lire_booleen(valeur, &cle)?);
            continue;
        }
        if complete == MAX_NGRAM_DIFF {
            if valeur.is_null() {
                out.inertes.remove(&complete);
                continue;
            }
            let n = lire_entier(valeur, &complete)?;
            if n < 0 {
                return Err(EsError::illegal_argument(format!(
                    "Failed to parse value [{n}] for setting [{complete}] must be >= 0"
                )));
            }
            out.inertes.insert(complete, n.to_string());
            continue;
        }
        if INERTES.contains(&complete.as_str()) {
            // `null` efface le reglage, comme chez ES (`PUT _settings` avec une
            // valeur nulle remet le defaut).
            if valeur.is_null() {
                out.inertes.remove(&complete);
            } else {
                out.inertes.insert(complete, en_chaine(valeur));
            }
            continue;
        }
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas le reglage d'index [{cle}] ; reglages acceptes : {INERTES:?} \
             (sans effet, ferrite etant mono-shard), [{ALLOW_UNMAPPED}] et [{MAX_NGRAM_DIFF}]"
        )));
    }
    Ok(out)
}

/// Le meme, pour `PUT /{index}/_settings` : un reglage fige a la creation n'y
/// est pas modifiable, et ES le dit avec ce message-la.
pub fn lire_pour_modification(settings: &Value, cibles: &str) -> EsResult<Reglages> {
    let obj = settings
        .as_object()
        .ok_or_else(|| EsError::parsing("[settings] doit etre un objet"))?;
    let mut plats: Vec<(String, &Value)> = Vec::new();
    aplatir("", obj, &mut plats);
    let bloques: Vec<String> = plats
        .iter()
        .map(|(cle, _)| complete(cle))
        .filter(|c| FIGES.contains(&c.as_str()))
        .collect();
    if !bloques.is_empty() {
        return Err(EsError::illegal_argument(format!(
            "Can't update non dynamic settings [[{}]] for open indices [[{cibles}]] unless the \
             `reopen` query parameter is set to true. Alternatively, close the indices, apply the \
             settings changes, and reopen the indices",
            bloques.join(", ")
        )));
    }
    let lus = lire(settings)?;
    if lus.allow_unmapped.is_some() {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas de changer [{ALLOW_UNMAPPED}] apres la creation de l'index : \
             il est fige dans la generation courante du schema, et un client qui le croirait \
             change chercherait longtemps (pose-le dans [settings] a la creation)"
        )));
    }
    Ok(lus)
}

/// `settings.analysis`, sous ses deux ecritures (`{"analysis": …}` ou
/// `{"index": {"analysis": …}}`).
pub fn section_analysis(settings: &Value) -> Option<&Value> {
    let o = settings.as_object()?;
    o.get("analysis")
        .or_else(|| o.get("index")?.as_object()?.get("analysis"))
}

/// La forme que rend ES : `{"index": {…}}`, cles nichees, valeurs en chaines.
pub fn rendre(
    inertes: &BTreeMap<String, String>,
    uuid: &str,
    nom: &str,
    creation: i64,
    allow_unmapped: bool,
) -> Value {
    let mut index = json!({
        "number_of_shards": "1",
        "number_of_replicas": "0",
        "uuid": uuid,
        "provided_name": nom,
        "creation_date": creation.to_string(),
        "version": {"created": crate::ES_VERSION},
    });
    // Ce que le client a pose l'emporte sur le defaut affiche : c'est ce qu'ES
    // rend, et c'est ce qu'un script d'init relit pour se verifier.
    for (cle, valeur) in inertes {
        let court = cle.strip_prefix("index.").unwrap_or(cle);
        nicher(&mut index, court, json!(valeur));
    }
    // Comme chez ES, le reglage n'apparait que s'il a ete pose.
    if !allow_unmapped {
        nicher(
            &mut index,
            "query.parse.allow_unmapped_fields",
            json!("false"),
        );
    }
    json!({ "index": index })
}

/// Aplatit une reponse `{"index": {"a": {"b": 1}}}` en
/// `{"index.a.b": 1}` — c'est ce que fait `flat_settings` chez ES.
pub fn aplatir_reponse(v: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Value::Object(o) = v {
        aplatir_json("", o, &mut out);
    }
    Value::Object(out)
}

fn aplatir_json(
    prefixe: &str,
    o: &serde_json::Map<String, Value>,
    out: &mut serde_json::Map<String, Value>,
) {
    for (cle, valeur) in o {
        let chemin = if prefixe.is_empty() {
            cle.clone()
        } else {
            format!("{prefixe}.{cle}")
        };
        match valeur {
            Value::Object(sous) => aplatir_json(&chemin, sous, out),
            autre => {
                out.insert(chemin, autre.clone());
            }
        }
    }
}

/// L'inverse d'[`aplatir_reponse`] : `{"index.a.b": 1}` redevient
/// `{"index": {"a": {"b": 1}}}`.
pub fn nicher_reponse(v: &Value) -> Value {
    let mut out = json!({});
    if let Value::Object(o) = v {
        for (cle, valeur) in o {
            nicher(&mut out, cle, valeur.clone());
        }
    }
    out
}

fn nicher(cible: &mut Value, chemin: &str, valeur: Value) {
    let mut courant = cible;
    let parts: Vec<&str> = chemin.split('.').collect();
    for (i, part) in parts.iter().enumerate() {
        if i + 1 == parts.len() {
            if let Value::Object(o) = courant {
                o.insert((*part).to_string(), valeur);
            }
            return;
        }
        let Value::Object(o) = courant else { return };
        courant = o
            .entry((*part).to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
    }
}

fn complete(cle: &str) -> String {
    if cle.starts_with("index.") {
        cle.to_string()
    } else {
        format!("index.{cle}")
    }
}

/// ES stocke et rend tous les reglages en chaines, quel que soit ce qu'on lui
/// a envoye : `1` ressort `"1"`.
fn en_chaine(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        autre => autre.to_string(),
    }
}

/// Aplatit les `settings` en chemins pointes. `analysis` est laisse de cote :
/// il est traite ailleurs (il est exploite, lui).
fn aplatir<'a>(
    prefixe: &str,
    obj: &'a serde_json::Map<String, Value>,
    out: &mut Vec<(String, &'a Value)>,
) {
    for (cle, valeur) in obj {
        let chemin = if prefixe.is_empty() {
            cle.clone()
        } else {
            format!("{prefixe}.{cle}")
        };
        if chemin == "analysis" || chemin == "index.analysis" {
            continue;
        }
        match valeur {
            Value::Object(o) => aplatir(&chemin, o, out),
            _ => out.push((chemin, valeur)),
        }
    }
}

/// L'ecart maximal declare par l'index, ou le defaut d'ES.
pub fn max_ngram_diff(inertes: &BTreeMap<String, String>) -> i64 {
    inertes
        .get(MAX_NGRAM_DIFF)
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(crate::ngram::MAX_NGRAM_DIFF_DEFAUT)
}

/// ES accepte un entier comme sa forme chaine (`"12"`) dans les settings.
fn lire_entier(v: &Value, cle: &str) -> EsResult<i64> {
    match v {
        Value::Number(n) if n.is_i64() => Ok(n.as_i64().unwrap_or_default()),
        Value::String(s) => s.trim().parse::<i64>().map_err(|_| {
            EsError::illegal_argument(format!(
                "Failed to parse value [{s}] for setting [{cle}] as a number"
            ))
        }),
        autre => Err(EsError::illegal_argument(format!(
            "Failed to parse value [{autre}] for setting [{cle}] as a number"
        ))),
    }
}

/// ES accepte un booleen comme sa forme chaine (`"false"`) dans les settings.
fn lire_booleen(v: &Value, cle: &str) -> EsResult<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::String(s) if s == "true" => Ok(true),
        Value::String(s) if s == "false" => Ok(false),
        autre => Err(EsError::illegal_argument(format!(
            "Failed to parse value [{autre}] as only [true] or [false] are allowed for setting \
             [{cle}]"
        ))),
    }
}

/// `index.refresh_interval: -1` : ne pas rafraichir tout seul.
///
/// C'est la seule valeur de ce reglage qui change quelque chose ici — une
/// valeur positive demande « visible au plus tard apres ce delai », et ferrite
/// rafraichit toutes les secondes.
pub fn rafraichissement_desactive(inertes: &BTreeMap<String, String>) -> bool {
    matches!(
        inertes.get("index.refresh_interval").map(String::as_str),
        Some("-1")
    )
}

/// Construit le mapping (analyzers et `allow_unmapped_fields` compris) et les
/// reglages inertes d'un index a partir de ses deux blocs de declaration.
///
/// C'est le meme geste pour une creation explicite (`PUT /{index}`) et pour un
/// template applique a une creation implicite : deux chemins qui liraient les
/// memes blocs differemment finiraient par diverger.
pub fn mapping_et_inertes(
    settings: Option<&Value>,
    mappings: Option<&Value>,
) -> EsResult<(crate::mapping::Mapping, BTreeMap<String, String>)> {
    let mut declares = crate::analysis::Analysis::default();
    let mut lus = Reglages::default();
    if let Some(s) = settings {
        // Les reglages d'abord : `index.max_ngram_diff` valide ce que la
        // section `analysis` declare, donc le lire apres elle le rendrait sans
        // effet sur la declaration qu'il est cense borner.
        lus = lire(s)?;
        // `analysis` est extrait a part : c'est la seule section de `settings`
        // qui declare quelque chose plutot que de regler.
        if let Some(a) = section_analysis(s) {
            declares = crate::analysis::Analysis::parse(a, max_ngram_diff(&lus.inertes))?;
        }
    }
    let mut mapping = match mappings {
        Some(m) => crate::mapping::Mapping::parse_avec(m, &declares)?,
        None => crate::mapping::Mapping::default(),
    };
    mapping.analysis = declares;
    if let Some(v) = lus.allow_unmapped {
        mapping.allow_unmapped_fields = v;
    }
    Ok((mapping, lus.inertes))
}

/// Les reglages qu'un corps met explicitement a `null` : chez ES, c'est ce qui
/// **efface** un reglage et le rend a son defaut.
pub fn cles_effacees(settings: &Value) -> Vec<String> {
    let Some(obj) = settings.as_object() else {
        return Vec::new();
    };
    let mut plats: Vec<(String, &Value)> = Vec::new();
    aplatir("", obj, &mut plats);
    plats
        .into_iter()
        .filter(|(_, v)| v.is_null())
        .map(|(cle, _)| complete(&cle))
        .collect()
}
