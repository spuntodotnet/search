#!/usr/bin/env python3
"""Sonde : les trois metriques qui restaient — `percentiles`, `extended_stats`,
`top_hits`.

Les trois ne posent pas la meme question, et c'est tout l'objet de ce fichier :

- `extended_stats` est **purement calculatoire**. Aucune approximation nulle
  part, donc aucune excuse : la variance, l'ecart-type et les bornes sigma
  doivent tomber au bit pres, y compris la ou ES rend `null` (zero document) et
  la ou il rend la **chaine** `"NaN"` (un seul document, variance
  d'echantillon) ;
- `top_hits` reproduit une **recherche entiere** a l'interieur d'un seau. Ce
  qu'on compare est donc le bloc `hits` complet — `total`, `max_score`,
  l'ordre, `_index`, `_id`, `_source`, le tableau `sort` — parce qu'un
  `top_hits` qui rend les bons documents dans le mauvais ordre est vert sur
  tout le reste ;
- `percentiles`, lui, n'est pas une fonction : c'est une **structure de
  donnees**. ES annonce lui-meme une approximation (un t-digest), dont le
  resultat depend de l'ordre d'insertion. Il n'y a donc pas de « bonne » valeur
  a viser dans l'absolu, et c'est la mesure ci-dessous qui devait etre faite
  **avant** d'ecrire une ligne de code.

## Ce que la mesure a dit sur `percentiles`

Elasticsearch 8.15 n'est approche qu'**au-dela de 2 000 valeurs**. En dessous,
son `TDigestState` retient les valeurs telles quelles et son quantile est une
**interpolation lineaire sur le tableau trie** :

    idx = q * (n - 1) ;  lo = floor(idx) ;  w = idx - lo
    v[lo] + w * (v[lo+1] - v[lo])           (et v[0] / v[n-1] aux bords)

C'est exact, et c'est reproductible au bit pres. La bascule se mesure a la
valeur pres : 1 999 valeurs dans le seau, ES est exact ; 2 000, il ne l'est
plus. La batterie `--frontiere` pose les deux cotes de cette bascule et imprime
l'ecart, parce que c'est lui la decision de la carte et non une phrase.

    python3 tests/compat/sonde_metriques.py [ferrite] [es]
    python3 tests/compat/sonde_metriques.py --calibrer [es_a] [es_b]
    python3 tests/compat/sonde_metriques.py --frontiere [ferrite] [es]

Elle **refuse de tourner** si elle ne trouve pas ses deux cibles : une sonde
differentielle qui ne compare rien rendrait « tout identique ». Et elle imprime
la **version** de chacune — un etalonnage a deux serveurs de la meme version
prouve le determinisme, pas l'independance a la version majeure.
"""
import json
import math
import sys
import urllib.error
import urllib.request

PETIT = "sonde-metriques"
GROS = "sonde-metriques-gros"


def http(base, method, path, body=None, ndjson=False):
    data = None
    headers = {}
    if body is not None:
        if ndjson:
            data = body.encode()
            headers["Content-Type"] = "application/x-ndjson"
        else:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        corps = e.read()
        try:
            return e.code, json.loads(corps or b"{}")
        except json.JSONDecodeError:
            return e.code, {"raw": corps.decode(errors="replace")}


MAPPING_PETIT = {
    "properties": {
        "v": {"type": "double"},
        "i": {"type": "long"},
        "d": {"type": "date"},
        "k": {"type": "keyword"},
        "t": {"type": "text"},
        "b": {"type": "boolean"},
        # `n` n'est renseigne que sur deux documents : c'est lui qui fait
        # exister un seau dont la metrique n'a **aucune** valeur.
        "n": {"type": "double"},
        # `m` est multivalue : ES compte alors les **valeurs**, doublons
        # compris, et c'est vrai des trois metriques de ce fichier.
        "m": {"type": "long"},
    }
}

DOCS_PETIT = {
    "0": {"v": 1.0, "i": 1, "d": "2026-01-01", "k": "a", "t": "aa", "b": True,
          "m": [1, 2, 3]},
    "1": {"v": 2.0, "i": 2, "d": "2026-01-02", "k": "a", "t": "bb", "b": False,
          "n": 5.0, "m": [10]},
    "2": {"v": 3.0, "i": 3, "d": "2026-01-03", "k": "b", "t": "cc", "b": True,
          "m": [1, 1, 2]},
    "3": {"v": 4.0, "i": 4, "d": "2026-01-04", "k": "b", "t": "dd", "b": False,
          "n": 7.0},
    "4": {"v": 10.0, "i": 10, "d": "2026-02-01", "k": "c", "t": "ee", "b": True},
}

MAPPING_GROS = {
    "properties": {"v": {"type": "double"}, "g": {"type": "keyword"}}
}

# Le corpus qui pose la bascule d'ES, a la valeur pres.
#
# `petit` porte 1 999 valeurs — le dernier compte pour lequel ES est exact ;
# `grand` en porte 2 000 — le premier pour lequel il ne l'est plus. Les deux
# tirages viennent du meme generateur congruentiel ecrit ici : une graine fixe
# et aucune dependance a la version de Python, sans quoi deux campagnes ne
# mesureraient pas le meme corpus.
FRONTIERE = 2000


def tirage(n, graine):
    """Un generateur congruentiel (celui de `glibc`), pour un corpus figé."""
    etat = graine
    out = []
    for _ in range(n):
        etat = (1103515245 * etat + 12345) % (1 << 31)
        out.append(round(etat / (1 << 31) * 1000.0, 3))
    return out


