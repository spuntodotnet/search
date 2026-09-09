#!/usr/bin/env python3
"""Sonde : `collapse` et `post_filter`, la mecanique d'une page de resultats.

Les deux repondent a un besoin qu'aucune autre clause ne couvre, et les deux se
definissent par leur **place dans la chaine** plutot que par ce qu'ils
calculent :

* `post_filter` filtre les hits **apres** les agregations. C'est la page a
  facettes : choisir « rouge » ne fait pas disparaitre les autres couleurs du
  panneau de gauche. Ce qui se mesure n'est donc pas « filtre-t-il » mais ce
  qu'il **ne touche pas** — les agregations, le score, l'arbre d'`explain`, le
  surlignage — et ce qu'il touche quand meme : `hits.total`, la pagination,
  `matched_queries`, et les `inner_hits` d'un `collapse` ;
* `collapse` ne regroupe pas, il **choisit un representant**. Son piege est
  dans la reponse : `hits.total` reste celui d'avant repliement, `from` / `size`
  paginent les groupes, les documents sans valeur font **un** groupe, et un
  champ multivalue fait tomber la recherche entiere.

Ce qui est compare est le hit **entier** — l'ordre des documents, le score, le
bloc `fields` (ou le repliement pose sa valeur), le tableau `sort`, le
surlignage, et chaque bloc `inner_hits` avec son total, son `max_score` et ses
documents. Un repliement qui rend les bons documents avec le mauvais `total`
serait vert sur la liste seule.

    python3 tests/compat/sonde_repli.py [ferrite] [es]
    python3 tests/compat/sonde_repli.py --calibrer [es_a] [es_b]

Elle **refuse de tourner** si elle ne trouve pas ses deux cibles : une sonde
differentielle qui ne compare rien ne doit pas rendre de verdict.
"""
import json
import sys
import urllib.error
import urllib.request

INDEX = "sonde-repli"
INDEX_B = "sonde-repli-b"
INDEX_MULTI = "sonde-repli-multi"
INDEX_DOUBLON_KW = "sonde-repli-doublon-kw"
INDEX_DOUBLON_NUM = "sonde-repli-doublon-num"
INDEX_NESTED = "sonde-repli-nested"


def http(base, method, path, body=None, ndjson=False):
    if ndjson:
        data, ctype = body.encode(), "application/x-ndjson"
    else:
        data = json.dumps(body).encode() if body is not None else None
        ctype = "application/json"
    req = urllib.request.Request(
        base + path, data=data, method=method, headers={"Content-Type": ctype})
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        corps = e.read()
        try:
            return e.code, json.loads(corps or b"{}")
        except json.JSONDecodeError:
            return e.code, {"raw": corps.decode(errors="replace")}


# Le corpus est bati pour que les scores soient **exacts des deux cotes** : les
# requetes de la batterie partent de `match_all`, de `term` sur un `keyword` ou
# d'un `constant_score`, jamais d'un BM25 sur du texte. C'est ce qui permet de
# comparer le `_score` au bit pres au lieu de choisir une tolerance — les rares
# cas qui ont besoin d'un vrai `match` sont marques `[bm25]` et compares a la
# precision ou les deux moteurs s'accordent (voir `arrondi`).
#
# Trois formes cohabitent exprès : deux documents sans `marque` (ils doivent
# faire **un** groupe, pas deux), plusieurs documents par marque (pour que le
# representant se voie), et une couleur qui se repete dans un groupe (pour le
# second niveau de repliement).
DOCS = {
    "d1": {"marque": "acme", "couleur": "rouge", "prix": 10, "poids": 0.5,
           "ref": "acme", "nom": "chaise rouge"},
    "d2": {"marque": "acme", "couleur": "rouge", "prix": 20, "poids": 1.5,
           "ref": "acme", "nom": "chaise bleue"},
    "d3": {"marque": "acme", "couleur": "bleu", "prix": 30, "poids": 0.5,
           "ref": "acme", "nom": "table rouge"},
    "d4": {"marque": "bolt", "couleur": "bleu", "prix": 40, "poids": 2.5,
           "ref": "bolt", "nom": "chaise bolt"},
    "d5": {"marque": "bolt", "couleur": "vert", "prix": 50, "poids": 2.5,
           "ref": "bolt", "nom": "table bolt"},
    # Sans `marque` : les deux suivants doivent tomber dans le meme groupe.
    "d6": {"couleur": "vert", "prix": 60, "poids": 3.5, "nom": "objet sans marque"},
    "d7": {"couleur": "rouge", "prix": 70, "nom": "autre objet sans marque"},
}

