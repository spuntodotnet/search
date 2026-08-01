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

use crate::analysis::{self, Analyzer};
use crate::error::{EsError, EsResult};

/// Champs internes du schema tantivy. Prefixes par `_` — un mapping utilisateur
/// ne peut pas les redefinir (voir [`validate_field_name`]).
pub const F_ID: &str = "_id";
pub const F_SOURCE: &str = "_source";
pub const F_VERSION: &str = "_version";
pub const F_SEQ_NO: &str = "_seq_no";

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
    /// L'analyzer d'un champ `text`. `None` = celui par defaut (`standard`).
    pub analyzer: Option<Analyzer>,
    /// Les multi-fields : le meme contenu indexe autrement, sous
    /// `parent.sous_champ`. ES n'en autorise qu'un niveau.
    pub fields: BTreeMap<String, FieldMapping>,
    /// `ignore_above` : au-dela de cette longueur, la chaine n'est pas indexee
    /// (elle reste dans `_source`). Le defaut d'ES pour les `.keyword` generes
    /// dynamiquement est 256.
    pub ignore_above: Option<usize>,
}

impl FieldMapping {
    pub fn new(ty: FieldType) -> Self {
        Self {
            ty,
            analyzer: None,
            fields: BTreeMap::new(),
            ignore_above: None,
        }
    }

    /// L'analyzer effectif d'un champ `text`.
    pub fn analyzer(&self) -> Analyzer {
        self.analyzer.unwrap_or_default()
    }

    fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("type".into(), json!(self.ty.name()));
        if let Some(a) = self.analyzer {
            o.insert("analyzer".into(), json!(a.name()));
        }
        if let Some(n) = self.ignore_above {
            o.insert("ignore_above".into(), json!(n));
        }
        if !self.fields.is_empty() {
            let mut subs = Map::new();
            for (name, fm) in &self.fields {
                subs.insert(name.clone(), fm.to_json());
            }
            o.insert("fields".into(), Value::Object(subs));
        }
        Value::Object(o)
    }
}

/// Que faire d'un champ absent du mapping, a l'indexation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dynamic {
    /// Le defaut d'ES : le type est devine et le champ ajoute au mapping.
    #[default]
    True,
    /// Le champ reste dans `_source` mais n'est ni indexe ni interrogeable.
    False,
    /// Le document est refuse.
    Strict,
}

impl Dynamic {
    fn parse(v: &Value) -> EsResult<Self> {
        let s = match v {
            Value::Bool(true) => "true",
            Value::Bool(false) => "false",
            Value::String(s) => s.as_str(),
            _ => return Err(EsError::mapper_parsing("[dynamic] : valeur invalide")),
        };
        match s.to_ascii_lowercase().as_str() {
            "true" => Ok(Self::True),
            "false" => Ok(Self::False),
            "strict" => Ok(Self::Strict),
            "runtime" => Err(EsError::unsupported(
                "ferrite ne supporte pas [dynamic: runtime] ; valeurs acceptees : true, false, \
                 strict",
            )),
            other => Err(EsError::mapper_parsing(format!(
                "[dynamic] : valeur [{other}] invalide (true, false, strict)"
            ))),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::True => "true",
            Self::False => "false",
            Self::Strict => "strict",
        }
    }
}

/// Le mapping d'un index. Ordonne par nom, comme le rend ES.
#[derive(Debug, Clone, Default)]
pub struct Mapping {
    pub properties: BTreeMap<String, FieldMapping>,
    pub dynamic: Dynamic,
}

impl Mapping {
    pub fn get(&self, field: &str) -> Option<&FieldMapping> {
        self.properties.get(field)
    }

    /// Le mapping tel qu'ES le rend sur `GET /{index}/_mapping`.
    pub fn to_json(&self) -> Value {
        let mut props = Map::new();
        for (name, fm) in &self.properties {
            props.insert(name.clone(), fm.to_json());
        }
        let mut o = Map::new();
        if self.dynamic != Dynamic::True {
            o.insert("dynamic".into(), json!(self.dynamic.name()));
        }
        o.insert("properties".into(), Value::Object(props));
        Value::Object(o)
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
        let mut dynamic = Dynamic::default();
        for (key, val) in obj {
            match key.as_str() {
                "properties" => {
                    let props = val.as_object().ok_or_else(|| {
                        EsError::mapper_parsing("[mappings.properties] doit etre un objet")
                    })?;
                    for (name, spec) in props {
                        validate_field_name(name)?;
                        properties.insert(name.clone(), parse_field_mapping(name, spec, false)?);
                    }
                }
                "dynamic" => dynamic = Dynamic::parse(val)?,
                other => {
                    return Err(EsError::unsupported(format!(
                        "ferrite ne supporte pas le parametre de mapping [{other}]"
                    )))
                }
            }
        }

        // Un mapping vide est desormais licite : les champs viendront des
        // documents si `dynamic` le permet.
        if properties.is_empty() && dynamic == Dynamic::Strict {
            return Err(EsError::illegal_argument(
                "[dynamic: strict] avec un mapping vide refuserait tous les documents : declare \
                 des [properties]",
            ));
        }
        Ok(Self {
            properties,
            dynamic,
        })
    }
}

