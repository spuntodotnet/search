//! `_search` : parametres, execution, reponse au format exact d'Elasticsearch.

use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::Uri;
use serde_json::{json, Map, Value};

use super::{elapsed_ms, expect_only, parse_body, selection_options, Json, Params, SharedState};
use crate::dsl::{build_query, QueryCtx};
use crate::engine::Generation;
use crate::error::{EsError, EsResult};
use crate::fetch::{self, Demande, Stored};
use crate::mapping::FieldKind;
use crate::scroll;
use crate::search::{
    balayer, execute, rendre_page, round_score, Cible, Rendu, SearchRequest, SortKey, SortSpec,
    SourceFilter,
};
use crate::selection::resoudre;
use crate::MAX_RESULT_WINDOW;

const DEFAULT_SIZE: usize = 10;

/// `POST|GET /_search` — sans index dans l'URL, ES cherche partout.
pub async fn search_all(State(st): State<SharedState>, uri: Uri, body: Bytes) -> EsResult<Json> {
    search(State(st), Path("_all".to_string()), uri, body).await
}

/// `POST|GET /{index}/_search`
///
/// `{index}` est une **expression** : un nom, un alias, une liste, un motif.
/// Voir [`crate::selection`].
pub async fn search(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let started = Instant::now();

    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    // `?scroll=1m` : la duree de vie du contexte a ouvrir. Sa presence change la
    // nature de l'appel — plus une page, mais le debut d'un balayage.
    let keep_alive = match p.opt("scroll") {
        Some(v) => Some(scroll::duree(&v, "scroll")?),
        None => None,
    };
    let param_from = p.number("from")?;
    let param_size = p.number("size")?;
    let param_sort = p.list("sort");
    let param_source = source_filter_opt(&mut p)?;
    // `?docvalue_fields=` et `?stored_fields=` existent chez ES ; `?fields=`
    // **non** — il le refuse comme un parametre inconnu, et ferrite fait de
    // meme en ne le lisant nulle part.
    let param_docvalue = p.list("docvalue_fields");
    let param_stored = p.list("stored_fields");
    // `preference` choisit un shard : ferrite n'en a qu'un, le parametre est
    // donc sans objet — pas ignore, juste sans effet possible.
    p.opt("preference");
    if let Some(v) = p.opt("track_total_hits") {
        check_track_total_hits(&Value::String(v))?;
    }
    if p.opt("q").is_some() {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas la recherche par chaine [q] (query_string) ; utilise le \
             Query DSL",
        ));
    }
    // ES **supporte** ce parametre : le refuser comme un parametre inconnu
    // laisserait croire a une faute de frappe. C'est un refus, et il se nomme.
    if p.opt("rest_total_hits_as_int").is_some() {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [rest_total_hits_as_int] : il change la forme de \
             [hits.total] (un nombre au lieu de {value, relation}) ; voir docs/compat-es7.md",
        ));
    }
    p.done()?;

    let indices = resoudre(&st.catalog, &index, &opts)?;
    // Une seule generation par index pour toute la requete : les `Field` d'une
    // generation n'ont aucun sens dans une autre.
    let generations: Vec<(String, String, std::sync::Arc<Generation>)> = indices
        .iter()
        .map(|i| (i.name.clone(), i.uuid.clone(), i.current()))
        .collect();
    let champs_connus = union_des_champs(&generations);

    let body = parse_body(&body)?;
    let body_obj = match &body {
        Value::Null => Map::new(),
        Value::Object(o) => o.clone(),
        _ => return Err(EsError::parsing("le corps de [_search] doit etre un objet")),
    };
    expect_only(
        &body_obj,
        &[
            "query",
            "from",
            "size",
            "sort",
            "_source",
            "track_total_hits",
            "aggs",
            "aggregations",
            "fields",
            "docvalue_fields",
            "stored_fields",
            "script_fields",
            "runtime_mappings",
        ],
        "_search",
    )?;

    if let Some(v) = body_obj.get("track_total_hits") {
        check_track_total_hits(v)?;
    }

    let demande = lire_demande(&body_obj, param_docvalue, param_stored)?;

    let from = match param_from {
        Some(v) => v,
        None => body_usize(&body_obj, "from")?.unwrap_or(0),
    };
    let size = match param_size {
        Some(v) => v,
        None => body_usize(&body_obj, "size")?.unwrap_or(DEFAULT_SIZE),
    };

    // `from` n'a pas de sens dans un scroll : le contexte avance tout seul, et
    // sauter des documents ferait perdre en silence ceux d'avant. ES le refuse,
    // avec ce type d'erreur et ce message.
    if keep_alive.is_some() && from != 0 {
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "action_request_validation_exception",
            "Validation Failed: 1: using [from] is not allowed in a scroll context;",
        ));
    }
    if from + size > MAX_RESULT_WINDOW {
        return Err(EsError::illegal_argument(format!(
            "Result window is too large, from + size must be less than or equal to: \
             [{MAX_RESULT_WINDOW}] but was [{}].",
            from + size
        )));
    }

    let source = match param_source {
        Some(f) => f,
        None => match body_obj.get("_source") {
            Some(v) => parse_source_body(v)?,
            // `stored_fields` demande des champs stockes un par un : ES ne rend
            // alors plus `_source`, sauf s'il est demande explicitement.
            None if demande.retire_le_source() => SourceFilter::None,
            None => SourceFilter::All,
        },
    };

    // `aggs` et `aggregations` sont deux noms pour la meme chose chez ES.
    let aggs = match (body_obj.get("aggs"), body_obj.get("aggregations")) {
        (Some(_), Some(_)) => {
            return Err(EsError::illegal_argument(
                "[aggs] et [aggregations] sont synonymes : n'en fournis qu'un",
            ))
        }
        (Some(a), None) | (None, Some(a)) => Some(a.clone()),
        (None, None) => None,
    };

    // `now` est resolu une fois pour toute la recherche, comme ES le fait sur
    // son noeud coordinateur : les index vises doivent tous repondre a la meme
    // question.
    let maintenant = crate::datemath::maintenant();

    // Aucun index vise : la boucle ci-dessous ne tourne pas, donc rien du corps
    // ne serait lu. Il est valide a part, contre un schema vide.
    if generations.is_empty() {
        valider_sans_index(
            body_obj.get("query"),
            aggs.as_ref(),
            param_sort.as_deref(),
            body_obj.get("sort"),
            maintenant,
        )?;
    }

    // Une cible par index vise, chacune avec sa requete, ses cles de tri et son
    // verdict sur les agregations.
    let mut cibles: Vec<Cible> = Vec::new();
    let mut ignore: Option<EsError> = None;
    let mut agg_ignore: Option<EsError> = None;
    let mut sort_asc: Vec<bool> = Vec::new();
    // Les index qui n'ont pas pu repondre, au format `_shards.failures` d'ES.
    let mut echecs: Vec<Value> = Vec::new();
    let nb_index = generations.len();

    for (nom, uuid, gen) in generations {
        let sort = match param_sort.as_ref() {
            Some(list) => parse_sort_params(list, &gen.fields),
            None => match body_obj.get("sort") {
                Some(v) => parse_sort_body(v, &gen.fields),
                None => Ok(Vec::new()),
            },
        };
        let sort = match sort {
            Ok(s) => s,
            // Trier sur un champ que cet index ne mappe pas : ES ne fait pas
            // echouer la recherche, il rapporte l'echec **de ce shard** et rend
            // les documents des autres. Ecarter l'index en silence rendrait le
            // meme total qu'ES mais sans dire qu'il manque quelque chose ; le
            // faire echouer entierement rendrait moins de documents qu'ES.
            Err(e) if e.champ_inconnu.is_some() => {
                echecs.push(echec_de_shard(&nom, &uuid, &e, &st.catalog.cluster_uuid));
                continue;
            }
            Err(e) => return Err(e),
        };
        // Le meme contexte de traduction sert a la requete et aux requetes
        // internes des agregations [filter] : toutes doivent etre construites
        // dans **cette** generation, leurs `Field` n'ont de sens que la.
        let searcher = gen.searcher();
        let ctx = QueryCtx::new(&gen.fields, &gen.index, &searcher)
            .avec_champs_ailleurs(&champs_connus)
            .avec_maintenant(maintenant)
            .selon_le_mapping(&gen.mapping);
        let query = match body_obj.get("query") {
            Some(v) => build_query(v, &ctx),
            None => Ok(Box::new(tantivy::query::AllQuery) as Box<dyn tantivy::query::Query>),
        };
        let query = match query {
            Ok(q) => q,
            // Ici, le champ n'est connu **d'aucun** index vise : ce n'est plus
            // un mapping heterogene, c'est une faute de frappe. L'erreur est
            // gardee et rendue une fois la boucle finie.
            Err(e) if e.champ_inconnu.is_some() => {
                ignore.get_or_insert(e);
                continue;
            }
            Err(e) => return Err(e),
        };
        // Meme raisonnement pour les agregations : un index qui ne mappe pas le
        // champ agrege n'a aucune valeur a y verser.
        let (agrege, filtres) = match &aggs {
            None => (false, crate::aggs::Filtres::default()),
            Some(a) => match crate::aggs::validate(a, Some(&gen.fields), &ctx) {
                Ok(filtres) => (true, filtres),
                Err(e) if e.champ_inconnu.is_some() => {
                    agg_ignore.get_or_insert(e);
                    (false, crate::aggs::Filtres::default())
                }
                Err(e) => return Err(e),
            },
        };
        // `fields` et `docvalue_fields` se resolvent sur **ce** mapping : un
        // `format` sur un `keyword`, un `docvalue_fields` sur un `text` sont
        // des echecs de shard chez ES, pas des erreurs de requete.
        //
        // Et ils n'ont lieu qu'a la phase de *fetch* : une recherche qui ne
        // ramene aucun document rend 200 malgre eux (mesure contre ES 8.15).
        // L'erreur est donc portee par le plan et levee au moment ou un hit de
        // cet index est rendu — enveloppee des maintenant dans le « all shards
        // failed » d'ES, la seule forme qu'il donne a un echec de fetch.
        let mut plan = fetch::resoudre(&demande, &gen, &nom)?;
        if let Some(e) = plan.erreur() {
            let echec = echec_de_shard(&nom, &uuid, e, &st.catalog.cluster_uuid);
            plan.poser_erreur(tous_les_shards_ont_echoue(&[echec]));
        }
        let plan = std::sync::Arc::new(plan);
        if sort_asc.is_empty() {
            sort_asc = sort.iter().map(|s| s.asc).collect();
        }
        cibles.push(Cible {
            nom,
            gen,
            plan,
            query,
            sort,
            agrege,
            filtres,
        });
    }

    if cibles.is_empty() {
        // Aucun index n'a su trier : ES groupe alors les echecs de shard en une
        // seule erreur « all shards failed ».
        if !echecs.is_empty() {
            return Err(tous_les_shards_ont_echoue(&echecs));
        }
        if let Some(e) = ignore {
            return Err(e);
        }
    }
    if aggs.is_some() && !cibles.is_empty() && cibles.iter().all(|c| !c.agrege) {
        if let Some(e) = agg_ignore {
            return Err(e);
        }
    }

    let req = SearchRequest {
        // Sans index vise, ES ne rend pas de section `aggregations` du tout,
        // pas meme vide (mesure contre ES 8.15) — et le corps vient d'etre
        // valide, il n'y a plus rien a calculer.
        aggs: if nb_index == 0 { None } else { aggs },
        from,
        size,
        sort_asc,
        source,
        avec_id: demande.avec_id(),
    };

    // Avec `?scroll=`, la recherche ne rend pas une page : elle ouvre un
    // contexte. Tout est balaye et ordonne une fois, la premiere tranche part
    // au client, le reste attend son `_scroll_id`.
    if let Some(keep_alive) = keep_alive {
        // Les pages suivantes rendent le meme `_shards` que la premiere : le
        // contexte en garde donc une copie.
        let echecs_du_scroll = echecs.clone();
        let (page, contexte, aggregations, total, max_score) =
            tokio::task::spawn_blocking(move || -> EsResult<_> {
                let rendu = Rendu {
                    source: req.source.clone(),
                    avec_id: req.avec_id,
                };
                let b = balayer(cibles, &req)?;
                let fin = req.size.min(b.hits.len());
                let page = rendre_page(&b.cibles, &b.hits[..fin], &rendu, b.trie, b.avec_score)?;
                let (total, max_score) = (b.total, b.max_score);
                let contexte = crate::scroll::Contexte {
                    cibles: b.cibles,
                    hits: b.hits,
                    total,
                    max_score,
                    rendu,
                    trie: b.trie,
                    avec_score: b.avec_score,
                    taille: req.size,
                    position: fin,
                    nb_index,
                    echecs: echecs_du_scroll.clone(),
                    expire: std::time::Instant::now(),
                };
                Ok((page, contexte, b.aggregations, total, max_score))
            })
            .await
            .map_err(|e| EsError::internal(format!("recherche: {e}")))??;

        let id = st.scrolls.ouvrir(contexte, keep_alive)?;
        let mut reponse = reponse_de_page(started, nb_index, echecs, total, max_score, page);
        if let Some(aggs) = aggregations {
            reponse.insert("aggregations".into(), aggs);
        }
        // `_scroll_id` d'abord : c'est l'ordre d'ES, et un humain qui lit la
        // reponse a la console cherche ca en premier.
        let mut avec_id = Map::new();
        avec_id.insert("_scroll_id".into(), json!(id));
        avec_id.extend(reponse);
        return Ok(Json::ok(Value::Object(avec_id)));
    }

    let outcome = tokio::task::spawn_blocking(move || execute(&cibles, &req))
        .await
        .map_err(|e| EsError::internal(format!("recherche: {e}")))??;

    let mut reponse = reponse_de_page(
        started,
        nb_index,
        echecs,
        outcome.total,
        outcome.max_score,
        outcome.hits,
    );
    if let Some(aggs) = outcome.aggregations {
        reponse.insert("aggregations".into(), aggs);
    }
    Ok(Json::ok(Value::Object(reponse)))
}

