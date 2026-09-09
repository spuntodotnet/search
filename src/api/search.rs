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
    balayer, execute, rendre_page, round_score, Cible, Rendu, SearchRequest, SortKey, SortMode,
    SortSpec, SourceFilter,
};
use crate::selection::resoudre;
use crate::MAX_RESULT_WINDOW;

const DEFAULT_SIZE: usize = 10;

/// `POST|GET /_search` — sans index dans l'URL, ES cherche partout.
///
/// C'est aussi **la seule** route d'une recherche sous point-in-time : l'index
/// vient alors du PIT, et en nommer un dans l'URL serait deux sources de verite
/// pour une seule question. ES le refuse, d'ou le drapeau porte jusqu'ici.
pub async fn search_all(State(st): State<SharedState>, uri: Uri, body: Bytes) -> EsResult<Json> {
    rechercher(st, "_all".to_string(), false, uri, body).await
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
    rechercher(st, index, true, uri, body).await
}

async fn rechercher(
    st: SharedState,
    index: String,
    index_dans_url: bool,
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
    // `timeout` est de la meme famille, et sa forme est quand meme verifiee :
    // chez ES c'est une borne **par shard** au-dela de laquelle la collecte
    // s'arrete et la reponse sort partielle avec `timed_out: true`. ferrite
    // cherche en un seul morceau, dans le processus : il n'y a pas de collecte
    // a interrompre, donc il rend toujours un resultat complet et
    // `timed_out: false` — ce qu'ES rend aussi tant que la borne n'est pas
    // atteinte. C'est le sens sur : un `timeout` honore rendrait **moins** de
    // documents. Refuser la route entiere serait pire : `?timeout=` est ce que
    // pose la suite de tests du client go, et le refus tombait alors sur
    // « unrecognized parameter », donc sur une faute de frappe supposee.
    verifier_timeout(p.opt("timeout").as_deref())?;
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
    // Ce parametre (ES 8.13) ne change **que** la forme de `matched_queries` :
    // un objet `{nom: score}` au lieu d'une liste de noms. Il a longtemps ete
    // refuse **en le nommant**, parce que ferrite ne rendait pas
    // `matched_queries` : servir la forme d'une information absente n'aurait
    // rien voulu dire. Le nom d'une clause etant maintenant lu, le refus tombe.
    let noms_avec_score = p.bool_opt("include_named_queries_score")?.unwrap_or(false);
    let param_explain = p.bool_opt("explain")?;
    p.done()?;

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
            "highlight",
            "timeout",
            "min_score",
            "explain",
            "search_after",
            "pit",
        ],
        "_search",
    )?;

    // Les deux nouveautes de la pagination profonde se lisent **avant** la
    // boucle sur les index, comme `min_score` et `highlight` : leur forme ne
    // depend d'aucun mapping, et un `search_after` mal ecrit doit etre refuse
    // meme quand la recherche ne vise aucun index.
    let apres_brut = lire_search_after(body_obj.get("search_after"))?;
    let pit = lire_bloc_pit(body_obj.get("pit"))?;

    // `min_score` se lit **avant** la boucle sur les index : sa forme ne depend
    // d'aucun mapping, et un `"abc"` doit etre refuse meme quand la recherche
    // ne vise aucun index.
    let min_score = lire_min_score(body_obj.get("min_score"))?;

    if let Some(v) = body_obj.get("track_total_hits") {
        check_track_total_hits(v)?;
    }
    // ES lit `timeout` des deux cotes, et le corps l'emporte. Meme reponse ici :
    // accepte, verifie, sans effet.
    if let Some(v) = body_obj.get("timeout") {
        // Un nombre nu (`{"timeout": 5}`) n'est pas une erreur de forme chez
        // ES : il le relit comme une duree, et rend « unit is missing ». Le
        // message doit donc parler de l'unite, pas du type.
        match v {
            Value::String(s) => verifier_timeout(Some(s))?,
            Value::Number(n) => verifier_timeout(Some(&n.to_string()))?,
            autre => {
                return Err(EsError::parsing(format!(
                    "[_search] : [timeout] attend une duree ecrite en chaine, pas {autre}"
                )))
            }
        }
    }

    let demande = lire_demande(&body_obj, param_docvalue, param_stored)?;
    // Le bloc `highlight` se lit **avant** la boucle sur les index : sa forme
    // ne depend d'aucun mapping, et un `type: fvh` doit etre refuse meme quand
    // la recherche ne vise aucun index (voir [`valider_sans_index`]).
    let surlignage = match body_obj.get("highlight") {
        Some(v) => Some(crate::highlight::lire(v)?),
        None => None,
    };

    let from = match param_from {
        Some(v) => v,
        None => body_usize(&body_obj, "from")?.unwrap_or(0),
    };
    let size = match param_size {
        Some(v) => v,
        None => body_usize(&body_obj, "size")?.unwrap_or(DEFAULT_SIZE),
    };

    // Ce qui ne peut pas cohabiter. ES les accumule dans **une** exception de
    // validation, numerotee, levee avant toute resolution d'index — et chacune
    // de ces phrases est la sienne, mot pour mot.
    //
    // Les trois premieres disent la meme chose de trois facons : un `scroll`
    // porte son curseur dans le serveur, `search_after` le porte chez le
    // client, et un PIT ne porte pas de curseur du tout. Les melanger
    // demanderait de choisir lequel gagne — donc de rendre, en 200, une
    // pagination que personne n'a demandee.
    let mut validations: Vec<String> = Vec::new();
    if keep_alive.is_some() {
        // `from` n'a pas de sens dans un scroll : le contexte avance tout seul,
        // et sauter des documents ferait perdre en silence ceux d'avant.
        if from != 0 {
            validations.push("using [from] is not allowed in a scroll context".into());
        }
        if pit.is_some() {
            validations.push("using [point in time] is not allowed in a scroll context".into());
        }
        if apres_brut.is_some() {
            validations.push("[search_after] cannot be used in a scroll context".into());
        }
    }
    if pit.is_some() && index_dans_url {
        validations.push(
            "[indices] cannot be used with point in time. Do not specify any index with point in \
             time."
                .into(),
        );
    }
    if apres_brut.is_some() && from != 0 {
        validations.push("[from] parameter must be set to 0 when [search_after] is used".into());
    }
    // `_shard_doc` est le numero interne d'un document dans une vue. Hors d'une
    // vue figee il change au premier commit, donc un `search_after` bati dessus
    // ne designe plus rien — ES le refuse pour cette raison, et seulement au
    // premier niveau : un `top_hits` peut le poser sans PIT (mesure).
    if pit.is_none() && cite_shard_doc(param_sort.as_deref(), body_obj.get("sort")) {
        validations.push("[_shard_doc] sort field cannot be used without [point in time]".into());
    }
    if !validations.is_empty() {
        let mut phrase = String::from("Validation Failed: ");
        for (i, v) in validations.iter().enumerate() {
            phrase.push_str(&format!("{}: {v};", i + 1));
        }
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "action_request_validation_exception",
            phrase,
        ));
    }
    if from + size > MAX_RESULT_WINDOW {
        // La phrase entiere d'ES, les deux conseils compris : elle nomme la
        // sortie (`search_after`, `scroll`) et le reglage qui deplace la borne.
        // Un client qui la journalise doit lire la meme des deux cotes.
        return Err(EsError::illegal_argument(format!(
            "Result window is too large, from + size must be less than or equal to: \
             [{MAX_RESULT_WINDOW}] but was [{}]. See the scroll api for a more efficient way to \
             request large data sets. This limit can be set by changing the \
             [index.max_result_window] index level setting.",
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

    // `explain` se lit dans le corps **et** en query string ; comme chez ES, le
    // parametre l'emporte (mesure).
    let explique = match param_explain {
        Some(v) => v,
        None => match body_obj.get("explain") {
            Some(Value::Bool(b)) => *b,
            None | Some(Value::Null) => false,
            Some(autre) => {
                return Err(EsError::parsing(format!(
                    "[_search] : [explain] attend un booleen, pas {autre}"
                )))
            }
        },
    };

    // Les clauses nommees sont retirees de la requete **avant** toute
    // traduction : aucun parseur de clause n'a donc a connaitre `_name`, et
    // chaque clause nommee devient une requete a part, rejouee document par
    // document (voir [`crate::dsl::extraire_noms`]).
    let (requete, clauses_nommees) = match body_obj.get("query") {
        Some(v) => {
            let (nettoye, noms) = crate::dsl::extraire_noms(v)?;
            (Some(nettoye), noms)
        }
        None => (None, Vec::new()),
    };

    // Les index vises, et **l'instantane sur lequel on les lit**. C'est la
    // seule difference entre une recherche ordinaire et une recherche sous
    // point-in-time : la premiere prend la generation courante et lui demande
    // un `Searcher` neuf, la seconde reprend celui que l'ouverture du PIT a
    // retenu. Tout ce qui suit est identique — c'est ce qui fait qu'un PIT
    // accepte n'importe quelle requete, contrairement a un `scroll`.
    let generations: Vec<(
        String,
        String,
        std::sync::Arc<Generation>,
        tantivy::Searcher,
    )> = match &pit {
        Some((id, keep)) => st
            .pits
            .lire(id, *keep)?
            .into_iter()
            .map(|c| (c.nom, c.uuid, c.gen, c.searcher))
            .collect(),
        None => resoudre(&st.catalog, &index, &opts)?
            .iter()
            .map(|i| {
                let gen = i.current();
                let searcher = gen.searcher();
                (i.name.clone(), i.uuid.clone(), gen, searcher)
            })
            .collect(),
    };
    let champs_connus = union_des_champs(&generations);

    // Aucun index vise : la boucle ci-dessous ne tourne pas, donc rien du corps
    // ne serait lu. Il est valide a part, contre un schema vide.
    if generations.is_empty() {
        valider_sans_index(
            requete.as_ref(),
            aggs.as_ref(),
            param_sort.as_deref(),
            body_obj.get("sort"),
            maintenant,
            apres_brut.as_deref(),
            pit.is_some(),
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

    for (nom, uuid, gen, searcher) in generations {
        let sort = tri_effectif(
            param_sort.as_deref(),
            body_obj.get("sort"),
            &gen.fields,
            pit.is_some(),
        );
        let sort = match sort {
            Ok(s) => s,
            // Trier sur un champ que cet index ne mappe pas : ES ne fait pas
            // echouer la recherche, il rapporte l'echec **de ce shard** et rend
            // les documents des autres. Ecarter l'index en silence rendrait le
            // meme total qu'ES mais sans dire qu'il manque quelque chose ; le
            // faire echouer entierement rendrait moins de documents qu'ES.
            //
            // Meme regle pour ce que le tri ne sait pas resoudre **sur ce
            // mapping-la** : un `missing` qui n'a pas le type du champ, un
            // `mode: sum` sur un `keyword`, un `unmapped_type` illisible. Tous
            // sont des echecs de shard chez ES, donc les autres index repondent.
            Err(e) if e.champ_inconnu.is_some() || e.de_shard => {
                echecs.push(echec_de_shard(&nom, &uuid, &e, &st.catalog.cluster_uuid));
                continue;
            }
            Err(e) => return Err(e),
        };
        // Le point de reprise se convertit **au type que ce mapping-la donne
        // aux cles de tri** : c'est ce que fait ES, qui pose un `FieldDoc` par
        // shard. Les fautes de forme (arite, tri sans champ, valeur illisible)
        // sont des erreurs de requete, pas des verdicts de mapping : elles
        // remontent telles quelles.
        let apres = match &apres_brut {
            Some(valeurs) => Some(convertir_search_after(valeurs, &sort, &gen.fields)?),
            None => None,
        };
        // Les incidents de scoring sont ranges par index, et enveloppes des
        // maintenant : la mise en forme d'un echec de shard a besoin du nom et
        // de l'uuid, que le `Scorer` n'a pas.
        let incidents = std::sync::Arc::new(crate::fonction_score::Incidents::pour(
            &nom,
            &uuid,
            &st.catalog.cluster_uuid,
        ));
        let ctx = QueryCtx::new(&gen.fields, &gen.index, &searcher)
            .avec_champs_ailleurs(&champs_connus)
            .avec_maintenant(maintenant)
            .avec_nom_index(&nom)
            .selon_le_mapping(&gen.mapping)
            .avec_incidents(incidents.clone());
        let query = match requete.as_ref() {
            Some(v) => build_query(v, &ctx),
            None => Ok(crate::dsl::tous_les_documents()),
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
            // Une clause valide que **ce mapping-la** ne sait pas servir (un
            // `field_value_factor` sur un `keyword`) est un echec de shard chez
            // ES, pas une erreur de requete : les autres index repondent.
            Err(e) if e.de_shard => {
                echecs.push(echec_de_shard(&nom, &uuid, &e, &st.catalog.cluster_uuid));
                continue;
            }
            Err(e) => return Err(e),
        };
        // Le seuil enveloppe la requete **avant** tout le reste : c'est ce qui
        // fait qu'il change `hits.total`, la pagination et les agregations —
        // les trois mesures qui separent `min_score` d'un filtrage fait cote
        // client apres coup (voir [`crate::seuil`]).
        let query: Box<dyn tantivy::query::Query> = match min_score {
            Some(m) => Box::new(crate::seuil::Seuil::new(query, m)),
            None => query,
        };
        // Meme raisonnement pour les agregations : un index qui ne mappe pas le
        // champ agrege n'a aucune valeur a y verser.
        let (agrege, prep) = match &aggs {
            None => (false, crate::aggs::Prepare::default()),
            Some(a) => match crate::aggs::validate(a, Some(&gen), &ctx, &demande.fields) {
                Ok(prep) => (true, prep),
                Err(e) if e.champ_inconnu.is_some() => {
                    agg_ignore.get_or_insert(e);
                    (false, crate::aggs::Prepare::default())
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
        // Le surlignage se resout sur ce mapping **et** sur cette requete :
        // deux index ne posent pas les memes termes sur le meme champ.
        let hl = std::sync::Arc::new(match &surlignage {
            Some(d) => crate::highlight::resoudre(d, requete.as_ref(), &gen)?,
            None => crate::highlight::Plan::default(),
        });
        // Les clauses nommees se traduisent dans **cette** generation, comme la
        // requete principale : un `Field` tantivy n'a de sens que dans le schema
        // ou il a ete obtenu. Une clause qu'un index ne sait pas construire (un
        // champ qu'il ne mappe pas) ne correspond a rien ici, exactement comme
        // dans la requete.
        let mut nommees: Vec<(String, Box<dyn tantivy::query::Query>)> = Vec::new();
        for (n, clause) in &clauses_nommees {
            match build_query(clause, &ctx) {
                Ok(q) => nommees.push((n.clone(), q)),
                Err(e) if e.champ_inconnu.is_some() || e.de_shard => {}
                Err(e) => return Err(e),
            }
        }
        if sort_asc.is_empty() {
            sort_asc = sort.iter().map(|s| s.asc).collect();
        }
        cibles.push(Cible {
            nom,
            gen,
            searcher,
            plan,
            hl,
            query,
            sort,
            agrege,
            prep,
            incidents,
            nommees: std::sync::Arc::new(nommees),
            apres,
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
        rendu: Rendu {
            source,
            avec_id: demande.avec_id(),
            explique,
            noms_avec_score,
            noeud: st.catalog.cluster_uuid.clone(),
        },
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
                let rendu = req.rendu.clone();
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
    // Le `pit_id` en tete, comme le `_scroll_id` : c'est l'ordre d'ES, et
    // surtout c'est **cet** identifiant-la qu'un client bien ecrit renvoie au
    // coup suivant (ES peut en changer entre deux pages ; ferrite ne le fait
    // pas, mais un client qui suivrait l'autre convention se casserait chez ES).
    if let Some((id, _)) = &pit {
        let mut avec_id = Map::new();
        avec_id.insert("pit_id".into(), json!(id));
        avec_id.extend(reponse);
        return Ok(Json::ok(Value::Object(avec_id)));
    }
    Ok(Json::ok(Value::Object(reponse)))
}

// ---------------------------------------------------------------------------
// search_after et point-in-time
// ---------------------------------------------------------------------------

/// Le tableau `search_after` du corps, verifie dans sa **forme** seule.
///
/// Deux refus tombent ici, avant tout mapping et meme sans index vise, parce
/// qu'ES les leve a la lecture du corps : la valeur doit etre un tableau, et il
/// ne doit pas etre vide. Le troisieme refus de forme — un element qui n'est pas
/// un scalaire — est leve ici aussi : `[[1]]` et `[{}]` ne designent aucune cle
/// de tri possible.
fn lire_search_after(v: Option<&Value>) -> EsResult<Option<Vec<Value>>> {
    let Some(v) = v else { return Ok(None) };
    let Value::Array(a) = v else {
        return Err(EsError::parsing(format!(
            "Unknown key for a {} in [search_after].",
            jeton_json(v)
        )));
    };
    if a.is_empty() {
        // La coquille est celle d'ES, et elle se reproduit : un client qui
        // journalise le message doit lire la meme phrase des deux cotes.
        return Err(EsError::illegal_argument(
            "Values must contains at least one value.",
        ));
    }
    for e in a {
        if matches!(e, Value::Array(_) | Value::Object(_)) {
            return Err(EsError::parsing(format!(
                "Expected [VALUE_STRING] or [VALUE_NUMBER] or [VALUE_BOOLEAN] or [VALUE_NULL] but \
                 found [{}] inside search_after.",
                jeton_json(e)
            )));
        }
    }
    Ok(Some(a.clone()))
}

/// Le nom que le parseur d'ES donne a un jeton JSON. Il apparait mot pour mot
/// dans trois de ses messages.
fn jeton_json(v: &Value) -> &'static str {
    match v {
        Value::Null => "VALUE_NULL",
        Value::Bool(_) => "VALUE_BOOLEAN",
        Value::Number(_) => "VALUE_NUMBER",
        Value::String(_) => "VALUE_STRING",
        Value::Array(_) => "START_ARRAY",
        Value::Object(_) => "START_OBJECT",
    }
}

/// Le bloc `pit` du corps : `{"id": "...", "keep_alive": "1m"}`.
///
/// `keep_alive` y est **facultatif** (contrairement a l'ouverture, ou ES
/// l'exige) : sans lui la vue garde l'echeance posee au coup precedent.
fn lire_bloc_pit(v: Option<&Value>) -> EsResult<Option<(String, Option<std::time::Duration>)>> {
    let Some(v) = v else { return Ok(None) };
    let Value::Object(o) = v else {
        return Err(EsError::parsing(format!(
            "Unknown key for a {} in [pit].",
            jeton_json(v)
        )));
    };
    expect_only(o, &["id", "keep_alive"], "pit")?;
    let id = match o.get("id") {
        Some(Value::String(s)) => s.clone(),
        None => {
            return Err(EsError::illegal_argument(
                "point in time id is not provided",
            ))
        }
        Some(autre) => {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "x_content_parse_exception",
                format!(
                    "[pit] id doesn't support values of type: {}",
                    jeton_json(autre)
                ),
            ))
        }
    };
    let keep = match o.get("keep_alive") {
        Some(Value::String(s)) => Some(scroll::duree(s, "keep_alive")?),
        None | Some(Value::Null) => None,
        Some(autre) => {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "x_content_parse_exception",
                format!(
                    "[pit] keep_alive doesn't support values of type: {}",
                    jeton_json(autre)
                ),
            ))
        }
    };
    Ok(Some((id, keep)))
}

/// Le `sort` demande cite-t-il `_shard_doc` ?
///
/// La question se pose sur le **JSON brut** plutot que sur les cles resolues,
/// et c'est voulu : la reponse ne depend d'aucun mapping, et ES leve ce refus
/// avant meme de resoudre les index (une recherche sans index vise le leve
/// aussi).
fn cite_shard_doc(param: Option<&[String]>, corps: Option<&Value>) -> bool {
    const NOM: &str = "_shard_doc";
    if let Some(list) = param {
        return list
            .iter()
            .any(|e| e.split_once(':').map_or(e.as_str(), |(f, _)| f) == NOM);
    }
    let Some(v) = corps else { return false };
    let entrees: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        autre => vec![autre],
    };
    entrees.iter().any(|e| match e {
        Value::String(s) => s == NOM,
        Value::Object(o) => o.keys().any(|k| k == NOM),
        _ => false,
    })
}

/// Les cles de tri effectives : celles du corps, **plus le departage implicite
/// d'un point-in-time**.
///
/// Sous un PIT, ES ajoute lui-meme `_shard_doc` en dernier des que le client a
/// demande un tri — et c'est la moitie du produit : sans elle, `search_after`
/// sauterait les ex aequo du dernier critere. Mesure contre ES 8.15 :
/// `sort: [{"n": "asc"}]` sous un PIT rend des tableaux `sort` a **deux**
/// elements. Il ne l'ajoute pas deux fois si le client l'a deja pose, et pas du
/// tout si le client n'a demande aucun tri.
///
/// Consequence a ne pas manquer : la normalisation « `[_score desc]` **est** le
/// tri par defaut » (voir [`tri_par_defaut`]) ne s'applique plus sous un PIT,
/// puisque le departage en fait un vrai tri a deux cles — ES rend alors bien un
/// tableau `sort` et `max_score: null` (mesure).
fn tri_effectif(
    param: Option<&[String]>,
    corps: Option<&Value>,
    champs: &crate::mapping::Fields,
    sous_pit: bool,
) -> EsResult<Vec<SortSpec>> {
    let brut = match param {
        Some(list) => parse_sort_params_brut(list, champs)?,
        None => match corps {
            Some(v) => parse_sort_body_brut(v, champs)?,
            None => Vec::new(),
        },
    };
    if !sous_pit {
        return Ok(tri_par_defaut(brut));
    }
    let mut specs = brut;
    if !specs.is_empty() && !specs.iter().any(|s| matches!(s.key, SortKey::ShardDoc)) {
        specs.push(SortSpec {
            key: SortKey::ShardDoc,
            asc: true,
        });
    }
    Ok(specs)
}

/// Le point de reprise, converti au type des cles de tri de **cet** index.
///
/// Trois refus sortent d'ici, et aucun ne se devine :
///
/// - un `search_after` sans cle de tri utilisable rend « Sort must contain at
///   least one field. » — et `_score` **n'en est pas une** (mesure :
///   `sort: [{"_score": "desc"}]` avec un `search_after` rend ce refus, la ou
///   `sort: ["_doc"]` repond) ;
/// - une arite qui ne correspond pas rend « search_after has N value(s) but
///   sort has M. » ;
/// - une valeur que le type du champ ne sait pas lire rend « Failed to parse
///   search_after value for field [x]. », **une seule phrase** quelle que soit
///   la faute — ES n'y detaille pas la cause.
fn convertir_search_after(
    valeurs: &[Value],
    sort: &[SortSpec],
    champs: &crate::mapping::Fields,
) -> EsResult<Vec<crate::search::SortValue>> {
    if !sort
        .iter()
        .any(|s| matches!(s.key, SortKey::Field(_) | SortKey::Doc | SortKey::ShardDoc))
    {
        return Err(EsError::illegal_argument(
            "Sort must contain at least one field.",
        ));
    }
    if valeurs.len() != sort.len() {
        return Err(EsError::illegal_argument(format!(
            "search_after has {} value(s) but sort has {}.",
            valeurs.len(),
            sort.len()
        )));
    }
    valeurs
        .iter()
        .zip(sort)
        .map(|(v, spec)| valeur_de_reprise(v, spec, champs))
        .collect()
}

fn valeur_de_reprise(
    v: &Value,
    spec: &SortSpec,
    champs: &crate::mapping::Fields,
) -> EsResult<crate::search::SortValue> {
    use crate::search::SortValue;
    let illisible = |nom: &str| {
        EsError::illegal_argument(format!(
            "Failed to parse search_after value for field [{nom}]."
        ))
    };
    match &spec.key {
        SortKey::Score => lit_double(v)
            .map(SortValue::F64)
            .map_err(|_| illisible("_score")),
        SortKey::Doc => lit_long(v)
            .map(SortValue::I64)
            .map_err(|_| illisible("_doc")),
        SortKey::ShardDoc => lit_long(v)
            .map(SortValue::I64)
            .map_err(|_| illisible("_shard_doc")),
        SortKey::Field(f) => {
            // `null` designe la **sentinelle** de la cle : c'est ce que le
            // tableau `sort` d'un document sans valeur porte sur un `keyword`,
            // donc c'est ce qu'un parcours renvoie tel quel au coup suivant.
            // Le lire autrement casserait la seule boucle qui compte : rendre
            // au serveur ce qu'il vient de rendre au client.
            if v.is_null() {
                return Ok(f.absente.clone());
            }
            match f.ty.kind() {
                FieldKind::Keyword | FieldKind::Text => Ok(SortValue::Str(match v {
                    Value::String(s) => s.clone(),
                    autre => autre.to_string(),
                })),
                FieldKind::F64 => lit_double(v)
                    .map(SortValue::F64)
                    .map_err(|_| illisible(&f.name)),
                // Une date se lit **par le format du champ**, pas comme un
                // nombre : ES accepte `"2026-01-05"` la ou `missing` exige des
                // millisecondes (mesure). Les deux parametres ne lisent donc
                // pas la meme grammaire, et les confondre rendrait 400 sur
                // l'ecriture la plus courante d'un point de reprise.
                // Une date de reprise se lit **comme une borne de `range`**,
                // date math comprise : `"2026-01-05||+1d"` y designe bien le
                // lendemain (mesure contre ES 8.15), et un mois ecrit sans son
                // jour s'arrondit vers le bas comme une borne basse. Elle
                // remonte donc aussi la phrase de ce parseur-la — celle qui dit
                // au client ce qu'il aurait fallu ecrire — plutot que la phrase
                // generique de `search_after`.
                //
                // Un seul point l'en separe, et il se mesure : `now` y est
                // **refuse** par ES. Sa cle de tri est construite sans horloge
                // (le fournisseur d'instant est nul), d'ou sa phrase — que
                // ferrite reprend au lieu de resoudre `now`, ce qui rendrait
                // des documents qu'ES ne rend pas.
                FieldKind::Date => {
                    if v.as_str()
                        .is_some_and(|s| s.trim_start().starts_with("now"))
                    {
                        return Err(EsError::new(
                            axum::http::StatusCode::BAD_REQUEST,
                            "parse_exception",
                            "could not read the current timestamp",
                        ));
                    }
                    crate::datemath::borne(
                        v,
                        champs.format_ou_defaut(&f.name),
                        0,
                        crate::datemath::Arrondi::Bas,
                    )
                    .map(SortValue::I64)
                }
                _ => lit_long(v)
                    .map(SortValue::I64)
                    .map_err(|_| illisible(&f.name)),
            }
        }
    }
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
        (None, Some(liste)) => fetch::stored_des_params(&liste)?,
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
    apres: Option<&[Value]>,
    sous_pit: bool,
) -> EsResult<()> {
    let vide = crate::engine::sans_index();
    let searcher = vide.searcher();
    let ctx = QueryCtx::new(&vide.fields, &vide.index, &searcher)
        .avec_maintenant(maintenant)
        .selon_le_mapping(&vide.mapping)
        .sans_index_vise();

    let sort = tri_effectif(param_sort, body_sort, &vide.fields, sous_pit);
    let sort: Vec<SortSpec> = sans_verdict_de_mapping_val(sort)?.unwrap_or_default();
    // `search_after` se verifie contre le schema vide comme le reste : son
    // arite et l'exigence d'au moins une cle de tri ne dependent d'aucun
    // mapping, et ES les leve bien sur un cluster vide. La **conversion** des
    // valeurs, elle, en depend — un champ inconnu du schema vide n'a pas de
    // type — donc elle ne rend pas de verdict ici.
    if let Some(valeurs) = apres {
        sans_verdict_de_mapping(convertir_search_after(valeurs, &sort, &vide.fields))?;
    }
    if let Some(v) = query {
        sans_verdict_de_mapping(build_query(v, &ctx))?;
    }
    if let Some(a) = aggs {
        sans_verdict_de_mapping(crate::aggs::validate(a, None, &ctx, &[]))?;
    }
    Ok(())
}

/// La meme validation, pour une route qui n'a qu'une requete a valider
/// (`_delete_by_query`, `_update_by_query`).
///
/// ES la fait aussi : `POST /rien-*/_delete_by_query` avec une clause inconnue
/// rend 400 sur un cluster vide, alors qu'il ne vise aucun index.
pub(crate) fn valider_sans_index_query(query: Option<&Value>, maintenant: i64) -> EsResult<()> {
    valider_sans_index(query, None, None, None, maintenant, None, false)
}

/// Jette ce qu'une validation sans index a produit, et avec lui les erreurs
/// qu'aucun mapping ne peut trancher (voir [`valider_sans_index`]).
fn sans_verdict_de_mapping<T>(r: EsResult<T>) -> EsResult<()> {
    sans_verdict_de_mapping_val(r).map(|_| ())
}

/// La meme chose, mais qui **garde** ce que la validation a produit : le tri
/// resolu contre le schema vide sert ensuite a verifier l'arite d'un
/// `search_after`.
fn sans_verdict_de_mapping_val<T>(r: EsResult<T>) -> EsResult<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        // `query_shard_exception` est precisement le type qu'ES reserve a ce
        // qu'un shard decide : sans shard, il n'y a pas de verdict a rendre.
        // Un echec de shard explicite non plus — mesure : sur un cluster vide,
        // `{"sort": [{"i": {"missing": "abc"}}]}` et `unmapped_type: nawak`
        // rendent 200, alors que `mode: nawak` rend 400.
        Err(e) if e.ty == "query_shard_exception" || e.champ_inconnu.is_some() || e.de_shard => {
            Ok(None)
        }
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
    let champs_connus = union_des_champs_de(
        &generations
            .iter()
            .map(|(_, _, g)| g.clone())
            .collect::<Vec<_>>(),
    );
    let mut prets: Vec<(std::sync::Arc<Generation>, Box<dyn tantivy::query::Query>)> = Vec::new();
    let mut ignore: Option<EsError> = None;
    let maintenant = crate::datemath::maintenant();
    // Meme trou qu'en recherche : sans index, la boucle ne tourne pas et la
    // requete n'est jamais lue (voir [`valider_sans_index`]).
    // `_name` est retire comme sur `_search` : `_count` ne rend pas de hits,
    // donc pas de `matched_queries`, mais la requete doit rester construisible.
    let requete = match body_obj.get("query") {
        Some(v) => Some(crate::dsl::extraire_noms(v)?.0),
        None => None,
    };
    if generations.is_empty() {
        valider_sans_index(requete.as_ref(), None, None, None, maintenant, None, false)?;
    }
    for (nom, _, gen) in &generations {
        let gen = gen.clone();
        let query = {
            let searcher = gen.searcher();
            let ctx = QueryCtx::new(&gen.fields, &gen.index, &searcher)
                .avec_champs_ailleurs(&champs_connus)
                .avec_maintenant(maintenant)
                .avec_nom_index(nom)
                .selon_le_mapping(&gen.mapping);
            match requete.as_ref() {
                Some(v) => build_query(v, &ctx),
                None => Ok(crate::dsl::tous_les_documents()),
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
    generations: &[(
        String,
        String,
        std::sync::Arc<Generation>,
        tantivy::Searcher,
    )],
) -> std::collections::BTreeSet<String> {
    union_des_champs_de(
        &generations
            .iter()
            .map(|(_, _, g, _)| g.clone())
            .collect::<Vec<_>>(),
    )
}

/// La meme chose, pour les routes qui n'ont pas besoin du nom ni de l'uuid des
/// index vises (`_delete_by_query`, `_update_by_query`).
pub(crate) fn union_des_champs_de(
    generations: &[std::sync::Arc<Generation>],
) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    // Sur un index unique il n'y a pas d'« ailleurs » : l'ensemble reste vide,
    // et un champ inconnu redevient l'erreur qu'il doit etre.
    if generations.len() < 2 {
        return out;
    }
    for gen in generations {
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

/// `timeout` : accepte et sans effet, mais **verifie**.
///
/// Un parametre sans effet dont la valeur n'est pas relue laisse passer
/// `timeout=1` (l'unite manque) la ou ES rend 400 : le client ne decouvrirait sa
/// faute qu'en changeant de serveur. Voir [`crate::util::valider_duree`] pour
/// les bords, tous mesures.
fn verifier_timeout(v: Option<&str>) -> EsResult<()> {
    match v {
        None => Ok(()),
        Some(s) => crate::util::valider_duree(s, "timeout").map_err(EsError::illegal_argument),
    }
}

/// ferrite compte toujours les hits exactement — `relation` vaut donc toujours
/// `eq`. Seul `false` (ne pas compter) n'a pas d'equivalent.
/// `min_score` : le seuil sous lequel un document ne sort pas.
///
/// ES le lit comme un `float` et accepte la **chaine** qui en porte un
/// (`"0.135"` filtre comme `0.135`). Ses deux refus sont mesures contre 8.15 :
/// `null` est une `parsing_exception` (« Unknown key for a VALUE_NULL in
/// [min_score]. »), une chaine illisible une `number_format_exception` (« For
/// input string: "abc" »). Un seuil negatif ne filtre rien, et il ne se refuse
/// pas : ES le prend tel quel.
fn lire_min_score(v: Option<&Value>) -> EsResult<Option<f32>> {
    match v {
        None => Ok(None),
        Some(Value::Number(n)) => Ok(n.as_f64().map(|f| f as f32)),
        Some(Value::String(s)) => s.trim().parse::<f32>().map(Some).map_err(|_| {
            EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "number_format_exception",
                format!("For input string: \"{s}\""),
            )
        }),
        Some(Value::Null) => Err(EsError::parsing(
            "Unknown key for a VALUE_NULL in [min_score].",
        )),
        Some(autre) => Err(EsError::parsing(format!(
            "[_search] : [min_score] attend un nombre, pas {autre}"
        ))),
    }
}

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

pub(crate) fn parse_source_body(v: &Value) -> EsResult<SourceFilter> {
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

/// Un `sort` qui **est** le classement par defaut n'en est pas un.
///
/// Mesure contre ES 8.15 : `sort: [{"_score": "desc"}]` (et son ecriture courte
/// `["_score"]`) rend `max_score` renseigne et **aucun** tableau `sort` dans les
/// hits — exactement comme une recherche sans `sort`. Des que la cle change de
/// sens (`asc`) ou qu'une seconde cle l'accompagne, c'est un vrai tri :
/// `max_score` repasse a `null` et le tableau `sort` apparait.
///
/// Sans cette normalisation, ferrite rendait `max_score: null` sur l'ecriture
/// la plus courante d'un tri par pertinence — un client qui lit `max_score` y
/// voyait « aucun score » sur des documents scores. Trouve par
/// `sonde_meta_champs.py`, en mesurant le voisinage de `_id`.
fn tri_par_defaut(specs: Vec<SortSpec>) -> Vec<SortSpec> {
    match specs.as_slice() {
        [SortSpec {
            key: SortKey::Score,
            asc: false,
        }] => Vec::new(),
        _ => specs,
    }
}

pub(crate) fn parse_sort_body(
    v: &Value,
    champs: &crate::mapping::Fields,
) -> EsResult<Vec<SortSpec>> {
    parse_sort_body_brut(v, champs).map(tri_par_defaut)
}

/// Le meme, **sans** la normalisation du tri par defaut.
///
/// La distinction ne servait a rien tant que rien ne s'ajoutait au tri : sous un
/// point-in-time, le departage implicite fait de `[_score desc]` un vrai tri a
/// deux cles, donc la normalisation doit venir apres l'ajout, pas avant (voir
/// [`tri_effectif`]).
fn parse_sort_body_brut(v: &Value, champs: &crate::mapping::Fields) -> EsResult<Vec<SortSpec>> {
    let entries: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut specs = Vec::new();
    for entry in entries {
        match entry {
            Value::String(s) => specs.push(sort_spec(s, &Options::default(), champs)?),
            Value::Object(o) => {
                for (field, spec) in o {
                    let opts = match spec {
                        Value::String(s) => Options {
                            order: Some(s.clone()),
                            ..Options::default()
                        },
                        Value::Object(inner) => {
                            expect_only(
                                inner,
                                &["order", "missing", "mode", "unmapped_type"],
                                "sort",
                            )?;
                            Options {
                                order: inner
                                    .get("order")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                                missing: inner.get("missing").cloned(),
                                mode: inner.get("mode").cloned(),
                                unmapped_type: inner.get("unmapped_type").cloned(),
                            }
                        }
                        _ => {
                            return Err(EsError::illegal_argument(
                                "[sort] : chaine ou objet {order} attendu",
                            ))
                        }
                    };
                    specs.push(sort_spec(field, &opts, champs)?);
                }
            }
            _ => return Err(EsError::illegal_argument("[sort] : entree invalide")),
        }
    }
    Ok(specs)
}

/// Ce qu'une entree de `sort` peut porter a cote du champ. Les trois derniers
/// ne se lisent que dans la forme objet : `?sort=` en query string ne connait
/// que `champ:sens`, chez ES comme ici.
#[derive(Default)]
struct Options {
    order: Option<String>,
    missing: Option<Value>,
    mode: Option<Value>,
    unmapped_type: Option<Value>,
}

/// `?sort=annee:desc,titre`
fn parse_sort_params_brut(
    list: &[String],
    champs: &crate::mapping::Fields,
) -> EsResult<Vec<SortSpec>> {
    list.iter()
        .map(|entry| {
            let (field, order) = match entry.split_once(':') {
                Some((field, order)) => (field, Some(order.to_string())),
                None => (entry.as_str(), None),
            };
            sort_spec(
                field,
                &Options {
                    order,
                    ..Options::default()
                },
                champs,
            )
        })
        .collect()
}

fn sort_spec(field: &str, opts: &Options, champs: &crate::mapping::Fields) -> EsResult<SortSpec> {
    let order = opts.order.as_deref();
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
    // Le `mode` se lit avant le champ, pour la meme raison que l'ordre : c'est
    // une faute de corps, qu'ES rend meme sans index vise.
    let mode = match &opts.mode {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(SortMode::parse(s).ok_or_else(|| {
            EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "x_content_parse_exception",
                format!(
                    "[field_sort] failed to parse field [mode] : Unknown SortMode [{s}] \
                     (min|max|sum|avg|median)"
                ),
            )
        })?),
        Some(v) => {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "x_content_parse_exception",
                format!("[field_sort] failed to parse field [mode] : chaine attendue, recu {v}"),
            ))
        }
    };
    let key = match field {
        // ES ne lit ni `missing`, ni `mode`, ni `unmapped_type` a cote de
        // `_score` : il les refuse comme des cles inconnues, sans index vise
        // compris. `_doc`, lui, les **accepte et les ignore** (mesure).
        "_score" => {
            for (nom, val) in [
                ("missing", &opts.missing),
                ("mode", &opts.mode),
                ("unmapped_type", &opts.unmapped_type),
            ] {
                if val.is_some() {
                    return Err(EsError::new(
                        axum::http::StatusCode::BAD_REQUEST,
                        "x_content_parse_exception",
                        format!("[_score] unknown field [{nom}]"),
                    ));
                }
            }
            SortKey::Score
        }
        "_doc" => SortKey::Doc,
        // `_shard_doc` accepte les memes parametres decoratifs que `_doc`, et
        // son refus hors d'un point-in-time se leve **avant** ici (voir
        // [`cite_shard_doc`]) : ES le range dans la validation de la requete,
        // pas dans la resolution du tri — ce qui explique qu'un `top_hits`
        // puisse le poser sans PIT (mesure).
        "_shard_doc" => SortKey::ShardDoc,
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
            // Le champ, ou l'echappatoire : `unmapped_type` dit de quel type
            // traiter un champ que **cet** index ne mappe pas, plutot que de
            // faire echouer son shard. Quand l'index le mappe, il est ignore
            // (mesure : ES ne s'en sert meme pas pour verifier le type).
            let (ty, mappe) = match champs.get(name) {
                Some(mapped) => (mapped.ty, true),
                None => match &opts.unmapped_type {
                    Some(v) => (type_non_mappe(v)?, false),
                    None => {
                        // Le type d'ES, et le marqueur qui permet a une
                        // recherche multi-index de n'echouer que sur **cet**
                        // index.
                        return Err(EsError::new(
                            axum::http::StatusCode::BAD_REQUEST,
                            "query_shard_exception",
                            format!("No mapping found for [{name}] in order to sort on"),
                        )
                        .sur_champ_inconnu(name));
                    }
                },
            };
            // Le nom que porte le refus : ES fabrique un mapper **anonyme**
            // pour un `unmapped_type`, et c'est ce nom-la qu'il rend.
            let dit = if mappe { name } else { "__anonymous_" };
            if ty.kind() == FieldKind::Text {
                return Err(EsError::illegal_argument(format!(
                    "Fielddata is disabled on [{dit}] : ferrite ne trie pas sur un champ [text] \
                     ; utilise un champ [keyword]"
                ))
                .sur_un_shard());
            }
            // `sum`, `avg` et `median` sur autre chose qu'un nombre : ES le
            // refuse par shard, avec cette phrase.
            if mode.is_some_and(SortMode::numerique_seulement) && ty.kind() == FieldKind::Keyword {
                return Err(EsError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "query_shard_exception",
                    "we only support AVG, MEDIAN and SUM on number based fields",
                )
                .sur_un_shard());
            }
            let asc = ordre.unwrap_or(true);
            SortKey::Field(Box::new(crate::search::SortField {
                name: name.to_string(),
                ty,
                mappe,
                mode,
                absente: valeur_absente(ty, asc, opts.missing.as_ref())?,
            }))
        }
    };
    // Defaut d'ES : `desc` sur `_score`, `asc` partout ailleurs.
    let asc = ordre.unwrap_or(!matches!(key, SortKey::Score));
    Ok(SortSpec { key, asc })
}

/// Le type qu'`unmapped_type` demande, resolu comme ES resout un mapper.
///
/// Deux refus portent **sa** phrase, parce que ce sont les siens : `object` et
/// `nested` ne sont pas des feuilles, et un nom inconnu n'est aucun mapper. Le
/// troisieme est celui de ferrite : un type qu'ES sait mapper et pas lui
/// (`ip`, `binary`, `scaled_float`...) doit se dire, pas se deguiser en « type
/// inconnu ».
fn type_non_mappe(v: &Value) -> EsResult<crate::mapping::FieldType> {
    let s = v.as_str().ok_or_else(|| {
        EsError::illegal_argument(format!(
            "[sort] : [unmapped_type] attend une chaine, recu {v}"
        ))
    })?;
    if let Some(ty) = crate::mapping::FieldType::parse(s) {
        return Ok(ty);
    }
    let e = match s {
        "object" | "nested" => {
            EsError::illegal_argument(format!("Mapper for type [{s}] must be a leaf field"))
        }
        _ => EsError::unsupported(format!(
            "ferrite ne supporte pas le type de champ [{s}] ([sort.unmapped_type]) ; types \
             supportes : text, keyword, byte, short, integer, long, float, double, boolean, date"
        )),
    };
    Err(e.sur_un_shard())
}

/// Ce qu'un document sans valeur porte comme cle de tri : `_last` (le defaut),
/// `_first`, ou une valeur de substitution **typee selon le champ**.
///
/// Trois regles mesurees contre ES 8.15, dont aucune ne se lit dans sa doc :
///
/// - `_first` et `_last` sont **sensibles a la casse**. `_FIRST` n'est pas un
///   mot-cle mais une valeur de substitution — donc `"_FIRST"` sur un `long`
///   rend 400, la ou `"_first"` trie ;
/// - la substitution d'une **date** est un nombre de millisecondes, pas une
///   date : `missing: "2020-03-01"` sur un champ `date` rend 400. Un booleen se
///   substitue de meme par `0` ou `1`, jamais par `true` ;
/// - une chaine se lit strictement (`Long.parseLong` / `Double.parseDouble`),
///   un nombre JSON se **tronque** : `missing: 7.9` vaut 7 sur un `long`, mais
///   `missing: "7.9"` y rend 400.
fn valeur_absente(
    ty: crate::mapping::FieldType,
    asc: bool,
    missing: Option<&Value>,
) -> EsResult<crate::search::SortValue> {
    use crate::search::{sentinelle, SortValue};
    let v = match missing {
        None | Some(Value::Null) => return Ok(sentinelle(ty, asc, false)),
        Some(Value::String(s)) if s == "_first" => return Ok(sentinelle(ty, asc, true)),
        Some(Value::String(s)) if s == "_last" => return Ok(sentinelle(ty, asc, false)),
        Some(v @ (Value::Array(_) | Value::Object(_))) => {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "x_content_parse_exception",
                format!(
                    "[field_sort] missing doesn't support values of type: {}",
                    if v.is_array() {
                        "START_ARRAY"
                    } else {
                        "START_OBJECT"
                    }
                ),
            ))
        }
        Some(v) => v,
    };
    match ty.kind() {
        // Toute valeur simple devient sa forme texte : `42` rend la cle `"42"`,
        // `true` rend `"true"`.
        FieldKind::Keyword | FieldKind::Text => Ok(SortValue::Str(match v {
            Value::String(s) => s.clone(),
            autre => autre.to_string(),
        })),
        FieldKind::F64 => Ok(SortValue::F64(lit_double(v)?)),
        // Une date et un booleen se comparent comme des entiers, donc se
        // substituent comme eux.
        _ => Ok(SortValue::I64(lit_long(v)?)),
    }
}

/// L'erreur qu'ES rend sur une substitution illisible, jusqu'a la mise entre
/// guillemets de la valeur : c'est `NumberFormatException` qui remonte telle
/// quelle, et elle fait echouer le shard.
fn nombre_illisible(texte: &str) -> EsError {
    EsError::new(
        axum::http::StatusCode::BAD_REQUEST,
        "number_format_exception",
        format!("For input string: \"{texte}\""),
    )
    .sur_un_shard()
}

/// `Long.parseLong` sur une chaine, `.longValue()` sur un nombre.
fn lit_long(v: &Value) -> EsResult<i64> {
    match v {
        Value::String(s) => java_long(s).ok_or_else(|| nombre_illisible(s)),
        Value::Bool(b) => Err(nombre_illisible(if *b { "true" } else { "false" })),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Ok(i);
            }
            // `(long) 7.9` vaut 7 et `(long) 1e300` sature a `i64::MAX` : c'est
            // la conversion de Java, que `as` reproduit en Rust.
            #[allow(clippy::cast_possible_truncation)]
            Ok(n.as_f64().map_or(0, |f| f as i64))
        }
        autre => Err(nombre_illisible(&autre.to_string())),
    }
}

