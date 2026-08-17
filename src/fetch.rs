//! `fields`, `docvalue_fields`, `stored_fields` : choisir ce que la reponse
//! transporte, au lieu de rendre tout le `_source`.
//!
//! Les trois ne lisent pas au meme endroit, et c'est tout le sujet :
//!
//! - `fields` lit le **`_source`**, puis type chaque valeur selon le mapping.
//!   Il garde donc l'ordre et les doublons du document (`["b","a","b"]` reste
//!   `["b","a","b"]`), et rend une date au format demande ;
//! - `docvalue_fields` lit les **colonnes**. Elles sont triees : un `keyword`
//!   ressort trie **et dedoublonne** (`["a","b"]`), un numerique trie en
//!   gardant ses doublons (`[1,1,3]`), et un `float` ressort avec la precision
//!   de son stockage (`0.1` devient `0.10000000149011612`). Les trois regles
//!   sont mesurees contre ES 8.15 — aucune n'etait devinable ;
//! - `stored_fields` lit les champs **stockes un par un** par Lucene. ferrite
//!   refuse `store` au mapping (voir [`crate::mapping`]) : aucun champ n'est
//!   donc stocke, et un ES dont le mapping ne porte pas `store: true` ne rend
//!   rien non plus. Reconstituer les valeurs depuis `_source` rendrait des
//!   valeurs qu'ES **ne rend pas** ; ce qui s'implemente, c'est donc ce que
//!   `stored_fields` change vraiment a la reponse : il retire `_source`, et
//!   `_none_` retire aussi `_id`.
//!
//! La forme du bloc rendu est ce qui compte le plus pour un client : **chaque
//! valeur est un tableau**, meme pour un champ mono-value, et un champ absent
//! n'a pas de cle du tout — ce n'est pas une valeur nulle.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};
use tantivy::DocAddress;

use crate::dateformat::DateFormat;
use crate::engine::Generation;
use crate::error::{EsError, EsResult};
use crate::mapping::{self, FieldKind, FieldType, TypedValue};
use crate::search::glob_match;

/// Un champ demande : un nom, un motif, et le format de ses dates.
#[derive(Debug, Clone)]
pub struct Champ {
    pub motif: String,
    /// Le `format` demande. `None` = celui du mapping, sinon celui d'ES par
    /// defaut.
    pub format: Option<DateFormat>,
    /// `include_unmapped` : lire aussi dans `_source` les chemins qu'aucun
    /// champ ne mappe. C'est ce que fait Kibana, qui envoie
    /// `{"field": "*", "include_unmapped": true}` sur chaque recherche.
    pub include_unmapped: bool,
}

/// `stored_fields`, tel que demande.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Stored {
    /// Absent : la reponse garde sa forme habituelle.
    #[default]
    Absent,
    /// `_none_` : ni `_source`, ni `_id`.
    Aucun,
    /// Une liste de noms. Aucun champ n'etant stocke, elle ne rend rien — mais
    /// elle retire quand meme `_source`.
    Liste,
}

/// Ce qu'une recherche demande de transporter, avant resolution sur un mapping.
#[derive(Debug, Clone, Default)]
pub struct Demande {
    pub fields: Vec<Champ>,
    pub docvalue: Vec<Champ>,
    pub stored: Stored,
}

impl Demande {
    /// Rien a lire : ni `fields`, ni `docvalue_fields`.
    pub fn est_vide(&self) -> bool {
        self.fields.is_empty() && self.docvalue.is_empty()
    }

    /// `_source` est-il rendu ? `stored_fields` le retire, sauf `_source`
    /// explicite (mesure contre ES 8.15 : `{"stored_fields": ["titre"]}` rend
    /// un hit sans `_source`, `+ {"_source": true}` le rend).
    pub fn retire_le_source(&self) -> bool {
        self.stored != Stored::Absent
    }

    /// `_id` est-il rendu ? Seul `_none_` le retire.
    pub fn avec_id(&self) -> bool {
        self.stored != Stored::Aucun
    }
}

// ---------------------------------------------------------------------------
// Lecture du corps
// ---------------------------------------------------------------------------

/// Lit `fields` / `docvalue_fields` : une liste de noms, de motifs, ou d'objets
/// `{field, format, include_unmapped}`.
///
/// Une chaine nue a la place de la liste est refusee comme chez ES
/// (`Unknown key for a VALUE_STRING in [fields].`) : c'est une faute de forme,
/// pas une abreviation.
pub fn lire_champs(v: &Value, cle: &str) -> EsResult<Vec<Champ>> {
    let items = match v {
        Value::Array(a) => a,
        autre => {
            return Err(EsError::parsing(format!(
                "Unknown key for a {} in [{cle}].",
                jeton(autre)
            )))
        }
    };
    items.iter().map(|item| lire_champ(item, cle)).collect()
}

