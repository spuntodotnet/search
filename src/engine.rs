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

/// Ou vont les repertoires des index supprimes, en attente d'effacement.
///
/// Un sous-repertoire plutot qu'un prefixe : ES accepte les noms d'index qui
/// commencent par un point (`.kibana`), donc un prefixe serait un nom qu'un
/// client peut prendre. Celui-ci ne porte pas de `ferrite.json` a sa racine,
/// il n'est donc jamais lu comme un index.
const CORBEILLE: &str = ".corbeille";
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
    /// Le bloc `fields` des champs **stockes** demandes, quand la lecture en
    /// demande. `None` = aucun n'a ete demande, ou aucun n'a de valeur.
    pub stockes: Option<Value>,
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
    /// Les reglages acceptes et sans effet (voir [`crate::reglages::INERTES`]).
    ///
    /// Ils ne changent rien a ce que l'index repond — c'est leur definition —
    /// mais ils sont **rendus** par `GET /{index}/_settings`, donc ils vivent
    /// avec l'index et survivent au redemarrage : un script d'init qui relit ce
    /// qu'il a pose doit le retrouver.
    inertes: RwLock<BTreeMap<String, String>>,
    seq_counter: AtomicU64,
    dirty: AtomicBool,
    /// Vrai des que `Catalog::delete` a retire cet index du catalogue.
    ///
    /// Un `Arc` survit a la suppression : la boucle de fond travaille sur un
    /// instantane du catalogue (`refresh_dirty` appelle `list()` une fois),
    /// donc elle peut s'occuper d'un index que le `DELETE` vient de retirer.
    /// Ses repertoires s'appellent `{index}/index-0`, `{index}/index-1` —
    /// exactement ceux qu'un index du **meme nom**, cree juste apres, vient de
    /// s'attribuer. Le vieux balayage efface alors la generation vivante du
    /// neuf, et le vieux commit publie son `meta.json` par-dessus le sien.
    ///
    /// Le drapeau rend inertes les deux gestes de fond. Il est pose sous le
    /// verrou de rafraichissement, donc un rafraichissement deja en cours a
    /// fini avant que le repertoire ne disparaisse.
    supprime: AtomicBool,
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

    /// Les reglages acceptes et sans effet poses sur cet index.
    pub fn inertes(&self) -> BTreeMap<String, String> {
        self.inertes.read().expect("reglages lock").clone()
    }

    /// Pose (ou remplace) des reglages inertes et les persiste.
    ///
    /// `preserve_existing` est le parametre d'ES : ne poser que ce qui n'est
    /// pas deja la.
    pub fn poser_inertes(
        &self,
        demandes: &BTreeMap<String, String>,
        efface: &[String],
        preserve_existing: bool,
    ) -> EsResult<()> {
        {
            let mut courants = self.inertes.write().expect("reglages lock");
            for cle in efface {
                courants.remove(cle);
            }
            for (cle, valeur) in demandes {
                if preserve_existing && courants.contains_key(cle) {
                    continue;
                }
                courants.insert(cle.clone(), valeur.clone());
            }
        }
        let gen = self.current();
        ecrire_meta(
            &self.dir,
            &self.uuid,
            self.created_at,
            &gen,
            &self.inertes(),
        )
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
        // Un index supprime n'a plus de repertoire a lui : commiter ici, c'est
        // ecrire dans celui de l'homonyme qui l'a remplace.
        if self.est_supprime() {
            return Ok(());
        }
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
        if self.est_supprime() {
            // Le repertoire de l'index est deja parti avec lui — et son chemin
            // peut appartenir a un homonyme recree depuis. On lache les
            // generations sans toucher au disque.
            retirees.clear();
            return;
        }
        retirees.retain(|gen| {
            if Arc::strong_count(gen) == 1 {
                let _ = fs::remove_dir_all(&gen.dir);
                false
            } else {
                true
            }
        });
    }

    /// Vrai quand cet index a ete supprime du catalogue.
    pub fn est_supprime(&self) -> bool {
        self.supprime.load(Ordering::Acquire)
    }

    /// Marque l'index supprime : plus rien de ce qu'il porte ne doit toucher au
    /// disque. Pose sous le verrou de rafraichissement pour qu'un
    /// rafraichissement en cours soit **fini** au retour, et non a moitie
    /// commite dans un repertoire qu'on s'apprete a effacer.
    fn marquer_supprime(&self) {
        let _garde = self.refresh_lock.lock().expect("refresh lock");
        self.supprime.store(true, Ordering::Release);
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
            verifier_concurrence(&self.name, &self.uuid, id, live, &opts)?;
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
        verifier_concurrence(
            &self.name,
            &self.uuid,
            id,
            existing.filter(|m| !m.deleted),
            &opts,
        )?;

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
        self.get_doc_avec(id, &[])
    }

    /// Le meme, en lisant au passage les champs **stockes** demandes.
    ///
    /// `GET /{index}/_doc/{id}?stored_fields=` et `_mget` les servent comme
    /// `_search` : c'est le meme stockage, lu au meme endroit. Les separer
    /// aurait fait de `store` un parametre qui ne marche que sur une route.
    pub fn get_doc_avec(&self, id: &str, noms: &[String]) -> EsResult<Option<GetResult>> {
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
            stockes: crate::fetch::stockes_du_document(&gen, &doc, noms)?,
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
                    conflit_de_parametre(&nom, existant, &decl, &gen.mapping.analysis)?;
                }
                None => {
                    a_creer.insert(nom, decl);
                }
            }
        }
        if a_creer.is_empty() {
            return Ok(());
        }
        // Les refus de `copy_to` se lisent sur le mapping **entier** : le corps
        // d'un `PUT /_mapping` ne dit pas si la cible qu'il cite est un objet
        // de l'index. Sans cette relecture, la copie serait posee et jetee en
        // silence a l'indexation.
        let mut fusionne = gen.mapping.clone();
        for (nom, decl) in &a_creer {
            fusionne.properties.insert(nom.clone(), decl.clone());
        }
        fusionne.verifier_copies()?;
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
        // Le document est parcouru en profondeur : un sous-objet ne cree pas de
        // champ pour lui-meme, seulement pour ses feuilles (`client.ville`).
        mapping::parcours_feuilles(obj, &mut |chemin, valeur| {
            if gen.fields.targets_of(chemin).is_some() {
                // La cible d'un `copy_to` est un champ comme un autre : si le
                // mapping ne la declare pas, elle se devine — au type de la
                // **valeur copiee**, comme chez ES (`copy_to` depuis un `long`
                // cree un `long`, pas un `text`). Sans ca, la copie partirait
                // dans le vide et la recherche sur `_all_text` ne rendrait rien,
                // en silence.
                if let Some(copies) = gen.fields.copies.get(chemin) {
                    for cible in copies {
                        if gen.fields.targets_of(cible).is_some()
                            || nouveaux.contains_key(cible.as_str())
                        {
                            continue;
                        }
                        match gen.mapping.dynamic {
                            Dynamic::Strict => {
                                return Err(EsError::strict_mapping(&self.name, cible))
                            }
                            Dynamic::False => {}
                            Dynamic::True => {
                                validate_dynamic_field_name(cible)?;
                                if let Some(fm) = mapping::infer(valeur) {
                                    nouveaux.insert(cible.clone(), fm);
                                }
                            }
                        }
                    }
                }
                return Ok(());
            }
            // Le champ `join` est declare, jamais devine.
            if gen
                .fields
                .join
                .as_ref()
                .is_some_and(|j| chemin == j.champ || mapping::est_sous_chemin(chemin, &j.champ))
            {
                return Ok(());
            }
            match gen.mapping.dynamic {
                Dynamic::Strict => Err(EsError::strict_mapping(&self.name, chemin)),
                Dynamic::False => Ok(()),
                Dynamic::True => {
                    validate_dynamic_field_name(chemin)?;
                    // Une valeur nulle ou un tableau vide ne cree pas de champ,
                    // comme chez ES : le type reste inconnu.
                    if let Some(fm) = mapping::infer(valeur) {
                        nouveaux.insert(chemin.to_string(), fm);
                    }
                    Ok(())
                }
            }
        })?;
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
        ecrire_meta(
            &self.dir,
            &self.uuid,
            self.created_at,
            &suivante,
            &self.inertes.read().expect("reglages lock").clone(),
        )?;

        let ancienne = std::mem::replace(&mut *courante, suivante);
        drop(courante);

        // On n'efface pas tout de suite : une recherche peut encore lire
        // l'ancienne generation. Le balayage s'en chargera quand plus personne
        // ne la tiendra.
        self.retirees.lock().expect("retirees lock").push(ancienne);
        Ok(())
    }
}

