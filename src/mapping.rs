//! Mapping Elasticsearch -> schema tantivy.
//!
//! C'est LE point dur du produit : Elasticsearch accepte des champs a la volee,
//! tantivy veut un schema fige a la creation de l'index. ferrite tranche en
//! exigeant un **mapping explicite** et en refusant tout champ inconnu, plutot
//! qu'en bricolant un schema extensible. Le mapping dynamique aura sa propre
//! iteration ; d'ici la, la couture est ici et nulle part ailleurs.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};
use tantivy::schema::{
    DateOptions, DateTimePrecision, Field, IndexRecordOption, NumericOptions, Schema,
    SchemaBuilder, TextFieldIndexing, TextOptions, FAST, INDEXED, STORED, STRING,
};
use tantivy::{DateTime, Term};

use crate::analysis::{self, Analysis, Analyzer};
use crate::dateformat::DateFormat;
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
    /// L'analyzer d'un champ `text`, **a l'indexation**. `None` = celui par
    /// defaut (`standard`).
    pub analyzer: Option<Analyzer>,
    /// L'analyzer d'un champ `text` **a la requete**, quand il differe de celui
    /// de l'indexation. C'est le compagnon oblige des n-grammes : on indexe en
    /// grammes, on cherche le mot entier — sans lui, `elan` rend tout ce qui
    /// commence par `e`.
    pub search_analyzer: Option<Analyzer>,
    /// Les multi-fields : le meme contenu indexe autrement, sous
    /// `parent.sous_champ`. ES n'en autorise qu'un niveau.
    pub fields: BTreeMap<String, FieldMapping>,
    /// `ignore_above` : au-dela de cette longueur, la chaine n'est pas indexee
    /// (elle reste dans `_source`). Le defaut d'ES pour les `.keyword` generes
    /// dynamiquement est 256.
    pub ignore_above: Option<usize>,
    /// Le `format` d'un champ `date`. `None` = celui d'ES par defaut.
    pub format: Option<DateFormat>,
    /// `copy_to` : les champs dans lesquels la valeur **brute** de celui-ci est
    /// recopiee a l'indexation. C'est ainsi qu'on se refait un `_all`.
    pub copy_to: Vec<String>,
    /// `store` : la valeur est conservee a part du `_source`, et c'est elle que
    /// `stored_fields` rend.
    pub store: bool,
    /// `index` : le champ entre-t-il dans l'index inverse ? Le defaut d'ES est
    /// `true`.
    ///
    /// A `false`, ES ne renonce pas a chercher : il **retombe sur la colonne**
    /// (ses *doc values*) pour tout type qui en a une — `keyword`, numeriques,
    /// `date`, `boolean` restent donc interrogeables, triables et agregeables,
    /// au prix d'un balayage et d'un score **constant**. Seul un `text`, qui
    /// n'a pas de colonne, devient inerte : ES refuse alors la clause en
    /// `query_shard_exception` (mesure contre 8.15.0, voir
    /// [`sonde_index_false.py`](../tests/compat/sonde_index_false.py)).
    pub indexe: bool,
}

impl FieldMapping {
    pub fn new(ty: FieldType) -> Self {
        Self {
            ty,
            analyzer: None,
            search_analyzer: None,
            fields: BTreeMap::new(),
            ignore_above: None,
            format: None,
            copy_to: Vec::new(),
            store: false,
            indexe: true,
        }
    }

    /// Le format effectif d'un champ `date`.
    pub fn format(&self) -> DateFormat {
        self.format.clone().unwrap_or_default()
    }

    /// L'analyzer effectif d'un champ `text`, a l'indexation.
    pub fn analyzer(&self) -> Analyzer {
        self.analyzer.unwrap_or_default()
    }

    /// L'analyzer effectif d'un champ `text`, a la requete : celui de
    /// `search_analyzer` s'il est declare, sinon celui de l'indexation. C'est la
    /// regle d'ES.
    pub fn search_analyzer(&self) -> Analyzer {
        self.search_analyzer.unwrap_or_else(|| self.analyzer())
    }

    fn to_json(&self, analysis: &Analysis) -> Value {
        let mut o = Map::new();
        o.insert("type".into(), json!(self.ty.name()));
        // ES nomme l'analyzer d'indexation des qu'un `search_analyzer` est
        // declare, meme quand il est reste celui par defaut — et il l'appelle
        // alors `default` (mesure contre 8.15).
        match (self.analyzer, self.search_analyzer) {
            (Some(a), _) => {
                o.insert("analyzer".into(), json!(a.name(analysis)));
            }
            (None, Some(_)) => {
                o.insert("analyzer".into(), json!("default"));
            }
            (None, None) => {}
        }
        if let Some(a) = self.search_analyzer {
            o.insert("search_analyzer".into(), json!(a.name(analysis)));
        }
        // `store: false` est le defaut : ES ne le rend pas, seulement `true`.
        if self.store {
            o.insert("store".into(), json!(true));
        }
        // `index` est l'exact miroir : `true` est le defaut, donc ES ne le rend
        // pas ; `false` est conserve. Un mapping qui perdrait le second ne
        // ferait pas d'aller-retour, et un outil qui relit le mapping pour
        // decider s'il doit reindexer y lirait le contraire de ce qu'il a pose.
        if !self.indexe {
            o.insert("index".into(), json!(false));
        }
        if let Some(n) = self.ignore_above {
            o.insert("ignore_above".into(), json!(n));
        }
        if let Some(f) = &self.format {
            o.insert("format".into(), json!(f.source));
        }
        // Toujours un tableau, meme pour une cible unique declaree en chaine.
        if !self.copy_to.is_empty() {
            o.insert("copy_to".into(), json!(self.copy_to));
        }
        if !self.fields.is_empty() {
            let mut subs = Map::new();
            for (name, fm) in &self.fields {
                subs.insert(name.clone(), fm.to_json(analysis));
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
    pub fn parse(v: &Value) -> EsResult<Self> {
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

/// Le champ `join` d'un index : un seul, et ses relations parent -> enfants.
///
/// ferrite etant mono-shard, parent et enfant sont forcement au meme endroit :
/// la jointure n'a besoin ni de routage ni de *global ordinals*, seulement de
/// deux colonnes (le nom de la relation, l'identifiant du parent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Join {
    /// Le nom du champ (`lien`).
    pub champ: String,
    /// `parent -> enfants`.
    pub relations: BTreeMap<String, Vec<String>>,
}

impl Join {
    /// Le parent declare pour ce nom de relation, s'il s'agit d'un enfant.
    pub fn parent_de(&self, nom: &str) -> Option<&str> {
        self.relations
            .iter()
            .find(|(_, enfants)| enfants.iter().any(|e| e == nom))
            .map(|(p, _)| p.as_str())
    }

    pub fn connait(&self, nom: &str) -> bool {
        self.relations.contains_key(nom) || self.parent_de(nom).is_some()
    }

    pub fn noms(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.relations.keys().map(String::as_str).collect();
        for enfants in self.relations.values() {
            out.extend(enfants.iter().map(String::as_str));
        }
        out
    }

    fn to_json(&self) -> Value {
        let relations: Map<String, Value> = self
            .relations
            .iter()
            .map(|(p, enfants)| {
                let v = if enfants.len() == 1 {
                    json!(enfants[0])
                } else {
                    json!(enfants)
                };
                (p.clone(), v)
            })
            .collect();
        json!({"type": "join", "eager_global_ordinals": true, "relations": relations})
    }

    fn parse(champ: &str, obj: &Map<String, Value>) -> EsResult<Self> {
        for cle in obj.keys() {
            if !matches!(cle.as_str(), "type" | "relations" | "eager_global_ordinals") {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas [{cle}] sur un champ [join] ([{champ}])"
                )));
            }
        }
        let relations = obj
            .get("relations")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                EsError::mapper_parsing(format!("[{champ}] : un [join] declare ses [relations]"))
            })?;
        let mut out = BTreeMap::new();
        for (parent, enfants) in relations {
            let enfants = match enfants {
                Value::String(s) => vec![s.clone()],
                Value::Array(a) => a
                    .iter()
                    .map(|v| {
                        v.as_str().map(str::to_string).ok_or_else(|| {
                            EsError::mapper_parsing(format!(
                                "[{champ}.relations.{parent}] : noms d'enfants attendus"
                            ))
                        })
                    })
                    .collect::<EsResult<Vec<_>>>()?,
                _ => {
                    return Err(EsError::mapper_parsing(format!(
                        "[{champ}.relations.{parent}] : une chaine ou un tableau est attendu"
                    )))
                }
            };
            out.insert(parent.clone(), enfants);
        }
        if out.is_empty() {
            return Err(EsError::mapper_parsing(format!(
                "[{champ}] : un [join] sans relation n'a pas d'objet"
            )));
        }
        Ok(Self {
            champ: champ.to_string(),
            relations: out,
        })
    }
}

