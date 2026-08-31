#!/usr/bin/env python3
"""Sonde : « pourquoi ce document, avec ce score » — et **ou** le score diverge.

    python3 tests/compat/sonde_explain.py [ferrite] [es]
    python3 tests/compat/sonde_explain.py --calibrer [es_a] [es_b]
    python3 tests/compat/sonde_explain.py --ecart [ferrite] [es]

Trois mecanismes, et un instrument.

Les trois mecanismes sont `_name` / `matched_queries`, `explain: true` dans le
corps d'une recherche, et la route `GET /{index}/_explain/{id}`. Ils repondent a
la meme question sous trois formes, et la sonde les compare a un vrai
Elasticsearch : les **noms rendus et leur ordre**, le booleen `matched`, la
**forme** de l'arbre d'explication et la **valeur** de chacun de ses noeuds.

L'instrument, c'est `--ecart`, et c'est le vrai livrable de cette carte. Le
`_score` d'un `match` ne coincide pas entre les deux moteurs : c'est declare
depuis longtemps dans `docs/compat.md`, et jusqu'ici la seule chose qu'on
pouvait en dire etait « les nombres different ». Un arbre d'explication permet
de dire **de combien et pourquoi** : il porte les cinq statistiques dont le BM25
depend (`n`, `N`, `freq`, `dl`, `avgdl`), et `--ecart` les met cote a cote, puis
**recalcule le score d'ES avec les statistiques de ferrite et l'inverse**. Si le
nombre retombe juste, l'ecart est entierement explique par ces statistiques-la
et par rien d'autre — ce qui est une mesure, pas une hypothese.

`--calibrer` rejoue la meme batterie contre **deux** Elasticsearch : elle ecrit
(elle cree son index), donc elle ne peut pas s'etalonner contre un seul. Tant
qu'elle n'y est pas a zero, ce qu'elle dit de ferrite ne vaut rien.
"""
import json
import math
import sys
import urllib.error
import urllib.request

INDEX = "sonde-explain"


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
    "corps": {"type": "text"},
    "cat": {"type": "keyword"},
    # Un `keyword` que **tous** les documents n'ont pas. `cat`, lui, est sur
    # chacun : c'est ce qui les separe, et c'est tout le sujet de `--ecart`.
    "ref": {"type": "keyword"},
    "vues": {"type": "long"},
    "prix": {"type": "double"},
    "date": {"type": "date"},
}}}

# Le corpus est bati pour ce que l'arbre doit montrer :
#   - des documents qui **n'ont pas** le champ `titre` (c'est la seule facon de
#     separer le `N` de Lucene — les documents qui ont le champ — de celui de
#     tantivy, qui les compte tous) ;
#   - des longueurs de champ tres differentes (`dl`), donc un `avgdl` qui ne
#     vaut pas 1 ;
#   - un terme repete dans un document (`freq` > 1) ;
#   - un terme rare et un terme frequent (`n` de 1 a 4).
DOCS = {
    "d1": {"titre": "alpha beta gamma delta epsilon", "corps": "un texte",
           "cat": "a", "ref": "REF-1", "vues": 100, "prix": 19.99,
           "date": "2026-08-01"},
    "d2": {"titre": "alpha", "corps": "un autre texte", "cat": "b",
           "ref": "REF-1", "vues": 3, "prix": 0.5, "date": "2026-06-01"},
    "d3": {"titre": "alpha alpha alpha beta", "corps": "encore", "cat": "a",
           "ref": "REF-2", "vues": 0, "prix": 0.0, "date": "2026-08-31"},
    "d4": {"titre": "zeta", "cat": "c", "vues": -20, "prix": -1.5,
           "date": "2027-01-15"},
    # Les deux sans `titre` : ce sont eux qui font diverger `N` et `avgdl`.
    "d5": {"corps": "sans titre du tout", "cat": "b", "vues": 7},
    "d6": {"corps": "sans titre non plus", "cat": "a", "vues": 42},
}


def prepare(base):
    http(base, "DELETE", "/" + INDEX)
    http(base, "PUT", "/" + INDEX, MAPPING)
    lignes = []
    for _id, doc in DOCS.items():
        lignes.append(json.dumps({"index": {"_index": INDEX, "_id": _id}}))
        lignes.append(json.dumps(doc))
    corps = "\n".join(lignes) + "\n"
    req = urllib.request.Request(
        base + "/_bulk?refresh=true", data=corps.encode(), method="POST",
        headers={"Content-Type": "application/x-ndjson"})
    urllib.request.urlopen(req).read()


