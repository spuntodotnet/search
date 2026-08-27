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

# Le surlignage : six ecarts trouves par le fuzzing, chacun invisible au
# harnais ecrit a la main parce qu'il demande une combinaison a laquelle on
# n'avait pas pense (un tiret dans un mot, une premiere valeur vide, un
# `should` sous un `filter` qui echoue).
SURLIGNE = {
    "mappings": {"properties": {
        "t": {"type": "text"},
        "k": {"type": "keyword", "ignore_above": 12},
        "src": {"type": "text", "copy_to": "tout"},
        "tout": {"type": "text"},
        "u": {"type": "keyword"},
    }},
}
DOCS_SURLIGNE = [
    ("a", {"t": "aluminium batterie aluminium leger ecole", "k": "tiret-bas",
           "src": "capteur edition batterie verre", "u": "a"}),
    # Une premiere valeur **vide** : ES saute les separateurs de tete, donc
    # `no_match_size` lui rend « delta », pas rien.
    ("b", {"t": "optique verre et rien d'autre", "k": ["", "delta"], "u": "b"}),
    # Une valeur plus longue que l'`ignore_above` du champ : elle n'a pas ete
    # indexee, donc elle ne se surligne pas — et `no_match_size` l'ignore.
    ("c", {"t": "beta tout court", "k": "beaucoup trop longue pour douze",
           "u": "c"}),
    # Des blancs aux deux bords, et un espace fine (U+2009) que `String.trim()`
    # de Java **ne** rogne pas — il s'arrete a U+0020.
    ("d", {"t": ["  cible et du texte autour  ", "cible\u2009", "cible\t"],
           "u": "d"}),
    # Une valeur de `keyword` vide **au milieu** puis **a la fin** : la
    # premiere rend `<em></em>`, la seconde rien du tout.
    ("e", {"t": "compact hotel l'ascension silencieux ecran",
           "k": ["alpha", "", "beta"], "u": "e"}),
    ("f", {"t": "aa bb cible cc dd cible ee ff", "k": ["alpha", ""], "u": "f"}),
    # Un `keyword` dont la valeur porte ses propres blancs : le terme les
    # contient, donc le surlignage aussi.
    ("g", {"t": "rien", "k": "  espaces   multiples  ", "u": "g"}),
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

NESTE = {"mappings": {"properties": {
    "b": {"type": "nested", "properties": {"x": {"type": "double"},
                                           "y": {"type": "date"}}},
    # Un champ de la racine : il sert d'agregation porteuse, pour verifier que
    # le refus d'un sous-champ de `nested` vaut aussi en sous-agregation.
    "k": {"type": "keyword"}}}}
DOCS_NESTE = [("n1", {"k": "a", "b": [{"x": 1.0, "y": "2026-01-01"},
                                      {"x": 3.0, "y": "2026-02-01"}]}),
              ("n2", {"k": "b", "b": [{"x": 8.0, "y": "2026-03-01"}]})]

# Un champ dont l'analyzer porte un filtre a n-grammes : ses grammes occupent
# **tous** la meme position. C'est le seul cas ou une phrase cesse d'etre une
# suite de termes pour devenir une suite de positions a alternatives.
GRAMMES = {
    "settings": {
        "index": {"max_ngram_diff": 12},
        "analysis": {
            "filter": {"gr": {"type": "ngram", "min_gram": 2, "max_gram": 5}},
            "analyzer": {"a": {"type": "custom", "tokenizer": "standard",
                               "filter": ["lowercase", "gr"]}},
        },
    },
    "mappings": {"properties": {"g": {"type": "text", "analyzer": "a"}}},
}
DOCS_GRAMMES = [("g1", {"g": "ecole normale"}), ("g2", {"g": "ecologie"}),
                ("g3", {"g": "le pre"}), ("g4", {"g": "une grande ecole"})]

TERMES = {"mappings": {"properties": {"k": {"type": "keyword"}}}}
DOCS_TERMES = [(f"t{i:02d}", {"k": f"v{i:02d}"}) for i in range(20)]

# Un champ flottant dont une valeur est **entiere** : c'est la seule forme qui
# separe la cle `2` de la cle `2.0`, et aucun corpus ecrit a la main n'en
# contient — le fuzzer, lui, tire `0.0` et `1024.0` expres.
ENTIERS_FLOTTANTS = {"mappings": {"properties": {"f": {"type": "double"},
                                                 "g": {"type": "float"}}}}
DOCS_ENTIERS_FLOTTANTS = [("1", {"f": 2.0, "g": 2}), ("2", {"f": 3.5, "g": 3.5}),
                          ("3", {})]

# Le meme champ, mais avec des comptes inegaux : c'est ce qu'il faut pour que
# `sum_other_doc_count` ait quelque chose a compter.
INEGAUX = {"mappings": {"properties": {"k": {"type": "keyword"}}}}
DOCS_INEGAUX = [(f"i{n:02d}", {"k": cle})
                for n, cle in enumerate(["a"] * 5 + ["b"] * 3 + ["c"] * 2
                                        + ["d", "e", "f"])]


# Un mot plus long que la limite des tokenizers, et un champ a n-grammes pour
# le budget de `match_phrase_prefix`.
LONGS = {
    "settings": {"index": {"max_ngram_diff": 12}, "analysis": {
        "filter": {"gr": {"type": "ngram", "min_gram": 1, "max_gram": 3,
                          "preserve_original": True}},
        "analyzer": {"ng": {"type": "custom", "tokenizer": "standard",
                            "filter": ["lowercase", "gr"]}}}},
    "mappings": {"properties": {
        "t": {"type": "text"},
        "g": {"type": "text", "analyzer": "ng"},
        "u": {"type": "keyword"},
    }},
}
DOCS_LONGS = [
    ("1", {"t": "z" * 254, "g": "reduction", "u": "1"}),
    ("2", {"t": "z" * 255, "g": "compact", "u": "2"}),
    ("3", {"t": "z" * 300, "g": "hotel", "u": "3"}),
    # Le mot long est **entre** deux mots courts : c'est lui qui montre que les
    # positions de ce qui suit se decalent.
    ("4", {"t": "avant " + "z" * 300 + " apres", "g": "reductio", "u": "4"}),
    ("5", {"t": "avant apres", "g": "aluminium", "u": "5"}),
]

COPIES = {"mappings": {"properties": {
    "titre": {"type": "text", "copy_to": "tout"},
    "n": {"type": "long", "copy_to": "tout"},
    "tout": {"type": "keyword"},
    "a": {"type": "text", "copy_to": "b"},
    "b": {"type": "text", "copy_to": "c"},
    "c": {"type": "text"},
    "u": {"type": "keyword"},
}}}
DOCS_COPIES = [
    ("1", {"titre": "horla", "n": 7, "a": "zebre", "u": "1"}),
    ("2", {"titre": "bel ami", "u": "2"}),
    # Sans aucune source : c'est lui qui doit tomber dans le `missing`.
    ("3", {"u": "3"}),
]

STOCKES = {"mappings": {"properties": {
    "k": {"type": "keyword", "store": True},
    "n": {"type": "integer", "store": True},
    "f": {"type": "float", "store": True},
    "d": {"type": "date", "format": "yyyy-MM-dd HH:mm:ss", "store": True},
    "libre": {"type": "keyword"},
    "l": {"type": "nested", "properties": {"x": {"type": "keyword", "store": True}}},
    "u": {"type": "keyword"},
}}}
DOCS_STOCKES = [
    ("1", {"k": ["b", "a", "b"], "n": [3, 1, 1], "f": 0.1, "libre": "dans le source",
           "d": "2023-06-18 20:26:30", "l": [{"x": "e1"}, {"x": "e2"}], "u": "1"}),
    ("2", {"libre": "seulement", "u": "2"}),
]

# Un champ indexe en grammes et cherche en mots entiers, et son jumeau sans
# `search_analyzer` : les deux ensemble disent ce que le parametre change.
RECHERCHE = {
    "settings": {"index": {"max_ngram_diff": 12}, "analysis": {
        "filter": {"eg": {"type": "edge_ngram", "min_gram": 1, "max_gram": 15}},
        "analyzer": {"eg_a": {"type": "custom", "tokenizer": "standard",
                              "filter": ["asciifolding", "lowercase", "eg"]}}}},
    "mappings": {"properties": {
        "t": {"type": "text", "analyzer": "eg_a", "search_analyzer": "standard"},
        "s": {"type": "text", "analyzer": "eg_a"},
        "u": {"type": "keyword"},
    }},
}
DOCS_RECHERCHE = [
    ("1", {"t": "Elan bleu", "s": "Elan bleu", "u": "1"}),
    ("2", {"t": "Elephant", "s": "Elephant", "u": "2"}),
]


# ---------------------------------------------------------------------------
# Les cas
#
# (jeu, libelle, ce qui etait faux, corps de la recherche, extraction)
# ---------------------------------------------------------------------------

def hits(r):
    return [(h["_id"], h.get("sort")) for h in r.get("hits", {}).get("hits", [])]


def agg(nom):
    return lambda r: r.get("aggregations", {}).get(nom)


def surligne(r):
    """Le bloc `highlight` de chaque hit, par identifiant."""
    return {h["_id"]: h.get("highlight")
            for h in r.get("hits", {}).get("hits", [])}


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

    # -- surlignage --------------------------------------------------------
    (SURLIGNE, DOCS_SURLIGNE, "un tiret ne coupe pas un mot",
     "le `BreakIterator` de Java joint deux lettres par un tiret, la ou UAX#29 "
     "coupe : `no_match_size: 5` rendait « tiret- » au lieu de « tiret-bas »",
     {"query": {"term": {"u": "a"}}, "size": 10, "sort": ["u"],
      "highlight": {"fields": {"k": {}}, "no_match_size": 5}}, surligne),
    (SURLIGNE, DOCS_SURLIGNE, "une premiere valeur vide ne coute pas le fragment",
     "ES concatene les valeurs et saute les separateurs de tete : "
     "`no_match_size` rend la premiere valeur **non vide**",
     {"query": {"term": {"u": "b"}}, "size": 10, "sort": ["u"],
      "highlight": {"fields": {"k": {}}, "no_match_size": 5}}, surligne),
    (SURLIGNE, DOCS_SURLIGNE, "ignore_above ecarte la valeur du surlignage",
     "une valeur trop longue n'a pas ete indexee : ni surlignee, ni rendue par "
     "`no_match_size` — lire le `_source` n'est pas lire ce qui est indexe",
     {"query": {"term": {"u": "c"}}, "size": 10, "sort": ["u"],
      "highlight": {"fields": {"k": {}}, "no_match_size": 200}}, surligne),
    (SURLIGNE, DOCS_SURLIGNE, "un keyword n'est pas analyse par la clause",
     "`match` sur un `keyword` cherche la valeur **entiere** : l'analyser "
     "coupait « tiret-bas » en deux termes, et plus rien ne se surlignait",
     {"query": {"match": {"k": "tiret-bas"}}, "size": 10, "sort": ["u"],
      "highlight": {"fields": {"k": {}}}}, surligne),
    (SURLIGNE, DOCS_SURLIGNE, "la cible d'un copy_to se surligne",
     "sa valeur n'est nulle part dans son `_source` : elle est dans celui de "
     "la source, et ES la surligne quand meme",
     {"query": {"match": {"tout": "batterie"}}, "size": 10, "sort": ["u"],
      "highlight": {"fields": {"tout": {}}}}, surligne),
    (SURLIGNE, DOCS_SURLIGNE, "deux marques qui se chevauchent",
     "debut croissant, **fin decroissante** : la plus longue d'abord rend "
     "`<em>optique verre</em>`, l'autre ordre rendait "
     "`<em>optique</em><em> verre</em>`",
     {"query": {"dis_max": {"queries": [
         {"match_phrase_prefix": {"t": "optiq"}},
         {"match_phrase": {"t": "optique verre"}}]}},
      "size": 10, "sort": ["u"], "highlight": {"fields": {"t": {}}}}, surligne),
    (SURLIGNE, DOCS_SURLIGNE, "un should sous un filter qui echoue ne marque rien",
     "ES ne surligne que ce qui a fait correspondre **ce** document : le "
     "`bool` ne tient pas, donc son `should` ne marque pas, meme quand le "
     "document sort par l'autre branche du `dis_max`",
     {"query": {"dis_max": {"queries": [
         {"exists": {"field": "u"}},
         {"bool": {"should": [{"match": {"t": "beta"}}],
                   "filter": [{"match": {"t": "introuvable"}}]}}]}},
      "size": 10, "sort": ["u"], "highlight": {"fields": {"t": {}}}}, surligne),
    (SURLIGNE, DOCS_SURLIGNE, "un must_not match_all rend le bool sterile",
     "la reecriture de Lucene en fait un `MatchNoDocsQuery` : ses termes "
     "disparaissent, y compris sous `require_field_match: false`",
     {"query": {"dis_max": {"queries": [
         {"exists": {"field": "u"}},
         {"bool": {"should": [{"match": {"t": "beta"}}],
                   "must_not": [{"match_all": {}}]}}]}},
      "size": 10, "sort": ["u"],
      "highlight": {"require_field_match": False, "fields": {"t": {}}}},
     surligne),
    (SURLIGNE, DOCS_SURLIGNE, "require_field_match false eclate les phrases",
     "ES y perd la structure de la phrase et marque chaque terme separement, "
     "aux positions ou la phrase l'a trouve",
     {"query": {"match_phrase": {"t": "aluminium batterie"}}, "size": 10,
      "sort": ["u"],
      "highlight": {"require_field_match": False, "number_of_fragments": 0,
                    "fields": {"t": {}}}}, surligne),

    (SURLIGNE, DOCS_SURLIGNE, "number_of_fragments 0 ne rogne pas",
     "ES n'y passe plus par le decoupeur borne : le fragment sort avec ses "
     "blancs de bord",
     {"query": {"term": {"u": "d"}}, "size": 10, "sort": ["u"],
      "highlight": {"fields": {"t": {}}, "number_of_fragments": 0,
                    "require_field_match": False}}, surligne),
    (SURLIGNE, DOCS_SURLIGNE, "le rognage est celui de String.trim()",
     "il s'arrete a U+0020 : la tabulation part, l'espace fine (U+2009) reste "
     "— et la longueur qui note le fragment est celle d'**avant** rognage",
     {"query": {"match": {"t": "cible"}}, "size": 10, "sort": ["u"],
      "highlight": {"fields": {"t": {}}, "number_of_fragments": 2}}, surligne),

    (SURLIGNE, DOCS_SURLIGNE, "une valeur de keyword vide se surligne",
     "vide **au milieu** d'autres valeurs, elle rend `<em></em>` ; vide **en "
     "derniere position**, elle ne rend rien — ES s'arrete des que la "
     "correspondance commence au-dela du dernier caractere du champ",
     {"query": {"terms": {"k": ["", ""]}}, "size": 10, "sort": ["u"],
      "highlight": {"fields": {"k": {}}}}, surligne),
    (SURLIGNE, DOCS_SURLIGNE, "un keyword porte ses propres blancs",
     "le terme est la valeur entiere : le rognage ne mord pas dessus",
     {"query": {"term": {"k": "  espaces   multiples  "}}, "size": 10,
      "sort": ["u"], "highlight": {"fields": {"k": {}}}}, surligne),
    (SURLIGNE, DOCS_SURLIGNE, "la borne droite s'arrete a la correspondance",
     "quand `fragment_size` ne laisse plus de place, le fragment finit **a la "
     "fin de la marque**, pas a la frontiere de mot suivante — un caractere "
     "d'ecart, et une phrase entiere de difference sur la marque",
     {"query": {"dis_max": {"queries": [
         {"match_phrase": {"t": "l'ascension silencieux ecran"}},
         {"match_phrase": {"t": "l'ascension"}}]}},
      "size": 10, "sort": ["u"],
      "highlight": {"fields": {"t": {}}, "fragment_size": 20}}, surligne),
    (SURLIGNE, DOCS_SURLIGNE, "un regexp compte pour un terme, pas pour un mot",
     "le `PassageScorer` note un fragment clause par clause : deux mots "
     "differents trouves par le **meme** `regexp` pesent comme un mot vu deux "
     "fois, et ca change quel fragment survit a `number_of_fragments`",
     {"query": {"regexp": {"t": "[a-z].*"}}, "size": 10, "sort": ["u"],
      "highlight": {"fields": {"t": {"fragment_size": 15,
                                     "number_of_fragments": 2}}}}, surligne),

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

    # -- sous-agregations d'un bucket vide ---------------------------------
    #
    # `histogram` et `date_histogram` comblent leurs trous des deux cotes. Mais
    # tantivy ne fait pas tourner ce qu'il y a **dessous** dans les buckets
    # qu'il fabrique : une sous-agregation `range` y rendait `buckets: []`. Un
    # graphe qui empile deux niveaux perdait donc ses categories sur les
    # periodes creuses, en 200 et sans un mot.
    (TROUS, DOCS_TROUS, "range sous un bucket vide de histogram",
     "tantivy comble les trous sans executer ce qu'il y a dessous : le `range` "
     "rendait `buckets: []` dans les buckets 1000 a 4000, la ou ES rend ses "
     "trois intervalles a `doc_count: 0`",
     {"size": 0, "aggs": {"a": {"histogram": {"field": "n", "interval": 1000},
                                "aggs": {"r": {"range": {"field": "n", "ranges": [
                                    {"to": 0}, {"from": 0, "to": 100},
                                    {"from": 100}]}}}}}}, agg("a")),
    (TROUS, DOCS_TROUS, "range keyed sous un bucket vide de histogram",
     "meme chose sous la forme `keyed`, ou la map sortait vide",
     {"size": 0, "aggs": {"a": {"histogram": {"field": "n", "interval": 1000},
                                "aggs": {"r": {"range": {
                                    "field": "n", "keyed": True, "ranges": [
                                        {"to": 0, "key": "bas"},
                                        {"from": 0, "key": "haut"}]}}}}}},
     agg("a")),
    (TROUS, DOCS_TROUS, "range sous un bucket vide de date_histogram",
     "le remplissage d'un `date_histogram` a le meme trou",
     {"size": 0, "aggs": {"a": {"date_histogram": {"field": "d",
                                                   "fixed_interval": "5d"},
                                "aggs": {"r": {"range": {"field": "n", "ranges": [
                                    {"to": 0}, {"from": 0}]}}}}}}, agg("a")),
    (TROUS, DOCS_TROUS, "deux niveaux sous un bucket vide de histogram",
     "ce qui est sous le `range` disparaissait avec lui : le `terms` et le "
     "`stats` de chaque intervalle n'existaient meme pas",
     {"size": 0, "aggs": {"a": {"histogram": {"field": "n", "interval": 1000},
                                "aggs": {"r": {
                                    "range": {"field": "n", "ranges": [
                                        {"to": 0}, {"from": 0}]},
                                    "aggs": {"t": {"terms": {"field": "n"}},
                                             "s": {"stats": {"field": "n"}}}}}}}},
     agg("a")),
    (TROUS, DOCS_TROUS, "range sous un bucket d'extended_bounds",
     "un bucket demande par `extended_bounds` est vide pour la meme raison, et "
     "perdait ses sous-agregations de la meme facon",
     {"size": 0, "aggs": {"a": {
         "histogram": {"field": "n", "interval": 1000,
                       "extended_bounds": {"min": -3000, "max": 8000}},
         "aggs": {"r": {"range": {"field": "n", "ranges": [
             {"to": 0}, {"from": 0}]}}}}}}, agg("a")),
    (TROUS, DOCS_TROUS, "recherche sans resultat, histogram + range",
     "quand rien ne correspond, **tous** les buckets sont fabriques : c'est le "
     "cas ou la perte est totale",
     {"size": 0, "query": {"match_none": {}}, "aggs": {"a": {
         "histogram": {"field": "n", "interval": 1000,
                       "extended_bounds": {"min": 0, "max": 2000}},
         "aggs": {"r": {"range": {"field": "n", "ranges": [
             {"to": 0}, {"from": 0}]}}}}}}, agg("a")),

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
    (NESTE, DOCS_NESTE, "tri sur un sous-champ de nested depuis la racine",
     "ferrite triait sur les valeurs a plat et rendait un ordre en 200 ; ES "
     "refuse (« it is mandatory to set the [nested] context on the nested sort "
     "field »). Les deux refusent maintenant",
     {"sort": [{"b.x": "asc"}], "_source": False}, statut),
    # -- n-grammes ---------------------------------------------------------
    (GRAMMES, DOCS_GRAMMES, "match_phrase d'un mot sur un champ a n-grammes",
     "un filtre a n-grammes pose **tous** les grammes d'un mot a la meme "
     "position : ce sont des alternatives, pas une suite. ferrite les "
     "enchainait, donc il rendait 1 document (« g3 » n'a que le gramme `le ») "
     "la ou ES en rend 4 — en 200",
     {"query": {"match_phrase": {"g": "ecole"}}, "_source": False, "size": 10},
     lambda r: sorted(h["_id"] for h in r["hits"]["hits"])),
    (GRAMMES, DOCS_GRAMMES, "match operator=and sur un champ a n-grammes",
     "`and` porte sur les **positions**, pas sur les termes : Lucene fait "
     "l'union des grammes d'un mot avant d'appliquer l'operateur. ferrite les "
     "exigeait tous, donc « le document contient tous les grammes du mot "
     "cherche » — 1 document au lieu de 4, en 200",
     {"query": {"match": {"g": {"query": "ecole", "operator": "and"}}},
      "_source": False, "size": 10},
     lambda r: sorted(h["_id"] for h in r["hits"]["hits"])),
    (GRAMMES, DOCS_GRAMMES, "match d'un mot sur un champ a n-grammes",
     "le meme decoupage vu par la clause qui, elle, a toujours ete une union : "
     "c'est la reponse de reference du cas precedent",
     {"query": {"match": {"g": "ecole"}}, "_source": False, "size": 10},
     lambda r: sorted(h["_id"] for h in r["hits"]["hits"])),
    (GRAMMES, DOCS_GRAMMES, "match_phrase_prefix d'un mot sur un champ a n-grammes",
     "meme cause, autre clause : chaque gramme y est developpe par son prefixe",
     {"query": {"match_phrase_prefix": {"g": "ecol"}}, "_source": False,
      "size": 10}, lambda r: sorted(h["_id"] for h in r["hits"]["hits"])),
    # -- cles flottantes ---------------------------------------------------
    # Trouve par la brique `store` / `copy_to`, et anterieur aux deux : une
    # valeur entiere dans un champ flottant ressort avec le type JSON de
    # l'entier chez tantivy. Un client qui type strictement sa reponse y lit un
    # entier la ou ES lui donne un flottant.
    (ENTIERS_FLOTTANTS, DOCS_ENTIERS_FLOTTANTS, "terms sur un double, cle entiere",
     "ferrite rendait la cle `2` la ou ES rend `2.0`",
     {"size": 0, "aggs": {"a": {"terms": {"field": "f"}}}}, agg("a")),
    (ENTIERS_FLOTTANTS, DOCS_ENTIERS_FLOTTANTS, "terms sur un float, cle entiere",
     "meme chose sur 32 bits",
     {"size": 0, "aggs": {"a": {"terms": {"field": "g"}}}}, agg("a")),
    (ENTIERS_FLOTTANTS, DOCS_ENTIERS_FLOTTANTS, "terms missing sur un double",
     "la valeur de remplissage se pose **au type du champ** : `missing: 0` sur "
     "un `double` doit rendre la cle `0.0`, pas `0`",
     {"size": 0, "aggs": {"a": {"terms": {"field": "f", "missing": 0}}}}, agg("a")),
    (ENTIERS_FLOTTANTS, DOCS_ENTIERS_FLOTTANTS, "range sur un double",
     "les bornes d'un `range` sur un flottant portent la meme regle",
     {"size": 0, "aggs": {"a": {"range": {"field": "f", "ranges": [
         {"to": 3}, {"from": 3}]}}}}, agg("a")),

    # -- copy_to -----------------------------------------------------------
    (COPIES, DOCS_COPIES, "terms sur la cible d'un copy_to",
     "la copie se fait a l'indexation, sur la valeur brute, et la cible la relit "
     "avec **son** type : un `long` copie dans un `keyword` s'y indexe en `\"7\"`",
     {"size": 0, "aggs": {"a": {"terms": {"field": "tout", "order": {"_key": "asc"}}}}},
     agg("a")),
    (COPIES, DOCS_COPIES, "terms missing sur une cible de copy_to",
     "le document sans aucune source n'a pas de valeur dans la cible : c'est "
     "lui qui doit tomber dans le bucket de remplissage",
     {"size": 0, "aggs": {"a": {"terms": {"field": "tout", "missing": "vide",
                                          "order": {"_key": "asc"}}}}}, agg("a")),
    (COPIES, DOCS_COPIES, "la copie ne se chaine pas",
     "la cible d'une cible ne recoit rien : `a -> b -> c` ne met rien de `a` "
     "dans `c`",
     {"size": 0, "aggs": {"a": {"terms": {"field": "c"}}}}, agg("a")),
    (COPIES, DOCS_COPIES, "la copie n'entre pas dans le _source",
     "elle est indexee, pas stockee : le document rendu est celui qu'on a ecrit",
     {"size": 10, "sort": [{"u": {"order": "asc"}}]},
     lambda r: [h.get("_source") for h in r.get("hits", {}).get("hits", [])]),

    # -- store -------------------------------------------------------------
    (STOCKES, DOCS_STOCKES, "stored_fields garde l'ordre du document",
     "un champ stocke n'est pas une colonne : il garde l'ordre d'ecriture et "
     "ses doublons, la ou `docvalue_fields` trie et dedoublonne",
     {"size": 10, "sort": [{"u": {"order": "asc"}}], "stored_fields": ["k", "n"]},
     lambda r: [h.get("fields") for h in r.get("hits", {}).get("hits", [])]),
    (STOCKES, DOCS_STOCKES, "un champ non stocke n'a pas de cle",
     "`stored_fields` ne reconstitue rien depuis le `_source`",
     {"size": 10, "sort": [{"u": {"order": "asc"}}], "stored_fields": ["libre"]},
     lambda r: [h.get("fields") for h in r.get("hits", {}).get("hits", [])]),
    (STOCKES, DOCS_STOCKES, "un float stocke garde sa forme courte",
     "Lucene le range sur 32 bits et le rend par le plus court texte qui s'y "
     "relit : `0.1` reste `0.1`, la ou sa colonne rend `0.10000000149011612`",
     {"size": 10, "sort": [{"u": {"order": "asc"}}], "stored_fields": ["f"]},
     lambda r: [h.get("fields") for h in r.get("hits", {}).get("hits", [])]),
    (STOCKES, DOCS_STOCKES, "fields l'emporte sur stored_fields",
     "le meme champ des deux cotes : c'est `fields` qui rend la valeur, donc "
     "le `format` qu'il demande. La valeur stockee ecrasait la valeur formatee, "
     "en 200 (trouve par une plage de controle du fuzzer, graine 5150180)",
     {"size": 10, "sort": [{"u": {"order": "asc"}}],
      "fields": [{"field": "d", "format": "epoch_millis"}], "stored_fields": ["*"]},
     lambda r: [h.get("fields") for h in r.get("hits", {}).get("hits", [])]),
    (STOCKES, DOCS_STOCKES, "rien de stocke sous un nested",
     "chez ES la valeur stockee vit dans le document enfant, invisible depuis "
     "la racine : la rendre serait rendre **plus** qu'ES, en silence",
     {"size": 10, "sort": [{"u": {"order": "asc"}}], "stored_fields": ["l.x"]},
     lambda r: [h.get("fields") for h in r.get("hits", {}).get("hits", [])]),

    # -- la limite des tokenizers -------------------------------------------
    # Anterieurs aux trois parametres, et sortis par eux : `copy_to` a fait
    # entrer un `keyword` de 300 caracteres dans un champ `text`, ce que rien
    # n'avait jamais fait.
    (LONGS, DOCS_LONGS, "un mot de plus de 255 caracteres est coupe, pas jete",
     "Lucene coupe a `maxTokenLength` en morceaux de 255 caracteres ; ferrite "
     "jetait le mot entier, donc le texte disparaissait de l'index en 200",
     {"size": 10, "sort": [{"u": {"order": "asc"}}], "_source": False,
      "query": {"prefix": {"t": {"value": "zzz"}}}}, hits),
    (LONGS, DOCS_LONGS, "les morceaux prennent des positions successives",
     "un mot coupe en deux consomme deux positions : tout ce qui suit se "
     "decale, et une phrase posee dessus ne trouve plus rien",
     {"size": 10, "sort": [{"u": {"order": "asc"}}], "_source": False,
      "query": {"match_phrase": {"t": "avant apres"}}}, hits),
    (LONGS, DOCS_LONGS, "un mot de 255 caracteres pile est garde",
     "la limite est inclusive chez Lucene ; ferrite jetait deja a 255",
     {"size": 10, "sort": [{"u": {"order": "asc"}}], "_source": False,
      "query": {"term": {"t": "z" * 255}}}, hits),
    (LONGS, DOCS_LONGS, "max_expansions est un budget par position",
     "`MultiPhrasePrefixQuery` remplit un seul ensemble pour toute la position ; "
     "un budget par terme developpait vingt fois plus de prefixes, et rendait "
     "plus de documents qu'ES",
     {"size": 10, "sort": [{"u": {"order": "asc"}}], "_source": False,
      "query": {"match_phrase_prefix": {"g": {"query": "reductio"}}}}, hits),

    # -- search_analyzer ---------------------------------------------------
    (RECHERCHE, DOCS_RECHERCHE, "search_analyzer : la requete n'est pas decoupee",
     "sans lui, la requete subit le meme decoupage que l'indexation et `elan` "
     "rend tout ce qui commence par `e`",
     {"size": 10, "sort": [{"u": {"order": "asc"}}], "_source": False,
      "query": {"match": {"t": "elan"}}}, hits),
    (RECHERCHE, DOCS_RECHERCHE, "sans search_analyzer, le meme champ ratisse large",
     "le revers, mesure identique chez ES : ce n'est pas un defaut, c'est ce "
     "que `search_analyzer` corrige",
     {"size": 10, "sort": [{"u": {"order": "asc"}}], "_source": False,
      "query": {"match": {"s": "elan"}}}, hits),
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
    (GRAMMES, DOCS_GRAMMES, "match_phrase de plusieurs mots sur un champ a n-grammes",
     "chaque position y porte des alternatives : Lucene construit une "
     "`MultiPhraseQuery`, que tantivy n'a pas. Enchainer les grammes rendrait "
     "moins de documents, en silence — un mot seul, lui, passe",
     {"query": {"match_phrase": {"g": "ecole normale"}}, "_source": False}),
    (GRAMMES, DOCS_GRAMMES, "match_phrase_prefix de plusieurs mots, meme champ",
     "meme cause, autre clause",
     {"query": {"match_phrase_prefix": {"g": "ecole norm"}}, "_source": False}),
    (TROUS, DOCS_TROUS, "range agg sur une date, avec un trou",
     "le bucket de remplissage de tantivy avale l'intervalle demande quand les "
     "bornes sont des dates : le bucket `2026-02-01-*` disparaissait",
     {"size": 0, "aggs": {"a": {"range": {"field": "dm", "ranges": [
         {"to": "2026-01-03"}, {"from": "2026-02-01"}]}}}}),
    # Un sous-champ de `nested` agrege depuis la racine : ES n'y voit aucun
    # document (ses valeurs vivent dans des documents caches) et rend le
    # resultat vide de l'agregation. ferrite les porte sur le document parent :
    # il rendait un autre nombre, en 200. Mesure : `avg` de 1.0 la ou ES rend
    # `null`, `sum` de 1.0 la ou ES rend 0.0.
    (NESTE, DOCS_NESTE, "avg sur un sous-champ de nested depuis la racine",
     "ferrite agregeait a plat et rendait un nombre plausible et faux ; ES rend "
     "`null`, parce qu'il ne voit aucun document au niveau racine",
     {"size": 0, "aggs": {"a": {"avg": {"field": "b.x"}}}}),
    (NESTE, DOCS_NESTE, "sum sur un sous-champ de nested depuis la racine",
     "meme cause, et c'est la que le zero d'ES se lit le plus mal : `0.0` la ou "
     "ferrite rendait la somme des sous-documents",
     {"size": 0, "aggs": {"a": {"sum": {"field": "b.x"}}}}),
    (NESTE, DOCS_NESTE, "terms sur un sous-champ de nested depuis la racine",
     "ES rend `buckets: []` ; ferrite listait les valeurs des sous-documents",
     {"size": 0, "aggs": {"a": {"terms": {"field": "b.y"}}}}),
    (NESTE, DOCS_NESTE, "avg sur un sous-champ de nested, sous un terms",
     "le refus vaut a tous les niveaux : une sous-agregation n'y echappe pas, "
     "et c'est la que le nombre faux se voyait le moins (ES rend `null` dans "
     "chaque bucket)",
     {"size": 0, "aggs": {"t": {"terms": {"field": "k"},
                                "aggs": {"a": {"avg": {"field": "b.x"}}}}}}),
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
    # Un cas peut porter ses propres `settings` — un analyzer declare, par
    # exemple : sans ca, aucun ecart lie a l'analyse ne serait exprimable ici.
    http(base, "PUT", f"/{INDEX}",
         dict(mapping, settings={"number_of_shards": 1, "number_of_replicas": 0,
                                 **mapping.get("settings", {})}))
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