/// Lit ce que la reponse doit transporter : `fields`, `docvalue_fields`,
/// `stored_fields` — et refuse les deux qui supposent Painless.
///
/// `script_fields` et `runtime_mappings` **vides** sont acceptes : un objet
/// sans entree ne definit aucun champ, donc ne demande rien, et ES rend la meme
/// reponse avec ou sans (mesure contre ES 8.15). Ce n'est pas une complaisance :
/// c'est la forme que 774 requetes du corpus envoient, parce que les gabarits
/// des tracks Rally la laissent vide quand leur parametre n'est pas rempli. Un
/// objet **non** vide, lui, est refuse : il porte un script Painless (18 des 19
/// occurrences non vides du corpus), qui est hors du perimetre declare.
fn lire_demande(
    body: &Map<String, Value>,
    param_docvalue: Option<Vec<String>>,
    param_stored: Option<Vec<String>>,
) -> EsResult<Demande> {
    for cle in ["script_fields", "runtime_mappings"] {
        match body.get(cle) {
            None | Some(Value::Null) => {}
            Some(Value::Object(o)) if o.is_empty() => {}
            Some(_) => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas [{cle}] dans [_search] : il definit des champs \
                     calcules par un script Painless, que ferrite n'execute pas (seul l'objet \
                     vide, qui ne definit rien, est accepte)"
                )))
            }
        }
    }

    let mut demande = Demande::default();
    if let Some(v) = body.get("fields") {
        demande.fields = fetch::lire_champs(v, "fields")?;
    }
    demande.docvalue = match (body.get("docvalue_fields"), param_docvalue) {
        (Some(v), _) => fetch::lire_champs(v, "docvalue_fields")?,
        (None, Some(liste)) => liste
            .into_iter()
            .map(|motif| fetch::Champ {
                motif,
                format: None,
                include_unmapped: false,
            })
            .collect(),
        (None, None) => Vec::new(),
    };
    demande.stored = match (body.get("stored_fields"), param_stored) {
        (Some(v), _) => fetch::lire_stored(v)?,
        (None, Some(liste)) => fetch::stored_des_params(&liste),
        (None, None) => Stored::Absent,
    };

    // Retirer les champs stockes et en demander en meme temps est contradictoire
    // — `fields` en a besoin pour lire le `_source`. Le message est celui d'ES,
    // type compris.
    if demande.stored == Stored::Aucun && !demande.fields.is_empty() {
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "action_request_validation_exception",
            "Validation Failed: 1: [stored_fields] cannot be disabled when using the [fields] \
             option;",
        ));
    }
    Ok(demande)
}