MAPPING = {"mappings": {"properties": {
    "marque": {"type": "keyword"},
    "couleur": {"type": "keyword"},
    "prix": {"type": "long"},
    "poids": {"type": "double"},
    # Un champ garde sans etre cherchable : il a quand meme une colonne, donc
    # ES sait replier dessus (mesure).
    "ref": {"type": "keyword", "index": False},
    "nom": {"type": "text", "fields": {"kw": {"type": "keyword"}}},
    "quand": {"type": "date"},
    "actif": {"type": "boolean"},
    "tags": {"type": "keyword"},
}}}

# Le champ multivalue vit dans son propre index : chez ES il fait tomber la
# recherche entiere, donc un seul document suffit a rendre le reste du corpus
# inutilisable.
DOCS_MULTI = {
    "m1": {"marque": "acme", "prix": 1},
    "m2": {"marque": ["acme", "bolt"], "prix": 2},
}

# Le meme piege, une case plus loin, et il se retourne : un `keyword` qui porte
# **deux fois la meme valeur** est mono-value pour ES (ses
# `SortedSetDocValues` dedoublonnent) alors qu'un numerique ne l'est pas (ses
# `SortedNumericDocValues` gardent les doublons). Deux index, parce que le
# premier document fautif condamne tout l'index.
DOCS_DOUBLON_KW = {
    "k1": {"marque": "acme", "prix": 1},
    "k2": {"marque": ["bolt", "bolt"], "prix": 2},
}
DOCS_DOUBLON_NUM = {
    "j1": {"marque": "acme", "prix": 1},
    "j2": {"marque": "bolt", "prix": [2, 2]},
}

DOCS_B = {
    "b1": {"marque": "acme", "couleur": "noir", "prix": 5},
    "b2": {"marque": "zeta", "couleur": "noir", "prix": 15},
}


def bulk(base, index, docs):
    lignes = []
    for id_, doc in docs.items():
        lignes.append(json.dumps({"index": {"_index": index, "_id": id_}}))
        lignes.append(json.dumps(doc))
    http(base, "POST", "/_bulk?refresh=true", "\n".join(lignes) + "\n", ndjson=True)


def prepare(base):
    for idx in (INDEX, INDEX_B, INDEX_MULTI, INDEX_NESTED,
                INDEX_DOUBLON_KW, INDEX_DOUBLON_NUM):
        http(base, "DELETE", f"/{idx}")
    http(base, "PUT", f"/{INDEX}", MAPPING)
    bulk(base, INDEX, DOCS)
    http(base, "PUT", f"/{INDEX_B}", MAPPING)
    bulk(base, INDEX_B, DOCS_B)
    http(base, "PUT", f"/{INDEX_MULTI}", MAPPING)
    bulk(base, INDEX_MULTI, DOCS_MULTI)
    http(base, "PUT", f"/{INDEX_DOUBLON_KW}", MAPPING)
    bulk(base, INDEX_DOUBLON_KW, DOCS_DOUBLON_KW)
    http(base, "PUT", f"/{INDEX_DOUBLON_NUM}", MAPPING)
    bulk(base, INDEX_DOUBLON_NUM, DOCS_DOUBLON_NUM)
    http(base, "PUT", f"/{INDEX_NESTED}", {"mappings": {"properties": {
        "lignes": {"type": "nested", "properties": {"ref": {"type": "keyword"}}}}}})
    bulk(base, INDEX_NESTED, {"n1": {"lignes": [{"ref": "x"}, {"ref": "y"}]}})


# ---------------------------------------------------------------------------
# La batterie
# ---------------------------------------------------------------------------

TOUT = {"match_all": {}}
# Un score qui ne depend pas du BM25 : les deux moteurs le rendent au bit pres.
def cst(clause, boost):
    return {"constant_score": {"filter": clause, "boost": boost}}


AGG = {"c": {"terms": {"field": "couleur"}}}


