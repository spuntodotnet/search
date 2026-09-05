#!/usr/bin/env python3
"""Sonde : `search_after`, le point-in-time, et le tri total qu'ils exigent.

C'est la pagination que la 8.x recommande a la place du `scroll`, et elle ne
tient qu'a une chose : **un tri total**. `search_after` reprend « apres cette
cle de tri » ; si deux documents portent la meme cle, ES saute les ex aequo
sans un mot (mesure : `sort: [{"n":"asc"}]`, `search_after: [1]` sur un corpus
ou trois documents portent `n=1` rend directement les `n=2`). Un parcours bati
sur un tri non total perd donc des documents en 200 — la categorie que ce depot
refuse en premier.

Ce fichier pose la meme question aux deux serveurs et compare trois choses que
seule la troisieme rend utile :

- **les documents rendus dans leur ordre**, plus le tableau `sort` de chaque
  hit et le statut/la phrase d'un refus ;
- **le parcours complet** d'un index, page par page : memes documents, meme
  ordre, meme decoupe. Un parcours qui rend les bons documents dans le mauvais
  decoupage n'est pas le meme parcours ;
- **la totalite du tri** : que `_doc` et `_shard_doc` ne rendent jamais deux
  fois la meme cle. Elle ne se compare pas a ES — les deux moteurs n'ont pas
  les memes numeros de document internes — elle se **verifie** de chaque cote.
  Une egalite de valeurs y serait un test qu'aucun des deux ne peut passer ; la
  propriete, elle, est exactement ce dont `search_after` depend.

    python3 tests/compat/sonde_pagination.py [ferrite] [es]
    python3 tests/compat/sonde_pagination.py --calibrer [es_a] [es_b]

Elle **ecrit** (elle cree des index, ouvre et ferme des contextes) : elle ne
peut donc pas s'etalonner contre un seul serveur. Et elle **refuse de tourner**
si elle ne trouve pas ses deux cibles : une sonde differentielle qui ne compare
rien rendrait « tout identique ».

Le rapport imprime la **version** de chaque cible. Un etalonnage a deux serveurs
de la meme version prouve le determinisme, pas l'independance a la version
majeure.
"""
import json
import sys
import urllib.error
import urllib.request

MONO = "sonde-pagination"
A, B = "sonde-pagination-a", "sonde-pagination-b"
GROS = "sonde-pagination-gros"
INDEX = [MONO, A, B, GROS]

# La taille du corpus du parcours complet. La carte demandait 50 000 documents ;
# la sonde en pose 5 000 par defaut, et `--parcours N` en pose autant qu'on veut
# — c'est la meme mesure, et le critere d'acceptation se joue avec `--parcours
# 50000`. Ce qui compte n'est pas le nombre mais qu'il depasse largement
# `max_result_window` en nombre de **pages**, et qu'il tienne sur plusieurs
# segments des deux cotes.
PARCOURS_DEFAUT = 5000


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
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        corps = e.read()
        try:
            return e.code, json.loads(corps or b"{}")
        except json.JSONDecodeError:
            return e.code, {"raw": corps.decode(errors="replace")}


def version(base):
    _, corps = http(base, "GET", "/")
    return corps.get("version", {}).get("number", "?")


# Le corpus mono-index. `n` porte des ex aequo **exprès** : c'est la seule forme
# ou le saut d'un `search_after` sur un tri non total se voit. `u` est unique,
# c'est le tri total ecrit a la main.
DOCS = {}
for _i in range(12):
    DOCS[f"d{_i:02d}"] = {"n": _i // 3, "u": _i, "k": f"k{_i % 4}",
                          "t": f"2026-01-{_i + 1:02d}T00:00:00Z"}
DOCS["dzz"] = {}          # aucun champ : il exerce la sentinelle de `missing`
DOCS["dyy"] = {"n": 99, "u": 99, "k": "kz", "t": "2027-01-01T00:00:00Z"}

MAPPING = {"mappings": {"properties": {
    "n": {"type": "long"}, "u": {"type": "long"},
    "k": {"type": "keyword"}, "t": {"type": "date"}}}}


def bulk(base, docs, refresh=True):
    lignes = []
    for index, id_, doc in docs:
        lignes.append(json.dumps({"index": {"_index": index, "_id": id_}}))
        lignes.append(json.dumps(doc))
    suffixe = "?refresh=true" if refresh else ""
    http(base, "POST", "/_bulk" + suffixe, "\n".join(lignes) + "\n", ndjson=True)


def prepare(base, taille):
    for idx in INDEX:
        http(base, "DELETE", "/" + idx)
    fermer_tout(base)
    for idx in (MONO, A, B, GROS):
        http(base, "PUT", "/" + idx, MAPPING)
    # Le corpus est pose en **plusieurs lots rafraichis** : sans ca, tout tombe
    # dans un segment unique des deux cotes, et la totalite de `_doc` — le seul
    # endroit ou ferrite s'est trompe — ne mesure plus rien. Un numero de
    # document local a son segment est unique tant qu'il n'y a qu'un segment.
    items = list(DOCS.items())
    for debut in range(0, len(items), 4):
        bulk(base, [(MONO, id_, doc) for id_, doc in items[debut:debut + 4]])
    bulk(base, [(A, "a1", {"n": 1, "u": 1, "k": "aa"}),
                (A, "a2", {"n": 2, "u": 2, "k": "bb"})])
    bulk(base, [(B, "b1", {"n": 1, "u": 3, "k": "cc"}),
                (B, "b2", {"n": 3, "u": 4, "k": "dd"})])
    # Le corpus du parcours, lui aussi en plusieurs lots.
    lot = []
    for i in range(taille):
        lot.append((GROS, f"g{i:06d}", {"n": i % 7, "u": i, "k": f"k{i % 13}"}))
        if len(lot) == 1000:
            bulk(base, lot)
            lot = []
    if lot:
        bulk(base, lot)


