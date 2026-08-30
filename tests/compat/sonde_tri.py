#!/usr/bin/env python3
"""Sonde : `missing`, `mode` et `unmapped_type` sur un `sort`.

Trois parametres qu'un champ optionnel ou multivalue rend obligatoires, et dont
aucun bord n'est ecrit dans la documentation d'Elasticsearch. Ce fichier pose la
meme question aux deux serveurs et compare **l'ordre complet des documents,
ex aequo compris, plus le tableau `sort` de chaque hit** — pas seulement
l'ensemble des identifiants : un `mode` faux change l'ordre sans changer le
total, et une sentinelle fausse ne se voit que dans le tableau `sort`.

    python3 tests/compat/sonde_tri.py [ferrite] [es]
    python3 tests/compat/sonde_tri.py --calibrer [es_a] [es_b]

Elle **refuse de tourner** si elle ne trouve pas ses deux cibles : une sonde
differentielle qui ne compare rien rendrait « tout identique ».
"""
import json
import sys
import urllib.error
import urllib.request

MONO = "sonde-tri"
# Trois index pour le multi-index : `multi-a` mappe `i` en `long`, `multi-b` ne
# le mappe pas du tout (c'est lui que `unmapped_type` sauve), `multi-c` le mappe
# en `keyword` (c'est lui qui fait diverger les familles de tri).
A, B, C = "sonde-tri-multi-a", "sonde-tri-multi-b", "sonde-tri-multi-c"
INDEX = [MONO, A, B, C]


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


# Le corpus mono-index. `c` n'a aucun champ : c'est lui qui exerce `missing`.
# `d` est multivalue sur les cinq types, dans le **desordre** — sans quoi le
# `mode` ne mesurerait rien.
DOCS = {
    "a": {"k": "alpha", "i": 1, "f": 1.5, "d": "2020-01-01", "b": True},
    "b": {"k": "zoulou", "i": 10, "f": -2.5, "d": "2021-01-01", "b": False},
    "c": {},
    "d": {"k": ["m1", "m2", "m3", "m0"], "i": [5, 1, 9, 3],
          "f": [2.0, -1.0, 7.5], "d": ["2019-06-01", "2022-06-01"],
          "b": [True, False]},
    "e": {"k": "beta", "i": 4, "f": 0.0, "d": "2020-06-01", "b": True},
    # Les bords des entiers, poses dans le **meme document et dans le
    # desordre** : c'est la seule forme ou le debordement de `mode: sum` se
    # voit, et l'ordre de la colonne y decide de tout.
    "g": {"i": [1, 9223372036854775807]},
    "h": {"i": [-9223372036854775808, -1]},
    "j": {"i": [2, 3]},
    "l": {"i": [1, 2, 3, 4, 5]},
    "m": {"i": [7], "f": [1e308, 1e308]},
    # Un tableau vide et un `null` explicite : ES les traite en « sans valeur ».
    "n": {"i": [], "k": None},
}
MAPPING = {"mappings": {"properties": {
    "k": {"type": "keyword"}, "i": {"type": "long"}, "f": {"type": "double"},
    "d": {"type": "date"}, "b": {"type": "boolean"}, "t": {"type": "text"}}}}


def bulk(base, docs):
    lignes = []
    for index, id_, doc in docs:
        lignes.append(json.dumps({"index": {"_index": index, "_id": id_}}))
        lignes.append(json.dumps(doc))
    http(base, "POST", "/_bulk?refresh=true", "\n".join(lignes) + "\n", ndjson=True)


def prepare(base):
    for idx in INDEX:
        http(base, "DELETE", "/" + idx)
    http(base, "PUT", "/" + MONO, MAPPING)
    http(base, "PUT", "/" + A, {"mappings": {"properties": {
        "k": {"type": "keyword"}, "i": {"type": "long"}, "f": {"type": "double"}}}})
    http(base, "PUT", "/" + B, {"mappings": {"properties": {
        "k": {"type": "keyword"}}}})
    http(base, "PUT", "/" + C, {"mappings": {"properties": {
        "k": {"type": "keyword"}, "i": {"type": "keyword"}, "f": {"type": "float"}}}})
    bulk(base, [(MONO, id_, doc) for id_, doc in DOCS.items()] + [
        (A, "a1", {"k": "aa", "i": 3, "f": 1.5}),
        (A, "a2", {"k": "bb", "i": 9, "f": 2.5}),
        (B, "b1", {"k": "cc"}),
        (B, "b2", {"k": "dd"}),
        (C, "c1", {"k": "ee", "i": "zz", "f": 0.5}),
    ])