/// Le nom qu'ES donne au jeton JSON qu'il vient de lire, pour son message
/// d'erreur.
fn jeton(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "VALUE_STRING",
        Value::Number(_) => "VALUE_NUMBER",
        Value::Bool(_) => "VALUE_BOOLEAN",
        Value::Null => "VALUE_NULL",
        Value::Array(_) => "START_ARRAY",
        Value::Object(_) => "START_OBJECT",
    }
}

fn lire_champ(v: &Value, cle: &str) -> EsResult<Champ> {
    match v {
        Value::String(s) => Ok(Champ {
            motif: s.clone(),
            format: None,
            include_unmapped: false,
        }),
        // ES lit un nombre comme un nom de champ (il rend 200 et ne trouve
        // rien) : le refuser serait plus severe que lui, sans rien proteger.
        Value::Number(n) => Ok(Champ {
            motif: n.to_string(),
            format: None,
            include_unmapped: false,
        }),
        Value::Object(o) => {
            for k in o.keys() {
                if !matches!(k.as_str(), "field" | "format" | "include_unmapped") {
                    return Err(EsError::new(
                        axum::http::StatusCode::BAD_REQUEST,
                        "x_content_parse_exception",
                        format!("[fetch_field_and_format] unknown field [{k}]"),
                    ));
                }
            }
            let motif = match o.get("field") {
                Some(Value::String(s)) => s.clone(),
                Some(autre) => {
                    return Err(EsError::illegal_argument(format!(
                        "[{cle}] : [field] doit etre une chaine, recu {autre}"
                    )))
                }
                None => return Err(EsError::illegal_argument("Required [field]")),
            };
            let format = match o.get("format") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(DateFormat::parse(s)?),
                Some(autre) => {
                    return Err(EsError::illegal_argument(format!(
                        "[{cle}] : [format] doit etre une chaine, recu {autre}"
                    )))
                }
            };
            // ES accepte le booleen comme la chaine : les clients Kibana
            // envoient `"true"`.
            let include_unmapped = match o.get("include_unmapped") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(Value::String(s)) if s == "true" => true,
                Some(Value::String(s)) if s == "false" => false,
                Some(autre) => {
                    return Err(EsError::illegal_argument(format!(
                        "[{cle}] : [include_unmapped] doit etre un booleen, recu {autre}"
                    )))
                }
            };
            Ok(Champ {
                motif,
                format,
                include_unmapped,
            })
        }
        autre => Err(EsError::parsing(format!(
            "Unknown key for a {} in [{cle}].",
            jeton(autre)
        ))),
    }
}

/// Lit `stored_fields` : `_none_`, un nom, ou une liste de noms.
pub fn lire_stored(v: &Value) -> EsResult<Stored> {
    let noms: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        autre => vec![autre],
    };
    let mut aucun = false;
    let mut nommes = false;
    for n in noms {
        match n {
            Value::String(s) if s == "_none_" => aucun = true,
            Value::String(_) => nommes = true,
            autre => {
                return Err(EsError::illegal_argument(format!(
                    "[stored_fields] : liste de chaines attendue, recu {autre}"
                )))
            }
        }
    }
    Ok(match (aucun, nommes) {
        (true, _) => Stored::Aucun,
        (false, _) => Stored::Liste,
    })
}

/// La meme lecture, depuis la query string (`?stored_fields=a,b`).
///
/// `?fields=` n'existe pas chez ES — il le refuse comme un parametre inconnu,
/// et ferrite fait pareil en ne le lisant nulle part.
pub fn stored_des_params(liste: &[String]) -> Stored {
    if liste.iter().any(|s| s == "_none_") {
        Stored::Aucun
    } else {
        Stored::Liste
    }
}

// ---------------------------------------------------------------------------
// Resolution sur un mapping
// ---------------------------------------------------------------------------