/// Un `PUT /{index}/_mapping` qui **redeclare** un champ autrement.
///
/// ES refuse de changer `analyzer` et `store` sur un champ existant, avec cette
/// phrase-la (mesure contre 8.15). ferrite y ajoute `search_analyzer` et
/// `copy_to`, qu'ES sait mettre a jour : les accepter en silence sans rien
/// changer serait le pire des trois resultats — un client croirait sa copie
/// posee. Redeclarer a l'identique reste licite : c'est ce que fait une
/// application qui declare le meme champ pour deux de ses modeles.
fn conflit_de_parametre(
    nom: &str,
    avant: &FieldMapping,
    apres: &FieldMapping,
    analysis: &crate::analysis::Analysis,
) -> EsResult<()> {
    let refus = |parametre: &str, a: String, b: String| {
        Err(EsError::illegal_argument(format!(
            "Mapper for [{nom}] conflicts with existing mapper:\n\tCannot update parameter \
             [{parametre}] from [{a}] to [{b}]"
        )))
    };
    // Ce qui se compare est l'analyzer **effectif**, pas la facon de l'ecrire :
    // ne rien declarer, ecrire `default` ou ecrire `standard` demandent la meme
    // chose, et refuser la redeclaration serait plus severe qu'ES sans rien
    // proteger.
    if avant.analyzer() != apres.analyzer() {
        return refus(
            "analyzer",
            avant.analyzer().name(analysis),
            apres.analyzer().name(analysis),
        );
    }
    if avant.search_analyzer() != apres.search_analyzer() {
        return refus(
            "search_analyzer",
            avant.search_analyzer().name(analysis),
            apres.search_analyzer().name(analysis),
        );
    }
    if avant.store != apres.store {
        return refus("store", avant.store.to_string(), apres.store.to_string());
    }
    if avant.copy_to != apres.copy_to {
        return refus(
            "copy_to",
            avant.copy_to.join(", "),
            apres.copy_to.join(", "),
        );
    }
    Ok(())
}

