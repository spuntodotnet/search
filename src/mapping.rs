//! Mapping Elasticsearch -> schema tantivy.
//!
//! C'est LE point dur du produit : Elasticsearch accepte des champs a la volee,
//! tantivy veut un schema fige a la creation de l'index. ferrite tranche en
//! exigeant un **mapping explicite** et en refusant tout champ inconnu, plutot
//! qu'en bricolant un schema extensible. Le mapping dynamique aura sa propre
//! iteration ; d'ici la, la couture est ici et nulle part ailleurs.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use tantivy::schema::{
    DateOptions, DateTimePrecision, Field, IndexRecordOption, NumericOptions, Schema,
    SchemaBuilder, TextFieldIndexing, TextOptions, FAST, INDEXED, STORED, STRING,
};
use tantivy::{DateTime, Term};

use crate::error::{EsError, EsResult};

/// Champs internes du schema tantivy. Prefixes par `_` — un mapping utilisateur
/// ne peut pas les redefinir (voir [`validate_field_name`]).
pub const F_ID: &str = "_id";
pub const F_SOURCE: &str = "_source";
pub const F_VERSION: &str = "_version";
pub const F_SEQ_NO: &str = "_seq_no";

/// Le tokenizer applique aux champs `text`.
///
/// `default` de tantivy = decoupe sur les non-alphanumeriques + minuscules +
/// rejet des tokens > 40 caracteres. Proche de l'analyzer `standard` d'ES pour
/// du texte latin, pas identique (voir `docs/compat.md`).
pub const TEXT_TOKENIZER: &str = "default";
/// Tokenizer des champs `keyword` : la valeur entiere, telle quelle.
pub const RAW_TOKENIZER: &str = "raw";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    Text,
    Keyword,
    Byte,
    Short,
    Integer,
    Long,
    Float,
    Double,
    Boolean,
    Date,
}