def fermer_tout(base):
    """Un contexte retient un instantane de l'index : une campagne qui en laisse
    derriere elle mesure le serveur d'avant. On ne peut pas enumerer les PIT
    ouverts (ni chez ES, ni ici) — le nettoyage se fait donc par identifiant, et
    chaque cas ferme ce qu'il ouvre."""
    http(base, "DELETE", "/_search/scroll", {"scroll_id": ["_all"]})


# ---------------------------------------------------------------------------
# Ce qui se compare
# ---------------------------------------------------------------------------

def resume_erreur(st, corps):
    err = corps.get("error", {})
    # Toutes les erreurs ne sont pas une enveloppe : un 405 rend `error` en
    # **chaine** chez ES, et une reponse non-JSON n'en a pas du tout.
    if not isinstance(err, dict):
        return f"{st} {err}"
    cause = err
    if err.get("root_cause"):
        cause = err["root_cause"][0]
    elif err.get("caused_by"):
        cause = err["caused_by"]
    return f"{st} {cause.get('type')} : {cause.get('reason')}"


def resume_recherche(st, corps, masque_queue=0):
    """Ce qu'un client lit : les documents rendus **dans leur ordre**, leur
    tableau `sort`, le total, et la presence d'un `pit_id`.

    `masque_queue` retire les N dernieres valeurs du tableau `sort`. Il ne sert
    qu'a une chose, et elle est declaree : le departage implicite d'un PIT est
    un **numero interne de document**, et les deux moteurs n'ont pas la meme
    numerotation. ES documente lui-meme cet ordre comme depourvu de sens
    (`_doc` « has no real use-case besides being the most efficient sort
    order ») ; ce qui compte est qu'il existe, qu'il soit unique et qu'il ne
    change pas — ce que verifient les cas `propriete :` et les parcours. Le
    masque ne cache donc pas un ecart, il enleve de la comparaison la seule
    valeur dont ES ne promet rien."""
    if st != 200:
        return resume_erreur(st, corps)
    hits = []
    for h in corps["hits"]["hits"]:
        cle = tuple(h["sort"][:len(h["sort"]) - masque_queue]) if "sort" in h else None
        hits.append((h["_index"], h["_id"]) + ((cle,) if cle is not None else ()))
    return json.dumps([hits, corps["hits"]["total"]["value"],
                       "pit_id" in corps])