/// Une valeur a lire dans `_source`, resolue sur le mapping d'un index.
#[derive(Debug, Clone)]
struct Lecture {
    /// Le chemin du champ mappe — la cle rendue (`titre.keyword`).
    chemin: String,
    /// Le chemin de la valeur dans `_source`. Un multi-field lit son parent :
    /// `titre.keyword` n'existe pas dans le document, `titre` si.
    source: String,
    ty: FieldType,
    /// `ignore_above` du champ. Une valeur plus longue n'a pas ete indexee :
    /// elle ne sort **pas** dans `fields`, elle sort dans
    /// `ignored_field_values` (mesure contre ES 8.15). Le test est celui de
    /// l'indexation, au caractere pres — sinon les deux ne s'accorderaient pas
    /// sur ce qui a ete ignore.
    ignore_above: Option<usize>,
    /// Le format qui **lit** la date du `_source` : celui du mapping, puisque
    /// c'est lui qui a servi a l'indexer.
    lecture: DateFormat,
    /// Le format qui la **rend** : celui demande, sinon celui du mapping.
    rendu: DateFormat,
}

/// Une colonne a lire, resolue sur le mapping d'un index.
#[derive(Debug, Clone)]
struct Colonne {
    chemin: String,
    ty: FieldType,
    rendu: DateFormat,
}

/// Ce qu'une recherche transporte, resolu sur **un** mapping.
///
/// Un plan par index vise : deux index de mappings differents ne rendent pas
/// les memes champs pour le meme motif, exactement comme deux shards chez ES.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Par chemin rendu — une table, pas une liste, parce que **la derniere
    /// specification gagne** : `fields: [{"field": "d", "format": "yyyy"}, "d*"]`
    /// rend la date au format du mapping, et l'ordre inverse au format demande
    /// (mesure contre ES 8.15).
    lectures: BTreeMap<String, Lecture>,
    colonnes: BTreeMap<String, Colonne>,
    /// Les metadonnees demandees par leur nom (`_id`, `_index`, `_version`).
    meta: Vec<String>,
    /// Les motifs de `include_unmapped` : ce qui se cherche dans `_source`
    /// hors mapping.
    libres: Vec<String>,
    /// Les racines `nested` de l'index, pour grouper les valeurs par element.
    nested: BTreeSet<String>,
    /// L'erreur que ce mapping-la reserve — un `format` sur un `keyword`, un
    /// `docvalue_fields` sur un `text`.
    ///
    /// Elle est **differee** : chez ES c'est la phase de *fetch* qui echoue,
    /// donc une recherche qui ne ramene aucun document rend 200 malgre elle
    /// (mesure : `{"docvalue_fields": ["un_text"]}` avec `size: 0` ou zero
    /// correspondance rend 200 ; des qu'un document est ramene, 400). La lever
    /// a la resolution ferait echouer une recherche qu'ES accepte.
    erreur: Option<EsError>,
}

impl Plan {
    pub fn est_vide(&self) -> bool {
        self.lectures.is_empty()
            && self.colonnes.is_empty()
            && self.meta.is_empty()
            && self.libres.is_empty()
    }

    /// L'erreur differee, s'il y en a une (voir [`Plan::erreur`]).
    pub fn erreur(&self) -> Option<&EsError> {
        self.erreur.as_ref()
    }

    /// Remplace l'erreur differee par celle qui sera vraiment rendue — la
    /// couche HTTP l'enveloppe dans le « all shards failed » d'ES, qu'elle
    /// seule sait construire (il lui faut le nom et l'uuid de l'index).
    pub fn poser_erreur(&mut self, e: EsError) {
        self.erreur = Some(e);
    }
}

/// Les metadonnees que `fields` sait rendre.
///
/// Mesure contre ES 8.15, nom par nom : `_id`, `_index` et `_version` rendent
/// une valeur ; `_score`, `_routing`, `_type` et `_doc` rendent **rien** (200,
/// pas de cle) ; `_seq_no` et `_source` rendent un **500** (« Cannot fetch
/// values for internal field »). Un 500 ne se reproduit pas : ferrite les
/// refuse explicitement.
///
/// `_ignored` est le troisieme refus, et pour une autre raison : ES y liste
/// les champs qu'un `ignore_above` a ecartes, et ferrite ne tient pas cette
/// liste — ni comme cle du hit, ni comme champ adressable. Rendre un tableau
/// vide serait dire « aucun champ ecarte » alors qu'on ne le sait pas. La meme
/// information est disponible la ou elle a ete demandee : les valeurs ecartees
/// sortent dans `ignored_field_values`.
const META_RENDUES: [&str; 3] = ["_id", "_index", "_version"];
const META_REFUSEES: [&str; 3] = ["_seq_no", "_source", "_ignored"];

