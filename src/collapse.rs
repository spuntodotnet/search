//! `collapse` : un seul document par valeur de champ.
//!
//! Le parametre a l'air d'un `terms` deguise, et il ne l'est pas : il ne
//! regroupe rien, il **choisit un representant** dans la liste deja ordonnee et
//! jette les autres. Trois consequences, toutes mesurees contre un ES 8.15 et
//! aucune devinable :
//!
//! * `hits.total` reste celui d'**avant** repliement. Un catalogue qui affiche
//!   « 7 resultats » et trois lignes n'est pas en panne : c'est ce que rend ES,
//!   et le compter apres coup cote client donnerait un autre nombre ;
//! * `from` et `size` paginent les **groupes**, pas les documents. La page 2
//!   d'un repliement n'est donc pas la page 2 de la recherche ;
//! * les documents **sans valeur** pour le champ ne disparaissent pas et ne
//!   sortent pas non plus un par un : ils forment **un** groupe, dont le
//!   representant ne porte aucune entree dans `fields`.
//!
//! Et un quatrieme, qui est un echec franc : un champ **multivalue** fait
//! tomber la recherche entiere chez ES (`the grouping field must be single
//! valued`), parce que le collecteur de Lucene ne sait pas dire de quel groupe
//! un document a deux valeurs releve. Choisir la premiere valeur — la tentation
//! evidente — rendrait un repliement plausible et faux, en 200.
//!
//! `inner_hits` ramene les documents replies. Ils ne sont pas relus par une
//! seconde recherche : les candidats du groupe sont deja collectes, et c'est ce
//! qui garantit qu'ils voient exactement ce que la page voit — y compris le
//! `post_filter`, qui **filtre aussi les `inner_hits`** (mesure : un
//! `post_filter` sur `p >= 3` fait tomber le total d'un groupe de 6 a 4).

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use crate::error::{EsError, EsResult};
use crate::mapping::{FieldKind, Fields};
use crate::search::{SortSpec, SourceFilter, ValeurRepli};

/// Ce que le corps demande, avant tout mapping.
#[derive(Debug, Clone)]
pub struct Demande {
    pub champ: String,
    pub blocs: Vec<Bloc>,
}

/// Un bloc `inner_hits` : les documents replies d'un groupe.
#[derive(Debug, Clone)]
pub struct Bloc {
    pub nom: String,
    pub from: usize,
    /// Le defaut d'ES est **3**, pas 10 : mesure sur un groupe de six.
    pub size: usize,
    /// Le `sort` brut. Il se resout dans le mapping de chaque index, comme
    /// celui de la racine.
    pub sort: Option<Value>,
    pub source: SourceFilter,
    /// Le second niveau de repliement (`field` seulement : ES refuse un
    /// `inner_hits` a l'interieur, « Invalid token in the inner collapse »).
    pub replie: Option<String>,
}

/// Le defaut d'ES pour `inner_hits.size`.
const TAILLE_INNER: usize = 3;

/// Lit le bloc `collapse` du corps.
///
/// `max_concurrent_group_searches` est **accepte et sans effet** : chez ES il
/// borne le parallelisme des sous-recherches d'`inner_hits`, et ferrite n'en
/// lance aucune (les membres d'un groupe sont deja collectes). Il ne peut donc
/// pas changer une reponse — c'est le meme raisonnement que `preference`.
pub fn lire(v: &Value) -> EsResult<Demande> {
    let Value::Object(o) = v else {
        return Err(EsError::parsing(format!(
            "Unknown key for a {} in [collapse].",
            jeton(v)
        )));
    };
    for cle in o.keys() {
        if !["field", "inner_hits", "max_concurrent_group_searches"].contains(&cle.as_str()) {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "x_content_parse_exception",
                format!("[collapse] unknown field [{cle}]"),
            ));
        }
    }
    if let Some(v) = o.get("max_concurrent_group_searches") {
        if v.as_u64().is_none() {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "x_content_parse_exception",
                "[collapse] failed to parse field [max_concurrent_group_searches]",
            ));
        }
    }
    let champ = match o.get("field") {
        Some(Value::String(s)) => s.clone(),
        // ES tombe ici sur une `NullPointerException` en 500 (« Cannot invoke
        // "Object.hashCode()" because "pk" is null ») : un repliement sans
        // champ n'a pas de cle. Reproduire un plantage n'a pas de sens ; le
        // refus est explicite et porte son type.
        None => {
            return Err(EsError::illegal_argument(
                "[collapse] : [field] est obligatoire",
            ))
        }
        Some(autre) => {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "x_content_parse_exception",
                format!(
                    "[collapse] field doesn't support values of type: {}",
                    jeton(autre)
                ),
            ))
        }
    };
    let blocs = match o.get("inner_hits") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|e| lire_bloc(e, false))
            .collect::<EsResult<_>>()?,
        Some(v) => vec![lire_bloc(v, false)?],
    };
    Ok(Demande { champ, blocs })
}

