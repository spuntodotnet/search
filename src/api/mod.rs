//! La couche HTTP : routage, parametres, format de reponse.
//!
//! Aucune logique de moteur ici — elle vit dans [`crate::engine`],
//! [`crate::dsl`] et [`crate::search`].

pub mod aliases;
pub mod cluster;
pub mod docs;
pub mod indices;
pub mod search;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::Router;
use serde_json::{json, Value};

use crate::engine::Catalog;
use crate::error::{EsError, EsResult};

pub struct AppState {
    pub catalog: Arc<Catalog>,
    pub started: Instant,
    /// Les contextes de `scroll` ouverts (voir [`crate::scroll`]).
    pub scrolls: crate::scroll::RegistrePartage,
}

pub type SharedState = Arc<AppState>;

/// Un corps de reponse JSON au format ES.
pub struct Json(pub StatusCode, pub Value);

impl Json {
    pub fn ok(value: Value) -> Self {
        Self(StatusCode::OK, value)
    }
}

impl IntoResponse for Json {
    fn into_response(self) -> Response {
        let body = serde_json::to_vec(&self.1).unwrap_or_else(|_| b"{}".to_vec());
        (self.0, [(header::CONTENT_TYPE, "application/json")], body).into_response()
    }
}

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/", get(cluster::root))
        .route("/_cluster/health", get(cluster::health))
        .route(
            "/_cluster/settings",
            get(cluster::settings_get).put(cluster::settings_put),
        )
        .route("/_cluster/health/{index}", get(cluster::health_index))
        .route("/_cat/health", get(cluster::cat_health))
        .route("/_cat/indices", get(cluster::cat_indices))
        .route("/_cat/indices/{index}", get(cluster::cat_indices_one))
        .route("/_nodes", get(cluster::nodes))
        .route("/_nodes/{spec}", get(cluster::nodes_spec))
        .route("/_bulk", post(docs::bulk).put(docs::bulk))
        .route("/_search", post(search::search_all).get(search::search_all))
        // `scroll` : la pagination par contexte fige. C'est ce dont se sert
        // `helpers.scan` du client officiel, donc tout export d'index.
        .route(
            "/_search/scroll",
            post(search::scroll_suivant)
                .get(search::scroll_suivant)
                .delete(search::scroll_effacer),
        )
        .route(
            "/_search/scroll/{scroll_id}",
            post(search::scroll_suivant_par_url)
                .get(search::scroll_suivant_par_url)
                .delete(search::scroll_effacer_par_url),
        )
        .route(
            "/{index}",
            put(indices::create)
                .post(indices::create)
                .delete(indices::delete)
                .head(indices::exists)
                .get(indices::get_index),
        )
        .route("/_mapping", get(indices::get_mapping_all))
        .route(
            "/_refresh",
            post(indices::refresh_all).get(indices::refresh_all),
        )
        .route(
            "/{index}/_mapping",
            get(indices::get_mapping).put(indices::put_mapping),
        )
        .route(
            "/_analyze",
            post(|s, u, b| indices::analyze(s, None, u, b))
                .get(|s, u, b| indices::analyze(s, None, u, b)),
        )
        .route(
            "/{index}/_analyze",
            post(|s, p, u, b| indices::analyze(s, Some(p), u, b))
                .get(|s, p, u, b| indices::analyze(s, Some(p), u, b)),
        )
        .route(
            "/{index}/_refresh",
            post(indices::refresh).get(indices::refresh),
        )
        .route("/{index}/_search", post(search::search).get(search::search))
        .route(
            "/{index}/_bulk",
            post(docs::bulk_index).put(docs::bulk_index),
        )
        .route("/{index}/_doc", post(docs::index_auto_id))
        .route(
            "/{index}/_doc/{id}",
            put(docs::index_doc)
                .post(docs::index_doc)
                .get(docs::get_doc)
                .head(docs::head_doc)
                .delete(docs::delete_doc),
        )
        .route(
            "/{index}/_create/{id}",
            put(docs::create_doc).post(docs::create_doc),
        )
        .route("/_count", get(search::count_all).post(search::count_all))
        .route("/{index}/_count", get(search::count).post(search::count))
        .route("/{index}/_update/{id}", post(docs::update_doc))
        .route(
            "/_mget",
            post(|s, u, b| docs::mget(s, None, u, b)).get(|s, u, b| docs::mget(s, None, u, b)),
        )
        .route(
            "/{index}/_mget",
            post(|s, p, u, b| docs::mget(s, Some(p), u, b))
                .get(|s, p, u, b| docs::mget(s, Some(p), u, b)),
        )
        // Des routes qu'ES expose et que ferrite n'implemente pas : mieux vaut
        // le dire que de laisser croire a une faute de frappe.
        .route(
            "/{index}/_update_by_query",
            post(unsupported_route).get(unsupported_route),
        )
        .route(
            "/{index}/_delete_by_query",
            post(unsupported_route).get(unsupported_route),
        )
        .route("/_reindex", post(unsupported_route))
        // Le seul reglage d'index que ferrite exploite se pose a la creation :
        // le dire vaut mieux qu'un « no handler » qui laisserait croire a une
        // faute d'URL.
        .route(
            "/{index}/_settings",
            put(reglages_non_modifiables)
                .post(reglages_non_modifiables)
                .get(indices::get_settings),
        )
        .route(
            "/{index}/_msearch",
            post(unsupported_route).get(unsupported_route),
        )
        .route("/_msearch", post(unsupported_route).get(unsupported_route))
        // Les alias : un nom stable au-dessus d'index qui changent.
        .route("/_aliases", post(aliases::actions))
        .route("/_alias", get(aliases::lister_tout))
        .route(
            "/_alias/{nom}",
            get(aliases::lister_par_alias).head(aliases::exister),
        )
        .route("/{index}/_alias", get(aliases::lister_par_index))
        .route(
            "/{index}/_alias/{nom}",
            put(aliases::poser)
                .post(aliases::poser)
                .delete(aliases::retirer)
                .get(aliases::lister)
                .head(aliases::exister_dans),
        )
        .route(
            "/{index}/_aliases/{nom}",
            put(aliases::poser)
                .post(aliases::poser)
                .delete(aliases::retirer),
        )
        .fallback(no_handler)
        .layer(axum::middleware::from_fn(elastic_headers))
        .with_state(state)
}

