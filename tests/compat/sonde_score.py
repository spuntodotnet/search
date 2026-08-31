#!/usr/bin/env python3
"""Sonde : `function_score` et `boosting` rendent-ils **le meme score** qu'ES ?

    python3 tests/compat/sonde_score.py [ferrite] [es]
    python3 tests/compat/sonde_score.py --calibrer [es_a] [es_b]

Les autres sondes de ce depot comparent un ensemble de documents et leur ordre.
Celle-ci compare une **valeur** : `function_score` existe pour produire un
`_score`, et c'est ce nombre-la que le client lit, affiche et compare. Un ordre
juste avec des scores faux serait vert partout ailleurs.

Ce qui est compare, pour chaque question :

  - le `_score` de chaque hit, **au bit pres** (voir `TOLERANCE` plus bas) ;
  - l'ordre des documents, separement — un ecart de score qui ne change pas
    l'ordre reste un ecart, et un ecart d'ordre qui ne change pas les scores
    aussi ;
  - `max_score`, le total, et le statut / type / message d'une erreur.

`--calibrer` rejoue la meme batterie contre **deux** Elasticsearch : elle ecrit
(elle cree ses index), donc elle ne peut pas s'etalonner contre un seul. Tant
qu'elle n'y est pas a zero, ce qu'elle dit de ferrite ne vaut rien.
"""
import json
import math
import sys
import urllib.error
import urllib.request

INDEX = "sonde-score"

# La tolerance, et pourquoi la batterie est coupee en deux.
#
# ES serialise un `float` : deux serveurs qui calculent la meme chose rendent le
# meme nombre, ou ne calculent pas la meme chose. Il n'y a donc **rien a
# tolerer** sur l'arithmetique de `function_score` — et c'est pour ca que la
# plupart des questions posent une requete dont le score de base est **exact des
# deux cotes** : une somme de `constant_score` (5.0, 3.0, 2.0, 1.0). Sur
# celles-la, l'egalite est exigee au bit pres, et ce qu'elle mesure est
# exactement ce que cette carte livre.
#
# Un `match` reel, lui, part d'un BM25 que tantivy et Lucene ne calculent pas au
# dernier bit pres — un ecart **anterieur a cette carte**, qui se voit deja sur
# la requete seule (`diff_relevance.py` compare l'ordre, pas la valeur). Les
# questions posees sur un `match` sont donc marquees, et leur tolerance n'est pas
# choisie : c'est le pire ecart relatif **mesure sur la requete nue**, sans
# aucune fonction, calcule par la sonde elle-meme au demarrage et imprime.
TOLERANCE = 0.0