impl FieldType {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "text" => Self::Text,
            "keyword" => Self::Keyword,
            "byte" => Self::Byte,
            "short" => Self::Short,
            "integer" => Self::Integer,
            "long" => Self::Long,
            "float" => Self::Float,
            "double" => Self::Double,
            "boolean" => Self::Boolean,
            "date" => Self::Date,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Keyword => "keyword",
            Self::Byte => "byte",
            Self::Short => "short",
            Self::Integer => "integer",
            Self::Long => "long",
            Self::Float => "float",
            Self::Double => "double",
            Self::Boolean => "boolean",
            Self::Date => "date",
        }
    }

    /// Le type de stockage tantivy derriere le type ES.
    pub fn kind(self) -> FieldKind {
        match self {
            Self::Text => FieldKind::Text,
            Self::Keyword => FieldKind::Keyword,
            Self::Byte | Self::Short | Self::Integer | Self::Long => FieldKind::I64,
            Self::Float | Self::Double => FieldKind::F64,
            Self::Boolean => FieldKind::Bool,
            Self::Date => FieldKind::Date,
        }
    }

    /// Bornes du type entier ES, pour refuser une valeur hors domaine plutot que
    /// de la tronquer en silence.
    fn int_range(self) -> Option<(i64, i64)> {
        match self {
            Self::Byte => Some((-128, 127)),
            Self::Short => Some((-32_768, 32_767)),
            Self::Integer => Some((-2_147_483_648, 2_147_483_647)),
            Self::Long => Some((i64::MIN, i64::MAX)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Keyword,
    I64,
    F64,
    Bool,
    Date,
}

#[derive(Debug, Clone)]
pub struct FieldMapping {
    pub ty: FieldType,
}

/// Le mapping d'un index. Ordonne par nom, comme le rend ES.
#[derive(Debug, Clone, Default)]
pub struct Mapping {
    pub properties: BTreeMap<String, FieldMapping>,
}

impl Mapping {
    pub fn get(&self, field: &str) -> Option<&FieldMapping> {
        self.properties.get(field)
    }

    /// Le mapping tel qu'ES le rend sur `GET /{index}/_mapping`.
    pub fn to_json(&self) -> Value {
        let mut props = Map::new();
        for (name, fm) in &self.properties {
            props.insert(name.clone(), json!({ "type": fm.ty.name() }));
        }
        json!({ "properties": Value::Object(props) })
    }

    /// Parse `{"properties": {...}}`.
    ///
    /// Tout ce qui n'est pas compris est refuse : c'est la seule facon de ne pas
    /// mentir sur ce qui est indexe.
    pub fn parse(v: &Value) -> EsResult<Self> {
        let obj = v
            .as_object()
            .ok_or_else(|| EsError::mapper_parsing("[mappings] doit etre un objet"))?;

        let mut properties = BTreeMap::new();
        for (key, val) in obj {
            match key.as_str() {
                "properties" => {
                    let props = val.as_object().ok_or_else(|| {
                        EsError::mapper_parsing("[mappings.properties] doit etre un objet")
                    })?;
                    for (name, spec) in props {
                        validate_field_name(name)?;
                        properties.insert(name.clone(), parse_field_mapping(name, spec)?);
                    }
                }
                "dynamic" => {
                    // Le seul reglage compatible avec l'absence de mapping
                    // dynamique est `strict` : l'accepter, refuser les autres.
                    let s = val.as_str().unwrap_or("");
                    if !s.eq_ignore_ascii_case("strict") {
                        return Err(EsError::unsupported(
                            "ferrite ne supporte pas le mapping dynamique : seul \
                             [dynamic: strict] est accepte",
                        ));
                    }
                }
                other => {
                    return Err(EsError::unsupported(format!(
                        "ferrite ne supporte pas le parametre de mapping [{other}]"
                    )))
                }
            }
        }

        if properties.is_empty() {
            return Err(EsError::illegal_argument(
                "ferrite exige un mapping explicite : [mappings.properties] est vide ou absent",
            ));
        }
        Ok(Self { properties })
    }
}

fn parse_field_mapping(name: &str, spec: &Value) -> EsResult<FieldMapping> {
    let obj = spec.as_object().ok_or_else(|| {
        EsError::mapper_parsing(format!("[mappings.properties.{name}] doit etre un objet"))
    })?;

    let mut ty = None;
    for (key, val) in obj {
        match key.as_str() {
            "type" => {
                let s = val.as_str().ok_or_else(|| {
                    EsError::mapper_parsing(format!("[{name}.type] doit etre une chaine"))
                })?;
                ty = Some(FieldType::parse(s).ok_or_else(|| {
                    EsError::unsupported(format!(
                        "ferrite ne supporte pas le type de champ [{s}] (champ [{name}]) ; types \
                         supportes : text, keyword, byte, short, integer, long, float, double, \
                         boolean, date"
                    ))
                })?);
            }
            "properties" => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas les champs objet/imbriques (champ [{name}])"
                )))
            }
            other => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas le parametre de champ [{other}] (champ [{name}]) ; \
                     seul [type] est accepte"
                )))
            }
        }
    }

    let ty = ty.ok_or_else(|| {
        EsError::mapper_parsing(format!("[{name}] doit declarer un [type] explicite"))
    })?;
    Ok(FieldMapping { ty })
}

fn validate_field_name(name: &str) -> EsResult<()> {
    if name.is_empty() {
        return Err(EsError::mapper_parsing("nom de champ vide"));
    }
    if name.starts_with('_') {
        return Err(EsError::mapper_parsing(format!(
            "[{name}] : les noms de champ commencant par [_] sont reserves"
        )));
    }
    if name.contains('.') {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas les noms de champ pointes (champ [{name}])"
        )));
    }
    Ok(())
}

/// Les handles tantivy resolus une fois pour toutes a l'ouverture de l'index.
#[derive(Debug, Clone)]
pub struct Fields {
    pub id: Field,
    pub source: Field,
    pub version: Field,
    pub seq_no: Field,
    pub mapped: BTreeMap<String, (Field, FieldType)>,
}

impl Fields {
    pub fn get(&self, name: &str) -> Option<(Field, FieldType)> {
        self.mapped.get(name).copied()
    }
}