def docs_gros():
    out = {}
    for i, v in enumerate(tirage(FRONTIERE - 1, 7)):
        out[f"p{i}"] = {"v": v, "g": "petit"}
    for i, v in enumerate(tirage(FRONTIERE, 11)):
        out[f"g{i}"] = {"v": v, "g": "grand"}
    return out


def prepare(base, index, mapping, docs):
    http(base, "DELETE", "/" + index)
    st, corps = http(base, "PUT", "/" + index, {
        "mappings": mapping,
        "settings": {"number_of_shards": 1, "number_of_replicas": 0},
    })
    if st >= 300:
        print(f"# creation de [{index}] refusee sur {base} : {corps}")
        return False
    lignes = []
    for doc_id, doc in docs.items():
        lignes.append(json.dumps({"index": {"_index": index, "_id": doc_id}}))
        lignes.append(json.dumps(doc))
    for debut in range(0, len(lignes), 2000):
        http(base, "POST", "/_bulk",
             "\n".join(lignes[debut:debut + 2000]) + "\n", ndjson=True)
    http(base, "POST", f"/{index}/_refresh")
    return True


# ---------------------------------------------------------------------------
# La batterie
# ---------------------------------------------------------------------------
#
# Chaque cas est (libelle, index, corps de recherche). Le corps entier est
# compare : le bloc `aggregations` sur un 200, le type et la phrase du
# `root_cause` sur un refus.

MATCH_ALL = {"match_all": {}}


def cas_extended_stats():
    """`extended_stats` : purement calculatoire, donc aucune tolerance."""
    def q(aggs, query=None):
        c = {"size": 0, "aggs": aggs}
        c["query"] = query or MATCH_ALL
        return c
    return [
        ("es defaut", PETIT, q({"a": {"extended_stats": {"field": "v"}}})),
        ("es sur entier", PETIT, q({"a": {"extended_stats": {"field": "i"}}})),
        ("es sur date", PETIT, q({"a": {"extended_stats": {"field": "d"}}})),
        ("es sigma 0", PETIT, q({"a": {"extended_stats": {"field": "v", "sigma": 0}}})),
        ("es sigma 1.5", PETIT, q({"a": {"extended_stats": {"field": "v", "sigma": 1.5}}})),
        ("es sigma 3", PETIT, q({"a": {"extended_stats": {"field": "v", "sigma": 3}}})),
        ("es sigma entier", PETIT, q({"a": {"extended_stats": {"field": "v", "sigma": 2}}})),
        # ES refuse un sigma negatif a la lecture du corps, pas a l'execution.
        ("es sigma negatif", PETIT, q({"a": {"extended_stats": {"field": "v", "sigma": -1}}})),
        ("es champ partiel", PETIT, q({"a": {"extended_stats": {"field": "n"}}})),
        ("es missing", PETIT, q({"a": {"extended_stats": {"field": "n", "missing": 0}}})),
        ("es multivalue", PETIT, q({"a": {"extended_stats": {"field": "m"}}})),
        # Les trois comptes ou la formule change de branche.
        ("es zero document", PETIT, q({"a": {"extended_stats": {"field": "v"}}},
                                      {"term": {"k": "zzz"}})),
        ("es un document", PETIT, q({"a": {"extended_stats": {"field": "v"}}},
                                    {"term": {"k": "c"}})),
        ("es deux documents", PETIT, q({"a": {"extended_stats": {"field": "v"}}},
                                       {"term": {"k": "a"}})),
        ("es un document sur une date", PETIT,
         q({"a": {"extended_stats": {"field": "d"}}}, {"term": {"k": "c"}})),
        ("es zero document sur une date", PETIT,
         q({"a": {"extended_stats": {"field": "d"}}}, {"term": {"k": "zzz"}})),
        ("es sur keyword", PETIT, q({"a": {"extended_stats": {"field": "k"}}})),
        ("es sur text", PETIT, q({"a": {"extended_stats": {"field": "t"}}})),
        ("es sur booleen", PETIT, q({"a": {"extended_stats": {"field": "b"}}})),
        ("es champ absent", PETIT, q({"a": {"extended_stats": {"field": "zzz"}}})),
        ("es parametre inconnu", PETIT,
         q({"a": {"extended_stats": {"field": "v", "zzz": 1}}})),
        ("es sans field", PETIT, q({"a": {"extended_stats": {}}})),
        ("es porte une sous-agg", PETIT,
         q({"a": {"extended_stats": {"field": "v"}, "aggs": {"b": {"avg": {"field": "v"}}}}})),
        ("es sous terms", PETIT,
         q({"g": {"terms": {"field": "k"}, "aggs": {"a": {"extended_stats": {"field": "v"}}}}})),
        ("es sous terms, metrique vide", PETIT,
         q({"g": {"terms": {"field": "k"}, "aggs": {"a": {"extended_stats": {"field": "n"}}}}})),
        ("es sous filter", PETIT,
         q({"f": {"filter": {"term": {"k": "a"}},
                  "aggs": {"a": {"extended_stats": {"field": "v"}}}}})),
        ("es sous range", PETIT,
         q({"r": {"range": {"field": "v", "ranges": [{"to": 3}, {"from": 3}]},
                  "aggs": {"a": {"extended_stats": {"field": "v"}}}}})),
        ("es sous histogram, seau vide", PETIT,
         q({"h": {"histogram": {"field": "v", "interval": 2, "min_doc_count": 0},
                  "aggs": {"a": {"extended_stats": {"field": "v"}}}}})),
        ("es sous date_histogram", PETIT,
         q({"h": {"date_histogram": {"field": "d", "calendar_interval": "month"},
                  "aggs": {"a": {"extended_stats": {"field": "v"}}}}})),
        # L'ordre par sous-agregation : la seule forme d'ordre qui demande de
        # calculer la metrique **avant** de trier les seaux.
        ("es order variance desc", PETIT,
         q({"g": {"terms": {"field": "k", "order": {"a.variance": "desc"}},
                  "aggs": {"a": {"extended_stats": {"field": "v"}}}}})),
        ("es order std_deviation asc", PETIT,
         q({"g": {"terms": {"field": "k", "order": {"a.std_deviation": "asc"}},
                  "aggs": {"a": {"extended_stats": {"field": "v"}}}}})),
        ("es order sum_of_squares desc", PETIT,
         q({"g": {"terms": {"field": "k", "order": {"a.sum_of_squares": "desc"}},
                  "aggs": {"a": {"extended_stats": {"field": "v"}}}}})),
        ("es order variance_sampling desc", PETIT,
         q({"g": {"terms": {"field": "k", "order": {"a.variance_sampling": "desc"}},
                  "aggs": {"a": {"extended_stats": {"field": "v"}}}}})),
        ("es order count asc", PETIT,
         q({"g": {"terms": {"field": "k", "order": {"a.count": "asc"}},
                  "aggs": {"a": {"extended_stats": {"field": "v"}}}}})),
        # Les deux formes de chemin qu'ES refuse : sans propriete (metrique
        # multi-valuee), et vers un sous-objet.
        ("es order sans propriete", PETIT,
         q({"g": {"terms": {"field": "k", "order": {"a": "desc"}},
                  "aggs": {"a": {"extended_stats": {"field": "v"}}}}})),
        ("es order bounds.upper", PETIT,
         q({"g": {"terms": {"field": "k",
                            "order": {"a.std_deviation_bounds.upper": "desc"}},
                  "aggs": {"a": {"extended_stats": {"field": "v"}}}}})),
        ("es order propriete inconnue", PETIT,
         q({"g": {"terms": {"field": "k", "order": {"a.zzz": "desc"}},
                  "aggs": {"a": {"extended_stats": {"field": "v"}}}}})),
        ("es order variance, metrique vide", PETIT,
         q({"g": {"terms": {"field": "k", "order": {"a.variance": "desc"}},
                  "aggs": {"a": {"extended_stats": {"field": "n"}}}}})),
        ("es order min, metrique vide", PETIT,
         q({"g": {"terms": {"field": "k", "order": {"a.min": "desc"}},
                  "aggs": {"a": {"extended_stats": {"field": "n"}}}}})),
    ]