/// Le nom du jeton JSON, tel qu'ES le donne dans ses messages de parsing.
pub fn jeton(v: &Value) -> &'static str {
    match v {
        Value::Null => "VALUE_NULL",
        Value::Bool(_) => "VALUE_BOOLEAN",
        Value::Number(_) => "VALUE_NUMBER",
        Value::String(_) => "VALUE_STRING",
        Value::Array(_) => "START_ARRAY",
        Value::Object(_) => "START_OBJECT",
    }
}

/// Les cles d'`inner_hits` que ferrite sert. Tout le reste est refuse **en le
/// nommant** : ES les sert, donc les laisser passer rendrait une reponse a
/// laquelle il manquerait ce qui a ete demande.
const CLES_INNER: [&str; 6] = ["name", "from", "size", "sort", "_source", "collapse"];

fn lire_bloc(v: &Value, second_niveau: bool) -> EsResult<Bloc> {
    let Value::Object(o) = v else {
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "x_content_parse_exception",
            "[collapse] failed to parse field [inner_hits]",
        ));
    };
    for cle in o.keys() {
        if CLES_INNER.contains(&cle.as_str()) {
            continue;
        }
        // Un nom qu'ES connait, et que ferrite ne rend pas : le dire, plutot
        // que de renvoyer un bloc `inner_hits` ampute en silence.
        if [
            "highlight",
            "docvalue_fields",
            "fields",
            "stored_fields",
            "script_fields",
            "runtime_mappings",
            "explain",
            "version",
            "seq_no_primary_term",
            "track_scores",
        ]
        .contains(&cle.as_str())
        {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{cle}] dans [collapse.inner_hits] ; parametres \
                 acceptes : {CLES_INNER:?}"
            )));
        }
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "x_content_parse_exception",
            format!("[inner_hits] unknown field [{cle}]"),
        ));
    }
    // `name` est obligatoire, et c'est une mesure : ES rend 400 sur un
    // `inner_hits` sans nom, la ou la documentation le presente comme optionnel.
    let nom =
        match o.get("name") {
            Some(Value::String(s)) => s.clone(),
            _ => return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "x_content_parse_exception",
                "[collapse] failed to parse field [inner_hits] : inner_hits must have a [name]; \
                 set the [name] field in the inner_hits definition",
            )),
        };
    let entier = |cle: &str, defaut: usize| -> EsResult<usize> {
        match o.get(cle) {
            None | Some(Value::Null) => Ok(defaut),
            Some(v) => v
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| {
                    EsError::new(
                        axum::http::StatusCode::BAD_REQUEST,
                        "x_content_parse_exception",
                        format!("[inner_hits] failed to parse field [{cle}]"),
                    )
                }),
        }
    };
    let source = match o.get("_source") {
        None | Some(Value::Null) => SourceFilter::All,
        Some(v) => crate::api::search::parse_source_body(v)?,
    };
    let replie = match o.get("collapse") {
        None | Some(Value::Null) => None,
        // ES s'arrete a deux niveaux, et il le dit avec cette phrase-la.
        Some(_) if second_niveau => {
            return Err(EsError::parsing("Invalid token in the inner collapse"))
        }
        Some(Value::Object(c)) => {
            for cle in c.keys() {
                if cle != "field" {
                    return Err(EsError::parsing("Invalid token in the inner collapse"));
                }
            }
            match c.get("field") {
                Some(Value::String(s)) => Some(s.clone()),
                _ => return Err(EsError::parsing("Invalid token in the inner collapse")),
            }
        }
        Some(_) => return Err(EsError::parsing("Invalid token in the inner collapse")),
    };
    Ok(Bloc {
        nom,
        from: entier("from", 0)?,
        size: entier("size", TAILLE_INNER)?,
        sort: o.get("sort").cloned(),
        source,
        replie,
    })
}

