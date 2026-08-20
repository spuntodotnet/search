//! `_delete_by_query` et `_update_by_query` : la couche HTTP.
//!
//! Les deux routes partagent tout sauf trois choses, et ces trois-la ne sont pas
//! des details :
//!
//! * `_update_by_query` **sans corps** vaut `match_all` ; `_delete_by_query`
//!   sans `query` rend 400 `query is missing`. Purger tout un index par omission
//!   n'est pas quelque chose qu'on laisse arriver, et ES ne le laisse pas non
//!   plus (mesure contre ES 8.15) ;
//! * la reponse de `_delete_by_query` ne porte **pas** `updated` ;
//! * `script` n'a de sens que sur `_update_by_query`, et il est refuse : c'est
//!   Painless, hors perimetre.
//!
//! Le reste — `refresh`, `conflicts`, `max_docs`, `scroll_size` — est commun, et
//! chaque parametre non tenu est refuse par son nom. `wait_for_completion=false`
//! en particulier : il rend une **tache** chez ES, et ferrite n'a pas d'API de
//! taches. Rendre un identifiant de tache bidon serait le pire des deux mondes,
//! puisque le client irait ensuite le suivre sur `_tasks`.

use std::sync::Arc;
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use serde_json::{Map, Value};

use super::{elapsed_ms, parse_body, selection_options, Json, Params, SharedState};
use crate::dsl::{build_query, QueryCtx};
use crate::engine::Generation;
use crate::error::{EsError, EsResult};
use crate::parrequete::{self, Cible, Demande, Geste, LOT_MAX, LOT_PAR_DEFAUT};
use crate::selection::resoudre;

/// `POST /{index}/_delete_by_query`
pub async fn supprimer(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    executer(st, index, uri, body, Geste::Supprimer).await
}

/// `POST /{index}/_update_by_query`
pub async fn reindexer(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    executer(st, index, uri, body, Geste::Reindexer).await
}

fn route(geste: Geste) -> &'static str {
    match geste {
        Geste::Supprimer => "_delete_by_query",
        Geste::Reindexer => "_update_by_query",
    }
}