def cas_post_filter():
    """`post_filter` : ce qu'il touche, et surtout ce qu'il ne touche pas."""
    out = []
    a = lambda lib, corps, index=INDEX: out.append((lib, corps, index))  # noqa: E731

    a("pf seul", {"post_filter": {"term": {"couleur": "rouge"}}})
    a("[bm25] pf + query", {"query": {"term": {"couleur": "rouge"}},
                     "post_filter": {"range": {"prix": {"gte": 20}}}})
    # Le coeur du parametre : les seaux ne bougent pas.
    a("pf + aggs", {"query": TOUT, "post_filter": {"term": {"couleur": "rouge"}},
                    "aggs": AGG})
    a("sans pf + aggs (temoin)", {"query": TOUT, "aggs": AGG})
    a("pf + aggs + size 0", {"query": TOUT, "size": 0, "aggs": AGG,
                             "post_filter": {"term": {"couleur": "rouge"}}})
    # `hits.total` **bouge**, et c'est ce qui le separe d'un filtrage client.
    a("pf + from/size", {"query": TOUT, "post_filter": {"term": {"couleur": "rouge"}},
                         "from": 1, "size": 1})
    a("pf + from au-dela", {"query": TOUT, "post_filter": {"term": {"couleur": "rouge"}},
                            "from": 9})
    a("pf + sort", {"query": TOUT, "post_filter": {"term": {"couleur": "bleu"}},
                    "sort": [{"prix": "desc"}]})
    # Il ne **note** rien : un boost pose dedans ne deplace pas le `_score`.
    a("pf ne note pas (boost 10)", {"query": TOUT,
                                    "post_filter": {"term": {"couleur": {"value": "rouge",
                                                                         "boost": 10}}}})
    a("pf sous constant_score", {"query": cst({"term": {"couleur": "rouge"}}, 3.5),
                                 "post_filter": {"range": {"prix": {"gte": 20}}}})
    # Il filtre **apres** le seuil, et le seuil compare le score de la requete.
    a("pf + min_score", {"query": cst({"exists": {"field": "marque"}}, 2.0),
                         "min_score": 1.5,
                         "post_filter": {"term": {"couleur": "bleu"}}})
    a("pf + min_score qui coupe tout", {"query": cst(TOUT, 0.5), "min_score": 1.0,
                                        "post_filter": {"term": {"couleur": "bleu"}}})
    # Ses clauses nommees sortent dans `matched_queries`, dans la meme liste.
    a("pf nomme", {"query": TOUT,
                   "post_filter": {"term": {"couleur": {"value": "rouge", "_name": "pf"}}}})
    a("[bm25] pf nomme + query nommee",
      {"query": {"term": {"couleur": {"value": "rouge", "_name": "q"}}},
       "post_filter": {"range": {"prix": {"gte": 20, "_name": "pf"}}}})
    # Mais il n'entre ni dans l'arbre d'`explain` ni dans le surlignage.
    a("pf + explain", {"query": cst({"term": {"couleur": "rouge"}}, 2.0),
                       "post_filter": {"range": {"prix": {"gte": 20}}}, "explain": True})
    a("[bm25] pf + highlight", {"query": {"match": {"nom": "chaise"}},
                                "post_filter": {"match": {"nom": "bolt"}},
                                "highlight": {"fields": {"nom": {}}}})
    # Formes booleennes, et les bords de lecture.
    a("pf must_not", {"query": TOUT,
                      "post_filter": {"bool": {"must_not": {"term": {"couleur": "rouge"}}}}})
    a("pf exists", {"query": TOUT, "post_filter": {"exists": {"field": "marque"}}})
    a("pf champ inconnu", {"query": TOUT, "post_filter": {"term": {"nawak": "x"}}})
    a("pf clause inconnue", {"query": TOUT, "post_filter": {"pas_une_clause": {"a": 1}}})
    a("pf null", {"query": TOUT, "post_filter": None})
    a("pf {}", {"query": TOUT, "post_filter": {}})
    a("pf liste", {"query": TOUT, "post_filter": [{"term": {"couleur": "rouge"}}]})
    a("pf multi-index", {"query": TOUT, "post_filter": {"term": {"couleur": "noir"}},
                         "aggs": AGG}, f"{INDEX},{INDEX_B}")
    a("pf + docvalue_fields", {"query": TOUT, "docvalue_fields": ["prix"],
                               "post_filter": {"term": {"couleur": "vert"}}})
    a("pf + _source false", {"query": TOUT, "_source": False,
                             "post_filter": {"term": {"couleur": "vert"}}})
    return out