# ---------------------------------------------------------------------------
# La batterie
# ---------------------------------------------------------------------------

ALPHA = {"match": {"titre": "alpha"}}


def cas():
    """(libelle, genre, charge) — `genre` dit ce que la question interroge."""
    b = []

    # --- `_name` / `matched_queries` : ce qui a fait correspondre le document.
    def nom(libelle, requete):
        b.append((f"nom: {libelle}", "noms", requete))

    nom("term nomme", {"term": {"cat": {"value": "a", "_name": "cat_a"}}})
    nom("term sans objet (ES refuse)", {"term": {"cat": "a", "_name": "x"}})
    nom("match nomme", {"match": {"titre": {"query": "alpha", "_name": "m"}}})
    nom("match_all nomme", {"match_all": {"_name": "tout"}})
    nom("bool et sa clause", {"bool": {
        "must": [{"term": {"cat": {"value": "a", "_name": "interne"}}}],
        "_name": "externe"}})
    nom("filter nomme", {"bool": {
        "filter": [{"term": {"cat": {"value": "a", "_name": "filtre"}}}]}})
    nom("sous un must_not", {"bool": {
        "must": [{"match_all": {}}],
        "must_not": [{"term": {"cat": {"value": "a", "_name": "nie"}}}]}})
    nom("should qui ne correspond a rien", {"bool": {
        "must": [{"match_all": {}}],
        "should": [{"term": {"cat": {"value": "zzz", "_name": "vide"}}}]}})
    nom("deux noms", {"bool": {"should": [
        {"term": {"cat": {"value": "a", "_name": "aaa"}}},
        {"match": {"titre": {"query": "alpha", "_name": "zzz"}}}]}})
    # L'ordre des noms n'est ni celui de la requete ni l'alphabetique : c'est
    # celui d'une table de hachage de Java. Cinq noms choisis pour que les deux
    # lectures fausses se voient.
    nom("cinq noms, l'ordre compte", {"bool": {"should": [
        {"term": {"cat": {"value": "a", "_name": "zzz"}}},
        {"term": {"cat": {"value": "b", "_name": "aaa"}}},
        {"exists": {"field": "vues", "_name": "mmm"}},
        {"range": {"vues": {"gte": 0, "_name": "kkk"}}},
        {"match_all": {"_name": "bbb"}}]}})
    nom("treize noms (la table double)", {"bool": {"should": [
        {"term": {"cat": {"value": "a", "_name": f"q{i:03}"}}}
        for i in range(13)]}})
    nom("le meme nom deux fois", {"bool": {"should": [
        {"term": {"cat": {"value": "a", "_name": "double"}}},
        {"term": {"cat": {"value": "b", "_name": "double"}}}]}})
    nom("nested nomme", {"nested": {"path": "l", "query": {"match_all": {}},
                                    "_name": "n"}})
    nom("nomme sous un constant_score", {"constant_score": {
        "filter": {"term": {"cat": {"value": "a", "_name": "csf"}}},
        "_name": "cs"}})
    nom("nomme dans un dis_max", {"dis_max": {"queries": [
        {"term": {"cat": {"value": "a", "_name": "d1"}}},
        {"match": {"titre": {"query": "alpha", "_name": "d2"}}}],
        "_name": "dm"}})
    nom("nomme dans un function_score", {"function_score": {
        "query": {"term": {"cat": {"value": "a", "_name": "fq"}}},
        "_name": "fs"}})
    nom("nomme dans un boosting", {"boosting": {
        "positive": {"match_all": {"_name": "p"}},
        "negative": {"term": {"cat": {"value": "a", "_name": "ng"}}},
        "negative_boost": 0.5, "_name": "bo"}})
    nom("nom numerique", {"term": {"cat": {"value": "a", "_name": 42}}})
    nom("nom sur un objet (refus)", {"term": {"cat": {"value": "a",
                                                      "_name": {"a": 1}}}})
    b.append(("nom: avec le score", "noms_score", {"bool": {"should": [
        {"term": {"cat": {"value": "a", "_name": "aaa"}}},
        {"match": {"titre": {"query": "alpha", "_name": "zzz"}}}],
        "filter": [{"range": {"vues": {"gte": -100, "_name": "kkk"}}}]}}))

    # --- `explain: true` : l'arbre.
    def arbre(libelle, requete):
        b.append((f"arbre: {libelle}", "explain", requete))

    arbre("term sur keyword", {"term": {"cat": "a"}})
    arbre("term sur text", {"term": {"titre": "alpha"}})
    arbre("term rare", {"term": {"titre": "gamma"}})
    arbre("match un mot", ALPHA)
    arbre("match deux mots", {"match": {"titre": "alpha beta"}})
    arbre("match operator and", {"match": {"titre": {"query": "alpha beta",
                                                     "operator": "and"}}})
    arbre("match_all", {"match_all": {}})
    arbre("match_none", {"match_none": {}})
    arbre("term avec boost", {"term": {"titre": {"value": "alpha",
                                                 "boost": 3}}})
    arbre("bool must", {"bool": {"must": [{"term": {"titre": "alpha"}},
                                          {"term": {"cat": "a"}}]}})
    arbre("bool should msm", {"bool": {"should": [
        {"term": {"cat": "a"}}, {"term": {"cat": "b"}}],
        "minimum_should_match": 1}})
    arbre("bool filter", {"bool": {"must": [{"term": {"cat": "a"}}],
                                   "filter": [{"range": {"vues": {"gte": -100}}}]}})
    arbre("bool must_not seul", {"bool": {"must_not": [{"term": {"cat": "c"}}]}})
    arbre("bool avec boost", {"bool": {"must": [{"term": {"cat": "a"}}],
                                       "boost": 3}})
    arbre("constant_score", {"constant_score": {"filter": {"term": {"cat": "a"}},
                                                "boost": 3}})
    arbre("range", {"range": {"vues": {"gte": 0, "lte": 100}}})
    arbre("exists", {"exists": {"field": "titre"}})
    arbre("prefix", {"prefix": {"cat": "a"}})
    arbre("wildcard", {"wildcard": {"cat": "a*"}})
    arbre("regexp", {"regexp": {"cat": "a"}})
    arbre("terms", {"terms": {"cat": ["a", "b"]}})
    arbre("ids", {"ids": {"values": ["d1"]}})
    arbre("match_phrase", {"match_phrase": {"titre": "alpha beta"}})
    arbre("match_phrase_prefix", {"match_phrase_prefix": {"titre": "alpha be"}})
    arbre("dis_max", {"dis_max": {"queries": [{"term": {"cat": "a"}},
                                              {"match": {"titre": "alpha"}}],
                                  "tie_breaker": 0.3}})
    arbre("multi_match", {"multi_match": {"query": "alpha",
                                          "fields": ["titre", "corps"]}})
    arbre("function_score", {"function_score": {
        "query": ALPHA, "field_value_factor": {"field": "vues",
                                               "modifier": "sqrt",
                                               "missing": 1},
        "boost_mode": "multiply"}})
    arbre("boosting", {"boosting": {"positive": {"match_all": {}},
                                    "negative": {"term": {"cat": "a"}},
                                    "negative_boost": 0.5}})

    # --- la route `_explain`.
    for doc, libelle in [("d1", "document qui correspond"),
                         ("d4", "document qui ne correspond pas"),
                         ("inconnu", "document absent")]:
        b.append((f"route: {libelle}", f"route:{doc}", ALPHA))
    b.append(("route: sans corps", "route:d1", None))
    b.append(("route: clause inconnue", "route:d1", {"pas_une_clause": {}}))
    b.append(("route: bool a moitie satisfait", "route:d1",
              {"bool": {"must": [{"term": {"cat": "a"}}],
                        "must_not": [{"term": {"vues": 100}}]}}))
    return b


