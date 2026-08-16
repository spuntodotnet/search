//! `_stats` : ce que ferrite sait vraiment compter sur un index.
//!
//! ES rend ici une vingtaine de groupes de compteurs. ferrite en rend
//! **quatre** — `docs`, `store`, `segments`, `shard_stats` — parce que ce sont
//! les quatre qu'il mesure. Les autres (`indexing`, `search`, `get`, `merges`,
//! `translog`, les caches…) ne sont pas rendus a zero : un `index_total: 0` sur
//! un index ou l'on vient d'ecrire mille documents ferait passer « non
//! mesure » pour « aucune activite », et c'est exactement l'echec silencieux
//! que ce projet refuse. Un client qui en nomme un
//! (`GET /_stats/indexing`) recoit donc un refus explicite.
//!
//! La forme, elle, est celle d'ES a l'octet pres : `_shards`, `_all`
//! (`primaries` / `total`), puis `indices`. Sur un moteur mono-shard sans
//! replique, `primaries` et `total` portent les memes nombres — c'est vrai, pas
//! une simplification.

use axum::extract::{Path, State};
use axum::http::Uri;
use serde_json::{json, Map, Value};

use super::{selection_options, Json, Params, SharedState};
use crate::engine::FerriteIndex;
use crate::error::{EsError, EsResult};
use crate::selection::resoudre;

/// Les groupes que ferrite mesure.
const SERVIS: &[&str] = &["docs", "store", "segments", "shard_stats"];

/// Les groupes qu'ES rend et que ferrite ne compte pas. Nommes ici pour que le
/// refus dise « ferrite ne sait pas » et non « ce nom n'existe pas ».
const NON_COMPTES: &[&str] = &[
    "bulk",
    "completion",
    "fielddata",
    "flush",
    "get",
    "indexing",
    "merge",
    "query_cache",
    "recovery",
    "refresh",
    "request_cache",
    "search",
    "suggest",
    "translog",
    "warmer",
];

/// `GET /_stats` et `GET /_stats/{metric}`
pub async fn stats_all(
    State(st): State<SharedState>,
    metric: Option<Path<String>>,
    uri: Uri,
) -> EsResult<Json> {
    stats_impl(st, "_all".to_string(), metric.map(|Path(m)| m), uri)
}

/// `GET /{index}/_stats` et `GET /{index}/_stats/{metric}`
pub async fn stats_index(
    State(st): State<SharedState>,
    Path(index): Path<String>,
    uri: Uri,
) -> EsResult<Json> {
    stats_impl(st, index, None, uri)
}

/// `GET /{index}/_stats/{metric}`
pub async fn stats_index_metric(
    State(st): State<SharedState>,
    Path((index, metric)): Path<(String, String)>,
    uri: Uri,
) -> EsResult<Json> {
    stats_impl(st, index, Some(metric), uri)
}

