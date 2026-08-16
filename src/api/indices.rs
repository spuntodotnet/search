//! Cycle de vie d'un index : creation avec mapping explicite, suppression,
//! existence, mapping, refresh.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use super::{expect_only, parse_body, selection_options, Json, Params, SharedState};
use crate::engine::ActionAlias;
use crate::error::{EsError, EsResult};
use crate::mapping::Mapping;
use crate::selection::{index_unique, resoudre, Options};

/// `PUT /{index}` — creation avec mapping explicite obligatoire.
pub async fn create(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("wait_for_active_shards");
    p.opt("timeout");
    p.opt("master_timeout");
    p.done()?;

    let body = parse_body(&body)?;
    let obj = match &body {
        Value::Null => serde_json::Map::new(),
        Value::Object(o) => o.clone(),
        _ => {
            return Err(EsError::parsing(
                "le corps de [PUT /{index}] doit etre un objet",
            ))
        }
    };
    expect_only(&obj, &["mappings", "settings", "aliases"], "PUT /{index}")?;

    // Un template qui correspond s'applique aussi a une creation **explicite**,
    // et ce que le corps de la requete dit l'emporte. C'est ce que fait ES, et
    // c'est ce qui rend un `PUT /{index}` d'un script d'init compatible avec un
    // template pose juste avant.
    let tpl = st.catalog.templates().pour(&index);
    let settings = crate::templates::fusionner(
        tpl.as_ref().and_then(|t| t.settings.clone()),
        obj.get("settings").cloned(),
    );
    let mappings = crate::templates::fusionner(
        tpl.as_ref().and_then(|t| t.mappings.clone()),
        obj.get("mappings").cloned(),
    );
    let aliases = crate::templates::fusionner(
        tpl.as_ref().and_then(|t| t.aliases.clone()),
        obj.get("aliases").cloned(),
    );

    // Les alias sont poses **apres** la creation : un alias ne peut pas
    // designer un index qui n'existe pas encore.
    let mut alias_a_poser = Vec::new();
    if let Some(aliases) = &aliases {
        let decl = aliases
            .as_object()
            .ok_or_else(|| EsError::parsing("[PUT /{index}] : [aliases] doit etre un objet"))?;
        for (nom, corps) in decl {
            crate::alias::valider_nom(nom)?;
            alias_a_poser.push(ActionAlias::Ajouter {
                index: index.clone(),
                alias: nom.clone(),
                attache: crate::alias::lire_attache(corps, "PUT /{index}.aliases")?,
            });
        }
    }

    // Sans `mappings`, l'index part vide et se remplit par mapping dynamique,
    // comme chez ES.
    let (mapping, inertes) =
        crate::reglages::mapping_et_inertes(settings.as_ref(), mappings.as_ref())?;

    st.catalog.create(&index, mapping, inertes)?;
    if !alias_a_poser.is_empty() {
        // Un alias refuse laisse un index sans alias : on defait la creation
        // plutot que de rendre `acknowledged` sur une moitie de demande.
        if let Err(e) = st.catalog.modifier_alias(&alias_a_poser) {
            let _ = st.catalog.delete(&index);
            return Err(e);
        }
    }
    Ok(Json::ok(json!({
        "acknowledged": true,
        "shards_acknowledged": true,
        "index": index,
    })))
}

/// `DELETE /{index}` — un nom, une liste, un motif.
///
/// Le motif est ce qui rend une retention par index quotidien tenable :
/// `DELETE /audits-2026.07.*` en un appel, plutot qu'une boucle cote client qui
/// doit d'abord savoir quels index existent.
pub async fn delete(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    // Operations synchrones et immediates : ces delais n'ont rien a attendre.
    p.opt("timeout");
    p.opt("master_timeout");
    let opts = selection_options(&mut p)?;
    p.done()?;

    // `action.destructive_requires_name` : ES 8 refuse par defaut de supprimer
    // ce que le client n'a pas nomme. Le message est le sien, mot pour mot.
    if st.catalog.destructive_requires_name()
        && index
            .split(',')
            .map(str::trim)
            .any(|t| t.contains('*') || t == "_all")
    {
        return Err(EsError::illegal_argument(
            "Wildcard expressions or all indices are not allowed",
        ));
    }

    // Supprimer « l'index » designe par un alias effacerait des donnees que le
    // client n'a pas nommees : ES refuse, ferrite aussi.
    for terme in index.split(',').map(str::trim) {
        if st.catalog.est_alias(terme) {
            return Err(EsError::illegal_argument(format!(
                "The provided expression [{terme}] matches an alias, specify the corresponding \
                 concrete indices instead."
            )));
        }
    }

    let vises = match resoudre(&st.catalog, &index, &opts) {
        Ok(v) => v,
        Err(e) if opts.ignore_unavailable && e.ty == "index_not_found_exception" => Vec::new(),
        Err(e) => return Err(e),
    };
    for idx in vises {
        match st.catalog.delete(&idx.name) {
            Ok(()) => {}
            // Un autre appel a pu passer devant : l'index est parti, c'est le
            // resultat demande.
            Err(e) if e.ty == "index_not_found_exception" => {}
            Err(e) => return Err(e),
        }
    }
    Ok(Json::ok(json!({"acknowledged": true})))
}