# ---------------------------------------------------------------------------
# Interroger
# ---------------------------------------------------------------------------

def interroge(base, genre, charge):
    if genre.startswith("route:"):
        doc = genre.split(":", 1)[1]
        corps = {"query": charge} if charge is not None else None
        st, r = http(base, "GET", f"/{INDEX}/_explain/{doc}", corps)
        if st != 200:
            return {"statut": st, "erreur": type_erreur(r),
                    "matched": r.get("matched")}
        return {"statut": st, "matched": r.get("matched"),
                "arbre": r.get("explanation")}

    corps = {"query": charge, "size": 10}
    suffixe = ""
    if genre == "explain":
        corps["explain"] = True
    if genre == "noms_score":
        suffixe = "?include_named_queries_score=true"
    st, r = http(base, "POST", f"/{INDEX}/_search{suffixe}", corps)
    if st != 200:
        return {"statut": st, "erreur": type_erreur(r)}
    hits = r["hits"]["hits"]
    # Les hits sont ranges **par identifiant**, jamais par rang : l'ordre des
    # documents est ce que `diff_relevance.py` mesure, et une requete dont le
    # score diverge (voir `--ecart`) n'a aucune raison de les classer pareil.
    # Ce que cette sonde compare, c'est ce que la reponse dit d'un document
    # donne.
    sortie = {"statut": st,
              "ids": sorted(h["_id"] for h in hits),
              "noms": {h["_id"]: h.get("matched_queries") for h in hits}}
    if genre == "explain":
        # Les arbres sont ranges **par identifiant**, pas par rang. L'ordre des
        # documents est ce que `diff_relevance.py` mesure ; ici on compare ce
        # qu'un arbre dit d'un document donne, et une requete dont le score
        # diverge n'a aucune raison de les classer pareil.
        sortie["arbres"] = {h["_id"]: h.get("_explanation") for h in hits}
        sortie["scores"] = {h["_id"]: h.get("_score") for h in hits}
        sortie["shard"] = all(h.get("_shard") is not None for h in hits)
        sortie["noms"] = None
    return sortie