def cas_mono():
    """(libelle, index, corps de `sort`)."""
    out = []
    # Le defaut, type par type : c'est la ligne de base contre laquelle
    # `missing` et `mode` se lisent.
    for champ in "kifdb":
        for sens in ("asc", "desc"):
            out.append((f"{champ} {sens}", MONO, [{champ: {"order": sens}}]))

    # `missing` : les deux mots-cles, sur les cinq types et dans les deux sens.
    for champ in "kifdb":
        for sens in ("asc", "desc"):
            for m in ("_first", "_last"):
                out.append((f"{champ} {sens} missing={m}", MONO,
                            [{champ: {"order": sens, "missing": m}}]))

    # `missing` : les valeurs de substitution, et leurs refus. La casse compte
    # (`_FIRST` n'est pas un mot-cle), une date se substitue par un nombre de
    # millisecondes et un booleen par 0 ou 1.
    subs = [
        ("k", ["mm", "zzz", 42, True, None, "", "_FIRST", "_LAST", "_first "]),
        ("i", [7, "7", "+7", 7.9, -0.0, 1e300, "abc", "7.9", " 7", "7 ", "1e3",
               "0x10", "9223372036854775808", True, None, [], {}]),
        ("f", [0.5, "0.5", "7.9", "1e3", "Infinity", "-Infinity", "NaN", "inf",
               "abc", None]),
        ("d", ["2020-03-01", 1580000000000, 0, "abc"]),
        ("b", [True, "true", 1, 0, "abc"]),
    ]
    for champ, valeurs in subs:
        for v in valeurs:
            for sens in ("asc", "desc"):
                out.append((f"{champ} {sens} missing={v!r}", MONO,
                            [{champ: {"order": sens, "missing": v}}]))

    # `mode` : les cinq modes sur les cinq types, dans les deux sens. C'est ici
    # que se lisent l'arrondi de `avg`, le debordement de `sum` et le milieu de
    # `median`.
    for champ in "kifdb":
        for mode in ("min", "max", "sum", "avg", "median"):
            for sens in ("asc", "desc"):
                out.append((f"{champ} {sens} mode={mode}", MONO,
                            [{champ: {"order": sens, "mode": mode}}]))
    out.append(("i asc mode=MIN (casse)", MONO, [{"i": {"order": "asc", "mode": "MIN"}}]))
    out.append(("i asc mode=nawak", MONO, [{"i": {"order": "asc", "mode": "nawak"}}]))
    out.append(("i asc mode=1 (pas une chaine)", MONO, [{"i": {"order": "asc", "mode": 1}}]))
    out.append(("t asc mode=min (champ text)", MONO, [{"t": {"order": "asc", "mode": "min"}}]))

    # `mode` et `missing` ensemble : la valeur de substitution ne passe **pas**
    # par le mode.
    out.append(("i asc mode=avg missing=_first", MONO,
                [{"i": {"order": "asc", "mode": "avg", "missing": "_first"}}]))
    out.append(("i asc mode=sum missing=3", MONO,
                [{"i": {"order": "asc", "mode": "sum", "missing": 3}}]))
    out.append(("i desc mode=median missing=_last", MONO,
                [{"i": {"order": "desc", "mode": "median", "missing": "_last"}}]))

    # Les trois parametres sur les cles qui ne sont pas des champs : `_score`
    # les refuse, `_doc` les accepte et les ignore.
    for nom, val in [("missing", "_first"), ("mode", "min"), ("unmapped_type", "long")]:
        out.append((f"_score {nom}", MONO, [{"_score": {"order": "desc", nom: val}}]))
        out.append((f"_doc {nom}", MONO, [{"_doc": {"order": "asc", nom: val}}]))

    # Multi-cles : la sentinelle d'une cle ne doit pas manger la suivante.
    out.append(("i asc missing=_first, k desc", MONO,
                [{"i": {"order": "asc", "missing": "_first"}}, {"k": {"order": "desc"}}]))
    out.append(("k asc missing=_first, i desc", MONO,
                [{"k": {"order": "asc", "missing": "_first"}}, {"i": {"order": "desc"}}]))
    # La forme courte et la forme objet vide restent lisibles.
    out.append(("i (objet vide)", MONO, [{"i": {}}]))
    out.append(("i mode seul, sans order", MONO, [{"i": {"mode": "max"}}]))
    out.append(("cle inconnue dans sort", MONO, [{"i": {"order": "asc", "nawak": 1}}]))
    # `unmapped_type` sur un champ que l'index **mappe** : ES l'ignore.
    out.append(("k asc unmapped_type=long (champ mappe)", MONO,
                [{"k": {"order": "asc", "unmapped_type": "long"}}]))
    return out