def cas_percentiles():
    def q(aggs, query=None, index=PETIT):
        c = {"size": 0, "aggs": aggs}
        c["query"] = query or MATCH_ALL
        return c
    return [
        ("p defaut", PETIT, q({"a": {"percentiles": {"field": "v"}}})),
        ("p keyed false", PETIT,
         q({"a": {"percentiles": {"field": "v", "keyed": False}}})),
        ("p keyed true", PETIT,
         q({"a": {"percentiles": {"field": "v", "keyed": True}}})),
        ("p percents", PETIT,
         q({"a": {"percentiles": {"field": "v", "percents": [10, 50, 90]}}})),
        # ES **trie** les percents demandes : `[99, 1, 50]` ressort `1, 50, 99`.
        ("p percents en desordre", PETIT,
         q({"a": {"percentiles": {"field": "v", "percents": [99, 1, 50]}}})),
        ("p percents fractionnaire", PETIT,
         q({"a": {"percentiles": {"field": "v", "percents": [33.3, 66.6]}}})),
        ("p percents 0 et 100", PETIT,
         q({"a": {"percentiles": {"field": "v", "percents": [0, 100]}}})),
        ("p percents doublon", PETIT,
         q({"a": {"percentiles": {"field": "v", "percents": [50, 50]}}})),
        ("p percents vide", PETIT,
         q({"a": {"percentiles": {"field": "v", "percents": []}}})),
        ("p percents > 100", PETIT,
         q({"a": {"percentiles": {"field": "v", "percents": [101]}}})),
        ("p percents negatif", PETIT,
         q({"a": {"percentiles": {"field": "v", "percents": [-1]}}})),
        ("p sur entier", PETIT, q({"a": {"percentiles": {"field": "i"}}})),
        ("p sur date", PETIT, q({"a": {"percentiles": {"field": "d"}}})),
        ("p sur booleen", PETIT, q({"a": {"percentiles": {"field": "b"}}})),
        ("p sur keyword", PETIT, q({"a": {"percentiles": {"field": "k"}}})),
        ("p sur text", PETIT, q({"a": {"percentiles": {"field": "t"}}})),
        ("p champ absent", PETIT, q({"a": {"percentiles": {"field": "zzz"}}})),
        ("p champ partiel", PETIT, q({"a": {"percentiles": {"field": "n"}}})),
        ("p missing", PETIT,
         q({"a": {"percentiles": {"field": "n", "missing": 0}}})),
        ("p missing sur une date", PETIT,
         q({"a": {"percentiles": {"field": "d", "missing": "2026-01-01"}}})),
        ("p multivalue", PETIT, q({"a": {"percentiles": {"field": "m"}}})),
        ("p zero document", PETIT,
         q({"a": {"percentiles": {"field": "v"}}}, {"term": {"k": "zzz"}})),
        ("p un document", PETIT,
         q({"a": {"percentiles": {"field": "v"}}}, {"term": {"k": "c"}})),
        ("p deux documents", PETIT,
         q({"a": {"percentiles": {"field": "v"}}}, {"term": {"k": "a"}})),
        ("p zero document, keyed false", PETIT,
         q({"a": {"percentiles": {"field": "v", "keyed": False}}},
           {"term": {"k": "zzz"}})),
        ("p zero document sur une date", PETIT,
         q({"a": {"percentiles": {"field": "d"}}}, {"term": {"k": "zzz"}})),
        ("p tdigest vide", PETIT,
         q({"a": {"percentiles": {"field": "v", "tdigest": {}}}})),
        ("p tdigest compression", PETIT,
         q({"a": {"percentiles": {"field": "v", "tdigest": {"compression": 200}}}})),
        ("p hdr", PETIT,
         q({"a": {"percentiles": {"field": "v",
                                  "hdr": {"number_of_significant_value_digits": 3}}}})),
        ("p parametre inconnu", PETIT,
         q({"a": {"percentiles": {"field": "v", "zzz": 1}}})),
        ("p sans field", PETIT, q({"a": {"percentiles": {}}})),
        ("p porte une sous-agg", PETIT,
         q({"a": {"percentiles": {"field": "v"}, "aggs": {"b": {"avg": {"field": "v"}}}}})),
        ("p sous terms", PETIT,
         q({"g": {"terms": {"field": "k"}, "aggs": {"a": {"percentiles": {"field": "v"}}}}})),
        ("p sous terms, metrique vide", PETIT,
         q({"g": {"terms": {"field": "k"}, "aggs": {"a": {"percentiles": {"field": "n"}}}}})),
        ("p sous terms sur un entier", PETIT,
         q({"g": {"terms": {"field": "i", "size": 3},
                  "aggs": {"a": {"percentiles": {"field": "v"}}}}})),
        ("p sous terms sur un booleen", PETIT,
         q({"g": {"terms": {"field": "b"},
                  "aggs": {"a": {"percentiles": {"field": "v"}}}}})),
        ("p sous terms sur une date", PETIT,
         q({"g": {"terms": {"field": "d", "size": 3},
                  "aggs": {"a": {"percentiles": {"field": "v"}}}}})),
        ("p sous filter", PETIT,
         q({"f": {"filter": {"term": {"k": "a"}},
                  "aggs": {"a": {"percentiles": {"field": "v"}}}}})),
        ("p sous range", PETIT,
         q({"r": {"range": {"field": "v", "ranges": [{"to": 3}, {"from": 3}]},
                  "aggs": {"a": {"percentiles": {"field": "v"}}}}})),
        ("p sous histogram, seau vide", PETIT,
         q({"h": {"histogram": {"field": "v", "interval": 2, "min_doc_count": 0},
                  "aggs": {"a": {"percentiles": {"field": "v"}}}}})),
        ("p sous date_histogram", PETIT,
         q({"h": {"date_histogram": {"field": "d", "calendar_interval": "month"},
                  "aggs": {"a": {"percentiles": {"field": "v"}}}}})),
        ("p sous deux niveaux", PETIT,
         q({"g": {"terms": {"field": "k"},
                  "aggs": {"h": {"range": {"field": "v",
                                           "ranges": [{"to": 5}, {"from": 5}]},
                                 "aggs": {"a": {"percentiles": {"field": "v"}}}}}}})),
        ("p order par percentile", PETIT,
         q({"g": {"terms": {"field": "k", "order": {"a.50": "desc"}},
                  "aggs": {"a": {"percentiles": {"field": "v"}}}}})),
        ("p et une autre metrique", PETIT,
         q({"a": {"percentiles": {"field": "v"}}, "b": {"avg": {"field": "v"}}})),
        # Le corpus de 4 000 documents : les deux cotes de la bascule d'ES.
        ("p 1999 valeurs (ES exact)", GROS,
         q({"a": {"percentiles": {"field": "v", "percents": [1, 5, 25, 50, 75, 95, 99]}}},
           {"term": {"g": "petit"}})),
        ("p 2000 valeurs (ES approche)", GROS,
         q({"a": {"percentiles": {"field": "v", "percents": [1, 5, 25, 50, 75, 95, 99]}}},
           {"term": {"g": "grand"}})),
        ("p sous terms, 1999 et 2000", GROS,
         q({"g": {"terms": {"field": "g"},
                  "aggs": {"a": {"percentiles": {"field": "v"}}}}})),
    ]