def type_erreur(r):
    e = r.get("error")
    if isinstance(e, dict):
        return e.get("type")
    return None


# ---------------------------------------------------------------------------
# Comparer
# ---------------------------------------------------------------------------

def aplatit(noeud, chemin=""):
    """(chemin, valeur, arite, description) pour chaque noeud, en prefixe."""
    if noeud is None:
        return
    details = noeud.get("details") or []
    yield (chemin, noeud.get("value"), len(details), noeud.get("description"))
    for i, enfant in enumerate(details):
        yield from aplatit(enfant, f"{chemin}/{i}")


def proche(a, b, tol):
    if a is None or b is None:
        return a == b
    if a == b:
        return True
    ech = max(abs(a), abs(b))
    return abs(a - b) <= tol * ech if ech else True


def compare_arbre(g, d, tol):
    """Les ecarts entre deux arbres : (chemin, cote gauche, cote droit)."""
    pg = {c: (v, a, s) for c, v, a, s in aplatit(g)}
    pd = {c: (v, a, s) for c, v, a, s in aplatit(d)}
    ecarts = []
    for chemin in sorted(set(pg) | set(pd)):
        a, b = pg.get(chemin), pd.get(chemin)
        if a is None or b is None:
            ecarts.append((chemin, "absent" if a is None else a[2],
                           "absent" if b is None else b[2]))
        elif a[1] != b[1]:
            ecarts.append((chemin, f"{a[1]} enfants ({a[2]})",
                           f"{b[1]} enfants ({b[2]})"))
        elif not proche(a[0], b[0], tol):
            ecarts.append((chemin, a[0], b[0]))
    return ecarts


# Les divergences laissees passer, chacune avec son predicat ecrit. Aucune n'est
# un code d'etat tolere en bloc : ce sont des fonctions, qui regardent ce que
# les deux serveurs ont reellement rendu.
def divergence_bm25(ecarts, g, d):
    """L'ecart ne porte que sur les statistiques du BM25, et sur ce qu'elles
    entrainent.

    C'est la divergence declaree de `docs/compat.md` : Lucene calcule `N` et
    `avgdl` sur les documents **qui ont le champ**, tantivy sur tous. Le
    predicat ne se contente pas de le constater — il **recalcule** le score
    d'un cote avec les statistiques de l'autre, et n'accepte l'ecart que si le
    nombre retombe juste. Sans ca, il absorberait n'importe quel defaut de
    scoring."""
    if not ecarts:
        return False
    statistiques = {"N, total number of documents",
                    "N, total number of documents with field",
                    "avgdl, average length of field"}
    feuilles_g = {c: (v, s) for c, v, a, s in aplatit(g) if a == 0}
    feuilles_d = {c: (v, s) for c, v, a, s in aplatit(d) if a == 0}
    touche = False
    for chemin, _, _ in ecarts:
        sg, sd = feuilles_g.get(chemin), feuilles_d.get(chemin)
        if sg and sd and (sg[1] in statistiques or sd[1] in statistiques):
            touche = True
            continue
        # Un noeud interne (idf, tf, score, weight, sum of) : son ecart est
        # admis seulement si la formule le reproduit, ce que verifie
        # `verifie_formule` ci-dessous sur l'arbre entier.
        if chemin in feuilles_g and feuilles_g[chemin][1] not in statistiques:
            return False
    return touche and verifie_formule(g, d)