/// Refuse l'ecriture si le document n'est plus dans l'etat observe par le
/// client (`if_seq_no` / `if_primary_term`).
///
/// ES a **deux** messages, et la difference porte l'information : le document a
/// bouge (« current document has seqNo [n] »), ou il n'est plus la (« but no
/// document was found »). ferrite rendait le second dans les deux cas — lisible
/// comme « quelqu'un a supprime le document » alors qu'il avait seulement ete
/// reecrit. Mesure contre ES 8.15, et c'est ce message que `_delete_by_query`
/// recopie dans ses `failures[]`.
fn verifier_concurrence(
    index: &str,
    uuid: &str,
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
        let etat = match actuel {
            Some(seq) => format!("current document has seqNo [{seq}] and primary term [1]"),
            None => "but no document was found".to_string(),
        };
        return Err(EsError::new(
            axum::http::StatusCode::CONFLICT,
            "version_conflict_engine_exception",
            format!(
                "[{id}]: version conflict, required seqNo [{attendu_seq}], primary term \
                 [{attendu_term}]. {etat}"
            ),
        )
        .with("index_uuid", json!(uuid))
        .with("shard", json!("0"))
        .with("index", json!(index)));
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

    // Le champ `join` n'est pas un champ ordinaire : sa valeur decrit la place
    // du document dans la relation, pas une donnee a indexer telle quelle.
    if let (Some(j), Some(f_nom)) = (&gen.fields.join, gen.fields.join_name) {
        if let Some(v) = obj.get(&j.champ) {
            let (nom, parent) = match v {
                Value::String(s) => (s.as_str(), None),
                Value::Object(o) => {
                    for cle in o.keys() {
                        if cle != "name" && cle != "parent" {
                            return Err(EsError::mapper_parsing(format!(
                                "[{}] : cle [{cle}] inconnue dans un champ [join]",
                                j.champ
                            )));
                        }
                    }
                    let nom = o.get("name").and_then(Value::as_str).ok_or_else(|| {
                        EsError::mapper_parsing(format!("[{}] : cle [name] manquante", j.champ))
                    })?;
                    (nom, o.get("parent").and_then(Value::as_str))
                }
                _ => {
                    return Err(EsError::mapper_parsing(format!(
                        "[{}] : un [join] attend une chaine ou {{name, parent}}",
                        j.champ
                    )))
                }
            };
            if !j.connait(nom) {
                return Err(EsError::illegal_argument(format!(
                    "[{}] : relation [{nom}] inconnue ; declarees : {:?}",
                    j.champ,
                    j.noms()
                )));
            }
            match (j.parent_de(nom), parent) {
                (Some(_), None) => {
                    return Err(EsError::illegal_argument(format!(
                        "[{}] : [{nom}] est un enfant, son [parent] est obligatoire",
                        j.champ
                    )))
                }
                (None, Some(_)) => {
                    return Err(EsError::illegal_argument(format!(
                        "[{}] : [{nom}] est un parent, il n'a pas de [parent]",
                        j.champ
                    )))
                }
                _ => {}
            }
            doc.add_text(f_nom, nom);
            if let (Some(p), Some(f_parent)) = (parent, gen.fields.join_parent) {
                doc.add_text(f_parent, p);
            }
        }
    }

    // Deux passes sur le document : la premiere compte les elements de chaque
    // `nested`, la seconde indexe les valeurs. Elles pourraient n'en faire
    // qu'une, mais elles ecrivent toutes deux dans `doc` — et un document JSON
    // se reparcourt pour rien.
    let mut cardinaux: Vec<(String, u32)> = Vec::new();
    let mut valeurs: Vec<(String, &Value, Option<u32>)> = Vec::new();
    let sans_join: serde_json::Map<String, Value>;
    let obj = match &gen.fields.join {
        Some(j) if obj.contains_key(&j.champ) => {
            sans_join = obj
                .iter()
                .filter(|(k, _)| *k != &j.champ)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            &sans_join
        }
        _ => obj,
    };
    mapping::parcours_nested(
        obj,
        &gen.fields.nested,
        &mut |chemin, value, elem| {
            valeurs.push((chemin.to_string(), value, elem));
            Ok(())
        },
        &mut |racine, n| {
            cardinaux.push((racine.to_string(), n));
            Ok(())
        },
    )?;

    for (racine, n) in cardinaux {
        if let Some(f) = gen.fields.nelem.get(&racine) {
            doc.add_u64(*f, u64::from(n));
        }
    }

    for (chemin, value, elem) in valeurs {
        // `copy_to` recopie la valeur **brute**, avant toute analyse, dans un
        // autre champ — qui la lit avec **son** type et son analyzer. La copie
        // ne se chaine pas : la cible d'une cible ne recoit rien (mesure contre
        // ES 8.15), ce que ce parcours garantit en n'ajoutant les copies qu'aux
        // valeurs venues du document.
        let copies = gen
            .fields
            .copies
            .get(&chemin)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let cibles = gen.fields.targets_of(&chemin).unwrap_or_default();
        if cibles.is_empty() && copies.is_empty() {
            continue;
        }
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
            let format = gen.fields.format_de(&chemin);
            for cible in cibles {
                ecrire(&mut doc, &chemin, cible, v, format, elem)?;
            }
            // Puis dans les cibles de `copy_to`, avec **leur** type et **leur**
            // format : la copie de `n: 42` dans un `text` s'ecrit « 42 ».
            for nom in copies {
                let Some(vers) = gen.fields.targets_of(nom) else {
                    continue;
                };
                let format = gen.fields.format_de(nom);
                for cible in vers {
                    ecrire(&mut doc, nom, cible, v, format, elem)?;
                }
            }
        }
    }
    Ok(doc)
}