/// Le mapping d'un index. Ordonne par nom, comme le rend ES.
///
/// Les sous-objets n'ont pas de representation propre : ils sont **aplatis en
/// chemins pointes** (`fournisseur.nom`), exactement comme le fait
/// Elasticsearch, qui n'indexe pas non plus d'objet — il indexe des chemins.
/// C'est ce qui permet aux requetes, aux tris et aux agregations de resoudre un
/// champ imbrique sans rien connaitre des objets : `Fields.mapped` est deja une
/// table `chemin -> champ`. Le nichage n'existe que dans la reponse
/// `GET /{index}/_mapping`, ou [`Mapping::to_json`] le reconstruit.
#[derive(Debug, Clone)]
pub struct Mapping {
    pub properties: BTreeMap<String, FieldMapping>,
    /// Les chemins declares `type: nested`. Un `nested` s'indexe comme un objet
    /// — mais chaque valeur retient **a quel element du tableau** elle
    /// appartient, ce qui permet de retrouver la correspondance qu'un `object`
    /// perd. Voir [`crate::nested`].
    pub nested: BTreeSet<String>,
    /// Les objets declares sans aucun sous-champ (`{"type": "object"}`). Ils
    /// n'indexent rien — leurs champs viendront des documents — mais ES les
    /// rend dans `_mapping`, donc on les garde.
    pub objets_vides: BTreeSet<String>,
    /// Le champ `join`, s'il y en a un. ES n'en autorise qu'un par index.
    pub join: Option<Join>,
    /// Les analyzers sur mesure de l'index (`settings.analysis`). Ils vivent
    /// avec le mapping parce que c'est lui qui les nomme, mais ils ne sont pas
    /// rendus par `_mapping` : leur place est dans `_settings`.
    pub analysis: crate::analysis::Analysis,
    pub dynamic: Dynamic,
    /// `index.query.parse.allow_unmapped_fields` — le reglage d'Elasticsearch,
    /// avec **son** defaut (`true`).
    ///
    /// A `true`, une clause qui cite un champ absent du mapping ne correspond a
    /// rien, comme chez ES : c'est ce qui permet a un filtre pose sur chaque
    /// recherche (`archiveAt`) de fonctionner avant que le premier document ne
    /// porte le champ. A `false`, c'est une erreur explicite — le comportement
    /// que ferrite avait toujours eu, et qui attrape les fautes de frappe.
    pub allow_unmapped_fields: bool,
}

impl Default for Mapping {
    fn default() -> Self {
        Self {
            properties: BTreeMap::new(),
            nested: BTreeSet::new(),
            objets_vides: BTreeSet::new(),
            join: None,
            analysis: crate::analysis::Analysis::default(),
            dynamic: Dynamic::default(),
            allow_unmapped_fields: true,
        }
    }
}

impl Mapping {
    pub fn get(&self, field: &str) -> Option<&FieldMapping> {
        self.properties.get(field)
    }

    /// Le mapping tel qu'ES le rend sur `GET /{index}/_mapping` : les chemins
    /// pointes y redeviennent des objets imbriques.
    pub fn to_json(&self) -> Value {
        let mut props = Map::new();
        for (chemin, fm) in &self.properties {
            niche(&mut props, chemin, fm.to_json(&self.analysis));
        }
        for vide in &self.objets_vides {
            if pointe_mut(&mut props, vide).is_none() {
                niche(&mut props, vide, json!({"type": "object"}));
            }
        }
        if let Some(j) = &self.join {
            props.insert(j.champ.clone(), j.to_json());
        }
        for racine in &self.nested {
            if let Some(o) = pointe_mut(&mut props, racine).and_then(Value::as_object_mut) {
                o.insert("type".into(), json!("nested"));
            }
        }
        let mut o = Map::new();
        if self.dynamic != Dynamic::True {
            o.insert("dynamic".into(), json!(self.dynamic.name()));
        }
        o.insert("properties".into(), Value::Object(props));
        Value::Object(o)
    }

    /// Parse `{"properties": {...}}`, sous-objets compris.
    ///
    /// Tout ce qui n'est pas compris est refuse : c'est la seule facon de ne pas
    /// mentir sur ce qui est indexe.
    pub fn parse(v: &Value) -> EsResult<Self> {
        Self::parse_avec(v, &Analysis::default())
    }