/// Resout une demande sur le mapping d'un index.
///
/// Les erreurs rendues ici sont des erreurs **de shard** chez ES (un `format`
/// sur un `keyword` fait echouer le shard, pas la requete) : elles portent donc
/// le marqueur qui permet a une recherche multi-index de n'echouer que sur cet
/// index-la.
pub fn resoudre(demande: &Demande, gen: &Generation, index: &str) -> EsResult<Plan> {
    let mut plan = Plan {
        nested: gen.fields.nested.clone(),
        ..Plan::default()
    };
    for champ in &demande.fields {
        resoudre_fields(champ, gen, &mut plan)?;
    }
    for champ in &demande.docvalue {
        resoudre_docvalue(champ, gen, index, &mut plan)?;
    }
    // Un champ demande des deux cotes est rendu par `fields`, pas par
    // `docvalue_fields` — donc dans l'ordre du `_source` et non dans celui de
    // la colonne (mesure contre ES 8.15 : `{"fields": ["k"], "docvalue_fields":
    // ["k"]}` rend `["b","a","b"]`, pas `["a","b"]`). Le refus que porte la
    // colonne, lui, reste : ES echoue quand meme sur un `text`.
    plan.colonnes
        .retain(|chemin, _| !plan.lectures.contains_key(chemin));
    plan.meta.sort();
    plan.meta.dedup();
    Ok(plan)
}

fn resoudre_fields(champ: &Champ, gen: &Generation, plan: &mut Plan) -> EsResult<()> {
    let motif = champ.motif.as_str();
    let joker = motif.contains('*');

    if !joker && motif.starts_with('_') {
        if META_REFUSEES.contains(&motif) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{motif}] dans [fields] : c'est un champ interne dont \
                 ferrite ne tient pas la valeur ; les valeurs ecartees par [ignore_above] sortent \
                 dans [ignored_field_values]"
            )));
        }
        if META_RENDUES.contains(&motif) {
            plan.meta.push(motif.to_string());
        }
        // Les autres metadonnees ne rendent rien chez ES non plus.
        return Ok(());
    }

    for (chemin, mapped) in &gen.fields.mapped {
        if !correspond(motif, chemin, joker) {
            continue;
        }
        // Un `format` ne veut dire quelque chose que sur une date. ES fait
        // echouer le shard, et cite le motif quand le champ vient d'un joker.
        if champ.format.is_some() && mapped.ty.kind() != FieldKind::Date {
            differe(
                plan,
                EsError::illegal_argument(format!(
                    "error fetching [{chemin}]{}: Field [{chemin}] of type [{}] doesn't support \
                     formats.",
                    if joker {
                        format!(" which matched [{motif}] ")
                    } else {
                        ": ".to_string()
                    },
                    mapped.ty.name()
                ))
                .sur_un_shard(),
            );
            continue;
        }
        let mapping_format = gen.fields.format_ou_defaut(chemin).clone();
        plan.lectures.insert(
            chemin.clone(),
            Lecture {
                source: chemin_source(chemin, gen),
                chemin: chemin.clone(),
                ty: mapped.ty,
                ignore_above: mapped.ignore_above,
                rendu: champ
                    .format
                    .clone()
                    .unwrap_or_else(|| mapping_format.clone()),
                lecture: mapping_format,
            },
        );
    }
    if champ.include_unmapped {
        plan.libres.push(motif.to_string());
    }
    Ok(())
}

fn resoudre_docvalue(
    champ: &Champ,
    gen: &Generation,
    index: &str,
    plan: &mut Plan,
) -> EsResult<()> {
    let motif = champ.motif.as_str();
    let joker = motif.contains('*');

    if !joker && motif.starts_with('_') {
        // ES refuse `_id` (« Fielddata access on the _id field is
        // disallowed ») ; les autres metadonnees n'ont pas de colonne.
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [{motif}] dans [docvalue_fields] : un champ interne n'a pas \
             de colonne"
        )));
    }

    for (chemin, mapped) in &gen.fields.mapped {
        if !correspond(motif, chemin, joker) {
            continue;
        }
        // Un `text` n'a pas de colonne : ES fait echouer le shard, que le champ
        // ait ete nomme ou attrape par un joker (mesure : `t*` echoue comme
        // `titre`). La phrase est la sienne, mot pour mot.
        if mapped.ty.kind() == FieldKind::Text {
            differe(
                plan,
                EsError::illegal_argument(format!(
                "Fielddata is disabled on [{chemin}] in [{index}]. Text fields are not optimised \
                 for operations that require per-document field data like aggregations and \
                 sorting, so these operations are disabled by default. Please use a keyword field \
                 instead. Alternatively, set fielddata=true on [{chemin}] in order to load field \
                 data by uninverting the inverted index. Note that this can use significant \
                     memory."
                ))
                .sur_un_shard(),
            );
            continue;
        }
        // Sous un `nested`, les valeurs vivent chez ES dans les documents
        // enfants : `docvalue_fields` n'en rend aucune. ferrite les a bien dans
        // ses colonnes — les rendre serait rendre plus qu'ES, en silence.
        if gen.fields.racine_nested(chemin).is_some() {
            continue;
        }
        if champ.format.is_some() && mapped.ty.kind() != FieldKind::Date {
            // Sur un `keyword`, ES refuse. Sur un numerique, il applique le
            // motif comme un `DecimalFormat` de Java (`format: "yyyy"` sur la
            // valeur 1 rend `"yyyy1"`) : ferrite ne l'imite pas, il le refuse.
            differe(
                plan,
                EsError::illegal_argument(format!(
                    "Field [{chemin}] of type [{}] does not support custom formats",
                    mapped.ty.name()
                ))
                .sur_un_shard(),
            );
            continue;
        }
        plan.colonnes.insert(
            chemin.clone(),
            Colonne {
                rendu: champ
                    .format
                    .clone()
                    .unwrap_or_else(|| gen.fields.format_ou_defaut(chemin).clone()),
                chemin: chemin.clone(),
                ty: mapped.ty,
            },
        );
    }
    Ok(())
}