/// Parse la declaration d'un champ.
///
/// `sous_champ` indique qu'on est deja dans un `fields` : ES n'autorise qu'un
/// seul niveau de multi-fields, et ferrite refuse le second explicitement.
fn parse_field_mapping(name: &str, spec: &Value, sous_champ: bool) -> EsResult<FieldMapping> {
    let obj = spec.as_object().ok_or_else(|| {
        EsError::mapper_parsing(format!("[mappings.properties.{name}] doit etre un objet"))
    })?;

    let mut ty = None;
    let mut fields = BTreeMap::new();
    let mut ignore_above = None;
    let mut analyzer = None;

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
            "fields" => {
                if sous_champ {
                    return Err(EsError::mapper_parsing(format!(
                        "[{name}] : un multi-field ne peut pas en contenir d'autres \
                         (Elasticsearch n'autorise qu'un niveau)"
                    )));
                }
                let subs = val.as_object().ok_or_else(|| {
                    EsError::mapper_parsing(format!("[{name}.fields] doit etre un objet"))
                })?;
                for (sub_name, sub_spec) in subs {
                    validate_field_name_part(sub_name, &format!("{name}.{sub_name}"))?;
                    fields.insert(
                        sub_name.clone(),
                        parse_field_mapping(&format!("{name}.{sub_name}"), sub_spec, true)?,
                    );
                }
            }
            "analyzer" => {
                let nom = val.as_str().ok_or_else(|| {
                    EsError::mapper_parsing(format!("[{name}.analyzer] doit etre une chaine"))
                })?;
                analyzer = Some(analysis::parse_declaration(nom, name)?);
            }
            "ignore_above" => {
                ignore_above = Some(
                    val.as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or_else(|| {
                            EsError::mapper_parsing(format!(
                                "[{name}.ignore_above] : entier positif attendu"
                            ))
                        })?,
                );
            }
            "properties" => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas les champs objet/imbriques (champ [{name}])"
                )))
            }
            other => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas le parametre de champ [{other}] (champ [{name}]) ; \
                     parametres acceptes : type, analyzer, fields, ignore_above"
                )))
            }
        }
    }

    let ty = ty.ok_or_else(|| {
        EsError::mapper_parsing(format!("[{name}] doit declarer un [type] explicite"))
    })?;
    if ignore_above.is_some() && ty.kind() != FieldKind::Keyword {
        return Err(EsError::mapper_parsing(format!(
            "[{name}] : [ignore_above] ne s'applique qu'a un champ [keyword]"
        )));
    }
    if analyzer.is_some() && ty.kind() != FieldKind::Text {
        return Err(EsError::mapper_parsing(format!(
            "[{name}] : [analyzer] ne s'applique qu'a un champ [text]"
        )));
    }
    Ok(FieldMapping {
        ty,
        analyzer,
        fields,
        ignore_above,
    })
}

/// Le champ porte-t-il un objet, directement ou dans un tableau ?
///
/// `infer` rend `None` sur un objet — sans ce test, un document dont un champ
/// vaut `[{...}]` serait **accepte en silence** : conserve dans `_source`,
/// absent du mapping, donc introuvable. C'est exactement l'echec silencieux que
/// ce projet refuse ; l'objet nu, lui, etait deja rejete.
pub fn contient_un_objet(value: &Value) -> bool {
    match value {
        Value::Object(_) => true,
        Value::Array(a) => a.iter().any(contient_un_objet),
        _ => false,
    }
}

