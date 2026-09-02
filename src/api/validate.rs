//! `_validate/query` : la requete est-elle valide, et sinon pourquoi.
//!
//! Rien de neuf ici : c'est le traducteur du Query DSL ([`crate::dsl`]) rendu
//! observable, sans executer la recherche. Ce qu'apporte la route, c'est la
//! **separation** qu'ES fait entre deux familles d'invalidite, et qui tient
//! dans deux formes de reponse differentes :
//!
//! * la requete est mal formee en elle-meme (clause inconnue, parametre
//!   refuse) — ES appelle ca une erreur du noeud coordinateur et rend
//!   `{"valid": false}`, sans `_shards`. ferrite la trouve au meme endroit :
//!   en validant la requete contre un **schema vide** ([`crate::engine::sans_index`]),
//!   ou aucune erreur ne peut venir du mapping ;
//! * la requete est bien formee mais impossible a construire **sur ce
//!   mapping-la** (une valeur qui n'a pas le type du champ) — ES rend alors
//!   `_shards` et une explication par index.
//!
//! L'`explanation` d'une requete valide est une **divergence assumee** : celle
//! d'ES est la chaine Lucene de la requete reecrite, ferrite rend la sienne.
//! Les deux moteurs ne construisent pas les memes objets, et inventer une
//! chaine Lucene qu'on n'a pas serait pire que d'en rendre une qui dit
//! honnetement ce que ferrite a compris.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::Uri;
use serde_json::{json, Map, Value};

use super::{parse_body, selection_options, Json, Params, SharedState};
use crate::dsl::{build_query, QueryCtx};
use crate::error::{EsError, EsResult};
use crate::selection::resoudre;

/// `GET|POST /_validate/query`
pub async fn validate_all(State(st): State<SharedState>, uri: Uri, body: Bytes) -> EsResult<Json> {
    validate(State(st), Path("_all".to_string()), uri, body).await
}

/// `GET|POST /{index}/_validate/query`
pub async fn validate(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
    body: Bytes,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    let explain = p.flag("explain", false)?;
    // Un shard par index : demander « tous les shards » ne change rien a ce
    // qui est interroge, donc rien a la reponse.
    p.opt("all_shards");
    if p.opt("rewrite").is_some() {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [rewrite] sur [_validate/query] : il demande la forme \
             **reecrite** de la requete Lucene, que ferrite n'a pas (voir docs/compat.md)",
        ));
    }
    for param in [
        "q",
        "df",
        "default_operator",
        "analyzer",
        "analyze_wildcard",
        "lenient",
    ] {
        if p.opt(param).is_some() {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{param}] sur [_validate/query] : la recherche par \
                 chaine [q] (query_string) n'est pas implementee ; utilise le Query DSL dans le \
                 corps"
            )));
        }
    }
    p.done()?;

    let body = parse_body(&body)?;
    // Le corps est lu **avant** de resoudre les index : c'est ce qu'ES appelle
    // la validation du noeud coordinateur, et elle ne depend d'aucun mapping.
    let requete = match &body {
        Value::Null => None,
        // `_name` est retire ici comme sur `_search` : il ne change pas ce que
        // la requete vaut, et le laisser dans l'arbre le ferait prendre pour un
        // parametre inconnu — donc rendre `valid: false` sur une requete qu'ES
        // declare valide. Trouve par le fuzzer, le jour ou on lui a donne une
        // brique pour les clauses nommees.
        Value::Object(o) => match lire_corps(o).and_then(|q| match q {
            Some(v) => crate::dsl::extraire_noms(&v).map(|(n, _)| Some(n)),
            None => Ok(None),
        }) {
            Ok(q) => q,
            Err(e) => return Ok(invalide(explain, &e)),
        },
        _ => {
            return Ok(invalide(
                explain,
                &EsError::parsing("le corps de [_validate/query] doit etre un objet"),
            ))
        }
    };

    let maintenant = crate::datemath::maintenant();
    // Meme geste que la recherche sans index : la requete est construite contre
    // un schema vide, ou seule sa **forme** peut echouer. Une clause inconnue
    // sort ici, et c'est exactement ce qu'ES range en erreur de coordinateur.
    //
    // Seules les erreurs de **forme** comptent : contre un schema vide, un
    // `nested` sur un chemin qui n'y existe pas echoue aussi, et le prendre
    // pour une requete invalide rendait `valid: false` sur une requete qu'ES
    // declare valide. C'est le fuzzer differentiel qui l'a trouve, sur un
    // `nested` tire au sort.
    if let Some(q) = &requete {
        let vide = crate::engine::sans_index();
        let searcher = vide.searcher();
        let ctx = QueryCtx::new(&vide.fields, &vide.index, &searcher).avec_maintenant(maintenant);
        if let Err(e) = build_query(q, &ctx) {
            if erreur_de_forme(&e) {
                return Ok(invalide(explain, &e));
            }
        }
    }

    let indices = resoudre(&st.catalog, &index, &opts)?;
    let mut explications = Vec::new();
    let mut valide = true;
    for idx in &indices {
        let gen = idx.current();
        let searcher = gen.searcher();
        let ctx = QueryCtx::new(&gen.fields, &gen.index, &searcher)
            .avec_maintenant(maintenant)
            .avec_nom_index(&idx.name)
            .selon_le_mapping(&gen.mapping);
        let mut e = Map::new();
        e.insert("index".into(), json!(idx.name));
        match &requete {
            None => {
                e.insert("valid".into(), json!(true));
                if explain {
                    e.insert("explanation".into(), json!("*:*"));
                }
            }
            Some(q) => match build_query(q, &ctx) {
                Ok(query) => {
                    e.insert("valid".into(), json!(true));
                    if explain {
                        e.insert(
                            "explanation".into(),
                            json!(rendu(&query, &gen.index.schema())),
                        );
                    }
                }
                Err(err) => {
                    valide = false;
                    e.insert("valid".into(), json!(false));
                    if explain {
                        e.insert(
                            "error".into(),
                            json!(format!("[{}] {}: {}", idx.name, err.ty, err.reason)),
                        );
                    }
                }
            },
        }
        explications.push(Value::Object(e));
    }

    let n = indices.len();
    let mut out = Map::new();
    out.insert(
        "_shards".into(),
        json!({"total": n, "successful": n, "failed": 0}),
    );
    out.insert("valid".into(), json!(valide));
    if explain {
        out.insert("explanations".into(), Value::Array(explications));
    }
    Ok(Json::ok(Value::Object(out)))
}