def cas_collapse():
    """`collapse` : le representant, le total, les groupes, et les refus."""
    out = []
    a = lambda lib, corps, index=INDEX: out.append((lib, corps, index))  # noqa: E731

    # La forme de base, et les deux documents sans valeur qui font **un** groupe.
    a("collapse keyword", {"query": TOUT, "collapse": {"field": "marque"}})
    a("collapse keyword size 10", {"query": TOUT, "collapse": {"field": "marque"}, "size": 10})
    a("collapse long", {"query": TOUT, "collapse": {"field": "prix"}, "size": 10})
    a("collapse double", {"query": TOUT, "collapse": {"field": "poids"}, "size": 10})
    a("collapse index:false", {"query": TOUT, "collapse": {"field": "ref"}, "size": 10})
    # Replier sur un champ non indexe marche ; en ramener les membres, non.
    a("collapse index:false + inner_hits",
      {"query": TOUT, "collapse": {"field": "ref", "inner_hits": {"name": "g"}}})
    a("collapse + inner niveau 2 index:false",
      {"query": TOUT, "collapse": {"field": "marque",
                                   "inner_hits": {"name": "g",
                                                  "collapse": {"field": "ref"}}}})
    a("collapse multi-field", {"query": TOUT, "collapse": {"field": "nom.kw"}, "size": 10})
    # `total` reste celui d'avant repliement, et la pagination porte sur les
    # groupes.
    a("collapse from/size", {"query": TOUT, "collapse": {"field": "marque"},
                             "from": 1, "size": 1})
    a("collapse from au-dela", {"query": TOUT, "collapse": {"field": "marque"}, "from": 9})
    a("collapse size 0", {"query": TOUT, "collapse": {"field": "marque"}, "size": 0})
    # Sous un tri, le representant change : c'est le premier **du tri**.
    a("collapse + sort asc", {"query": TOUT, "collapse": {"field": "marque"},
                              "sort": [{"prix": "asc"}]})
    a("collapse + sort desc", {"query": TOUT, "collapse": {"field": "marque"},
                               "sort": [{"prix": "desc"}]})
    a("collapse + sort deux cles", {"query": TOUT, "collapse": {"field": "couleur"},
                                    "sort": [{"poids": "desc"}, {"prix": "asc"}]})
    a("collapse + sort _score", {"query": cst({"range": {"prix": {"gte": 30}}}, 4.0),
                                 "collapse": {"field": "marque"},
                                 "sort": [{"_score": "desc"}]})
    # Les agregations ne voient pas le repliement (mesure).
    a("collapse + aggs", {"query": TOUT, "collapse": {"field": "marque"}, "aggs": AGG})
    a("collapse + post_filter", {"query": TOUT, "collapse": {"field": "marque"},
                                 "post_filter": {"range": {"prix": {"gte": 30}}},
                                 "aggs": AGG})
    a("collapse + min_score", {"query": cst({"exists": {"field": "marque"}}, 2.0),
                               "min_score": 1.5, "collapse": {"field": "couleur"}})
    a("[bm25] collapse + highlight", {"query": {"match": {"nom": "chaise"}},
                                      "collapse": {"field": "marque"},
                                      "highlight": {"fields": {"nom": {}}}})
    # Le bloc `fields` du repliement se pose **a cote** de ce que `fields` et
    # `docvalue_fields` ont deja rendu.
    a("collapse + docvalue_fields", {"query": TOUT, "collapse": {"field": "marque"},
                                     "docvalue_fields": ["prix"]})
    a("collapse + fields", {"query": TOUT, "collapse": {"field": "marque"},
                            "fields": ["couleur"]})
    a("collapse + _source false", {"query": TOUT, "collapse": {"field": "marque"},
                                   "_source": False})
    a("collapse multi-index", {"query": TOUT, "collapse": {"field": "marque"}, "size": 10},
      f"{INDEX},{INDEX_B}")
    a("[bm25] collapse + query filtrante", {"query": {"term": {"couleur": "rouge"}},
                                     "collapse": {"field": "marque"}})

    # Les refus, chacun avec la phrase d'ES.
    a("collapse sur text", {"query": TOUT, "collapse": {"field": "nom"}})
    a("collapse sur date", {"query": TOUT, "collapse": {"field": "quand"}})
    a("collapse sur boolean", {"query": TOUT, "collapse": {"field": "actif"}})
    a("collapse champ inconnu", {"query": TOUT, "collapse": {"field": "nawak"}})
    a("collapse multivalue", {"query": TOUT, "collapse": {"field": "marque"}}, INDEX_MULTI)
    a("collapse sous nested", {"query": TOUT, "collapse": {"field": "lignes.ref"}},
      INDEX_NESTED)
    # Un `keyword` a valeurs repetees est mono-value pour ES, un numerique non.
    a("collapse keyword doublon", {"query": TOUT, "collapse": {"field": "marque"}},
      INDEX_DOUBLON_KW)
    a("collapse numerique doublon", {"query": TOUT, "collapse": {"field": "prix"}},
      INDEX_DOUBLON_NUM)
    a("collapse sans field", {"query": TOUT, "collapse": {}})
    a("collapse chaine", {"query": TOUT, "collapse": "marque"})
    a("collapse nombre", {"query": TOUT, "collapse": {"field": 42}})
    a("collapse null", {"query": TOUT, "collapse": None})
    a("collapse cle inconnue", {"query": TOUT, "collapse": {"field": "marque", "nawak": 1}})
    a("collapse mcgs", {"query": TOUT, "collapse": {"field": "marque",
                                                    "max_concurrent_group_searches": 4}})
    return out