def http(base, method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        base + path, data=data, method=method,
        headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        corps = e.read()
        try:
            return e.code, json.loads(corps or b"{}")
        except json.JSONDecodeError:
            return e.code, {"raw": corps.decode(errors="replace")}


MAPPING = {"mappings": {"properties": {
    "titre": {"type": "text"},
    "cat": {"type": "keyword"},
    "vues": {"type": "long"},
    "note": {"type": "double"},
    "prix": {"type": "float"},
    "date": {"type": "date"},
    "actif": {"type": "boolean"},
    "multi": {"type": "long"},
    "toujours": {"type": "long"},
}}}

# Le corpus est bati pour les endroits ou une fonction de score se decide :
#   - des documents qui **n'ont pas** le champ (une decroissance y rend 1.0, un
#     `field_value_factor` sans `missing` y fait echouer la recherche) ;
#   - une valeur **nulle** et une valeur **negative** (le domaine de `log`,
#     `sqrt` et `reciprocal` s'y arrete) ;
#   - un champ **multivalue** (ES lit la plus petite valeur, et la plus petite
#     **distance** — ce n'est pas la meme chose) ;
#   - des dates de part et d'autre de l'origine, dont une a la milliseconde ;
#   - un score de base qui **varie** d'un document a l'autre, sans quoi
#     `boost_mode` et `max_boost` rendraient tous la meme chose.
DOCS = {
    "d1": {"titre": "alpha beta gamma", "cat": "a", "vues": 100, "note": 4.5,
           "prix": 19.99, "date": "2026-08-01", "actif": True,
           "multi": [5, 1, 9], "toujours": 3},
    "d2": {"titre": "alpha", "cat": "b", "vues": 3, "note": 1.0,
           "prix": 0.5, "date": "2026-06-01T12:34:56.789Z", "actif": False,
           "multi": [7], "toujours": 1},
    "d3": {"titre": "alpha alpha beta", "cat": "a", "vues": 0, "note": 0.0,
           "prix": 0.0, "date": "2026-08-31", "actif": True, "toujours": 7},
    "d4": {"titre": "alpha delta", "cat": "c", "vues": -20, "note": -2.5,
           "prix": -1.5, "date": "2027-01-15", "actif": False,
           "multi": [-3, 12], "toujours": 2},
    # Celui qui n'a aucun des champs numeriques : c'est lui qui separe
    # « distance manquante = 0 » de « document ecarte ».
    "d5": {"titre": "alpha epsilon beta", "cat": "b", "toujours": 5},
    "d6": {"titre": "zeta", "cat": "a", "vues": 1000000, "note": 12.25,
           "prix": 1234.5, "date": "2020-01-01", "actif": True,
           "multi": [0], "toujours": 4},
}

ALPHA = {"match": {"titre": "alpha"}}

# La requete de base des questions « exactes » : son score varie d'un document a
# l'autre (5, 3, 2, 1) et il est **exact en `float` des deux cotes**, puisque ce
# ne sont que des sommes de `constant_score` entiers. Sans un score de base qui
# varie, `boost_mode`, `max_boost` et `min_score` rendraient tous la meme chose.
BASE = {"bool": {"should": [
    {"constant_score": {"filter": {"term": {"cat": "a"}}, "boost": 4}},
    {"constant_score": {"filter": {"term": {"cat": "b"}}, "boost": 2}},
    {"constant_score": {"filter": {"exists": {"field": "vues"}}, "boost": 1}},
]}}


def bulk(base, index, docs):
    lignes = []
    for id_, doc in docs.items():
        lignes.append(json.dumps({"index": {"_index": index, "_id": id_}}))
        lignes.append(json.dumps(doc))
    corps = "\n".join(lignes) + "\n"
    req = urllib.request.Request(
        base + "/_bulk?refresh=true", data=corps.encode(), method="POST",
        headers={"Content-Type": "application/x-ndjson"})
    urllib.request.urlopen(req).read()


def prepare(base):
    http(base, "DELETE", f"/{INDEX}")
    http(base, "PUT", f"/{INDEX}", MAPPING)
    bulk(base, INDEX, DOCS)


def fs(**kw):
    """Un `function_score` sur la requete de base au score exact."""
    corps = dict(query=BASE)
    corps.update(kw)
    return {"function_score": corps}


def cas():
    """Les questions, chacune avec ce qu'elle separe."""
    out = []

    def q(libelle, requete, exact=True, corps=None):
        out.append((libelle, requete, exact, corps))

    # --- ce que la clause fait quand elle ne fait rien
    q("aucune fonction", fs())
    q("functions vide", fs(functions=[]))
    q("sans query", {"function_score": {"weight": 2}})

    # --- weight, seul et filtre
    q("weight seul", fs(weight=2))
    q("weight 0", fs(weight=0))
    q("weight 0.5", fs(weight=0.5))
    q("weight en chaine", fs(weight="2"))
    q("weight negatif (refus)", fs(weight=-1))
    q("weight dans functions", fs(functions=[{"weight": 3}]))
    q("weight filtre", fs(functions=[{"filter": {"term": {"cat": "a"}}, "weight": 3}]))
    q("filter seul (refus)", fs(functions=[{"filter": {"term": {"cat": "a"}}}]))
    q("boost_factor (ES le refuse aussi)", fs(boost_factor=3))

    # --- field_value_factor
    q("fvf simple", fs(field_value_factor={"field": "toujours"}))
    q("fvf champ absent chez d5 (refus)", fs(field_value_factor={"field": "vues"}))
    q("fvf missing", fs(field_value_factor={"field": "vues", "missing": 1}))
    q("fvf missing 0", fs(field_value_factor={"field": "vues", "missing": 0}))
    q("fvf factor", fs(field_value_factor={"field": "toujours", "factor": 2.5}))
    q("fvf factor negatif (refus)",
      fs(field_value_factor={"field": "toujours", "factor": -1}))
    for m in ["none", "log", "log1p", "log2p", "ln", "ln1p", "ln2p",
              "square", "sqrt", "reciprocal"]:
        q(f"fvf modifier {m}",
          fs(field_value_factor={"field": "toujours", "modifier": m}))
    q("fvf log sur 0 (refus)",
      fs(field_value_factor={"field": "vues", "missing": 0, "modifier": "log"}))
    q("fvf sur valeurs negatives (refus)",
      fs(field_value_factor={"field": "note", "missing": 1}))
    q("fvf sur double", fs(field_value_factor={"field": "note", "missing": 1,
                                               "modifier": "square"}))
    q("fvf sur float", fs(field_value_factor={"field": "prix", "missing": 1,
                                              "modifier": "square"}))
    q("fvf sur date", fs(field_value_factor={"field": "date", "missing": 1,
                                             "modifier": "log"}))
    q("fvf sur booleen", fs(field_value_factor={"field": "actif", "missing": 0}))
    q("fvf multivalue", fs(field_value_factor={"field": "multi", "missing": 1,
                                               "modifier": "square"}))
    q("fvf champ non mappe", fs(field_value_factor={"field": "inconnu", "missing": 2}))
    q("fvf sur keyword (refus)", fs(field_value_factor={"field": "cat"}))
    q("fvf sur text (refus)", fs(field_value_factor={"field": "titre"}))
    q("fvf sans field (refus)", fs(field_value_factor={}))
    q("fvf modifier inconnu (refus)",
      fs(field_value_factor={"field": "toujours", "modifier": "nawak"}))

    # --- les trois decroissances, sur un numerique
    for f in ["gauss", "exp", "linear"]:
        q(f"{f} numerique", fs(**{f: {"vues": {"origin": 100, "scale": 50}}}))
        q(f"{f} offset", fs(**{f: {"vues": {"origin": 100, "scale": 50, "offset": 30}}}))
        q(f"{f} decay 0.1",
          fs(**{f: {"vues": {"origin": 100, "scale": 50, "decay": 0.1}}}))
        q(f"{f} decay 0.9",
          fs(**{f: {"vues": {"origin": 100, "scale": 50, "decay": 0.9}}}))
        q(f"{f} echelle fine",
          fs(**{f: {"note": {"origin": 0, "scale": 0.5}}}))
        q(f"{f} grande echelle",
          fs(**{f: {"vues": {"origin": 0, "scale": 1000000}}}))
        q(f"{f} multivalue", fs(**{f: {"multi": {"origin": 0, "scale": 4}}}))
        q(f"{f} sur booleen", fs(**{f: {"actif": {"origin": 1, "scale": 1}}}))
        q(f"{f} date", fs(**{f: {"date": {"origin": "2026-08-31", "scale": "10d"}}}))
        q(f"{f} date offset",
          fs(**{f: {"date": {"origin": "2026-08-31", "scale": "10d",
                             "offset": "2d", "decay": 0.3}}}))
        q(f"{f} date scale en ms",
          fs(**{f: {"date": {"origin": "2026-08-31", "scale": "864000000ms"}}}))
        q(f"{f} date origin en date math",
          fs(**{f: {"date": {"origin": "2026-08-31||-1M", "scale": "30d"}}}))
        q(f"{f} date origin absent",
          fs(**{f: {"date": {"scale": "36500d"}}}))
        q(f"{f} champ non mappe", fs(**{f: {"inconnu": {"origin": 1, "scale": 1}}}))
        q(f"{f} sans scale (refus)", fs(**{f: {"vues": {"origin": 1}}}))
        q(f"{f} sans origin sur numerique (refus)", fs(**{f: {"vues": {"scale": 1}}}))
        q(f"{f} scale 0 (refus)", fs(**{f: {"vues": {"origin": 1, "scale": 0}}}))
        q(f"{f} scale negatif (refus)", fs(**{f: {"vues": {"origin": 1, "scale": -5}}}))
        q(f"{f} offset negatif (refus)",
          fs(**{f: {"vues": {"origin": 1, "scale": 5, "offset": -1}}}))
        q(f"{f} decay 0 (refus)",
          fs(**{f: {"vues": {"origin": 1, "scale": 5, "decay": 0}}}))
        q(f"{f} decay 1 (refus)",
          fs(**{f: {"vues": {"origin": 1, "scale": 5, "decay": 1}}}))
        q(f"{f} sur keyword (refus)", fs(**{f: {"cat": {"origin": "a", "scale": 1}}}))
        q(f"{f} date scale sans unite (refus)",
          fs(**{f: {"date": {"origin": "2026-08-31", "scale": 10}}}))
        q(f"{f} deux champs (refus)",
          fs(**{f: {"vues": {"origin": 1, "scale": 5},
                    "note": {"origin": 1, "scale": 5}}}))
        q(f"{f} vide (refus)", fs(**{f: {}}))

    # --- score_mode : la combinaison des fonctions entre elles
    TROIS = [
        {"filter": {"term": {"cat": "a"}}, "weight": 3},
        {"filter": {"term": {"cat": "b"}}, "weight": 5},
        {"field_value_factor": {"field": "toujours"}},
    ]
    for m in ["multiply", "sum", "avg", "first", "max", "min"]:
        q(f"score_mode {m}", fs(functions=TROIS, score_mode=m))
        # Aucune fonction ne s'applique : `sum`, `avg`, `max` et `min`
        # retombent alors sur 1.0, pas sur 0 ni sur -inf.
        q(f"score_mode {m} sans fonction applicable",
          fs(functions=[{"filter": {"term": {"cat": "zzz"}}, "weight": 3}],
             score_mode=m))
        # `avg` divise par la **somme des poids**, pas par le nombre : deux
        # fonctions de poids 3 et 5 ne font pas une moyenne sur 2.
        q(f"score_mode {m} avec poids",
          fs(functions=[{"weight": 3}, {"weight": 5}], score_mode=m))
        q(f"score_mode {m} fonction+poids",
          fs(functions=[{"field_value_factor": {"field": "toujours"}, "weight": 2},
                        {"weight": 5}], score_mode=m))
    q("score_mode inconnu (refus)", fs(weight=2, score_mode="nawak"))
    q("score_mode majuscules", fs(weight=2, score_mode="AVG"))

    # --- boost_mode : la combinaison avec le score de la requete
    for m in ["multiply", "replace", "sum", "avg", "min", "max"]:
        q(f"boost_mode {m}", fs(weight=2, boost_mode=m))
        q(f"boost_mode {m} avec fvf",
          fs(field_value_factor={"field": "toujours"}, boost_mode=m))
    q("boost_mode inconnu (refus)", fs(weight=2, boost_mode="nawak"))

    # --- max_boost : il plafonne le score des **fonctions**, pas le resultat
    q("max_boost", fs(field_value_factor={"field": "toujours"}, max_boost=3))
    q("max_boost sous boost_mode sum",
      fs(field_value_factor={"field": "toujours"}, max_boost=3, boost_mode="sum"))
    q("max_boost 0", fs(weight=2, max_boost=0))

    # --- min_score : il coupe **avant** le boost de la clause
    q("min_score", fs(field_value_factor={"field": "toujours"}, min_score=3))
    q("min_score et boost",
      fs(field_value_factor={"field": "toujours"}, min_score=3, boost=10))
    q("min_score qui ne coupe rien", fs(weight=2, min_score=0))
    q("min_score qui coupe tout", fs(weight=2, min_score=1000))
    q("min_score negatif", fs(weight=2, min_score=-1))
    # Le `boost` d'une clause ne s'applique **que si** le collecteur demande
    # des scores : sous un `sort` par champ, ou a `size: 0`, Lucene comme
    # tantivy laissent tomber leur `BoostQuery`. Ca ne se voit nulle part
    # ailleurs — un facteur constant ne change pas un ensemble de documents —
    # sauf ici, ou `min_score` en fait un seuil.
    seuil = fs(field_value_factor={"field": "toujours"}, min_score=6, boost=3)
    q("min_score et boost, avec un tri par champ", seuil,
      corps={"sort": [{"cat": "asc"}], "track_total_hits": True})
    q("min_score et boost, a size 0", seuil,
      corps={"size": 0, "track_total_hits": True})
    q("min_score et boost, en recherche libre", seuil,
      corps={"track_total_hits": True})

    # --- le boost de la clause
    q("boost", fs(weight=2, boost=3))

    # --- la forme du corps
    q("functions + fonction unique (refus)", fs(weight=2, functions=[{"weight": 3}]))
    q("cle inconnue (refus)", fs(weight=2, nawak=1))
    q("cle inconnue dans functions (refus)", fs(functions=[{"weight": 2, "nawak": 1}]))
    q("deux fonctions dans une entree (refus)",
      fs(functions=[{"gauss": {"vues": {"origin": 1, "scale": 5}},
                     "exp": {"vues": {"origin": 1, "scale": 5}}}]))
    q("random_score (refus ferrite)", fs(random_score={}))
    q("script_score (refus ferrite)", fs(script_score={"script": "1"}))
    q("multi_value_mode (refus ferrite)",
      fs(gauss={"multi": {"origin": 0, "scale": 4}, "multi_value_mode": "max"}))

    # --- imbrication : la clause doit se composer comme les autres
    q("function_score sous bool",
      {"bool": {"should": [fs(weight=3), {"constant_score": {
          "filter": {"term": {"cat": "b"}}, "boost": 7}}]}})
    q("function_score de function_score",
      {"function_score": {"query": fs(weight=2), "weight": 3}})
    q("function_score sur un bool",
      {"function_score": {"query": {"bool": {"must": [BASE],
                                             "must_not": [{"term": {"cat": "c"}}]}},
                          "field_value_factor": {"field": "toujours"}}})

    # --- et les memes gestes sur un **vrai** score BM25 : ceux-la portent
    # l'ecart de base entre tantivy et Lucene, et leur tolerance est mesuree
    # (voir `TOLERANCE` en tete de fichier).
    def qm(libelle, requete, corps=None):
        q("[bm25] " + libelle, requete, exact=False, corps=corps)

    def fsm(**kw):
        corps = dict(query=ALPHA)
        corps.update(kw)
        return {"function_score": corps}

    qm("aucune fonction", fsm())
    qm("weight", fsm(weight=2))
    qm("fvf", fsm(field_value_factor={"field": "toujours"}))
    qm("gauss date", fsm(gauss={"date": {"origin": "2026-08-31", "scale": "10d"}}))
    qm("exp numerique", fsm(exp={"vues": {"origin": 100, "scale": 50}}))
    qm("boost_mode sum", fsm(weight=2, boost_mode="sum"))
    qm("boost_mode replace", fsm(weight=2, boost_mode="replace"))
    qm("min_score", fsm(field_value_factor={"field": "toujours"}, min_score=1))
    qm("boosting", {"boosting": {"positive": ALPHA,
                                 "negative": {"term": {"cat": "a"}},
                                 "negative_boost": 0.2}})

    # --- boosting
    q("boosting", {"boosting": {"positive": BASE,
                                "negative": {"term": {"cat": "a"}},
                                "negative_boost": 0.2}})
    q("boosting 0", {"boosting": {"positive": BASE,
                                  "negative": {"term": {"cat": "a"}},
                                  "negative_boost": 0}})
    q("boosting >1", {"boosting": {"positive": BASE,
                                   "negative": {"term": {"cat": "a"}},
                                   "negative_boost": 1.5}})
    q("boosting negative sans correspondance",
      {"boosting": {"positive": BASE, "negative": {"term": {"cat": "zzz"}},
                    "negative_boost": 0.2}})
    q("boosting negative = positive",
      {"boosting": {"positive": BASE, "negative": BASE, "negative_boost": 0.5}})
    q("boosting boost", {"boosting": {"positive": BASE,
                                      "negative": {"term": {"cat": "a"}},
                                      "negative_boost": 0.5, "boost": 2}})
    q("boosting sous function_score",
      {"function_score": {"query": {"boosting": {
          "positive": BASE, "negative": {"term": {"cat": "a"}},
          "negative_boost": 0.5}}, "weight": 2}})
    q("boosting sans negative_boost (refus)",
      {"boosting": {"positive": BASE, "negative": {"term": {"cat": "a"}}}})
    q("boosting negative_boost negatif (refus)",
      {"boosting": {"positive": BASE, "negative": {"term": {"cat": "a"}},
                    "negative_boost": -0.5}})
    q("boosting sans positive (refus)",
      {"boosting": {"negative": {"term": {"cat": "a"}}, "negative_boost": 0.5}})
    q("boosting sans negative (refus)",
      {"boosting": {"positive": BASE, "negative_boost": 0.5}})
    q("boosting cle inconnue (refus)",
      {"boosting": {"positive": BASE, "negative": {"term": {"cat": "a"}},
                    "negative_boost": 0.5, "nawak": 1}})
    return out


def interroge(base, requete, corps_en_plus=None):
    """La reponse comparee : les hits avec **leur score**, `max_score`, le
    total — ou le statut et la cause d'une erreur."""
    corps = {"size": 20, "_source": False, "query": requete}
    if corps_en_plus:
        corps.update(corps_en_plus)
    st, body = http(base, "POST", f"/{INDEX}/_search", corps)
    if st != 200:
        err = body.get("error", {})
        cause = err.get("root_cause", [err])[0] if err.get("root_cause") else err
        return {"statut": st, "type": cause.get("type"),
                "raison": cause.get("reason")}
    return {
        "hits": [(h["_id"], h["_score"]) for h in body["hits"]["hits"]],
        "max_score": body["hits"]["max_score"],
        "total": body["hits"]["total"]["value"],
    }


def ecart_relatif(a, b):
    if a == b:
        return 0.0
    if a is None or b is None:
        return math.inf
    if isinstance(a, bool) or isinstance(b, bool):
        return math.inf
    d = abs(a - b)
    e = max(abs(a), abs(b))
    return d / e if e else d


def compare(gauche, droite, tolerance=TOLERANCE):
    """Ce qui separe les deux reponses, en clair.

    Rend `(identiques, ordre_identique, pire_ecart, phrase)`."""
    if "statut" in gauche or "statut" in droite:
        if gauche == droite:
            return True, True, 0.0, None
        return False, True, math.inf, None
    ids_g = [i for i, _ in gauche["hits"]]
    ids_d = [i for i, _ in droite["hits"]]
    if ids_g != ids_d or gauche["total"] != droite["total"]:
        return False, False, math.inf, None
    pire = ecart_relatif(gauche["max_score"], droite["max_score"])
    for (_, sg), (_, sd) in zip(gauche["hits"], droite["hits"]):
        pire = max(pire, ecart_relatif(sg, sd))
    if pire > tolerance:
        pires = [f"{i}: {sg!r} vs {sd!r}"
                 for (i, sg), (_, sd) in zip(gauche["hits"], droite["hits"])
                 if ecart_relatif(sg, sd) > tolerance]
        return False, True, pire, "; ".join(pires[:4])
    return True, True, pire, None


# Les ecarts assumes, chacun avec sa raison ecrite. Deux classes :
#
# - `refus` : les **deux** serveurs refusent, seuls le type et la phrase
#   different. Le predicat verifie que les deux refusent, et avec le meme
#   statut : sans cette seconde moitie, la liste couvrirait aussi le cas ou
#   ferrite refuse ce qu'ES sait faire ;
# - `perimetre` : ferrite refuse ce qu'ES sait faire, et c'est ecrit dans
#   `compat.yaml`. Le predicat verifie que ferrite **refuse explicitement**,
#   jamais qu'il rend un resultat en silence.
REFUS_ASSUMES = {
    "random_score (refus ferrite)": ("perimetre",
        "un score tire au sort ne se reproduit pas d'un moteur a l'autre : le "
        "servir voudrait dire reproduire le hachage de Lucene sur des "
        "identifiants internes qui ne sont pas les memes. Refuse en le nommant"),
    "script_score (refus ferrite)": ("perimetre",
        "il suppose Painless, un langage a part entiere — hors perimetre, "
        "declare"),
    "multi_value_mode (refus ferrite)": ("perimetre",
        "seul le defaut d'ES (`min`, applique a la **distance** et non a la "
        "valeur) est servi ; les trois autres sont refuses en les nommant "
        "plutot que servis sans avoir ete mesures"),
    "fvf sur keyword (refus)": ("refus",
        "les deux refusent. ES le fait par un message qui cite une classe Java "
        "(`SortedSetOrdinalsIndexFieldData cannot be cast to ...`) ; ferrite dit "
        "que le champ n'est pas numerique"),
    "fvf sur text (refus)": ("refus",
        "les deux refusent : ES par « Fielddata is disabled on [titre] », "
        "ferrite en disant que le champ n'est pas numerique"),
    "fvf modifier inconnu (refus)": ("refus",
        "les deux refusent en 400 ; ES rend « No enum constant ...Modifier.NAWAK », "
        "ferrite nomme les dix modificateurs acceptes"),
    "score_mode inconnu (refus)": ("refus",
        "les deux refusent en 400 ; ES rend « No enum constant ...ScoreMode.NAWAK », "
        "ferrite nomme les six modes acceptes"),
    "boost_mode inconnu (refus)": ("refus",
        "les deux refusent en 400 ; ES rend « No enum constant "
        "...CombineFunction.NAWAK », ferrite nomme les six modes acceptes"),
    "gauss sur keyword (refus)": ("refus",
        "les deux refusent : ES en echouant a lire l'origine (« For input "
        "string: \"a\" »), ferrite en disant que le champ n'est pas numerique"),
    "exp sur keyword (refus)": ("refus", "meme raison que `gauss sur keyword`"),
    "linear sur keyword (refus)": ("refus", "meme raison que `gauss sur keyword`"),
    "fvf champ absent chez d5 (refus)": ("refus",
        "les deux refusent en 500 avec « Missing value for field [vues] » ; seul "
        "le nom du noeud dans `failed_shards` differe, et il n'est pas compare"),
    "fvf log sur 0 (refus)": ("refus",
        "les deux refusent en 400 avec la meme phrase ; ES ajoute un "
        "`caused_by` imbrique que ferrite ne rend pas"),
    "fvf sur valeurs negatives (refus)": ("refus", "meme raison"),
    "fvf factor negatif (refus)": ("refus", "meme raison"),
    "gauss deux champs (refus)": ("perimetre",
        "ES accepte plusieurs champs dans une meme decroissance et n'en applique "
        "**qu'un**, sans dire lequel (mesure) ; le reproduire demanderait de "
        "deviner. Refuse en le nommant"),
    "exp deux champs (refus)": ("perimetre", "meme raison que `gauss deux champs`"),
    "linear deux champs (refus)": ("perimetre",
        "meme raison que `gauss deux champs`"),
    "max_boost 0": ("refus",
        "un plafond nul rend un score de fonction nul, donc un score final nul "
        "— aucun des deux ne l'interdit, mais ES nomme le document fautif par "
        "son numero interne quand il faut le refuser, et ces numeros ne sont "
        "pas les memes"),
}


# Ce qui ne se compare pas entre **deux** serveurs, quels qu'ils soient : un
# score tire au sort. Hors `--calibrer`, ferrite le refuse et le cas mesure ce
# refus ; en calibrage, deux Elasticsearch rendent deux tirages differents, et
# les comparer ne dirait rien.
NON_DETERMINISTES = {"random_score (refus ferrite)"}


def refuse(vu):
    return "statut" in vu


def assume(libelle, gauche, droite):
    classe = REFUS_ASSUMES.get(libelle, (None, None))[0]
    if classe == "refus":
        return (refuse(gauche) and refuse(droite)
                and gauche["statut"] == droite["statut"])
    if classe == "perimetre":
        return refuse(gauche)
    return False


def abrege(vu, n=130):
    s = json.dumps(vu, ensure_ascii=False)
    return s if len(s) <= n else s[:n - 3] + "..."


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
            http(base, "GET", "/")
        except Exception as e:  # noqa: BLE001
            print(f"# {nom} indisponible ({base}) : {e}")
            print("# une sonde differentielle qui ne compare rien ne rend pas "
                  "de verdict : arret.")
            return 2
    for _, base in cibles:
        prepare(base)

    # Le temoin : la requete `match` **nue**, sans une seule fonction. L'ecart
    # qu'elle porte est celui que les deux serveurs ont deja sur un BM25, et il
    # est anterieur a cette carte. C'est lui, et rien d'autre, qui sert de
    # tolerance aux questions marquees `[bm25]` — une tolerance mesuree, pas
    # choisie assez large pour que ca passe.
    tg = interroge(cibles[0][1], ALPHA)
    td = interroge(cibles[1][1], ALPHA)
    _, _, temoin, _ = compare(tg, td)
    if temoin == math.inf:
        temoin = 0.0
    # Ce que la clause ajoute a cet ecart, et rien de plus : un `float` arrondit
    # a 2^-24 pres en relatif, et la chaine de `function_score` en pose au plus
    # trois par-dessus le score de base (la valeur de la fonction, la
    # combinaison, le boost). La tolerance des questions [bm25] est donc
    # `temoin + 3 x 2^-24`, dont aucun des deux termes n'est choisi.
    tolerance_bm25 = temoin + 3 * 2 ** -24
    print(f"# temoin : la requete nue (`match alpha`, aucune fonction) diverge "
          f"de {temoin:.3g} en relatif entre les deux serveurs — c'est l'ecart "
          f"que\n#          tantivy et Lucene ont deja sur un BM25, anterieur a "
          f"cette carte.")
    print(f"# tolerance des questions [bm25] : {tolerance_bm25:.3g} "
          f"(= temoin + 3 arrondis de float). Les autres sont comparees au bit "
          f"pres.\n")

    batterie = cas()
    ecarts = assumes = 0
    ordres_faux = 0
    pire_ecart = 0.0
    for libelle, requete, exact, corps in batterie:
        g = interroge(cibles[0][1], requete, corps)
        d = interroge(cibles[1][1], requete, corps)
        pareils, meme_ordre, ecart, detail = compare(
            g, d, TOLERANCE if exact else tolerance_bm25)
        if calibrer and libelle in NON_DETERMINISTES:
            print(f"~ {libelle:48} non comparable entre deux serveurs "
                  f"(score tire au sort)")
            assumes += 1
            continue
        if pareils:
            print(f"  {libelle:48} {abrege(g)}")
            continue
        if assume(libelle, g, d):
            assumes += 1
            print(f"~ {libelle:48} {cibles[0][0]}={abrege(g, 90)}")
            print(f"  {'':48} {cibles[1][0]}={abrege(d, 90)}")
            print(f"      assume ({REFUS_ASSUMES[libelle][0]}) : "
                  f"{REFUS_ASSUMES[libelle][1]}")
            continue
        ecarts += 1
        if not meme_ordre:
            ordres_faux += 1
        if ecart != math.inf:
            pire_ecart = max(pire_ecart, ecart)
        print(f"* {libelle:48} {cibles[0][0]}={abrege(g)}")
        print(f"  {'':48} {cibles[1][0]}={abrege(d)}")
        if detail:
            print(f"      scores : {detail}  (ecart relatif {ecart:.3g})")
        elif not meme_ordre:
            print("      les documents ou leur ordre different")

    total = len(batterie)
    print(f"\n{total - ecarts - assumes}/{total} identiques, "
          f"{assumes} refus assumes, {ecarts} ecarts "
          f"(dont {ordres_faux} sur l'ordre ou l'ensemble des documents)")
    if pire_ecart:
        print(f"pire ecart relatif de score restant : {pire_ecart:.3g}")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