def cas_top_hits():
    def q(aggs, query=None):
        c = {"size": 0, "aggs": aggs}
        c["query"] = query or MATCH_ALL
        return c
    return [
        ("th nu", PETIT, q({"a": {"top_hits": {}}})),
        ("th size 2", PETIT, q({"a": {"top_hits": {"size": 2}}})),
        ("th size 100", PETIT, q({"a": {"top_hits": {"size": 100}}})),
        ("th size 0", PETIT, q({"a": {"top_hits": {"size": 0}}})),
        ("th from", PETIT,
         q({"a": {"top_hits": {"size": 1, "from": 1, "sort": [{"v": "asc"}]}}})),
        ("th from au-dela", PETIT,
         q({"a": {"top_hits": {"size": 2, "from": 10, "sort": [{"v": "asc"}]}}})),
        ("th sort desc", PETIT,
         q({"a": {"top_hits": {"size": 2, "sort": [{"v": "desc"}]}}})),
        ("th sort deux cles", PETIT,
         q({"a": {"top_hits": {"size": 3, "sort": [{"b": "asc"}, {"v": "desc"}]}}})),
        ("th sort sur une date", PETIT,
         q({"a": {"top_hits": {"size": 2, "sort": [{"d": "desc"}]}}})),
        ("th sort _score", PETIT,
         q({"a": {"top_hits": {"size": 2, "sort": ["_score"]}}})),
        ("th sort missing", PETIT,
         q({"a": {"top_hits": {"size": 5,
                               "sort": [{"n": {"order": "asc", "missing": "_last"}}]}}})),
        ("th source false", PETIT, q({"a": {"top_hits": {"size": 1, "_source": False}}})),
        ("th source true", PETIT, q({"a": {"top_hits": {"size": 1, "_source": True}}})),
        ("th source liste", PETIT,
         q({"a": {"top_hits": {"size": 1, "_source": ["v", "k"]}}})),
        ("th source includes/excludes", PETIT,
         q({"a": {"top_hits": {"size": 1,
                               "_source": {"includes": ["v", "k"], "excludes": ["k"]}}}})),
        ("th docvalue_fields", PETIT,
         q({"a": {"top_hits": {"size": 1, "docvalue_fields": ["v", "k"]}}})),
        # `fields` **s'herite** de la recherche englobante, et rien d'autre.
        # Aucune documentation ne le dit, et ferrite rendait un hit sans son
        # bloc `fields` — en 200, trouve par une plage de controle du fuzzer.
        # Les quatre cas suivants disent la regle en entier : il s'herite, un
        # `fields` declare dans le `top_hits` le **remplace** au lieu de s'y
        # ajouter, `_source: false` ne le supprime pas, et ni
        # `docvalue_fields` ni `stored_fields` ne s'heritent.
        ("th herite du fields de la recherche", PETIT,
         dict(q({"a": {"top_hits": {"size": 1}}}), fields=["v"])),
        ("th fields remplace celui de la recherche", PETIT,
         dict(q({"a": {"top_hits": {"size": 1, "fields": ["i"]}}}), fields=["v"])),
        ("th herite du fields sans _source", PETIT,
         dict(q({"a": {"top_hits": {"size": 1, "_source": False}}}), fields=["v"])),
        ("th n'herite pas du docvalue_fields", PETIT,
         dict(q({"a": {"top_hits": {"size": 1}}}), docvalue_fields=["v"])),
        ("th herite du fields joker", PETIT,
         dict(q({"a": {"top_hits": {"size": 1}}}), fields=["*"])),
        ("th sous terms herite du fields", PETIT,
         dict(q({"g": {"terms": {"field": "k"},
                       "aggs": {"a": {"top_hits": {"size": 1}}}}}),
              fields=["v"])),
        ("th fields", PETIT, q({"a": {"top_hits": {"size": 1, "fields": ["v"]}}})),
        ("th stored_fields", PETIT,
         q({"a": {"top_hits": {"size": 1, "stored_fields": ["v"]}}})),
        ("th script_fields vide", PETIT,
         q({"a": {"top_hits": {"size": 1, "script_fields": {}}}})),
        ("th highlight", PETIT,
         q({"a": {"top_hits": {"size": 1, "highlight": {"fields": {"t": {}}}}}},
           {"match": {"t": "aa"}})),
        ("th explain", PETIT, q({"a": {"top_hits": {"size": 1, "explain": True}}})),
        ("th version", PETIT, q({"a": {"top_hits": {"size": 1, "version": True}}})),
        ("th seq_no_primary_term", PETIT,
         q({"a": {"top_hits": {"size": 1, "seq_no_primary_term": True}}})),
        ("th track_scores", PETIT,
         q({"a": {"top_hits": {"size": 1, "sort": [{"v": "desc"}], "track_scores": True}}})),
        ("th parametre inconnu", PETIT, q({"a": {"top_hits": {"zzz": 1}}})),
        ("th porte une sous-agg", PETIT,
         q({"a": {"top_hits": {"size": 1}, "aggs": {"b": {"avg": {"field": "v"}}}}})),
        ("th sous terms", PETIT,
         q({"g": {"terms": {"field": "k"},
                  "aggs": {"a": {"top_hits": {"size": 1, "sort": [{"v": "desc"}]}}}}})),
        ("th sous terms, plusieurs", PETIT,
         q({"g": {"terms": {"field": "k"},
                  "aggs": {"a": {"top_hits": {"size": 3, "sort": [{"v": "asc"}]}},
                           "m": {"avg": {"field": "v"}}}}})),
        ("th sous filter", PETIT,
         q({"f": {"filter": {"term": {"k": "b"}},
                  "aggs": {"a": {"top_hits": {"size": 2, "sort": [{"v": "asc"}]}}}}})),
        ("th sous range", PETIT,
         q({"r": {"range": {"field": "v", "ranges": [{"to": 3}, {"from": 3}]},
                  "aggs": {"a": {"top_hits": {"size": 1, "sort": [{"v": "asc"}]}}}}})),
        ("th sous histogram, seau vide", PETIT,
         q({"h": {"histogram": {"field": "v", "interval": 2, "min_doc_count": 0},
                  "aggs": {"a": {"top_hits": {"size": 1, "sort": [{"v": "asc"}]}}}}})),
        ("th sous date_histogram", PETIT,
         q({"h": {"date_histogram": {"field": "d", "calendar_interval": "month"},
                  "aggs": {"a": {"top_hits": {"size": 1, "sort": [{"v": "asc"}]}}}}})),
        # Un `top_hits` rend des hits, donc il rend leur `matched_queries` :
        # ES rejoue chaque clause nommee contre les documents d'un seau comme
        # contre ceux de la reponse. ferrite ne les rendait pas — trouve par
        # une plage de controle du fuzzer, pas par les questions ecrites ici.
        ("th matched_queries", PETIT,
         {"size": 0,
          "query": {"bool": {"should": [
              {"term": {"k": {"value": "a", "_name": "na"}}},
              {"term": {"i": {"value": 10, "_name": "ni"}}}]}},
          "aggs": {"a": {"top_hits": {"size": 3, "_source": False,
                                      "sort": [{"v": "asc"}]}}}}),
        ("th matched_queries sous terms", PETIT,
         {"size": 0,
          "query": {"bool": {"should": [
              {"term": {"k": {"value": "a", "_name": "na"}}},
              {"term": {"b": {"value": True, "_name": "nb"}}}]}},
          "aggs": {"g": {"terms": {"field": "k"},
                         "aggs": {"a": {"top_hits": {"size": 2, "_source": False,
                                                     "sort": [{"v": "asc"}]}}}}}}),
        ("th sous une requete", PETIT,
         q({"a": {"top_hits": {"size": 2, "sort": [{"v": "asc"}]}}},
           {"range": {"v": {"gte": 3}}})),
        ("th order par top_hits", PETIT,
         q({"g": {"terms": {"field": "k", "order": {"a": "desc"}},
                  "aggs": {"a": {"top_hits": {"size": 1}}}}})),
        ("th sous terms sur un booleen", PETIT,
         q({"g": {"terms": {"field": "b"},
                  "aggs": {"a": {"top_hits": {"size": 1, "sort": [{"v": "asc"}]}}}}})),
    ]


