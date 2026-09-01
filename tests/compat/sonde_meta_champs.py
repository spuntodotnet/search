#!/usr/bin/env python3
"""Sonde : les champs de **metadonnees** en clause de requete.

Une clause `term` sur `_id` rendait `hits.total = 0` chez ferrite, sans erreur,
pour un document qui existe — la pire des trois categories du projet : un vide
qui ressemble a « document absent ». Mais « faire repondre `_id` » n'est pas la
regle a appliquer, et c'est tout l'objet de ce fichier : chez Elasticsearch,
`term`, `terms`, `match` et `exists` repondent sur `_id` **et** `prefix`,
`wildcard`, `regexp` et `range` y sont **refuses** en le nommant. Servir ces
quatre-la aurait ete une divergence de plus, dans l'autre sens.

Trois issues possibles par case, et il faut savoir laquelle avant d'ecrire :

* **repond** — ES rend des documents : ferrite doit rendre les memes ;
* **refuse** — ES rend une erreur : ferrite doit refuser, avec sa phrase ;
* **vide** — ES rend 200 et aucun document : ferrite peut rendre vide.

C'est la troisieme colonne qui empeche de se tromper de regle. `{"term":
{"champ_absent": "x"}}` rend 0 en silence des **deux** cotes : c'est un
`MatchNoDocsQuery` chez ES, donc la parite est juste et en faire une erreur
serait une divergence introduite au nom d'une regle mal citee. La regle du
projet n'est pas « tout vide est un bug », c'est « jamais un resultat que
ferrite sait faux ».

    python3 tests/compat/sonde_meta_champs.py [ferrite] [es]
    python3 tests/compat/sonde_meta_champs.py --calibrer [es_a] [es_b]
    python3 tests/compat/sonde_meta_champs.py --table [es]   # ce qu'ES rend, classe

`--table` ne demande **qu'un** serveur et n'emet aucun verdict : c'est le
premier livrable, la table des trois issues, et elle vaut avant qu'une ligne de
code soit ecrite. Elle sert aussi a mesurer une **autre version** d'ES (7.10.2)
sans rien comparer a ferrite.

La sonde imprime la **version de chacune de ses cibles**. Un etalonnage a deux
serveurs de la meme version prouve que la sonde est deterministe ; il ne peut
pas montrer qu'une reponse depend de la version majeure. Le rapport doit donc
dire de quoi il est la mesure.

Elle **refuse de tourner** si elle ne trouve pas ses deux cibles : une sonde
differentielle qui ne compare rien rendrait « tout identique ».
"""
import json
import sys
import urllib.error
import urllib.request

IDX = "sonde-meta"
# Le second index : `_index` n'a de sens en clause que sur une recherche qui en
# vise plusieurs — c'est meme son seul usage reel.
IDX2 = "sonde-meta-b"
# Le troisieme index n'existe que pour `_routing`, et il existe **a part** pour
# une raison mesuree : ferrite refuse `?routing=` a l'indexation. Poser le
# document route dans l'index principal vidait le corpus des deux tiers de ses
# documents chez lui, et les 174 lignes rougissaient toutes — un defaut
# d'outillage qui se lisait « ferrite ne sait rien faire ». Isole ici, le refus
# est compte une fois, sur sa propre ligne, et ne salit rien d'autre.
IDXR = "sonde-meta-r"
# L'expression que visent les cas multi-index. Surtout **pas** `*` : la sonde y
# balayait les index laisses par les autres campagnes, et les deux serveurs de
# `--calibrer` n'en portent pas les memes — un `must_not` y rendait 6 256
# documents d'un cote et 14 de l'autre. Un dénominateur qu'on ne choisit pas
# n'est pas le meme des deux cotes.
MULTI = "sonde-meta,sonde-meta-b"

ID1 = "mon-id-1"
ID2 = "mon-id-2"
ROUTAGE = "r1"


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
    """Ce que la cible dit d'elle-meme : c'est ce que le rapport doit porter."""
    try:
        _, corps = http(base, "GET", "/")
    except Exception as e:  # noqa: BLE001
        return f"injoignable ({e})"
    v = corps.get("version", {})
    return f"{v.get('number')} ({v.get('build_hash', '?')[:16]})"


