#!/usr/bin/env python3
"""Les cas que le fuzzing differentiel a trouves, figes.

`fuzz_vs_es.py` tire au sort ; ce fichier-ci ne tire rien. Chaque cas y est
l'ecart **precis** qu'un tirage a sorti, reduit au plus petit mapping et aux
plus petits documents qui le montrent, puis pose aux deux serveurs.

Pourquoi les deux fichiers : une graine ne se rejoue qu'a generateur constant.
Ajouter une brique au tirage, changer une probabilite, et la graine 237 ne
designe plus le meme cas. Un ecart qui a coute une correction ne peut donc pas
rester une graine — il devient un cas ecrit, qui ne bougera plus.

    python3 tests/compat/sonde_fuzz.py [ferrite] [es]

Chaque cas porte la phrase de ce qui etait faux. Elle n'est pas decorative :
elle dit ce qu'on perd si le cas repasse au rouge.

Outil de developpement : exige un Elasticsearch 8.15 lance a cote (Docker).
"""
import json
import struct
import sys
import urllib.error
import urllib.request

INDEX = "sonde_fuzz"


def f32(v):
    """La valeur d'un `float` d'ES : un flottant 32 bits.

    ES stocke un `float` sur 32 bits et l'imprime au plus court texte qui y
    revient (`2894.4688`) ; ferrite le traduit en `f64` et l'imprime entier
    (`2894.46875`). C'est le meme flottant — cette fonction le verifie plutot
    que de le supposer — et c'est une divergence declaree
    (`type.flottants` dans compat.yaml), pas une neutralisation tacite."""
    if isinstance(v, float):
        return struct.unpack("f", struct.pack("f", v))[0]
    if isinstance(v, list):
        return [f32(x) for x in v]
    if isinstance(v, tuple):
        return tuple(f32(x) for x in v)
    if isinstance(v, dict):
        return {k: f32(x) for k, x in v.items()}
    return v