def cas_search_after():
    """(libelle, index, corps). Sans PIT : ce que `search_after` fait tout seul.

    Trois familles, et la premiere est celle qui decide du reste : ES **ne se
    plaint pas** d'un tri non total. Il saute les ex aequo, en 200. La carte
    supposait l'inverse ; c'est la mesure qui tranche, et ferrite doit sauter
    pareil."""
    out = []
    base = {"size": 4, "_source": False}

    # Le saut sur les ex aequo, dans les deux sens et sur les quatre types.
    for sens in ("asc", "desc"):
        for champ, apres in [("n", 1), ("u", 5), ("k", "k1"),
                             ("t", "2026-01-05T00:00:00Z")]:
            out.append((f"search_after {champ} {sens} apres {apres!r}", MONO,
                        {**base, "sort": [{champ: sens}], "search_after": [apres]}))
    # Le tri total ecrit a la main : champ non unique **puis** champ unique.
    out.append(("search_after n asc + u asc (tri total)", MONO,
                {**base, "sort": [{"n": "asc"}, {"u": "asc"}],
                 "search_after": [1, 4]}))
    out.append(("search_after n desc + u desc", MONO,
                {**base, "sort": [{"n": "desc"}, {"u": "desc"}],
                 "search_after": [2, 7]}))
    out.append(("search_after _score + n", MONO,
                {**base, "sort": [{"_score": "desc"}, {"n": "asc"}],
                 "query": {"match_all": {}}, "search_after": [1.0, 1]}))

    # La sentinelle d'une valeur absente : `dzz` n'a aucun champ.
    for m in ("_first", "_last"):
        out.append((f"search_after n asc missing={m} apres 3", MONO,
                    {**base, "sort": [{"n": {"order": "asc", "missing": m}}],
                     "search_after": [3]}))
    out.append(("search_after k asc apres null", MONO,
                {**base, "sort": [{"k": "asc"}], "search_after": [None]}))
    out.append(("search_after n asc apres la sentinelle", MONO,
                {**base, "sort": [{"n": "asc"}],
                 "search_after": [9223372036854775807]}))

    # La coercition de la valeur. Elle se lit **au type du champ**, comme celle
    # de `missing` — et un flottant sur un `long` se **tronque vers zero**
    # (2,7 et 2,2 rendent le meme resultat, mesure).
    for v in ["3", 3.7, 2.7, 2.2, -0.5, "abc", True, [1], {}, 1e300]:
        out.append((f"search_after n asc = {v!r}", MONO,
                    {**base, "sort": [{"n": "asc"}], "search_after": [v]}))
    for v in [5, "k1", True, None]:
        out.append((f"search_after k asc = {v!r}", MONO,
                    {**base, "sort": [{"k": "asc"}], "search_after": [v]}))
    # Une date de reprise lit la **grammaire d'une borne**, date math comprise
    # — sauf `now`, qu'ES refuse ici (sa clé de tri se construit sans horloge).
    for v in ["2026-01-05T00:00:00Z", "2026-01-05", 1767571200000,
              "1767571200000", "pas-une-date", "2026-01-05||+1d",
              "2026-01", "now", "now-1d", True]:
        out.append((f"search_after t asc = {v!r}", MONO,
                    {**base, "sort": [{"t": "asc"}], "search_after": [v]}))

    # Les refus. Aucun ne se devine, et trois d'entre eux ne sont pas des
    # `illegal_argument_exception`.
    out.append(("search_after sans sort", MONO, {**base, "search_after": [1]}))
    out.append(("search_after avec sort vide", MONO,
                {**base, "sort": [], "search_after": [1]}))
    out.append(("search_after avec _score seul", MONO,
                {**base, "sort": [{"_score": "desc"}], "search_after": [1.0]}))
    out.append(("search_after trop de valeurs", MONO,
                {**base, "sort": [{"n": "asc"}], "search_after": [1, 2]}))
    out.append(("search_after pas assez de valeurs", MONO,
                {**base, "sort": [{"n": "asc"}, {"u": "asc"}], "search_after": [1]}))
    out.append(("search_after tableau vide", MONO,
                {**base, "sort": [{"n": "asc"}], "search_after": []}))
    out.append(("search_after pas un tableau", MONO,
                {**base, "sort": [{"n": "asc"}], "search_after": 3}))
    out.append(("search_after null nu", MONO,
                {**base, "sort": [{"n": "asc"}], "search_after": None}))
    out.append(("search_after + from", MONO,
                {**base, "from": 2, "sort": [{"n": "asc"}], "search_after": [1]}))
    out.append(("search_after + from: 0 explicite", MONO,
                {**base, "from": 0, "sort": [{"n": "asc"}], "search_after": [1]}))
    out.append(("search_after + size au-dela de la fenetre", MONO,
                {**base, "size": 10001, "sort": [{"n": "asc"}], "search_after": [1]}))

    # `_doc` est une cle de tri comme une autre pour `search_after` : elle
    # compte comme « au moins un champ », contrairement a `_score`.
    out.append(("search_after sur _doc", MONO,
                {**base, "sort": ["_doc"], "search_after": [2]}))
    out.append(("search_after sur _shard_doc sans pit", MONO,
                {**base, "sort": ["_shard_doc"], "search_after": [2]}))
    out.append(("sort _shard_doc sans pit", MONO, {**base, "sort": ["_shard_doc"]}))
    out.append(("sort _shard_doc desc sans pit", MONO,
                {**base, "sort": [{"_shard_doc": "desc"}]}))

    # Multi-index : la cle de tri se compare entre index, comme partout.
    out.append(("multi : search_after n asc apres 1", f"{A},{B}",
                {**base, "sort": [{"n": "asc"}], "search_after": [1]}))
    out.append(("multi : search_after u asc apres 2", f"{A},{B}",
                {**base, "sort": [{"u": "asc"}], "search_after": [2]}))

    # Les agregations ne voient pas `search_after` : il ne coupe que les hits.
    out.append(("search_after + aggs", MONO,
                {**base, "sort": [{"n": "asc"}], "search_after": [1],
                 "aggs": {"c": {"value_count": {"field": "n"}}}}))
    # Le total non plus.
    out.append(("search_after + track_total_hits", MONO,
                {**base, "sort": [{"n": "asc"}], "search_after": [1],
                 "track_total_hits": True}))
    return out


def cas_scroll_et_pit():
    """Ce qui ne peut pas cohabiter, et les phrases exactes des refus."""
    return [
        ("search_after + ?scroll", MONO, {"size": 3, "sort": [{"n": "asc"}],
                                          "search_after": [1]}, "?scroll=1m"),
        ("sort seul + ?scroll", MONO, {"size": 3, "sort": [{"n": "asc"}]}, "?scroll=1m"),
    ]


# ---------------------------------------------------------------------------
# Le point-in-time
# ---------------------------------------------------------------------------

def cas_pit_ouverture():
    """(libelle, methode, chemin, corps) — ce que `_pit` accepte a l'ouverture.

    Ce qui se compare est le statut et la **forme** de la reponse (la presence
    d'un `id`), jamais l'identifiant lui-meme : il est opaque des deux cotes, et
    ES y encode ses shards."""
    return [
        ("pit sans keep_alive", "POST", f"/{MONO}/_pit", None),
        ("pit keep_alive vide", "POST", f"/{MONO}/_pit?keep_alive=", None),
        ("pit keep_alive sans unite", "POST", f"/{MONO}/_pit?keep_alive=1", None),
        ("pit keep_alive trop long", "POST", f"/{MONO}/_pit?keep_alive=2d", None),
        ("pit keep_alive 1d", "POST", f"/{MONO}/_pit?keep_alive=1d", None),
        ("pit sur index absent", "POST", "/pas-la/_pit?keep_alive=1m", None),
        ("pit sur motif", "POST", "/sonde-pagination-*/_pit?keep_alive=1m", None),
        ("pit sur liste", "POST", f"/{A},{B}/_pit?keep_alive=1m", None),
        ("pit sur _all", "POST", "/_all/_pit?keep_alive=1m", None),
        ("pit sans index", "POST", "/_pit?keep_alive=1m", None),
        ("pit en GET", "GET", f"/{MONO}/_pit?keep_alive=1m", None),
        ("pit parametre inconnu", "POST", f"/{MONO}/_pit?keep_alive=1m&nawak=1", None),
        ("pit expand_wildcards", "POST",
         f"/{MONO}/_pit?keep_alive=1m&expand_wildcards=all&ignore_unavailable=true", None),
        ("pit corps inconnu", "POST", f"/{MONO}/_pit?keep_alive=1m", {"nawak": 1}),
        ("close sans corps", "DELETE", "/_pit", None),
        ("close id inconnu", "DELETE", "/_pit", {"id": "pas-un-id"}),
        ("close corps sans id", "DELETE", "/_pit", {"nawak": 1}),
        ("close id en liste", "DELETE", "/_pit", {"id": ["a", "b"]}),
    ]