# ---------------------------------------------------------------------------
# Le corpus. Deux index, trois documents, un seul route : c'est le minimum pour
# que chaque champ de metadonnees ait une valeur presente **et** une valeur
# absente quelque part.
# ---------------------------------------------------------------------------
PROPRIETES = {
    "kw": {"type": "keyword"},
    "txt": {"type": "text"},
    "num": {"type": "long"},
    # Un champ dont la valeur trop longue sera **ecartee** : c'est ce qui
    # remplit `_ignored`.
    "ia": {"type": "keyword", "ignore_above": 3},
}

DOCS = [
    (IDX, ID1, {"kw": "alpha", "txt": "le vif renard brun", "num": 1, "ia": "abc"}, None),
    (IDX, ID2, {"kw": "beta", "txt": "le loup gris", "num": 2, "ia": "trop-long"}, None),
    (IDX2, "autre-1", {"kw": "gamma", "txt": "le chat", "num": 3}, None),
    (IDXR, "sans-routage", {"kw": "delta"}, None),
    (IDXR, "avec-routage", {"kw": "epsilon"}, ROUTAGE),
]


def prepare(base):
    for nom in (IDX, IDX2, IDXR):
        http(base, "DELETE", "/" + nom)
        http(base, "PUT", "/" + nom, {"mappings": {"properties": PROPRIETES}})
    # Un document a la fois, dans un ordre fixe : `_seq_no` et `_version` en
    # dependent, et ils sont eux-memes interroges plus bas.
    ecritures = []
    for idx, id_, doc, routage in DOCS:
        chemin = f"/{idx}/_doc/{id_}?refresh=true"
        if routage:
            chemin += f"&routing={routage}"
        st, corps = http(base, "PUT", chemin, doc)
        if st not in (200, 201):
            ecritures.append(f"{idx}/{id_} = {erreur(st, corps)}")
    return ecritures


# ---------------------------------------------------------------------------
# Les questions. Chaque champ de metadonnees recoit les memes clauses : ce qui
# se compare d'une ligne a l'autre est ce que le champ change, et rien d'autre.
# ---------------------------------------------------------------------------
META = {
    #                terme          motif        borne        index vise
    "_id": (ID1, "mon-id", ID1, IDX),
    "_index": (IDX, "sonde-meta", IDX, IDX),
    "_routing": (ROUTAGE, "r", ROUTAGE, IDXR),
    "_seq_no": (0, "0", 0, IDX),
    "_type": ("_doc", "_do", "_doc", IDX),
    "_source": ("alpha", "alph", "alpha", IDX),
    "_field_names": ("kw", "k", "kw", IDX),
    "_version": (1, "1", 1, IDX),
    "_ignored": ("ia", "i", "ia", IDX),
}


