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
    /// Les `root_cause[]` a rendre, quand ils ne sont pas l'erreur elle-meme.
    ///
    /// ES **groupe** les echecs de shard sous une erreur unique, mais garde
    /// dans `root_cause` la cause de chacun : c'est ce qui permet a un client de
    /// dire quel index a echoue et pourquoi.
    /// (Boxee : rare, et `EsError` voyage dans tous les `Result` du serveur.)
    pub racines: Option<Box<Vec<Value>>>,
    /// Le champ absent de **ce** mapping, quand c'est la cause de l'erreur.
    ///
    /// Sur un index unique, c'est une erreur (regle du projet : un champ
    /// inconnu n'est pas 0 resultat). Sur une recherche multi-index, c'est une
    /// information : si un **autre** index vise connait ce champ, ce n'est plus
    /// une faute de frappe, seulement un mapping heterogene — la clause devient
    /// alors « ne correspond a rien » pour cet index-la, comme chez ES.
    pub champ_inconnu: Option<Box<str>>,
    /// La valeur cherchee n'a pas le type du champ vise (« alice » sur un
    /// `long`, une date illisible, une phrase a prefixe sur un `keyword`).
    ///
    /// C'est exactement ce que `lenient` couvre chez Elasticsearch : sans lui
    /// l'erreur sort, avec lui le champ est simplement ecarte de la clause
    /// (mesure contre ES 8.15 : voir [`crate::dsl`]). Aucune autre erreur n'est
    /// avalee par `lenient` — un parametre non supporte reste un refus.
    pub valeur_illisible: bool,
    /// L'erreur est celle d'**un** index, pas de la requete : chez ES, c'est un
    /// echec de shard, groupe sous « all shards failed » quand tous echouent.
    ///
    /// C'est le cas d'un `format` pose sur un `keyword` ou d'un
    /// `docvalue_fields` sur un `text` : la requete est valide, c'est *ce*
    /// mapping-la qui ne sait pas y repondre. Sur une recherche multi-index,
    /// les autres index repondent quand meme.
    pub de_shard: bool,
}

impl EsError {
    pub fn new(status: StatusCode, ty: &str, reason: impl Into<String>) -> Self {
        Self {
            status,
            ty: ty.to_string(),
            reason: reason.into(),
            extra: Vec::new(),
            racines: None,
            champ_inconnu: None,
            valeur_illisible: false,
            de_shard: false,
        }
    }

    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.extra.push((key.to_string(), value));
        self
    }

    /// Remplace les `root_cause[]` par les causes fournies.
    pub fn avec_racines(mut self, racines: Vec<Value>) -> Self {
        self.racines = Some(Box::new(racines));
        self
    }

    /// Marque une erreur comme « champ absent de ce mapping » (voir
    /// [`EsError::champ_inconnu`]).
    pub fn sur_champ_inconnu(mut self, champ: &str) -> Self {
        self.champ_inconnu = Some(champ.into());
        self
    }

    /// Marque une erreur comme « la valeur n'a pas le type du champ » — la
    /// seule famille d'erreurs que `lenient` avale (voir
    /// [`EsError::valeur_illisible`]).
    pub fn sur_valeur_illisible(mut self) -> Self {
        self.valeur_illisible = true;
        self
    }

    /// Marque une erreur comme celle d'**un** index (voir
    /// [`EsError::de_shard`]).
    pub fn sur_un_shard(mut self) -> Self {
        self.de_shard = true;
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
            let racines = match &self.racines {
                Some(r) => Value::Array((**r).clone()),
                None => json!([self.cause()]),
            };
            o.insert("root_cause".into(), racines);
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