/// Garde la **premiere** erreur differee : ES rend celle qu'il rencontre en
/// premier, et une seule.
fn differe(plan: &mut Plan, e: EsError) {
    if plan.erreur.is_none() {
        plan.erreur = Some(e);
    }
}

/// Un motif ne matche les metadonnees chez personne : `fields: ["*"]` ne rend
/// ni `_id` ni `_index` (mesure contre ES 8.15).
fn correspond(motif: &str, chemin: &str, joker: bool) -> bool {
    if joker {
        !chemin.starts_with('_') && glob_match(motif, chemin)
    } else {
        motif == chemin
    }
}

/// Ou lire la valeur d'un champ dans `_source`.
///
/// Un multi-field (`titre.keyword`) n'existe pas dans le document : sa valeur
/// est celle de son parent. Un sous-champ d'objet (`client.ville`), lui, est
/// une propriete a part entiere.
fn chemin_source(chemin: &str, gen: &Generation) -> String {
    if gen.mapping.properties.contains_key(chemin) {
        return chemin.to_string();
    }
    match chemin.rsplit_once('.') {
        Some((parent, _)) if gen.mapping.properties.contains_key(parent) => parent.to_string(),
        _ => chemin.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Rendu d'un hit
// ---------------------------------------------------------------------------

/// Le document tel que le bloc `fields` le lit : `_source` **complet** (pas
/// celui que le filtre `_source` a reduit), plus les metadonnees adressables.
#[derive(Clone, Copy)]
pub struct Document<'a> {
    pub source: &'a Value,
    pub index: &'a str,
    pub id: &'a str,
    pub version: Option<u64>,
}

/// Ce qu'un hit gagne : le bloc `fields`, et le bloc `ignored_field_values`
/// des valeurs qu'`ignore_above` a laissees de cote.
#[derive(Default)]
pub struct Blocs {
    pub fields: Option<Value>,
    /// Les valeurs trop longues pour `ignore_above`. ES ne les met **pas**
    /// dans `fields` — elles n'ont pas ete indexees — mais les rend a part,
    /// et seulement quand `fields` est demande.
    pub ignores: Option<Value>,
}

/// Les blocs d'un hit, vides s'il n'y a rien a rendre.
///
/// ES n'ajoute pas de cle vide : un champ absent n'apparait pas, et un bloc
/// entierement vide n'apparait pas non plus.
pub fn rendre(
    plan: &Plan,
    gen: &Generation,
    searcher: &tantivy::Searcher,
    addr: DocAddress,
    doc: &Document<'_>,
) -> EsResult<Blocs> {
    let Document {
        source,
        index: index_name,
        id,
        version,
    } = *doc;
    // La phase de fetch d'ES echoue ici, pas plus tot : un document est
    // vraiment ramene, donc le refus a lieu (voir [`Plan::erreur`]).
    if let Some(e) = &plan.erreur {
        return Err(e.clone());
    }
    if plan.est_vide() {
        return Ok(Blocs::default());
    }
    let mut bloc: Map<String, Value> = Map::new();
    let mut ignores: Map<String, Value> = Map::new();

    // `fields`, depuis le `_source`.
    let refs: Vec<&Lecture> = plan.lectures.values().collect();
    for (cle, valeur) in rendre_niveau(source, "", &refs, &plan.nested, &mut ignores)? {
        bloc.insert(cle, valeur);
    }

    // `include_unmapped` : ce que le mapping ne connait pas.
    if !plan.libres.is_empty() {
        let mut vus: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        chemins_libres(source, "", &mut vus);
        for (chemin, valeurs) in vus {
            if gen.fields.mapped.contains_key(&chemin) || bloc.contains_key(&chemin) {
                continue;
            }
            // Le contenu d'un `nested` se rend groupe, pas a plat.
            if gen.fields.racine_nested(&chemin).is_some() {
                continue;
            }
            if plan.libres.iter().any(|m| correspond_libre(m, &chemin)) && !valeurs.is_empty() {
                bloc.insert(chemin, Value::Array(valeurs));
            }
        }
    }

    // Les metadonnees demandees par leur nom.
    for m in &plan.meta {
        match m.as_str() {
            "_id" => bloc.insert(m.clone(), json!([id])),
            "_index" => bloc.insert(m.clone(), json!([index_name])),
            "_version" => match version {
                Some(v) => bloc.insert(m.clone(), json!([v])),
                None => continue,
            },
            _ => continue,
        };
    }

    // `docvalue_fields`, depuis les colonnes.
    if !plan.colonnes.is_empty() {
        let ff = searcher
            .segment_reader(addr.segment_ord)
            .fast_fields()
            .clone();
        for col in plan.colonnes.values() {
            let valeurs = lire_colonne(&ff, col, addr.doc_id)?;
            if !valeurs.is_empty() {
                bloc.insert(col.chemin.clone(), Value::Array(valeurs));
            }
        }
    }

    Ok(Blocs {
        fields: (!bloc.is_empty()).then(|| Value::Object(bloc)),
        ignores: (!ignores.is_empty()).then(|| Value::Object(ignores)),
    })
}

/// Rend les valeurs d'un niveau : les champs plats, et les `nested` groupes
/// par element.
///
/// Un sous-champ de `nested` ne se rend pas a plat : ES rend
/// `{"lignes": [{"ref": ["X1"]}, {"ref": ["X2"]}]}`, un objet par element du
/// tableau, avec les cles **relatives** a la racine. Un element qui ne porte
/// aucune des valeurs demandees est omis — il n'y a pas d'objet vide.
fn rendre_niveau(
    src: &Value,
    prefixe: &str,
    lectures: &[&Lecture],
    nested: &BTreeSet<String>,
    ignores: &mut Map<String, Value>,
) -> EsResult<Map<String, Value>> {
    let mut plat: Map<String, Value> = Map::new();
    let mut groupes: BTreeMap<String, Vec<&Lecture>> = BTreeMap::new();

    for l in lectures {
        match racine_sous(nested, &l.chemin, prefixe) {
            Some(racine) => groupes.entry(racine).or_default().push(l),
            None => {
                let mut brutes = Vec::new();
                descendre(src, &l.source[prefixe.len()..], &mut brutes);
                let mut valeurs = Vec::with_capacity(brutes.len());
                for b in brutes {
                    // Une valeur qu'`ignore_above` a ecartee n'a pas ete
                    // indexee : la rendre dans `fields` serait rendre une
                    // valeur qu'ES ne rend pas la. Elle part dans
                    // `ignored_field_values`, telle quelle.
                    if trop_longue(l, b) {
                        match ignores.entry(l.chemin.clone()).or_insert_with(|| json!([])) {
                            Value::Array(a) => a.push(b.clone()),
                            _ => unreachable!("toujours un tableau"),
                        }
                        continue;
                    }
                    valeurs.push(rendre_valeur(l, b)?);
                }
                if !valeurs.is_empty() {
                    plat.insert(l.chemin[prefixe.len()..].to_string(), Value::Array(valeurs));
                }
            }
        }
    }

    for (racine, sous) in groupes {
        let mut elements = Vec::new();
        collecter_elements(src, &racine[prefixe.len()..], &mut elements);
        let mut rendus = Vec::new();
        for e in elements {
            let m = rendre_niveau(e, &format!("{racine}."), &sous, nested, ignores)?;
            if !m.is_empty() {
                rendus.push(Value::Object(m));
            }
        }
        if !rendus.is_empty() {
            plat.insert(racine[prefixe.len()..].to_string(), Value::Array(rendus));
        }
    }
    Ok(plat)
}

/// La racine `nested` la plus **externe** sous laquelle ce chemin tombe, a
/// partir du prefixe deja consomme. `None` si le chemin est plat a ce niveau.
fn racine_sous(nested: &BTreeSet<String>, chemin: &str, prefixe: &str) -> Option<String> {
    nested
        .iter()
        .filter(|r| {
            r.len() > prefixe.len()
                && r.starts_with(prefixe)
                && chemin.len() > r.len()
                && chemin.starts_with(r.as_str())
                && chemin.as_bytes()[r.len()] == b'.'
        })
        .min_by_key(|r| r.len())
        .cloned()
}

/// Toutes les valeurs scalaires d'un chemin pointe, tableaux traverses.
///
/// Les `null` sont ecartes : ES ne les rend pas (mesure sur `{"k": [null,
/// "x"]}`, qui rend `["x"]`).
fn descendre<'a>(v: &'a Value, chemin: &str, out: &mut Vec<&'a Value>) {
    if chemin.is_empty() {
        return;
    }
    match v {
        Value::Array(a) => a.iter().for_each(|e| descendre(e, chemin, out)),
        Value::Object(o) => match chemin.split_once('.') {
            Some((tete, reste)) => {
                if let Some(sous) = o.get(tete) {
                    descendre(sous, reste, out);
                }
            }
            None => {
                if let Some(x) = o.get(chemin) {
                    aplatir(x, out);
                }
            }
        },
        _ => {}
    }
}