    /// Parse un mapping en connaissant les analyzers declares dans les
    /// `settings` : c'est ce qui permet a un champ de citer `fr_produit`.
    pub fn parse_avec(v: &Value, declares: &Analysis) -> EsResult<Self> {
        let obj = v
            .as_object()
            .ok_or_else(|| EsError::mapper_parsing("[mappings] doit etre un objet"))?;

        let mut properties = BTreeMap::new();
        let mut nested = BTreeSet::new();
        let mut vides = BTreeSet::new();
        let mut join = None;
        let mut dynamic = Dynamic::default();
        for (key, val) in obj {
            match key.as_str() {
                "properties" => {
                    let props = val.as_object().ok_or_else(|| {
                        EsError::mapper_parsing("[mappings.properties] doit etre un objet")
                    })?;
                    for (name, spec) in props {
                        if spec.get("type").and_then(Value::as_str) == Some("join") {
                            if join.is_some() {
                                return Err(EsError::mapper_parsing(
                                    "un index n'accepte qu'un seul champ [join]",
                                ));
                            }
                            join = Some(Join::parse(name, as_obj(spec, name)?)?);
                            continue;
                        }
                        parse_propriete(
                            name,
                            spec,
                            &mut properties,
                            &mut nested,
                            &mut vides,
                            declares,
                        )?;
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
        let mapping = Self {
            properties,
            nested,
            objets_vides: vides,
            join,
            analysis: declares.clone(),
            dynamic,
            ..Self::default()
        };
        mapping.verifier_copies()?;
        Ok(mapping)
    }

    /// Les trois refus qu'ES oppose a une cible de `copy_to`, mesures contre
    /// 8.15 et repris avec ses phrases.
    ///
    /// Ils ne peuvent pas se verifier a la lecture d'un champ : il faut le
    /// mapping entier pour savoir ce qu'est la cible. Une cible **inconnue**,
    /// elle, est licite — c'est le mapping dynamique qui la creera, au type de
    /// la valeur copiee.
    pub fn verifier_copies(&self) -> EsResult<()> {
        for (source, fm) in &self.properties {
            for cible in &fm.copy_to {
                // Un multi-field ne peut etre ni la source ni la cible d'une
                // copie ; la source est refusee des la lecture du champ.
                if let Some((parent, _)) = cible.rsplit_once('.') {
                    if self
                        .properties
                        .get(parent)
                        .is_some_and(|p| p.fields.contains_key(&cible[parent.len() + 1..]))
                    {
                        return Err(EsError::illegal_argument(format!(
                            "[copy_to] may not be used to copy to a multi-field: [{cible}]"
                        )));
                    }
                }
                if self.objets_vides.contains(cible)
                    || self.nested.contains(cible)
                    || self
                        .properties
                        .keys()
                        .any(|c| est_sous_chemin(c, cible.as_str()))
                {
                    return Err(EsError::illegal_argument(format!(
                        "Cannot copy to field [{cible}] since it is mapped as an object"
                    )));
                }
                // Une copie ne descend pas dans un `nested` : chez ES elle ne
                // peut aller que vers le document `nested` **courant** ou l'un
                // de ses parents. Ecrire dans un autre element ferait entrer la
                // valeur dans un document qu'elle n'a jamais habite.
                if let Some(racine) = self.racine_nested(cible) {
                    let depuis = self.racine_nested(source);
                    if depuis.map(str::to_string).as_deref() != Some(racine) {
                        return Err(EsError::illegal_argument(format!(
                            "Illegal combination of [copy_to] and [nested] mappings: [copy_to] may \
                             only copy data to the current nested document or any of its parents, \
                             however one [copy_to] directive is trying to copy data from nested \
                             object [{}] to [{racine}]",
                            depuis.unwrap_or("null")
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// La racine `nested` dont ce chemin depend, s'il y en a une.
    fn racine_nested(&self, chemin: &str) -> Option<&str> {
        self.nested
            .iter()
            .rev()
            .find(|r| est_sous_chemin(chemin, r))
            .map(String::as_str)
    }

    /// Ce que le mapping declare **exactement** a ce chemin.
    ///
    /// Les trois issues ne sont pas symetriques et c'est tout leur objet : un
    /// chemin inconnu n'a rien en dessous de lui (par construction, une feuille
    /// declaree plus bas ferait de lui un objet), donc il n'y a rien a verifier
    /// sous lui.
    pub fn forme(&self, chemin: &str) -> Forme<'_> {
        if self.join.as_ref().is_some_and(|j| j.champ == chemin) {
            return Forme::Join;
        }
        if let Some(fm) = self.properties.get(chemin) {
            return Forme::Feuille(fm);
        }
        if self.nested.contains(chemin)
            || self.objets_vides.contains(chemin)
            || self.properties.keys().any(|c| est_sous_chemin(c, chemin))
        {
            return Forme::Objet;
        }
        Forme::Inconnu
    }

    /// Le plus proche **ancetre strict** de ce chemin qui soit une feuille
    /// declaree.
    ///
    /// C'est la question que posent les trois routes qui font entrer un chemin
    /// nouveau dans le mapping — un document, une cible de `copy_to`, un
    /// `PUT /_mapping`. `a.b` sous un `a` de type `keyword` n'est pas un champ
    /// qu'on peut creer : c'est un objet pose sur une valeur, et ES le refuse.
    pub fn ancetre_feuille<'c>(&self, chemin: &'c str) -> Option<(&'c str, &FieldMapping)> {
        let mut reste = chemin;
        while let Some((tete, _)) = reste.rsplit_once('.') {
            if let Some(fm) = self.properties.get(tete) {
                return Some((&chemin[..tete.len()], fm));
            }
            reste = tete;
        }
        None
    }
}

/// Ce que le mapping declare a un chemin donne (voir [`Mapping::forme`]).
pub enum Forme<'a> {
    /// Un champ : une valeur y est attendue, pas un objet.
    Feuille(&'a FieldMapping),
    /// Un objet — declare vide, `nested`, ou porteur d'au moins une feuille.
    Objet,
    /// Le champ `join`, dont la valeur est un objet que le mapping ne decrit
    /// pas champ par champ.
    Join,
    /// Rien de declare ici, ni en dessous.
    Inconnu,
}

/// Le `Preview of field's value` d'Elasticsearch : le `toString` d'une `Map`
/// de Java, dont les cles sortent **triees** (mesure : `{"c":"y","b":"x"}`
/// s'imprime `{b=x, c=y}`).
///
/// Rien n'y est echappe ni cite : une chaine sort nue (`{b=a'b}`), un nombre
/// tel quel, un tableau entre crochets.
fn apercu(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            let elems: Vec<String> = a.iter().map(apercu).collect();
            format!("[{}]", elems.join(", "))
        }
        Value::Object(o) => {
            let mut cles: Vec<&String> = o.keys().collect();
            cles.sort();
            let elems: Vec<String> = cles
                .iter()
                .map(|c| format!("{c}={}", apercu(&o[c.as_str()])))
                .collect();
            format!("{{{}}}", elems.join(", "))
        }
    }
}

/// L'objet qu'une valeur pose sur une feuille, s'il y en a un.
///
/// Un tableau ne compte que par son **premier** objet : c'est celui sur lequel
/// ES bute, et c'est lui qu'il donne en apercu (mesure : `[{"b":"x"},{"b":"y"}]`
/// rend `{b=x}`, et `[1,{"b":"x"}]` aussi).
fn objet_pose(v: &Value) -> Option<&Value> {
    match v {
        Value::Object(_) => Some(v),
        Value::Array(a) => a.iter().find(|e| e.is_object()),
        _ => None,
    }
}

/// « un objet la ou le mapping attend une valeur » — la phrase d'ES, mesuree
/// contre 8.15.0.
///
/// Le prefixe `[ligne:colonne]` qu'ES place devant n'est pas repris : il
/// designe une position dans le JSON brut, que ferrite n'a plus une fois le
/// corps parse. C'est la seule difference, et elle est declaree.
fn objet_sur_feuille(chemin: &str, ty: &str, id: &str, valeur: &Value) -> EsError {
    EsError::mapper_parsing(format!(
        "failed to parse field [{chemin}] of type [{ty}] in document with id '{id}'. Preview of \
         field's value: '{}'",
        apercu(valeur)
    ))
}

/// L'inverse : une valeur concrete la ou le mapping attend un objet.
///
/// `nom` n'est pas toujours `chemin` : dans un tableau, ES n'a plus de nom de
/// champ courant et imprime `[null]` (mesure).
fn valeur_sur_objet(nom: &str, chemin: &str) -> EsError {
    EsError::mapper_parsing(format!(
        "object mapping for [{chemin}] tried to parse field [{nom}] as object, but found a \
         concrete value"
    ))
}

/// L'apercu d'une valeur **copiee** : la cible est reconstruite en objets
/// imbriques depuis l'ancetre qui bloque (mesure : `copy_to: "a.b.c"` sur un
/// `a` de type `keyword` rend `{b={c=x}}`).
fn apercu_copie(reste: &str, v: &Value) -> String {
    match reste.split_once('.') {
        None => format!("{{{reste}={}}}", apercu(v)),
        Some((tete, suite)) => format!("{{{tete}={}}}", apercu_copie(suite, v)),
    }
}

/// Le document tient-il dans la **forme** du mapping ?
///
/// Ce controle est celui qu'ES fait dans son parseur de document, et sa place
/// dans la chaine est mesuree : elle est **avant** `dynamic`. Un `a` de type
/// `keyword` qui recoit `{"b": "x"}` est refuse en 400 que `dynamic` vaille
/// `true`, `false` ou `strict` — le champ `a.b` n'est jamais cree, jamais
/// ignore non plus.
///
/// Sans lui, `a.b` entrait dans le mapping a cote de `a`, et le rendu de
/// `_mapping` — qui repose les chemins pointes en objets — n'avait plus d'objet
/// ou nicher la feuille. Il paniquait, et le processus entier mourait
/// (`panic = "abort"`) : un mapping accepte en 200 puis un seul document, et
/// tous les index du serveur devenaient injoignables.
pub fn verifie_formes(mapping: &Mapping, id: &str, source: &Value) -> EsResult<()> {
    let Some(obj) = source.as_object() else {
        return Ok(());
    };
    for (nom, valeur) in obj {
        descend_formes(mapping, id, nom, valeur)?;
    }
    Ok(())
}

fn descend_formes(mapping: &Mapping, id: &str, chemin: &str, valeur: &Value) -> EsResult<()> {
    match mapping.forme(chemin) {
        Forme::Join | Forme::Inconnu => Ok(()),
        Forme::Feuille(fm) => {
            if let Some(o) = objet_pose(valeur) {
                return Err(objet_sur_feuille(chemin, fm.ty.name(), id, o));
            }
            // Une feuille qui copie ailleurs pose sa valeur **sous** la cible :
            // si un ancetre de la cible est lui-meme une feuille, c'est le meme
            // conflit, et ES le rend avec la meme phrase. Il ne tombe que si la
            // valeur existe : `[]` ne copie rien, `null` si (mesure).
            if !fm.copy_to.is_empty() {
                let copiee = match valeur {
                    Value::Array(a) => a.first(),
                    v => Some(v),
                };
                if let Some(copiee) = copiee {
                    for cible in &fm.copy_to {
                        if let Some((anc, afm)) = mapping.ancetre_feuille(cible) {
                            return Err(EsError::mapper_parsing(format!(
                                "failed to parse field [{anc}] of type [{}] in document with id \
                                 '{id}'. Preview of field's value: '{}'",
                                afm.ty.name(),
                                apercu_copie(&cible[anc.len() + 1..], copiee)
                            )));
                        }
                    }
                }
            }
            Ok(())
        }
        Forme::Objet => match valeur {
            Value::Object(o) => {
                for (nom, v) in o {
                    descend_formes(mapping, id, &joins(chemin, nom), v)?;
                }
                Ok(())
            }
            Value::Array(a) => {
                for v in a {
                    match v {
                        Value::Null => {}
                        Value::Object(_) => descend_formes(mapping, id, chemin, v)?,
                        _ => return Err(valeur_sur_objet("null", chemin)),
                    }
                }
                Ok(())
            }
            // Un objet absent n'est pas un objet mal forme : ES indexe.
            Value::Null => Ok(()),
            _ => Err(valeur_sur_objet(chemin, chemin)),
        },
    }
}

/// Les champs de **metadonnees** d'Elasticsearch : les redeclarer est une
/// erreur, chez lui comme ici, et il la dit avec cette phrase-la.
///
/// Ce n'est **pas** « tout nom qui commence par `_` » : `_score`, `_doc`,
/// `_type`, `_size`, `_all` et `_parent` passent chez ES, mesure a l'appui.
/// Le prefixe entier a longtemps ete refuse ici, et ca bloquait une application
/// reelle — Wagtail nomme ses champs `_all_text` et `_edgengrams` — sur un nom
/// qu'un vrai ES accepte.
const METADONNEES: &[&str] = &[
    "_id",
    "_index",
    "_source",
    "_routing",
    "_field_names",
    "_ignored",
    "_seq_no",
    "_version",
    "_nested_path",
    "_feature",
    "_data_stream_timestamp",
    "_tier",
];

/// Les noms que **ferrite** se reserve en plus : ce sont ceux de ses colonnes
/// internes, et un champ utilisateur qui les porterait ecraserait la colonne
/// jumelle d'un `nested` ou le lien parent d'un `join`.
///
/// ES les accepte ; ferrite les refuse **explicitement**, avec sa raison. Un
/// nom qui n'y est pas ne peut pas entrer en collision : les colonnes internes
/// sont `_elem.{chemin}`, `_nelem.{chemin}`, `_store.{chemin}` et
/// `_join_parent`.
const INTERNES: &[&str] = &["_elem", "_nelem", "_join_parent", "_store"];

/// Refuse un nom de champ reserve — par ES, ou par ferrite.
pub fn nom_reserve(chemin: &str) -> EsResult<()> {
    let racine = chemin.split('.').next().unwrap_or(chemin);
    if METADONNEES.contains(&chemin) {
        return Err(EsError::mapper_parsing(format!(
            "Field [{chemin}] is defined more than once"
        )));
    }
    if INTERNES.contains(&racine) {
        return Err(EsError::mapper_parsing(format!(
            "[{chemin}] : le nom [{racine}] est celui d'une colonne interne de ferrite (l'indice \
             d'element d'un [nested], le lien parent d'un [join]) ; un champ qui le porterait \
             l'ecraserait"
        )));
    }
    Ok(())
}

/// Parse une propriete du mapping, qui peut etre un champ ou un sous-objet.
///
/// Un sous-objet ne produit aucune entree pour lui-meme : il n'existe que par
/// les chemins de ses feuilles (`fournisseur` -> `fournisseur.nom`,
/// `fournisseur.pays`). C'est exactement le modele d'Elasticsearch, ou un objet
/// n'est pas un champ indexable.
fn parse_propriete(
    chemin: &str,
    spec: &Value,
    dans: &mut BTreeMap<String, FieldMapping>,
    nested: &mut BTreeSet<String>,
    vides: &mut BTreeSet<String>,
    declares: &Analysis,
) -> EsResult<()> {
    for part in chemin.split('.') {
        validate_field_name_part(part, chemin)?;
    }
    nom_reserve(chemin)?;

    let obj = spec.as_object().ok_or_else(|| {
        EsError::mapper_parsing(format!("[mappings.properties.{chemin}] doit etre un objet"))
    })?;

    // Un objet se reconnait a son `properties`. ES accepte les deux ecritures :
    // avec ou sans `"type": "object"`.
    if let Some(sous) = obj.get("properties") {
        let ty = obj.get("type").and_then(Value::as_str);
        if !matches!(ty, None | Some("object") | Some("nested")) {
            return Err(EsError::mapper_parsing(format!(
                "[{chemin}] : [properties] n'a pas de sens sur un champ de type [{}]",
                ty.unwrap_or("?")
            )));
        }
        if ty == Some("nested") {
            // Un `nested` dans un `nested` demanderait un indice d'element par
            // niveau : refus explicite plutot qu'une correlation fausse.
            if let Some(parent) = nested.iter().find(|r| est_sous_chemin(chemin, r)) {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas un [nested] dans un autre [nested] (champ \
                     [{chemin}], deja sous [{parent}])"
                )));
            }
            nested.insert(chemin.to_string());
        }
        for autre in obj.keys() {
            if autre != "properties" && autre != "type" && autre != "dynamic" {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas le parametre [{autre}] sur l'objet [{chemin}] ; \
                     parametres acceptes : type, properties"
                )));
            }
        }
        if obj.contains_key("dynamic") {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [dynamic] par objet (champ [{chemin}]) ; il se declare \
                 au niveau du mapping"
            )));
        }
        let sous = sous.as_object().ok_or_else(|| {
            EsError::mapper_parsing(format!("[{chemin}.properties] doit etre un objet"))
        })?;
        if sous.is_empty() {
            // Un objet sans sous-champ ne declare rien — ES l'accepte, et ses
            // champs viendront des documents. On le memorise quand meme pour
            // pouvoir le rendre dans `_mapping`.
            vides.insert(chemin.to_string());
            return Ok(());
        }
        for (nom, decl) in sous {
            parse_propriete(
                &format!("{chemin}.{nom}"),
                decl,
                dans,
                nested,
                vides,
                declares,
            )?;
        }
        return Ok(());
    }
    if matches!(
        obj.get("type").and_then(Value::as_str),
        Some("object") | Some("nested")
    ) {
        // Un objet n'est pas un champ : il n'entre pas dans l'index inverse, il
        // n'existe que par ses feuilles. ES refuse donc `index` la (mesure :
        // « Mapping definition for [o] has unsupported parameters: [index :
        // false] »). L'accepter sans effet laisserait croire que les feuilles
        // en heritent.
        if obj.contains_key("index") {
            return Err(EsError::mapper_declaration(format!(
                "Mapping definition for [{chemin}] has unsupported parameters: [index] : un \
                 [{}] n'entre pas dans l'index inverse, seules ses feuilles y entrent",
                obj.get("type").and_then(Value::as_str).unwrap_or("object")
            )));
        }
        vides.insert(chemin.to_string());
        return Ok(());
    }

    let fm = parse_field_mapping(chemin, spec, false, declares)?;
    // `a` feuille et `a.b` objet ne peuvent pas coexister — ES refuse aussi.
    conflit_de_chemin(chemin, fm.ty, dans)?;
    dans.insert(chemin.to_string(), fm);
    Ok(())
}