def statistiques_bm25(arbre):
    """Les cinq statistiques d'un noeud de scoring, par chemin de `weight`."""
    par_chemin = {}
    noeuds = {c: (v, a, s) for c, v, a, s in aplatit(arbre)}
    for chemin, (valeur, arite, desc) in noeuds.items():
        if not desc or not desc.startswith("score(freq="):
            continue
        stats = {"score": valeur}
        for c2, (v2, a2, s2) in noeuds.items():
            if not c2.startswith(chemin + "/") or a2 != 0 or not s2:
                continue
            for cle, motif in [("n", "n, number of documents"),
                               ("N", "N, total number of documents"),
                               ("freq", "freq, occurrences"),
                               ("dl", "dl, length of field"),
                               ("avgdl", "avgdl, average length"),
                               ("boost", "boost")]:
                if s2.startswith(motif):
                    stats.setdefault(cle, v2)
        par_chemin[chemin] = stats
    return par_chemin


def bm25(boost, n, N, freq, dl, avgdl, k1=1.2, b=0.75):
    idf = math.log(1 + (N - n + 0.5) / (n + 0.5))
    tf = freq / (freq + k1 * (1 - b + b * dl / avgdl))
    return boost * idf * tf


def verifie_formule(g, d):
    """Le score du cote gauche, recalcule avec les statistiques du cote droit,
    donne-t-il celui du cote droit ?

    C'est la preuve que l'ecart vient **entierement** des statistiques : si un
    autre terme de la formule differait, le nombre ne retomberait pas."""
    sg, sd = statistiques_bm25(g), statistiques_bm25(d)
    if not sg or set(sg) != set(sd):
        return False
    for chemin, a in sg.items():
        b_ = sd[chemin]
        if set(a) < {"n", "N", "freq", "dl", "avgdl", "boost"}:
            return False
        for stats, attendu in ((a, a["score"]), (b_, b_["score"])):
            calcule = bm25(stats["boost"], stats["n"], stats["N"],
                           stats["freq"], stats["dl"], stats["avgdl"])
            if not proche(calcule, attendu, 1e-5):
                return False
    return True


REFUS_ASSUMES = {
    "arbre: match_phrase": (
        "idf detaille",
        "ES detaille l'idf d'une phrase terme par terme, tantivy n'en garde "
        "que la somme : meme valeur, un niveau de moins"),
    "arbre: match_phrase_prefix": (
        "idf detaille",
        "meme chose, plus l'enveloppe booleenne que ferrite pose autour d'une "
        "phrase a prefixe"),
    "nom: nested nomme": (
        "nested",
        "le corpus n'a pas de champ nested : les deux serveurs refusent, mais "
        "avec des types d'erreur differents"),
    "nom: nom sur un objet (refus)": (
        "refus plus propre",
        "un `_name` qui n'est pas une valeur simple fait rendre **500** "
        "(`illegal_state_exception`) a ES ; ferrite rend 400 en nommant la "
        "faute — refuser mieux que la reference reste un ecart, il est ecrit",
        lambda g, d: (g.get("statut") == 400 and d.get("statut") == 500
                      and d.get("erreur") == "illegal_state_exception")),
    "arbre: bool avec boost": (
        "reecriture de Lucene",
        "un `bool` a une seule clause obligatoire est **reecrit** par Lucene "
        "en cette clause : ES explique le terme, ferrite garde le `sum of:` "
        "du booleen. Meme valeur, un niveau de plus"),
    "arbre: dis_max": (
        "ordre des branches",
        "ES ne rend pas les branches d'un `dis_max` dans l'ordre de la "
        "requete (mesure) ; ferrite les rend dans cet ordre, donc les arbres "
        "ne s'alignent pas branche a branche"),
    "arbre: boosting": (
        "forme de la clause",
        "ES construit `boosting` comme un `FunctionScoreQuery` et n'explique "
        "que la partie positive ; ferrite rend les deux, la positive et la "
        "negative quand elle a joue"),
    "route: bool a moitie satisfait": (
        "pourquoi pas",
        "ES reconstruit la raison Lucene de la non-correspondance ; ferrite "
        "rend `matched: false` et le dit, plutot que d'inventer une raison "
        "plausible (voir src/explain.rs)"),
}