/// Construit le schema tantivy correspondant a un mapping ES.
pub fn build_schema(mapping: &Mapping) -> (Schema, Fields) {
    let mut b = SchemaBuilder::new();

    let id = b.add_text_field(F_ID, STRING | STORED | FAST);
    let source = b.add_text_field(F_SOURCE, STORED);
    let version = b.add_u64_field(F_VERSION, FAST | STORED);
    let seq_no = b.add_u64_field(F_SEQ_NO, FAST | STORED);

    let mut mapped = BTreeMap::new();
    for (name, fm) in &mapping.properties {
        let field = match fm.ty.kind() {
            FieldKind::Text => {
                let opts = TextOptions::default().set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer(TEXT_TOKENIZER)
                        .set_index_option(IndexRecordOption::WithFreqsAndPositions),
                );
                b.add_text_field(name, opts)
            }
            FieldKind::Keyword => {
                // `fast` pour pouvoir trier dessus ; `raw` pour que le terme soit
                // la valeur entiere, comme un keyword ES.
                let opts = TextOptions::default()
                    .set_indexing_options(
                        TextFieldIndexing::default()
                            .set_tokenizer(RAW_TOKENIZER)
                            .set_index_option(IndexRecordOption::Basic),
                    )
                    .set_fast(Some(RAW_TOKENIZER));
                b.add_text_field(name, opts)
            }
            FieldKind::I64 => b.add_i64_field(name, NumericOptions::from(INDEXED | FAST)),
            FieldKind::F64 => b.add_f64_field(name, NumericOptions::from(INDEXED | FAST)),
            FieldKind::Bool => b.add_bool_field(name, NumericOptions::from(INDEXED | FAST)),
            FieldKind::Date => b.add_date_field(
                name,
                DateOptions::from(INDEXED | FAST).set_precision(DateTimePrecision::Milliseconds),
            ),
        };
        mapped.insert(name.clone(), (field, fm.ty));
    }

    (
        b.build(),
        Fields {
            id,
            source,
            version,
            seq_no,
            mapped,
        },
    )
}

/// Une valeur JSON convertie au type du champ, prete a devenir un `Term` ou une
/// valeur de document.
#[derive(Debug, Clone, PartialEq)]
pub enum TypedValue {
    Str(String),
    I64(i64),
    F64(f64),
    Bool(bool),
    /// Millisecondes depuis l'epoch.
    Date(i64),
}

impl TypedValue {
    pub fn to_term(&self, field: Field) -> Term {
        match self {
            Self::Str(s) => Term::from_field_text(field, s),
            Self::I64(v) => Term::from_field_i64(field, *v),
            Self::F64(v) => Term::from_field_f64(field, *v),
            Self::Bool(v) => Term::from_field_bool(field, *v),
            Self::Date(ms) => Term::from_field_date(
                field,
                DateTime::from_timestamp_millis(*ms).truncate(DateTimePrecision::Milliseconds),
            ),
        }
    }
}

/// Convertit une valeur JSON vers le type declare du champ, avec les coercions
/// que fait ES (chaine numerique -> nombre, `"true"` -> booleen, ...).
///
/// Toute valeur non convertible est une erreur : jamais de valeur ignoree.
pub fn coerce(field: &str, ty: FieldType, v: &Value) -> EsResult<TypedValue> {
    let bad = |expected: &str| {
        EsError::mapper_parsing(format!(
            "failed to parse field [{field}] of type [{}] : valeur {v} non convertible en \
             {expected}",
            ty.name()
        ))
    };

    match ty.kind() {
        FieldKind::Text | FieldKind::Keyword => Ok(TypedValue::Str(match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => return Err(bad("une chaine")),
        })),
        FieldKind::I64 => {
            let n = match v {
                Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        i
                    } else if let Some(f) = n.as_f64() {
                        if f.fract() != 0.0 {
                            return Err(bad("un entier"));
                        }
                        f as i64
                    } else {
                        return Err(bad("un entier"));
                    }
                }
                Value::String(s) => s.trim().parse::<i64>().map_err(|_| bad("un entier"))?,
                Value::Bool(b) => i64::from(*b),
                _ => return Err(bad("un entier")),
            };
            if let Some((lo, hi)) = ty.int_range() {
                if n < lo || n > hi {
                    return Err(EsError::mapper_parsing(format!(
                        "failed to parse field [{field}] of type [{}] : {n} est hors des bornes \
                         [{lo}, {hi}]",
                        ty.name()
                    )));
                }
            }
            Ok(TypedValue::I64(n))
        }
        FieldKind::F64 => {
            let f = match v {
                Value::Number(n) => n.as_f64().ok_or_else(|| bad("un nombre"))?,
                Value::String(s) => s.trim().parse::<f64>().map_err(|_| bad("un nombre"))?,
                Value::Bool(b) => f64::from(u8::from(*b)),
                _ => return Err(bad("un nombre")),
            };
            Ok(TypedValue::F64(f))
        }
        FieldKind::Bool => {
            let b = match v {
                Value::Bool(b) => *b,
                Value::String(s) => match s.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(bad("un booleen")),
                },
                _ => return Err(bad("un booleen")),
            };
            Ok(TypedValue::Bool(b))
        }
        FieldKind::Date => Ok(TypedValue::Date(parse_date(field, v)?)),
    }
}