fn aplatir<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    match v {
        Value::Array(a) => a.iter().for_each(|e| aplatir(e, out)),
        Value::Null => {}
        autre => out.push(autre),
    }
}

/// Les elements d'un tableau `nested` : un objet seul compte pour un element,
/// comme chez ES.
fn collecter_elements<'a>(v: &'a Value, chemin: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Array(a) => a.iter().for_each(|e| collecter_elements(e, chemin, out)),
        Value::Object(o) => match chemin.split_once('.') {
            Some((tete, reste)) => {
                if let Some(sous) = o.get(tete) {
                    collecter_elements(sous, reste, out);
                }
            }
            None => match o.get(chemin) {
                Some(Value::Array(a)) => out.extend(a.iter()),
                Some(Value::Null) | None => {}
                Some(autre) => out.push(autre),
            },
        },
        _ => {}
    }
}

/// La valeur depasse-t-elle l'`ignore_above` du champ ? Le test est celui de
/// l'indexation : un compte de **caracteres**, et seulement sur une chaine.
fn trop_longue(l: &Lecture, brut: &Value) -> bool {
    match l.ignore_above {
        Some(n) => brut.as_str().is_some_and(|s| s.chars().count() > n),
        None => false,
    }
}

/// Type la valeur lue selon le mapping, puis la rend comme ES la rend.
fn rendre_valeur(l: &Lecture, brut: &Value) -> EsResult<Value> {
    let typee = mapping::coerce_avec(&l.chemin, l.ty, brut, Some(&l.lecture))?;
    Ok(match typee {
        TypedValue::Str(s) => json!(s),
        TypedValue::I64(n) => json!(n),
        TypedValue::F64(n) => json!(n),
        TypedValue::Bool(b) => json!(b),
        TypedValue::Date(ms) => match l.rendu.rend(ms) {
            Some(s) => json!(s),
            None => {
                return Err(EsError::illegal_argument(format!(
                    "[{}] : date [{ms}] non representable au format [{}]",
                    l.chemin, l.rendu.source
                )))
            }
        },
    })
}