def assume(libelle, g, d):
    """Un refus assume l'est pour **la** divergence ecrite, pas pour tout ce
    qui pourrait arriver sous ce libelle.

    Les sept divergences de `REFUS_ASSUMES` portent toutes sur la forme ou la
    valeur d'un arbre rendu en 200 des deux cotes. Un statut different n'en est
    pas une : la clause du libelle ne doit pas absorber un refus. C'est ce qui
    separe « 4/54 contre le binaire d'avant » de « 11/54 » — le libelle seul
    aurait laisse passer sept refus que ce binaire prononce."""
    entree = REFUS_ASSUMES.get(libelle)
    if entree is None:
        return False
    if len(entree) > 2:
        return entree[2](g, d)
    return (g.get("statut") == d.get("statut")
            and g.get("matched") == d.get("matched"))


def seau_java(nom, capacite):
    """Le seau d'une `HashMap` de Java : `String.hashCode`, l'etalement, le
    modulo. C'est l'ordre dans lequel ES rend `matched_queries` (mesure)."""
    h = 0
    for c in str(nom):
        h = (h * 31 + ord(c)) & 0xFFFFFFFF
    return ((h ^ (h >> 16)) & 0xFFFFFFFF) & (capacite - 1)


def capacite_java(n):
    cap = 16
    while n > 0.75 * cap:
        cap *= 2
    return cap


def meme_ordre_de_seaux(a, b):
    """Les deux listes ne different que **dans un seau**.

    ferrite reproduit l'ordre des seaux d'ES ; ce qu'il ne reproduit pas est
    l'ordre a l'interieur d'un seau, qui depend chez ES de l'historique des
    deux tables chainees qu'un nom traverse. Le predicat n'accepte donc que ca,
    et rien d'autre : meme ensemble de noms, et meme suite de seaux."""
    if sorted(a) != sorted(b):
        return False
    cap = capacite_java(len(a))
    return [seau_java(x, cap) for x in a] == [seau_java(x, cap) for x in b]


def compare_noms(g, d, tol):
    """(identiques, pourquoi non). Les noms sont compares dans leur ordre ; les
    scores de `include_named_queries_score` a la tolerance mesuree."""
    if g is None or d is None:
        return (g == d), "un cote ne rend rien"
    if sorted(g) != sorted(d):
        return False, "pas les memes documents"
    for doc in g:
        a, b_ = g[doc], d[doc]
        if a is None or b_ is None:
            if a != b_:
                return False, f"[{doc}] : un cote ne nomme rien"
            continue
        if list(a) != list(b_):
            if isinstance(a, list) and meme_ordre_de_seaux(a, b_):
                continue
            return False, f"[{doc}] : noms ou ordre"
        if isinstance(a, dict):
            for cle in a:
                if not proche(a[cle], b_[cle], tol):
                    return False, f"[{doc}] : score de [{cle}]"
    return True, ""


def abrege(x, n=110):
    s = json.dumps(x, ensure_ascii=False, sort_keys=True)
    return s if len(s) <= n else s[:n - 1] + "…"