/// Devine le mapping d'un champ a partir de sa premiere valeur, comme le fait
/// Elasticsearch quand `dynamic` vaut `true`.
///
/// Les regles sont celles d'ES et elles ont des consequences : une chaine
/// devient un `text` **doublé d'un sous-champ `keyword`** (c'est ce qui permet
/// de trier et de filtrer exactement dessus), et la detection de date est
/// active par defaut alors que la detection de nombre ne l'est pas.
pub fn infer(value: &Value) -> Option<FieldMapping> {
    match value {
        Value::Null => None,
        // Un tableau prend le type de son premier element non nul.
        Value::Array(a) => a.iter().find_map(infer),
        Value::Bool(_) => Some(FieldMapping::new(FieldType::Boolean)),
        Value::Number(n) => Some(FieldMapping::new(if n.is_f64() {
            // ES retient `float`, pas `double`, pour un flottant devine.
            FieldType::Float
        } else {
            FieldType::Long
        })),
        Value::String(s) => {
            // `date_detection` est actif par defaut chez ES ; `numeric_detection`
            // ne l'est pas, donc « 42 » reste du texte.
            if parse_date("_", value).is_ok() && ressemble_a_une_date(s) {
                return Some(FieldMapping::new(FieldType::Date));
            }
            let mut fm = FieldMapping::new(FieldType::Text);
            fm.fields.insert(
                "keyword".to_string(),
                FieldMapping {
                    ty: FieldType::Keyword,
                    analyzer: None,
                    fields: BTreeMap::new(),
                    ignore_above: Some(256),
                },
            );
            Some(fm)
        }
        Value::Object(_) => None,
    }
}

/// `parse_date` accepte aussi les entiers en chaine (`epoch_millis`) ; la
/// detection dynamique d'ES, elle, ne considere que les formats de date.
fn ressemble_a_une_date(s: &str) -> bool {
    let s = s.trim();
    s.len() >= 8 && s.as_bytes().iter().filter(|b| **b == b'-').count() >= 2
}

fn validate_field_name_part(name: &str, chemin: &str) -> EsResult<()> {
    if name.is_empty() {
        return Err(EsError::mapper_parsing("nom de champ vide"));
    }
    if name.contains('.') {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas les noms de champ pointes (champ [{chemin}])"
        )));
    }
    Ok(())
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

/// Un champ du schema tantivy, resolu.
#[derive(Debug, Clone, Copy)]
pub struct MappedField {
    pub field: Field,
    pub ty: FieldType,
    pub ignore_above: Option<usize>,
    /// L'analyzer a appliquer aux requetes sur ce champ.
    pub analyzer: Analyzer,
}

/// Les handles tantivy resolus une fois pour toutes a l'ouverture de l'index.
#[derive(Debug, Clone)]
pub struct Fields {
    pub id: Field,
    pub source: Field,
    pub version: Field,
    pub seq_no: Field,
    /// Tous les champs interrogeables, par chemin complet : `titre`,
    /// `titre.keyword`, ...
    pub mapped: BTreeMap<String, MappedField>,
    /// Pour une propriete de premier niveau, toutes les cibles a alimenter a
    /// l'indexation : le champ lui-meme **et** ses multi-fields.
    pub targets: BTreeMap<String, Vec<MappedField>>,
}

impl Fields {
    pub fn get(&self, name: &str) -> Option<MappedField> {
        self.mapped.get(name).copied()
    }

    pub fn targets_of(&self, name: &str) -> Option<&[MappedField]> {
        self.targets.get(name).map(Vec::as_slice)
    }
}

/// Construit le schema tantivy correspondant a un mapping ES.
///
/// Un multi-field devient un champ tantivy a part entiere, nomme par son chemin
/// (`titre.keyword`) : c'est ce qui permet de l'interroger et de trier dessus
/// comme n'importe quel autre champ, sans traitement particulier ailleurs.
pub fn build_schema(mapping: &Mapping) -> (Schema, Fields) {
    let mut b = SchemaBuilder::new();

    let id = b.add_text_field(F_ID, STRING | STORED | FAST);
    let source = b.add_text_field(F_SOURCE, STORED);
    let version = b.add_u64_field(F_VERSION, FAST | STORED);
    let seq_no = b.add_u64_field(F_SEQ_NO, FAST | STORED);

    let mut mapped = BTreeMap::new();
    let mut targets: BTreeMap<String, Vec<MappedField>> = BTreeMap::new();

    for (name, fm) in &mapping.properties {
        let mut cibles = Vec::with_capacity(1 + fm.fields.len());
        for (chemin, decl) in std::iter::once((name.clone(), fm)).chain(
            fm.fields
                .iter()
                .map(|(sub, decl)| (format!("{name}.{sub}"), decl)),
        ) {
            let entry = MappedField {
                field: add_field(&mut b, &chemin, decl.ty, decl.analyzer()),
                ty: decl.ty,
                ignore_above: decl.ignore_above,
                analyzer: decl.analyzer(),
            };
            mapped.insert(chemin, entry);
            cibles.push(entry);
        }
        targets.insert(name.clone(), cibles);
    }

    (
        b.build(),
        Fields {
            id,
            source,
            version,
            seq_no,
            mapped,
            targets,
        },
    )
}