async fn executer(
    st: SharedState,
    index: String,
    uri: Uri,
    body: Bytes,
    geste: Geste,
) -> EsResult<Json> {
    let started = Instant::now();
    let nom_route = route(geste);

    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    let rafraichir = refresh_strict(&mut p)?;
    let conflits_param = conflits(&mut p)?;
    let max_docs_param = p.number("max_docs")?;
    let taille_de_lot = taille_de_lot(&mut p)?;
    // Un shard, une replique : ces deux-la sont deja satisfaits quand la
    // requete arrive. Acceptes et sans objet, comme partout ailleurs dans
    // ferrite.
    p.opt("timeout");
    p.opt("wait_for_active_shards");
    p.opt("preference");
    refuser_les_parametres_non_tenus(&mut p, nom_route, geste)?;
    p.done()?;

    let corps = parse_body(&body)?;
    let (requete, du_corps) = lire_corps(&corps, geste, nom_route)?;

    // `max_docs` et `conflicts` s'ecrivent des deux cotes, et il faut les lire
    // des deux cotes : le **client officiel** met `max_docs` dans le corps sur
    // les deux routes, et `conflicts` dans le corps sur `_update_by_query`. Les
    // refuser dans le corps rendait `client.update_by_query(conflicts=...)`
    // inutilisable — trouve par le harnais, pas par la lecture de l'API.
    let max_docs = match (du_corps.max_docs, max_docs_param) {
        (Some(a), Some(b)) if a != b => {
            return Err(EsError::illegal_argument(format!(
                "[max_docs] set to two different values [{a}] and [{b}]"
            )))
        }
        (a, b) => a.or(b),
    };
    // Le parametre l'emporte sur le corps quand les deux sont la (mesure contre
    // ES 8.15 : `?conflicts=abort` avec `"conflicts": "proceed"` dans le corps
    // s'arrete au premier conflit).
    let proceder = conflits_param.or(du_corps.proceder).unwrap_or(false);

    if max_docs == Some(0) {
        // Le message d'ES, mot pour mot : `slices` vaut 1 par defaut, et
        // `max_docs` doit lui rester superieur.
        return Err(EsError::illegal_argument(
            "[max_docs] should be >= [slices]",
        ));
    }

    let indices = resoudre(&st.catalog, &index, &opts)?;
    // Une seule generation par index pour toute la commande : les `Field` d'une
    // generation n'ont aucun sens dans une autre.
    let generations: Vec<Arc<Generation>> = indices.iter().map(|i| i.current()).collect();
    let champs_connus = crate::api::search::union_des_champs_de(&generations);
    let maintenant = crate::datemath::maintenant();

    // Aucun index vise : la boucle ci-dessous ne tourne pas, donc rien du corps
    // ne serait lu. ES, lui, refuse quand meme une requete mal formee (mesure :
    // `POST /rien-*/_delete_by_query` avec `{"query": {"zzz": {}}}` rend 400 sur
    // un cluster vide). C'est le meme geste que la recherche sans index.
    if generations.is_empty() {
        crate::api::search::valider_sans_index_query(requete.as_ref(), maintenant)?;
    }

    let mut cibles: Vec<Cible> = Vec::new();
    let mut ignore: Option<EsError> = None;
    for (idx, gen) in indices.iter().zip(&generations) {
        let searcher = gen.searcher();
        let ctx = QueryCtx::new(&gen.fields, &gen.index, &searcher)
            .avec_champs_ailleurs(&champs_connus)
            .avec_maintenant(maintenant)
            .selon_le_mapping(&gen.mapping);
        let query = match &requete {
            Some(v) => build_query(v, &ctx),
            None => Ok(Box::new(tantivy::query::AllQuery) as Box<dyn tantivy::query::Query>),
        };
        match query {
            Ok(q) => cibles.push(Cible {
                index: idx.clone(),
                gen: gen.clone(),
                query: q,
            }),
            // Le champ est inconnu de **cet** index : si un autre index vise le
            // connait, la clause ne correspond simplement a rien ici (mapping
            // heterogene). Sinon, l'erreur sort une fois la boucle finie.
            Err(e) if e.champ_inconnu.is_some() => {
                ignore.get_or_insert(e);
            }
            Err(e) => return Err(e),
        }
    }
    if cibles.is_empty() {
        if let Some(e) = ignore {
            return Err(e);
        }
    }

    let demande = Demande {
        max_docs,
        taille_de_lot,
        proceder_sur_conflit: proceder,
    };
    let bilan = tokio::task::spawn_blocking(move || parrequete::executer(&cibles, geste, &demande))
        .await
        .map_err(|e| EsError::internal(format!("{}: {e}", route(geste))))??;

    if rafraichir {
        for idx in &indices {
            idx.refresh()?;
        }
    }

    // Un conflit n'est pas une erreur de la requete : la reponse est complete,
    // avec ses compteurs, et c'est son **statut** qui passe a 409 (mesure contre
    // ES 8.15). `conflicts=proceed` vide `failures[]`, donc rend 200.
    let statut = if bilan.failures.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::CONFLICT
    };
    Ok(Json(
        statut,
        parrequete::reponse(&bilan, geste, elapsed_ms(started)),
    ))
}

/// Ce que le corps porte en plus de la requete.
#[derive(Default)]
struct DuCorps {
    max_docs: Option<usize>,
    proceder: Option<bool>,
}