/// Valide le corps d'une recherche qui ne vise **aucun** index.
///
/// Sans index, il n'y a aucune generation ou traduire la requete : le corps
/// n'etait donc pas lu du tout, et `{"aggs": {"a": {"significant_terms": …}}}`
/// rendait 200 et des agregations vides la ou le premier index venu le refuse.
/// C'etait le seul echec silencieux connu du projet.
///
/// La traduction est donc exercee contre un schema vide
/// ([`crate::engine::sans_index`]) et seule l'erreur compte : ce qui en sort
/// est jete.
///
/// Ce qui n'en sort pas : les verdicts qui dependent d'un mapping (« champ
/// inconnu », `nested` sur un chemin, `has_child` sans champ `join`). ES les
/// rend a l'execution d'un shard, et il n'y a pas de shard — mesure contre ES
/// 8.15 sur un cluster vide, qui rend 200 sur `{"sort": ["absent"]}` comme sur
/// `{"query": {"term": {"absent": "x"}}}`, et 400 sur `unknown query
/// [pas_une_query]` comme sur `Unknown aggregation type [pas_une_agg]`.
fn valider_sans_index(
    query: Option<&Value>,
    aggs: Option<&Value>,
    param_sort: Option<&[String]>,
    body_sort: Option<&Value>,
    maintenant: i64,
) -> EsResult<()> {
    let vide = crate::engine::sans_index();
    let searcher = vide.searcher();
    let ctx = QueryCtx::new(&vide.fields, &vide.index, &searcher)
        .avec_maintenant(maintenant)
        .selon_le_mapping(&vide.mapping)
        .sans_index_vise();

    match (param_sort, body_sort) {
        (Some(list), _) => sans_verdict_de_mapping(parse_sort_params(list, &vide.fields))?,
        (None, Some(v)) => sans_verdict_de_mapping(parse_sort_body(v, &vide.fields))?,
        (None, None) => {}
    }
    if let Some(v) = query {
        sans_verdict_de_mapping(build_query(v, &ctx))?;
    }
    if let Some(a) = aggs {
        sans_verdict_de_mapping(crate::aggs::validate(a, None, &ctx))?;
    }
    Ok(())
}