/// Un chemin ne doit etre ni le prefixe, ni le prolongement d'un autre.
///
/// La phrase est celle d'ES, mesuree contre 8.15.0 : la declaration nomme
/// toujours la **feuille** et le type qu'elle porte, quel que soit l'ordre dans
/// lequel les deux champs sont ecrits (`{"a": keyword, "a.b": keyword}` et son
/// inverse rendent le meme message).
fn conflit_de_chemin(
    chemin: &str,
    ty: FieldType,
    dans: &BTreeMap<String, FieldMapping>,
) -> EsResult<()> {
    for existant in dans.keys() {
        let (court, long) = if existant.len() < chemin.len() {
            (existant.as_str(), chemin)
        } else {
            (chemin, existant.as_str())
        };
        if long.strip_prefix(court).is_some_and(|r| r.starts_with('.')) {
            let ty = if court == chemin { ty } else { dans[court].ty };
            return Err(EsError::mapper_declaration(format!(
                "Failed to parse mapping: mapper [{court}] cannot be changed from type [{}] to \
                 [ObjectMapper]",
                ty.name()
            )));
        }
    }
    Ok(())
}

fn as_obj<'a>(v: &'a Value, quoi: &str) -> EsResult<&'a Map<String, Value>> {
    v.as_object()
        .ok_or_else(|| EsError::mapper_parsing(format!("[{quoi}] doit etre un objet")))
}

/// `chemin` est-il sous `racine` (strictement) ?
pub fn est_sous_chemin(chemin: &str, racine: &str) -> bool {
    chemin
        .strip_prefix(racine)
        .is_some_and(|reste| reste.starts_with('.'))
}

/// Retrouve le noeud `{"properties": {...}}` d'un chemin dans un arbre rendu.
fn pointe_mut<'a>(props: &'a mut Map<String, Value>, chemin: &str) -> Option<&'a mut Value> {
    match chemin.split_once('.') {
        None => props.get_mut(chemin),
        Some((tete, reste)) => {
            let sous = props
                .get_mut(tete)?
                .as_object_mut()?
                .get_mut("properties")?
                .as_object_mut()?;
            pointe_mut(sous, reste)
        }
    }
}

/// Repose un chemin pointe dans l'arbre `properties` d'une reponse `_mapping`.
///
/// Un prefixe de chemin **doit** etre un objet : `a` feuille et `a.b` feuille
/// ne peuvent pas coexister, et trois controles l'interdisent ([`verifie_formes`]
/// a l'ecriture, [`conflit_de_chemin`] a la declaration, la fusion de
/// `add_fields`). Cette fonction ne s'en sert pas comme d'une garantie pour
/// autant : elle a paniqué ici pendant toute la vie du projet, et un rendu de
/// `_mapping` n'est pas l'endroit ou l'on decide qu'un serveur doit mourir. Si
/// l'invariant tombe malgre les trois controles, la feuille est reposee sous
/// son nom pointe — visible dans la reponse, donc diagnosticable, plutot que
/// silencieusement perdue.
fn niche(props: &mut Map<String, Value>, chemin: &str, feuille: Value) {
    match chemin.split_once('.') {
        None => {
            props.insert(chemin.to_string(), feuille);
        }
        Some((tete, reste)) => {
            let entree = props
                .entry(tete.to_string())
                .or_insert_with(|| json!({"properties": {}}));
            match entree
                .as_object_mut()
                .and_then(|o| o.get_mut("properties"))
                .and_then(Value::as_object_mut)
            {
                Some(sous) => niche(sous, reste, feuille),
                None => {
                    props.insert(chemin.to_string(), feuille);
                }
            }
        }
    }
}

/// Parse la declaration d'un champ.
///
/// Lit la valeur d'un `index` de mapping : `Some(true)`, `Some(false)`, ou
/// `None` si la valeur n'est ni l'un ni l'autre.
///
/// Elasticsearch accepte les deux ecritures — le booleen et la chaine — et
/// refuse tout le reste (`"no"`, `1`, `null`) par un `mapper_parsing_exception`
/// « only [true] or [false] are allowed » (mesure contre 8.15.0).
/// Une valeur telle qu'ES la recopie dans son message : une chaine y figure
/// **sans** ses guillemets (`[oui]`, pas `["oui"]`).
fn brut(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        autre => autre.to_string(),
    }
}

fn index_demande(val: &Value) -> Option<bool> {
    match val {
        Value::Bool(b) => Some(*b),
        Value::String(s) if s == "true" => Some(true),
        Value::String(s) if s == "false" => Some(false),
        _ => None,
    }
}

