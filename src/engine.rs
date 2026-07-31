//! Le catalogue d'index et le moteur tantivy.
//!
//! Cette couche ne connait ni HTTP ni le Query DSL : elle expose des index, des
//! documents, un `searcher`. Un shard, zero replique, un index = un repertoire
//! tantivy + un `ferrite.json` qui porte le mapping.

use std::collections::HashMap;
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
use crate::mapping::{self, Fields, Mapping, TypedValue, F_ID, F_SEQ_NO, F_VERSION};
use crate::util;

const META_FILE: &str = "ferrite.json";
const INDEX_DIR: &str = "index";
const WRITER_HEAP: usize = 50_000_000;

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

pub struct FerriteIndex {
    pub name: String,
    pub uuid: String,
    pub created_at: i64,
    pub mapping: Mapping,
    pub fields: Fields,
    dir: PathBuf,
    index: Index,
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
    docs: RwLock<HashMap<String, DocMeta>>,
    seq_counter: AtomicU64,
    dirty: AtomicBool,
}

impl FerriteIndex {
    pub fn searcher(&self) -> Searcher {
        self.reader.searcher()
    }

    pub fn tantivy_index(&self) -> &Index {
        &self.index
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
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let mut w = self.writer.lock().expect("writer lock");
        w.commit()?;
        drop(w);
        self.reader.reload()?;
        Ok(())
    }