/// Jette ce qu'une validation sans index a produit, et avec lui les erreurs
/// qu'aucun mapping ne peut trancher (voir [`valider_sans_index`]).
fn sans_verdict_de_mapping<T>(r: EsResult<T>) -> EsResult<()> {
    match r {
        Ok(_) => Ok(()),
        // `query_shard_exception` est precisement le type qu'ES reserve a ce
        // qu'un shard decide : sans shard, il n'y a pas de verdict a rendre.
        Err(e) if e.ty == "query_shard_exception" || e.champ_inconnu.is_some() => Ok(()),
        Err(e) => Err(e),
    }
}

/// Le corps commun d'une reponse de recherche : `took`, `_shards`, `hits`.
///
/// Une page de `scroll` a exactement la meme forme qu'une reponse de `_search`,
/// `_scroll_id` en plus — y compris le `_shards` de la recherche d'origine, que
/// les clients relisent a chaque page (`helpers.scan` s'arrete si un shard a
/// echoue).
fn reponse_de_page(
    started: Instant,
    nb_index: usize,
    echecs: Vec<Value>,
    total: usize,
    max_score: Option<f32>,
    hits: Vec<Value>,
) -> Map<String, Value> {
    let mut reponse = Map::new();
    reponse.insert("took".into(), json!(elapsed_ms(started)));
    reponse.insert("timed_out".into(), json!(false));
    // Un index = un shard : ES compte les shards, pas les index.
    let mut shards = json!({
        "total": nb_index,
        "successful": nb_index - echecs.len(),
        "skipped": 0,
        "failed": echecs.len(),
    });
    if !echecs.is_empty() {
        shards["failures"] = Value::Array(echecs);
    }
    reponse.insert("_shards".into(), shards);
    reponse.insert(
        "hits".into(),
        json!({
            // `total` est un objet {value, relation}, pas un entier : un client
            // type le remarque immediatement.
            "total": {"value": total, "relation": "eq"},
            // `null` dit « aucun document n'a ete score » ; sans le moindre
            // shard, ES rend `0.0` (mesure contre ES 8.15 sur un cluster vide,
            // ou il rend `null` des qu'un index existe).
            "max_score": if nb_index == 0 { Some(0.0) } else { max_score.map(round_score) },
            "hits": hits,
        }),
    );
    reponse
}