/// `sous_champ` indique qu'on est deja dans un `fields` : ES n'autorise qu'un
/// seul niveau de multi-fields, et ferrite refuse le second explicitement.
fn parse_field_mapping(
    name: &str,
    spec: &Value,
    sous_champ: bool,
    declares: &Analysis,
) -> EsResult<FieldMapping> {
    let obj = spec.as_object().ok_or_else(|| {
        EsError::mapper_parsing(format!("[mappings.properties.{name}] doit etre un objet"))
    })?;

    let mut ty = None;
    let mut fields = BTreeMap::new();
    let mut ignore_above = None;
    let mut analyzer = None;
    let mut search_analyzer = None;
    let mut format = None;
    let mut copy_to = Vec::new();
    let mut store = false;
    let mut indexe = true;

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
                        parse_field_mapping(
                            &format!("{name}.{sub_name}"),
                            sub_spec,
                            true,
                            declares,
                        )?,
                    );
                }
            }
            // `default` n'est pas un analyzer, c'est le **nom** de celui de
            // l'index : ES le rend tel quel dans `_mapping` des qu'un champ
            // declare un `search_analyzer` sans analyzer d'indexation. Le lire
            // comme « aucun analyzer declare » est ce qui rend le mapping
            // stable a la relecture — sans quoi un redemarrage transformerait
            // le `default` qu'ES ecrit en `standard`, que personne n'a demande.
            "analyzer" => {
                let nom = val.as_str().ok_or_else(|| {
                    EsError::mapper_parsing(format!("[{name}.analyzer] doit etre une chaine"))
                })?;
                analyzer = match nom {
                    "default" if declares.index_de(nom).is_none() => None,
                    _ => Some(analysis::parse_declaration(nom, name, declares)?),
                };
            }
            "search_analyzer" => {
                let nom = val.as_str().ok_or_else(|| {
                    EsError::mapper_parsing(format!(
                        "[{name}.search_analyzer] doit etre une chaine"
                    ))
                })?;
                search_analyzer = Some(analysis::parse_declaration(nom, name, declares)?);
            }
            // Un multi-field ne peut pas etre la **source** d'une copie : ES le
            // refuse avec cette phrase-la, et la mesure la lui prend mot pour
            // mot (`[copy_to] may not be used to copy from a multi-field`).
            "copy_to" => {
                if sous_champ {
                    return Err(EsError::illegal_argument(format!(
                        "[copy_to] may not be used to copy from a multi-field: [{name}]"
                    )));
                }
                copy_to = lire_copy_to(name, val)?;
            }
            // Comme `index: true`, `store: false` est le defaut d'ES : il ne
            // demande rien, et ES ne le conserve meme pas dans le mapping qu'il
            // rend. Une valeur qui n'est ni l'un ni l'autre est refusee avec sa
            // phrase.
            "store" => match index_demande(val) {
                Some(b) => store = b,
                None => {
                    return Err(EsError::mapper_declaration(format!(
                        "Failed to parse value [{}] as only [true] or [false] are allowed.",
                        brut(val)
                    )))
                }
            },
            "format" => {
                let motif = val.as_str().ok_or_else(|| {
                    EsError::mapper_parsing(format!("[{name}.format] doit etre une chaine"))
                })?;
                format = Some(DateFormat::parse(motif)?);
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
            // `index: true` est le **defaut** d'Elasticsearch : ES lui-meme ne
            // le garde pas — un `GET /{index}/_mapping` sur un champ pose avec
            // `index: true` rend `{"type": "keyword"}` tout court, la ou il
            // conserve `index: false` (mesure contre 8.15.0). Le premier ne
            // demande donc rien, le second demande tout : voir
            // [`FieldMapping::indexe`].
            "index" => match index_demande(val) {
                Some(b) => indexe = b,
                None => {
                    return Err(EsError::mapper_declaration(format!(
                        "Failed to parse value [{}] as only [true] or [false] are allowed. \
                         (champ [{name}])",
                        brut(val)
                    )))
                }
            },
            other => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas le parametre de champ [{other}] (champ [{name}]) ; \
                     parametres acceptes : type, analyzer, search_analyzer, fields, ignore_above, \
                     format, index, copy_to, store"
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
    // ES ne connait meme pas le parametre ailleurs que sur un `text` : sa
    // phrase est celle d'un parametre inconnu, pas celle d'un mauvais type.
    if search_analyzer.is_some() && ty.kind() != FieldKind::Text {
        return Err(EsError::mapper_declaration(format!(
            "unknown parameter [search_analyzer] on mapper [{name}] of type [{}]",
            ty.name()
        )));
    }
    Ok(FieldMapping {
        ty,
        analyzer,
        search_analyzer,
        fields,
        ignore_above,
        format,
        copy_to,
        store,
        indexe,
    })
}

/// Lit `copy_to` : une cible, ou une liste de cibles.
///
/// ES accepte un nombre comme un nom de champ (il le rend en chaine :
/// `copy_to: 42` ressort `["42"]`), et une liste **vide** ne demande rien — il
/// ne la conserve pas. Les deux sont repris tels quels : etre plus severe que
/// lui ne protegerait rien.
fn lire_copy_to(name: &str, val: &Value) -> EsResult<Vec<String>> {
    let items: Vec<&Value> = match val {
        Value::Array(a) => a.iter().collect(),
        autre => vec![autre],
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::String(s) => out.push(s.clone()),
            Value::Number(n) => out.push(n.to_string()),
            autre => {
                return Err(EsError::mapper_parsing(format!(
                    "[{name}.copy_to] : nom de champ attendu, recu {autre}"
                )))
            }
        }
    }
    Ok(out)
}

/// Parcourt un document en profondeur et appelle `f` sur chaque **feuille**,
/// avec son chemin pointe.
///
/// C'est la traduction, cote document, du choix fait cote mapping : un objet
/// n'est pas une valeur indexable, ses feuilles le sont.
/// `{"client": {"ville": "Lyon"}}` appelle `f("client.ville", "Lyon")`.
///
/// Un tableau d'objets est **aplati**, comme chez Elasticsearch :
/// `{"l": [{"ref": "A"}, {"ref": "B"}]}` appelle `f("l.ref", "A")` puis
/// `f("l.ref", "B")` — le champ devient multivalue et la correspondance entre
/// sous-champs d'un meme element est perdue. C'est precisement ce que le type
/// `nested` existe pour conserver.
pub fn parcours_feuilles(
    obj: &Map<String, Value>,
    f: &mut impl FnMut(&str, &Value) -> EsResult<()>,
) -> EsResult<()> {
    parcours_feuilles_nested(obj, &BTreeSet::new(), &mut |chemin, valeur, _| {
        f(chemin, valeur)
    })
}

/// Meme parcours, mais en suivant les elements des tableaux `nested`.
///
/// Le troisieme argument du rappel est l'indice de l'element du tableau
/// `nested` le plus proche — `None` hors de tout `nested`. C'est **la** donnee
/// qui distingue `nested` d'`object` : elle permet de savoir, plus tard, que
/// `l.ref = "A"` et `l.qte = 5` venaient du meme element.
///
/// Le rappel `element` est appele une fois par racine `nested` rencontree, avec
/// le nombre d'elements du tableau.
pub fn parcours_nested<'a>(
    obj: &'a Map<String, Value>,
    nested: &BTreeSet<String>,
    f: &mut impl FnMut(&str, &'a Value, Option<u32>) -> EsResult<()>,
    cardinal: &mut impl FnMut(&str, u32) -> EsResult<()>,
) -> EsResult<()> {
    parcours_feuilles_nested_avec(obj, nested, f, &mut Some(cardinal))
}

fn parcours_feuilles_nested<'a>(
    obj: &'a Map<String, Value>,
    nested: &BTreeSet<String>,
    f: &mut impl FnMut(&str, &'a Value, Option<u32>) -> EsResult<()>,
) -> EsResult<()> {
    parcours_feuilles_nested_avec(
        obj,
        nested,
        f,
        &mut None::<&mut dyn FnMut(&str, u32) -> EsResult<()>>,
    )
}

fn parcours_feuilles_nested_avec<'a>(
    obj: &'a Map<String, Value>,
    nested: &BTreeSet<String>,
    f: &mut impl FnMut(&str, &'a Value, Option<u32>) -> EsResult<()>,
    cardinal: &mut Option<impl FnMut(&str, u32) -> EsResult<()>>,
) -> EsResult<()> {
    fn descend<'a>(
        chemin: &str,
        valeur: &'a Value,
        elem: Option<u32>,
        nested: &BTreeSet<String>,
        f: &mut impl FnMut(&str, &'a Value, Option<u32>) -> EsResult<()>,
        cardinal: &mut Option<impl FnMut(&str, u32) -> EsResult<()>>,
    ) -> EsResult<()> {
        // Une racine `nested` : ses elements sont numerotes, et c'est ce
        // numero que porteront toutes les valeurs qui en descendent.
        if nested.contains(chemin) {
            let elements: Vec<&Value> = match valeur {
                Value::Array(a) => a.iter().filter(|v| !v.is_null()).collect(),
                Value::Null => return Ok(()),
                v => vec![v],
            };
            if let Some(c) = cardinal.as_mut() {
                c(chemin, elements.len() as u32)?;
            }
            for (i, element) in elements.iter().enumerate() {
                let o = element.as_object().ok_or_else(|| {
                    EsError::mapper_parsing(format!(
                        "[{chemin}] est declare [nested] : ses elements doivent etre des objets"
                    ))
                })?;
                for (nom, v) in o {
                    descend(&joins(chemin, nom), v, Some(i as u32), nested, f, cardinal)?;
                }
            }
            return Ok(());
        }

        match valeur {
            Value::Object(o) => {
                for (nom, v) in o {
                    descend(&joins(chemin, nom), v, elem, nested, f, cardinal)?;
                }
                Ok(())
            }
            Value::Array(a) if a.iter().any(Value::is_object) => {
                if a.iter().any(|v| !v.is_object() && !v.is_null()) {
                    return Err(EsError::mapper_parsing(format!(
                        "[{chemin}] melange des objets et des valeurs dans le meme tableau"
                    )));
                }
                for v in a {
                    if !v.is_null() {
                        descend(chemin, v, elem, nested, f, cardinal)?;
                    }
                }
                Ok(())
            }
            v => f(chemin, v, elem),
        }
    }

    for (nom, valeur) in obj {
        descend(nom, valeur, None, nested, f, cardinal)?;
    }
    Ok(())
}