def resume_ouverture(st, corps):
    if isinstance(corps, dict) and "succeeded" in corps:
        return json.dumps([st, "corps", corps.get("succeeded"), corps.get("num_freed")])
    if st != 200:
        return resume_erreur(st, corps)
    if "id" in corps:
        return json.dumps(["id", sorted(corps.keys())])
    return json.dumps(["corps", sorted(corps.keys()),
                       corps.get("succeeded"), corps.get("num_freed")])


def cas_pit_recherche():
    """(libelle, corps, masque) — une recherche **sous** un PIT.

    L'identifiant est injecte par l'appelant : il n'est pas comparable entre
    serveurs. `masque` dit combien de valeurs de queue du tableau `sort` sont
    un **numero interne de document** — la seule chose de cette API dont ES ne
    promette rien, et dont les deux moteurs n'ont donc pas la meme valeur."""
    return [
        ("pit + sort n asc (tiebreak implicite)", {"size": 3, "_source": False,
                                                   "sort": [{"n": "asc"}]}, 1),
        ("pit + sort u asc", {"size": 3, "_source": False, "sort": [{"u": "asc"}]}, 1),
        ("pit + n asc + _shard_doc explicite", {"size": 3, "_source": False,
                                                "sort": [{"n": "asc"}, "_shard_doc"]}, 1),
        ("pit + aggs", {"size": 0, "aggs": {"c": {"value_count": {"field": "n"}}}}, 0),
        ("pit + query", {"size": 3, "_source": False, "sort": [{"u": "asc"}],
                         "query": {"range": {"n": {"gte": 2}}}}, 1),
        ("pit + from", {"size": 3, "from": 2, "_source": False, "sort": [{"u": "asc"}]}, 1),
        ("pit + keep_alive dans le corps", {"size": 2, "_source": False,
                                            "sort": [{"u": "asc"}],
                                            "_pit_keep_alive": "3m"}, 1),
        ("pit + cle inconnue", {"size": 2, "_source": False, "sort": [{"u": "asc"}],
                                "_pit_extra": {"nawak": 1}}, 0),
        # `t` est unique et `k` ne l'est pas : le second exerce le departage
        # implicite sur un vrai paquet d'ex aequo.
        ("pit + sort t desc", {"size": 3, "_source": False, "sort": [{"t": "desc"}]}, 1),
        ("pit + sort k asc", {"size": 5, "_source": False, "sort": [{"k": "asc"}]}, 1),
        ("pit + search_after apres u=4", {"size": 3, "_source": False,
                                          "sort": [{"u": "asc"}, "_shard_doc"],
                                          "search_after": [4, 0]}, 1),
    ]


def cas_pit_ordre_interne():
    """Les recherches dont **l'ordre lui-meme** vient du numero interne.

    Elles ne se comparent pas a ES, et le dire est plus honnete que de masquer
    une colonne : quand le tri est `_doc`, `_shard_doc`, ou un score constant,
    il ne reste rien d'autre pour departager, et les deux moteurs ne numerotent
    pas dans le meme ordre (chez tantivy, l'ordre des documents n'est deja pas
    l'ordre d'ecriture — c'est un ecart anterieur, declare dans compat.yaml).

    Ce qu'on verifie alors est ce dont `search_after` a besoin, et rien de
    moins : meme nombre de hits, meme total, et des cles **strictement
    croissantes et uniques**. Le parcours complet, lui, prouve qu'aucun
    document n'est saute ni repete."""
    return [
        ("pit sans sort", {"size": 3, "_source": False}),
        ("pit + _shard_doc", {"size": 3, "_source": False, "sort": ["_shard_doc"]}),
        ("pit + _shard_doc desc", {"size": 3, "_source": False,
                                   "sort": [{"_shard_doc": "desc"}]}),
        ("pit + _doc", {"size": 3, "_source": False, "sort": ["_doc"]}),
        ("pit + _score", {"size": 3, "_source": False, "sort": [{"_score": "desc"}]}),
        ("pit + search_after sur _shard_doc", {"size": 3, "_source": False,
                                               "sort": ["_shard_doc"],
                                               "search_after": [2]}),
    ]


def verdict_ordre_interne(st, corps):
    """Le predicat de la classe ci-dessus, ecrit plutot que suppose."""
    if st != 200:
        return resume_erreur(st, corps)
    hits = corps["hits"]["hits"]
    cles = [tuple(h["sort"]) for h in hits] if all("sort" in h for h in hits) else None
    croissant = cles is None or (cles == sorted(cles) and len(set(cles)) == len(cles))
    return json.dumps([len(hits), corps["hits"]["total"]["value"],
                       "pit_id" in corps, croissant, cles is not None])


def cas_pit_forme():
    """Les formes du bloc `pit` que le corps peut porter — sans identifiant
    valide, donc comparables telles quelles entre serveurs."""
    return [
        ("pit sans id", {"size": 2, "pit": {"keep_alive": "1m"}, "sort": [{"u": "asc"}]}),
        ("pit id bidon", {"size": 2, "pit": {"id": "pas-un-id"}, "sort": [{"u": "asc"}]}),
        ("pit chaine nue", {"size": 2, "pit": "abc", "sort": [{"u": "asc"}]}),
        ("pit null", {"size": 2, "pit": None, "sort": [{"u": "asc"}]}),
        ("pit id null", {"size": 2, "pit": {"id": None}, "sort": [{"u": "asc"}]}),
        ("pit id nombre", {"size": 2, "pit": {"id": 3}, "sort": [{"u": "asc"}]}),
    ]