// ---------------------------------------------------------------------------
// Resolution sur un mapping
// ---------------------------------------------------------------------------

/// Une colonne a lire pour chaque document collecte.
#[derive(Debug, Clone)]
pub struct Colonne {
    pub champ: String,
    pub genre: FieldKind,
}

/// Un bloc `inner_hits`, resolu dans le mapping d'**un** index.
#[derive(Debug, Clone)]
pub struct BlocResolu {
    pub nom: String,
    pub from: usize,
    pub size: usize,
    pub source: SourceFilter,
    /// Les cles de tri, resolues ici. Vide : les membres sortent dans l'ordre
    /// de la recherche (le score, decroissant).
    pub tri: Vec<SortSpec>,
    /// Ou commencent ces cles dans le tableau `keys` d'un candidat.
    pub debut: usize,
    /// Le rang, dans [`Plan::colonnes`], du second niveau de repliement.
    pub colonne: Option<usize>,
    /// Le nom du champ de ce second niveau, tel que `fields` le porte.
    pub champ2: String,
}

/// Le repliement, resolu dans le mapping d'un index.
#[derive(Debug, Clone)]
pub struct Plan {
    pub champ: String,
    /// Les colonnes a lire : la racine en 0, puis les seconds niveaux.
    pub colonnes: Vec<Colonne>,
    pub blocs: Vec<BlocResolu>,
}

impl Plan {
    /// Les cles de tri a collecter en plus de celles de la racine, dans
    /// l'ordre ou les blocs les liront.
    pub fn tris(&self) -> Vec<SortSpec> {
        self.blocs.iter().flat_map(|b| b.tri.clone()).collect()
    }
}

/// Le champ replie existe-t-il, et ES sait-il replier dessus ?
///
/// Les trois refus portent **sa** phrase, avec ses guillemets obliques et ses
/// crochets, parce qu'ils sont les siens. Le quatrieme est celui de ferrite :
/// sous un `nested`, ES rend 200 en rangeant **tous** les documents racine dans
/// le groupe « sans valeur » (ses elements sont des documents a part, donc la
/// racine n'a pas de colonne) ; ferrite, lui, a bien une colonne a cet endroit
/// et replierait sur autre chose. Le dire est le seul choix honnete — c'est la
/// meme regle que pour le tri et les agregations.
pub fn valider_champ(champs: &Fields, nom: &str, avec_inner: bool) -> EsResult<FieldKind> {
    if champs.racine_nested(nom).is_some() {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [collapse] sur un sous-champ de [nested] ([{nom}]) ; ES y \
             range tous les documents dans le groupe sans valeur, ferrite replierait sur les \
             valeurs des elements"
        ))
        .sur_un_shard());
    }
    let Some(m) = champs.get(nom) else {
        return Err(EsError::illegal_argument(format!(
            "no mapping found for `{nom}` in order to collapse on"
        ))
        .sur_un_shard()
        .sur_champ_inconnu(nom));
    };
    // Replier sur un champ **non indexe** marche : il garde sa colonne, et ES
    // s'en sert. Mais en ramener les documents replies, non — chez ES un
    // `inner_hits` est une vraie sous-recherche, qui a besoin de l'index
    // inverse. La regle n'est donc pas « ce champ se replie » mais « ce champ
    // se replie *avec* ou *sans* ses membres », et elle se mesure : sans
    // `inner_hits` il repond 200, avec il refuse (trouve par le fuzzer, quatre
    // graines, toutes rendues **en silence** par ferrite).
    if avec_inner && !m.indexe {
        return Err(EsError::illegal_argument(format!(
            "cannot expand `inner_hits` for collapse field `{nom}`, only indexed field can \
             retrieve `inner_hits`"
        ))
        .sur_un_shard());
    }
    match m.ty.kind() {
        FieldKind::Keyword | FieldKind::I64 | FieldKind::F64 => Ok(m.ty.kind()),
        // `text`, `date` et `boolean` : ES refuse les trois, avec cette phrase.
        _ => Err(EsError::illegal_argument(format!(
            "collapse is not supported for the field [{nom}] of the type [{}]",
            m.ty.name()
        ))
        .sur_un_shard()),
    }
}