/// `HEAD /{index}` — 200 ou 404, sans corps.
pub async fn exists(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
) -> Response {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p).unwrap_or_default();
    // Un motif qui ne correspond a rien reste un 200 chez ES : la question
    // posee est « cette expression est-elle resoluble ? », pas « trouve-t-elle
    // quelque chose ? ». Seul un nom concret absent rend 404.
    match resoudre(&st.catalog, &index, &opts) {
        Ok(_) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /{index}`
pub async fn get_index(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    p.done()?;

    let registre = st.catalog.aliases();
    let mut out = serde_json::Map::new();
    for idx in resoudre(&st.catalog, &index, &opts)? {
        let mut aliases = serde_json::Map::new();
        for (nom, cibles) in &registre {
            if let Some(attache) = cibles.get(&idx.name) {
                aliases.insert(nom.clone(), attache.to_json());
            }
        }
        out.insert(
            idx.name.clone(),
            json!({
                "aliases": Value::Object(aliases),
                "mappings": idx.mapping().to_json(),
                "settings": reglages_de(&idx),
            }),
        );
    }
    Ok(Json::ok(Value::Object(out)))
}

/// Les `settings` d'un index, au format d'ES (`{"index": {...}}`, valeurs en
/// chaines).
fn reglages_de(idx: &crate::engine::FerriteIndex) -> Value {
    crate::reglages::rendre(
        &idx.inertes(),
        &idx.uuid,
        &idx.name,
        idx.created_at,
        idx.mapping().allow_unmapped_fields,
    )
}

/// `GET /_settings` — tous les index, comme `_all`.
pub async fn get_settings_all(
    State(st): State<SharedState>,
    nom: Option<Path<String>>,
    uri: Uri,
) -> EsResult<Json> {
    get_settings_impl(st, "_all".to_string(), nom.map(|Path(n)| n), uri)
}

/// `PUT /_settings` — tous les index, comme `_all`.
pub async fn put_settings_all(
    State(st): State<SharedState>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    put_settings(State(st), Path("_all".to_string()), uri, body).await
}

/// `GET /{index}/_settings`
pub async fn get_settings(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    get_settings_impl(st, index, None, uri)
}

/// `GET /{index}/_settings/{nom}` — filtrer par nom de reglage.
pub async fn get_settings_nomme(
    State(st): State<SharedState>,
    Path((index, nom)): Path<(String, String)>,
    uri: Uri,
) -> EsResult<Json> {
    get_settings_impl(st, index, Some(nom), uri)
}

fn get_settings_impl(
    st: SharedState,
    index: String,
    noms: Option<String>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    p.opt("master_timeout");
    // Un seul noeud : « demande au noeud local » designe le meme etat.
    p.opt("local");
    let plat = p.flag("flat_settings", false)?;
    super::refuser_include_defaults(&mut p, "/{index}/_settings")?;
    p.done()?;

    // `{nom}` est une expression : une liste, des jokers, `_all`. Elle porte sur
    // les cles **aplaties** (`index.number_of_shards`), qu'on filtre avant de
    // rendre l'arborescence — sinon le meme nom filtrerait autrement selon
    // `flat_settings`.
    let motifs: Option<Vec<String>> = noms.map(|n| {
        n.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                if s == "_all" {
                    "*".to_string()
                } else {
                    s.to_string()
                }
            })
            .collect()
    });

    let mut out = serde_json::Map::new();
    for idx in resoudre(&st.catalog, &index, &opts)? {
        let mut reglages = crate::reglages::aplatir_reponse(&reglages_de(&idx));
        if let (Some(motifs), Value::Object(o)) = (&motifs, &mut reglages) {
            o.retain(|cle, _| motifs.iter().any(|m| crate::search::glob_match(m, cle)));
            // Aucun reglage retenu : ES laisse tomber l'index, il ne rend pas
            // une entree vide.
            if o.is_empty() {
                continue;
            }
        }
        if !plat {
            reglages = crate::reglages::nicher_reponse(&reglages);
        }
        out.insert(idx.name.clone(), json!({"settings": reglages}));
    }
    Ok(Json::ok(Value::Object(out)))
}

/// `PUT /{index}/_settings` — poser les reglages modifiables.
///
/// Ferrite n'a qu'un reglage qui change ses reponses, et il est fige a la
/// creation. Refuser la route entiere pour autant faisait echouer un script
/// d'init complet sur une ligne (`number_of_replicas: 1`) qui ne changerait
/// rien ici : les reglages **inertes** sont donc acceptes, gardes et rendus,
/// et tout le reste est refuse explicitement (voir [`crate::reglages`]).
pub async fn put_settings(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    p.opt("master_timeout");
    p.opt("timeout");
    let preserve_existing = p.flag("preserve_existing", false)?;
    if p.opt("reopen").is_some() {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [reopen] sur [PUT /{index}/_settings] : il rouvre l'index \
             pour poser un reglage fige, et ferrite n'a pas d'index ferme",
        ));
    }
    p.done()?;

    let body = parse_body(&body)?;
    let obj = body.as_object().ok_or_else(|| {
        EsError::parsing("le corps de [PUT /{index}/_settings] doit etre un objet")
    })?;
    // Les deux ecritures d'ES : les reglages a la racine, ou sous `settings`.
    let bloc = match obj.get("settings") {
        Some(v) if obj.len() == 1 => v.clone(),
        _ => body.clone(),
    };

    let vises = match resoudre(&st.catalog, &index, &opts) {
        Ok(v) => v,
        Err(e) if opts.ignore_unavailable && e.ty == "index_not_found_exception" => Vec::new(),
        Err(e) => return Err(e),
    };
    let cibles = vises
        .iter()
        .map(|i| format!("{}/{}", i.name, i.uuid))
        .collect::<Vec<_>>()
        .join(", ");
    let lus = crate::reglages::lire_pour_modification(&bloc, &cibles)?;
    // Ce que le corps met a `null` est efface, comme chez ES.
    let effaces = crate::reglages::cles_effacees(&bloc);

    for idx in &vises {
        idx.poser_inertes(&lus.inertes, &effaces, preserve_existing)?;
    }
    Ok(Json::ok(json!({"acknowledged": true})))
}

/// `GET /{index}/_mapping`
pub async fn get_mapping(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    p.done()?;
    let mut out = serde_json::Map::new();
    for idx in resoudre(&st.catalog, &index, &opts)? {
        out.insert(
            idx.name.clone(),
            json!({"mappings": idx.mapping().to_json()}),
        );
    }
    Ok(Json::ok(Value::Object(out)))
}

/// `GET /_mapping` — tous les index, comme `_all`.
pub async fn get_mapping_all(State(st): State<SharedState>, uri: Uri) -> EsResult<Json> {
    get_mapping(State(st), Path("_all".to_string()), uri).await
}

/// `POST /_refresh` — tous les index.
pub async fn refresh_all(State(st): State<SharedState>, uri: Uri) -> EsResult<Json> {
    refresh(State(st), Path("_all".to_string()), uri).await
}

/// `PUT /{index}/_mapping` — ajoute des champs au mapping.
///
/// Possible depuis que le schema vit dans des generations. Comme chez ES, on
/// **ajoute** des champs : en changer le type reste refuse.
pub async fn put_mapping(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    p.opt("timeout");
    p.opt("master_timeout");
    p.done()?;

    let body = parse_body(&body)?;
    let mapping = Mapping::parse(&body)?;
    if mapping.dynamic != crate::mapping::Dynamic::default() {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas la modification de [dynamic] sur un index existant",
        ));
    }

    // `PUT /{index}/_mapping` accepte plusieurs index chez ES : le meme champ
    // est ajoute a chacun.
    let vises = resoudre(&st.catalog, &index, &Options::default())?;
    for idx in vises {
        let props = mapping.properties.clone();
        tokio::task::spawn_blocking(move || idx.add_fields(props))
            .await
            .map_err(|e| EsError::internal(format!("put_mapping: {e}")))??;
    }
    Ok(Json::ok(json!({"acknowledged": true})))
}

/// `POST|GET /{index}/_analyze` et `/_analyze` — montre comment un texte est
/// decoupe en termes.
///
/// C'est l'API qui rend l'analyse **verifiable** : le meme appel sur ferrite et
/// sur Elasticsearch doit rendre les memes tokens, et
/// `tests/compat/diff_analyzers.py` s'en sert pour le mesurer.
pub async fn analyze(
    State(st): State<SharedState>,
    index: Option<Path<String>>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    Params::parse(&uri).done()?;
    let body = parse_body(&body)?;
    let obj = body
        .as_object()
        .ok_or_else(|| EsError::parsing("le corps de [_analyze] doit etre un objet"))?;
    expect_only(obj, &["text", "analyzer", "field"], "_analyze")?;

    let textes: Vec<String> = match obj.get("text") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| EsError::illegal_argument("[_analyze.text] : chaines attendues"))
            })
            .collect::<EsResult<_>>()?,
        _ => {
            return Err(EsError::illegal_argument(
                "[_analyze] : [text] est obligatoire",
            ))
        }
    };

    // Soit un analyzer nomme, soit celui d'un champ de l'index.
    let (analyzer, gen) = match (obj.get("analyzer"), obj.get("field")) {
        (Some(_), Some(_)) => {
            return Err(EsError::illegal_argument(
                "[_analyze] : [analyzer] et [field] sont exclusifs",
            ))
        }
        (Some(a), None) => {
            let nom = a.as_str().ok_or_else(|| {
                EsError::illegal_argument("[_analyze.analyzer] : chaine attendue")
            })?;
            let gen = match &index {
                Some(Path(nom_index)) => Some(index_unique(&st.catalog, nom_index)?.current()),
                None => None,
            };
            // Un analyzer sur mesure n'existe que dans son index : `_analyze`
            // sans index ne connait que les analyzers integres.
            let declares = gen
                .as_ref()
                .map(|g| g.mapping.analysis.clone())
                .unwrap_or_default();
            (
                crate::analysis::parse_declaration(nom, "_analyze", &declares)?,
                gen,
            )
        }
        (None, Some(f)) => {
            let Path(nom_index) = index.as_ref().ok_or_else(|| {
                EsError::illegal_argument("[_analyze.field] exige un index dans l'URL")
            })?;
            let champ = f
                .as_str()
                .ok_or_else(|| EsError::illegal_argument("[_analyze.field] : chaine attendue"))?;
            let gen = index_unique(&st.catalog, nom_index)?.current();
            let mapped = gen.fields.get(champ).ok_or_else(|| {
                EsError::illegal_argument(format!("[_analyze] : champ [{champ}] inconnu"))
            })?;
            (mapped.analyzer, Some(gen))
        }
        (None, None) => (crate::analysis::Analyzer::default(), None),
    };

    // Sans index, on se sert d'un gestionnaire de tokenizers autonome.
    let manager = match &gen {
        Some(g) => g.index.tokenizers().clone(),
        None => {
            let m = tantivy::tokenizer::TokenizerManager::default();
            crate::analysis::register_all(&m);
            m
        }
    };

    let mut tokens = Vec::new();
    let mut decalage = 0usize;
    for texte in &textes {
        for t in crate::analysis::analyser(&manager, analyzer, texte)? {
            tokens.push(json!({
                "token": t.text,
                "start_offset": decalage + t.start_offset,
                "end_offset": decalage + t.end_offset,
                "type": "<ALPHANUM>",
                "position": t.position,
            }));
        }
        decalage += texte.chars().count();
    }
    Ok(Json::ok(json!({"tokens": tokens})))
}

/// `POST /{index}/_refresh`
pub async fn refresh(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    p.done()?;
    let index = resoudre(&st.catalog, &index, &opts)?;
    let nb = index.len();
    tokio::task::spawn_blocking(move || {
        for idx in index {
            idx.refresh()?;
        }
        Ok::<_, EsError>(())
    })
    .await
    .map_err(|e| EsError::internal(format!("refresh: {e}")))??;
    // Un index = un shard : ES compte les shards touches, pas les index.
    Ok(Json::ok(
        json!({"_shards": {"total": nb, "successful": nb, "failed": 0}}),
    ))
}
