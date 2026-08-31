//! La couche HTTP : routage, parametres, format de reponse.
//!
//! Aucune logique de moteur ici — elle vit dans [`crate::engine`],
//! [`crate::dsl`] et [`crate::search`].

pub mod aliases;
pub mod cluster;
pub mod docs;
pub mod explain;
pub mod fieldcaps;
pub mod indices;
pub mod parrequete;
pub mod search;
pub mod stats;
pub mod templates;
pub mod validate;

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

/// La taille maximale d'un corps de requete — le `http.max_content_length` d'ES,
/// dont le defaut est 100 Mo.
///
/// C'est une valeur **annoncee** : `GET /_nodes` la publie, et les clients
/// officiels s'en servent pour dimensionner leurs lots (`helpers.bulk` de
/// `elasticsearch-py` decoupe a 100 Mo par defaut). Tant que la couche HTTP
/// gardait le defaut d'axum — 2 Mo — ferrite annoncait donc cinquante fois ce
/// qu'il acceptait, et refusait en `413 text/plain` un `_bulk` de 5 000
/// documents, la taille de lot par defaut des tracks Rally. La constante est
/// posee ici et lue par [`cluster::nodes`] : les deux chiffres ne peuvent plus
/// diverger.
pub const MAX_CONTENT_LENGTH: usize = 104_857_600;

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

/// Le routeur complet.
///
/// Les routes vivent dans un routeur **interne**, monte comme service de repli
/// d'un routeur vide. C'est ce qui place [`elastic_headers`] a l'exterieur du
/// routage lui-meme, et non autour de chaque poignee : le header
/// `X-elastic-product` et le corps d'un 405 sont alors poses sur *toutes* les
/// reponses, y compris celles qu'axum fabrique tout seul. Une middleware posee
/// a l'interieur ne voit pas le `Allow` qu'axum ajoute apres coup, donc ne peut
/// pas dire quelles methodes la route accepte — ce qui est exactement
/// l'information qu'un 405 doit porter.
pub fn router(state: SharedState) -> Router {
    Router::new()
        .fallback_service(routes(state))
        // La limite de taille porte sur ce qui **arrive**, donc sur le corps
        // compresse — comme le `http.max_content_length` d'ES, que Netty
        // applique avant de decompresser.
        .layer(axum::middleware::from_fn(decompresser))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_CONTENT_LENGTH))
        .layer(axum::middleware::from_fn(elastic_headers))
}

/// `Content-Encoding: gzip` / `deflate` — ce que pose tout client officiel a qui
/// on demande de compresser (`http_compress=True` en Python, `compression: true`
/// en JavaScript, `CompressRequestBody` en Go).
///
/// Sans ca, ferrite recevait les octets compresses et les lisait comme du JSON :
/// un client qui active la compression — ce que fait le client JavaScript **par
/// defaut** vers Elastic Cloud — ne pouvait plus rien ecrire, sur un message
/// (« le corps de [_bulk] doit etre de l'UTF-8 ») qui ne nommait pas la cause.
///
/// Les deux encodages sont ceux qu'un vrai ES 8.15 decompresse, mesure : `gzip`
/// et `deflate` (enveloppe zlib) passent, `br` et un nom inconnu sont laisses
/// tels quels — Netty n'a pas de decodeur pour eux et transmet le corps sans
/// rien dire. Un flux illisible, lui, est refuse en nommant l'encodage plutot
/// que rendu vide : le message d'ES sur ce cas (« request body is required »)
/// designe la mauvaise cause.
async fn decompresser(req: Request, next: Next) -> Response {
    let encodage = req
        .headers()
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_ascii_lowercase());
    let Some(encodage) = encodage else {
        return next.run(req).await;
    };
    if !matches!(encodage.as_str(), "gzip" | "x-gzip" | "deflate") {
        return next.run(req).await;
    }

    let (mut parts, body) = req.into_parts();
    let compresse = match axum::body::to_bytes(body, MAX_CONTENT_LENGTH).await {
        Ok(octets) => octets,
        Err(e) => {
            return EsError::parsing(format!("corps illisible : {e}")).into_response();
        }
    };
    let mut clair = Vec::new();
    let lu = if encodage == "deflate" {
        std::io::Read::read_to_end(
            &mut flate2::read::ZlibDecoder::new(&compresse[..]),
            &mut clair,
        )
    } else {
        std::io::Read::read_to_end(
            &mut flate2::read::GzDecoder::new(&compresse[..]),
            &mut clair,
        )
    };
    if let Err(e) = lu {
        return EsError::parsing(format!(
            "corps annonce en [Content-Encoding: {encodage}] mais illisible : {e}"
        ))
        .into_response();
    }
    // `Content-Length` decrivait le corps compresse : le laisser ferait mentir
    // tout ce qui le relit en aval.
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.remove(header::CONTENT_ENCODING);
    next.run(Request::from_parts(parts, Body::from(clair)))
        .await
}