fn stats_impl(
    st: SharedState,
    index: String,
    metric_url: Option<String>,
    uri: Uri,
) -> EsResult<Json> {
    let mut p = Params::parse(&uri);
    let opts = selection_options(&mut p)?;
    let niveau = match p.opt("level") {
        None => Niveau::Indices,
        Some(v) => Niveau::parse(&v)?,
    };
    let metric_param = p.list("metric");
    // Ces deux-la filtrent des compteurs que ferrite ne rend pas du tout : les
    // accepter en silence rendrait la meme reponse qu'un appel qui ne les pose
    // pas, sans le dire.
    for param in ["fields", "completion_fields", "fielddata_fields", "groups"] {
        if p.opt(param).is_some() {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{param}] sur [_stats] : il filtre des compteurs que \
                 ferrite ne tient pas (voir docs/compat.md)"
            )));
        }
    }
    p.opt("include_segment_file_sizes");
    p.opt("include_unloaded_segments");
    p.opt("forbid_closed_indices");
    p.done()?;

    let chemin = uri.path().to_string();
    let demandes = match (metric_url, metric_param) {
        (Some(m), None) => Some(decouper(&m)),
        (None, Some(m)) => Some(m),
        (Some(_), Some(_)) => {
            return Err(EsError::illegal_argument(
                "[_stats] : [metric] est fourni dans l'URL et dans la query string ; n'en \
                 fournis qu'un",
            ))
        }
        (None, None) => None,
    };
    let groupes = groupes_demandes(demandes.as_deref(), &chemin)?;

    let indices = resoudre(&st.catalog, &index, &opts)?;
    let mut total = Compteurs::default();
    let mut par_index = Map::new();
    for idx in &indices {
        let c = Compteurs::de(idx)?;
        total.ajouter(&c);
        if niveau == Niveau::Cluster {
            continue;
        }
        let corps = c.to_json(&groupes);
        let mut e = Map::new();
        e.insert("uuid".into(), json!(idx.uuid));
        // Un shard, zero replique, et jamais de shard non assigne : la sante
        // d'un index de ferrite ne peut pas etre autre chose (voir la
        // divergence assumee sur `_cluster/health`).
        e.insert("health".into(), json!("green"));
        e.insert("status".into(), json!("open"));
        e.insert("primaries".into(), corps.clone());
        e.insert("total".into(), corps.clone());
        if niveau == Niveau::Shards {
            let mut shard = corps.clone();
            if let Value::Object(o) = &mut shard {
                o.insert(
                    "routing".into(),
                    json!({"state": "STARTED", "primary": true, "node": st.catalog.node_name}),
                );
            }
            e.insert("shards".into(), json!({"0": [shard]}));
        }
        par_index.insert(idx.name.clone(), Value::Object(e));
    }

    let n = indices.len();
    let tout = total.to_json(&groupes);
    let mut out = Map::new();
    out.insert(
        "_shards".into(),
        json!({"total": n, "successful": n, "failed": 0}),
    );
    out.insert(
        "_all".into(),
        json!({"primaries": tout, "total": tout.clone()}),
    );
    if niveau != Niveau::Cluster {
        out.insert("indices".into(), Value::Object(par_index));
    }
    Ok(Json::ok(Value::Object(out)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Niveau {
    Cluster,
    Indices,
    Shards,
}

impl Niveau {
    fn parse(v: &str) -> EsResult<Self> {
        match v {
            "cluster" => Ok(Self::Cluster),
            "indices" => Ok(Self::Indices),
            "shards" => Ok(Self::Shards),
            autre => Err(EsError::illegal_argument(format!(
                "level parameter must be one of [cluster] or [indices] or [shards] but was \
                 [{autre}]"
            ))),
        }
    }
}

/// Les compteurs d'un index, dans l'ordre ou ES les rend.
#[derive(Debug, Default, Clone, Copy)]
struct Compteurs {
    docs: u64,
    deleted: u64,
    taille: u64,
    segments: u64,
    shards: u64,
}

impl Compteurs {
    fn de(idx: &FerriteIndex) -> EsResult<Self> {
        let searcher = idx.searcher();
        let deleted: u64 = searcher
            .segment_readers()
            .iter()
            .map(|s| s.num_deleted_docs() as u64)
            .sum();
        Ok(Self {
            docs: idx.doc_count() as u64,
            deleted,
            taille: idx.store_size(),
            segments: searcher.segment_readers().len() as u64,
            shards: 1,
        })
    }

    fn ajouter(&mut self, autre: &Self) {
        self.docs += autre.docs;
        self.deleted += autre.deleted;
        self.taille += autre.taille;
        self.segments += autre.segments;
        self.shards += autre.shards;
    }

    fn to_json(self, groupes: &[&'static str]) -> Value {
        let mut o = Map::new();
        for g in groupes {
            let v = match *g {
                "docs" => json!({"count": self.docs, "deleted": self.deleted}),
                "shard_stats" => json!({"total_count": self.shards}),
                "store" => json!({
                    "size_in_bytes": self.taille,
                    "total_data_set_size_in_bytes": self.taille,
                    "reserved_in_bytes": 0,
                }),
                "segments" => json!({"count": self.segments}),
                _ => continue,
            };
            o.insert((*g).to_string(), v);
        }
        Value::Object(o)
    }
}

/// Traduit la demande de metriques en groupes servis, ou refuse.
fn groupes_demandes(demandes: Option<&[String]>, chemin: &str) -> EsResult<Vec<&'static str>> {
    let Some(demandes) = demandes else {
        return Ok(SERVIS.to_vec());
    };
    if demandes.iter().any(|m| m == "_all") {
        if demandes.len() > 1 {
            return Err(EsError::illegal_argument(format!(
                "request [{chemin}] contains _all and individual metrics [{}]",
                demandes.join(",")
            )));
        }
        return Ok(SERVIS.to_vec());
    }
    let mut out = Vec::new();
    for m in demandes {
        if let Some(g) = SERVIS.iter().find(|s| *s == m) {
            out.push(*g);
            continue;
        }
        if NON_COMPTES.contains(&m.as_str()) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas la metrique [{m}] de [_stats] : il ne tient pas ce \
                 compteur, et le rendre a zero ferait passer « non mesure » pour « aucune \
                 activite » (voir docs/compat.md)"
            )));
        }
        return Err(EsError::illegal_argument(format!(
            "request [{chemin}] contains unrecognized metric: [{m}]{}",
            match proche(m) {
                Some(p) => format!(" -> did you mean [{p}]?"),
                None => String::new(),
            }
        )));
    }
    // Toujours dans l'ordre d'ES, quel que soit l'ordre demande : une reponse
    // dont l'ordre depend de la question est plus dure a comparer.
    out.sort_by_key(|g| SERVIS.iter().position(|s| s == g).unwrap_or(usize::MAX));
    out.dedup();
    Ok(out)
}

/// Le « did you mean » d'ES : la metrique connue la plus proche, s'il y en a
/// une a distance raisonnable.
fn proche(saisi: &str) -> Option<&'static str> {
    let connues: Vec<&'static str> = SERVIS.iter().chain(NON_COMPTES.iter()).copied().collect();
    let mut meilleur: Option<(usize, &'static str)> = None;
    for c in connues {
        let d = distance(saisi, c);
        // Le seuil d'ES : une faute sur trois caracteres, au plus.
        if d == 0 || d > 1 + saisi.len() / 3 {
            continue;
        }
        if meilleur.is_none_or(|(md, _)| d < md) {
            meilleur = Some((d, c));
        }
    }
    meilleur.map(|(_, c)| c)
}

/// Distance de Levenshtein, sur les octets (les noms de metrique sont ASCII).
fn distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut ligne: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut precedent = ligne[0];
        ligne[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cout = usize::from(ca != cb);
            let nouveau = (ligne[j + 1] + 1).min(ligne[j] + 1).min(precedent + cout);
            precedent = ligne[j + 1];
            ligne[j + 1] = nouveau;
        }
    }
    ligne[b.len()]
}

fn decouper(v: &str) -> Vec<String> {
    v.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}