# ---------------------------------------------------------------------------
# Ce que ferrite refuse, et pourquoi. Un refus n'est « assume » que s'il est
# ecrit **ici** et que c'est bien ferrite qui refuse la ou ES repond.
# ---------------------------------------------------------------------------
REFUS_ASSUMES = {
    "es sur booleen": "ES agrege un booleen comme 0/1 ; ferrite refuse deja "
                      "[stats] sur ce type, et [extended_stats] suit",
    "p sur booleen": "meme raison que [stats] sur un booleen",
    "p sous terms sur un booleen": "meme raison",
    "th sous terms sur un booleen": "le seau existe, mais [percentiles] et "
                                    "[top_hits] y sont refuses par le type du "
                                    "champ agrege — voir la ligne precedente",
    "es sur date": "la somme des carres s'accumule en nanosecondes et ne se "
                   "ramene pas en millisecondes carrees sans perdre ses bits "
                   "de poids faible : sur un seul document ES rend "
                   "[std_deviation: 0.0], ferrite rendait [23170.475]",
    "es un document sur une date": "meme raison",
    "es zero document sur une date": "meme raison",
    "p tdigest compression": "le parametre choisit l'approximation d'ES ; "
                             "ferrite rend l'exact, il n'a pas de compression",
    "p hdr": "un autre algorithme approche, que ferrite ne reproduit pas",
    "p order par percentile": "l'ordre par sous-agregation se calcule avant la "
                              "troncature ; ferrite calcule les percentiles "
                              "apres, seau par seau",
    "th highlight": "le surlignage se resout sur la requete de la recherche, "
                    "pas sur celle d'un seau",
    "th explain": "l'arbre du score d'un top_hits n'est pas resolu",
    "th version": "le hit d'un top_hits ne porte pas [_version]",
    "th seq_no_primary_term": "le hit d'un top_hits ne porte pas [_seq_no]",
    "th track_scores": "il faudrait noter les documents sous un tri par champ",
    # Ces deux-la ne viennent pas de cette carte : ferrite refuse **toute**
    # agregation sur un champ qu'aucun mapping ne declare, la ou ES rend un
    # resultat vide. Mesure : `{"avg": {"field": "zzz"}}` rend 400 chez ferrite
    # et `{"value": null}` chez ES. Il est ecrit ici parce qu'il se voit ici.
    "es champ absent": "ferrite refuse une agregation sur un champ non mappe, "
                       "et ce n'est pas propre a [extended_stats] : [avg] fait "
                       "de meme (divergence anterieure a la carte)",
    "p champ absent": "meme raison",
}