/// `POST|GET /_search/scroll` — la page suivante d'un contexte ouvert.
pub async fn scroll_suivant(
    State(st): State<SharedState>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    scroll_avec_id(st, None, uri, body).await
}

/// `POST|GET /_search/scroll/{scroll_id}` — la forme heritee, ou l'identifiant
/// est dans l'URL. Des scripts de 2016 s'en servent encore.
pub async fn scroll_suivant_par_url(
    State(st): State<SharedState>,
    Path(id): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    scroll_avec_id(st, Some(id), uri, body).await
}

async fn scroll_avec_id(
    st: SharedState,
    id_url: Option<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let started = Instant::now();
    let mut p = Params::parse(&uri);
    let mut keep_alive = match p.opt("scroll") {
        Some(v) => Some(scroll::duree(&v, "scroll")?),
        None => None,
    };
    let mut ids: Vec<String> = p.opt("scroll_id").into_iter().collect();
    p.done()?;

    let body = parse_body(&body)?;
    if let Value::Object(obj) = &body {
        expect_only(obj, &["scroll_id", "scroll"], "_search/scroll")?;
        if let Some(v) = obj.get("scroll") {
            let v = v
                .as_str()
                .ok_or_else(|| EsError::illegal_argument("[scroll] : chaine attendue"))?;
            keep_alive = Some(scroll::duree(v, "scroll")?);
        }
        if let Some(v) = obj.get("scroll_id") {
            ids.extend(scroll::ids_du_corps(v, "scroll_id")?);
        }
    } else if !matches!(body, Value::Null) {
        return Err(EsError::parsing(
            "le corps de [_search/scroll] doit etre un objet",
        ));
    }
    if let Some(id) = id_url {
        ids.push(id);
    }
    // Plusieurs identifiants sur une lecture ne veulent rien dire : ES ne sait
    // pas non plus lequel poursuivre.
    let id = match ids.len() {
        1 => ids.remove(0),
        0 => {
            return Err(EsError::illegal_argument(
                "[scroll_id] est obligatoire sur [_search/scroll]",
            ))
        }
        n => {
            return Err(EsError::illegal_argument(format!(
                "[_search/scroll] : un seul [scroll_id] a la fois ({n} fournis)"
            )))
        }
    };

    let suite = st.scrolls.avancer(&id, keep_alive)?;
    let (nb_index, echecs) = (suite.nb_index, suite.echecs.clone());
    let (total, max_score) = (suite.total, suite.max_score);
    let hits = tokio::task::spawn_blocking(move || {
        rendre_page(
            &suite.cibles,
            &suite.hits,
            &suite.rendu,
            suite.trie,
            suite.avec_score,
        )
    })
    .await
    .map_err(|e| EsError::internal(format!("scroll: {e}")))??;

    let mut reponse = Map::new();
    reponse.insert("_scroll_id".into(), json!(id));
    reponse.extend(reponse_de_page(
        started, nb_index, echecs, total, max_score, hits,
    ));
    Ok(Json::ok(Value::Object(reponse)))
}