/// Ecrit une valeur dans une cible du schema, avec l'indice d'element qui va
/// avec.
fn ecrire(
    doc: &mut TantivyDocument,
    chemin: &str,
    cible: &mapping::MappedField,
    v: &Value,
    format: Option<&crate::dateformat::DateFormat>,
    elem: Option<u32>,
) -> EsResult<()> {
    if let Some(limite) = cible.ignore_above {
        if v.as_str().is_some_and(|s| s.chars().count() > limite) {
            return Ok(());
        }
    }
    match mapping::coerce_avec(chemin, cible.ty, v, format)? {
        TypedValue::Str(s) => doc.add_text(cible.field, s),
        TypedValue::I64(n) => doc.add_i64(cible.field, n),
        TypedValue::F64(n) => doc.add_f64(cible.field, n),
        TypedValue::Bool(b) => doc.add_bool(cible.field, b),
        TypedValue::Date(ms) => doc.add_date(cible.field, DateTime::from_timestamp_millis(ms)),
    }
    // L'indice d'element est ecrit **exactement** quand une valeur l'est : les
    // deux colonnes gardent la meme arite, meme si `ignore_above` en saute une.
    if let (Some(e), Some(f)) = (elem, cible.elem) {
        doc.add_u64(f, u64::from(e));
    }
    Ok(())
}

