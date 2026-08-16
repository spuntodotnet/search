//! `_field_caps` : ce que chaque champ sait faire, index par index.
//!
//! Toute l'information est deja dans le mapping — c'est ce qui rend cette route
//! petite. Ce qu'elle apporte, c'est l'**agregation** : sur une liste d'index,
//! le meme nom de champ peut porter deux types, et un outil de decouverte a
//! besoin de le savoir avant de proposer un filtre qui echouerait sur la
//! moitie des index.
//!
//! `searchable` et `aggregatable` se deduisent du type, parce que ferrite n'a
//! ni `index: false` ni `doc_values: false` : un `text` n'est pas agregeable,
//! un `object` (et un `nested`) n'est ni l'un ni l'autre, tout le reste est
//! les deux. Mesure faite contre un vrai ES 8.15.

use std::collections::{BTreeMap, BTreeSet};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::Uri;
use serde_json::{json, Map, Value};

use super::{expect_only, parse_body, selection_options, Json, Params, SharedState};
use crate::error::{EsError, EsResult};
use crate::mapping::{FieldType, Mapping};
use crate::search::glob_match;
use crate::selection::resoudre;

/// Ce qu'une entree de `fields` dit d'un type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Capacites {
    /// Le nom du type tel qu'ES le rend (`keyword`, `object`, `unmapped`…).
    ty: &'static str,
    searchable: bool,
    aggregatable: bool,
}

/// Le type « pas dans cet index », rendu par `include_unmapped=true`.
const UNMAPPED: Capacites = Capacites {
    ty: "unmapped",
    searchable: false,
    aggregatable: false,
};

/// Un conteneur : ES l'expose, mais on n'y cherche ni n'y agrege.
const OBJET: Capacites = Capacites {
    ty: "object",
    searchable: false,
    aggregatable: false,
};

const NESTED: Capacites = Capacites {
    ty: "nested",
    searchable: false,
    aggregatable: false,
};

const JOIN: Capacites = Capacites {
    ty: "join",
    searchable: true,
    aggregatable: true,
};

fn capacites_de(ty: FieldType) -> Capacites {
    Capacites {
        ty: ty.name(),
        searchable: true,
        // Un `text` est analyse : ES ne l'agrege pas sans `fielddata`, que
        // ferrite n'a pas.
        aggregatable: ty != FieldType::Text,
    }
}

/// `GET|POST /_field_caps`
pub async fn field_caps_all(
    State(st): State<SharedState>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    field_caps(State(st), Path("_all".to_string()), uri, body).await
}

/// `GET|POST /{index}/_field_caps`
pub async fn field_caps(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    let motifs_url = p.list("fields");
    let include_unmapped = p.flag("include_unmapped", false)?;
    p.done()?;

    let body = parse_body(&body)?;
    let (motifs_corps, index_filter) = match &body {
        Value::Null => (None, None),
        Value::Object(o) => {
            expect_only(o, &["fields", "index_filter"], "_field_caps")?;
            let motifs = match o.get("fields") {
                None => None,
                Some(v) => Some(liste_de_champs(v)?),
            };
            (motifs, o.get("index_filter").cloned())
        }
        _ => {
            return Err(EsError::parsing(
                "le corps de [_field_caps] doit etre un objet",
            ))
        }
    };

    // ES refuse la demande plutot que de rendre tous les champs par defaut :
    // sans `fields`, il n'y a rien a decrire.
    let motifs =
        match (motifs_url, motifs_corps) {
            (Some(a), None) => a,
            (None, Some(b)) => b,
            // Les deux : ES prend celui de la query string. On refuse plutot que de
            // choisir en silence.
            (Some(_), Some(_)) => return Err(EsError::illegal_argument(
                "[_field_caps] : [fields] est fourni dans l'URL et dans le corps ; n'en fournis \
                 qu'un",
            )),
            (None, None) => {
                return Err(EsError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "action_request_validation_exception",
                    "Validation Failed: 1: no fields specified;",
                ))
            }
        };

    let indices = resoudre(&st.catalog, &index, &opts)?;
    let indices = match &index_filter {
        None => indices,
        Some(f) => filtrer_les_index(indices, f).await?,
    };
    let noms: Vec<String> = indices.iter().map(|i| i.name.clone()).collect();

    // `champ -> type -> index qui le portent`. Les deux niveaux sont ordonnes
    // pour que la reponse ne depende pas de l'ordre de parcours.
    let mut par_champ: BTreeMap<String, BTreeMap<Capacites, BTreeSet<String>>> = BTreeMap::new();
    for idx in &indices {
        let mapping = idx.mapping();
        for (champ, cap) in champs_de(&mapping) {
            if !motifs.iter().any(|m| glob_match(m, &champ)) {
                continue;
            }
            par_champ
                .entry(champ)
                .or_default()
                .entry(cap)
                .or_default()
                .insert(idx.name.clone());
        }
    }

    if include_unmapped {
        for types in par_champ.values_mut() {
            let vus: BTreeSet<&String> = types.values().flatten().collect();
            let absents: BTreeSet<String> =
                noms.iter().filter(|n| !vus.contains(n)).cloned().collect();
            if !absents.is_empty() {
                types.insert(UNMAPPED, absents);
            }
        }
    }

    let mut fields = Map::new();
    for (champ, types) in par_champ {
        // ES ne pose `indices` que quand le champ a **plusieurs** entrees :
        // avec une seule, la reponse ne dit rien de plus que la liste d'index
        // deja rendue en tete. Mesure contre ES 8.15 — un champ present dans un
        // seul des deux index vises n'a pas de `indices` tant qu'il n'a qu'un
        // type.
        let detaille = types.len() > 1;
        let mut par_type = Map::new();
        for (cap, porteurs) in types {
            let mut o = Map::new();
            o.insert("type".into(), json!(cap.ty));
            o.insert("metadata_field".into(), json!(false));
            o.insert("searchable".into(), json!(cap.searchable));
            o.insert("aggregatable".into(), json!(cap.aggregatable));
            if detaille {
                o.insert(
                    "indices".into(),
                    Value::Array(porteurs.into_iter().map(Value::String).collect()),
                );
            }
            par_type.insert(cap.ty.to_string(), Value::Object(o));
        }
        fields.insert(champ, Value::Object(par_type));
    }

    Ok(Json::ok(json!({
        "indices": noms,
        "fields": Value::Object(fields),
    })))
}