/// `DELETE /_search/scroll` — rendre les contextes avant leur expiration.
///
/// Un client bien eleve (dont `helpers.scan`) appelle ca a la fin de son
/// export : chaque contexte retient un instantane de l'index.
pub async fn scroll_effacer(
    State(st): State<SharedState>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    effacer_avec_id(st, None, uri, body).await
}

/// `DELETE /_search/scroll/{scroll_id}` — la forme heritee.
pub async fn scroll_effacer_par_url(
    State(st): State<SharedState>,
    Path(id): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    effacer_avec_id(st, Some(id), uri, body).await
}

async fn effacer_avec_id(
    st: SharedState,
    id_url: Option<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let mut ids: Vec<String> = p.list("scroll_id").unwrap_or_default();
    p.done()?;

    let body = parse_body(&body)?;
    if let Value::Object(obj) = &body {
        expect_only(obj, &["scroll_id"], "DELETE /_search/scroll")?;
        if let Some(v) = obj.get("scroll_id") {
            ids.extend(scroll::ids_du_corps(v, "scroll_id")?);
        }
    }
    if let Some(id) = id_url {
        ids.extend(id.split(',').map(str::trim).map(str::to_string));
    }
    if ids.is_empty() {
        return Err(EsError::illegal_argument(
            "[scroll_id] est obligatoire sur [DELETE /_search/scroll]",
        ));
    }
    let liberes = st.scrolls.fermer(&ids);
    // ES rend `succeeded: true` meme quand l'identifiant n'existait plus :
    // fermer deux fois n'est pas une erreur, c'est le cas normal d'un client
    // qui nettoie apres une interruption.
    Ok(Json::ok(json!({"succeeded": true, "num_freed": liberes})))
}

/// `GET|POST /_count` — sans index dans l'URL, ES compte partout.
pub async fn count_all(State(st): State<SharedState>, uri: Uri, body: Bytes) -> EsResult<Json> {
    count(State(st), Path("_all".to_string()), uri, body).await
}

/// `GET|POST /{index}/_count` — combien de documents correspondent.
pub async fn count(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    p.opt("preference");
    if p.opt("q").is_some() {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas la recherche par chaine [q] ; utilise le Query DSL",
        ));
    }
    p.done()?;

    let body = parse_body(&body)?;
    let body_obj = match &body {
        Value::Null => Map::new(),
        Value::Object(o) => o.clone(),
        _ => return Err(EsError::parsing("le corps de [_count] doit etre un objet")),
    };
    expect_only(&body_obj, &["query"], "_count")?;

    let indices = resoudre(&st.catalog, &index, &opts)?;
    let generations: Vec<(String, String, std::sync::Arc<Generation>)> = indices
        .iter()
        .map(|i| (i.name.clone(), i.uuid.clone(), i.current()))
        .collect();
    let champs_connus = union_des_champs(&generations);
    let mut prets: Vec<(std::sync::Arc<Generation>, Box<dyn tantivy::query::Query>)> = Vec::new();
    let mut ignore: Option<EsError> = None;
    let maintenant = crate::datemath::maintenant();
    // Meme trou qu'en recherche : sans index, la boucle ne tourne pas et la
    // requete n'est jamais lue (voir [`valider_sans_index`]).
    if generations.is_empty() {
        valider_sans_index(body_obj.get("query"), None, None, None, maintenant)?;
    }
    for (_, _, gen) in &generations {
        let gen = gen.clone();
        let query = {
            let searcher = gen.searcher();
            let ctx = QueryCtx::new(&gen.fields, &gen.index, &searcher)
                .avec_champs_ailleurs(&champs_connus)
                .avec_maintenant(maintenant)
                .selon_le_mapping(&gen.mapping);
            match body_obj.get("query") {
                Some(v) => build_query(v, &ctx),
                None => Ok(Box::new(tantivy::query::AllQuery) as Box<dyn tantivy::query::Query>),
            }
        };
        match query {
            Ok(q) => prets.push((gen, q)),
            Err(e) if e.champ_inconnu.is_some() => {
                ignore.get_or_insert(e);
            }
            Err(e) => return Err(e),
        }
    }
    if prets.is_empty() {
        if let Some(e) = ignore {
            return Err(e);
        }
    }

    let nb_index = indices.len();
    let total = tokio::task::spawn_blocking(move || {
        let mut total = 0usize;
        for (gen, query) in &prets {
            total += gen.searcher().search(query, &tantivy::collector::Count)?;
        }
        Ok::<_, EsError>(total)
    })
    .await
    .map_err(|e| EsError::internal(format!("count: {e}")))??;

    Ok(Json::ok(json!({
        "count": total,
        "_shards": {"total": nb_index, "successful": nb_index, "skipped": 0, "failed": 0},
    })))
}