def cas_par_champ():
    """Les 17 clauses qui prennent un nom de champ, posees sur chaque champ de
    metadonnees. C'est la matrice que la carte demande."""
    for f, (terme, motif, borne, cible) in META.items():
        q = [
            (f"term {f}", {"query": {"term": {f: terme}}}),
            (f"term {f} (forme longue, boost 3)",
             {"query": {"term": {f: {"value": terme, "boost": 3}}}}),
            (f"terms {f}", {"query": {"terms": {f: [terme]}}}),
            (f"match {f}", {"query": {"match": {f: terme}}}),
            (f"match_phrase {f}", {"query": {"match_phrase": {f: terme}}}),
            (f"match_phrase_prefix {f}", {"query": {"match_phrase_prefix": {f: terme}}}),
            # La barre de recherche qui balaie plusieurs champs : un champ de
            # metadonnees dans `fields` ne doit pas vider la clause entiere —
            # c'est le piege deja paye sur un champ non mappe.
            (f"multi_match {f} + kw", {"query": {"multi_match": {
                "query": str(terme), "fields": [f, "kw"]}}}),
            (f"prefix {f}", {"query": {"prefix": {f: motif}}}),
            (f"wildcard {f}", {"query": {"wildcard": {f: motif + "*"}}}),
            (f"regexp {f}", {"query": {"regexp": {f: motif + ".*"}}}),
            (f"fuzzy {f}", {"query": {"fuzzy": {f: terme}}}),
            (f"range {f} gte", {"query": {"range": {f: {"gte": borne}}}}),
            (f"exists {f}", {"query": {"exists": {"field": f}}}),
            # La negation : un vide silencieux s'y retourne en « tous les
            # documents », ce qui est le meme defaut avec le signe oppose.
            (f"must_not term {f}",
             {"query": {"bool": {"must_not": [{"term": {f: terme}}]}}}),
            # La clause composee : une feuille qui se vide ne doit pas vider sa
            # voisine (c'est le piege deja paye sur `multi_match`).
            (f"bool should term {f} + term kw",
             {"query": {"bool": {"should": [{"term": {f: terme}},
                                            {"term": {"kw": "beta"}}]}}}),
            (f"sort {f} asc", {"sort": [{f: "asc"}], "_source": False}),
            (f"agg terms {f}", {"size": 0, "aggs": {"a": {"terms": {"field": f}}}}),
            (f"docvalue_fields {f}", {"docvalue_fields": [f], "_source": False}),
            (f"fields {f}", {"fields": [f], "_source": False}),
        ]
        for libelle, corps in q:
            yield libelle, corps, cible, f