def avec_pit(corps, pid):
    """Injecte l'identifiant dans le corps, en respectant les deux marqueurs que
    la batterie utilise pour demander une forme particuliere."""
    corps = dict(corps)
    bloc = {"id": pid}
    if "_pit_keep_alive" in corps:
        bloc["keep_alive"] = corps.pop("_pit_keep_alive")
    if "_pit_extra" in corps:
        bloc.update(corps.pop("_pit_extra"))
    corps["pit"] = bloc
    return corps


# ---------------------------------------------------------------------------
# Les proprietes : ce qui ne se compare pas a ES, mais se verifie de chaque cote
# ---------------------------------------------------------------------------

def propriete_tri_total(base, sous_pit):
    """`_doc` (et `_shard_doc` sous un PIT) rendent-ils une cle **unique** par
    document ?

    C'est la propriete dont depend tout `search_after`, et c'est exactement la
    ou ferrite se trompait : son numero de document etait celui du **segment**,
    pas celui de l'index, donc deux documents de deux segments rendaient tous
    les deux `[0]`. Un `search_after` bati sur ce tri-la boucle ou saute.

    Elle ne se compare pas a ES : les deux moteurs n'ont pas les memes numeros
    internes, et un test d'egalite serait un test qu'aucun des deux ne peut
    passer. La propriete, elle, est ce dont `search_after` a besoin."""
    corps = {"size": 100, "_source": False,
             "sort": ["_shard_doc" if sous_pit else "_doc"]}
    pid = None
    if sous_pit:
        st, ouv = http(base, "POST", f"/{MONO}/_pit?keep_alive=1m")
        if st != 200:
            return f"pit refuse : {resume_erreur(st, ouv)}"
        pid = ouv["id"]
        corps["pit"] = {"id": pid}
        st, r = http(base, "POST", "/_search", corps)
    else:
        st, r = http(base, "POST", f"/{MONO}/_search", corps)
    try:
        if st != 200:
            return resume_erreur(st, r)
        cles = [tuple(h["sort"]) for h in r["hits"]["hits"]]
        n = len(cles)
        uniques = len(set(cles))
        croissant = cles == sorted(cles)
        return json.dumps([n, uniques == n, croissant])
    finally:
        if pid:
            http(base, "DELETE", "/_pit", {"id": pid})


def propriete_pit_fige(base):
    """Un PIT retient-il vraiment une vue ?

    Un identifiant qu'on rend sans rien retenir serait le pire des deux mondes :
    le client croit paginer sur une vue stable et lit un index qui bouge. La
    mesure est directe — ouvrir, ecrire, relire sous le PIT."""
    st, ouv = http(base, "POST", f"/{GROS}/_pit?keep_alive=2m")
    if st != 200:
        return resume_erreur(st, ouv)
    pid = ouv["id"]
    try:
        _, avant = http(base, "POST", "/_search",
                        {"size": 0, "pit": {"id": pid}, "track_total_hits": True})
        avant = avant["hits"]["total"]["value"]
        bulk(base, [(GROS, "intrus-1", {"n": 4, "u": -1, "k": "intrus"}),
                    (GROS, "intrus-2", {"n": 4, "u": -2, "k": "intrus"})])
        _, apres = http(base, "POST", "/_search",
                        {"size": 0, "pit": {"id": pid}, "track_total_hits": True})
        apres = apres["hits"]["total"]["value"]
        _, hors = http(base, "POST", f"/{GROS}/_search",
                       {"size": 0, "track_total_hits": True})
        hors = hors["hits"]["total"]["value"]
        # Ce qui se compare : le PIT n'a pas bouge, et le serveur, si.
        return json.dumps([avant == apres, hors == avant + 2])
    finally:
        http(base, "DELETE", "/_pit", {"id": pid})
        http(base, "POST", f"/{GROS}/_delete_by_query?refresh=true",
             {"query": {"term": {"k": "intrus"}}})


def propriete_pit_cycle(base):
    """Ouvrir, fermer, puis s'en servir : ce que rend un identifiant perime.

    Un PIT ferme doit rendre le meme 404 qu'un scroll expire — c'est ce qu'un
    client reconnait pour dire « recommence », plutot que « ta requete est
    invalide »."""
    st, ouv = http(base, "POST", f"/{MONO}/_pit?keep_alive=1m")
    if st != 200:
        return resume_erreur(st, ouv)
    pid = ouv["id"]
    st1, c1 = http(base, "DELETE", "/_pit", {"id": pid})
    st2, c2 = http(base, "DELETE", "/_pit", {"id": pid})
    st3, r3 = http(base, "POST", "/_search",
                   {"size": 2, "pit": {"id": pid}, "sort": [{"u": "asc"}]})
    type3 = (r3.get("error", {}).get("root_cause") or [{}])[0].get("type")
    return json.dumps([
        [st1, c1.get("succeeded"), c1.get("num_freed")],
        [st2, c2.get("succeeded"), c2.get("num_freed")],
        [st3, type3],
    ])