/// Tous les noms de champ connus d'au moins un des index vises.
///
/// C'est ce qui distingue « faute de frappe » (personne ne connait ce champ) de
/// « mapping heterogene » (un index quotidien plus recent a un champ de plus).
fn union_des_champs(
    generations: &[(String, String, std::sync::Arc<Generation>)],
) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    // Sur un index unique il n'y a pas d'« ailleurs » : l'ensemble reste vide,
    // et un champ inconnu redevient l'erreur qu'il doit etre.
    if generations.len() < 2 {
        return out;
    }
    for (_, _, gen) in generations {
        out.extend(gen.fields.mapped.keys().cloned());
    }
    out
}

/// L'echec d'un index, au format `_shards.failures[]` d'Elasticsearch.
///
/// Un index = un shard, donc `shard: 0` toujours.
fn echec_de_shard(nom: &str, uuid: &str, e: &EsError, node: &str) -> Value {
    json!({
        "shard": 0,
        "index": nom,
        "node": node,
        "reason": {
            "type": e.ty,
            "reason": e.reason,
            "index_uuid": uuid,
            "index": nom,
        },
    })
}

/// Quand aucun index n'a pu repondre, ES ne rend pas une reponse vide : il rend
/// une erreur qui **groupe** les causes, une par shard.
fn tous_les_shards_ont_echoue(echecs: &[Value]) -> EsError {
    let causes: Vec<Value> = echecs.iter().map(|e| e["reason"].clone()).collect();
    EsError::new(
        axum::http::StatusCode::BAD_REQUEST,
        "search_phase_execution_exception",
        "all shards failed",
    )
    .with("phase", json!("query"))
    .with("grouped", json!(true))
    .with("failed_shards", Value::Array(echecs.to_vec()))
    .avec_racines(causes)
}

/// Lit un entier positif du corps. Une valeur invalide est refusee plutot que
/// remplacee par le defaut : `size: -1` doit se voir, pas devenir `10`.
fn body_usize(obj: &Map<String, Value>, key: &str) -> EsResult<Option<usize>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| {
                EsError::illegal_argument(format!("[{key}] : entier positif attendu, recu {v}"))
            }),
    }
}