def cas_particuliers():
    """Ce que la matrice ne couvre pas : les temoins, les valeurs absentes, et
    les formes que les vraies applications ecrivent."""
    # Le temoin de la carte : `ids` trouve le document que `term` ne trouvait
    # pas. Les deux doivent rendre la meme chose.
    yield "[temoin] ids values", {"query": {"ids": {"values": [ID1]}}}
    yield ("[temoin] term _id == ids values",
           {"query": {"bool": {"should": [{"ids": {"values": [ID1]}},
                                          {"term": {"_id": ID1}}]}}})
    # Un `_id` qui n'existe pas : les deux doivent rendre vide, et c'est
    # exactement ce dont le vide de la carte etait indiscernable.
    yield "term _id absent", {"query": {"term": {"_id": "jamais-indexe"}}}
    # Le vide **juste** : un champ que le mapping ne connait pas. ES en fait un
    # `MatchNoDocsQuery` ; en faire une erreur serait une divergence introduite.
    yield ("term champ_absent (vide juste)",
           {"query": {"term": {"champ_qui_nexiste_pas": "x"}}})
    yield ("must_not term champ_absent (vide juste, negatif)",
           {"query": {"bool": {"must_not": [{"term": {"champ_qui_nexiste_pas": "x"}}]}}})
    # `_id` sous les enveloppes qui ne notent pas.
    yield ("constant_score term _id",
           {"query": {"constant_score": {"filter": {"term": {"_id": ID1}}, "boost": 2}}})
    yield ("bool filter term _id",
           {"query": {"bool": {"filter": [{"term": {"_id": ID1}}]}}})
    # Plusieurs identifiants d'un coup — la forme qu'une application ecrit pour
    # rapatrier un lot.
    yield "terms _id (deux)", {"query": {"terms": {"_id": [ID1, ID2]}}}
    yield "terms _id (un absent)", {"query": {"terms": {"_id": [ID1, "jamais"]}}}
    # Un identifiant **numerique** : le meme piege que `_bulk` avait paye.
    yield "term _id numerique", {"query": {"term": {"_id": 42}}}
    # `match` sur deux identifiants : `_id` est-il analyse ?
    yield "match _id deux valeurs", {"query": {"match": {"_id": f"{ID1} {ID2}"}}}
    # `_index` en multi-index : son seul usage reel.
    yield ("term _index sur deux index", {"query": {"term": {"_index": IDX2}}}, MULTI)
    yield ("terms _index sur deux index", {"query": {"terms": {"_index": [IDX, IDX2]}}}, MULTI)
    yield ("term _index inconnu", {"query": {"term": {"_index": "pas-un-index"}}}, MULTI)
    # La valeur d'une clause sur `_index` n'est pas une chaine exacte mais une
    # **expression de nom d'index** — et seule l'etoile y est un joker. Les
    # quatre lignes suivantes sont ce qui separe `Regex.simpleMatch` d'ES de la
    # syntaxe de `wildcard` : lire la valeur comme une chaine exacte rendait
    # zero document la ou ES en rend trois, en 200.
    yield ("term _index avec un joker", {"query": {"term": {"_index": "sonde-meta*"}}}, MULTI)
    yield ("term _index joker au milieu", {"query": {"term": {"_index": "*meta*"}}}, MULTI)
    yield ("term _index avec un [?] (pas un joker)",
           {"query": {"term": {"_index": "sonde-met?"}}}, MULTI)
    yield ("wildcard _index avec un [?] (pas un joker)",
           {"query": {"wildcard": {"_index": "sonde-meta-?"}}}, MULTI)
    yield ("term _index etoile echappee (la barre n'echappe rien)",
           {"query": {"term": {"_index": "sonde-meta\\*"}}}, MULTI)
    yield ("prefix _index qui porte deja une etoile",
           {"query": {"prefix": {"_index": "sonde-meta*"}}}, MULTI)
    yield ("wildcard _index sur deux index",
           {"query": {"wildcard": {"_index": "sonde-meta-*"}}}, MULTI)
    yield ("prefix _index sur deux index",
           {"query": {"prefix": {"_index": "sonde-meta-"}}}, MULTI)
    yield ("bool filter term _index",
           {"query": {"bool": {"filter": [{"term": {"_index": IDX2}}]}}}, MULTI)
    yield ("must_not term _index sur deux index",
           {"query": {"bool": {"must_not": [{"term": {"_index": IDX2}}]}}}, MULTI)
    # Le tri par `_doc` et par `_score`, qui ne sont pas des champs mais des
    # mots-cles : la sonde doit dire ou passe la frontiere.
    yield "sort _doc asc", {"sort": [{"_doc": "asc"}], "_source": False}
    yield "sort _score desc", {"query": {"match_all": {}}, "sort": [{"_score": "desc"}],
                               "_source": False}
    yield "sort _id asc (avec _doc en second)", {"sort": [{"_id": "asc"}, {"_doc": "asc"}],
                                                 "_source": False}
    # Les temoins sur un champ ordinaire : ils disent que ce qui bouge vient du
    # champ de metadonnees et pas de la clause.
    yield "[temoin] term kw", {"query": {"term": {"kw": "alpha"}}}
    yield "[temoin] exists kw", {"query": {"exists": {"field": "kw"}}}
    yield "[temoin] agg terms kw", {"size": 0, "aggs": {"a": {"terms": {"field": "kw"}}}}
    yield "[temoin] sort kw asc", {"sort": [{"kw": "asc"}], "_source": False}


def batterie():
    out = [(lib, corps, cible) for lib, corps, cible, _ in cas_par_champ()]
    for cas in cas_particuliers():
        out.append((cas[0], cas[1], cas[2] if len(cas) > 2 else IDX))
    return out


# ---------------------------------------------------------------------------
# La comparaison
# ---------------------------------------------------------------------------
def erreur(st, corps):
    err = corps.get("error", {})
    if not isinstance(err, dict):
        return f"{st} {err}"
    cause = err.get("root_cause", [err])[0] if err.get("root_cause") else err
    return f"{st} {cause.get('type')}: {cause.get('reason')}"


def arrondi(x):
    return None if x is None else round(x, 4)


def interroge(base, corps, cible):
    st, body = http(base, "POST", f"/{cible}/_search", corps)
    if st != 200:
        return erreur(st, body)
    hits = [[h["_index"], h["_id"], arrondi(h.get("_score")), h.get("sort"), h.get("fields")]
            for h in body["hits"]["hits"]]
    return json.dumps([body["hits"]["total"]["value"], arrondi(body["hits"]["max_score"]),
                       hits, body.get("aggregations")], sort_keys=True)


