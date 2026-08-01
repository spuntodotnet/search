//! Le catalogue d'index et le moteur tantivy.
//!
//! Cette couche ne connait ni HTTP ni le Query DSL : elle expose des index, des
//! documents, un `searcher`. Un shard, zero replique, un index = un repertoire
//! tantivy + un `ferrite.json` qui porte le mapping.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::{json, Value};
use tantivy::collector::TopDocs;
use tantivy::query::TermQuery;
use tantivy::schema::{IndexRecordOption, TantivyDocument, Value as _};
use tantivy::{DateTime, Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, Term};

use crate::error::{EsError, EsResult};
use crate::mapping::{
    self, Dynamic, FieldMapping, Fields, Mapping, TypedValue, F_ID, F_SEQ_NO, F_VERSION,
};
use crate::util;

const META_FILE: &str = "ferrite.json";
const INDEX_DIR_PREFIX: &str = "index-";
const WRITER_HEAP: usize = 50_000_000;

/// Les conditions d'une ecriture.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteOptions {
    /// L'action `create` : conflit si le document existe deja.
    pub require_absent: bool,
    /// Controle de concurrence optimiste : l'ecriture n'a lieu que si le
    /// document est encore dans l'etat observe.
    pub if_seq_no: Option<u64>,
    pub if_primary_term: Option<u64>,
}

/// Ce que devient un document apres ecriture — de quoi remplir la reponse ES.
#[derive(Debug, Clone, Copy)]
pub struct WriteOutcome {
    pub version: u64,
    pub seq_no: u64,
    pub created: bool,
}

/// L'etat d'un `_id` connu de l'index.
///
/// Une entree survit a la suppression du document (`deleted`) pour que
/// `_version` reste monotone par identifiant, comme chez ES.
#[derive(Debug, Clone, Copy)]
struct DocMeta {
    version: u64,
    seq_no: u64,
    deleted: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct DeleteOutcome {
    /// `false` si l'identifiant n'existait pas ou etait deja supprime.
    pub found: bool,
    pub version: u64,
    pub seq_no: u64,
}

#[derive(Debug, Clone)]
pub struct GetResult {
    pub version: u64,
    pub seq_no: u64,
    pub source: Value,
}

/// Une **generation** de l'index : un schema tantivy fige, et tout ce qui en
/// depend.
///
/// tantivy ne sait pas ajouter un champ a un index existant. Quand le mapping
/// dynamique decouvre un champ, ferrite construit donc une generation neuve et
/// y rejoue les documents depuis le `_source` qu'il conserve deja. Les handles
/// de champ (`Field`) n'ont de sens que dans leur generation : une requete se
/// construit et s'execute sur la **meme** generation, obtenue une fois pour
/// toutes en debut de traitement.
pub struct Generation {
    pub mapping: Mapping,
    pub fields: Fields,
    pub index: Index,
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
    dir: PathBuf,
    seq: u64,
}

impl Generation {
    pub fn searcher(&self) -> Searcher {
        self.reader.searcher()
    }

    fn commit(&self) -> EsResult<()> {
        let mut w = self.writer.lock().expect("writer lock");
        w.commit()?;
        drop(w);
        self.reader.reload()?;
        Ok(())
    }
}

pub struct FerriteIndex {
    pub name: String,
    pub uuid: String,
    pub created_at: i64,
    dir: PathBuf,
    current: RwLock<Arc<Generation>>,
    /// Les generations remplacees, gardees vivantes tant qu'une recherche en
    /// cours en detient une reference (voir
    /// [`FerriteIndex::balayer_generations_retirees`]).
    ///
    /// Precaution, pas correctif demontre : les ecritures, elles, sont
    /// protegees par le verrou de lecture tenu pendant toute leur duree, et le
    /// test `aucune_perte_pendant_les_evolutions_concurrentes` echoue si on
    /// l'enleve. Pour les **lectures**, effacer le repertoire immediatement n'a
    /// pas suffi a faire echouer ce meme test : sous Linux, les fichiers deja
    /// ouverts en mmap survivent a leur suppression. Mais tantivy ouvre
    /// certains fichiers paresseusement, et ce comportement n'est garanti ni
    /// par tantivy ni par les autres systemes — on ne s'y appuie donc pas.
    retirees: Mutex<Vec<Arc<Generation>>>,
    docs: RwLock<HashMap<String, DocMeta>>,
    seq_counter: AtomicU64,
    dirty: AtomicBool,
    /// Serialise les rafraichissements entre eux.
    ///
    /// `refresh` est une garantie : au retour, ce qui etait ecrit avant l'appel
    /// est visible. Sans ce verrou, un appel explicite pouvait tomber pendant
    /// qu'un rafraichissement de fond avait deja pris le drapeau `dirty` sans
    /// avoir fini de commiter — il rendait alors la main trop tot, et le
    /// document n'etait pas encore visible.
    refresh_lock: Mutex<()>,
}

impl FerriteIndex {
    /// La generation courante. A prendre **une seule fois** par requete : les
    /// `Field` qu'elle expose ne valent que pour elle.
    pub fn current(&self) -> Arc<Generation> {
        self.current.read().expect("generation lock").clone()
    }