def cas_inner_hits():
    """`inner_hits` : les documents replies d'un groupe."""
    out = []
    a = lambda lib, corps, index=INDEX: out.append((lib, corps, index))  # noqa: E731
    col = lambda ih: {"query": TOUT, "collapse": {"field": "marque", "inner_hits": ih}}  # noqa: E731

    a("inner defaut (size 3)", col({"name": "g"}))
    a("inner size 10", col({"name": "g", "size": 10}))
    a("inner size 1", col({"name": "g", "size": 1}))
    a("inner size 0", col({"name": "g", "size": 0}))
    a("inner from 1", col({"name": "g", "from": 1, "size": 10}))
    a("inner from au-dela", col({"name": "g", "from": 100}))
    a("inner sort prix desc", col({"name": "g", "size": 10, "sort": [{"prix": "desc"}]}))
    a("inner sort deux cles", col({"name": "g", "size": 10,
                                   "sort": [{"poids": "asc"}, {"prix": "desc"}]}))
    a("inner sort _score", col({"name": "g", "size": 10, "sort": [{"_score": "desc"}]}))
    a("inner sort missing", col({"name": "g", "size": 10,
                                 "sort": [{"poids": {"order": "asc", "missing": "_first"}}]}))
    a("inner _source false", col({"name": "g", "_source": False}))
    a("inner _source includes", col({"name": "g", "_source": ["prix"]}))
    a("inner deux blocs", col([{"name": "g1", "size": 1},
                               {"name": "g2", "size": 10, "sort": [{"prix": "desc"}]}]))
    a("inner niveau 2", col({"name": "g", "size": 10, "collapse": {"field": "couleur"}}))
    a("inner niveau 2 + sort", col({"name": "g", "size": 10, "sort": [{"prix": "desc"}],
                                    "collapse": {"field": "couleur"}}))
    a("inner + post_filter",
      {"query": TOUT, "post_filter": {"range": {"prix": {"gte": 30}}},
       "collapse": {"field": "marque", "inner_hits": {"name": "g", "size": 10}}})
    a("[bm25] inner + query filtrante",
      {"query": {"term": {"couleur": "rouge"}},
       "collapse": {"field": "marque", "inner_hits": {"name": "g", "size": 10}}})
    a("inner + root sort", {"query": TOUT, "sort": [{"prix": "desc"}],
                            "collapse": {"field": "marque",
                                         "inner_hits": {"name": "g", "size": 10}}})
    # Les refus.
    a("inner sans name", col({"size": 2}))
    a("inner {}", col({}))
    a("inner cle inconnue", col({"name": "g", "nawak": 1}))
    a("inner size negatif", col({"name": "g", "size": -1}))
    a("inner niveau 3", col({"name": "g", "collapse": {"field": "couleur",
                                                       "inner_hits": {"name": "h"}}}))
    a("inner niveau 2 champ inconnu", col({"name": "g", "collapse": {"field": "nawak"}}))
    a("inner niveau 2 sur text", col({"name": "g", "collapse": {"field": "nom"}}))
    a("inner highlight", col({"name": "g", "highlight": {"fields": {"nom": {}}}}))
    a("inner docvalue_fields", col({"name": "g", "docvalue_fields": ["prix"]}))
    a("inner explain", col({"name": "g", "explain": True}))
    a("inner version", col({"name": "g", "version": True}))
    return out


def cas_routes():
    """Les routes qui lisent une requete et qui ne prennent ni l'un ni
    l'autre — un refus qui n'existe pas laisse croire que le parametre a ete
    applique."""
    return [
        ("_count + post_filter", "POST", f"/{INDEX}/_count",
         {"query": TOUT, "post_filter": {"term": {"couleur": "rouge"}}}),
        ("_count + collapse", "POST", f"/{INDEX}/_count",
         {"query": TOUT, "collapse": {"field": "marque"}}),
        ("scroll + collapse", "POST", f"/{INDEX}/_search?scroll=1m",
         {"query": TOUT, "collapse": {"field": "marque"}}),
        ("_delete_by_query + post_filter", "POST", f"/{INDEX}/_delete_by_query?dry_run",
         {"query": {"term": {"couleur": "jamais"}},
          "post_filter": {"term": {"couleur": "rouge"}}}),
    ]


# ---------------------------------------------------------------------------
# Comparaison
# ---------------------------------------------------------------------------