fn routes(state: SharedState) -> Router {
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
        // `_explain` : pourquoi **ce** document, avec ce score.
        .route(
            "/{index}/_explain/{id}",
            post(explain::explain).get(explain::explain),
        )
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
        // Modifier ou purger **par requete** : ce qu'un script de maintenance
        // fait tous les jours. Les deux routes sont en `POST` seul, comme chez
        // ES — un `GET` y rend 405, pas 400.
        .route("/{index}/_update_by_query", post(parrequete::reindexer))
        .route("/{index}/_delete_by_query", post(parrequete::supprimer))
        // Des routes qu'ES expose et que ferrite n'implemente pas : mieux vaut
        // le dire que de laisser croire a une faute de frappe. Les
        // `_rethrottle` en font partie : ils changent le debit d'une **tache**
        // en cours, et une commande par requete de ferrite est finie quand elle
        // repond.
        .route("/_reindex", post(unsupported_route))
        .route(
            "/_delete_by_query/{task_id}/_rethrottle",
            post(unsupported_route),
        )
        .route(
            "/_update_by_query/{task_id}/_rethrottle",
            post(unsupported_route),
        )
        .route(
            "/{index}/_settings",
            put(indices::put_settings)
                .post(indices::put_settings)
                .get(indices::get_settings),
        )
        // `/_settings` sans index vaut `_all`, et `{nom}` filtre par nom de
        // reglage : deux formes qu'un `no handler found` rendait indechiffrables.
        .route(
            "/_settings",
            get(|s, u| indices::get_settings_all(s, None, u))
                .put(indices::put_settings_all)
                .post(indices::put_settings_all),
        )
        .route(
            "/_settings/{nom}",
            get(|s, n, u| indices::get_settings_all(s, Some(n), u)),
        )
        .route("/{index}/_settings/{nom}", get(indices::get_settings_nomme))
        // `_field_caps`, `_validate/query` et `_stats` : trois routes sans
        // difficulte de moteur, dont l'absence bloquait des outils entiers.
        .route(
            "/_field_caps",
            get(fieldcaps::field_caps_all).post(fieldcaps::field_caps_all),
        )
        .route(
            "/{index}/_field_caps",
            get(fieldcaps::field_caps).post(fieldcaps::field_caps),
        )
        .route(
            "/_validate/query",
            get(validate::validate_all).post(validate::validate_all),
        )
        .route(
            "/{index}/_validate/query",
            get(validate::validate).post(validate::validate),
        )
        .route("/_stats", get(|s, u| stats::stats_all(s, None, u)))
        .route(
            "/_stats/{metric}",
            get(|s, m, u| stats::stats_all(s, Some(m), u)),
        )
        .route("/{index}/_stats", get(stats::stats_index))
        .route("/{index}/_stats/{metric}", get(stats::stats_index_metric))
        // Les templates : un mapping applique a un index qui n'existe pas
        // encore. Les deux familles, parce qu'un script d'init venu de la 7.x
        // pose un `_template` et que ce code-la ne doit pas changer.
        .route(
            "/_index_template",
            get(|s, u| templates::lire_composables(s, None, u)),
        )
        .route(
            "/_index_template/{nom}",
            put(templates::poser_composable)
                .post(templates::poser_composable)
                .get(|s, n, u| templates::lire_composables(s, Some(n), u))
                .head(templates::existe_composable)
                .delete(templates::supprimer_composable),
        )
        .route(
            "/_template",
            get(|s, u| templates::lire_anciens(s, None, u)),
        )
        .route(
            "/_template/{nom}",
            put(templates::poser_ancien)
                .post(templates::poser_ancien)
                .get(|s, n, u| templates::lire_anciens(s, Some(n), u))
                .head(templates::existe_ancien)
                .delete(templates::supprimer_ancien),
        )
        .route(
            "/{index}/_msearch",
            post(unsupported_route).get(unsupported_route),
        )
        .route("/_msearch", post(unsupported_route).get(unsupported_route))
        // Les alias : un nom stable au-dessus d'index qui changent.
        .route("/_aliases", post(aliases::actions))
        // Les sept URL de `put_alias` : le nom de l'alias et celui de l'index
        // peuvent venir du corps plutot que du chemin. Elles sont posterieures
        // a la suite de conformance d'Elastic (figee en 7.10.2), qui ne pouvait
        // donc pas les exercer — c'est celle d'OpenSearch qui les a sorties.
        .route(
            "/_alias",
            get(aliases::lister_tout).put(aliases::poser_par_le_corps),
        )
        .route(
            "/_alias/{nom}",
            get(aliases::lister_par_alias)
                .head(aliases::exister)
                .put(aliases::poser_sans_index)
                .post(aliases::poser_sans_index),
        )
        .route(
            "/_aliases/{nom}",
            put(aliases::poser_sans_index).post(aliases::poser_sans_index),
        )
        .route(
            "/{index}/_alias",
            get(aliases::lister_par_index).put(aliases::poser_sans_nom),
        )
        .route("/{index}/_aliases", put(aliases::poser_sans_nom))
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

/// Le 405 d'ES quand la route existe mais pas pour cette methode.
///
/// axum le rend **vide** ; ES rend un corps, et il est utile : il dit quelles
/// methodes la route accepte. `POST /{index}/_delete_by_query` existe,
/// `GET /{index}/_delete_by_query` non, et un client qui recoit 405 sans corps
/// n'a aucun moyen de savoir laquelle des deux il a manquee. Le header `Allow`
/// est celui qu'axum a deja pose.
fn methode_interdite(chemin: &str, methode: &str, resp: &Response) -> Response {
    let permises = resp
        .headers()
        .get(header::ALLOW)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let body = json!({
        "error": format!(
            "Incorrect HTTP method for uri [{chemin}] and method [{methode}], allowed: [{permises}]"
        ),
        "status": 405,
    });
    let mut out = (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_vec(&body).unwrap(),
    )
        .into_response();
    if let Some(allow) = resp.headers().get(header::ALLOW) {
        out.headers_mut().insert(header::ALLOW, allow.clone());
    }
    out
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
    let (chemin, methode) = (req.uri().to_string(), req.method().to_string());

    let mut resp = next.run(req).await;
    if resp.status() == StatusCode::METHOD_NOT_ALLOWED {
        resp = methode_interdite(&chemin, &methode, &resp);
    }
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

    /// Un booleen **facultatif** : `None` dit qu'il n'a pas ete pose du tout,
    /// ce que `flag` ne distingue pas d'un `false` explicite. `explain` en a
    /// besoin : le parametre l'emporte sur le corps, mais seulement s'il est la.
    pub fn bool_opt(&mut self, name: &str) -> EsResult<Option<bool>> {
        match self.map.remove(name) {
            None => Ok(None),
            Some(v) => parse_bool(name, &v).map(Some),
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

/// Refuse les parametres qui changent la **forme** d'une reponse de reglages
/// et que ferrite n'applique pas.
///
/// `include_defaults` ajoute une section `defaults` de plusieurs dizaines de
/// reglages qu'ES a et que ferrite n'a pas : l'accepter puis l'ignorer rend une
/// reponse que personne n'a demandee, sans le dire — c'est exactement l'echec
/// silencieux que ce projet refuse. `flat_settings`, lui, n'est qu'une
/// reecriture des cles : il est **applique** la ou ferrite rend des reglages
/// (voir [`crate::reglages::aplatir_reponse`]), et refuse ailleurs.
pub fn refuser_reglages_non_supportes(p: &mut Params, route: &str) -> EsResult<()> {
    for param in ["flat_settings", "include_defaults"] {
        if p.opt(param).is_some() {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{param}] sur [{route}] : il changerait la forme de la \
                 reponse, et ferrite la rendrait inchangee (voir docs/compat.md)"
            )));
        }
    }
    Ok(())
}

/// Le seul des deux qui reste refuse la ou `flat_settings` est applique.
pub fn refuser_include_defaults(p: &mut Params, route: &str) -> EsResult<()> {
    if p.opt("include_defaults").is_some() {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [include_defaults] sur [{route}] : il ajoute une section \
             [defaults] avec les dizaines de reglages qu'ES a et que ferrite n'a pas (voir \
             docs/compat.md)"
        )));
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