    pub fn mapping(&self) -> Mapping {
        self.current().mapping.clone()
    }

    pub fn searcher(&self) -> Searcher {
        self.current().searcher()
    }

    pub fn doc_count(&self) -> usize {
        self.docs
            .read()
            .expect("docs lock")
            .values()
            .filter(|m| !m.deleted)
            .count()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }

    /// Taille sur disque, pour `_cat/indices`.
    pub fn store_size(&self) -> u64 {
        dir_size(&self.dir)
    }

    /// Rend les ecritures visibles a la recherche (le `_refresh` d'ES).
    pub fn refresh(&self) -> EsResult<()> {
        // Attendre un rafraichissement deja en cours plutot que de rendre la
        // main pendant qu'il commite encore.
        let _garde = self.refresh_lock.lock().expect("refresh lock");
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        // Verrou de lecture tenu pendant le commit : une evolution ne peut pas
        // remplacer la generation sous nos pieds.
        let gen = self.current.read().expect("generation lock");
        gen.commit()
    }

    /// Efface le repertoire des generations que plus personne ne tient.
    ///
    /// Une recherche en cours peut encore lire une generation remplacee ; tant
    /// que son `Arc` vit, son repertoire doit vivre aussi.
    pub fn balayer_generations_retirees(&self) {
        let mut retirees = self.retirees.lock().expect("retirees lock");
        retirees.retain(|gen| {
            if Arc::strong_count(gen) == 1 {
                let _ = fs::remove_dir_all(&gen.dir);
                false
            } else {
                true
            }
        });
    }

    /// Ecrit (ou remplace) un document.
    ///
    /// `require_absent` implemente l'action `create` du `_bulk` : conflit si le
    /// document existe deja.
    ///
    /// Le verrou de lecture sur la generation est tenu pendant toute
    /// l'ecriture : une evolution de schema ne peut donc pas remplacer — ni
    /// effacer — la generation dans laquelle on est en train d'ecrire.
    pub fn index_doc(
        &self,
        id: &str,
        source: &Value,
        opts: WriteOptions,
    ) -> EsResult<WriteOutcome> {
        loop {
            let gen = self.current.read().expect("generation lock");

            // Le mapping doit accueillir le document. Si des champs manquent,
            // on relache le verrou, on fait evoluer, et on recommence sur la
            // generation neuve.
            if let Some(nouveaux) = self.champs_manquants(&gen, source)? {
                drop(gen);
                self.evoluer(nouveaux)?;
                continue;
            }

            let mut docs = self.docs.write().expect("docs lock");
            let existing = docs.get(id).copied();
            let live = existing.filter(|m| !m.deleted);
            if opts.require_absent && live.is_some() {
                return Err(EsError::version_conflict(&self.name, id));
            }
            verifier_concurrence(&self.name, id, live, &opts)?;
            let version = existing.map_or(1, |m| m.version + 1);
            let seq_no = self.seq_counter.fetch_add(1, Ordering::Relaxed);

            let doc = build_doc(&gen, id, source, version, seq_no)?;
            {
                let w = gen.writer.lock().expect("writer lock");
                if live.is_some() {
                    w.delete_term(Term::from_field_text(gen.fields.id, id));
                }
                w.add_document(doc)?;
            }
            docs.insert(
                id.to_string(),
                DocMeta {
                    version,
                    seq_no,
                    deleted: false,
                },
            );
            self.dirty.store(true, Ordering::Release);

            return Ok(WriteOutcome {
                version,
                seq_no,
                created: live.is_none(),
            });
        }
    }