def propriete_pit_deux_ouvertures(base):
    """Deux ouvertures rendent-elles deux contextes distincts ?"""
    st1, a = http(base, "POST", f"/{MONO}/_pit?keep_alive=1m")
    st2, b = http(base, "POST", f"/{MONO}/_pit?keep_alive=1m")
    if st1 != 200 or st2 != 200:
        return resume_erreur(st1 if st1 != 200 else st2, a if st1 != 200 else b)
    distincts = a["id"] != b["id"]
    # Fermer l'un ne doit pas fermer l'autre.
    http(base, "DELETE", "/_pit", {"id": a["id"]})
    st, _ = http(base, "POST", "/_search",
                 {"size": 1, "pit": {"id": b["id"]}, "sort": [{"u": "asc"}]})
    http(base, "DELETE", "/_pit", {"id": b["id"]})
    return json.dumps([distincts, st])


def propriete_pit_renvoye(base):
    """La reponse porte-t-elle le `pit_id` a rendre au coup suivant, et est-ce
    le meme ? (ES le renvoie a chaque page : un client bien ecrit repart de
    celui-la, pas de celui qu'il a ouvert.)"""
    st, ouv = http(base, "POST", f"/{MONO}/_pit?keep_alive=1m")
    if st != 200:
        return resume_erreur(st, ouv)
    pid = ouv["id"]
    try:
        st, r = http(base, "POST", "/_search",
                     {"size": 2, "_source": False, "pit": {"id": pid},
                      "sort": [{"u": "asc"}]})
        if st != 200:
            return resume_erreur(st, r)
        return json.dumps(["pit_id" in r, r.get("pit_id") == pid])
    finally:
        http(base, "DELETE", "/_pit", {"id": pid})


def propriete_pit_index_dans_url(base):
    """Une recherche sous un PIT ne prend pas d'index dans l'URL : l'index vient
    du PIT, et en nommer un serait deux sources de verite pour une seule
    question."""
    st, ouv = http(base, "POST", f"/{MONO}/_pit?keep_alive=1m")
    if st != 200:
        return resume_erreur(st, ouv)
    pid = ouv["id"]
    try:
        a, ra = http(base, "POST", f"/{MONO}/_search",
                     {"size": 1, "pit": {"id": pid}, "sort": [{"u": "asc"}]})
        b, rb = http(base, "POST", "/_all/_search",
                     {"size": 1, "pit": {"id": pid}, "sort": [{"u": "asc"}]})
        c, rc = http(base, "POST", "/_search?scroll=1m",
                     {"size": 1, "pit": {"id": pid}, "sort": [{"u": "asc"}]})
        return json.dumps([resume_erreur(a, ra) if a != 200 else "200",
                           resume_erreur(b, rb) if b != 200 else "200",
                           resume_erreur(c, rc) if c != 200 else "200"])
    finally:
        http(base, "DELETE", "/_pit", {"id": pid})


# ---------------------------------------------------------------------------
# Le parcours complet — le critere d'acceptation de la carte
# ---------------------------------------------------------------------------

def parcourir(base, taille_page, sous_pit, sort, limite=None):
    """Parcourt `GROS` de bout en bout par `search_after`, et rend la liste des
    pages (chaque page etant la liste de ses `_id`).

    C'est la decoupe qui compte autant que l'ordre : deux serveurs qui rendent
    les memes documents dans le meme ordre mais coupes autrement n'ont pas fait
    le meme parcours — et c'est exactement ce qu'une cle de tri non unique
    produit."""
    pages = []
    apres = None
    pid = None
    if sous_pit:
        st, ouv = http(base, "POST", f"/{GROS}/_pit?keep_alive=5m")
        if st != 200:
            return resume_erreur(st, ouv)
        pid = ouv["id"]
    try:
        while True:
            corps = {"size": taille_page, "_source": False, "sort": sort}
            if apres is not None:
                corps["search_after"] = apres
            if pid:
                corps["pit"] = {"id": pid, "keep_alive": "5m"}
                st, r = http(base, "POST", "/_search", corps)
            else:
                st, r = http(base, "POST", f"/{GROS}/_search", corps)
            if st != 200:
                return resume_erreur(st, r)
            hits = r["hits"]["hits"]
            if not hits:
                break
            pages.append([h["_id"] for h in hits])
            apres = hits[-1]["sort"]
            if pid and r.get("pit_id"):
                pid = r["pit_id"]
            if limite and len(pages) >= limite:
                break
        return pages
    finally:
        if pid:
            http(base, "DELETE", "/_pit", {"id": pid})


def invariants_parcours(pages, taille):
    """Le meme resume, **prive de ce qui depend du numero interne** : ce qui
    reste est exactement ce dont un export a besoin — toutes les pages, tous
    les documents, aucun saute, aucun repete."""
    r = resume_parcours(pages, taille)
    if not r.startswith("{"):
        return r
    d = json.loads(r)
    d.pop("premier", None)
    d.pop("dernier", None)
    return json.dumps(d, sort_keys=True)


def resume_parcours(pages, taille):
    """Ce qui se publie d'un parcours : le nombre de pages, la taille de
    chacune (resumee), le nombre de documents vus, et surtout **combien ont ete
    vus deux fois ou pas du tout**. Un parcours qui repete est aussi faux qu'un
    parcours qui saute, et les deux rendent 200."""
    if isinstance(pages, str):
        return pages
    plats = [d for page in pages for d in page]
    attendus = {f"g{i:06d}" for i in range(taille)}
    vus = set(plats)
    tailles = sorted({len(p) for p in pages})
    return json.dumps({
        "pages": len(pages),
        "tailles": tailles,
        "documents": len(plats),
        "distincts": len(vus),
        "manquants": len(attendus - vus),
        "en_trop": len(vus - attendus),
        "repetes": len(plats) - len(vus),
        "premier": plats[0] if plats else None,
        "dernier": plats[-1] if plats else None,
    }, sort_keys=True)