def cas_multi():
    out = []
    # Sans `unmapped_type`, l'index qui ignore le champ fait echouer son shard.
    # Avec, il participe et tous ses documents sont « sans valeur ».
    out.append(("a,b : i asc (sans unmapped_type)", f"{A},{B}", [{"i": "asc"}]))
    for ty in ["long", "integer", "short", "byte", "date", "boolean", "keyword",
               "double", "float", "text", "nawak", "object", "nested", "ip"]:
        out.append((f"a,b : i asc unmapped_type={ty}", f"{A},{B}",
                    [{"i": {"order": "asc", "unmapped_type": ty}}]))
    for sens in ("asc", "desc"):
        for m in ("_first", "_last"):
            out.append((f"a,b : i {sens} ut=long missing={m}", f"{A},{B}",
                        [{"i": {"order": sens, "unmapped_type": "long", "missing": m}}]))
    out.append(("a,b : i asc ut=long missing=5", f"{A},{B}",
                [{"i": {"order": "asc", "unmapped_type": "long", "missing": 5}}]))
    out.append(("a,b : i asc ut=long mode=avg", f"{A},{B}",
                [{"i": {"order": "asc", "unmapped_type": "long", "mode": "avg"}}]))
    # La substitution est typee **par index** : `aaa` est lisible sur le
    # `keyword` anonyme de `b`, pas sur le `long` de `a`.
    out.append(("a,b : i asc ut=keyword missing=aaa", f"{A},{B}",
                [{"i": {"order": "asc", "unmapped_type": "keyword", "missing": "aaa"}}]))
    # Un champ qu'aucun index ne mappe : `unmapped_type` le rend triable partout.
    out.append(("a,b : absent asc", f"{A},{B}", [{"absent": "asc"}]))
    out.append(("a,b : absent asc ut=long", f"{A},{B}",
                [{"absent": {"order": "asc", "unmapped_type": "long"}}]))
    out.append(("a,b : absent asc ut=keyword", f"{A},{B}",
                [{"absent": {"order": "asc", "unmapped_type": "keyword"}}]))
    out.append(("a,b : absent asc ut=ip", f"{A},{B}",
                [{"absent": {"order": "asc", "unmapped_type": "ip"}}]))

    # Le conflit de familles de tri. `a` mappe `i` en `long`, `c` en `keyword` :
    # ES ne fusionne pas, et rend 400 — pas un ordre.
    out.append(("a,c : i asc (long vs keyword)", f"{A},{C}", [{"i": "asc"}]))
    out.append(("a,c : f asc (double vs float)", f"{A},{C}", [{"f": "asc"}]))
    out.append(("a,c : k asc (pas de conflit)", f"{A},{C}", [{"k": "asc"}]))
    out.append(("a,c : _score puis i asc", f"{A},{C}",
                [{"_score": "desc"}, {"i": "asc"}]))
    out.append(("c seul : i asc", C, [{"i": "asc"}]))
    return out


def cas_multi_avec_query():
    """Les cas ou le conflit **ne se leve pas** : personne a fusionner."""
    return [
        ("a,c : i asc, query ne vise que a", f"{A},{C}", [{"i": "asc"}],
         {"term": {"k": "aa"}}, 10),
        ("a,c : i asc, size=0", f"{A},{C}", [{"i": "asc"}], None, 0),
        ("a,c : i asc, size=1", f"{A},{C}", [{"i": "asc"}], None, 1),
        ("a,b : i asc ut=keyword, query ne vise que a", f"{A},{C}",
         [{"i": {"order": "asc", "unmapped_type": "keyword"}}],
         {"term": {"k": "aa"}}, 10),
    ]


def interroge(base, index, sort, query=None, size=30):
    """Ce qui se compare : l'ordre complet des documents **et** leur tableau
    `sort`, ou le statut et le message d'erreur.

    Les echecs de shard partiels comptent aussi : une reponse 200 qui perd un
    index en silence n'est pas la meme qu'une reponse 200 complete."""
    corps = {"size": size, "sort": sort, "_source": False}
    if query:
        corps["query"] = query
    st, body = http(base, "POST", f"/{index}/_search", corps)
    if st != 200:
        err = body.get("error", {})
        cause = err
        if err.get("root_cause"):
            cause = err["root_cause"][0]
        elif err.get("caused_by"):
            cause = err["caused_by"]
        return f"{st} {cause.get('type')}"
    ordre = [(h["_index"], h["_id"], h.get("sort")) for h in body["hits"]["hits"]]
    echecs = sorted((f["index"], f["reason"]["type"])
                    for f in body["_shards"].get("failures", []))
    return json.dumps([ordre, body["hits"]["total"]["value"], echecs])