    /// Supprime un document.
    ///
    /// Supprimer un identifiant deja supprime n'est pas une erreur : ES
    /// repond 404 tout en faisant avancer `_version`. On garde donc une
    /// pierre tombale plutot que d'oublier l'identifiant.
    pub fn delete_doc(&self, id: &str, opts: WriteOptions) -> EsResult<DeleteOutcome> {
        let gen = self.current.read().expect("generation lock");
        let mut docs = self.docs.write().expect("docs lock");
        let existing = docs.get(id).copied();
        let was_live = existing.is_some_and(|m| !m.deleted);
        verifier_concurrence(&self.name, id, existing.filter(|m| !m.deleted), &opts)?;

        if was_live {
            let w = gen.writer.lock().expect("writer lock");
            w.delete_term(Term::from_field_text(gen.fields.id, id));
            self.dirty.store(true, Ordering::Release);
        }

        let version = existing.map_or(1, |m| m.version + 1);
        let seq_no = self.seq_counter.fetch_add(1, Ordering::Relaxed);
        docs.insert(
            id.to_string(),
            DocMeta {
                version,
                seq_no,
                deleted: true,
            },
        );
        Ok(DeleteOutcome {
            found: was_live,
            version,
            seq_no,
        })
    }

    /// `GET /{index}/_doc/{id}`. Temps reel comme chez ES : si des ecritures
    /// sont en attente, on les rend visibles avant de lire.
    pub fn get_doc(&self, id: &str) -> EsResult<Option<GetResult>> {
        let meta = { self.docs.read().expect("docs lock").get(id).copied() };
        let Some(meta) = meta.filter(|m| !m.deleted) else {
            return Ok(None);
        };
        self.refresh()?;

        let gen = self.current();
        let searcher = gen.searcher();
        let query = TermQuery::new(
            Term::from_field_text(gen.fields.id, id),
            IndexRecordOption::Basic,
        );
        let top = searcher.search(&query, &TopDocs::with_limit(1).order_by_score())?;
        let Some((_, addr)) = top.first().copied() else {
            return Ok(None);
        };
        let doc: TantivyDocument = searcher.doc(addr)?;
        let source = stored_source(&doc, &gen.fields)?;
        Ok(Some(GetResult {
            version: meta.version,
            seq_no: meta.seq_no,
            source,
        }))
    }

    /// Met a jour un document par fusion partielle (`POST /{index}/_update/{id}`).
    ///
    /// `_source` etant conserve, la fusion se fait sur le document existant et
    /// le resultat est reindexe. `noop` quand la fusion ne change rien, comme
    /// chez ES.
    pub fn update_doc(
        &self,
        id: &str,
        partiel: Option<&Value>,
        upsert: Option<&Value>,
        doc_as_upsert: bool,
        opts: WriteOptions,
    ) -> EsResult<(WriteOutcome, &'static str)> {
        let actuel = self.get_doc(id)?;
        match actuel {
            Some(existant) => {
                let partiel = partiel.ok_or_else(|| {
                    EsError::illegal_argument("[_update] : [doc] ou [script] est obligatoire")
                })?;
                let mut fusionne = existant.source.clone();
                fusionner(&mut fusionne, partiel);
                if fusionne == existant.source {
                    // ES ne reindexe pas et ne fait pas avancer la version.
                    return Ok((
                        WriteOutcome {
                            version: existant.version,
                            seq_no: existant.seq_no,
                            created: false,
                        },
                        "noop",
                    ));
                }
                let out = self.index_doc(id, &fusionne, opts)?;
                Ok((out, "updated"))
            }
            None => {
                // Absent : `upsert` fournit le document initial, ou `doc` fait
                // office de document initial avec `doc_as_upsert`.
                let initial = match (upsert, doc_as_upsert, partiel) {
                    (Some(u), _, _) => u,
                    (None, true, Some(d)) => d,
                    _ => {
                        return Err(EsError::new(
                            axum::http::StatusCode::NOT_FOUND,
                            "document_missing_exception",
                            format!("[{id}]: document missing"),
                        )
                        .with("index_uuid", json!(self.uuid))
                        .with("shard", json!("0"))
                        .with("index", json!(self.name)))
                    }
                };
                let out = self.index_doc(id, initial, opts)?;
                Ok((out, "created"))
            }
        }
    }