/// `strict_date_optional_time || epoch_millis`, le format par defaut d'ES.
fn parse_date(field: &str, v: &Value) -> EsResult<i64> {
    use time::format_description::well_known::Rfc3339;
    use time::macros::format_description;
    use time::{Date, OffsetDateTime, PrimitiveDateTime};

    match v {
        Value::Number(n) => n.as_i64().ok_or_else(|| {
            EsError::mapper_parsing(format!("[{field}] : date epoch_millis {n} invalide"))
        }),
        Value::String(s) => {
            let s = s.trim();
            if let Ok(dt) = OffsetDateTime::parse(s, &Rfc3339) {
                return Ok((dt.unix_timestamp_nanos() / 1_000_000) as i64);
            }
            let naive = format_description!(
                "[year]-[month]-[day]T[hour]:[minute]:[second][optional [.[subsecond]]]"
            );
            if let Ok(dt) = PrimitiveDateTime::parse(s, naive) {
                return Ok((dt.assume_utc().unix_timestamp_nanos() / 1_000_000) as i64);
            }
            let day = format_description!("[year]-[month]-[day]");
            if let Ok(d) = Date::parse(s, day) {
                return Ok(d.midnight().assume_utc().unix_timestamp() * 1000);
            }
            if let Ok(ms) = s.parse::<i64>() {
                return Ok(ms);
            }
            Err(EsError::mapper_parsing(format!(
                "failed to parse date field [{field}] with value [{s}] : formats acceptes = \
                 strict_date_optional_time, epoch_millis"
            )))
        }
        _ => Err(EsError::mapper_parsing(format!(
            "failed to parse date field [{field}] : valeur {v} invalide"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(json_str: &str) -> EsResult<Mapping> {
        Mapping::parse(&serde_json::from_str(json_str).unwrap())
    }

    #[test]
    fn parse_les_types_du_perimetre() {
        let m = mapping(
            r#"{"properties":{"t":{"type":"text"},"k":{"type":"keyword"},
                "i":{"type":"integer"},"l":{"type":"long"},"d":{"type":"double"},
                "b":{"type":"boolean"},"dt":{"type":"date"}}}"#,
        )
        .unwrap();
        assert_eq!(m.properties.len(), 7);
        assert_eq!(m.get("k").unwrap().ty, FieldType::Keyword);
    }

    #[test]
    fn refuse_un_type_inconnu() {
        let e = mapping(r#"{"properties":{"g":{"type":"geo_point"}}}"#).unwrap_err();
        assert_eq!(e.ty, UNSUPPORTED_TY);
        assert!(e.reason.contains("geo_point"));
    }

    #[test]
    fn refuse_un_parametre_de_champ_non_supporte() {
        let e = mapping(r#"{"properties":{"t":{"type":"text","analyzer":"french"}}}"#).unwrap_err();
        assert!(e.reason.contains("analyzer"));
    }

    #[test]
    fn refuse_les_multi_fields() {
        let e = mapping(
            r#"{"properties":{"t":{"type":"text","fields":{"keyword":{"type":"keyword"}}}}}"#,
        )
        .unwrap_err();
        assert!(e.reason.contains("fields"));
    }

    #[test]
    fn refuse_un_mapping_vide() {
        assert!(mapping(r#"{"properties":{}}"#).is_err());
    }

    const UNSUPPORTED_TY: &str = crate::error::UNSUPPORTED;

    #[test]
    fn coercions_es() {
        assert_eq!(
            coerce("a", FieldType::Integer, &json!("42")).unwrap(),
            TypedValue::I64(42)
        );
        assert_eq!(
            coerce("a", FieldType::Boolean, &json!("true")).unwrap(),
            TypedValue::Bool(true)
        );
        assert!(coerce("a", FieldType::Integer, &json!(1e12)).is_err());
        assert!(coerce("a", FieldType::Integer, &json!("abc")).is_err());
    }

    #[test]
    fn dates() {
        assert_eq!(parse_date("d", &json!("1970-01-01")).unwrap(), 0);
        assert_eq!(
            parse_date("d", &json!("1970-01-01T00:00:01Z")).unwrap(),
            1000
        );
        assert_eq!(parse_date("d", &json!(1234)).unwrap(), 1234);
        assert!(parse_date("d", &json!("hier")).is_err());
    }
}
