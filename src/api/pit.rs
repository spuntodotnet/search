//! `POST /{index}/_pit` et `DELETE /_pit` — ouvrir et fermer une vue figee.
//!
//! Deux routes, et la seconde compte autant que la premiere : un contexte
//! retient un instantane de l'index, donc des fichiers que tantivy ne peut pas
//! recycler. Un client qui n'appelle jamais `DELETE /_pit` fait grossir le
//! disque du serveur jusqu'a l'expiration du `keep_alive` — c'est aussi vrai
//! chez ES, et c'est pour ca qu'il rend `keep_alive` **obligatoire** a
//! l'ouverture alors qu'il est facultatif partout ailleurs.
//!
//! Voir [`crate::pit`] pour ce que le contexte retient, et pourquoi ce n'est
//! pas la meme chose qu'un `scroll`.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::Uri;
use serde_json::{json, Value};

use super::{expect_only, parse_body, selection_options, Json, Params, SharedState};
use crate::error::{EsError, EsResult};
use crate::scroll;
use crate::selection::resoudre;

/// `POST /{index}/_pit?keep_alive=1m`
pub async fn ouvrir(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    // `preference` et `routing` choisissent un shard : ferrite n'en a qu'un.
    p.opt("preference");
    p.opt("routing");
    let keep_alive = p.opt("keep_alive");
    p.done()?;

    // Le `keep_alive` est **obligatoire**, et c'est le seul endroit de l'API ou
    // il l'est : sans lui, un contexte oublie retiendrait un instantane pour
    // toujours. La phrase est celle d'ES, avec son numerotage de validation.
    let Some(keep_alive) = keep_alive else {
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "action_request_validation_exception",
            "Validation Failed: 1: [keep_alive] is not specified;",
        ));
    };
    let keep_alive = scroll::duree(&keep_alive, "keep_alive")?;

    let corps = parse_body(&body)?;
    match &corps {
        Value::Null => {}
        Value::Object(o) => {
            // `index_filter` restreint la vue aux shards qui peuvent porter des
            // documents correspondants. C'est une **optimisation** chez ES
            // (« can match »), pas un filtre de resultats : l'accepter sans
            // rien filtrer serait juste, l'accepter en filtrant serait faux.
            // ferrite n'ayant qu'un shard par index, elle n'a rien a elaguer —
            // mais elle se refuse en la nommant plutot que de laisser croire
            // qu'un filtre a ete pose.
            expect_only(o, &[], "_pit")?;
        }
        _ => return Err(EsError::parsing("le corps de [_pit] doit etre un objet")),
    }

    let indices = resoudre(&st.catalog, &index, &opts)?;
    let cibles: Vec<crate::pit::CibleFigee> = indices
        .iter()
        .map(|i| {
            let gen = i.current();
            let searcher = gen.searcher();
            crate::pit::CibleFigee {
                nom: i.name.clone(),
                uuid: i.uuid.clone(),
                gen,
                searcher,
            }
        })
        .collect();

    let id = st.pits.ouvrir(
        crate::pit::Contexte {
            cibles,
            expire: std::time::Instant::now(),
        },
        keep_alive,
    )?;
    Ok(Json::ok(json!({"id": id})))
}

/// `POST /_pit` — sans index, ES refuse. La route existe quand meme : sans
/// elle, le refus serait un 404 de routage, donc « cette API n'existe pas »
/// plutot que « il manque l'index ».
pub async fn ouvrir_sans_index(
    State(_st): State<SharedState>,
    _uri: Uri,
    _body: Bytes,
) -> EsResult<Json> {
    Err(EsError::new(
        axum::http::StatusCode::BAD_REQUEST,
        "action_request_validation_exception",
        "Validation Failed: 1: [index] is not specified;",
    ))
}

/// `DELETE /_pit` — rendre la vue avant son expiration.
///
/// La reponse n'est pas une enveloppe d'erreur, meme sur le 404 : ES rend
/// `{"succeeded": false, "num_freed": 0}` avec le statut 404 quand
/// l'identifiant ne designe plus rien (mesure). C'est le cas normal d'un client
/// qui nettoie deux fois, et un corps d'erreur y ferait lever ses exceptions
/// typees pour rien.
pub async fn fermer(State(st): State<SharedState>, uri: Uri, body: Bytes) -> EsResult<Json> {
    let p = Params::parse(&uri);
    p.done()?;

    let corps = parse_body(&body)?;
    let Value::Object(o) = &corps else {
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "parse_exception",
            "request body or source parameter is required",
        ));
    };
    expect_only(o, &["id"], "DELETE /_pit")?;
    let id = match o.get("id") {
        Some(Value::String(s)) => s.clone(),
        None => {
            return Err(EsError::illegal_argument(
                "[id] est obligatoire sur [DELETE /_pit]",
            ))
        }
        // ES rend ici sa propre phrase, coquille comprise : le corps ne lit
        // **qu'** un identifiant, pas une liste (contrairement a
        // `clear_scroll`, qui en accepte plusieurs).
        Some(_) => {
            return Err(EsError::illegal_argument(
                "the request must contain only [id field",
            ))
        }
    };

    let libere = st.pits.fermer(&id);
    let corps = json!({"succeeded": libere, "num_freed": usize::from(libere)});
    if libere {
        Ok(Json::ok(corps))
    } else {
        Ok(Json(axum::http::StatusCode::NOT_FOUND, corps))
    }
}