fn joins(prefixe: &str, nom: &str) -> String {
    if prefixe.is_empty() {
        nom.to_string()
    } else {
        format!("{prefixe}.{nom}")
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
                    ignore_above: Some(256),
                    ..FieldMapping::new(FieldType::Keyword)
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

/// Un champ du schema tantivy, resolu.
#[derive(Debug, Clone, Copy)]
pub struct MappedField {
    pub field: Field,
    pub ty: FieldType,
    pub ignore_above: Option<usize>,
    /// L'analyzer applique **a l'indexation** — celui du schema tantivy, et
    /// celui que `_analyze` sur un champ rejoue (mesure contre ES 8.15 :
    /// `_analyze` avec `field` ignore le `search_analyzer`).
    pub analyzer: Analyzer,
    /// L'analyzer applique **a la requete**. Egal au precedent tant qu'aucun
    /// `search_analyzer` n'est declare.
    pub search_analyzer: Analyzer,
    /// `store: true` : la valeur est conservee a part, et `stored_fields` la
    /// rend.
    pub store: bool,
    /// `index` du mapping. A `false`, le champ n'est pas dans l'index inverse :
    /// une clause qui le vise se lit sur sa colonne (voir
    /// [`crate::colonne`]), et un `text`, qui n'en a pas, n'est plus
    /// cherchable du tout.
    pub indexe: bool,
    /// Sous un `nested` : la colonne jumelle qui dit, pour chaque valeur
    /// indexee ici, de quel element du tableau elle vient. Meme arite, par
    /// construction — elle est alimentee dans la meme boucle.
    pub elem: Option<Field>,
    /// Pour un champ **numerique** declare `store: true` : le champ jumeau qui
    /// garde la valeur dans l'ordre du document, la colonne etant triee. Voir
    /// [`stocke_seul`]. `None` pour un champ textuel, dont l'ordre ne bouge
    /// pas — c'est alors `field` lui-meme qui porte le stockage.
    pub stocke: Option<Field>,
}

/// Prefixe des colonnes internes qui portent l'indice d'element d'un `nested`.
/// Un champ utilisateur ne peut pas commencer par `_`, donc pas de collision.
pub const P_ELEM: &str = "_elem.";
/// Prefixe de la colonne qui compte les elements d'un `nested`, par document.
pub const P_NELEM: &str = "_nelem.";
/// La colonne qui porte l'identifiant du parent, pour un document enfant.
pub const F_JOIN_PARENT: &str = "_join_parent";
/// Prefixe des champs jumeaux qui gardent un numerique `store: true` dans
/// l'ordre du document.
pub const P_STORE: &str = "_store.";

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
    /// Les racines `nested` declarees.
    pub nested: BTreeSet<String>,
    /// Par racine `nested` : le nombre d'elements du document.
    pub nelem: BTreeMap<String, Field>,
    /// Le `format` declare des champs `date`, par chemin. Il sert a la lecture
    /// (indexation, bornes d'un `range`) comme au rendu (`*_as_string`).
    pub formats: BTreeMap<String, DateFormat>,
    /// `copy_to`, resolu : par propriete de premier niveau, les chemins ou sa
    /// valeur brute part **en plus** a l'indexation.
    pub copies: BTreeMap<String, Vec<String>>,
    /// L'inverse : par chemin de cible, les chemins qui y copient, tries par
    /// nom. C'est l'ordre dans lequel ES rend les valeurs copiees dans
    /// `fields` — la valeur propre de la cible d'abord, puis les copies par
    /// ordre de nom de source (mesure contre 8.15).
    pub copiants: BTreeMap<String, Vec<String>>,
    /// Le champ `join` declare, et ses deux colonnes : le nom de la relation
    /// (interrogeable comme un `keyword`, sous le nom du champ) et
    /// l'identifiant du parent.
    pub join: Option<Join>,
    pub join_name: Option<Field>,
    pub join_parent: Option<Field>,
}

impl Fields {
    pub fn get(&self, name: &str) -> Option<MappedField> {
        self.mapped.get(name).copied()
    }

    pub fn targets_of(&self, name: &str) -> Option<&[MappedField]> {
        self.targets.get(name).map(Vec::as_slice)
    }

    /// Le format de date d'un champ, s'il en declare un.
    pub fn format_de(&self, chemin: &str) -> Option<&DateFormat> {
        self.formats.get(chemin)
    }

    /// Le format de date d'un champ, ou celui d'ES par defaut.
    ///
    /// Une borne de requete a toujours besoin d'un format : c'est lui qui lit
    /// l'ancre d'une expression `2026-03-15||+1d` et qui dit quelle periode
    /// couvre une date partielle.
    pub fn format_ou_defaut(&self, chemin: &str) -> &DateFormat {
        match self.formats.get(chemin) {
            Some(f) => f,
            None => format_par_defaut(),
        }
    }

    /// La racine `nested` dont ce chemin depend, s'il y en a une.
    pub fn racine_nested(&self, chemin: &str) -> Option<&str> {
        self.nested
            .iter()
            .rev()
            .find(|r| est_sous_chemin(chemin, r))
            .map(String::as_str)
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

    let mut mapped: BTreeMap<String, MappedField> = BTreeMap::new();
    let mut targets: BTreeMap<String, Vec<MappedField>> = BTreeMap::new();
    let mut nelem = BTreeMap::new();
    let mut formats = BTreeMap::new();
    let mut copies: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut copiants: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let (mut join_name, mut join_parent) = (None, None);
    if let Some(j) = &mapping.join {
        let f = add_field(
            &mut b,
            &j.champ,
            FieldType::Keyword,
            Analyzer::default(),
            false,
            true,
        );
        // Interrogeable comme un `keyword` sous son propre nom — c'est ce que
        // fait ES : `{"term": {"lien": "article"}}` filtre sur la relation.
        // Present dans `mapped` (donc dans les requetes) mais pas dans
        // `targets` : sa valeur ne s'indexe pas comme un champ ordinaire.
        mapped.insert(
            j.champ.clone(),
            MappedField {
                field: f,
                ty: FieldType::Keyword,
                ignore_above: None,
                analyzer: Analyzer::default(),
                search_analyzer: Analyzer::default(),
                store: false,
                indexe: true,
                elem: None,
                stocke: None,
            },
        );
        join_name = Some(f);
        join_parent = Some(add_field(
            &mut b,
            F_JOIN_PARENT,
            FieldType::Keyword,
            Analyzer::default(),
            false,
            true,
        ));
    }
    for racine in &mapping.nested {
        nelem.insert(
            racine.clone(),
            b.add_u64_field(&format!("{P_NELEM}{racine}"), NumericOptions::from(FAST)),
        );
    }

    for (name, fm) in &mapping.properties {
        if !fm.copy_to.is_empty() {
            copies.insert(name.clone(), fm.copy_to.clone());
            for cible in &fm.copy_to {
                copiants
                    .entry(cible.clone())
                    .or_default()
                    .push(name.clone());
            }
        }
        let mut cibles = Vec::with_capacity(1 + fm.fields.len());
        for (chemin, decl) in std::iter::once((name.clone(), fm)).chain(
            fm.fields
                .iter()
                .map(|(sub, decl)| (format!("{name}.{sub}"), decl)),
        ) {
            // Un champ stocke sous un `nested` ne rend rien chez ES : ses
            // valeurs vivent dans les documents enfants, que `stored_fields` ne
            // lit pas. Ne pas le stocker du tout evite d'avoir a s'en souvenir
            // au moment de rendre.
            let sous_nested = mapping.nested.iter().any(|r| est_sous_chemin(&chemin, r));
            let store = decl.store && !sous_nested;
            // Un numerique stocke prend un champ jumeau : sa colonne est triee,
            // sa valeur stockee ne l'est pas. `field` ne porte donc plus le
            // stockage dans ce cas-la.
            let stocke = store
                .then(|| stocke_seul(&mut b, &chemin, decl.ty))
                .flatten();
            let entry = MappedField {
                field: add_field(
                    &mut b,
                    &chemin,
                    decl.ty,
                    decl.analyzer(),
                    store && stocke.is_none(),
                    decl.indexe,
                ),
                stocke,
                ty: decl.ty,
                ignore_above: decl.ignore_above,
                analyzer: decl.analyzer(),
                search_analyzer: decl.search_analyzer(),
                store,
                indexe: decl.indexe,
                elem: mapping
                    .nested
                    .iter()
                    .any(|r| est_sous_chemin(&chemin, r))
                    .then(|| {
                        b.add_u64_field(&format!("{P_ELEM}{chemin}"), NumericOptions::from(FAST))
                    }),
            };
            if let Some(f) = &decl.format {
                formats.insert(chemin.clone(), f.clone());
            }
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
            nested: mapping.nested.clone(),
            nelem,
            formats,
            copies,
            copiants,
            join: mapping.join.clone(),
            join_name,
            join_parent,
        },
    )
}

/// Ajoute un champ au schema tantivy.
///
/// `indexe` est le `index` du mapping. A `false`, le champ perd ses options
/// d'**indexation** et **garde tout le reste** : c'est exactement ce que fait
/// Lucene, et c'est la raison pour laquelle un `keyword` non indexe reste
/// cherchable chez ES (par sa colonne) alors qu'un `text` ne l'est plus (il
/// n'en a pas).
fn add_field(
    b: &mut SchemaBuilder,
    name: &str,
    ty: FieldType,
    analyzer: Analyzer,
    store: bool,
    indexe: bool,
) -> Field {
    match ty.kind() {
        FieldKind::Text => {
            // Un `text` non indexe n'a ni index inverse ni colonne : il ne
            // reste que le stockage, s'il est demande. Le champ existe quand
            // meme dans le schema pour que le mapping, `fields` et les refus
            // sachent de quoi ils parlent.
            let mut opts = TextOptions::default();
            if indexe {
                opts = opts.set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer(&analyzer.tokenizer())
                        .set_index_option(IndexRecordOption::WithFreqsAndPositions),
                );
            }
            if store {
                opts = opts.set_stored();
            }
            b.add_text_field(name, opts)
        }
        FieldKind::Keyword => {
            // `fast` pour pouvoir trier dessus ; `raw` pour que le terme soit
            // la valeur entiere, comme un keyword ES.
            //
            // `set_fieldnorms(false)` parce qu'un `keyword` d'ES est declare
            // `norms: false` : son score ne depend pas du nombre de valeurs du
            // champ. Avec les fieldnorms, un `term` sur un champ a trois
            // valeurs marquait moins qu'un champ a une seule — ES donne le meme
            // score aux deux, et le classement en dependait. Mesure par
            // `tests/compat/fuzz_vs_es.py`.
            let mut opts = TextOptions::default().set_fast(Some(RAW_TOKENIZER));
            if indexe {
                opts = opts.set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer(RAW_TOKENIZER)
                        .set_index_option(IndexRecordOption::Basic)
                        .set_fieldnorms(false),
                );
            }
            if store {
                opts = opts.set_stored();
            }
            b.add_text_field(name, opts)
        }
        FieldKind::I64 => b.add_i64_field(name, numerique(store, indexe)),
        FieldKind::F64 => b.add_f64_field(name, numerique(store, indexe)),
        // Sans fieldnorm : chez Lucene un `boolean` est indexe `omitNorms`, donc
        // deux documents qui portent `true` marquent pareil, que le champ ait
        // une valeur ou trois. Avec les fieldnorms, ferrite les departageait —
        // et le classement changeait. Mesure par `tests/compat/fuzz_vs_es.py`.
        // `FAST` puis `set_indexed()` plutot que `INDEXED | FAST` : c'est le
        // seul chemin qui laisse les fieldnorms a `false` (le drapeau `INDEXED`
        // les allume).
        FieldKind::Bool => {
            let mut opts = NumericOptions::from(FAST);
            if indexe {
                opts = opts.set_indexed();
            }
            if store {
                opts = opts.set_stored();
            }
            b.add_bool_field(name, opts)
        }
        FieldKind::Date => {
            let mut opts = if indexe {
                DateOptions::from(INDEXED | FAST)
            } else {
                DateOptions::from(FAST)
            }
            .set_precision(DateTimePrecision::Milliseconds);
            if store {
                opts = opts.set_stored();
            }
            b.add_date_field(name, opts)
        }
    }
}