/// Lit le corps : `query`, `max_docs`, `conflicts`, et rien d'autre.
///
/// Ces trois-la s'ecrivent **aussi** dans le corps, et ce n'est pas une
/// curiosite de l'API : le client officiel y met `max_docs` sur les deux routes
/// et `conflicts` sur `_update_by_query`. Les refuser dans le corps rendait
/// `client.delete_by_query(max_docs=1)` inutilisable — un echec que seul le
/// harnais pouvait trouver, puisque la documentation les presente comme des
/// parametres de requete.
fn lire_corps(corps: &Value, geste: Geste, route: &str) -> EsResult<(Option<Value>, DuCorps)> {
    let obj = match corps {
        Value::Null => Map::new(),
        Value::Object(o) => o.clone(),
        _ => {
            return Err(EsError::parsing(format!(
                "le corps de [{route}] doit etre un objet"
            )))
        }
    };

    if geste == Geste::Reindexer && obj.contains_key("script") {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [script] sur [_update_by_query] : c'est un script Painless, \
             hors perimetre. Sans script, la route reindexe les documents depuis leur [_source] \
             — ce qui est le geste utile apres un [PUT /{index}/_mapping] (voir docs/compat.md)",
        ));
    }
    for cle in ["slice", "size", "sort", "_source"] {
        if obj.contains_key(cle) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{cle}] dans le corps de [{route}] {} (voir \
                 docs/compat.md)",
                note_de_corps(cle)
            )));
        }
    }
    super::expect_only(&obj, &["query", "max_docs", "conflicts"], route)?;

    let mut du_corps = DuCorps::default();
    if let Some(v) = obj.get("max_docs") {
        du_corps.max_docs = Some(
            v.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| {
                    EsError::parsing(format!("[{route}] : [max_docs] attend un entier positif"))
                })?,
        );
    }
    if let Some(v) = obj.get("conflicts") {
        du_corps.proceder = Some(match v.as_str() {
            Some("abort") => false,
            Some("proceed") => true,
            _ => return Err(conflits_illisible(v)),
        });
    }

    let requete = match obj.get("query") {
        // `{"query": null}` n'est pas « pas de requete » : c'est une requete
        // qu'on n'a pas su ecrire, et ES la refuse **avant** de se demander si
        // elle manque — meme sur `_update_by_query`, ou l'absence de corps est
        // pourtant licite. Le message est celui de son parseur, mot pour mot.
        Some(Value::Null) => {
            return Err(EsError::parsing("Unknown key for a VALUE_NULL in [query]."))
        }
        None => match geste {
            // Le corps d'un `_update_by_query` est facultatif : sans lui, ES
            // reindexe tout l'index. Une purge, non.
            Geste::Reindexer => None,
            Geste::Supprimer => {
                return Err(EsError::new(
                    StatusCode::BAD_REQUEST,
                    "action_request_validation_exception",
                    "Validation Failed: 1: query is missing;",
                ))
            }
        },
        Some(q) => Some(q.clone()),
    };
    Ok((requete, du_corps))
}

/// Pourquoi une clef de corps est refusee. Une phrase par clef : un refus qui ne
/// dit pas ce qu'il coute est un refus qu'on ne sait pas contourner.
fn note_de_corps(cle: &str) -> &'static str {
    match cle {
        "slice" => ": le decoupage en tranches suppose des taches paralleles, que ferrite n'a pas",
        "size" => ": c'est l'ancien nom de `scroll_size` ; passe-le en parametre de requete",
        "sort" => {
            ": il choisit **quels** documents `max_docs` retient, et ferrite ne sait pas encore \
             l'appliquer ici"
        }
        _ => ": il ne change rien a ce que la commande ecrit, et ferrite ne rend pas de documents",
    }
}

/// `refresh` sur ces deux routes n'accepte que `true` et `false`.
///
/// C'est une exception dans l'API d'ES — `_doc` et `_bulk` acceptent
/// `wait_for` — et elle se mesure : `?refresh=wait_for` rend 400 sur un
/// `_delete_by_query`, avec ce message-la.
fn refresh_strict(p: &mut Params) -> EsResult<bool> {
    match p.opt("refresh") {
        None => Ok(false),
        Some(v) if v.is_empty() || v == "true" => Ok(true),
        Some(v) if v == "false" => Ok(false),
        Some(v) => Err(EsError::illegal_argument(format!(
            "Failed to parse value [{v}] as only [true] or [false] are allowed."
        ))),
    }
}

/// `conflicts` en parametre : `abort` ou `proceed`. `None` = non fourni, et
/// c'est le corps qui tranchera.
fn conflits(p: &mut Params) -> EsResult<Option<bool>> {
    match p.opt("conflicts") {
        None => Ok(None),
        Some(v) if v == "abort" => Ok(Some(false)),
        Some(v) if v == "proceed" => Ok(Some(true)),
        Some(v) => Err(conflits_illisible(&Value::String(v))),
    }
}