/// Ce que ferrite a compris de la requete.
///
/// C'est le rendu de la requete **tantivy** construite, pas la chaine Lucene
/// qu'ES rendrait : voir l'entete du module. Une seule retouche, et elle est
/// necessaire : tantivy y ecrit ses champs par leur numero interne
/// (`field=11`), qui ne veut rien dire pour celui qui lit — on y remet le nom.
fn rendu(query: &dyn tantivy::query::Query, schema: &tantivy::schema::Schema) -> String {
    let brut = format!("{query:?}");
    // `AllQuery` est le seul cas ou les deux moteurs disent la meme chose, et
    // c'est le plus frequent (`_validate/query` sans corps, ou `match_all`).
    if brut == "AllQuery" {
        return "*:*".to_string();
    }
    nommer_les_champs(&brut, schema)
}

fn nommer_les_champs(brut: &str, schema: &tantivy::schema::Schema) -> String {
    // `brut` est le `Debug` de la requete, et il transporte les **valeurs**
    // cherchees : `{"term": {"k": "field=999999"}}` y ecrit `field=999999` en
    // toutes lettres. Un identifiant lu ici n'est donc pas forcement celui d'un
    // champ, et `get_field_entry` indexe un tableau sans borne — le processus
    // entier mourait sur `index out of bounds` (mesure : la requete ci-dessus
    // sur `_validate/query?explain=true` tuait le serveur).
    let combien = schema.fields().count() as u32;
    let mut out = String::with_capacity(brut.len());
    let mut reste = brut;
    let mut dedans = false; // dans une valeur citee ?
    while let Some(pos) = reste.find("field=") {
        // Une valeur est citee dans le `Debug` : ce qui est entre guillemets
        // est ce que le client a tape, pas un numero de champ. Compter les
        // guillemets non echappes est ce qui separe les deux.
        dedans = cite(&reste[..pos + 6], dedans);
        out.push_str(&reste[..pos + 6]);
        reste = &reste[pos + 6..];
        let fin = reste
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(reste.len());
        match reste[..fin].parse::<u32>() {
            Ok(id) if fin > 0 && !dedans && id < combien => {
                let champ = tantivy::schema::Field::from_field_id(id);
                out.push_str(schema.get_field_entry(champ).name());
            }
            _ => out.push_str(&reste[..fin]),
        }
        dedans = cite(&reste[..fin], dedans);
        reste = &reste[fin..];
    }
    out.push_str(reste);
    out
}