/// Le champ jumeau qui garde un numerique **dans l'ordre du document**.
///
/// Chez Lucene, un champ `store: true` et sa colonne sont deux structures
/// distinctes : la colonne est triee par document
/// (`SortedNumericDocValues`), le champ stocke garde l'ordre d'ecriture. ferrite
/// les confondait dans un seul champ tantivy, ce qui n'a rien coute tant que
/// l'ordre de la colonne etait celui du document — il ne l'est plus (voir
/// `crate::engine::pose`). Un champ numerique declare `store: true` a donc deux
/// champs : `{chemin}` porte la colonne triee, `_store.{chemin}` la valeur
/// stockee, non indexee et sans colonne.
///
/// Les champs textuels n'en ont pas besoin : rien ne reordonne leurs valeurs.
fn stocke_seul(b: &mut SchemaBuilder, name: &str, ty: FieldType) -> Option<Field> {
    let nom = format!("{P_STORE}{name}");
    Some(match ty.kind() {
        FieldKind::Text | FieldKind::Keyword => return None,
        FieldKind::I64 => b.add_i64_field(&nom, NumericOptions::from(STORED)),
        FieldKind::F64 => b.add_f64_field(&nom, NumericOptions::from(STORED)),
        FieldKind::Bool => b.add_bool_field(&nom, NumericOptions::from(STORED)),
        FieldKind::Date => b.add_date_field(
            &nom,
            DateOptions::from(STORED).set_precision(DateTimePrecision::Milliseconds),
        ),
    })
}

/// Les options d'une colonne numerique, avec ou sans stockage a part, et avec
/// ou sans index inverse (`index: false`).
fn numerique(store: bool, indexe: bool) -> NumericOptions {
    let opts = if indexe {
        NumericOptions::from(INDEXED | FAST)
    } else {
        NumericOptions::from(FAST)
    };
    if store {
        opts.set_stored()
    } else {
        opts
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
    coerce_avec(field, ty, v, None)
}

/// Comme [`coerce`], avec le `format` declare du champ pour les dates.
pub fn coerce_avec(
    field: &str,
    ty: FieldType,
    v: &Value,
    format: Option<&DateFormat>,
) -> EsResult<TypedValue> {
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
            // Un `float` d'ES tient sur 32 bits : `1e308` y deborde, et ES
            // refuse le document en 400. ferrite l'acceptait en 201 et le
            // gardait en `f64` — donc un `_mapping` qui annonce `float` et une
            // valeur qu'aucun float ne represente, rendue telle quelle. Les
            // bornes des types **entiers** etaient deja verifiees juste
            // au-dessus ; celle-ci manquait. Trouvee en posant au fuzzer des
            // valeurs numeriques extremes.
            if ty == FieldType::Float && f.is_finite() && (f as f32).is_infinite() {
                return Err(EsError::mapper_parsing(format!(
                    "failed to parse field [{field}] of type [float] : {f} est hors des bornes \
                     d'un flottant 32 bits"
                )));
            }
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
        FieldKind::Date => Ok(TypedValue::Date(match format {
            Some(f) => f.lit(field, v)?,
            None => parse_date(field, v)?,
        })),
    }
}

/// `strict_date_optional_time||epoch_millis`, le format par defaut d'ES.
///
/// Un champ `date` sans `format` declare est lu par ce format-la, et par le
/// meme code que les autres : il y a eu deux lecteurs ISO dans ce fichier et
/// dans [`crate::dateformat`], et deux lecteurs finissent toujours par diverger
/// — celui-ci n'acceptait ni `2026-03` ni `2026-03-15T12`, qu'ES accepte.
pub fn format_par_defaut() -> &'static DateFormat {
    static DEFAUT: std::sync::LazyLock<DateFormat> = std::sync::LazyLock::new(DateFormat::default);
    &DEFAUT
}