def compare(libelle, g, d, tol, tol_noms=0.0):
    """(identiques, description de l'ecart, categorie)."""
    if g.get("statut") != d.get("statut"):
        return False, f"statuts {g.get('statut')} / {d.get('statut')}", "statut"
    if g.get("statut") != 200:
        # Les deux refusent : c'est le meme verdict, et le message est du texte.
        return True, "", "refus commun"
    if g.get("matched") != d.get("matched"):
        return False, f"matched {g.get('matched')} / {d.get('matched')}", "matched"
    if g.get("ids") != d.get("ids"):
        return False, f"documents {g.get('ids')} / {d.get('ids')}", "documents"
    ok, pourquoi = compare_noms(g.get("noms"), d.get("noms"), tol_noms)
    if not ok:
        return False, (f"matched_queries {pourquoi} : "
                       f"{abrege(g.get('noms'))} / {abrege(d.get('noms'))}"), "noms"

    arbres_g = g.get("arbres")
    arbres_d = d.get("arbres")
    if arbres_g is None and arbres_d is None:
        arbres_g = {"": g["arbre"]} if g.get("arbre") else {}
        arbres_d = {"": d["arbre"]} if d.get("arbre") else {}
    categorie = ""
    for doc in sorted(arbres_g):
        ag, ad = arbres_g[doc], arbres_d.get(doc)
        ecarts = compare_arbre(ag, ad, tol)
        if not ecarts:
            continue
        if divergence_bm25(ecarts, ag, ad):
            categorie = "bm25"
            continue
        chemin, a, b_ = ecarts[0]
        return False, (f"arbre de [{doc or 'la route'}], noeud "
                       f"[{chemin or 'racine'}] : {a} / {b_}"), "arbre"
    if g.get("shard") != d.get("shard"):
        return False, "_shard / _node absents d'un cote", "forme"
    return True, "", categorie


# ---------------------------------------------------------------------------
# `--ecart` : d'ou vient la difference de score
# ---------------------------------------------------------------------------

ECART_CAS = [
    ("match alpha (terme frequent)", {"match": {"titre": "alpha"}}),
    ("match beta", {"match": {"titre": "beta"}}),
    ("term gamma (terme rare)", {"term": {"titre": "gamma"}}),
    # Les deux `keyword`, et c'est leur opposition qui fait la demonstration :
    # `cat` est sur **tous** les documents, `ref` non. Le premier ne diverge
    # pas d'un bit, le second diverge — sur un type ou il n'y a ni analyzer,
    # ni frequence de terme, ni longueur de champ variable. Le seul terme qui
    # reste est le **denominateur** des statistiques de corpus.
    ("term sur keyword, champ que tous les documents ont", {"term": {"cat": "a"}}),
    ("term sur keyword, champ que deux documents n'ont pas",
     {"term": {"ref": "REF-1"}}),
    ("match sur corps", {"match": {"corps": "texte"}}),
]


def ecart(gauche, droite, noms):
    print("# D'ou vient l'ecart de `_score` entre les deux moteurs.")
    print("#")
    print("# Les cinq statistiques du BM25, cote a cote, puis le score que la")
    print("# formule `boost * idf * tf` rend avec chacune des deux series.")
    print("# La derniere colonne est la seule qui compte : si le score de")
    print("# chaque cote se recalcule a partir de **ses** statistiques, alors")
    print("# l'ecart ne vient que d'elles — c'est mesure, pas suppose.\n")
    pire = 0.0
    for libelle, requete in ECART_CAS:
        g = interroge(gauche, "explain", requete)
        d = interroge(droite, "explain", requete)
        if g.get("statut") != 200 or d.get("statut") != 200:
            print(f"{libelle} : refus ({g.get('statut')}/{d.get('statut')})")
            continue
        print(f"## {libelle}")
        print(f"   {'doc':6} {'cote':8} {'n':>4} {'N':>4} {'freq':>5} "
              f"{'dl':>6} {'avgdl':>9} {'score':>13} {'formule':>13}")
        for doc in g["ids"]:
            if doc not in d["ids"]:
                continue
            sg = statistiques_bm25(g["arbres"][doc])
            sd = statistiques_bm25(d["arbres"][doc])
            cles = sorted(set(sg) & set(sd))
            for chemin in cles:
                for cote, stats in ((noms[0], sg[chemin]), (noms[1], sd[chemin])):
                    if not {"n", "N", "freq", "dl", "avgdl"} <= set(stats):
                        continue
                    calcule = bm25(stats["boost"], stats["n"], stats["N"],
                                   stats["freq"], stats["dl"], stats["avgdl"])
                    print(f"   {doc:6} {cote:8} {stats['n']:>4g} "
                          f"{stats['N']:>4g} {stats['freq']:>5g} "
                          f"{stats['dl']:>6g} {stats['avgdl']:>9.5g} "
                          f"{stats['score']:>13.7g} {calcule:>13.7g}")
            if g["scores"][doc] and d["scores"][doc]:
                rel = abs(g["scores"][doc] - d["scores"][doc]) / max(
                    abs(g["scores"][doc]), abs(d["scores"][doc]))
                pire = max(pire, rel)
        print()
    print(f"pire ecart relatif de `_score` sur cette batterie : {pire:.3g}")
    return 0