/// `Double.parseDouble` sur une chaine, la valeur elle-meme sur un nombre.
fn lit_double(v: &Value) -> EsResult<f64> {
    match v {
        Value::String(s) => java_double(s).ok_or_else(|| nombre_illisible(s)),
        Value::Bool(b) => Err(nombre_illisible(if *b { "true" } else { "false" })),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        autre => Err(nombre_illisible(&autre.to_string())),
    }
}

/// La grammaire de `Long.parseLong` : un signe optionnel, puis **des chiffres
/// et rien d'autre**, dans les bornes d'un `long`.
///
/// Elle est plus stricte que celle de Rust sur un point qui se mesure : ES
/// refuse `" 7"`, `"7 "`, `"7.9"` et `"1e3"` sur un `long`, et accepte `"+7"`.
fn java_long(s: &str) -> Option<i64> {
    let corps = s.strip_prefix(['+', '-']).unwrap_or(s);
    if corps.is_empty() || !corps.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.strip_prefix('+').unwrap_or(s).parse::<i64>().ok()
}

/// La grammaire de `Double.parseDouble`, restreinte a ce qu'un client ecrit :
/// un signe optionnel, puis `NaN`, `Infinity`, ou un nombre decimal.
///
/// Le detour par une verification plutot que par le parseur de Rust n'est pas
/// une precaution de style : Rust accepte `"inf"` et `"infinity"` que Java
/// refuse, donc ferrite classerait un document la ou ES rend 400.
fn java_double(s: &str) -> Option<f64> {
    let corps = s.strip_prefix(['+', '-']).unwrap_or(s);
    let negatif = s.starts_with('-');
    if corps == "NaN" {
        return Some(f64::NAN);
    }
    if corps == "Infinity" {
        return Some(if negatif {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    // Un suffixe de type (`1.5f`, `2d`) fait partie de la grammaire de Java.
    let corps = corps.strip_suffix(['f', 'F', 'd', 'D']).unwrap_or(corps);
    let (mantisse, exposant) = match corps.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (corps, None),
    };
    let chiffres = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    let mantisse_ok = match mantisse.split_once('.') {
        Some(("", b)) => chiffres(b),
        Some((a, "")) => chiffres(a),
        Some((a, b)) => chiffres(a) && chiffres(b),
        None => chiffres(mantisse),
    };
    let exposant_ok = exposant.is_none_or(|e| chiffres(e.strip_prefix(['+', '-']).unwrap_or(e)));
    if !mantisse_ok || !exposant_ok {
        return None;
    }
    s.parse::<f64>().ok()
}