/// Une route qu'ES expose mais que ferrite n'implemente pas : erreur explicite,
/// pas un 404 muet.
async fn unsupported_route(uri: Uri) -> EsError {
    EsError::unsupported(format!(
        "ferrite n'implemente pas la route [{}] dans cette version (voir docs/compat.md)",
        uri.path()
    ))
}

/// `PUT /{index}/_settings` : ferrite n'a pas de reglage modifiable a chaud.
///
/// Le seul reglage qu'il exploite — `index.query.parse.allow_unmapped_fields` —
/// se pose a la creation de l'index. Le changer ensuite demanderait de
/// reconstruire la generation courante, et un client qui croit l'avoir change
/// alors qu'il n'en est rien chercherait longtemps.
async fn reglages_non_modifiables() -> EsError {
    EsError::unsupported(
        "ferrite ne supporte pas [PUT /{index}/_settings] : le seul reglage qu'il exploite, \
         [index.query.parse.allow_unmapped_fields], se pose dans [settings] a la creation de \
         l'index (voir docs/compat.md)",
    )
}

/// Le 400 d'ES pour une route inconnue, au format exact (une chaine, pas un
/// objet).
async fn no_handler(req: Request) -> Response {
    let body = json!({
        "error": format!(
            "no handler found for uri [{}] and method [{}]",
            req.uri(), req.method()
        ),
    });
    (
        StatusCode::BAD_REQUEST,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_vec(&body).unwrap(),
    )
        .into_response()
}