# ---------------------------------------------------------------------------

def main():
    argv = [a for a in sys.argv[1:] if not a.startswith("--")]
    calibrer = "--calibrer" in sys.argv
    mode_ecart = "--ecart" in sys.argv
    gauche = argv[0] if argv else ("http://localhost:9201" if calibrer
                                   else "http://localhost:9200")
    droite = argv[1] if len(argv) > 1 else ("http://localhost:9202" if calibrer
                                            else "http://localhost:9201")
    noms = ("es_a", "es_b") if calibrer else ("ferrite", "es")
    for nom, base in zip(noms, (gauche, droite)):
        try:
            http(base, "GET", "/")
        except Exception as e:  # noqa: BLE001
            print(f"# {nom} indisponible ({base}) : {e}")
            print("# une sonde differentielle qui ne compare rien ne rend pas "
                  "de verdict : arret.")
            return 2
    for base in (gauche, droite):
        prepare(base)

    if mode_ecart:
        return ecart(gauche, droite, noms)

    # La tolerance sur la valeur d'un noeud. Elle n'est pas choisie : c'est
    # l'arrondi d'un `float`, que les deux serveurs serialisent chacun de leur
    # cote. Les ecarts que cette sonde cherche sont de plusieurs pour cent.
    tol = 2 ** -22

    # La tolerance sur le score d'une clause nommee, elle, se **mesure** : un
    # `_name` pose sur un `match` rend le score du BM25, et c'est justement ce
    # que les deux moteurs ne calculent pas pareil. Le temoin est la requete
    # nue, sans aucun nom — le meme geste que `sonde_score.py`.
    tg = interroge(gauche, "explain", ALPHA)
    td = interroge(droite, "explain", ALPHA)
    tol_noms = 0.0
    if tg.get("statut") == 200 and td.get("statut") == 200:
        for doc in set(tg["scores"]) & set(td["scores"]):
            a, b_ = tg["scores"][doc], td["scores"][doc]
            if a and b_:
                tol_noms = max(tol_noms, abs(a - b_) / max(abs(a), abs(b_)))
    print(f"# temoin : la requete nue (`match alpha`) diverge de "
          f"{tol_noms:.3g} en relatif entre les deux serveurs — c'est l'ecart "
          f"que\n#          le BM25 porte deja, et c'est la tolerance des "
          f"scores de `matched_queries`. Les arbres, eux,\n#          sont "
          f"compares au bit pres, et leur ecart est explique noeud par noeud "
          f"(voir --ecart).\n")

    batterie = cas()
    ecarts = assumes = 0
    par_categorie = {}
    for libelle, genre, charge in batterie:
        g = interroge(gauche, genre, charge)
        d = interroge(droite, genre, charge)
        pareils, detail, categorie = compare(libelle, g, d, tol, tol_noms)
        if pareils:
            if assume(libelle, g, d):
                # Une divergence declaree qui a disparu : ca se dit.
                print(f"  {libelle:44} identique (le refus assume ne s'applique "
                      f"plus)")
                continue
            par_categorie[categorie] = par_categorie.get(categorie, 0) + 1
            marque = " [bm25]" if categorie == "bm25" else ""
            print(f"  {libelle:44} {abrege(g.get('ids') or g.get('matched'), 60)}"
                  f"{marque}")
            continue
        if assume(libelle, g, d):
            assumes += 1
            print(f"~ {libelle:44} {detail}")
            print(f"      assume ({REFUS_ASSUMES[libelle][0]}) : "
                  f"{REFUS_ASSUMES[libelle][1]}")
            continue
        ecarts += 1
        print(f"* {libelle:44} {detail}")
        print(f"      {noms[0]}={abrege(g)}")
        print(f"      {noms[1]}={abrege(d)}")

    total = len(batterie)
    print(f"\n{total - ecarts - assumes}/{total} identiques, "
          f"{assumes} refus assumes, {ecarts} ecarts")
    if par_categorie.get("bm25"):
        print(f"dont {par_categorie['bm25']} arbres dont les valeurs different "
              f"par les seules statistiques du BM25 — l'ecart declare, et le "
              f"predicat\nne l'accepte qu'apres avoir recalcule les deux scores "
              f"a partir des deux series de statistiques (`--ecart` les "
              f"imprime).")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