# Les refus que les **deux** serveurs prononcent, et dont seule la phrase ou le
# type differe. Ils sont ranges a part parce qu'ils ne mesurent pas la meme
# chose : le client voit une erreur des deux cotes, et son code se branche sur
# le statut avant la phrase.
PHRASES_ASSUMEES = {
    "es sigma negatif": "ES prefixe ses erreurs de lecture de corps par la "
                        "position dans le JSON brut ([1:70]), que ferrite n'a "
                        "plus une fois le corps parse (divergence declaree)",
    "es parametre inconnu": "meme prefixe [ligne:colonne]",
    "p parametre inconnu": "meme prefixe [ligne:colonne]",
    "es sur text": "ferrite garde sa propre phrase pour [text] sur les "
                   "agregations anterieures a la carte ; les deux refusent",
    "p sur text": "meme raison",
    "es order sans propriete": "ES range ses erreurs de chemin d'ordre sous un "
                               "[search_phase_execution_exception] au "
                               "root_cause vide ; ferrite rend directement "
                               "l'[illegal_argument_exception] qu'il porte",
    "es order bounds.upper": "meme raison",
    "es order propriete inconnue": "meme raison",
    "th order par top_hits": "ES rend [invalid_path] avec sa phrase ; ferrite "
                             "refuse en nommant l'agregation",
}