    /// Les metadonnees d'un identifiant, sans lire le document.
    pub fn meta_of(&self, id: &str) -> Option<(u64, u64)> {
        self.docs
            .read()
            .expect("docs lock")
            .get(id)
            .filter(|m| !m.deleted)
            .map(|m| (m.version, m.seq_no))
    }

    /// Ajoute des champs au mapping (`PUT /{index}/_mapping`).
    ///
    /// Possible depuis que le schema vit dans des generations : les champs
    /// existants ne peuvent toujours pas changer de type, comme chez ES.
    pub fn add_fields(&self, nouveaux: BTreeMap<String, FieldMapping>) -> EsResult<()> {
        let gen = self.current();
        let mut a_creer = BTreeMap::new();
        for (nom, decl) in nouveaux {
            match gen.mapping.properties.get(&nom) {
                Some(existant) => {
                    // Redeclarer a l'identique est licite, changer ne l'est pas.
                    if existant.ty != decl.ty {
                        return Err(EsError::illegal_argument(format!(
                            "mapper [{nom}] cannot be changed from type [{}] to [{}]",
                            existant.ty.name(),
                            decl.ty.name()
                        )));
                    }
                }
                None => {
                    a_creer.insert(nom, decl);
                }
            }
        }
        if a_creer.is_empty() {
            return Ok(());
        }
        self.evoluer(a_creer)
    }

    /// Les champs du document que le mapping ne connait pas encore.
    ///
    /// C'est ici que vit la semantique de `dynamic` :
    /// - `strict` : un champ inconnu refuse le document ;
    /// - `false` : il reste dans `_source`, sans etre indexe ;
    /// - `true` : son type est devine et il faudra faire evoluer le schema.
    fn champs_manquants(
        &self,
        gen: &Generation,
        source: &Value,
    ) -> EsResult<Option<BTreeMap<String, FieldMapping>>> {
        let obj = source
            .as_object()
            .ok_or_else(|| EsError::mapper_parsing("le document doit etre un objet JSON"))?;

        let mut nouveaux: BTreeMap<String, FieldMapping> = BTreeMap::new();
        for (name, value) in obj {
            if gen.fields.targets_of(name).is_some() {
                continue;
            }
            match gen.mapping.dynamic {
                Dynamic::Strict => return Err(EsError::strict_mapping(&self.name, name)),
                Dynamic::False => continue,
                Dynamic::True => {
                    if mapping::contient_un_objet(value) {
                        return Err(EsError::unsupported(format!(
                            "ferrite ne supporte pas les champs objet/imbriques : [{name}] dans \
                             l'index [{}]",
                            self.name
                        )));
                    }
                    validate_dynamic_field_name(name)?;
                    // Une valeur nulle ou un tableau vide ne cree pas de champ,
                    // comme chez ES : le type reste inconnu.
                    if let Some(fm) = mapping::infer(value) {
                        nouveaux.insert(name.clone(), fm);
                    }
                }
            }
        }
        Ok((!nouveaux.is_empty()).then_some(nouveaux))
    }