def arrondi(x, ref):
    """Un score, ou sa comparaison a une reference.

    Le corpus est bati pour que le score soit exact des deux cotes
    (`match_all`, `constant_score`) : l'egalite y est exigee **au bit pres**.
    Quelques cas ont quand meme besoin d'un vrai BM25 — un `term` sur un
    `keyword`, un `match` sur du texte — et la, tantivy et Lucene ne rendent
    pas le meme dernier bit : c'est une divergence declaree (`docs/compat.md` :
    `N` et `avgdl` se comptent sur les documents *qui ont le champ* chez
    Lucene, sur tous chez tantivy), mesuree ici a un ULP de `float`
    (`0.8266785` contre `0.8266786`).

    Tolerer un ecart de cette taille serait une tolerance **choisie**, et elle
    couvrirait aussi bien un `post_filter` qui noterait un peu. Ce qui est
    compare est donc autre chose, et c'est exactement ce que ces cas-la doivent
    prouver : le score sous `post_filter` (ou sous `collapse`) est celui que
    **le meme serveur** rend a la meme requete sans eux. La divergence BM25
    s'annule des deux cotes, et il ne reste plus rien a tolerer."""
    if x is None or ref is None:
        return x
    return "=ref" if x == ref else ["!=ref", x, ref]


def hit(h, ref):
    """Ce qui se compare dans un hit : tout ce qu'un client lit."""
    out = {
        "i": h["_index"], "id": h["_id"], "sc": arrondi(h.get("_score"), ref.get(h["_id"])),
        "f": h.get("fields"), "s": h.get("sort"), "hl": h.get("highlight"),
        "src": h.get("_source"), "mq": h.get("matched_queries"),
    }
    # L'arbre d'explication ne se compare pas nœud par nœud ici — c'est l'objet
    # de `sonde_explain.py`. Ce qui compte pour cette sonde est qu'un
    # `post_filter` n'y ajoute **rien** : sa valeur racine doit rester celle de
    # la requete seule.
    if "_explanation" in h:
        out["ex"] = arrondi(h["_explanation"]["value"], ref.get(h["_id"]))
    if "inner_hits" in h:
        out["ih"] = {
            nom: {"t": b["hits"]["total"],
                  "ms": arrondi(b["hits"]["max_score"], ref.get("__max__")),
                  "h": [{"i": y["_index"], "id": y["_id"],
                         "sc": arrondi(y.get("_score"), ref.get(y["_id"])),
                         "f": y.get("fields"), "s": y.get("sort"),
                         "src": y.get("_source")}
                        for y in b["hits"]["hits"]]}
            for nom, b in h["inner_hits"].items()}
    return out


def reference(base, corps, index):
    """Les scores de la **meme** requete, privee de tout ce que cette sonde
    mesure : `post_filter`, `collapse`, `min_score`, la pagination.

    C'est l'etalon des cas `[bm25]` : ce qui est compare n'est plus une valeur
    (que les deux moteurs n'ecrivent pas pareil) mais le fait qu'elle **n'ait
    pas bouge**."""
    nu = {c: v for c, v in corps.items()
          if c not in ("post_filter", "collapse", "min_score", "from", "size")}
    nu["size"] = 100
    st, body = http(base, "POST", f"/{index}/_search", nu)
    if st != 200:
        return {}
    ref = {h["_id"]: h["_score"] for h in body["hits"]["hits"]}
    ref["__max__"] = body["hits"]["max_score"]
    return ref


def interroge(base, corps, index, bm25):
    ref = reference(base, corps, index) if bm25 else {}
    st, body = http(base, "POST", f"/{index}/_search", corps)
    if st != 200:
        return json.dumps([st, type_erreur(body), phrase(body)])
    h = body["hits"]
    return json.dumps([
        h["total"], arrondi(h["max_score"], ref.get("__max__")),
        [hit(x, ref) for x in h["hits"]],
        body.get("aggregations"),
        sorted((f["index"], f["reason"]["type"]) for f in body["_shards"].get("failures", [])),
    ], sort_keys=True)


def type_erreur(body):
    e = body.get("error", {})
    if not isinstance(e, dict):
        return "?"
    rc = e.get("root_cause") or []
    return (rc[0].get("type") if rc else None) or e.get("type", "?")


def phrase(body):
    """La phrase du refus : celle du `root_cause`, la seule qu'une exception de
    client officiel remonte."""
    e = body.get("error", {})
    if not isinstance(e, dict):
        return "?"
    rc = e.get("root_cause") or []
    return (rc[0].get("reason") if rc else None) or e.get("reason", "?")