# ---------------------------------------------------------------------------
# Les ecarts assumes
# ---------------------------------------------------------------------------

REFUS_ASSUMES = {
    "search_after null nu": ("type",
        "ES rend `parsing_exception` avec la position dans le JSON brut "
        "(`[1:52]`), que ferrite n'a plus une fois le corps parse ; c'est la "
        "meme divergence deja declaree sur les messages de parsing"),
    "search_after pas un tableau": ("type",
        "meme cause : ES cite la ligne et la colonne du JSON brut"),
    "search_after n asc = [1]": ("type",
        "ES nomme le jeton JavaCC trouve (`[START_ARRAY]`) ; ferrite dit que la "
        "valeur n'est pas un scalaire, sans le vocabulaire de son parseur"),
    "search_after n asc = {}": ("type",
        "meme cause que la ligne precedente, sur l'objet"),
    "pit corps inconnu": ("type",
        "ES rend `x_content_parse_exception [1:2] [open_point_in_time_request] "
        "unknown field [nawak]` ; ferrite refuse la cle en la nommant, sans la "
        "position dans le JSON brut"),
    "pit id bidon": ("id_opaque",
        "ES decode l'identifiant en base64 (il y encode ses shards) et rend "
        "`400 x_content_parse_exception [pit] failed to parse field [id]` ; "
        "celui de ferrite est un uuid **opaque** qui ne porte rien a decoder, "
        "donc un identifiant illisible y est un contexte introuvable — le 404 "
        "de `search_context_missing_exception`, exactement ce qu'ES rend pour "
        "un identifiant bien forme mais expire, et ce qu'un client lit pour "
        "dire « recommence »"),
    "close id inconnu": ("id_opaque",
        "meme cause : ES echoue au decodage base64 (`Last unit does not have "
        "enough valid bits`, en 400), ferrite ne trouve pas le contexte et rend "
        "le 404 + `{succeeded: false, num_freed: 0}` qu'ES rend, lui, pour un "
        "identifiant **bien forme** mais expire"),
    "close corps sans id": ("type",
        "ES rend `x_content_parse_exception ... unknown field [nawak]`, ferrite "
        "nomme la cle obligatoire manquante"),
    "close id en liste": ("type",
        "ES rend `the request must contain only [id field` (sa phrase, sa "
        "coquille comprise) ; ferrite refuse le type de la valeur"),
    "pit + cle inconnue": ("type",
        "ES rend `x_content_parse_exception [1:245] [pit] unknown field "
        "[nawak]` ; ferrite refuse la cle en la nommant, sans la position dans "
        "le JSON brut"),
    "pit chaine nue": ("type",
        "position dans le JSON brut, encore"),
    "pit id nombre": ("type", "position dans le JSON brut, encore"),
    "pit id null": ("type", "position dans le JSON brut, encore"),
    "search_after n asc missing=_first apres 3": ("ecart_es",
        "ES rend **500** (`null_pointer_exception`) des qu'un `search_after` "
        "croise une sentinelle de valeur absente sur un type numerique : "
        "`Cannot invoke \"java.lang.Long.longValue()\" because \"value\" is "
        "null`. C'est un defaut d'ES, mesure ; ferrite rend le resultat que la "
        "regle de tri implique. Reproduire un 500 serait servir une panne sous "
        "le nom de la compatibilite"),
    "search_after k asc apres null": ("ecart_es",
        "meme famille : sur un `keyword`, la sentinelle **est** `null` dans le "
        "tableau `sort`, donc reprendre apres elle est le cas normal d'un "
        "parcours. ES y rend zero document quel que soit le sens du tri ; "
        "ferrite rend ce que la sentinelle implique — les documents places "
        "apres elle. La divergence est declaree et chiffree dans compat.yaml"),
    "search_after k asc = None": ("ecart_es", "le meme cas, ecrit autrement"),
}


def sans_le_type(vu):
    """La reponse privee du **nom** du type d'erreur — et de lui seul.

    Le predicat garde le statut et la phrase reduite a sa forme : un refus que
    ferrite prononce la ou ES repond ne peut donc pas y entrer."""
    if vu.startswith("[") or vu.startswith("{"):
        return vu
    tete = vu.split(" ", 1)[0]
    return tete


def refuse(vu):
    return not (vu.startswith("[") or vu.startswith("{"))


def assume(libelle, reps):
    classe = REFUS_ASSUMES.get(libelle, (None, None))[0]
    if classe == "type":
        return len({sans_le_type(vu) for _, vu in reps}) == 1
    if classe == "ecart_es":
        # La divergence n'est assumee que si ferrite **repond** : l'excuse « ES
        # a un defaut » ne doit pas couvrir un refus de ferrite.
        return not refuse(reps[0][1])
    if classe == "id_opaque":
        # Le predicat de cette classe-la ne se contente pas de « les deux
        # refusent » : il exige que ferrite refuse **par la porte qu'un client
        # reconnait**, celle du contexte introuvable. Un refus quelconque n'y
        # entre pas.
        vu = reps[0][1]
        return "404" in vu.split(",")[0] and (
            "search_context_missing" in vu or '"corps"' in vu
        )
    return False


def abrege(vu, n=170):
    return vu if len(vu) <= n else vu[:n - 3] + "..."


# ---------------------------------------------------------------------------