/// ferrite compte toujours les hits exactement — `relation` vaut donc toujours
/// `eq`. Seul `false` (ne pas compter) n'a pas d'equivalent.
fn check_track_total_hits(v: &Value) -> EsResult<()> {
    let refused = match v {
        Value::Bool(b) => !*b,
        Value::String(s) => s == "false",
        Value::Number(_) => false,
        _ => true,
    };
    if refused {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [track_total_hits: false] : le total est toujours exact",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// _source
// ---------------------------------------------------------------------------

/// Lit `_source` / `_source_includes` / `_source_excludes` depuis la query
/// string. Renvoie [`SourceFilter::All`] si rien n'est precise.
pub fn source_filter(p: &mut Params) -> EsResult<SourceFilter> {
    Ok(source_filter_opt(p)?.unwrap_or(SourceFilter::All))
}

fn source_filter_opt(p: &mut Params) -> EsResult<Option<SourceFilter>> {
    let includes = p.list("_source_includes").unwrap_or_default();
    let excludes = p.list("_source_excludes").unwrap_or_default();
    let source = p.opt("_source");

    match (source.as_deref(), includes.is_empty(), excludes.is_empty()) {
        (None, true, true) => Ok(None),
        (Some("false"), _, _) => Ok(Some(SourceFilter::None)),
        (Some("true") | None, _, _) => {
            if includes.is_empty() && excludes.is_empty() {
                Ok(Some(SourceFilter::All))
            } else {
                Ok(Some(SourceFilter::Filter { includes, excludes }))
            }
        }
        (Some(list), _, _) => {
            let mut includes = includes;
            includes.extend(list.split(',').map(str::trim).map(str::to_string));
            Ok(Some(SourceFilter::Filter { includes, excludes }))
        }
    }
}

fn parse_source_body(v: &Value) -> EsResult<SourceFilter> {
    match v {
        Value::Bool(true) => Ok(SourceFilter::All),
        Value::Bool(false) => Ok(SourceFilter::None),
        Value::String(s) => Ok(SourceFilter::Filter {
            includes: vec![s.clone()],
            excludes: vec![],
        }),
        Value::Array(a) => Ok(SourceFilter::Filter {
            includes: a
                .iter()
                .map(|x| {
                    x.as_str().map(str::to_string).ok_or_else(|| {
                        EsError::illegal_argument("[_source] : liste de chaines attendue")
                    })
                })
                .collect::<EsResult<_>>()?,
            excludes: vec![],
        }),
        Value::Object(o) => {
            expect_only(o, &["includes", "excludes"], "_source")?;
            let read = |key: &str| -> EsResult<Vec<String>> {
                match o.get(key) {
                    None => Ok(vec![]),
                    Some(Value::String(s)) => Ok(vec![s.clone()]),
                    Some(Value::Array(a)) => a
                        .iter()
                        .map(|x| {
                            x.as_str().map(str::to_string).ok_or_else(|| {
                                EsError::illegal_argument(format!(
                                    "[_source.{key}] : liste de chaines attendue"
                                ))
                            })
                        })
                        .collect(),
                    Some(_) => Err(EsError::illegal_argument(format!(
                        "[_source.{key}] : chaine ou liste attendue"
                    ))),
                }
            };
            Ok(SourceFilter::Filter {
                includes: read("includes")?,
                excludes: read("excludes")?,
            })
        }
        _ => Err(EsError::illegal_argument("[_source] : valeur invalide")),
    }
}

// ---------------------------------------------------------------------------
// sort
// ---------------------------------------------------------------------------

fn parse_sort_body(v: &Value, champs: &crate::mapping::Fields) -> EsResult<Vec<SortSpec>> {
    let entries: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut specs = Vec::new();
    for entry in entries {
        match entry {
            Value::String(s) => specs.push(sort_spec(s, None, champs)?),
            Value::Object(o) => {
                for (field, spec) in o {
                    let order = match spec {
                        Value::String(s) => Some(s.clone()),
                        Value::Object(inner) => {
                            expect_only(inner, &["order"], "sort")?;
                            inner
                                .get("order")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        }
                        _ => {
                            return Err(EsError::illegal_argument(
                                "[sort] : chaine ou objet {order} attendu",
                            ))
                        }
                    };
                    specs.push(sort_spec(field, order.as_deref(), champs)?);
                }
            }
            _ => return Err(EsError::illegal_argument("[sort] : entree invalide")),
        }
    }
    Ok(specs)
}

/// `?sort=annee:desc,titre`
fn parse_sort_params(list: &[String], champs: &crate::mapping::Fields) -> EsResult<Vec<SortSpec>> {
    list.iter()
        .map(|entry| match entry.split_once(':') {
            Some((field, order)) => sort_spec(field, Some(order), champs),
            None => sort_spec(entry, None, champs),
        })
        .collect()
}

fn sort_spec(
    field: &str,
    order: Option<&str>,
    champs: &crate::mapping::Fields,
) -> EsResult<SortSpec> {
    // L'ordre se lit **avant** le champ : c'est une faute de corps, pas un
    // verdict de mapping, et ES la rend en premier lui aussi. Le lire apres
    // laissait `{"sort": [{"absent": {"order": "nawak"}}]}` passer en silence
    // quand aucun index n'etait vise (mesure : ES rend 400 sur un cluster
    // vide, ferrite rendait 200).
    let ordre = match order {
        None => None,
        Some(s) if s.eq_ignore_ascii_case("asc") => Some(true),
        Some(s) if s.eq_ignore_ascii_case("desc") => Some(false),
        Some(s) => {
            return Err(EsError::illegal_argument(format!(
                "[sort] : ordre [{s}] invalide (asc|desc)"
            )))
        }
    };
    let key = match field {
        "_score" => SortKey::Score,
        "_doc" => SortKey::Doc,
        name => {
            // Un sous-champ de `nested` trie a plat trierait sur autre chose
            // que ce qu'une clause `nested` filtre : ES refuse, et ferrite
            // rendait un ordre en 200. La phrase est **celle d'ES**, relevee
            // mot pour mot contre un 8.15 : un client qui la journalise doit
            // lire la meme des deux cotes.
            if champs.racine_nested(name).is_some() {
                return Err(EsError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "query_shard_exception",
                    format!(
                        "it is mandatory to set the [nested] context on the nested sort field: \
                         [{name}]."
                    ),
                ));
            }
            let mapped = champs.get(name).ok_or_else(|| {
                // Le type d'ES, et le marqueur qui permet a une recherche
                // multi-index de n'echouer que sur **cet** index.
                EsError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "query_shard_exception",
                    format!("No mapping found for [{name}] in order to sort on"),
                )
                .sur_champ_inconnu(name)
            })?;
            if mapped.ty.kind() == FieldKind::Text {
                return Err(EsError::illegal_argument(format!(
                    "Fielddata is disabled on [{name}] : ferrite ne trie pas sur un champ [text] \
                     ; utilise un champ [keyword]"
                )));
            }
            SortKey::Field {
                name: name.to_string(),
                kind: mapped.ty.kind(),
            }
        }
    };
    // Defaut d'ES : `desc` sur `_score`, `asc` partout ailleurs.
    let asc = ordre.unwrap_or(!matches!(key, SortKey::Score));
    Ok(SortSpec { key, asc })
}