def classe(vu):
    """Laquelle des trois issues ? C'est ce que `--table` imprime, et c'est ce
    qui dit quoi ecrire avant de l'ecrire."""
    if vu == "corpus indexe":
        return "ecrit"
    if not vu.startswith("200") and not vu.startswith("["):
        return "refuse"
    try:
        total = json.loads(vu)[0]
    except (json.JSONDecodeError, IndexError, TypeError):
        return "repond"
    return "repond" if total else "vide"


# Les ecarts assumes, chacun avec sa raison ecrite et le predicat qu'il doit
# passer (memes classes que `sonde_index_false.py`).
REFUS_ASSUMES = {
    "indexation du corpus": ("ecriture",
        "ferrite refuse `?routing=` a l'indexation (`unrecognized parameter`) — "
        "refus anterieur a cette carte, ecrit dans compat.yaml. C'est ce qui "
        "rend les lignes `_routing` mesurables **a part** : le document route "
        "vit dans son propre index"),
    "sort _doc asc": ("corps",
        "ferrite rend `[0]` pour **les deux** documents la ou ES rend `[0]` et "
        "`[1]` : son numero de document est celui du segment, pas celui de "
        "l'index. Ecart anterieur a cette carte et sans rapport avec les champs "
        "de metadonnees — l'ordre de `_doc` chez tantivy n'est deja pas celui "
        "de Lucene (voir CLAUDE.md, `_delete_by_query ?max_docs=1`), donc un "
        "numero global ne rapprocherait pas les deux. Mesure faite, pas "
        "corrigee : c'est un manque declare, pas un silence"),
    # `_seq_no` : ES sert le terme, la borne, le tri, l'agregation et la
    # colonne ; ferrite tient bien le numero mais ne l'expose pas en clause.
    # Le refus est explicite et nomme — c'est ce qui le separe du vide de la
    # carte.
    "fields _seq_no": ("refus2",
        "les deux refusent : ES rend un **500** "
        "(`Cannot fetch values for internal field [_seq_no].`), ferrite un 400 "
        "qui nomme le champ. Un 500 ne se reproduit pas"),
    "fields _source": ("refus2", "meme 500 chez ES, meme refus nomme chez ferrite"),
    "fields _routing": ("corps",
        "les deux repondent ; ce qui differe est le **corpus**, pas la lecture — "
        "l'index `sonde-meta-r` porte deux documents chez ES et un seul chez "
        "ferrite, qui refuse `?routing=` a l'indexation (voir la ligne "
        "`indexation du corpus`)"),
    "fields _field_names": ("es_casse",
        "ES rend un **500** (`Cannot fetch values for internal field "
        "[_field_names].`) ; ferrite rend 200 sans la cle, ce qu'ES rend "
        "lui-meme pour tout champ absent. Un 500 ne se reproduit pas"),
}

# Ce que ferrite ne sert pas, champ par champ, avec la raison ecrite une fois.
# C'est un **cout de perimetre** : ES repond, ferrite refuse explicitement — et
# le predicat verifie qu'il refuse, jamais qu'il rend un resultat en silence.
HORS_PERIMETRE = {
    "_routing": "ferrite est mono-shard et refuse `?routing=` a l'indexation : "
                "aucun document n'en porte, et le corpus le montre — l'index "
                "`sonde-meta-r` n'a qu'un document chez lui contre deux chez ES. "
                "Rendre vide serait indiscernable d'un document sans routage",
    "_seq_no": "le numero de sequence est tenu par ferrite (il sert de condition "
               "de concurrence) mais n'est pas interrogeable en clause ; ES y "
               "repond en `term`, `range`, tri et agregation",
    "_ignored": "ferrite ne tient pas la liste des valeurs qu'un `ignore_above` a "
                "ecartees — manque anterieur a cette carte, deja ecrit dans "
                "compat.yaml et dans `src/fetch.rs`",
}
# Les libelles viennent de la batterie elle-meme plutot que d'une liste
# recopiee : un cas ajoute ne peut pas sortir de cette table en silence.
for _libelle, _, _, _champ in cas_par_champ():
    if _champ in HORS_PERIMETRE:
        REFUS_ASSUMES.setdefault(_libelle, ("refus", HORS_PERIMETRE[_champ]))