    /// Construit la generation suivante : nouveau schema, documents rejoues
    /// depuis leur `_source`, puis bascule.
    ///
    /// Sûreté au crash : la nouvelle generation est entierement ecrite et
    /// validee **avant** que `ferrite.json` ne la designe (ecriture atomique par
    /// renommage). Un arret entre les deux laisse l'ancienne generation intacte
    /// et le repertoire orphelin est nettoye au demarrage suivant.
    fn evoluer(&self, nouveaux: BTreeMap<String, FieldMapping>) -> EsResult<()> {
        // Verrou exclusif : personne n'ecrit ni ne bascule pendant ce temps.
        let mut courante = self.current.write().expect("generation lock");

        // Un autre appel a pu faire le travail pendant qu'on attendait.
        let manquants: BTreeMap<_, _> = nouveaux
            .into_iter()
            .filter(|(name, _)| courante.fields.targets_of(name).is_none())
            .collect();
        if manquants.is_empty() {
            return Ok(());
        }

        let mut mapping = courante.mapping.clone();
        for (name, fm) in manquants {
            mapping.properties.insert(name, fm);
        }

        // On part d'un etat commit : le rejeu lit les documents stockes.
        courante.commit()?;
        let suivante = Arc::new(construire_generation(
            &self.dir,
            courante.seq + 1,
            mapping,
            Some(&courante),
        )?);
        ecrire_meta(&self.dir, &self.uuid, self.created_at, &suivante)?;

        let ancienne = std::mem::replace(&mut *courante, suivante);
        drop(courante);

        // On n'efface pas tout de suite : une recherche peut encore lire
        // l'ancienne generation. Le balayage s'en chargera quand plus personne
        // ne la tiendra.
        self.retirees.lock().expect("retirees lock").push(ancienne);
        Ok(())
    }
}

/// Refuse l'ecriture si le document n'est plus dans l'etat observe par le
/// client (`if_seq_no` / `if_primary_term`).
fn verifier_concurrence(
    index: &str,
    id: &str,
    live: Option<DocMeta>,
    opts: &WriteOptions,
) -> EsResult<()> {
    if opts.if_seq_no.is_none() && opts.if_primary_term.is_none() {
        return Ok(());
    }
    let (attendu_seq, attendu_term) = (
        opts.if_seq_no.unwrap_or(u64::MAX),
        opts.if_primary_term.unwrap_or(1),
    );
    let actuel = live.map(|m| m.seq_no);
    if actuel != Some(attendu_seq) || attendu_term != 1 {
        return Err(EsError::new(
            axum::http::StatusCode::CONFLICT,
            "version_conflict_engine_exception",
            format!(
                "[{id}]: version conflict, required seqNo [{attendu_seq}], primary term \
                 [{attendu_term}]. but no document was found"
            ),
        )
        .with("index", json!(index))
        .with("shard", json!("0")));
    }
    Ok(())
}

/// Fusionne un document partiel dans un document existant, comme le `doc` d'un
/// `_update` : les objets fusionnent, tout le reste est remplace.
fn fusionner(cible: &mut Value, partiel: &Value) {
    match (cible, partiel) {
        (Value::Object(a), Value::Object(b)) => {
            for (cle, valeur) in b {
                match a.get_mut(cle) {
                    Some(existant) if existant.is_object() && valeur.is_object() => {
                        fusionner(existant, valeur)
                    }
                    _ => {
                        a.insert(cle.clone(), valeur.clone());
                    }
                }
            }
        }
        (cible, autre) => *cible = autre.clone(),
    }
}

/// Traduit un document JSON en document tantivy.
///
/// Un champ absent du schema est **ignore** : `accueillir` est passe avant et a
/// deja tranche (refus, ajout au mapping, ou mise a l'ecart volontaire quand
/// `dynamic` vaut `false`).
fn build_doc(
    gen: &Generation,
    id: &str,
    source: &Value,
    version: u64,
    seq_no: u64,
) -> EsResult<TantivyDocument> {
    let obj = source
        .as_object()
        .ok_or_else(|| EsError::mapper_parsing("le document doit etre un objet JSON"))?;

    let mut doc = TantivyDocument::new();
    doc.add_text(gen.fields.id, id);
    doc.add_text(gen.fields.source, serde_json::to_string(source).unwrap());
    doc.add_u64(gen.fields.version, version);
    doc.add_u64(gen.fields.seq_no, seq_no);

    for (name, value) in obj {
        let Some(cibles) = gen.fields.targets_of(name) else {
            continue;
        };
        let values: Vec<&Value> = match value {
            Value::Null => continue,
            Value::Array(a) => a.iter().collect(),
            v => vec![v],
        };
        for v in values {
            if v.is_null() {
                continue;
            }
            // Le meme contenu part dans le champ et dans chacun de ses
            // multi-fields, chacun avec son propre type.
            for cible in cibles {
                if let Some(limite) = cible.ignore_above {
                    if v.as_str().is_some_and(|s| s.chars().count() > limite) {
                        continue;
                    }
                }
                match mapping::coerce(name, cible.ty, v)? {
                    TypedValue::Str(s) => doc.add_text(cible.field, s),
                    TypedValue::I64(n) => doc.add_i64(cible.field, n),
                    TypedValue::F64(n) => doc.add_f64(cible.field, n),
                    TypedValue::Bool(b) => doc.add_bool(cible.field, b),
                    TypedValue::Date(ms) => {
                        doc.add_date(cible.field, DateTime::from_timestamp_millis(ms))
                    }
                }
            }
        }
    }
    Ok(doc)
}

/// Un champ devine ne doit pas pouvoir entrer en collision avec les champs
/// internes, ni introduire un chemin pointe.
fn validate_dynamic_field_name(name: &str) -> EsResult<()> {
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

fn stored_source(doc: &TantivyDocument, fields: &Fields) -> EsResult<Value> {
    let raw = doc
        .get_first(fields.source)
        .and_then(|v| v.as_str().map(str::to_string))
        .ok_or_else(|| EsError::internal("document sans _source stocke"))?;
    serde_json::from_str(&raw).map_err(|e| EsError::internal(format!("_source illisible: {e}")))
}

pub struct Catalog {
    root: PathBuf,
    pub cluster_name: String,
    pub node_name: String,
    pub cluster_uuid: String,
    indices: RwLock<HashMap<String, Arc<FerriteIndex>>>,
}

impl Catalog {
    /// Ouvre (ou cree) le repertoire de donnees et rouvre les index presents.
    pub fn open(root: PathBuf, cluster_name: String, node_name: String) -> EsResult<Arc<Self>> {
        fs::create_dir_all(&root)
            .map_err(|e| EsError::internal(format!("impossible de creer {root:?}: {e}")))?;

        let catalog = Arc::new(Self {
            root: root.clone(),
            cluster_name,
            node_name,
            cluster_uuid: util::random_uuid(),
            indices: RwLock::new(HashMap::new()),
        });

        let entries = fs::read_dir(&root)
            .map_err(|e| EsError::internal(format!("impossible de lire {root:?}: {e}")))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.join(META_FILE).is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let idx = open_index(&path, &name)?;
            catalog
                .indices
                .write()
                .expect("catalog lock")
                .insert(name, Arc::new(idx));
        }
        Ok(catalog)
    }

    pub fn get(&self, name: &str) -> EsResult<Arc<FerriteIndex>> {
        // Un nom invalide n'est pas un index absent : c'est ce que repond ES a
        // `GET /_une_route_inconnue`, et c'est plus utile qu'un 404.
        validate_index_name(name)?;
        self.indices
            .read()
            .expect("catalog lock")
            .get(name)
            .cloned()
            .ok_or_else(|| EsError::index_not_found(name))
    }

    pub fn exists(&self, name: &str) -> bool {
        self.indices
            .read()
            .expect("catalog lock")
            .contains_key(name)
    }

    /// Les index, tries par nom.
    pub fn list(&self) -> Vec<Arc<FerriteIndex>> {
        let mut v: Vec<_> = self
            .indices
            .read()
            .expect("catalog lock")
            .values()
            .cloned()
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn create(&self, name: &str, mapping: Mapping) -> EsResult<Arc<FerriteIndex>> {
        validate_index_name(name)?;
        let mut guard = self.indices.write().expect("catalog lock");
        if let Some(existing) = guard.get(name) {
            return Err(EsError::index_already_exists(name, &existing.uuid));
        }

        let dir = self.root.join(name);
        if dir.exists() {
            fs::remove_dir_all(&dir).ok();
        }
        fs::create_dir_all(&dir)
            .map_err(|e| EsError::internal(format!("creation de {dir:?}: {e}")))?;

        let uuid = util::random_uuid();
        let created_at = util::now_millis();
        let gen = Arc::new(construire_generation(&dir, 0, mapping, None)?);
        ecrire_meta(&dir, &uuid, created_at, &gen)?;

        let idx = Arc::new(FerriteIndex {
            name: name.to_string(),
            uuid,
            created_at,
            dir,
            current: RwLock::new(gen),
            retirees: Mutex::new(Vec::new()),
            docs: RwLock::new(HashMap::new()),
            seq_counter: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            refresh_lock: Mutex::new(()),
        });
        guard.insert(name.to_string(), idx.clone());
        Ok(idx)
    }

    pub fn delete(&self, name: &str) -> EsResult<()> {
        let mut guard = self.indices.write().expect("catalog lock");
        let Some(idx) = guard.remove(name) else {
            return Err(EsError::index_not_found(name));
        };
        let dir = idx.dir.clone();
        drop(idx);
        fs::remove_dir_all(&dir)
            .map_err(|e| EsError::internal(format!("suppression de {dir:?}: {e}")))?;
        Ok(())
    }

    /// Rafraichit les index qui ont des ecritures en attente. Appele par la
    /// boucle de fond (`index.refresh_interval` d'ES, en plus simple).
    pub fn refresh_dirty(&self) {
        for idx in self.list() {
            if idx.is_dirty() {
                let _ = idx.refresh();
            }
            idx.balayer_generations_retirees();
        }
    }
}

/// Construit une generation : cree son repertoire, son index tantivy, et — si
/// une generation precedente est fournie — y rejoue tous ses documents.
fn construire_generation(
    dir: &Path,
    seq: u64,
    mapping: Mapping,
    precedente: Option<&Generation>,
) -> EsResult<Generation> {
    let gen_dir = dir.join(format!("{INDEX_DIR_PREFIX}{seq}"));
    if gen_dir.exists() {
        fs::remove_dir_all(&gen_dir).ok();
    }
    fs::create_dir_all(&gen_dir)
        .map_err(|e| EsError::internal(format!("creation de {gen_dir:?}: {e}")))?;

    let (schema, fields) = mapping::build_schema(&mapping);
    let index = Index::create_in_dir(&gen_dir, schema)?;
    crate::analysis::register_all(index.tokenizers());
    let writer: IndexWriter = index.writer_with_num_threads(1, WRITER_HEAP)?;
    let reader: IndexReader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let gen = Generation {
        mapping,
        fields,
        index,
        writer: Mutex::new(writer),
        reader,
        dir: gen_dir,
        seq,
    };

    if let Some(source_gen) = precedente {
        rejouer(source_gen, &gen)?;
        gen.commit()?;
    }
    Ok(gen)
}

/// Rejoue tous les documents vivants d'une generation dans la suivante, depuis
/// leur `_source`.
fn rejouer(depuis: &Generation, vers: &Generation) -> EsResult<()> {
    let searcher = depuis.searcher();
    let writer = vers.writer.lock().expect("writer lock");
    for segment in searcher.segment_readers() {
        let store = segment.get_store_reader(0)?;
        for doc_id in segment.doc_ids_alive() {
            let doc: TantivyDocument = store.get(doc_id)?;
            let id = doc
                .get_first(depuis.fields.id)
                .and_then(|v| v.as_str().map(str::to_string))
                .ok_or_else(|| EsError::internal("document sans _id au rejeu"))?;
            let source = stored_source(&doc, &depuis.fields)?;
            let version = doc
                .get_first(depuis.fields.version)
                .and_then(|v| v.as_u64())
                .unwrap_or(1);
            let seq_no = doc
                .get_first(depuis.fields.seq_no)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            writer.add_document(build_doc(vers, &id, &source, version, seq_no)?)?;
        }
    }
    Ok(())
}

/// Ecrit `ferrite.json` de facon atomique (fichier temporaire puis renommage) :
/// il designe la generation courante, donc il ne doit jamais etre a moitie
/// ecrit.
fn ecrire_meta(dir: &Path, uuid: &str, created_at: i64, gen: &Generation) -> EsResult<()> {
    let meta = json!({
        "uuid": uuid,
        "created_at": created_at,
        "ferrite_version": crate::FERRITE_VERSION,
        "generation": gen.seq,
        "mappings": gen.mapping.to_json(),
    });
    let tmp = dir.join(format!("{META_FILE}.tmp"));
    fs::write(&tmp, serde_json::to_vec_pretty(&meta).unwrap())
        .map_err(|e| EsError::internal(format!("ecriture du mapping: {e}")))?;
    fs::rename(&tmp, dir.join(META_FILE))
        .map_err(|e| EsError::internal(format!("bascule du mapping: {e}")))?;
    Ok(())
}

fn open_index(dir: &Path, name: &str) -> EsResult<FerriteIndex> {
    let raw = fs::read(dir.join(META_FILE))
        .map_err(|e| EsError::internal(format!("lecture de {META_FILE} pour [{name}]: {e}")))?;
    let meta: Value = serde_json::from_slice(&raw)
        .map_err(|e| EsError::internal(format!("{META_FILE} de [{name}] illisible: {e}")))?;

    let uuid = meta["uuid"].as_str().unwrap_or_default().to_string();
    let created_at = meta["created_at"].as_i64().unwrap_or_else(util::now_millis);
    let seq = meta["generation"].as_u64().unwrap_or(0);
    let mapping = Mapping::parse(&meta["mappings"])?;

    let gen_dir = dir.join(format!("{INDEX_DIR_PREFIX}{seq}"));
    let (schema, fields) = mapping::build_schema(&mapping);
    let index = Index::open_in_dir(&gen_dir)?;
    crate::analysis::register_all(index.tokenizers());
    if index.schema() != schema {
        return Err(EsError::internal(format!(
            "[{name}] : le schema sur disque ne correspond pas au mapping enregistre"
        )));
    }
    let writer: IndexWriter = index.writer_with_num_threads(1, WRITER_HEAP)?;
    let reader: IndexReader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let gen = Generation {
        mapping,
        fields,
        index,
        writer: Mutex::new(writer),
        reader,
        dir: gen_dir,
        seq,
    };

    // Toute generation autre que la courante est un reliquat d'une evolution
    // interrompue : `ferrite.json` fait foi.
    nettoyer_generations_orphelines(dir, seq);

    let (docs, next_seq) = rebuild_doc_table(&gen)?;
    Ok(FerriteIndex {
        name: name.to_string(),
        uuid,
        created_at,
        dir: dir.to_path_buf(),
        current: RwLock::new(Arc::new(gen)),
        retirees: Mutex::new(Vec::new()),
        docs: RwLock::new(docs),
        seq_counter: AtomicU64::new(next_seq),
        dirty: AtomicBool::new(false),
        refresh_lock: Mutex::new(()),
    })
}

fn nettoyer_generations_orphelines(dir: &Path, courante: u64) {
    let garder = format!("{INDEX_DIR_PREFIX}{courante}");
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let nom = entry.file_name().to_string_lossy().to_string();
        if nom.starts_with(INDEX_DIR_PREFIX) && nom != garder {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Reconstruit la table `_id -> (_version, _seq_no)` a partir des fast fields.
///
/// ES garde ces compteurs dans le translog ; ferrite les relit a l'ouverture,
/// ce qui suffit a un mono-noeud et evite un journal supplementaire.
fn rebuild_doc_table(gen: &Generation) -> EsResult<(HashMap<String, DocMeta>, u64)> {
    let searcher = gen.searcher();
    let mut docs: HashMap<String, DocMeta> = HashMap::new();
    let mut max_seq = 0u64;
    let mut buf: Vec<u8> = Vec::new();

    for segment in searcher.segment_readers() {
        let ff = segment.fast_fields();
        let Some(ids) = ff.str(F_ID)? else { continue };
        let versions = ff.u64(F_VERSION)?;
        let seqs = ff.u64(F_SEQ_NO)?;
        for doc_id in segment.doc_ids_alive() {
            let Some(ord) = ids.term_ords(doc_id).next() else {
                continue;
            };
            buf.clear();
            if !ids.ord_to_bytes(ord, &mut buf).unwrap_or(false) {
                continue;
            }
            let id = String::from_utf8_lossy(&buf).to_string();
            let version = versions.first(doc_id).unwrap_or(1);
            let seq_no = seqs.first(doc_id).unwrap_or(0);
            max_seq = max_seq.max(seq_no);
            let entry = docs.entry(id).or_insert(DocMeta {
                version,
                seq_no,
                deleted: false,
            });
            if version > entry.version {
                *entry = DocMeta {
                    version,
                    seq_no,
                    deleted: false,
                };
            }
        }
    }
    let next_seq = if docs.is_empty() { 0 } else { max_seq + 1 };
    Ok((docs, next_seq))
}

/// Les regles de nommage d'index d'ES. Elles font aussi office de garde-fou :
/// le nom devient un nom de repertoire.
pub fn validate_index_name(name: &str) -> EsResult<()> {
    let invalid = |reason: &str| {
        Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_index_name_exception",
            format!("Invalid index name [{name}], {reason}"),
        )
        .with("index_uuid", json!("_na_"))
        .with("index", json!(name)))
    };
    if name.is_empty() {
        return invalid("must not be empty");
    }
    if name.len() > 255 {
        return invalid("index name is too long, (> 255)");
    }
    if name == "." || name == ".." {
        return invalid("must not be '.' or '..'");
    }
    if name.contains('*') || name.contains(',') {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas les motifs ni les listes d'index (recu [{name}]) : nomme un \
             index unique"
        )));
    }
    if name.starts_with('_') {
        return invalid("must not start with '_'.");
    }
    if name.starts_with('-') || name.starts_with('+') {
        return invalid("must not start with '_', '-', or '+'");
    }
    if name.chars().any(|c| c.is_uppercase()) {
        return invalid("must be lowercase");
    }
    const FORBIDDEN: &[char] = &['\\', '/', '*', '?', '"', '<', '>', '|', ' ', ',', '#', ':'];
    if let Some(c) = name.chars().find(|c| FORBIDDEN.contains(c)) {
        return invalid(&format!("must not contain the following characters [{c}]"));
    }
    Ok(())
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}