def interroge_route(base, methode, chemin, corps):
    st, body = http(base, methode, chemin, corps)
    if st != 200:
        return json.dumps([st, type_erreur(body)])
    # Une route qui **accepte** est le pire resultat : elle a ignore le
    # parametre en silence. On garde de quoi le voir.
    return json.dumps([st, sorted(body.keys())])


# Les ecarts assumes, chacun avec sa raison ecrite et le predicat qu'il doit
# passer. Trois classes :
#
# - `phrase` : les deux serveurs refusent avec le meme statut, seule la phrase
#   ou le type differe (ES prefixe ses erreurs de parsing d'un `[ligne:colonne]`
#   que ferrite n'a plus une fois le corps parse) ;
# - `perimetre` : ferrite refuse la ou ES repond. Le predicat verifie que
#   ferrite refuse **explicitement**, jamais qu'il rend un resultat en silence ;
# - `doc_interne` : le refus est le meme, mais il nomme un numero de document
#   **interne** — celui de Lucene chez ES, celui de tantivy ici.
REFUS_ASSUMES = {
    "pf {}": ("phrase",
        "ES rend `query malformed, empty clause found at [1:46]` — la position "
        "designe un offset dans le JSON brut, que ferrite n'a plus une fois le "
        "corps parse. Le statut et le refus sont les memes"),
    "collapse sans field": ("phrase",
        "ES tombe sur une `NullPointerException` en 500 (« Cannot invoke "
        "\"Object.hashCode()\" because \"pk\" is null ») : un repliement sans "
        "champ n'a pas de cle. ferrite refuse explicitement en 400 — "
        "reproduire un plantage de la reference n'aurait aucun interet"),
    "collapse cle inconnue": ("phrase",
        "meme refus, meme type ; ES prefixe sa phrase de `[1:62]`"),
    "collapse nombre": ("phrase",
        "meme refus, meme type, meme phrase ; ES la prefixe de `[1:52]`"),
    "pf clause inconnue": ("phrase",
        "meme refus, meme type, meme debut de phrase ; ferrite ajoute la liste "
        "des clauses qu'il sert — c'est ce qu'il fait deja sur `query`"),
    "inner niveau 2 sur text": ("phrase",
        "meme refus et meme phrase de fond ; ES l'enveloppe dans `failed to "
        "expand hits`, la phase ou il developpe les groupes, alors que ferrite "
        "refuse le champ des la resolution du plan"),
    "inner sans name": ("phrase",
        "meme refus, meme type, meme phrase ; ES la prefixe de `[1:81]` et la "
        "range sous un `caused_by`"),
    "inner {}": ("phrase", "meme chose, sans le `name`"),
    "inner cle inconnue": ("phrase", "meme refus ; ES prefixe de `[1:85]`"),
    "inner size negatif": ("phrase", "meme refus ; ES prefixe de `[1:93]`"),
    "inner niveau 3": ("phrase",
        "meme refus et meme phrase (`Invalid token in the inner collapse`) ; ES "
        "l'enveloppe dans un `x_content_parse_exception` prefixe"),
    "inner niveau 2 champ inconnu": ("phrase",
        "ES enveloppe l'echec dans `failed to expand hits` (la phase de fetch), "
        "ferrite le rend comme un echec de shard de la phase de requete — le "
        "champ est refuse des la resolution du plan"),
    "collapse multivalue": ("doc_interne",
        "meme type, meme statut, meme phrase a un nombre pres : elle nomme le "
        "document par son numero **interne**, celui de Lucene chez ES et celui "
        "de tantivy ici. Les deux n'ont aucune raison de coincider"),
    "collapse sous nested": ("perimetre",
        "ES rend 200 en rangeant **tous** les documents racine dans le groupe "
        "sans valeur (ses elements sont des documents a part, la racine n'a "
        "donc pas de colonne). ferrite, lui, a bien une colonne a cet endroit "
        "et replierait sur les valeurs des elements : il refuse en le nommant, "
        "comme il le fait deja pour le tri et les agregations"),
    "inner highlight": ("perimetre",
        "ES surligne dans un `inner_hits` ; ferrite refuse le parametre en le "
        "nommant plutot que de rendre un bloc ampute"),
    "inner docvalue_fields": ("perimetre", "meme chose pour `docvalue_fields`"),
    "inner explain": ("perimetre", "meme chose pour `explain`"),
    "inner version": ("perimetre", "meme chose pour `version`"),
    "_count + post_filter": ("phrase",
        "les deux refusent ; ES dit `request does not support [post_filter]`, "
        "ferrite liste les cles que `_count` accepte"),
    "_count + collapse": ("phrase", "meme chose pour `collapse`"),
    "_delete_by_query + post_filter": ("phrase",
        "les deux refusent le parametre ; les phrases different"),
}