# Les trois lectures de valeur — tri, agregation, colonne — sur les champs de
# metadonnees qu'ES sait servir et pas ferrite. Refus declare, et il n'est pas
# le meme que le precedent : ici c'est la **lecture** qui manque, pas le champ.
LECTURES = ("sort {f} asc", "agg terms {f}", "docvalue_fields {f}")
for _f in ("_index", "_type", "_version"):
    for _forme in LECTURES:
        REFUS_ASSUMES.setdefault(_forme.format(f=_f), ("refus",
            f"ES lit une valeur de `{_f}` par cette voie ; ferrite refuse en la "
            "nommant plutot que de rendre une colonne qu'il n'a pas. Cout de "
            "perimetre declare — la clause de requete, elle, est servie"))

# Et les cas ou les **deux** refusent, avec des phrases differentes.
for _f in META:
    REFUS_ASSUMES.setdefault(f"sort {_f} asc", ("refus2",
        "les deux refusent le tri sur ce champ ; ES cite sa classe Java "
        "(`fielddata`), ferrite dit qu'aucun mapping ne porte le champ"))
    REFUS_ASSUMES.setdefault(f"agg terms {_f}", ("refus2",
        "les deux refusent, phrase propre a chacun"))
    REFUS_ASSUMES.setdefault(f"docvalue_fields {_f}", ("refus2",
        "les deux refusent, phrase propre a chacun"))
    REFUS_ASSUMES.setdefault(f"match_phrase_prefix {_f}", ("refus2",
        "les deux refusent la phrase a prefixe ; le message d'ES nomme parfois "
        "l'autre moitie de la regle (`does not support match queries`)"))
REFUS_ASSUMES.setdefault("sort _id asc (avec _doc en second)", ("refus2",
    "meme refus que `sort _id asc`, avec une seconde cle"))


def ordre_ex_aequo(reps):
    """Les deux serveurs rendent **les memes documents** et ne different que par
    l'ordre de ceux qu'ES lui-meme classe ex aequo.

    Ce n'est pas une tolerance en bloc sur l'ordre : c'est le meme predicat que
    `diff_relevance.py` applique depuis toujours. ES departage les scores egaux
    par son numero de document interne, tantivy par le sien, et les deux ne sont
    pas le meme ordre (mesure : un `_bulk` de 25 documents ressort en `d002,
    d000, d003, d001`). Un ecart d'ordre entre documents dont ES donne des
    scores **differents** reste un ecart, et un ecart sur la valeur du tableau
    `sort` aussi.
    """
    try:
        charges = [json.loads(vu) for _, vu in reps]
    except json.JSONDecodeError:
        return False
    totaux, maxs, listes, aggs = zip(*charges)
    if len(set(totaux)) > 1 or len({json.dumps(a, sort_keys=True) for a in aggs}) > 1:
        return False
    if len({json.dumps(m) for m in maxs}) > 1:
        return False
    # Memes hits, a l'ordre pres.
    cles = [sorted(json.dumps(h, sort_keys=True) for h in liste) for liste in listes]
    if len({json.dumps(c) for c in cles}) > 1:
        return False
    # Et les documents dont l'ordre bouge portent tous le **meme** score cote
    # ES : sans cette moitie, le predicat couvrirait un vrai defaut de tri.
    bouges = {h[1] for a, b in zip(listes[0], listes[1]) if a != b for h in (a, b)}
    scores = {h[2] for h in listes[-1] if h[1] in bouges}
    tris = {json.dumps(h[3]) for h in listes[-1] if h[1] in bouges}
    return len(scores) <= 1 and len(tris) <= 1


ORDRE = ("ordre",
         "memes documents, memes scores, memes agregations : seul l'ordre de "
         "documents qu'ES classe **ex aequo** differe. ES departage par son "
         "numero de document interne, tantivy par le sien — ecart anterieur et "
         "general, deja assume par `diff_relevance.py`")