def main():
    argv = []
    calibrer = False
    taille = PARCOURS_DEFAUT
    it = iter(sys.argv[1:])
    for a in it:
        if a == "--calibrer":
            calibrer = True
        elif a == "--parcours":
            taille = int(next(it))
        else:
            argv.append(a)
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
    # La version de chaque cible : un etalonnage a deux serveurs de la meme
    # version prouve le determinisme, pas l'independance a la version majeure.
    print("# cibles : " + ", ".join(f"{nom}={base} (v{version(base)})"
                                    for nom, base in cibles))
    print(f"# corpus du parcours : {taille} documents")
    for _, base in cibles:
        prepare(base, taille)

    resultats = []   # (libelle, [(nom, resume)])

    for libelle, index, corps in cas_search_after():
        resultats.append((libelle, [
            (nom, resume_recherche(*http(base, "POST", f"/{index}/_search", corps)))
            for nom, base in cibles]))

    for libelle, index, corps, qs in cas_scroll_et_pit():
        resultats.append((libelle, [
            (nom, resume_recherche(*http(base, "POST", f"/{index}/_search{qs}", corps)))
            for nom, base in cibles]))

    for libelle, methode, chemin, corps in cas_pit_ouverture():
        reps = []
        for nom, base in cibles:
            st, r = http(base, methode, chemin, corps)
            reps.append((nom, resume_ouverture(st, r)))
            if st == 200 and "id" in r:
                http(base, "DELETE", "/_pit", {"id": r["id"]})
        resultats.append((libelle, reps))

    for libelle, corps in cas_pit_forme():
        resultats.append((libelle, [
            (nom, resume_recherche(*http(base, "POST", "/_search", corps)))
            for nom, base in cibles]))

    def sous_pit(base, corps, rendu):
        st, ouv = http(base, "POST", f"/{MONO}/_pit?keep_alive=1m")
        if st != 200:
            return resume_erreur(st, ouv)
        pid = ouv["id"]
        try:
            st, r = http(base, "POST", "/_search", avec_pit(corps, pid))
            return rendu(st, r)
        finally:
            http(base, "DELETE", "/_pit", {"id": pid})

    for libelle, corps, masque in cas_pit_recherche():
        resultats.append((libelle, [
            (nom, sous_pit(base, corps,
                           lambda st, r, m=masque: resume_recherche(st, r, m)))
            for nom, base in cibles]))

    for libelle, corps in cas_pit_ordre_interne():
        resultats.append((libelle, [
            (nom, sous_pit(base, corps, verdict_ordre_interne))
            for nom, base in cibles]))

    for libelle, fn in [
        ("propriete : _doc est un tri total", lambda b: propriete_tri_total(b, False)),
        ("propriete : _shard_doc est un tri total", lambda b: propriete_tri_total(b, True)),
        ("propriete : le pit fige la vue", propriete_pit_fige),
        ("propriete : cycle de vie du pit", propriete_pit_cycle),
        ("propriete : deux pit sont distincts", propriete_pit_deux_ouvertures),
        ("propriete : pit_id rendu par la recherche", propriete_pit_renvoye),
        ("propriete : pas d'index dans l'URL sous pit", propriete_pit_index_dans_url),
    ]:
        resultats.append((libelle, [(nom, fn(base)) for nom, base in cibles]))

    # Le critere d'acceptation : le parcours complet, decoupe comprise.
    for libelle, page, pit, sort in [
        (f"parcours u asc, pages de 500 ({taille} docs)", 500, False, [{"u": "asc"}]),
        (f"parcours u desc, pages de 333 ({taille} docs)", 333, False, [{"u": "desc"}]),
        (f"parcours n asc + u asc, pages de 700 ({taille} docs)", 700, False,
         [{"n": "asc"}, {"u": "asc"}]),
        (f"parcours k asc + u asc, pages de 1000 ({taille} docs)", 1000, False,
         [{"k": "asc"}, {"u": "asc"}]),
        (f"parcours sous pit, u asc, pages de 500 ({taille} docs)", 500, True,
         [{"u": "asc"}]),
        (f"parcours sous pit, n asc (tiebreak implicite) ({taille} docs)", 500, True,
         [{"n": "asc"}]),
    ]:
        resultats.append((libelle, [
            (nom, resume_parcours(parcourir(base, page, pit, sort), taille))
            for nom, base in cibles]))

    # Le parcours par le **numero interne** : c'est celui qu'un export ecrit
    # sans se soucier du mapping, et c'est le seul dont l'ordre ne se compare
    # pas. On compare donc ce qui compte — que tout sorte une fois et une seule
    # — sans le premier ni le dernier document.
    for libelle, page, sort in [
        (f"parcours sous pit par _shard_doc, pages de 500 ({taille} docs)", 500,
         ["_shard_doc"]),
        (f"parcours par _doc, pages de 700 ({taille} docs)", 700, ["_doc"]),
    ]:
        sous = sort == ["_shard_doc"]
        resultats.append((libelle, [
            (nom, invariants_parcours(parcourir(base, page, sous, sort), taille))
            for nom, base in cibles]))

    ecarts = assumes = 0
    for libelle, reps in resultats:
        vals = {vu for _, vu in reps}
        if len(vals) <= 1:
            print(f"  {libelle:52} {abrege(reps[0][1])}")
            continue
        if assume(libelle, reps):
            assumes += 1
            print(f"~ {libelle:52} " +
                  "\n      ".join(f"{nom}={abrege(vu)}" for nom, vu in reps))
            print(f"      assume ({REFUS_ASSUMES[libelle][0]}) : "
                  f"{REFUS_ASSUMES[libelle][1]}")
        else:
            ecarts += 1
            print(f"* {libelle:52} " +
                  "\n      ".join(f"{nom}={abrege(vu)}" for nom, vu in reps))
    total = len(resultats)
    print(f"\n{total - ecarts - assumes}/{total} identiques, "
          f"{assumes} refus assumes, {ecarts} ecarts")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