# Les ecarts assumes, chacun avec sa raison ecrite **et** le predicat qu'il doit
# passer. Deux classes, et elles ne verifient pas la meme chose :
#
# - `type` : les deux serveurs rendent le meme resultat, seul le **nom** du type
#   d'erreur differe ;
# - `perimetre` : ferrite refuse un type de champ qu'il ne mappe pas, la ou ES le
#   mappe. C'est un cout de perimetre, ecrit dans `compat.yaml` — et le predicat
#   verifie quand meme que ferrite **refuse explicitement**, jamais qu'il rend un
#   resultat en silence.
REFUS_ASSUMES = {
    "cle inconnue dans sort": ("type",
        "ES rend `x_content_parse_exception [field_sort] unknown field [nawak]`, "
        "ferrite le type qu'il reserve a ce qu'il ne sait pas faire — il ne "
        "distingue pas une faute de frappe d'un parametre de tri qu'il ne "
        "supporte pas (`nested`, `numeric_type`, `format`)"),
    "a,b : i asc unmapped_type=nawak": ("type",
        "ES rend `No mapper found for type [nawak]` ; ferrite ne connait pas la "
        "liste des types d'ES, il ne peut donc dire que ce qu'il sait : ce type "
        "n'est pas un des siens"),
    "a,b : i asc unmapped_type=ip": ("perimetre",
        "`ip` est un type qu'ES mappe et que ferrite ne mappe pas : le refus est "
        "celui de ferrite, et il est nomme. ES, lui, echoue ensuite pour une "
        "autre raison (`ip` trie en STRING, `i` en LONG dans l'autre index)"),
    "a,b : absent asc ut=ip": ("perimetre",
        "le meme refus, mais cette fois ES **sert** la requete : aucun index ne "
        "mappe le champ, donc rien ne fait diverger les familles. C'est un cout "
        "de perimetre, pas un ecart — et il se voit"),
}


def sans_le_type(vu):
    """La meme reponse, privee du **nom** du type d'erreur.

    C'est ce que compare le predicat `type`, et sa forme compte : il garde le
    statut, l'ordre des documents, le total, et **quels index ont echoue**. Un
    refus que ferrite prononce la ou ES repond ne peut donc pas y entrer —
    c'est exactement l'elargissement discret contre lequel ce depot s'est deja
    fait avoir une fois."""
    if vu.startswith("["):
        ordre, total, echecs = json.loads(vu)
        return json.dumps([ordre, total, sorted(idx for idx, _ in echecs)])
    return vu.split(" ", 1)[0]


def refuse(vu):
    """La reponse porte-t-elle un refus explicite ? Un 4xx, ou un 200 dont un
    index a echoue en le disant. Rendre un resultat complet n'en est pas un."""
    if not vu.startswith("["):
        return True
    _, _, echecs = json.loads(vu)
    return bool(echecs)


def assume(libelle, reps):
    """Le cas est-il un ecart assume, et lequel ?"""
    classe = REFUS_ASSUMES.get(libelle, (None, None))[0]
    if classe == "type":
        return len({sans_le_type(vu) for _, vu in reps}) == 1
    if classe == "perimetre":
        return refuse(reps[0][1])
    return False


def abrege(vu):
    if len(vu) > 150:
        return vu[:147] + "..."
    return vu


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

    batterie = ([(lib, idx, sort, None, 30) for lib, idx, sort in cas_mono()]
                + [(lib, idx, sort, None, 30) for lib, idx, sort in cas_multi()]
                + cas_multi_avec_query())
    ecarts = assumes = total = 0
    for libelle, index, sort, query, size in batterie:
        reps = [(nom, interroge(base, index, sort, query, size))
                for nom, base in cibles]
        vals = {vu for _, vu in reps}
        total += 1
        if len(vals) <= 1:
            print(f"  {libelle:44} {abrege(reps[0][1])}")
            continue
        # Un ecart n'est « assume » que s'il est ecrit **et** que les deux
        # serveurs refusent avec le meme statut : sans cette seconde moitie, la
        # liste couvrirait aussi le cas ou ferrite refuse ce qu'ES sait faire.
        if assume(libelle, reps):
            assumes += 1
            marque = "~"
        else:
            ecarts += 1
            marque = "*"
        print(f"{marque} {libelle:44} " +
              "\n      ".join(f"{nom}={abrege(vu)}" for nom, vu in reps))
        if marque == "~":
            print(f"      assume ({REFUS_ASSUMES[libelle][0]}) : "
                  f"{REFUS_ASSUMES[libelle][1]}")
    print(f"\n{total - ecarts - assumes}/{total} identiques, "
          f"{assumes} refus assumes, {ecarts} ecarts")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