/// Un champ devine ne doit pas pouvoir entrer en collision avec les champs
/// internes du schema.
fn validate_dynamic_field_name(name: &str) -> EsResult<()> {
    mapping::nom_reserve(name)?;
    // Un chemin pointe est licite — il vient d'un sous-objet, ou d'une cle que
    // le document ecrit deja a plat (`{"client.ville": ...}`), qu'Elasticsearch
    // traite comme un chemin. Un segment vide, lui, ne l'est pas.
    if name.split('.').any(str::is_empty) {
        return Err(EsError::mapper_parsing(format!(
            "[{name}] : chemin de champ invalide"
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

/// Une modification du registre d'alias, telle que `POST /_aliases` la decrit.
#[derive(Debug, Clone)]
pub enum ActionAlias {
    Ajouter {
        index: String,
        alias: String,
        attache: crate::alias::Attache,
    },
    Retirer {
        index: String,
        alias: String,
    },
}

pub struct Catalog {
    root: PathBuf,
    pub cluster_name: String,
    pub node_name: String,
    pub cluster_uuid: String,
    indices: RwLock<HashMap<String, Arc<FerriteIndex>>>,
    /// `alias -> index -> attache`, persiste dans `_aliases.json`.
    aliases: RwLock<crate::alias::Registre>,
    /// Les reglages de cluster, sous leurs deux durees de vie : `persistent`
    /// survit au redemarrage, `transient` non. C'est la distinction d'ES, et
    /// elle compte — un reglage destructif pose « pour cette fois » ne doit pas
    /// se retrouver actif au redemarrage suivant.
    persistants: RwLock<BTreeMap<String, Value>>,
    transitoires: RwLock<BTreeMap<String, Value>>,
    /// Les templates d'index, persistes dans `_templates.json`.
    templates: RwLock<crate::templates::Registre>,
}

/// Le fichier des reglages persistants, a la racine des donnees.
const REGLAGES_FILE: &str = "_cluster.json";

/// Les reglages de cluster que ferrite reconnait.
///
/// Le reste est refuse comme chez ES (`not recognized`) : accepter un reglage
/// sans l'appliquer est exactement l'echec silencieux que le projet interdit.
pub const REGLAGES_CONNUS: &[&str] = &["action.destructive_requires_name"];

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
            aliases: RwLock::new(crate::alias::Registre::new()),
            persistants: RwLock::new(lire_reglages(&root)),
            transitoires: RwLock::new(BTreeMap::new()),
            templates: RwLock::new(crate::templates::charger(&root)),
        });

        // Ce qu'une suppression precedente n'a pas pu finir d'effacer : plus
        // personne ne l'ecrit maintenant, c'est le bon moment (voir `delete`).
        let _ = fs::remove_dir_all(root.join(CORBEILLE));

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

        // Un alias qui designe un index disparu n'a plus de sens : on le laisse
        // tomber a l'ouverture plutot que de rendre 404 a chaque recherche.
        let mut registre = crate::alias::charger(&root);
        {
            let indices = catalog.indices.read().expect("catalog lock");
            for cibles in registre.values_mut() {
                cibles.retain(|nom, _| indices.contains_key(nom));
            }
        }
        registre.retain(|_, cibles| !cibles.is_empty());
        *catalog.aliases.write().expect("alias lock") = registre;
        Ok(catalog)
    }

    // -----------------------------------------------------------------------
    // Reglages de cluster
    // -----------------------------------------------------------------------

    /// `action.destructive_requires_name` : faut-il nommer chaque index a
    /// supprimer ?
    ///
    /// `true` par defaut, comme Elasticsearch **depuis la 8.0** — une purge
    /// ecrite `DELETE /audits-2026.07.*` est donc refusee tant que le reglage
    /// n'a pas ete bascule, exactement comme sur un vrai ES 8. C'est le
    /// contraire d'une commodite : un projet qui purge par motif l'a forcement
    /// bascule chez lui, et ferrite doit refuser la ou ES refuse, sinon la
    /// premiere difference de comportement serait une suppression de donnees.
    pub fn destructive_requires_name(&self) -> bool {
        let lire = |m: &BTreeMap<String, Value>| -> Option<bool> {
            match m.get("action.destructive_requires_name")? {
                Value::Bool(b) => Some(*b),
                Value::String(s) => s.parse().ok(),
                _ => None,
            }
        };
        // `transient` l'emporte sur `persistent`, comme chez ES.
        lire(&self.transitoires.read().expect("reglages lock"))
            .or_else(|| lire(&self.persistants.read().expect("reglages lock")))
            .unwrap_or(true)
    }

    /// Les reglages poses, sous la forme `(persistants, transitoires)`.
    pub fn reglages(&self) -> (BTreeMap<String, Value>, BTreeMap<String, Value>) {
        (
            self.persistants.read().expect("reglages lock").clone(),
            self.transitoires.read().expect("reglages lock").clone(),
        )
    }

    /// Pose (ou efface, sur `null`) des reglages. Rend ce que l'appel a change,
    /// comme le fait la reponse d'ES.
    pub fn poser_reglages(
        &self,
        persistants: &BTreeMap<String, Value>,
        transitoires: &BTreeMap<String, Value>,
    ) -> EsResult<()> {
        for (portee, m) in [("persistent", persistants), ("transient", transitoires)] {
            for cle in m.keys() {
                if !REGLAGES_CONNUS.contains(&cle.as_str()) {
                    return Err(EsError::illegal_argument(format!(
                        "{portee} setting [{cle}], not recognized"
                    )));
                }
            }
        }
        {
            let mut p = self.persistants.write().expect("reglages lock");
            appliquer(&mut p, persistants);
            ecrire_reglages(&self.root, &p)?;
        }
        let mut t = self.transitoires.write().expect("reglages lock");
        appliquer(&mut t, transitoires);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Templates
    // -----------------------------------------------------------------------

    pub fn templates(&self) -> crate::templates::Registre {
        self.templates.read().expect("templates lock").clone()
    }

    /// Pose un template. `create` refuse d'ecraser, comme le parametre d'ES.
    pub fn poser_template(
        &self,
        nom: &str,
        tpl: crate::templates::Template,
        composable: bool,
        create: bool,
    ) -> EsResult<()> {
        let mut registre = self.templates.write().expect("templates lock");
        let table = if composable {
            &registre.composables
        } else {
            &registre.anciens
        };
        if create && table.contains_key(nom) {
            return Err(EsError::illegal_argument(format!(
                "index_template [{nom}] already exists"
            )));
        }
        if composable {
            crate::templates::verifier_priorite(&registre, nom, &tpl)?;
            registre.composables.insert(nom.to_string(), tpl);
        } else {
            registre.anciens.insert(nom.to_string(), tpl);
        }
        crate::templates::enregistrer(&self.root, &registre)
    }

    /// Supprime un template. Rend le 404 d'ES si le nom (ou le motif) ne
    /// designe rien.
    pub fn supprimer_template(&self, nom: &str, composable: bool) -> EsResult<()> {
        let mut registre = self.templates.write().expect("templates lock");
        let table = if composable {
            &mut registre.composables
        } else {
            &mut registre.anciens
        };
        let vises: Vec<String> = table
            .keys()
            .filter(|n| crate::search::glob_match(nom, n))
            .cloned()
            .collect();
        if vises.is_empty() {
            return Err(EsError::new(
                axum::http::StatusCode::NOT_FOUND,
                "index_template_missing_exception",
                format!("index_template [{nom}] missing"),
            ));
        }
        for n in vises {
            table.remove(&n);
        }
        crate::templates::enregistrer(&self.root, &registre)
    }

    // -----------------------------------------------------------------------
    // Alias
    // -----------------------------------------------------------------------

    pub fn aliases(&self) -> crate::alias::Registre {
        self.aliases.read().expect("alias lock").clone()
    }

    pub fn est_alias(&self, nom: &str) -> bool {
        self.aliases.read().expect("alias lock").contains_key(nom)
    }

    /// Les index designes par un alias, tries par nom.
    ///
    /// Le verrou des alias est relache avant celui du catalogue : partout
    /// ailleurs l'ordre est catalogue puis alias, et deux ordres opposes sont
    /// exactement ce qui bloque un jour a deux heures du matin.
    pub fn cibles_alias(&self, alias: &str) -> Option<Vec<Arc<FerriteIndex>>> {
        let noms: Vec<String> = {
            let registre = self.aliases.read().expect("alias lock");
            registre.get(alias)?.keys().cloned().collect()
        };
        let indices = self.indices.read().expect("catalog lock");
        Some(
            noms.iter()
                .filter_map(|n| indices.get(n).cloned())
                .collect(),
        )
    }

    /// L'index vers lequel un alias dirige les **ecritures**.
    ///
    /// Un seul index : evident. Plusieurs : il faut qu'un `is_write_index`
    /// tranche, sinon ES refuse — et ferrite aussi, parce que choisir a sa
    /// place ecrirait silencieusement au mauvais endroit.
    pub fn index_d_ecriture(&self, alias: &str) -> EsResult<String> {
        let registre = self.aliases.read().expect("alias lock");
        let cibles = registre
            .get(alias)
            .ok_or_else(|| EsError::index_not_found(alias))?;
        let designes: Vec<&String> = cibles
            .iter()
            .filter(|(_, a)| a.is_write_index == Some(true))
            .map(|(n, _)| n)
            .collect();
        if designes.len() > 1 {
            return Err(EsError::illegal_argument(format!(
                "alias [{alias}] has more than one write index [{}]",
                designes
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )));
        }
        if let Some(n) = designes.first() {
            return Ok((*n).clone());
        }
        let ouverts: Vec<&String> = cibles
            .iter()
            .filter(|(_, a)| a.is_write_index != Some(false))
            .map(|(n, _)| n)
            .collect();
        match ouverts.len() {
            1 => Ok(ouverts[0].clone()),
            _ => Err(EsError::illegal_argument(format!(
                "no write index is defined for alias [{alias}]. The write index may be explicitly \
                 disabled using is_write_index=false or the alias points to multiple indices \
                 without one being designated as a write index"
            ))),
        }
    }

    /// Applique un lot de modifications d'alias — tout ou rien, comme le
    /// `POST /_aliases` d'ES.
    pub fn modifier_alias(&self, actions: &[ActionAlias]) -> EsResult<()> {
        let indices = self.indices.read().expect("catalog lock");
        let mut registre = self.aliases.write().expect("alias lock");
        let mut suivant = registre.clone();

        for action in actions {
            match action {
                ActionAlias::Ajouter {
                    index,
                    alias,
                    attache,
                } => {
                    if !indices.contains_key(index) {
                        return Err(EsError::index_not_found(index));
                    }
                    // Un alias ne peut pas porter le nom d'un index : la
                    // resolution ne saurait plus lequel des deux designer. Le
                    // type et la phrase sont ceux d'ES 8.15, releves — un client
                    // qui distingue ses erreurs le fait sur le `type`, et
                    // `illegal_argument_exception` ne disait pas laquelle des
                    // deux regles de nommage avait parle.
                    if indices.contains_key(alias.as_str()) {
                        return Err(EsError::new(
                            axum::http::StatusCode::BAD_REQUEST,
                            "invalid_alias_name_exception",
                            format!(
                                "Invalid alias name [{alias}]: an index or data stream exists \
                                 with the same name as the alias"
                            ),
                        ));
                    }
                    suivant
                        .entry(alias.clone())
                        .or_default()
                        .insert(index.clone(), attache.clone());
                }
                ActionAlias::Retirer { index, alias } => {
                    let vide = match suivant.get_mut(alias) {
                        None => {
                            return Err(EsError::new(
                                axum::http::StatusCode::NOT_FOUND,
                                "aliases_not_found_exception",
                                format!("aliases [{alias}] missing"),
                            ))
                        }
                        Some(cibles) => {
                            if cibles.remove(index).is_none() {
                                return Err(EsError::new(
                                    axum::http::StatusCode::NOT_FOUND,
                                    "aliases_not_found_exception",
                                    format!("aliases [{alias}] missing"),
                                ));
                            }
                            cibles.is_empty()
                        }
                    };
                    if vide {
                        suivant.remove(alias);
                    }
                }
            }
        }

        crate::alias::enregistrer(&self.root, &suivant)?;
        *registre = suivant;
        Ok(())
    }

    /// Retire un index de tous les alias qui le designent.
    fn purger_alias(&self, index: &str) {
        let mut registre = self.aliases.write().expect("alias lock");
        let mut change = false;
        for cibles in registre.values_mut() {
            change |= cibles.remove(index).is_some();
        }
        if change {
            registre.retain(|_, cibles| !cibles.is_empty());
            let _ = crate::alias::enregistrer(&self.root, &registre);
        }
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

    /// L'index, cree a la volee s'il n'existe pas encore.
    ///
    /// C'est le comportement d'Elasticsearch a l'ecriture
    /// (`action.auto_create_index`, actif par defaut) : indexer dans un index
    /// absent le cree, avec un mapping vide que les documents rempliront. La
    /// lecture, elle, ne cree rien — `GET` et `_search` rendent toujours 404.
    pub fn get_or_create(&self, name: &str) -> EsResult<Arc<FerriteIndex>> {
        match self.get(name) {
            Ok(idx) => Ok(idx),
            Err(e) if e.ty == "index_not_found_exception" => {
                // Un template qui correspond decide du mapping, des reglages et
                // des alias de l'index qui nait : c'est tout l'objet d'un
                // template, et c'est ici que la creation est **implicite**.
                let tpl = self.templates().pour(name);
                let (mapping, inertes) = match &tpl {
                    Some(t) => crate::reglages::mapping_et_inertes(
                        t.settings.as_ref(),
                        t.mappings.as_ref(),
                    )?,
                    None => (Mapping::default(), BTreeMap::new()),
                };
                match self.create(name, mapping, inertes) {
                    Ok(idx) => {
                        if let Some(actions) = alias_du_template(name, tpl.as_ref())? {
                            // Un alias refuse laisserait un index sans son
                            // alias : on defait plutot que de servir a moitie.
                            if let Err(e) = self.modifier_alias(&actions) {
                                let _ = self.delete(name);
                                return Err(e);
                            }
                        }
                        Ok(idx)
                    }
                    // Un autre appel a gagne la course : son index fait
                    // l'affaire. Mais s'il n'y a toujours pas d'index, c'est
                    // que la creation a echoue pour une autre raison — la
                    // rendre en « no such index » la deguiserait en absence,
                    // et un 404 sur une ecriture qui cree d'habitude est
                    // exactement le genre de message qui envoie chercher au
                    // mauvais endroit.
                    Err(echec) => self.get(name).map_err(|_| echec),
                }
            }
            Err(e) => Err(e),
        }
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

    /// L'index portant exactement ce nom, sans validation ni resolution
    /// d'alias. Le socle de [`crate::selection`].
    pub fn brut(&self, nom: &str) -> Option<Arc<FerriteIndex>> {
        self.indices.read().expect("catalog lock").get(nom).cloned()
    }

    pub fn create(
        &self,
        name: &str,
        mapping: Mapping,
        inertes: BTreeMap<String, String>,
    ) -> EsResult<Arc<FerriteIndex>> {
        validate_index_name(name)?;
        if self.est_alias(name) {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "invalid_index_name_exception",
                format!("Invalid index name [{name}], already exists as alias"),
            )
            .with("index_uuid", json!("_na_"))
            .with("index", json!(name)));
        }
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
        ecrire_meta(&dir, &uuid, created_at, &gen, &inertes)?;

        let idx = Arc::new(FerriteIndex {
            name: name.to_string(),
            uuid,
            created_at,
            dir,
            current: RwLock::new(gen),
            retirees: Mutex::new(Vec::new()),
            docs: RwLock::new(HashMap::new()),
            inertes: RwLock::new(inertes),
            seq_counter: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            supprime: AtomicBool::new(false),
            refresh_lock: Mutex::new(()),
        });
        guard.insert(name.to_string(), idx.clone());
        Ok(idx)
    }

    pub fn delete(&self, name: &str) -> EsResult<()> {
        let idx = {
            let mut guard = self.indices.write().expect("catalog lock");
            let Some(idx) = guard.remove(name) else {
                return Err(EsError::index_not_found(name));
            };
            idx
        };
        // Retire du catalogue ne veut pas dire mort : la boucle de fond en tient
        // un `Arc`. Le marquage attend qu'un rafraichissement en cours ait fini,
        // donc il se fait le verrou du catalogue relache — sinon toute requete
        // attendrait la fin de ce commit.
        idx.marquer_supprime();
        let dir = idx.dir.clone();

        // Un alias qui ne designerait plus que des index disparus rendrait 404
        // a la premiere recherche : il part avec l'index.
        self.purger_alias(name);

        // Liberer le nom d'abord, effacer ensuite — et le renommage est
        // atomique. Effacer sur place ne suffisait pas : apres un commit,
        // tantivy poursuit ses fusions et son ramassage dans le repertoire, et
        // `remove_dir_all` y tombait sur « Directory not empty ». Pire, un
        // index du **meme nom** recree juste apres reprenait ces chemins
        // (`{index}/index-0`) pendant que l'ancien y travaillait encore. Sous
        // la corbeille, plus aucun chemin n'est partage.
        let corbeille = self.root.join(CORBEILLE);
        fs::create_dir_all(&corbeille)
            .map_err(|e| EsError::internal(format!("creation de {corbeille:?}: {e}")))?;
        let tombe = corbeille.join(util::random_uuid());
        fs::rename(&dir, &tombe)
            .map_err(|e| EsError::internal(format!("suppression de {dir:?}: {e}")))?;
        // L'effacement, lui, a le droit d'echouer : le nom est deja rendu, et
        // ce qui reste est balaye a la prochaine ouverture du catalogue.
        let _ = fs::remove_dir_all(&tombe);
        Ok(())
    }

    /// Rafraichit les index qui ont des ecritures en attente. Appele par la
    /// boucle de fond (`index.refresh_interval` d'ES, en plus simple).
    pub fn refresh_dirty(&self) {
        for idx in self.list() {
            // `index.refresh_interval: -1` demande a ne **pas** rafraichir tout
            // seul : c'est la seule valeur de ce reglage qui change quelque
            // chose ici, et elle est appliquee plutot qu'acceptee et ignoree.
            // Un `POST /{index}/_refresh` explicite passe toujours.
            if crate::reglages::rafraichissement_desactive(&idx.inertes()) {
                idx.balayer_generations_retirees();
                continue;
            }
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
    mapping.analysis.register(index.tokenizers());
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
fn ecrire_meta(
    dir: &Path,
    uuid: &str,
    created_at: i64,
    gen: &Generation,
    inertes: &BTreeMap<String, String>,
) -> EsResult<()> {
    let meta = json!({
        "uuid": uuid,
        "created_at": created_at,
        "ferrite_version": crate::FERRITE_VERSION,
        "generation": gen.seq,
        "mappings": gen.mapping.to_json(),
        // `_mapping` ne rend pas les analyzers : ils vivent dans les settings.
        // Il faut donc les persister a part, sinon un redemarrage perdrait le
        // nom que les champs citent.
        "analysis": gen.mapping.analysis.to_json(),
        // Meme raison pour les reglages exploites : un redemarrage qui perdrait
        // `allow_unmapped_fields` changerait le comportement des recherches sans
        // que personne n'ait rien demande.
        "settings": {"index": {"query": {"parse": {
            "allow_unmapped_fields": gen.mapping.allow_unmapped_fields,
        }}}},
        // Les reglages acceptes sans effet : ils ne changent rien a ce que
        // l'index repond, mais `GET /{index}/_settings` les rend, donc les
        // perdre au redemarrage ferait mentir cette route.
        "reglages_inertes": inertes,
    });
    let tmp = dir.join(format!("{META_FILE}.tmp"));
    fs::write(&tmp, serde_json::to_vec_pretty(&meta).unwrap())
        .map_err(|e| EsError::internal(format!("ecriture du mapping: {e}")))?;
    fs::rename(&tmp, dir.join(META_FILE))
        .map_err(|e| EsError::internal(format!("bascule du mapping: {e}")))?;
    Ok(())
}

/// Les alias qu'un template pose sur l'index qu'il vient de faire naitre.
fn alias_du_template(
    index: &str,
    tpl: Option<&crate::templates::Template>,
) -> EsResult<Option<Vec<ActionAlias>>> {
    let Some(aliases) = tpl.and_then(|t| t.aliases.as_ref()) else {
        return Ok(None);
    };
    let Some(o) = aliases.as_object() else {
        return Ok(None);
    };
    let mut actions = Vec::new();
    for (nom, corps) in o {
        crate::alias::valider_nom(nom)?;
        actions.push(ActionAlias::Ajouter {
            index: index.to_string(),
            alias: nom.clone(),
            attache: crate::alias::lire_attache(corps, "template.aliases")?,
        });
    }
    Ok((!actions.is_empty()).then_some(actions))
}

/// `null` efface un reglage, comme chez ES ; toute autre valeur le pose.
fn appliquer(cible: &mut BTreeMap<String, Value>, demande: &BTreeMap<String, Value>) {
    for (cle, valeur) in demande {
        if valeur.is_null() {
            cible.remove(cle);
        } else {
            cible.insert(cle.clone(), valeur.clone());
        }
    }
}

fn lire_reglages(racine: &Path) -> BTreeMap<String, Value> {
    let Ok(raw) = fs::read(racine.join(REGLAGES_FILE)) else {
        return BTreeMap::new();
    };
    serde_json::from_slice::<BTreeMap<String, Value>>(&raw).unwrap_or_default()
}

fn ecrire_reglages(racine: &Path, reglages: &BTreeMap<String, Value>) -> EsResult<()> {
    let tmp = racine.join(format!("{REGLAGES_FILE}.tmp"));
    fs::write(&tmp, serde_json::to_vec_pretty(reglages).unwrap())
        .map_err(|e| EsError::internal(format!("ecriture des reglages: {e}")))?;
    fs::rename(&tmp, racine.join(REGLAGES_FILE))
        .map_err(|e| EsError::internal(format!("bascule des reglages: {e}")))?;
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
    let inertes: BTreeMap<String, String> = meta
        .get("reglages_inertes")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    // `index.max_ngram_diff` valide la section `analysis` : le relire avant
    // elle, sinon un index cree avec un ecart eleve ne rouvrirait pas.
    let declares = match meta.get("analysis") {
        Some(a) if !a.is_null() => {
            crate::analysis::Analysis::parse(a, crate::reglages::max_ngram_diff(&inertes))?
        }
        _ => crate::analysis::Analysis::default(),
    };
    let mut mapping = Mapping::parse_avec(&meta["mappings"], &declares)?;
    // Absent des index crees avant que le reglage n'existe : le defaut d'ES
    // s'applique alors, comme pour un index cree aujourd'hui sans le poser.
    if let Some(v) = meta
        .pointer("/settings/index/query/parse/allow_unmapped_fields")
        .and_then(Value::as_bool)
    {
        mapping.allow_unmapped_fields = v;
    }

    let gen_dir = dir.join(format!("{INDEX_DIR_PREFIX}{seq}"));
    let (schema, fields) = mapping::build_schema(&mapping);
    let index = Index::open_in_dir(&gen_dir)?;
    crate::analysis::register_all(index.tokenizers());
    mapping.analysis.register(index.tokenizers());
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
        inertes: RwLock::new(inertes),
        seq_counter: AtomicU64::new(next_seq),
        dirty: AtomicBool::new(false),
        supprime: AtomicBool::new(false),
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

/// Le schema d'« aucun index » : un index tantivy vide, en memoire, avec le
/// mapping par defaut.
///
/// Il n'est jamais interroge — il n'a aucun document et n'en aura jamais. Il
/// existe pour **valider** : une recherche qui ne vise aucun index (cluster
/// vide, motif sans correspondance) n'avait aucune generation ou exercer la
/// traduction du Query DSL, donc son corps n'etait pas lu du tout et une
/// requete invalide rendait 200. C'etait le seul endroit connu ou la regle
/// « jamais d'echec silencieux » ne tenait pas.
pub struct SansIndex {
    pub mapping: Mapping,
    pub fields: Fields,
    pub index: Index,
    reader: IndexReader,
}

impl SansIndex {
    pub fn searcher(&self) -> Searcher {
        self.reader.searcher()
    }
}

/// Le schema d'« aucun index », construit une seule fois pour tout le
/// processus (voir [`SansIndex`]).
///
/// Pas de `writer` : rien n'y sera jamais ecrit, et un `IndexWriter` couterait
/// un budget memoire et un thread pour un index qui ne sert qu'a lire une
/// requete.
pub fn sans_index() -> &'static SansIndex {
    static VIDE: std::sync::LazyLock<SansIndex> = std::sync::LazyLock::new(|| {
        let mapping = Mapping::default();
        let (schema, fields) = mapping::build_schema(&mapping);
        let index = Index::create_in_ram(schema);
        crate::analysis::register_all(index.tokenizers());
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .expect("reader sur un index en memoire");
        SansIndex {
            mapping,
            fields,
            index,
            reader,
        }
    });
    &VIDE
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