/// Tous les chemins pointes d'un `_source`, avec leurs valeurs scalaires.
///
/// Sert a `include_unmapped` : c'est le document lui-meme qui dit quels
/// chemins existent, puisque le mapping ne les connait pas.
fn chemins_libres(v: &Value, prefixe: &str, out: &mut BTreeMap<String, Vec<Value>>) {
    match v {
        Value::Object(o) => {
            for (k, sous) in o {
                let chemin = if prefixe.is_empty() {
                    k.clone()
                } else {
                    format!("{prefixe}.{k}")
                };
                chemins_libres(sous, &chemin, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|e| chemins_libres(e, prefixe, out)),
        Value::Null => {}
        autre => {
            if !prefixe.is_empty() {
                out.entry(prefixe.to_string())
                    .or_default()
                    .push(autre.clone());
            }
        }
    }
}

fn correspond_libre(motif: &str, chemin: &str) -> bool {
    if motif.contains('*') {
        glob_match(motif, chemin)
    } else {
        motif == chemin
    }
}

// ---------------------------------------------------------------------------
// Lecture des colonnes
// ---------------------------------------------------------------------------

fn lire_colonne(
    ff: &tantivy::fastfield::FastFieldReaders,
    col: &Colonne,
    doc: tantivy::DocId,
) -> EsResult<Vec<Value>> {
    Ok(match col.ty.kind() {
        // Un `text` n'arrive jamais ici : il est refuse a la resolution.
        FieldKind::Text | FieldKind::Keyword => {
            let Some(c) = ff.str(&col.chemin)? else {
                return Ok(Vec::new());
            };
            // Le dictionnaire d'un `keyword` est un ensemble trie : ES rend
            // donc ses valeurs triees **et dedoublonnees**, la ou `fields`
            // garde l'ordre du document et ses doublons.
            let mut ords: Vec<u64> = c.term_ords(doc).collect();
            ords.sort_unstable();
            ords.dedup();
            let mut out = Vec::with_capacity(ords.len());
            let mut buf = Vec::new();
            for ord in ords {
                buf.clear();
                if c.ord_to_bytes(ord, &mut buf).unwrap_or(false) {
                    out.push(json!(String::from_utf8_lossy(&buf).into_owned()));
                }
            }
            out
        }
        FieldKind::I64 => {
            let mut v: Vec<i64> = ff.i64(&col.chemin)?.values_for_doc(doc).collect();
            v.sort_unstable();
            v.into_iter().map(|n| json!(n)).collect()
        }
        FieldKind::F64 => {
            let mut v: Vec<f64> = ff.f64(&col.chemin)?.values_for_doc(doc).collect();
            // Un `float` est stocke sur 32 bits chez Lucene : la colonne rend
            // `0.10000000149011612` la ou `_source` porte `0.1`. ferrite range
            // les deux en `f64` — sans repasser par `f32`, il rendrait `0.1`,
            // et deux serveurs qui ont indexe la meme chose ne rendraient pas
            // la meme valeur (mesure contre ES 8.15).
            if col.ty == FieldType::Float {
                v = v.into_iter().map(|x| f64::from(x as f32)).collect();
            }
            v.sort_by(f64::total_cmp);
            v.into_iter().map(|n| json!(n)).collect()
        }
        FieldKind::Bool => {
            let mut v: Vec<bool> = ff.bool(&col.chemin)?.values_for_doc(doc).collect();
            v.sort_unstable();
            v.into_iter().map(|b| json!(b)).collect()
        }
        FieldKind::Date => {
            let mut v: Vec<i64> = ff
                .date(&col.chemin)?
                .values_for_doc(doc)
                .map(|d| d.into_timestamp_millis())
                .collect();
            v.sort_unstable();
            v.into_iter()
                .map(|ms| match col.rendu.rend(ms) {
                    Some(s) => json!(s),
                    None => Value::Null,
                })
                .filter(|x| !x.is_null())
                .collect()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liste_de_chaines_et_objets() {
        let c = lire_champs(&json!(["a", {"field": "b", "format": "yyyy"}]), "fields").unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].motif, "a");
        assert_eq!(c[1].format.as_ref().unwrap().source, "yyyy");
    }

    #[test]
    fn chaine_nue_refusee() {
        let e = lire_champs(&json!("a"), "fields").unwrap_err();
        assert_eq!(e.reason, "Unknown key for a VALUE_STRING in [fields].");
    }

    #[test]
    fn cle_inconnue_refusee() {
        let e = lire_champs(&json!([{"field": "a", "truc": 1}]), "fields").unwrap_err();
        assert!(e.reason.contains("unknown field [truc]"));
    }

    #[test]
    fn stored_none() {
        assert_eq!(lire_stored(&json!("_none_")).unwrap(), Stored::Aucun);
        assert_eq!(lire_stored(&json!(["a", "b"])).unwrap(), Stored::Liste);
        assert_eq!(lire_stored(&json!([])).unwrap(), Stored::Liste);
    }

    #[test]
    fn descendre_traverse_les_tableaux() {
        let src = json!({"a": [{"b": 1}, {"b": [2, null, 3]}]});
        let mut out = Vec::new();
        descendre(&src, "a.b", &mut out);
        assert_eq!(out, vec![&json!(1), &json!(2), &json!(3)]);
    }

    #[test]
    fn racine_nested_la_plus_externe() {
        let n: BTreeSet<String> = ["l".to_string(), "l.s".to_string()].into_iter().collect();
        assert_eq!(racine_sous(&n, "l.s.z", ""), Some("l".to_string()));
        assert_eq!(racine_sous(&n, "l.s.z", "l."), Some("l.s".to_string()));
        assert_eq!(racine_sous(&n, "l.r", "l."), None);
    }
}