def http(base, method, path, body=None, brut=None):
    data = brut if brut is not None else (
        json.dumps(body).encode() if body is not None else None)
    req = urllib.request.Request(
        base + path, data=data, method=method,
        headers={"Content-Type": "application/x-ndjson" if brut is not None
                 else "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        corps = e.read()
        try:
            return e.code, json.loads(corps or b"{}")
        except json.JSONDecodeError:
            return e.code, {"raw": corps.decode(errors="replace")}


# ---------------------------------------------------------------------------
# Les jeux de donnees, minimaux, un par famille d'ecart
# ---------------------------------------------------------------------------

MULTI = {
    "mappings": {"properties": {
        "n": {"type": "integer"}, "k": {"type": "keyword"},
        "b": {"type": "boolean"}, "d": {"type": "date"},
        "f": {"type": "float"}, "u": {"type": "keyword"},
    }},
}
DOCS_MULTI = [
    ("a", {"n": [5, 1, 9], "k": ["m", "a", "z"], "b": [False, True],
           "d": ["2026-03-01", "2026-01-01"], "f": 2894.46875, "u": "a"}),
    ("b", {"n": 3, "k": "c", "b": True, "d": "2026-02-01", "f": -430.671875,
           "u": "b"}),
    # Sans aucune valeur : c'est lui qui montre la sentinelle de tri.
    ("c", {"u": "c"}),
    # Un entier qui vaut *exactement* la sentinelle : chez ES il est ex aequo
    # avec le document qui n'a rien, et c'est la cle suivante qui les departage.
    ("d", {"n": 9223372036854775807, "u": "d"}),
]

TROUS = {"mappings": {"properties": {"n": {"type": "integer"},
                                     "d": {"type": "date"},
                                     "dm": {"type": "date", "format": "yyyy-MM-dd"}}}}
DOCS_TROUS = [
    ("1", {"n": -200, "d": "2026-01-01", "dm": "2026-01-01"}),
    ("2", {"n": 5, "d": "2026-01-05", "dm": "2026-01-05"}),
    ("3", {"n": 50, "d": "2026-02-05", "dm": "2026-02-05"}),
    ("4", {"n": 5000, "d": "2026-03-05", "dm": "2026-03-05"}),
]

SCORES = {"mappings": {"properties": {"n": {"type": "integer"},
                                      "k": {"type": "keyword"},
                                      "b": {"type": "boolean"}}}}
DOCS_SCORES = [
    ("un", {"n": 5, "k": "a", "b": True}),
    ("trois", {"n": [5, 6, 7], "k": ["a", "b", "c"], "b": [True, False]}),
]

NESTE = {"mappings": {"properties": {"b": {"type": "nested", "properties": {
    "x": {"type": "double"}, "y": {"type": "date"}}}}}}
DOCS_NESTE = [("n1", {"b": [{"x": 1.0, "y": "2026-01-01"}]})]

TERMES = {"mappings": {"properties": {"k": {"type": "keyword"}}}}
DOCS_TERMES = [(f"t{i:02d}", {"k": f"v{i:02d}"}) for i in range(20)]

# Le meme champ, mais avec des comptes inegaux : c'est ce qu'il faut pour que
# `sum_other_doc_count` ait quelque chose a compter.
INEGAUX = {"mappings": {"properties": {"k": {"type": "keyword"}}}}
DOCS_INEGAUX = [(f"i{n:02d}", {"k": cle})
                for n, cle in enumerate(["a"] * 5 + ["b"] * 3 + ["c"] * 2
                                        + ["d", "e", "f"])]


# ---------------------------------------------------------------------------
# Les cas
#
# (jeu, libelle, ce qui etait faux, corps de la recherche, extraction)
# ---------------------------------------------------------------------------

def hits(r):
    return [(h["_id"], h.get("sort")) for h in r.get("hits", {}).get("hits", [])]


def agg(nom):
    return lambda r: r.get("aggregations", {}).get(nom)


def statut(r):
    """Pour un cas ou seule la frontiere accepte/refuse se compare.

    Les deux serveurs refusent, mais avec leurs propres mots : c'est le
    **verdict** qui doit coincider, pas la phrase."""
    return "ok" if "hits" in r or "aggregations" in r else "refus"


CAS = [
    # -- tri ---------------------------------------------------------------
    (MULTI, DOCS_MULTI, "tri multivalue croissant",
     "ferrite triait sur la premiere valeur du champ, ES sur le minimum",
     {"sort": [{"n": {"order": "asc"}}, {"u": {"order": "asc"}}],
      "_source": False, "size": 10}, hits),
    (MULTI, DOCS_MULTI, "tri multivalue decroissant",
     "meme chose dans l'autre sens : ES trie sur le maximum",
     {"sort": [{"n": {"order": "desc"}}, {"u": {"order": "asc"}}],
      "_source": False, "size": 10}, hits),
    (MULTI, DOCS_MULTI, "tri multivalue sur un keyword",
     "les ordinaux d'un dictionnaire suivent l'ordre lexicographique : le plus "
     "petit ordinal est la plus petite chaine",
     {"sort": [{"k": {"order": "desc"}}, {"u": {"order": "asc"}}],
      "_source": False, "size": 10}, hits),
    (MULTI, DOCS_MULTI, "tri multivalue sur une date",
     "une date multivaluee se trie comme un entier multivalue",
     {"sort": [{"d": {"order": "asc"}}, {"u": {"order": "asc"}}],
      "_source": False, "size": 10}, hits),
    (MULTI, DOCS_MULTI, "sentinelle de tri sur un entier",
     "ferrite rendait `null` ; ES rend 9223372036854775807, et cette valeur en "
     "est une : un document qui la porte est ex aequo avec un document vide",
     {"sort": [{"n": {"order": "asc"}}, {"u": {"order": "asc"}}],
      "_source": False, "size": 10}, hits),
    (MULTI, DOCS_MULTI, "sentinelle de tri sur un flottant",
     "ES rend la **chaine** \"Infinity\", JSON n'ayant pas l'infini",
     {"sort": [{"f": {"order": "asc"}}, {"u": {"order": "asc"}}],
      "_source": False, "size": 10}, hits),
    (MULTI, DOCS_MULTI, "sentinelle de tri sur un flottant, decroissant",
     "et \"-Infinity\" dans l'autre sens",
     {"sort": [{"f": {"order": "desc"}}, {"u": {"order": "asc"}}],
      "_source": False, "size": 10}, hits),
    (MULTI, DOCS_MULTI, "booleen dans un tableau sort",
     "ferrite rendait `true` / `false` ; ES rend 1 / 0",
     {"sort": [{"b": {"order": "desc"}}, {"u": {"order": "asc"}}],
      "_source": False, "size": 10}, hits),
    (MULTI, DOCS_MULTI, "sentinelle de tri sur un keyword",
     "sur un keyword, et seulement la, ES rend bien `null`",
     {"sort": [{"k": {"order": "asc"}}, {"u": {"order": "asc"}}],
      "_source": False, "size": 10}, hits),

    # -- agregation range --------------------------------------------------
    (TROUS, DOCS_TROUS, "range agg avec un trou",
     "tantivy comble les trous : un bucket 10.0-1000.0 que personne n'a "
     "demande s'ajoutait et decalait tous les suivants",
     {"size": 0, "aggs": {"a": {"range": {"field": "n", "ranges": [
         {"to": -100}, {"from": -100, "to": 10}, {"from": 1000}]}}}}, agg("a")),
    (TROUS, DOCS_TROUS, "range agg keyed",
     "la cle de la map etait celle de tantivy (`-100-10`), pas celle d'ES "
     "(`-100.0-10.0`), et le bucket repetait un `key` qu'ES n'y met pas",
     {"size": 0, "aggs": {"a": {"range": {"field": "n", "keyed": True, "ranges": [
         {"to": -100, "key": "bas"}, {"from": -100, "to": 10},
         {"from": 1000, "key": "haut"}]}}}}, agg("a")),
    (TROUS, DOCS_TROUS, "range agg sur une date",
     "les bornes partaient telles quelles a tantivy, qui compte en "
     "nanosecondes : tous les buckets sortaient vides",
     {"size": 0, "aggs": {"a": {"range": {"field": "d", "ranges": [
         {"to": "2026-01-03"}, {"from": "2026-01-03"}]}}}}, agg("a")),
    (TROUS, DOCS_TROUS, "range agg sur une date au format du champ",
     "le `format` du champ sert a lire la borne **et** a rendre les "
     "`*_as_string` ; et la cle nommee par le client survit",
     {"size": 0, "aggs": {"a": {"range": {"field": "dm", "ranges": [
         {"to": "2026-01-03", "key": "avant"}, {"from": "2026-01-03"}]}}}},
     agg("a")),
    (TROUS, DOCS_TROUS, "histogram keyed",
     "la cle de la map etait `-1000`, ES la rend `-1000.0`",
     {"size": 0, "aggs": {"a": {"histogram": {"field": "n", "interval": 1000,
                                              "keyed": True}}}}, agg("a")),
    (TROUS, DOCS_TROUS, "date_histogram keyed",
     "la cle de la map etait l'epoch, ES y met la date lisible",
     {"size": 0, "aggs": {"a": {"date_histogram": {
         "field": "d", "fixed_interval": "30d", "keyed": True}}}}, agg("a")),

    # -- agregations sur une date ------------------------------------------
    (TROUS, DOCS_TROUS, "terms sur un champ date",
     "la cle etait une chaine ISO ; ES rend les millisecondes et ajoute "
     "`key_as_string`",
     {"size": 0, "aggs": {"a": {"terms": {"field": "d"}}}}, agg("a")),
    (TROUS, DOCS_TROUS, "terms sur un champ date a format",
     "et le `key_as_string` suit le `format` declare du champ",
     {"size": 0, "aggs": {"a": {"terms": {"field": "dm"}}}}, agg("a")),
    (TROUS, DOCS_TROUS, "value_count sur un champ date",
     "« 4 documents » devenait 4e-06, avec un `value_as_string` a l'epoque Unix",
     {"size": 0, "aggs": {"a": {"value_count": {"field": "d"}}}}, agg("a")),
    (TROUS, DOCS_TROUS, "stats sur un bucket vide",
     "ES rend `sum: 0.0` mais **pas** de `sum_as_string` : une somme de zero "
     "date n'est pas l'epoque Unix",
     {"size": 0, "query": {"match_none": {}},
      "aggs": {"a": {"stats": {"field": "d"}}}}, agg("a")),

    # -- deux agregations de meme nom a deux niveaux ------------------------
    (TROUS, DOCS_TROUS, "deux agregations homonymes a deux niveaux",
     "les metadonnees de mise en forme etaient rangees par nom : le "
     "`date_histogram` nomme `x` heritait de la mise en forme du `range` nomme "
     "`x` de l'autre branche, et rendait zero bucket",
     {"size": 0, "aggs": {
         "p": {"terms": {"field": "n"},
               "aggs": {"x": {"date_histogram": {"field": "d",
                                                 "fixed_interval": "30d"}}}},
         "q": {"date_histogram": {"field": "d", "fixed_interval": "365d"},
               "aggs": {"x": {"range": {"field": "n", "ranges": [
                   {"to": 0}, {"from": 1000}]}}}}}},
     lambda r: r.get("aggregations")),

    # -- doc_count_error_upper_bound ---------------------------------------
    (INEGAUX, DOCS_INEGAUX, "sum_other_doc_count sur une troncature",
     "ce que `size` ecarte compte dans `sum_other_doc_count`, et rien d'autre",
     {"size": 0, "aggs": {"a": {"terms": {"field": "k", "size": 2}}}}, agg("a")),
    (TERMES, DOCS_TERMES, "doc_count_error_upper_bound en _count croissant",
     "toujours 0 ; ES rend -1 quand l'ordre est `_count` croissant et que le "
     "nombre de termes distincts atteint `shard_size` (size x 1,5 + 10)",
     {"size": 0, "aggs": {"a": {"terms": {"field": "k", "size": 3,
                                          "order": {"_count": "asc"}}}}},
     lambda r: r["aggregations"]["a"]["doc_count_error_upper_bound"]),
    (TERMES, DOCS_TERMES, "doc_count_error_upper_bound en _count decroissant",
     "le meme tirage en decroissant vaut 0 : la borne est calculable",
     {"size": 0, "aggs": {"a": {"terms": {"field": "k", "size": 3,
                                          "order": {"_count": "desc"}}}}},
     lambda r: r["aggregations"]["a"]["doc_count_error_upper_bound"]),
    (TERMES, DOCS_TERMES, "doc_count_error_upper_bound sous le seuil",
     "et 0 des que `size` est assez grand pour que rien ne soit perdu",
     {"size": 0, "aggs": {"a": {"terms": {"field": "k", "size": 50,
                                          "order": {"_count": "asc"}}}}},
     lambda r: r["aggregations"]["a"]["doc_count_error_upper_bound"]),

    # -- range sur un booleen ----------------------------------------------
    (SCORES, DOCS_SCORES, "range sur un booleen (lte)",
     "500 internal_server_error : le RangeQuery de tantivy refuse un booleen",
     {"query": {"range": {"b": {"lte": True}}}, "_source": False}, hits),
    (SCORES, DOCS_SCORES, "range sur un booleen (intervalle)",
     "un booleen n'a que deux valeurs : les bornes en designent un sous-ensemble",
     {"query": {"range": {"b": {"gte": False, "lt": True}}}, "_source": False},
     hits),
    (SCORES, DOCS_SCORES, "range sur un booleen (vide)",
     "et un intervalle qui n'en retient aucune ne rend rien",
     {"query": {"range": {"b": {"gt": True}}}, "_source": False}, hits),

    # -- scoring ------------------------------------------------------------
    (SCORES, DOCS_SCORES, "score d'un term sur un numerique",
     "note par BM25 avec fieldnorm, donc un document a trois valeurs marquait "
     "moins ; ES interroge un arbre de points et donne 1.0 a tout le monde",
     {"query": {"term": {"n": 5}}, "_source": False},
     lambda r: [(h["_id"], round(h["_score"], 4))
                for h in r["hits"]["hits"]]),
    (SCORES, DOCS_SCORES, "score d'un term sur un keyword",
     "un `keyword` d'ES est declare `norms: false` : deux documents qui "
     "portent la valeur marquent pareil, qu'ils aient une valeur ou trois",
     {"query": {"term": {"k": "a"}}, "_source": False},
     lambda r: [(h["_id"], round(h["_score"], 4))
                for h in r["hits"]["hits"]]),
    (SCORES, DOCS_SCORES, "score d'un term sur un booleen",
     "meme chose : chez Lucene un `boolean` est indexe `omitNorms`",
     {"query": {"term": {"b": True}}, "_source": False},
     lambda r: [(h["_id"], round(h["_score"], 4))
                for h in r["hits"]["hits"]]),
    (SCORES, DOCS_SCORES, "score d'un bool purement negatif",
     "ES donne 0.0 aux documents qu'un `bool` sans clause positive laisse "
     "passer, quel que soit son `boost` ; ferrite leur donnait le score de la "
     "clause implicite (1.5 sous un `boost: 1.5`), et l'ordre changeait des que "
     "ce `bool` etait combine a autre chose",
     {"query": {"bool": {"must_not": [{"term": {"k": "z"}}], "boost": 1.5}},
      "_source": False},
     lambda r: [(h["_id"], round(h["_score"], 4)) for h in r["hits"]["hits"]]),
    (SCORES, DOCS_SCORES, "score d'un bool negatif avec une clause positive",
     "des qu'une clause obligatoire est la, le score redevient le sien",
     {"query": {"bool": {"must": [{"match_all": {}}],
                         "must_not": [{"term": {"k": "z"}}]}}, "_source": False},
     lambda r: [(h["_id"], round(h["_score"], 4)) for h in r["hits"]["hits"]]),

    # -- refuses des deux cotes : c'est la frontiere qui se compare ------------
    (TROUS, DOCS_TROUS, "fuzzy sur une date",
     "une distance d'edition se mesure entre deux chaines ; ferrite construisait "
     "un terme texte sur une colonne de dates et rendait **zero document en "
     "200** — un resultat vide qui se fait passer pour une reponse. ES refuse, "
     "ferrite refuse maintenant aussi",
     {"query": {"fuzzy": {"d": "2026-01-01"}}, "_source": False}, statut),
    (TROUS, DOCS_TROUS, "fuzzy sur un numerique",
     "meme cause",
     {"query": {"fuzzy": {"n": "50"}}, "_source": False}, statut),
    (NESTE, DOCS_NESTE, "prefix sur une date, sous un nested",
     "la verification du type de champ existait a la racine et manquait dans la "
     "branche `nested` : un prefixe sur une date y rendait 200",
     {"query": {"nested": {"path": "b", "query": {"prefix": {"b.y": "20"}}}},
      "_source": False}, statut),
]

# Ce que ferrite refuse **expres** plutot que de rendre un resultat faux. ES sait
# repondre ; la question posee ici n'est donc pas « la meme reponse ? » mais
# « un refus, plutot qu'un 200 en silence ? ».
REFUS = [
    (SCORES, DOCS_SCORES, "histogram sur un champ multivalue",
     "l'agregation de tantivy compte les valeurs, ES compte les documents",
     {"size": 0, "aggs": {"a": {"histogram": {"field": "n", "interval": 100}}}}),
    (SCORES, DOCS_SCORES, "range agg sur un champ multivalue",
     "meme cause",
     {"size": 0, "aggs": {"a": {"range": {"field": "n", "ranges": [
         {"to": 10}, {"from": 10}]}}}}),
    (TROUS, DOCS_TROUS, "range agg a intervalles qui se chevauchent",
     "ES compte un document dans chaque bucket qui le contient ; "
     "l'agregation de tantivy partitionne",
     {"size": 0, "aggs": {"a": {"range": {"field": "n", "ranges": [
         {"to": 10}, {"from": 5, "to": 60}]}}}}),
    (TROUS, DOCS_TROUS, "terms min_doc_count 0",
     "tantivy rendrait zero bucket la ou ES en rend un par valeur de l'index",
     {"size": 0, "aggs": {"a": {"terms": {"field": "n", "min_doc_count": 0}}}}),
    (INEGAUX, DOCS_INEGAUX, "terms min_doc_count 2",
     "au-dela de 1, `sum_other_doc_count` ne suit plus la regle d'ES : elle "
     "depend de l'ordre demande, de la troncature et de l'ordre de parcours du "
     "dictionnaire de termes (27 ecarts sur 1 450 cas tires au sort avant le "
     "refus)",
     {"size": 0, "aggs": {"a": {"terms": {"field": "k", "size": 2,
                                          "min_doc_count": 2}}}}),
    (TROUS, DOCS_TROUS, "range agg sur une date, avec un trou",
     "le bucket de remplissage de tantivy avale l'intervalle demande quand les "
     "bornes sont des dates : le bucket `2026-02-01-*` disparaissait",
     {"size": 0, "aggs": {"a": {"range": {"field": "dm", "ranges": [
         {"to": "2026-01-03"}, {"from": "2026-02-01"}]}}}}),
]

# Les cas ou c'est **ES** qui casse. Ils sont ici pour ne pas etre repris pour
# des defauts de ferrite au prochain passage.
ES_CASSE = [
    ({"mappings": {"properties": {"e": {"type": "date", "format": "epoch_millis"},
                                  "u": {"type": "keyword"}}}},
     [("1", {"e": 1700000000000, "u": "1"}), ("2", {"u": "2"})],
     "stats sur un bucket vide, champ date au format epoch_millis",
     "ES 8.15 ne sait pas formater sa propre metrique vide : « Cannot format "
     "stat [max] with format [DocValueFormat.DateTime(format[epoch_millis]…)] ». "
     "ferrite repond 200 et une reponse correcte",
     {"size": 0, "query": {"match_none": {}},
      "aggs": {"a": {"stats": {"field": "e"}}}}),
]


def preparer(base, mapping, docs):
    http(base, "DELETE", f"/{INDEX}")
    http(base, "PUT", f"/{INDEX}",
         dict(mapping, settings={"number_of_shards": 1, "number_of_replicas": 0}))
    lignes = []
    for doc_id, doc in docs:
        lignes.append(json.dumps({"index": {"_index": INDEX, "_id": doc_id}}))
        lignes.append(json.dumps(doc))
    http(base, "POST", "/_bulk?refresh=true",
         brut=("\n".join(lignes) + "\n").encode())


def main():
    ferrite = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
    es = sys.argv[2] if len(sys.argv) > 2 else "http://localhost:9201"
    for nom, base in (("ferrite", ferrite), ("ES", es)):
        try:
            st, r = http(base, "GET", "/")
        except Exception as exc:  # noqa: BLE001
            print(f"# {nom} indisponible ({base}) : {exc}")
            return 2
        print(f"# {nom:<7} {base}  {r.get('version', {}).get('number', '?')}")

    ok = ecarts = 0
    prepare = None
    print(f"\n== {len(CAS)} ecarts trouves par le fuzzing, poses aux deux serveurs\n")
    for mapping, docs, libelle, faux, corps, extrait in CAS:
        cle = id(mapping)
        if cle != prepare:
            for base in (ferrite, es):
                preparer(base, mapping, docs)
            prepare = cle
        vus = []
        for base in (ferrite, es):
            st, r = http(base, "POST", f"/{INDEX}/_search", corps)
            try:
                vus.append(json.dumps(f32(extrait(r)), sort_keys=True, default=str))
            except Exception:  # noqa: BLE001
                vus.append(f"{st} {json.dumps(r)[:120]}")
        if vus[0] == vus[1]:
            ok += 1
            print(f"  [ ok  ] {libelle}")
        else:
            ecarts += 1
            print(f"  [ecart] {libelle}\n          etait faux : {faux}\n"
                  f"          ferrite : {vus[0][:220]}\n"
                  f"          ES      : {vus[1][:220]}")

    print(f"\n== {len(REFUS)} refus assumes — ES sait repondre, ferrite doit le dire\n")
    silences = 0
    for mapping, docs, libelle, pourquoi, corps in REFUS:
        for base in (ferrite, es):
            preparer(base, mapping, docs)
        prepare = None
        st_f, _ = http(ferrite, "POST", f"/{INDEX}/_search", corps)
        st_e, _ = http(es, "POST", f"/{INDEX}/_search", corps)
        muet = st_f == 200
        silences += muet
        print(f"  [{'!!!!!' if muet else ' ok  '}] {libelle} "
              f"(ferrite {st_f}, ES {st_e})")
        if muet:
            print(f"          rendu en silence : {pourquoi}")

    print(f"\n== {len(ES_CASSE)} cas ou c'est ES qui casse\n")
    for mapping, docs, libelle, quoi, corps in ES_CASSE:
        for base in (ferrite, es):
            preparer(base, mapping, docs)
        prepare = None
        st_f, _ = http(ferrite, "POST", f"/{INDEX}/_search", corps)
        st_e, _ = http(es, "POST", f"/{INDEX}/_search", corps)
        marque = " ok  " if st_f == 200 and st_e != 200 else "change"
        print(f"  [{marque}] {libelle} (ferrite {st_f}, ES {st_e})")
        if marque != " ok  ":
            print(f"          {quoi}")

    for base in (ferrite, es):
        http(base, "DELETE", f"/{INDEX}")
    print(f"\n{ok}/{len(CAS)} identiques, {ecarts} ecarts, "
          f"{silences} refus rendus en silence")
    return 1 if (ecarts or silences) else 0


if __name__ == "__main__":
    sys.exit(main())