fn parse_date(field: &str, v: &Value) -> EsResult<i64> {
    format_par_defaut().lit(field, v)
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
    fn index_vrai_est_le_defaut_et_ne_ressort_pas() {
        // Ce que Gitea pose sur chacun de ses champs. ES accepte, et ne garde
        // pas le parametre : le mapping relu doit etre celui du champ nu.
        let m = mapping(
            r#"{"properties":{"id":{"type":"integer","index":true},
                "title":{"type":"text","index":"true"}}}"#,
        )
        .unwrap();
        assert_eq!(m.get("id").unwrap().ty, FieldType::Integer);
        assert_eq!(m.get("title").unwrap().ty, FieldType::Text);
        let rendu = serde_json::to_string(&m.to_json()).unwrap();
        assert!(!rendu.contains("index"), "rendu inattendu : {rendu}");
    }

    #[test]
    fn index_faux_est_lu_et_rendu() {
        // Les deux ecritures d'ES, le booleen et la chaine. Et ce qui compte
        // autant : `index: false` **ressort** du mapping, la ou `index: true`
        // n'y laisse rien (mesure contre 8.15). Sans ca, un outil qui relit son
        // mapping pour decider s'il doit reindexer y lirait le contraire de ce
        // qu'il a pose.
        for corps in [
            r#"{"properties":{"k":{"type":"keyword","index":false}}}"#,
            r#"{"properties":{"k":{"type":"keyword","index":"false"}}}"#,
        ] {
            let m = mapping(corps).unwrap();
            assert!(!m.get("k").unwrap().indexe);
            let rendu = serde_json::to_string(&m.to_json()).unwrap();
            assert!(
                rendu.contains(r#""index":false"#),
                "rendu inattendu : {rendu}"
            );
        }
    }

    #[test]
    fn refuse_une_valeur_d_index_qui_n_est_ni_vraie_ni_fausse() {
        for corps in [
            r#"{"properties":{"k":{"type":"keyword","index":"no"}}}"#,
            r#"{"properties":{"k":{"type":"keyword","index":1}}}"#,
            r#"{"properties":{"k":{"type":"keyword","index":null}}}"#,
        ] {
            let e = mapping(corps).unwrap_err();
            assert_eq!(e.ty, "mapper_parsing_exception");
            assert!(e.reason.contains("[true]"), "{}", e.reason);
        }
    }

    #[test]
    fn refuse_index_sur_un_objet() {
        // Un objet n'entre pas dans l'index inverse : il n'existe que par ses
        // feuilles. ES refuse le parametre la, en « unsupported parameters ».
        for corps in [
            r#"{"properties":{"o":{"type":"object","index":false}}}"#,
            r#"{"properties":{"o":{"type":"nested","index":false}}}"#,
        ] {
            let e = mapping(corps).unwrap_err();
            assert_eq!(e.ty, "mapper_parsing_exception");
            assert!(e.reason.contains("unsupported parameters"), "{}", e.reason);
        }
    }

    #[test]
    fn refuse_un_parametre_de_champ_non_supporte() {
        // `german` est servi depuis la carte des analyzers de langue ; le
        // finnois, lui, reste refuse **avec son chiffre**.
        assert!(mapping(r#"{"properties":{"t":{"type":"text","analyzer":"german"}}}"#).is_ok());
        let e =
            mapping(r#"{"properties":{"t":{"type":"text","analyzer":"finnish"}}}"#).unwrap_err();
        assert!(e.reason.contains("84 399"), "{}", e.reason);
        let e = mapping(r#"{"properties":{"t":{"type":"text","analyzer":"czech"}}}"#).unwrap_err();
        assert!(e.reason.contains("analyzer"), "{}", e.reason);
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

    fn feuilles(v: Value) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        parcours_feuilles(v.as_object().unwrap(), &mut |chemin, valeur| {
            out.push((chemin.to_string(), valeur.clone()));
            Ok(())
        })
        .unwrap();
        out
    }

    #[test]
    fn un_document_se_parcourt_par_chemins() {
        assert_eq!(
            // L'ordre est celui du document (`serde_json` preserve les cles).
            feuilles(json!({"titre": "a", "client": {"ville": "Lyon", "cp": 69}})),
            vec![
                ("titre".into(), json!("a")),
                ("client.ville".into(), json!("Lyon")),
                ("client.cp".into(), json!(69)),
            ]
        );
        // Profondeur quelconque.
        assert_eq!(
            feuilles(json!({"a": {"b": {"c": 1}}})),
            vec![("a.b.c".into(), json!(1))]
        );
        // Un tableau d'objets est aplati : deux valeurs pour le meme chemin,
        // comme chez ES — c'est ce que `nested` existe pour eviter.
        assert_eq!(
            feuilles(json!({"l": [{"ref": "A"}, {"ref": "B"}]})),
            vec![("l.ref".into(), json!("A")), ("l.ref".into(), json!("B"))]
        );
        // Un tableau de scalaires reste une seule feuille multivaluee.
        assert_eq!(
            feuilles(json!({"tags": ["a", "b"]})),
            vec![("tags".into(), json!(["a", "b"]))]
        );
        // Une cle deja pointee est un chemin, comme chez ES.
        assert_eq!(
            feuilles(json!({"client.ville": "Lyon"})),
            vec![("client.ville".into(), json!("Lyon"))]
        );
    }

    #[test]
    fn un_tableau_ne_melange_pas_objets_et_valeurs() {
        let mut rien = |_: &str, _: &Value| Ok(());
        let v = json!({"l": [{"ref": "A"}, 42]});
        let err = parcours_feuilles(v.as_object().unwrap(), &mut rien).unwrap_err();
        assert!(err.reason.contains("melange"), "{}", err.reason);
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
    fn search_analyzer_et_analyzer_par_defaut() {
        // Sans `search_analyzer`, la requete emprunte l'analyzer d'indexation.
        let m = Mapping::parse(&json!({"properties": {
            "a": {"type": "text", "analyzer": "english"},
            "b": {"type": "text", "analyzer": "english", "search_analyzer": "standard"},
            "c": {"type": "text", "search_analyzer": "keyword"},
        }}))
        .unwrap();
        assert_eq!(m.get("a").unwrap().search_analyzer(), Analyzer::English);
        assert_eq!(m.get("b").unwrap().analyzer(), Analyzer::English);
        assert_eq!(m.get("b").unwrap().search_analyzer(), Analyzer::Standard);
        assert_eq!(m.get("c").unwrap().analyzer(), Analyzer::Standard);
        assert_eq!(m.get("c").unwrap().search_analyzer(), Analyzer::Keyword);

        // Le mapping rendu nomme `default` l'analyzer d'indexation d'un champ
        // qui n'en declare pas — comme ES — et **se relit tel quel** : sans
        // ca, un redemarrage le transformerait en `standard`.
        let rendu = m.to_json();
        let c = &rendu["properties"]["c"];
        assert_eq!(c["analyzer"], json!("default"));
        assert_eq!(c["search_analyzer"], json!("keyword"));
        let relu = Mapping::parse(&rendu).unwrap();
        assert!(relu.get("c").unwrap().analyzer.is_none());
        assert_eq!(relu.to_json(), rendu);

        // Ailleurs que sur un `text`, c'est un parametre inconnu chez ES.
        let e = Mapping::parse(&json!({"properties": {
            "k": {"type": "keyword", "search_analyzer": "standard"}}}))
        .unwrap_err();
        assert_eq!(
            e.reason,
            "unknown parameter [search_analyzer] on mapper [k] of type [keyword]"
        );
    }

    #[test]
    fn copy_to_et_ses_trois_refus() {
        let m = Mapping::parse(&json!({"properties": {
            "t": {"type": "text", "copy_to": "tout"},
            "k": {"type": "keyword", "copy_to": ["tout", "gens"]},
            "tout": {"type": "text"},
            "gens": {"type": "text"},
        }}))
        .unwrap();
        // Rendu en tableau, meme declare en chaine.
        assert_eq!(m.to_json()["properties"]["t"]["copy_to"], json!(["tout"]));
        let (_, fields) = build_schema(&m);
        assert_eq!(fields.copies["k"], vec!["tout", "gens"]);
        // Et l'inverse, trie par nom de source : c'est l'ordre dans lequel ES
        // rend les valeurs copiees dans `fields`.
        assert_eq!(fields.copiants["tout"], vec!["k", "t"]);

        let refus = |props: Value| {
            Mapping::parse(&json!({ "properties": props }))
                .unwrap_err()
                .reason
        };
        assert!(refus(json!({
            "t": {"type": "text", "fields": {"k": {"type": "keyword", "copy_to": "x"}}},
            "x": {"type": "text"}}))
        .contains("may not be used to copy from a multi-field: [t.k]"));
        assert!(refus(json!({
            "t": {"type": "text", "copy_to": "x.k"},
            "x": {"type": "text", "fields": {"k": {"type": "keyword"}}}}))
        .contains("may not be used to copy to a multi-field: [x.k]"));
        assert!(refus(json!({
            "t": {"type": "text", "copy_to": "o"},
            "o": {"properties": {"a": {"type": "text"}}}}))
        .contains("Cannot copy to field [o] since it is mapped as an object"));
        assert!(refus(json!({
            "t": {"type": "text", "copy_to": "l.a"},
            "l": {"type": "nested", "properties": {"a": {"type": "text"}}}}))
        .contains("Illegal combination of [copy_to] and [nested]"));
        // Depuis un `nested` vers la racine, en revanche, ES l'autorise.
        Mapping::parse(&json!({"properties": {
            "l": {"type": "nested", "properties": {"a": {"type": "text", "copy_to": "tout"}}},
            "tout": {"type": "text"}}}))
        .unwrap();
    }

    #[test]
    fn store_est_rendu_seulement_quand_il_demande_quelque_chose() {
        let m = Mapping::parse(&json!({"properties": {
            "a": {"type": "keyword", "store": true},
            "b": {"type": "text", "store": "true"},
            "c": {"type": "text", "store": false},
            "l": {"type": "nested", "properties": {"x": {"type": "keyword", "store": true}}},
        }}))
        .unwrap();
        let rendu = m.to_json();
        assert_eq!(rendu["properties"]["a"]["store"], json!(true));
        assert_eq!(rendu["properties"]["b"]["store"], json!(true));
        // `store: false` est le defaut d'ES : il ne demande rien, et ES ne le
        // conserve pas non plus.
        assert_eq!(rendu["properties"]["c"], json!({"type": "text"}));

        let (_, fields) = build_schema(&m);
        assert!(fields.get("a").unwrap().store);
        assert!(!fields.get("c").unwrap().store);
        // Sous un `nested`, la valeur stockee vit chez ES dans le document
        // enfant : `stored_fields` n'en rend rien, donc ferrite ne la stocke
        // pas — rendre plus qu'ES serait le rendre en silence.
        assert!(!fields.get("l.x").unwrap().store);

        let e = Mapping::parse(&json!({"properties": {
            "a": {"type": "text", "store": "oui"}}}))
        .unwrap_err();
        assert_eq!(
            e.reason,
            "Failed to parse value [oui] as only [true] or [false] are allowed."
        );
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