def assume(libelle, reps):
    """Rend `(classe, raison)` si l'ecart est assume, `None` sinon."""
    if ordre_ex_aequo(reps):
        return ORDRE
    classe_, raison = REFUS_ASSUMES.get(libelle, (None, None))
    ok = False
    if classe_ == "message":
        ok = len({vu.split(":")[0] for _, vu in reps}) == 1 and all(
            not vu.startswith(("200", "[")) for _, vu in reps)
    elif classe_ == "corps":
        ok = all(vu.startswith(("200", "[")) for _, vu in reps)
    elif classe_ == "refus":
        ok = not reps[0][1].startswith(("200", "["))
    elif classe_ == "refus2":
        # Les **deux** refusent, avec des statuts ou des types differents. Le
        # predicat exige les deux moities : sans ca, il couvrirait aussi un
        # refus que ferrite prononce seul.
        ok = all(not vu.startswith(("200", "[")) for _, vu in reps)
    elif classe_ == "es_casse":
        # L'inverse : **ES** echoue en 500 la ou ferrite repond. Un 500 ne se
        # reproduit pas.
        ok = reps[0][1].startswith(("200", "[")) and reps[1][1].startswith("5")
    elif classe_ == "ecriture":
        # ferrite refuse une ecriture qu'ES accepte : les deux moities sont
        # exigees, sans quoi le predicat couvrirait aussi l'inverse.
        ok = reps[0][1] != "corpus indexe" and reps[1][1] == "corpus indexe"
    return (classe_, raison) if ok else None


def abrege(vu):
    return vu if len(vu) <= 170 else vu[:167] + "..."


def table(base):
    """Ce qu'un seul serveur rend, classe en trois issues. Ne compare rien, donc
    ne rend aucun verdict — c'est une mesure, pas un test."""
    print(f"# reference : {base} — Elasticsearch {version(base)}")
    for echec in prepare(base):
        print(f"# ecriture refusee a la preparation : {echec}")
    compte = {"repond": 0, "refuse": 0, "vide": 0}
    for libelle, corps, cible in batterie():
        vu = interroge(base, corps, cible)
        c = classe(vu)
        compte[c] += 1
        print(f"  {c:7} {libelle:44} {abrege(vu)}")
    print(f"\n{compte['repond']} repond, {compte['refuse']} refuse, {compte['vide']} vide "
          f"({sum(compte.values())} cas)")
    return 0


def main():
    argv = [a for a in sys.argv[1:] if not a.startswith("--")]
    calibrer = "--calibrer" in sys.argv
    if "--table" in sys.argv:
        return table(argv[0] if argv else "http://localhost:9201")
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
    # Le rapport dit de quoi il est la mesure : deux serveurs de la meme version
    # sont d'accord par construction, et ca ne prouve rien sur une autre.
    for nom, base in cibles:
        print(f"# {nom:7} {base} — version {version(base)}")

    # L'ecriture du corpus est une **ligne de la batterie**, pas un prealable
    # muet : c'est elle qui porte le refus de `?routing=`, et une preparation
    # qui echoue en silence ferait rougir cent lignes qui n'en parlent pas.
    ecritures = [(nom, " | ".join(prepare(base)) or "corpus indexe") for nom, base in cibles]

    ecarts = assumes = total = 0
    for libelle, corps, cible in [("indexation du corpus", None, None)] + batterie():
        if corps is None:
            reps = ecritures
        else:
            reps = [(nom, interroge(base, corps, cible)) for nom, base in cibles]
        total += 1
        if len({vu for _, vu in reps}) <= 1:
            print(f"  {classe(reps[0][1]):7} {libelle:44} {abrege(reps[0][1])}")
            continue
        assumee = assume(libelle, reps)
        if assumee:
            assumes += 1
            marque = "~"
        else:
            ecarts += 1
            marque = "*"
        print(f"{marque} {'':7} {libelle:44} " +
              "\n              ".join(f"{nom}={abrege(vu)}" for nom, vu in reps))
        if assumee:
            print(f"              assume ({assumee[0]}) : {assumee[1]}")
    print(f"\n{total - ecarts - assumes}/{total} identiques, "
          f"{assumes} ecarts assumes, {ecarts} ecarts")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