/// L'etat « dans une chaine citee » apres avoir lu ce morceau.
fn cite(morceau: &str, mut dedans: bool) -> bool {
    let mut echappe = false;
    for c in morceau.chars() {
        match c {
            _ if echappe => echappe = false,
            '\\' if dedans => echappe = true,
            '"' => dedans = !dedans,
            _ => {}
        }
    }
    dedans
}

/// Cette erreur porte-t-elle sur la **forme** de la requete, ou sur le mapping
/// contre lequel on la construit ?
///
/// C'est la frontiere qu'ES trace entre son noeud coordinateur et ses shards, et
/// elle decide de la forme de la reponse. Une clause inconnue ou un parametre
/// refuse ne dependent d'aucun mapping (`parsing_exception`,
/// `not_implemented_in_ferrite_exception`) ; tout le reste — un chemin `nested`
/// absent, une valeur qui n'a pas le type du champ — n'a de sens que sur un
/// index donne, et se juge index par index.
fn erreur_de_forme(e: &EsError) -> bool {
    // Le type ne suffit pas : ES range en `parsing_exception` des verdicts qui
    // viennent du **mapping** (une decroissance de `function_score` sur un
    // champ inconnu). Contre un schema vide, tout champ est inconnu — les
    // prendre pour des erreurs de forme rendrait `valid: false` sur des
    // requetes qu'ES declare valides. La marque « echec de shard » est ce qui
    // les separe (trouve par le fuzzer, deux fois : sur `nested` puis ici).
    e.champ_inconnu.is_none()
        && !e.de_shard
        && matches!(
            e.ty.as_str(),
            "parsing_exception" | crate::error::UNSUPPORTED
        )
}

/// Le corps de `_validate/query` : `{"query": …}` et rien d'autre.
///
/// ES range un corps mal forme en erreur de coordinateur — `valid: false`, et
/// non un 400. On rend donc l'erreur plutot que de la propager.
fn lire_corps(o: &Map<String, Value>) -> EsResult<Option<Value>> {
    for cle in o.keys() {
        if cle != "query" {
            return Err(EsError::parsing(format!(
                "request does not support [{cle}]"
            )));
        }
    }
    Ok(o.get("query").cloned())
}

/// La reponse d'une requete invalide **avant** tout mapping : pas de `_shards`,
/// comme chez ES.
fn invalide(explain: bool, e: &EsError) -> Json {
    let mut out = Map::new();
    out.insert("valid".into(), json!(false));
    if explain {
        out.insert("error".into(), json!(format!("{}: {}", e.ty, e.reason)));
    }
    Json::ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::schema::{Schema, STRING};

    /// Le `Debug` d'une requete transporte la **valeur** cherchee : un client
    /// qui tape `field=999999` y ecrit un numero de champ qui n'existe pas.
    /// `get_field_entry` indexe un tableau sans borne — le processus entier
    /// mourait sur `index out of bounds` (mesure : `_validate/query`
    /// `?explain=true` avec `{"term": {"k": "field=999999"}}`).
    #[test]
    fn une_valeur_qui_ressemble_a_un_numero_de_champ_ne_sort_pas_du_schema() {
        let mut b = Schema::builder();
        b.add_text_field("k", STRING);
        b.add_text_field("t", STRING);
        let schema = b.build();

        for valeur in ["field=999999", "field=0", "field=1"] {
            let brut = format!("TermQuery(Term(field=0, type=Str, \"{valeur}\"))");
            assert_eq!(
                nommer_les_champs(&brut, &schema),
                format!("TermQuery(Term(field=k, type=Str, \"{valeur}\"))"),
                "{valeur}"
            );
        }
        // Deux champs hors guillemets sont bien nommes tous les deux.
        assert_eq!(
            nommer_les_champs("(field=0, field=1)", &schema),
            "(field=k, field=t)"
        );
        // Un guillemet echappe dans la valeur ne rouvre pas la citation.
        assert_eq!(
            nommer_les_champs(r#"("a\"field=1b", field=1)"#, &schema),
            r#"("a\"field=1b", field=t)"#
        );
    }
}