/// Le refus d'ES, mot pour mot, quelle que soit la place ou `conflicts` a ete
/// ecrit.
fn conflits_illisible(v: &Value) -> EsError {
    let vu = v
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| v.to_string());
    EsError::illegal_argument(format!(
        "conflicts may only be \"proceed\" or \"abort\" but was [{vu}]"
    ))
}

/// `scroll_size` : la taille d'un lot, dans les memes bornes qu'ES.
fn taille_de_lot(p: &mut Params) -> EsResult<usize> {
    let Some(taille) = p.number("scroll_size")? else {
        return Ok(LOT_PAR_DEFAUT);
    };
    if taille == 0 {
        return Err(EsError::new(
            StatusCode::BAD_REQUEST,
            "action_request_validation_exception",
            "Validation Failed: 1: [size] cannot be [0] in a scroll context;",
        ));
    }
    if taille > LOT_MAX {
        return Err(EsError::illegal_argument(format!(
            "Batch size is too large, size must be less than or equal to: [{LOT_MAX}] but was \
             [{taille}]. Scroll batch sizes cost as much memory as result windows so they are \
             controlled by the [index.max_result_window] index level setting."
        )));
    }
    Ok(taille)
}

/// Les parametres qu'ES tient et que ferrite ne tient pas : chacun par son nom,
/// avec ce qu'il ferait s'il etait applique.
///
/// `slices=1` et `requests_per_second=-1` sont les **valeurs par defaut** d'ES :
/// les recevoir explicitement ne demande rien, et les refuser ferait echouer un
/// client qui se contente de les ecrire.
fn refuser_les_parametres_non_tenus(p: &mut Params, route: &str, geste: Geste) -> EsResult<()> {
    if let Some(v) = p.opt("wait_for_completion") {
        if v != "true" && !v.is_empty() {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [wait_for_completion=false] sur [{route}] : il rend une \
                 **tache** que le client suit ensuite sur [_tasks], et ferrite n'a pas d'API de \
                 taches. La commande est synchrone (voir docs/compat.md)"
            )));
        }
    }
    if let Some(v) = p.opt("slices") {
        if v != "1" {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [slices={v}] sur [{route}] : il decoupe le travail en \
                 taches paralleles, et change la reponse (une section [slices]). ferrite est \
                 mono-shard et synchrone (voir docs/compat.md)"
            )));
        }
    }
    if let Some(v) = p.opt("requests_per_second") {
        if v != "-1" && v != "-1.0" {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [requests_per_second={v}] sur [{route}] : il regule le \
                 debit et remplit [throttled_millis], que ferrite rendrait a zero — une valeur \
                 plausible pour une regulation qui n'a pas eu lieu (voir docs/compat.md)"
            )));
        }
    }
    for param in [
        "q",
        "df",
        "default_operator",
        "analyzer",
        "analyze_wildcard",
        "lenient",
    ] {
        if p.opt(param).is_some() {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{param}] sur [{route}] : la recherche par chaine [q] \
                 (query_string) n'est pas implementee ; utilise le Query DSL dans le corps"
            )));
        }
    }
    if p.opt("terminate_after").is_some() {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [terminate_after] sur [{route}] : il arrete la recherche a N \
             documents **par shard** sans arreter l'ecriture, et rendrait donc un [total] qui ne \
             dit plus combien de documents la commande a traites (voir docs/compat.md)"
        )));
    }
    // Les parametres de **recherche** qu'ES accepte ici parce que la commande
    // ouvre un scroll : ils ne changent rien a ce qu'elle ecrit, mais ES les
    // connait. Les laisser tomber dans « unrecognized parameter » les ferait
    // passer pour des fautes de frappe.
    for param in [
        "scroll",
        "search_timeout",
        "search_type",
        "request_cache",
        "stats",
        "version",
        "sort",
    ] {
        if p.opt(param).is_some() {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{param}] sur [{route}] : il porte sur la **recherche** \
                 interne de la commande, que ferrite n'expose pas — elle ne rend aucun document \
                 (voir docs/compat.md)"
            )));
        }
    }
    if geste == Geste::Reindexer && p.opt("pipeline").is_some() {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [pipeline] sur [_update_by_query] : les pipelines \
             d'ingestion sont hors perimetre (voir docs/compat.md)",
        ));
    }
    Ok(())
}