/// Resout le repliement — et le tri de chacun de ses blocs — dans un mapping.
///
/// `debut` est le nombre de cles de tri de la racine : les cles des blocs se
/// rangent derriere elles dans le meme tableau.
pub fn resoudre(
    d: &Demande,
    champs: &Fields,
    debut: usize,
    tri_du_bloc: &dyn Fn(&Value, &Fields) -> EsResult<Vec<SortSpec>>,
) -> EsResult<Plan> {
    let genre = valider_champ(champs, &d.champ, !d.blocs.is_empty())?;
    let mut colonnes = vec![Colonne {
        champ: d.champ.clone(),
        genre,
    }];
    let mut blocs = Vec::with_capacity(d.blocs.len());
    let mut curseur = debut;
    for b in &d.blocs {
        let tri = match &b.sort {
            Some(v) => tri_du_bloc(v, champs)?,
            None => Vec::new(),
        };
        let colonne = match &b.replie {
            None => None,
            Some(nom) => {
                // Le second niveau vit **dans** un `inner_hits`, et la regle
                // « seul un champ indexe se developpe » ne s'y applique
                // pourtant **pas** : ES y accepte un champ non indexe, parce
                // qu'il n'a plus de sous-recherche a lancer a ce niveau-la. Le
                // reflexe de generaliser etait faux, et c'est la sonde qui l'a
                // dit — mesure contre ES 8.15.
                let genre = valider_champ(champs, nom, false)?;
                colonnes.push(Colonne {
                    champ: nom.clone(),
                    genre,
                });
                Some(colonnes.len() - 1)
            }
        };
        blocs.push(BlocResolu {
            nom: b.nom.clone(),
            from: b.from,
            size: b.size,
            source: b.source.clone(),
            debut: curseur,
            colonne,
            champ2: b.replie.clone().unwrap_or_default(),
            tri,
        });
        curseur += blocs.last().expect("juste pousse").tri.len();
    }
    Ok(Plan {
        champ: d.champ.clone(),
        colonnes,
        blocs,
    })
}

// ---------------------------------------------------------------------------
// Repliement
// ---------------------------------------------------------------------------

/// Un groupe : le rang de son representant dans la liste ordonnee, et ceux de
/// tous ses membres (le representant compris, en premiere position).
pub struct Groupe {
    pub chef: usize,
    pub membres: Vec<usize>,
}

/// Replie une liste **deja ordonnee** : le premier document de chaque valeur
/// est le representant, les suivants deviennent ses membres.
///
/// `cle` rend la valeur de repliement d'un candidat. [`ValeurRepli::Absente`]
/// est une cle comme une autre — c'est ce qui met tous les documents sans
/// valeur dans **un** groupe, et pas un chacun.
pub fn replier(n: usize, cle: impl Fn(usize) -> ValeurRepli) -> Vec<Groupe> {
    let mut rangs: HashMap<ValeurRepli, usize> = HashMap::new();
    let mut groupes: Vec<Groupe> = Vec::new();
    for i in 0..n {
        match rangs.get(&cle(i)) {
            Some(&g) => groupes[g].membres.push(i),
            None => {
                rangs.insert(cle(i), groupes.len());
                groupes.push(Groupe {
                    chef: i,
                    membres: vec![i],
                });
            }
        }
    }
    groupes
}

/// La valeur de repliement, telle que le bloc `fields` du hit la porte.
///
/// Un document sans valeur n'a **pas** d'entree : ES ne rend pas `null`, il
/// n'ecrit rien — le groupe « sans valeur » se reconnait a l'absence de la cle.
pub fn valeur_json(v: &ValeurRepli) -> Option<Value> {
    match v {
        ValeurRepli::Absente | ValeurRepli::Multiple => None,
        ValeurRepli::Str(s) => Some(json!(s)),
        ValeurRepli::I64(n) => Some(json!(n)),
        ValeurRepli::F64(bits) => Some(json!(f64::from_bits(*bits))),
    }
}