    /// Ecrit (ou remplace) un document.
    ///
    /// `require_absent` implemente l'action `create` du `_bulk` : conflit si le
    /// document existe deja.
    pub fn index_doc(
        &self,
        id: &str,
        source: &Value,
        require_absent: bool,
    ) -> EsResult<WriteOutcome> {
        let doc = self.build_doc(id, source)?;

        let mut docs = self.docs.write().expect("docs lock");
        let existing = docs.get(id).copied();
        let live = existing.filter(|m| !m.deleted);
        if require_absent && live.is_some() {
            return Err(EsError::version_conflict(&self.name, id));
        }
        let version = existing.map_or(1, |m| m.version + 1);
        let seq_no = self.seq_counter.fetch_add(1, Ordering::Relaxed);

        let mut doc = doc;
        doc.add_u64(self.fields.version, version);
        doc.add_u64(self.fields.seq_no, seq_no);

        {
            let w = self.writer.lock().expect("writer lock");
            if live.is_some() {
                w.delete_term(Term::from_field_text(self.fields.id, id));
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

        Ok(WriteOutcome {
            version,
            seq_no,
            created: live.is_none(),
        })
    }

    /// Supprime un document.
    ///
    /// Supprimer un identifiant deja supprime n'est pas une erreur : ES
    /// repond 404 tout en faisant avancer `_version`. On garde donc une
    /// pierre tombale plutot que d'oublier l'identifiant.
    pub fn delete_doc(&self, id: &str) -> EsResult<DeleteOutcome> {
        let mut docs = self.docs.write().expect("docs lock");
        let existing = docs.get(id).copied();
        let was_live = existing.is_some_and(|m| !m.deleted);

        if was_live {
            let w = self.writer.lock().expect("writer lock");
            w.delete_term(Term::from_field_text(self.fields.id, id));
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

        let searcher = self.searcher();
        let query = TermQuery::new(
            Term::from_field_text(self.fields.id, id),
            IndexRecordOption::Basic,
        );
        let top = searcher.search(&query, &TopDocs::with_limit(1).order_by_score())?;
        let Some((_, addr)) = top.first().copied() else {
            return Ok(None);
        };
        let doc: TantivyDocument = searcher.doc(addr)?;
        let source = stored_source(&doc, &self.fields)?;
        Ok(Some(GetResult {
            version: meta.version,
            seq_no: meta.seq_no,
            source,
        }))
    }

    /// Traduit un document JSON en document tantivy, en refusant tout champ
    /// absent du mapping explicite.
    fn build_doc(&self, id: &str, source: &Value) -> EsResult<TantivyDocument> {
        let obj = source
            .as_object()
            .ok_or_else(|| EsError::mapper_parsing("le document doit etre un objet JSON"))?;

        let mut doc = TantivyDocument::new();
        doc.add_text(self.fields.id, id);
        doc.add_text(self.fields.source, serde_json::to_string(source).unwrap());

        for (name, value) in obj {
            let Some((field, ty)) = self.fields.get(name) else {
                return Err(EsError::strict_mapping(&self.name, name));
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
                match mapping::coerce(name, ty, v)? {
                    TypedValue::Str(s) => doc.add_text(field, s),
                    TypedValue::I64(n) => doc.add_i64(field, n),
                    TypedValue::F64(n) => doc.add_f64(field, n),
                    TypedValue::Bool(b) => doc.add_bool(field, b),
                    TypedValue::Date(ms) => {
                        doc.add_date(field, DateTime::from_timestamp_millis(ms))
                    }
                }
            }
        }
        Ok(doc)
    }
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
        let index_dir = dir.join(INDEX_DIR);
        fs::create_dir_all(&index_dir)
            .map_err(|e| EsError::internal(format!("creation de {index_dir:?}: {e}")))?;

        let uuid = util::random_uuid();
        let created_at = util::now_millis();
        let meta = json!({
            "uuid": uuid,
            "created_at": created_at,
            "ferrite_version": crate::FERRITE_VERSION,
            "mappings": mapping.to_json(),
        });
        fs::write(
            dir.join(META_FILE),
            serde_json::to_vec_pretty(&meta).unwrap(),
        )
        .map_err(|e| EsError::internal(format!("ecriture du mapping: {e}")))?;

        let (schema, fields) = mapping::build_schema(&mapping);
        let index = Index::create_in_dir(&index_dir, schema)?;
        let idx = Arc::new(finish_index(
            name.to_string(),
            uuid,
            created_at,
            mapping,
            fields,
            dir,
            index,
            HashMap::new(),
            0,
        )?);
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
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_index(
    name: String,
    uuid: String,
    created_at: i64,
    mapping: Mapping,
    fields: Fields,
    dir: PathBuf,
    index: Index,
    docs: HashMap<String, DocMeta>,
    next_seq: u64,
) -> EsResult<FerriteIndex> {
    let writer: IndexWriter = index.writer_with_num_threads(1, WRITER_HEAP)?;
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    Ok(FerriteIndex {
        name,
        uuid,
        created_at,
        mapping,
        fields,
        dir,
        index,
        writer: Mutex::new(writer),
        reader,
        docs: RwLock::new(docs),
        seq_counter: AtomicU64::new(next_seq),
        dirty: AtomicBool::new(false),
    })
}

fn open_index(dir: &Path, name: &str) -> EsResult<FerriteIndex> {
    let raw = fs::read(dir.join(META_FILE))
        .map_err(|e| EsError::internal(format!("lecture de {META_FILE} pour [{name}]: {e}")))?;
    let meta: Value = serde_json::from_slice(&raw)
        .map_err(|e| EsError::internal(format!("{META_FILE} de [{name}] illisible: {e}")))?;

    let uuid = meta["uuid"].as_str().unwrap_or_default().to_string();
    let created_at = meta["created_at"].as_i64().unwrap_or_else(util::now_millis);
    let mapping = Mapping::parse(&meta["mappings"])?;
    let (_, fields) = mapping::build_schema(&mapping);

    let index = Index::open_in_dir(dir.join(INDEX_DIR))?;
    let (docs, next_seq) = rebuild_doc_table(&index, &fields)?;
    finish_index(
        name.to_string(),
        uuid,
        created_at,
        mapping,
        fields,
        dir.to_path_buf(),
        index,
        docs,
        next_seq,
    )
}

/// Reconstruit la table `_id -> (_version, _seq_no)` a partir des fast fields.
///
/// ES garde ces compteurs dans le translog ; ferrite les relit a l'ouverture,
/// ce qui suffit a un mono-noeud et evite un journal supplementaire.
fn rebuild_doc_table(index: &Index, fields: &Fields) -> EsResult<(HashMap<String, DocMeta>, u64)> {
    let reader: IndexReader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let searcher = reader.searcher();

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
    let _ = fields;
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