/// `index_filter` : ne garder que les index qui ont au moins un document
/// correspondant.
///
/// C'est ce qu'ES appelle « le shard peut-il correspondre » — sur un index
/// quotidien derriere un motif, il evite de decrire mille index dont un seul
/// porte la periode demandee. ferrite etant mono-shard, la question se pose une
/// fois par index, et se repond en comptant un document.
///
/// Un index dont le mapping ne connait pas le champ filtre est **ecarte**, pas
/// remonte en erreur : c'est la reponse a « cet index a-t-il quelque chose dans
/// cette periode », et la reponse est non.
async fn filtrer_les_index(
    indices: Vec<std::sync::Arc<crate::engine::FerriteIndex>>,
    filtre: &Value,
) -> EsResult<Vec<std::sync::Arc<crate::engine::FerriteIndex>>> {
    let maintenant = crate::datemath::maintenant();
    let mut prets = Vec::new();
    for idx in indices {
        let gen = idx.current();
        let query = {
            let searcher = gen.searcher();
            let ctx = crate::dsl::QueryCtx::new(&gen.fields, &gen.index, &searcher)
                .avec_maintenant(maintenant)
                .selon_le_mapping(&gen.mapping);
            crate::dsl::build_query(filtre, &ctx)
        };
        match query {
            Ok(q) => prets.push((idx, gen, q)),
            Err(e) if e.champ_inconnu.is_some() => {}
            Err(e) => return Err(e),
        }
    }
    tokio::task::spawn_blocking(move || {
        let mut gardes = Vec::new();
        for (idx, gen, query) in prets {
            if gen.searcher().search(&query, &tantivy::collector::Count)? > 0 {
                gardes.push(idx);
            }
        }
        Ok::<_, EsError>(gardes)
    })
    .await
    .map_err(|e| EsError::internal(format!("field_caps: {e}")))?
}

/// Les champs d'un mapping, chemins pointes, conteneurs compris.
///
/// Un `object` n'est pas un champ (ferrite indexe des chemins), mais ES le
/// rend : un outil de decouverte s'en sert pour construire son arborescence.
/// On le reconstitue donc depuis les prefixes des chemins mappes.
fn champs_de(mapping: &Mapping) -> Vec<(String, Capacites)> {
    let mut out: BTreeMap<String, Capacites> = BTreeMap::new();
    for (chemin, fm) in &mapping.properties {
        out.insert(chemin.clone(), capacites_de(fm.ty));
        for (sous, sfm) in &fm.fields {
            out.insert(format!("{chemin}.{sous}"), capacites_de(sfm.ty));
        }
        // Les prefixes du chemin sont les objets qui le contiennent.
        let parts: Vec<&str> = chemin.split('.').collect();
        for i in 1..parts.len() {
            out.entry(parts[..i].join(".")).or_insert(OBJET);
        }
    }
    for vide in &mapping.objets_vides {
        out.entry(vide.clone()).or_insert(OBJET);
    }
    // `nested` l'emporte sur `object` : c'est ce que le conteneur est vraiment.
    for racine in &mapping.nested {
        out.insert(racine.clone(), NESTED);
    }
    if let Some(j) = &mapping.join {
        out.insert(j.champ.clone(), JOIN);
    }
    out.into_iter().collect()
}

/// `fields` du corps : une chaine ou une liste de chaines, comme chez ES.
fn liste_de_champs(v: &Value) -> EsResult<Vec<String>> {
    match v {
        Value::String(s) => Ok(s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                x.as_str().map(str::to_string).ok_or_else(|| {
                    EsError::illegal_argument("[_field_caps.fields] : chaines attendues")
                })
            })
            .collect(),
        _ => Err(EsError::illegal_argument(
            "[_field_caps.fields] : une chaine ou une liste de chaines attendue",
        )),
    }
}