fn add_field(b: &mut SchemaBuilder, name: &str, ty: FieldType, analyzer: Analyzer) -> Field {
    match ty.kind() {
        FieldKind::Text => {
            let opts = TextOptions::default().set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer(analyzer.tokenizer())
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
    }
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
    fn multi_fields() {
        let m = mapping(
            r#"{"properties":{"t":{"type":"text",
                "fields":{"keyword":{"type":"keyword","ignore_above":256}}}}}"#,
        )
        .unwrap();
        let sous = &m.get("t").unwrap().fields["keyword"];
        assert_eq!(sous.ty, FieldType::Keyword);
        assert_eq!(sous.ignore_above, Some(256));

        // Le schema expose le sous-champ par son chemin complet.
        let (_, fields) = build_schema(&m);
        assert!(fields.get("t").is_some());
        assert!(fields.get("t.keyword").is_some());
        assert_eq!(fields.targets_of("t").unwrap().len(), 2);
    }

    #[test]
    fn refuse_deux_niveaux_de_multi_fields() {
        let e = mapping(
            r#"{"properties":{"t":{"type":"text","fields":{
                "k":{"type":"keyword","fields":{"encore":{"type":"text"}}}}}}}"#,
        )
        .unwrap_err();
        assert!(e.reason.contains("niveau"));
    }

    #[test]
    fn ignore_above_reserve_aux_keyword() {
        let e = mapping(r#"{"properties":{"t":{"type":"text","ignore_above":10}}}"#).unwrap_err();
        assert!(e.reason.contains("ignore_above"));
    }

    #[test]
    fn un_mapping_vide_est_licite_en_dynamique() {
        // Les champs viendront des documents : c'est le defaut d'ES.
        let m = mapping(r#"{"properties":{}}"#).unwrap();
        assert!(m.properties.is_empty());
        assert_eq!(m.dynamic, Dynamic::True);
    }

    #[test]
    fn refuse_un_mapping_vide_en_strict() {
        assert!(mapping(r#"{"dynamic":"strict","properties":{}}"#).is_err());
    }

    #[test]
    fn inference_des_types() {
        let t = |v: Value| infer(&v).map(|fm| fm.ty);
        assert_eq!(t(json!(42)), Some(FieldType::Long));
        assert_eq!(t(json!(4.5)), Some(FieldType::Float));
        assert_eq!(t(json!(true)), Some(FieldType::Boolean));
        assert_eq!(t(json!("2025-01-15")), Some(FieldType::Date));
        // `numeric_detection` est desactive chez ES : « 42 » reste du texte.
        assert_eq!(t(json!("42")), Some(FieldType::Text));
        assert_eq!(t(json!(null)), None);
        // Un tableau prend le type de son premier element non nul.
        assert_eq!(t(json!([null, 7])), Some(FieldType::Long));

        // Une chaine gagne son sous-champ `.keyword`, comme chez ES.
        let fm = infer(&json!("bonjour")).unwrap();
        assert_eq!(fm.ty, FieldType::Text);
        assert_eq!(fm.fields["keyword"].ty, FieldType::Keyword);
        assert_eq!(fm.fields["keyword"].ignore_above, Some(256));
    }

    #[test]
    fn un_objet_se_reconnait_meme_dans_un_tableau() {
        // `infer` rend `None` dans les deux cas ; c'est ce test qui distingue
        // « pas de type » de « type refuse », et donc l'erreur explicite de
        // l'acceptation en silence.
        assert!(contient_un_objet(&json!({"a": 1})));
        assert!(contient_un_objet(&json!([{"a": 1}])));
        assert!(contient_un_objet(&json!([null, [{"a": 1}]])));
        assert!(!contient_un_objet(&json!([1, 2])));
        assert!(!contient_un_objet(&json!([])));
        assert!(!contient_un_objet(&json!("a")));
        assert!(!contient_un_objet(&json!(null)));
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