def refuse(vu):
    """La reponse porte-t-elle un refus explicite ? Un statut non-200, ou un
    200 dont un index a echoue en le disant."""
    val = json.loads(vu)
    if isinstance(val[0], int) and val[0] != 200:
        return True
    return bool(val[-1]) if isinstance(val[-1], list) else False


def meme_statut(reps):
    return len({json.loads(vu)[0] if isinstance(json.loads(vu)[0], int) else 200
                for _, vu in reps}) == 1


def assume(libelle, reps):
    classe = REFUS_ASSUMES.get(libelle, (None, None))[0]
    if classe == "phrase":
        # Le statut n'est pas exige identique ici : un des cas assumes est
        # precisement un 500 d'ES contre un 400 de ferrite. Ce qui est exige,
        # c'est que **les deux** refusent.
        return all(refuse(vu) for _, vu in reps)
    if classe == "doc_interne":
        # Meme statut, meme type, et la phrase identique une fois le numero
        # retire : c'est ce qui empeche la classe d'absorber autre chose.
        formes = {sans_le_numero(vu) for _, vu in reps}
        return len(formes) == 1
    if classe == "perimetre":
        return refuse(reps[0][1])
    return False


def sans_le_numero(vu):
    val = json.loads(vu)
    if len(val) == 3 and isinstance(val[2], str):
        val[2] = "".join("N" if c.isdigit() else c for c in val[2])
    return json.dumps(val)


def abrege(vu):
    return vu if len(vu) <= 190 else vu[:187] + "..."


def main():
    argv = [a for a in sys.argv[1:] if a != "--calibrer"]
    calibrer = "--calibrer" in sys.argv
    gauche = argv[0] if argv else ("http://localhost:9201" if calibrer
                                   else "http://localhost:9200")
    droite = argv[1] if len(argv) > 1 else ("http://localhost:9202" if calibrer
                                            else "http://localhost:9201")
    cibles = [("es_a" if calibrer else "ferrite", gauche),
              ("es_b" if calibrer else "es", droite)]
    for nom, base in cibles:
        try:
            st, info = http(base, "GET", "/")
            print(f"# {nom} : {base} — {info.get('version', {}).get('number', '?')}")
        except Exception as e:  # noqa: BLE001
            print(f"# {nom} indisponible ({base}) : {e}")
            print("# une sonde differentielle qui ne compare rien ne rend pas "
                  "de verdict : arret.")
            return 2

    ecarts = assumes = total = 0
    for _, base in cibles:
        prepare(base)

    batterie = cas_post_filter() + cas_collapse() + cas_inner_hits()
    for libelle, corps, index in batterie:
        bm25 = libelle.startswith("[bm25]")
        reps = [(nom, interroge(base, corps, index, bm25)) for nom, base in cibles]
        total += 1
        if len({vu for _, vu in reps}) <= 1:
            print(f"  {libelle:34} {abrege(reps[0][1])}")
            continue
        if assume(libelle, reps):
            assumes += 1
            print(f"~ {libelle:34} " +
                  "\n      ".join(f"{nom}={abrege(vu)}" for nom, vu in reps))
            print(f"      assume ({REFUS_ASSUMES[libelle][0]}) : "
                  f"{REFUS_ASSUMES[libelle][1]}")
        else:
            ecarts += 1
            print(f"* {libelle:34} " +
                  "\n      ".join(f"{nom}={abrege(vu)}" for nom, vu in reps))

    for libelle, methode, chemin, corps in cas_routes():
        reps = [(nom, interroge_route(base, methode, chemin, corps)) for nom, base in cibles]
        total += 1
        if len({vu for _, vu in reps}) <= 1:
            print(f"  {libelle:34} {abrege(reps[0][1])}")
            continue
        if assume(libelle, reps):
            assumes += 1
            print(f"~ {libelle:34} " +
                  "  |  ".join(f"{nom}={abrege(vu)}" for nom, vu in reps))
            print(f"      assume ({REFUS_ASSUMES[libelle][0]}) : "
                  f"{REFUS_ASSUMES[libelle][1]}")
        else:
            ecarts += 1
            print(f"* {libelle:34} " +
                  "  |  ".join(f"{nom}={abrege(vu)}" for nom, vu in reps))

    print(f"\n{total - ecarts - assumes}/{total} identiques, "
          f"{assumes} refus assumes, {ecarts} ecarts")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
