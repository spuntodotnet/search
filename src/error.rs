//! Le format d'erreur d'Elasticsearch.
//!
//! Regle du projet : jamais d'echec silencieux. Toute clause de DSL, tout type
//! de champ, toute route non supportee produit une [`EsError`] explicite plutot
//! qu'un resultat partiel presente comme complet.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

pub type EsResult<T> = Result<T, EsError>;

/// Type d'erreur specifique a ferrite, utilise quand Elasticsearch sait faire
/// quelque chose que ferrite ne sait pas (encore) faire.
///
/// Volontairement distinct des types d'ES : un client qui le voit sait que ce
/// n'est pas une erreur de sa requete mais une limite du serveur.
pub const UNSUPPORTED: &str = "not_implemented_in_ferrite_exception";

#[derive(Debug, Clone)]
pub struct EsError {
    pub status: StatusCode,
    pub ty: String,
    pub reason: String,
    /// Champs supplementaires poses a cote de `type`/`reason`, comme le fait ES
    /// (`index`, `resource.type`, ...).
    pub extra: Vec<(String, Value)>,
}

impl EsError {
    pub fn new(status: StatusCode, ty: &str, reason: impl Into<String>) -> Self {
        Self {
            status,
            ty: ty.to_string(),
            reason: reason.into(),
            extra: Vec::new(),
        }
    }

    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.extra.push((key.to_string(), value));
        self
    }

    /// Requete syntaxiquement ou semantiquement invalide.
    pub fn illegal_argument(reason: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "illegal_argument_exception",
            reason,
        )
    }

    /// Corps de requete impossible a interpreter (JSON casse, clause inconnue).
    pub fn parsing(reason: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "parsing_exception", reason)
    }

    /// Document refuse par le mapping.
    pub fn mapper_parsing(reason: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "document_parsing_exception",
            reason,
        )
    }

    /// Champ absent du mapping explicite. ferrite ne fait pas de mapping
    /// dynamique : c'est exactement la semantique de `dynamic: strict` d'ES.
    pub fn strict_mapping(index: &str, field: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "strict_dynamic_mapping_exception",
            format!(
                "mapping set to strict, dynamic introduction of [{field}] within [_doc] is not \
                 allowed (ferrite ne supporte pas le mapping dynamique : declare le champ dans le \
                 mapping de l'index [{index}])"
            ),
        )
    }

    /// Fonctionnalite d'Elasticsearch que ferrite n'implemente pas.
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, UNSUPPORTED, reason)
    }

    pub fn index_not_found(index: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "index_not_found_exception",
            format!("no such index [{index}]"),
        )
        .with("resource.type", json!("index_or_alias"))
        .with("resource.id", json!(index))
        .with("index_uuid", json!("_na_"))
        .with("index", json!(index))
    }

    pub fn index_already_exists(index: &str, uuid: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "resource_already_exists_exception",
            format!("index [{index}/{uuid}] already exists"),
        )
        .with("index_uuid", json!(uuid))
        .with("index", json!(index))
    }

    pub fn version_conflict(index: &str, id: &str) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "version_conflict_engine_exception",
            format!("[{id}]: version conflict, document already exists (current version [1])"),
        )
        .with("index", json!(index))
        .with("shard", json!("0"))
    }

    pub fn internal(reason: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_server_error",
            reason,
        )
    }

    /// Le detail d'erreur seul, tel qu'ES le met dans `root_cause[]` et dans les
    /// items de reponse `_bulk`.
    pub fn cause(&self) -> Value {
        let mut o = serde_json::Map::new();
        o.insert("type".into(), json!(self.ty));
        o.insert("reason".into(), json!(self.reason));
        for (k, v) in &self.extra {
            o.insert(k.clone(), v.clone());
        }
        Value::Object(o)
    }

    /// Le corps complet : `{"error": {...}, "status": n}`.
    pub fn body(&self) -> Value {
        let mut err = self.cause();
        if let Value::Object(o) = &mut err {
            o.insert("root_cause".into(), json!([self.cause()]));
        }
        json!({ "error": err, "status": self.status.as_u16() })
    }
}

impl std::fmt::Display for EsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.ty, self.reason)
    }
}

impl std::error::Error for EsError {}

impl From<std::io::Error> for EsError {
    fn from(e: std::io::Error) -> Self {
        EsError::internal(format!("io: {e}"))
    }
}

impl From<tantivy::TantivyError> for EsError {
    fn from(e: tantivy::TantivyError) -> Self {
        EsError::internal(format!("tantivy: {e}"))
    }
}

impl IntoResponse for EsError {
    fn into_response(self) -> Response {
        let body = serde_json::to_vec(&self.body()).unwrap_or_else(|_| b"{}".to_vec());
        (
            self.status,
            [(header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response()
    }
}