# Les ecarts **mesures** que ferrite assume en rendant une autre valeur qu'ES,
# et non un refus. Il n'y en a qu'une famille, et c'est la decision de la carte :
# au-dela de 2 000 valeurs, ES cesse d'etre exact et ferrite ne cesse pas.
DIVERGENCES_MESUREES = {
    "p 2000 valeurs (ES approche)":
        "ES bascule sur son t-digest a 2 000 valeurs ; ferrite rend le "
        "percentile exact",
    "p sous terms, 1999 et 2000":
        "le seau [grand] porte 2 000 valeurs : ES y est approche, ferrite exact",
}


# ---------------------------------------------------------------------------


def neutralise(v):
    """Les deux seules valeurs d'un hit qui ne peuvent pas coincider.

    `_node` est l'identifiant du noeud et `_shard` le nom du shard : deux
    serveurs n'en portent jamais les memes, et `--calibrer` en compterait un
    ecart qui ne mesure rien. Ils sont remplaces, pas retires — leur
    **presence** est comparee, elle, et c'est elle qui dit qu'`explain` a
    change la forme du hit.
    """
    if isinstance(v, dict):
        return {k: ("<noeud>" if k in ("_node", "_shard") else neutralise(x))
                for k, x in v.items()}
    if isinstance(v, list):
        return [neutralise(x) for x in v]
    return v


def interroge(base, index, corps):
    st, rep = http(base, "POST", f"/{index}/_search", corps)
    if st == 200:
        return "ok", neutralise(rep.get("aggregations", {}))
    err = rep.get("error", {})
    racine = (err.get("root_cause") or [{}])[0]
    return "err", {"status": st, "type": racine.get("type") or err.get("type"),
                   "reason": racine.get("reason") or err.get("reason")}


def compare(a, b, chemin, ecarts):
    """Compare deux blocs JSON. Les flottants doivent tomber au bit pres.

    Aucune tolerance : les trois metriques de ce fichier sont calculatoires,
    et une tolerance choisie ici serait exactement le genre de nombre que ce
    depot refuse de publier. Le seul flou tolere est celui d'un `_score` de
    BM25, et aucun cas de la batterie n'en compare.
    """
    if isinstance(a, dict) and isinstance(b, dict):
        for cle in sorted(set(a) | set(b)):
            if cle not in a:
                ecarts.append(f"{chemin}.{cle} absent a gauche (droite : "
                              f"{json.dumps(b[cle])[:70]})")
            elif cle not in b:
                ecarts.append(f"{chemin}.{cle} en trop a gauche "
                              f"({json.dumps(a[cle])[:70]})")
            else:
                compare(a[cle], b[cle], f"{chemin}.{cle}", ecarts)
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            ecarts.append(f"{chemin} : {len(a)} elements a gauche, {len(b)} a droite")
        for i, (x, y) in enumerate(zip(a, b)):
            compare(x, y, f"{chemin}[{i}]", ecarts)
    elif isinstance(a, bool) or isinstance(b, bool):
        if a is not b:
            ecarts.append(f"{chemin} : {a!r} / {b!r}")
    elif isinstance(a, (int, float)) and isinstance(b, (int, float)):
        if float(a) != float(b):
            ecarts.append(f"{chemin} : {a!r} / {b!r}")
    elif a != b:
        ecarts.append(f"{chemin} : {json.dumps(a)[:70]} / {json.dumps(b)[:70]}")


def abrege(v, n=150):
    s = json.dumps(v, sort_keys=True)
    return s if len(s) <= n else s[:n] + "…"


def version(base):
    _, corps = http(base, "GET", "/")
    v = corps.get("version", {})
    return f"{corps.get('tagline', '?')[:24]} {v.get('number', '?')}"


# ---------------------------------------------------------------------------
# `--frontiere` : la mesure qui a tranche la carte.
# ---------------------------------------------------------------------------

PCTS = [1, 5, 25, 50, 75, 95, 99]


def quantile_exact(valeurs, q):
    """Le quantile d'Elasticsearch **quand il est exact**.

    C'est l'interpolation lineaire de son `TDigestState` en mode tri, reproduite
    telle quelle : `idx = q * (n - 1)`, puis `v[lo] + w * (v[lo+1] - v[lo])`.
    Ecrite ici en Python pour que la sonde puisse dire, de chaque valeur rendue,
    si elle est exacte — sans redemander a ferrite ce qu'on est en train de
    mesurer.
    """
    v = sorted(valeurs)
    n = len(v)
    if n == 0:
        return None
    if n == 1:
        return v[0]
    idx = q / 100.0 * (n - 1)
    if idx <= 0:
        return v[0]
    if idx >= n - 1:
        return v[-1]
    lo = math.floor(idx)
    return v[lo] + (idx - lo) * (v[lo + 1] - v[lo])