/// Pose la valeur de repliement dans le bloc `fields` d'un hit deja construit.
///
/// ES l'ajoute **apres** ce que `fields` et `docvalue_fields` ont rendu, dans
/// le meme objet — et cree l'objet s'il n'existait pas.
pub fn poser_valeur(hit: &mut Map<String, Value>, champ: &str, v: &ValeurRepli) {
    let Some(val) = valeur_json(v) else { return };
    let bloc = hit
        .entry("fields".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(o) = bloc {
        o.insert(champ.to_string(), Value::Array(vec![val]));
    }
}

/// Ce qu'un `inner_hits` rend : la meme forme qu'un `hits` de premier niveau.
pub fn bloc_inner(total: usize, max_score: Option<f32>, hits: Vec<Value>) -> Value {
    json!({
        "hits": {
            "total": {"value": total, "relation": "eq"},
            "max_score": max_score.map(crate::search::round_score),
            "hits": hits,
        }
    })
}

/// L'erreur qu'ES rend sur un champ de repliement multivalue.
///
/// Elle nomme le document par son numero **interne** : celui de Lucene chez ES,
/// celui de tantivy ici, et les deux n'ont aucune raison de coincider. Le type,
/// le statut et le reste de la phrase sont les siens ; le numero est declare
/// dans `docs/compat.md`.
pub fn multivalue(doc: u32) -> EsError {
    EsError::new(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "illegal_state_exception",
        format!("failed to extract doc:{doc}, the grouping field must be single valued"),
    )
    .sur_un_shard()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cles(v: &[ValeurRepli]) -> Vec<(usize, Vec<usize>)> {
        replier(v.len(), |i| v[i].clone())
            .into_iter()
            .map(|g| (g.chef, g.membres))
            .collect()
    }

    /// Le representant est le **premier** de la liste ordonnee, et les
    /// documents sans valeur font un seul groupe — pas un chacun (mesure contre
    /// ES 8.15 : deux documents sans valeur rendent un hit et un `inner_hits`
    /// de total 2).
    #[test]
    fn un_groupe_pour_les_absents() {
        let v = [
            ValeurRepli::Str("a".into()),
            ValeurRepli::Str("a".into()),
            ValeurRepli::Absente,
            ValeurRepli::Str("b".into()),
            ValeurRepli::Absente,
        ];
        assert_eq!(
            cles(&v),
            vec![(0, vec![0, 1]), (2, vec![2, 4]), (3, vec![3])]
        );
    }

    /// Deux `NaN` ne sont pas egaux en arithmetique ; deux documents qui
    /// portent `NaN` sont pourtant le meme groupe. La cle passe donc par les
    /// bits.
    #[test]
    fn nan_est_une_cle() {
        let n = ValeurRepli::F64(f64::NAN.to_bits());
        assert_eq!(cles(&[n.clone(), n]), vec![(0, vec![0, 1])]);
    }

    /// Un champ sans valeur ne pose **pas** `null` dans `fields` : il ne pose
    /// rien du tout.
    #[test]
    fn absente_ne_pose_rien() {
        let mut hit = Map::new();
        poser_valeur(&mut hit, "m", &ValeurRepli::Absente);
        assert!(hit.is_empty());
        poser_valeur(&mut hit, "m", &ValeurRepli::Str("a".into()));
        assert_eq!(hit["fields"], json!({"m": ["a"]}));
    }

    /// Le second niveau s'arrete la : ES refuse un `inner_hits` sous le
    /// `collapse` d'un `inner_hits`, avec cette phrase.
    #[test]
    fn deux_niveaux_au_plus() {
        let e = lire(&json!({
            "field": "m",
            "inner_hits": {"name": "g", "collapse": {"field": "c", "inner_hits": {"name": "h"}}}
        }))
        .unwrap_err();
        assert_eq!(e.reason, "Invalid token in the inner collapse");
    }

    /// `size` vaut 3 par defaut dans un `inner_hits`, pas 10.
    #[test]
    fn taille_par_defaut() {
        let d = lire(&json!({"field": "m", "inner_hits": {"name": "g"}})).unwrap();
        assert_eq!(d.blocs[0].size, 3);
        assert_eq!(d.blocs[0].from, 0);
    }

    /// `name` est obligatoire — mesure, la documentation d'ES le presente comme
    /// optionnel.
    #[test]
    fn nom_obligatoire() {
        let e = lire(&json!({"field": "m", "inner_hits": {"size": 2}})).unwrap_err();
        assert!(
            e.reason.contains("inner_hits must have a [name]"),
            "{}",
            e.reason
        );
    }
}