/// Pose `X-elastic-product: Elasticsearch` sur **toutes** les reponses.
///
/// Sans ce header, les clients 8.x refusent de parler au serveur avec une
/// erreur qui ne pointe pas vers la vraie cause. C'est la premiere chose que
/// fait ferrite sur le chemin de reponse, avant toute autre consideration.
async fn elastic_headers(req: Request, next: Next) -> Response {
    let pretty = req
        .uri()
        .query()
        .map(|q| {
            q.split('&')
                .any(|kv| kv == "pretty" || kv.starts_with("pretty="))
        })
        .unwrap_or(false);

    let mut resp = next.run(req).await;
    resp.headers_mut().insert(
        "X-elastic-product",
        HeaderValue::from_static("Elasticsearch"),
    );

    if !pretty {
        return resp;
    }
    let is_json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !is_json {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, usize::MAX).await else {
        return (parts, Body::empty()).into_response();
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) => {
            let out = serde_json::to_vec_pretty(&v).unwrap_or_else(|_| bytes.to_vec());
            parts.headers.remove(header::CONTENT_LENGTH);
            (parts, Body::from(out)).into_response()
        }
        Err(_) => (parts, Body::from(bytes)).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Parametres de requete
// ---------------------------------------------------------------------------

/// Les parametres purement cosmetiques d'ES : acceptes partout, sans effet sur
/// la semantique de la reponse.
const COSMETIC: &[&str] = &["pretty", "human", "error_trace"];

/// Les parametres de query string, consommes un par un.
///
/// [`Params::done`] refuse tout parametre restant : un client qui envoie
/// `?routing=x` doit l'apprendre, pas voir sa demande ignoree.
pub struct Params {
    map: HashMap<String, String>,
    path: String,
}

impl Params {
    pub fn parse(uri: &Uri) -> Self {
        let mut map = HashMap::new();
        if let Some(q) = uri.query() {
            for pair in q.split('&') {
                if pair.is_empty() {
                    continue;
                }
                let (k, v) = match pair.split_once('=') {
                    Some((k, v)) => (k, v),
                    None => (pair, ""),
                };
                let k = percent_decode(k);
                if COSMETIC.contains(&k.as_str()) {
                    continue;
                }
                map.insert(k, percent_decode(v));
            }
        }
        Self {
            map,
            path: uri.path().to_string(),
        }
    }

    pub fn opt(&mut self, name: &str) -> Option<String> {
        self.map.remove(name)
    }

    pub fn list(&mut self, name: &str) -> Option<Vec<String>> {
        self.map.remove(name).map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
    }

    pub fn flag(&mut self, name: &str, default: bool) -> EsResult<bool> {
        match self.map.remove(name) {
            None => Ok(default),
            Some(v) => parse_bool(name, &v),
        }
    }

    pub fn number(&mut self, name: &str) -> EsResult<Option<usize>> {
        match self.map.remove(name) {
            None => Ok(None),
            Some(v) => v.trim().parse::<usize>().map(Some).map_err(|_| {
                EsError::illegal_argument(format!(
                    "Failed to parse int parameter [{name}] with value [{v}]"
                ))
            }),
        }
    }

    /// `refresh` : `true` / `false` / `wait_for` (equivalents ici — ferrite a un
    /// seul shard et commite de facon synchrone).
    pub fn refresh(&mut self) -> EsResult<bool> {
        match self.map.remove("refresh") {
            None => Ok(false),
            Some(v) if v.is_empty() => Ok(true),
            Some(v) if v == "wait_for" => Ok(true),
            Some(v) => parse_bool("refresh", &v),
        }
    }

    pub fn done(self) -> EsResult<()> {
        if let Some(name) = self.map.keys().min() {
            return Err(EsError::illegal_argument(format!(
                "request [{}] contains unrecognized parameter: [{name}]",
                self.path
            )));
        }
        Ok(())
    }
}

fn parse_bool(name: &str, v: &str) -> EsResult<bool> {
    match v {
        "true" | "" => Ok(true),
        "false" => Ok(false),
        other => Err(EsError::illegal_argument(format!(
            "Failed to parse value [{other}] as only [true] or [false] are allowed for parameter \
             [{name}]"
        ))),
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse un corps JSON sans exiger `Content-Type: application/json`.
///
/// Les clients 8.x envoient `application/vnd.elasticsearch+json;
/// compatible-with=8` : un extracteur JSON standard les rejetterait.
pub fn parse_body(body: &Bytes) -> EsResult<Value> {
    if body.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(body).map_err(|e| EsError::parsing(format!("corps JSON invalide : {e}")))
}

/// Refuse toute cle inconnue dans un corps de requete.
pub fn expect_only(
    obj: &serde_json::Map<String, Value>,
    allowed: &[&str],
    what: &str,
) -> EsResult<()> {
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{key}] dans [{what}] ; cles acceptees : {allowed:?}"
            )));
        }
    }
    Ok(())
}

/// Les tolerances de resolution d'une expression d'index, lues dans la query
/// string sous les noms d'Elasticsearch.
///
/// `expand_wildcards` merite un mot : ferrite n'a ni index fermes ni index
/// caches, donc `open`, `hidden` et `all` designent tous la meme chose. Un
/// client qui demande **uniquement** `closed` ne vise donc aucun index, et
/// c'est ce qu'on lui rend — plutot que de lui donner les index ouverts, ce qui
/// serait un resultat faux. `none` (« ne developpe pas les motifs ») est refuse
/// : le traiter comme un nom litteral rendrait `index_not_found` sur un nom que
/// le client n'a jamais ecrit.
pub fn selection_options(p: &mut Params) -> EsResult<crate::selection::Options> {
    let defaut = crate::selection::Options::default();
    let ignore_unavailable = p.flag("ignore_unavailable", defaut.ignore_unavailable)?;
    let allow_no_indices = p.flag("allow_no_indices", defaut.allow_no_indices)?;
    let expansion = match p.list("expand_wildcards") {
        None => true,
        Some(valeurs) => {
            for v in &valeurs {
                if !matches!(v.as_str(), "open" | "closed" | "hidden" | "all" | "none") {
                    return Err(EsError::illegal_argument(format!(
                        "No enum constant IndicesOptions.WildcardStates.{}",
                        v.to_uppercase()
                    )));
                }
                if v == "none" {
                    return Err(EsError::unsupported(
                        "ferrite ne supporte pas [expand_wildcards=none] : un motif non developpe \
                         serait cherche comme un nom d'index litteral, et rendrait une erreur sur \
                         un nom que personne n'a ecrit",
                    ));
                }
            }
            valeurs.iter().any(|v| v == "open" || v == "all")
        }
    };
    Ok(crate::selection::Options {
        ignore_unavailable,
        allow_no_indices,
        expansion,
    })
}

/// `_shards` d'une reponse d'ecriture : un shard, zero replique, toujours vert.
pub fn shards_ok() -> Value {
    json!({"total": 1, "successful": 1, "failed": 0})
}

pub fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}