def frontiere(cibles):
    """Ou ES cesse d'etre exact, et de combien il s'ecarte ensuite."""
    gauche, droite = cibles[0][1], cibles[1][1]
    print("\n== la bascule d'Elasticsearch, mesuree a la valeur pres")
    print("   (n valeurs dans le seau ; ecart relatif maximal sur "
          f"{PCTS} au percentile exact)\n")
    print("   " + "  ".join([f"{'n':>6}"] + [f"{nom:>12}" for nom, _ in cibles]))
    index = "sonde-metriques-frontiere"
    for n in (1997, 1998, 1999, 2000, 2001, 2048, 5000, 20000):
        vals = tirage(n, 3 + n)
        docs = {str(i): {"v": x, "g": "u"} for i, x in enumerate(vals)}
        ligne = [f"   {n:>6}"]
        for _, base in cibles:
            if not prepare(base, index, MAPPING_GROS, docs):
                ligne.append(f"{'?':>12}")
                continue
            _, rep = http(base, "POST", f"/{index}/_search",
                          {"size": 0,
                           "aggs": {"a": {"percentiles": {"field": "v",
                                                          "percents": PCTS}}}})
            vues = (rep.get("aggregations", {}).get("a", {}) or {}).get("values")
            if not isinstance(vues, dict):
                ligne.append(f"{'refus':>12}")
                continue
            pire = 0.0
            for p in PCTS:
                attendu = quantile_exact(vals, p)
                vu = vues.get(f"{float(p)}")
                if vu is None or attendu is None:
                    continue
                pire = max(pire, abs(vu - attendu) / max(1e-12, abs(attendu)))
            ligne.append(f"{pire * 100:11.5f}%")
        print("  ".join(ligne))
    for _, base in cibles:
        http(base, "DELETE", "/" + index)
    print("\n   0.00000% = le percentile exact, au bit pres.")


def main():
    argv = [a for a in sys.argv[1:] if not a.startswith("--")]
    calibrer = "--calibrer" in sys.argv
    seule_frontiere = "--frontiere" in sys.argv
    gauche = argv[0] if argv else ("http://localhost:9201" if calibrer
                                   else "http://localhost:9200")
    droite = argv[1] if len(argv) > 1 else ("http://localhost:9202" if calibrer
                                            else "http://localhost:9201")
    cibles = [("es_a" if calibrer else "ferrite", gauche),
              ("es_b" if calibrer else "es", droite)]
    for nom, base in cibles:
        try:
            http(base, "GET", "/")
        except Exception as e:  # noqa: BLE001
            print(f"# {nom} indisponible ({base}) : {e}")
            print("# une sonde differentielle qui ne compare rien ne rend pas "
                  "de verdict : arret.")
            return 2
    for nom, base in cibles:
        print(f"# {nom} = {base} : {version(base)}")

    if seule_frontiere:
        frontiere(cibles)
        return 0

    for _, base in cibles:
        prepare(base, PETIT, MAPPING_PETIT, DOCS_PETIT)
        prepare(base, GROS, MAPPING_GROS, docs_gros())

    batterie = cas_extended_stats() + cas_percentiles() + cas_top_hits()
    print(f"\n== {len(batterie)} questions posees aux deux cibles\n")
    ecarts = assumes = mesures = phrases = 0
    for libelle, index, corps in batterie:
        reps = [(nom, *interroge(base, index, corps)) for nom, base in cibles]
        (_, kg, vg), (_, kd, vd) = reps
        detail = []
        if kg != kd:
            detail.append(f"{kg} a gauche, {kd} a droite")
        elif kg == "err":
            if vg["status"] != vd["status"] or vg["type"] != vd["type"]:
                detail.append(f"{vg['status']} {vg['type']} / "
                              f"{vd['status']} {vd['type']}")
            elif (vg["reason"] or "") != (vd["reason"] or ""):
                detail.append(f"phrase : {str(vg['reason'])[:70]} / "
                              f"{str(vd['reason'])[:70]}")
        else:
            compare(vg, vd, "", detail)

        if not detail:
            print(f"  {libelle:38} {abrege(vg, 90)}")
            continue
        gauche_refuse = kg == "err" and kd == "ok"
        deux_refusent = kg == "err" and kd == "err"
        if libelle in REFUS_ASSUMES and gauche_refuse and not calibrer:
            assumes += 1
            print(f"~ {libelle:38} {abrege(vg, 90)}")
            print(f"      assume : {REFUS_ASSUMES[libelle]}")
        elif libelle in PHRASES_ASSUMEES and deux_refusent and not calibrer:
            phrases += 1
            print(f"= {libelle:38} {abrege(vg, 90)}")
            print(f"      phrase : {PHRASES_ASSUMEES[libelle]}")
        elif libelle in DIVERGENCES_MESUREES and not calibrer:
            mesures += 1
            print(f"# {libelle:38} {len(detail)} valeurs differentes")
            print(f"      mesure : {DIVERGENCES_MESUREES[libelle]}")
            for x in detail[:4]:
                print(f"         {x}")
        else:
            ecarts += 1
            print(f"* {libelle:38}")
            for x in detail[:6]:
                print(f"         {x}")
            if len(detail) > 6:
                print(f"         … et {len(detail) - 6} de plus")

    for _, base in cibles:
        http(base, "DELETE", "/" + PETIT)
        http(base, "DELETE", "/" + GROS)
    total = len(batterie)
    print(f"\n{total - ecarts - assumes - mesures - phrases}/{total} identiques, "
          f"{assumes} refus assumes, {phrases} refus des deux cotes dont la "
          f"phrase differe, {mesures} ecarts mesures et declares, "
          f"{ecarts} ecarts")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
