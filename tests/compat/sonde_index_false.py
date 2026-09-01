#!/usr/bin/env python3
"""Sonde : `index: false`, le champ qu'on garde sans le rendre cherchable.

`index: false` n'est pas « un parametre a accepter » : c'est une **famille de
consequences**, et aucune ne se devine. Ce fichier pose les memes questions aux
deux serveurs — type par type, operation par operation — et compare ce qui
repond, ce qui echoue, avec quel type d'erreur et quelle phrase.

Ce que la mesure a montre, et qui contredit ce que le refus d'avant supposait :

* sur un `keyword`, un `long`, un `double`, une `date` ou un `boolean`,
  `index: false` **laisse le champ cherchable**. ES n'y renonce pas : il execute
  la clause sur les *doc values* (`term`, `terms`, `range`, `match`, `prefix`,
  `wildcard`, `regexp`, `exists`, et le tri, les agregations, `fields`,
  `docvalue_fields`). `_field_caps` l'annonce `searchable: true` ;
* le seul changement observable est le **score** : une clause qui notait par
  BM25 (`term` et `match` sur un `keyword`) rend un score **constant de 1.0**,
  parce qu'une colonne ne porte ni frequence ni longueur de champ ;
* sur un `text`, il n'y a pas de colonne : le champ n'est plus cherchable du
  tout, et ES refuse la clause en `query_shard_exception`
  (`Cannot search on field [x] since it is not indexed.`) — sauf `exists`, qui
  rend 200 et **aucun** document, et le surlignage, qui ne rend aucun fragment.

    python3 tests/compat/sonde_index_false.py [ferrite] [es]
    python3 tests/compat/sonde_index_false.py --calibrer [es_a] [es_b]

Elle **refuse de tourner** si elle ne trouve pas ses deux cibles : une sonde
differentielle qui ne compare rien rendrait « tout identique ».
"""
import json
import sys
import urllib.error
import urllib.request

IDX = "sonde-index-false"
# Les index jetables des cas de mapping : chacun est cree puis relu, donc il ne
# peut pas servir deux fois.
MAP = "sonde-index-false-map"

TYPES = ["keyword", "text", "long", "double", "date", "boolean"]


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


# ---------------------------------------------------------------------------
# Le corpus. Chaque type est mappe deux fois — `_off` et `_on` — pour que
# chaque question ait sa temoin : ce qui differe entre les deux colonnes d'une
# meme ligne est l'effet d'`index: false`, et rien d'autre.
# ---------------------------------------------------------------------------
PROPRIETES = {}
for _t in TYPES:
    PROPRIETES[f"{_t}_off"] = {"type": _t, "index": False}
    PROPRIETES[f"{_t}_on"] = {"type": _t}
PROPRIETES.update({
    # Un champ non indexe mais stocke : c'est le cas de l'utilisateur qui veut
    # relire la valeur sans la chercher.
    "store_off": {"type": "keyword", "index": False, "store": True},
    # `ignore_above` sur un champ non indexe : la valeur trop longue sort-elle
    # quand meme du champ ?
    "ia_off": {"type": "keyword", "index": False, "ignore_above": 3},
    # Un multi-field non indexe sous un parent qui l'est.
    "multi": {"type": "keyword", "fields": {"pas": {"type": "keyword", "index": False}}},
    # Les deux sens de `copy_to` : depuis un champ non indexe, et vers un champ
    # non indexe.
    "src_off": {"type": "keyword", "index": False, "copy_to": ["cible_on"]},
    "cible_on": {"type": "keyword"},
    "src_on": {"type": "keyword", "copy_to": ["cible_off"]},
    "cible_off": {"type": "keyword", "index": False},
    # Un `text` non indexe qui declare quand meme son analyzer : ES l'accepte.
    "texte_off_an": {"type": "text", "index": False, "analyzer": "french"},
    # Non indexe **et** stocke : la seule forme ou ES 8.15 casse (voir plus bas).
    "texte_off_store": {"type": "text", "index": False, "store": True},
})

VALEURS = {
    "keyword": "alpha",
    "text": "le vif renard brun saute",
    "long": 42,
    "double": 1.5,
    "date": "2026-03-15T00:00:00.000Z",
    "boolean": True,
}

DOCS = {
    # Le document complet : c'est lui que toutes les clauses doivent trouver.
    "1": dict(
        {f"{t}_off": VALEURS[t] for t in TYPES},
        **{f"{t}_on": VALEURS[t] for t in TYPES},
        store_off="stockee", ia_off="trop-long", multi="mm",
        src_off="copiee", src_on="vers-off", texte_off_an="les ascensions",
        texte_off_store="note stockee",
    ),
    # Un second jeu de valeurs, pour que `range`, `prefix` et le tri aient de
    # quoi trancher.
    "2": {"keyword_off": "beta", "keyword_on": "beta", "long_off": 7, "long_on": 7,
          "double_off": -2.5, "double_on": -2.5, "date_off": "2020-01-01",
          "date_on": "2020-01-01", "boolean_off": False, "boolean_on": False,
          "text_off": "le loup gris", "text_on": "le loup gris"},
    # Multivalue et dans le desordre : une colonne rend ses valeurs triees, un
    # index inverse aussi — mais `fields` lit le `_source`.
    "3": {"keyword_off": ["gamma", "alpha"], "keyword_on": ["gamma", "alpha"],
          "long_off": [9, 3], "long_on": [9, 3]},
    # Le document qui n'a **aucun** de ces champs : c'est lui qui mesure
    # `exists`, `missing` et les sentinelles de tri.
    "4": {"autre": "rien"},
}


def prepare(base):
    http(base, "DELETE", "/" + IDX)
    for suffixe in SUFFIXES_MAP:
        http(base, "DELETE", f"/{MAP}-{suffixe}")
    http(base, "PUT", "/" + IDX, {"mappings": {"properties": PROPRIETES}})
    lignes = []
    for id_, doc in DOCS.items():
        lignes.append(json.dumps({"index": {"_index": IDX, "_id": id_}}))
        lignes.append(json.dumps(doc))
    http(base, "POST", "/_bulk?refresh=true", "\n".join(lignes) + "\n", ndjson=True)


# ---------------------------------------------------------------------------
# Les questions
# ---------------------------------------------------------------------------
def cas_recherche():
    """(libelle, corps de `_search`)."""
    out = []
    for t in TYPES:
        f, v = f"{t}_off", VALEURS[t]
        out.append((f"term {f}", {"query": {"term": {f: v}}}))
        out.append((f"terms {f}", {"query": {"terms": {f: [v]}}}))
        out.append((f"match {f}", {"query": {"match": {f: v}}}))
        out.append((f"match_phrase {f}", {"query": {"match_phrase": {f: v}}}))
        out.append((f"exists {f}", {"query": {"exists": {"field": f}}}))
        out.append((f"must_not exists {f}",
                    {"query": {"bool": {"must_not": [{"exists": {"field": f}}]}}}))
        borne = {"keyword": "a", "text": "a", "long": 7, "double": 0,
                 "date": "2021-01-01", "boolean": "true"}[t]
        out.append((f"range {f} gte", {"query": {"range": {f: {"gte": borne}}}}))
        out.append((f"sort {f} asc", {"sort": [{f: "asc"}], "_source": False}))
        out.append((f"sort {f} desc missing=_first",
                    {"sort": [{f: {"order": "desc", "missing": "_first"}}],
                     "_source": False}))
        out.append((f"agg terms {f}", {"size": 0, "aggs": {"a": {"terms": {"field": f}}}}))
        out.append((f"fields {f}", {"fields": [f], "_source": False}))
        out.append((f"docvalue_fields {f}",
                    {"docvalue_fields": [f], "_source": False}))
        out.append((f"highlight {f}", {"query": {"match_all": {}},
                                       "highlight": {"fields": {f: {}}}}))
        out.append((f"boost 3 term {f}",
                    {"query": {"term": {f: {"value": v, "boost": 3}}}}))
        out.append((f"constant_score {f}",
                    {"query": {"constant_score": {"filter": {"term": {f: v}},
                                                  "boost": 2}}}))
    # Les clauses de motif : elles n'ont de sens que sur une chaine.
    for f in ("keyword_off", "text_off"):
        out.append((f"prefix {f}", {"query": {"prefix": {f: "al"}}}))
        out.append((f"wildcard {f}", {"query": {"wildcard": {f: "al*"}}}))
        out.append((f"regexp {f}", {"query": {"regexp": {f: "al.*|be.*"}}}))
        out.append((f"fuzzy {f}", {"query": {"fuzzy": {f: "alpga"}}}))
        out.append((f"match_phrase_prefix {f}",
                    {"query": {"match_phrase_prefix": {f: "al"}}}))
    # La temoin : les memes questions sur les champs indexes. C'est elle qui dit
    # que ce qui bouge vient d'`index: false` et pas d'autre chose.
    for t in TYPES:
        f, v = f"{t}_on", VALEURS[t]
        out.append((f"[temoin] term {f}", {"query": {"term": {f: v}}}))
        out.append((f"[temoin] exists {f}", {"query": {"exists": {"field": f}}}))
        out.append((f"[temoin] sort {f} asc",
                    {"sort": [{f: "asc"}], "_source": False}))
    # Le score : c'est la seule chose qu'`index: false` change sur un champ qui
    # garde sa colonne, et rien d'autre ne le mesure.
    out.append(("score term keyword_off vs _on",
                {"query": {"bool": {"should": [{"term": {"keyword_off": "alpha"}}]}}}))
    out.append(("score match keyword_off",
                {"query": {"match": {"keyword_off": "alpha"}}}))

    # Les clauses composees : un champ non indexe ne doit pas vider la clause
    # qui l'entoure.
    out.append(("bool should off+on",
                {"query": {"bool": {"should": [{"term": {"keyword_off": "alpha"}},
                                               {"term": {"keyword_on": "beta"}}]}}}))
    out.append(("bool must_not term keyword_off",
                {"query": {"bool": {"must_not": [{"term": {"keyword_off": "alpha"}}]}}}))
    out.append(("bool must_not term text_off",
                {"query": {"bool": {"must_not": [{"term": {"text_off": "loup"}}]}}}))
    out.append(("multi_match keyword_off+keyword_on",
                {"query": {"multi_match": {"query": "alpha",
                                           "fields": ["keyword_off", "keyword_on"]}}}))
    out.append(("multi_match text_off+text_on",
                {"query": {"multi_match": {"query": "loup",
                                           "fields": ["text_off", "text_on"]}}}))
    out.append(("multi_match text_off+text_on lenient",
                {"query": {"multi_match": {"query": "loup", "lenient": True,
                                           "fields": ["text_off", "text_on"]}}}))
    out.append(("dis_max off+on",
                {"query": {"dis_max": {"queries": [{"term": {"keyword_off": "alpha"}},
                                                   {"term": {"keyword_on": "alpha"}}]}}}))

    # Les voisins de mapping : `store`, `ignore_above`, multi-field, `copy_to`.
    out.append(("term store_off", {"query": {"term": {"store_off": "stockee"}}}))
    # `exists` sur un `text` a la fois non indexe **et** stocke : ES 8.15 y rend
    # un **500** (son `FieldExistsQuery` ne se construit pas sur un champ qui
    # n'a ni colonne, ni norme, ni vecteur), alors que le meme champ sans
    # `store` rend 200 et aucun document. ferrite rend la seconde reponse dans
    # les deux cas : un 500 ne se reproduit pas. Trouve par le fuzzer, graine
    # 6660022.
    out.append(("exists texte_off_store (ES 8.15 casse)",
                {"query": {"exists": {"field": "texte_off_store"}}}))
    out.append(("stored_fields store_off",
                {"stored_fields": ["store_off"], "_source": False}))
    out.append(("fields store_off", {"fields": ["store_off"], "_source": False}))
    out.append(("term ia_off (valeur trop longue)",
                {"query": {"term": {"ia_off": "trop-long"}}}))
    out.append(("fields ia_off", {"fields": ["ia_off"], "_source": False}))
    out.append(("term multi.pas", {"query": {"term": {"multi.pas": "mm"}}}))
    out.append(("term cible_on (copie depuis un champ non indexe)",
                {"query": {"term": {"cible_on": "copiee"}}}))
    out.append(("term cible_off (copie vers un champ non indexe)",
                {"query": {"term": {"cible_off": "vers-off"}}}))
    out.append(("fields cible_off", {"fields": ["cible_off"], "_source": False}))
    out.append(("term texte_off_an", {"query": {"term": {"texte_off_an": "ascens"}}}))
    # Ce que le **surlignage** fait d'un champ non indexe, clause par clause.
    # La regle n'est pas « il ne marque rien » : seule la famille des automates
    # marque (`terms`, `prefix`, `wildcard`, `regexp`, `fuzzy`), parce que
    # Lucene les extrait de la requete sans rien demander a l'index. Un `term`,
    # un `match` ou un `range` n'y marquent rien — et `no_match_size` s'applique
    # quand meme. Deux clauses voisines, deux reponses : c'est mesure, et c'est
    # le fuzzer qui l'a sorti (graines 9310029 et 9310045).
    for nom, q in [
        ("term", {"term": {"keyword_off": "alpha"}}),
        ("terms", {"terms": {"keyword_off": ["alpha"]}}),
        ("match", {"match": {"keyword_off": "alpha"}}),
        ("prefix", {"prefix": {"keyword_off": "al"}}),
        ("wildcard", {"wildcard": {"keyword_off": "al*"}}),
        ("regexp", {"regexp": {"keyword_off": "al.*"}}),
        ("fuzzy", {"fuzzy": {"keyword_off": {"value": "alpha", "fuzziness": 0}}}),
        ("range", {"range": {"keyword_off": {"gte": "a"}}}),
        ("exists", {"exists": {"field": "keyword_off"}}),
    ]:
        out.append((f"highlight keyword_off sous {nom}",
                    {"query": q, "highlight": {"fields": {"keyword_off": {}}}}))
    out.append(("highlight no_match_size sur non indexe",
                {"query": {"match_all": {}},
                 "highlight": {"no_match_size": 5,
                               "fields": {"keyword_off": {}, "text_off": {}}}}))

    # La phrase sur un `text` non indexe : le refus depend du **nombre de
    # termes**, parce que c'est Lucene qui parle et pas le verificateur de
    # mapping. Trois reponses pour la meme clause (fuzzer, graine 9310061).
    for clause in ("match", "match_phrase", "match_phrase_prefix"):
        for texte in ("", "!!!", "renard", "renard brun"):
            out.append((f"{clause} text_off {texte!r}",
                        {"query": {clause: {"text_off": texte}}}))
    out.append(("term text_off vide", {"query": {"term": {"text_off": ""}}}))

    # Les 24 combinaisons de bornes d'un `range` sur un `boolean` non indexe.
    # Chez ES, un `lt` y **efface le reste de l'intervalle** : `{"gt": true,
    # "lt": false}` rend les documents a `false`. Ce bord ne vaut que pour le
    # booleen (mesure type par type) et il rendrait **moins** de documents qu'ES
    # sans lui. Trouve par le fuzzer (graine 5550060) ; la temoin indexee, sur
    # la ligne d'a cote, dit que c'est bien l'absence d'index qui le produit.
    bornes_bool = []
    for bas in [None, ("gt", False), ("gt", True), ("gte", False), ("gte", True)]:
        for haut in [None, ("lt", False), ("lt", True), ("lte", False), ("lte", True)]:
            if bas is None and haut is None:
                continue
            bornes_bool.append(dict([b for b in (bas, haut) if b]))
    for bornes in bornes_bool:
        out.append((f"range boolean_off {json.dumps(bornes, sort_keys=True)}",
                    {"query": {"range": {"boolean_off": bornes}},
                     "_source": False}))
        out.append((f"[temoin] range boolean_on {json.dumps(bornes, sort_keys=True)}",
                    {"query": {"range": {"boolean_on": bornes}}, "_source": False}))

    # `case_insensitive` : le seul motif qu'ES refuse sur une colonne. Son
    # automate de `regexp` y est construit sans les drapeaux de correspondance
    # — mais `prefix` et `wildcard`, qui passent par un autre chemin, les
    # gardent. Trouve par le fuzzer (graine 9310016).
    out.append(("regexp keyword_off case_insensitive",
                {"query": {"regexp": {"keyword_off": {"value": "AL.*",
                                                      "case_insensitive": True}}}}))
    out.append(("prefix keyword_off case_insensitive",
                {"query": {"prefix": {"keyword_off": {"value": "AL",
                                                      "case_insensitive": True}}}}))
    out.append(("wildcard keyword_off case_insensitive",
                {"query": {"wildcard": {"keyword_off": {"value": "AL*",
                                                        "case_insensitive": True}}}}))

    out.append(("fields *", {"fields": ["*"], "_source": False}))
    out.append(("_source complet", {"query": {"ids": {"values": ["1"]}}}))

    # Les agregations qui ne sont pas un `terms` : elles lisent la meme colonne.
    out.append(("agg stats long_off",
                {"size": 0, "aggs": {"a": {"stats": {"field": "long_off"}}}}))
    out.append(("agg range long_off",
                {"size": 0, "aggs": {"a": {"range": {"field": "long_off",
                                                     "ranges": [{"to": 8}, {"from": 8}]}}}}))
    out.append(("agg date_histogram date_off",
                {"size": 0, "aggs": {"a": {"date_histogram": {
                    "field": "date_off", "calendar_interval": "year"}}}}))
    out.append(("agg terms keyword_off missing",
                {"size": 0, "aggs": {"a": {"terms": {"field": "keyword_off",
                                                     "missing": "vide"}}}}))
    return out


def cas_lecture():
    """(libelle, methode, chemin, corps) — ce que le mapping et `_field_caps`
    rendent. Un aller-retour de mapping qui perd `index: false` ferait
    reindexer un client qui relit son propre mapping."""
    return [
        ("GET _mapping", "GET", f"/{IDX}/_mapping", None),
        # Champ par champ, et pas `fields=*` : ES y ajoute ses champs de
        # metadonnees (`_id`, `_seq_no`, `_data_stream_timestamp`…) que ferrite
        # ne rend pas, et cet ecart-la n'est pas celui qu'on mesure ici.
        ("GET _field_caps", "GET",
         f"/{IDX}/_field_caps?fields=" + ",".join(sorted(PROPRIETES)), None),
        ("GET _doc/1", "GET", f"/{IDX}/_doc/1", None),
        ("_validate term keyword_off", "POST",
         f"/{IDX}/_validate/query?explain=true",
         {"query": {"term": {"keyword_off": "alpha"}}}),
        ("_validate term text_off", "POST",
         f"/{IDX}/_validate/query?explain=true",
         {"query": {"term": {"text_off": "loup"}}}),
        ("_validate exists text_off", "POST",
         f"/{IDX}/_validate/query?explain=true",
         {"query": {"exists": {"field": "text_off"}}}),
        ("_ignored sur un champ non indexe", "POST", f"/{IDX}/_search",
         {"query": {"ids": {"values": ["1"]}}, "_source": False}),
        ("_count term keyword_off", "POST", f"/{IDX}/_count",
         {"query": {"term": {"keyword_off": "alpha"}}}),
        ("_delete_by_query term keyword_off (dry via _count)", "POST",
         f"/{IDX}/_count", {"query": {"term": {"keyword_off": "beta"}}}),
    ]


# Les cas de mapping : chacun cree son propre index, puis le relit. Le suffixe
# sert a ne jamais reutiliser un nom (un index deja cree repondrait
# « already exists » au lieu de la vraie erreur).
CAS_MAPPING = [
    ("index: false booleen", "a", {"k": {"type": "keyword", "index": False}}),
    ("index: true booleen", "b", {"k": {"type": "keyword", "index": True}}),
    ('index: "false" chaine', "c", {"k": {"type": "keyword", "index": "false"}}),
    ('index: "true" chaine', "d", {"k": {"type": "keyword", "index": "true"}}),
    ('index: "no" (l\'ecriture de la 2.x)', "e", {"k": {"type": "keyword", "index": "no"}}),
    ("index: 0", "f", {"k": {"type": "keyword", "index": 0}}),
    ("index: false sur un object", "g", {"o": {"type": "object", "index": False}}),
    ("index: false sur un nested", "h", {"o": {"type": "nested", "index": False}}),
    ("index: false sous un nested", "i",
     {"o": {"type": "nested", "properties": {"k": {"type": "keyword", "index": False}}}}),
    ("index: false + store", "j", {"k": {"type": "keyword", "index": False, "store": True}}),
    ("index: false + copy_to", "k", {"k": {"type": "keyword", "index": False,
                                           "copy_to": ["c"]}, "c": {"type": "keyword"}}),
    ("index: false + analyzer sur un text", "l",
     {"t": {"type": "text", "index": False, "analyzer": "french"}}),
    ("index: false + search_analyzer sur un text", "m",
     {"t": {"type": "text", "index": False, "search_analyzer": "french"}}),
    ("index: false + ignore_above", "n",
     {"k": {"type": "keyword", "index": False, "ignore_above": 3}}),
    ("index: false dans un multi-field", "o",
     {"k": {"type": "keyword", "fields": {"pas": {"type": "keyword", "index": False}}}}),
    ("index: false sur chaque type", "p",
     {f"{t}_off": {"type": t, "index": False} for t in TYPES}),
]
SUFFIXES_MAP = [s for _, s, _ in CAS_MAPPING] + ["maj"]


def joue_mapping(base, suffixe, props):
    """Cree l'index, puis relit le mapping qu'il rend : les deux comptent. Un
    `PUT` accepte qui perd le parametre serait vert sur le seul statut."""
    nom = f"{MAP}-{suffixe}"
    http(base, "DELETE", "/" + nom)
    st, corps = http(base, "PUT", "/" + nom, {"mappings": {"properties": props}})
    if st != 200:
        return erreur(st, corps)
    st2, relu = http(base, "GET", f"/{nom}/_mapping")
    return "200 " + json.dumps(relu.get(nom, {}).get("mappings", {}), sort_keys=True)


def joue_maj(base):
    """Changer `index` sur un champ deja declare : ES refuse dans les deux sens.
    Ajouter un champ non indexe apres coup, lui, passe."""
    nom = f"{MAP}-maj"
    http(base, "DELETE", "/" + nom)
    http(base, "PUT", "/" + nom, {"mappings": {"properties": {
        "on": {"type": "keyword"}, "off": {"type": "keyword", "index": False}}}})
    out = []
    for libelle, props in [
        ("on -> false", {"on": {"type": "keyword", "index": False}}),
        ("off -> true", {"off": {"type": "keyword", "index": True}}),
        ("off -> false (identique)", {"off": {"type": "keyword", "index": False}}),
        ("ajout d'un champ non indexe", {"neuf": {"type": "keyword", "index": False}}),
    ]:
        st, corps = http(base, "PUT", f"/{nom}/_mapping", {"properties": props})
        out.append(f"{libelle} = " + (str(st) if st == 200 else erreur(st, corps)))
    st, relu = http(base, "GET", f"/{nom}/_mapping")
    out.append("mapping = " + json.dumps(relu.get(nom, {}).get("mappings", {}),
                                         sort_keys=True))
    return " | ".join(out)


# ---------------------------------------------------------------------------
# La comparaison
# ---------------------------------------------------------------------------
def erreur(st, corps):
    err = corps.get("error", {})
    cause = err.get("root_cause", [err])[0] if err.get("root_cause") else err
    return f"{st} {cause.get('type')}: {cause.get('reason')}"


def arrondi(x):
    """Un score se compare a la quatrieme decimale : c'est assez fin pour
    separer un score constant (1.0) d'un BM25 (0.3885), et assez large pour ne
    pas mesurer le dernier bit de deux implementations de BM25."""
    return None if x is None else round(x, 4)


def interroge(base, corps):
    st, body = http(base, "POST", f"/{IDX}/_search", corps)
    if st != 200:
        return erreur(st, body)
    # `_ignored` ne fait pas partie de ce qui se compare ici : ferrite ne tient
    # pas la liste des champs qu'un `ignore_above` a ecartes, et c'est un ecart
    # **anterieur** a cette carte, ecrit dans `src/fetch.rs`. Le laisser entrer
    # ferait rougir cent lignes qui ne parlent pas d'`index: false` — il a donc
    # sa propre ligne, plus bas, et elle est comptee.
    hits = [[h["_id"], arrondi(h.get("_score")), h.get("sort"), h.get("fields"),
             h.get("highlight")]
            for h in body["hits"]["hits"]]
    return json.dumps([body["hits"]["total"]["value"], arrondi(body["hits"]["max_score"]),
                       hits, body.get("aggregations")], sort_keys=True)


def lit(base, methode, chemin, corps):
    st, body = http(base, methode, chemin, corps)
    if st != 200:
        return erreur(st, body)
    # Ce qui ne peut pas coincider entre deux serveurs.
    for cle in ("took", "_shards", "_seq_no", "_primary_term", "_version"):
        body.pop(cle, None)
    if "explanations" in body:
        for e in body["explanations"]:
            # L'explication cite l'uuid de l'index et la classe Java d'ES : ce
            # qui se compare est le verdict, et le fait qu'une erreur soit
            # rendue — pas le texte que la JVM y met.
            if "error" in e:
                e["error"] = "<erreur>"
            e.pop("explanation", None)
    return "200 " + json.dumps(body, sort_keys=True)


# Les ecarts assumes, chacun avec sa raison ecrite et le predicat qu'il doit
# passer.
#
# - `message` : les deux serveurs refusent, avec le meme statut et le meme type
#   d'erreur ; seule la phrase differe. C'est le cas des refus que ferrite
#   prononce dans ses propres mots depuis longtemps ;
# - `refus` : ferrite refuse la ou ES repond. C'est un cout de perimetre, et le
#   predicat verifie quand meme que ferrite **refuse explicitement** plutot que
#   de rendre un resultat en silence ;
# - `es_casse` : l'inverse — **ES** rend un 500 la ou ferrite repond. Un 500 ne
#   se reproduit pas, et le predicat exige de voir les deux moities.
REFUS_ASSUMES = {
    "sort text_off asc": ("message",
        "les deux refusent en `illegal_argument_exception` (fielddata) ; ferrite "
        "ecrit sa propre phrase, et c'est un ecart anterieur a cette carte — il "
        "vaut pour un `text` indexe comme non indexe"),
    "sort text_off desc missing=_first": ("message", "meme refus que la ligne au-dessus"),
    "agg terms text_off": ("message",
        "meme refus de fielddata, meme phrase propre a ferrite"),
    "exists texte_off_store (ES 8.15 casse)": ("es_casse",
        "ES 8.15 rend un **500** (`FieldExistsQuery requires that the field "
        "indexes doc values, norms or vectors`) sur un `text` a la fois non "
        "indexe et stocke ; ferrite rend 200 et aucun document, ce qu'ES rend "
        "lui-meme des qu'on retire le `store`. Un 500 ne se reproduit pas"),
    "_ignored sur un champ non indexe": ("corps",
        "ES liste dans `_ignored` les champs qu'un `ignore_above` a ecartes, y "
        "compris sur un champ non indexe ; ferrite ne tient pas cette liste — "
        "ecart anterieur a cette carte, ecrit dans `src/fetch.rs` et dans "
        "compat.yaml"),
    # Les cinq lignes qui suivent sont des **temoins** : elles portent sur des
    # champs indexes, et l'ecart y est anterieur a cette carte. Mesure faite
    # contre le binaire d'avant, sur un index a quatre documents : un `term` sur
    # un `keyword` indexe y rend deja 0.693 (ln 2) la ou ES rend 0.523. tantivy
    # et Lucene ne calculent pas le meme idf ; l'**ordre**, lui, est le meme —
    # c'est ce que `diff_relevance.py` mesure, et c'est pourquoi personne ne
    # l'avait vu : aucune sonde ne comparait un `term` en **valeur**.
    "[temoin] term keyword_on": ("corps",
        "BM25 : ecart anterieur, mesure contre le binaire d'avant (0.693 contre "
        "0.523 sur le meme index). Sans rapport avec `index: false` — c'est "
        "justement ce que la ligne `term keyword_off` rend identique, puisqu'une "
        "colonne se lit a score constant"),
    "[temoin] term boolean_on": ("corps", "meme ecart de BM25, sur un booleen"),
    "bool should off+on": ("corps",
        "la moitie indexee de ce `should` porte le meme ecart de BM25 ; la "
        "moitie non indexee, elle, est identique au bit pres"),
    "multi_match text_off+text_on lenient": ("corps",
        "le champ non indexe est bien ecarte par `lenient` des deux cotes — ce "
        "qui reste est le BM25 du champ `text` indexe"),
    "term cible_on (copie depuis un champ non indexe)": ("corps",
        "la copie arrive bien depuis un champ non indexe des deux cotes ; le "
        "score est celui du champ **indexe** qui la recoit, donc le meme ecart "
        "de BM25"),
    "[temoin] sort text_on asc": ("message",
        "meme refus de fielddata que sur le champ non indexe, meme phrase propre "
        "a ferrite : l'ecart ne vient pas d'`index: false`"),
    "agg range long_off": ("refus",
        "ferrite refuse l'agregation `range` sur un champ **multivalue** — "
        "refus anterieur, ecrit dans compat.yaml (l'agregation de tantivy y "
        "compte les valeurs, pas les documents). Sans rapport avec "
        "`index: false` : la colonne est lue de la meme facon"),
    'index: "no" (l\'ecriture de la 2.x)': ("message",
        "les deux refusent en `mapper_parsing_exception` ; ES ecrit « Failed to "
        "parse value [no] as only [true] or [false] are allowed. », ferrite "
        "nomme le champ en plus"),
    "index: 0": ("message", "meme refus, meme ecart de phrase"),
    "index: false sur un object": ("message",
        "les deux refusent en `mapper_parsing_exception` : ES parle de "
        "« unsupported parameters », ferrite nomme le parametre et le champ"),
    "index: false sur un nested": ("message", "meme refus que la ligne au-dessus"),
}


def assume(libelle, reps):
    classe = REFUS_ASSUMES.get(libelle, (None, None))[0]
    if classe == "message":
        # Meme statut **et** meme type d'erreur : sans cette seconde moitie, la
        # classe couvrirait aussi un refus que ferrite prononce seul.
        return len({vu.split(":")[0] for _, vu in reps}) == 1 and all(
            not vu.startswith("200") for _, vu in reps)
    if classe == "corps":
        # Les deux repondent, et l'ecart est **ecrit** : c'est le seul cas ou
        # une difference de corps est assumee, et il est nomme un par un.
        return all(vu.startswith(("200", "[")) for _, vu in reps)
    if classe == "refus":
        return not reps[0][1].startswith(("200", "["))
    if classe == "es_casse":
        # L'inverse du precedent, et il est le seul de son espece : c'est **ES**
        # qui echoue, en 500, la ou ferrite repond. Le predicat exige les deux
        # moities — sans quoi il couvrirait aussi un refus de ferrite.
        return reps[0][1].startswith(("200", "[")) and reps[1][1].startswith("5")
    return False


def abrege(vu):
    return vu if len(vu) <= 160 else vu[:157] + "..."


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

    batterie = []
    for libelle, suffixe, props in CAS_MAPPING:
        batterie.append((libelle, lambda base, s=suffixe, p=props: joue_mapping(base, s, p)))
    batterie.append(("mise a jour du mapping", joue_maj))
    for _, base in cibles:
        prepare(base)
    for libelle, methode, chemin, corps in cas_lecture():
        batterie.append((libelle, lambda base, m=methode, c=chemin, b=corps:
                         lit(base, m, c, b)))
    for libelle, corps in cas_recherche():
        batterie.append((libelle, lambda base, c=corps: interroge(base, c)))

    ecarts = assumes = total = 0
    for libelle, joue in batterie:
        reps = [(nom, joue(base)) for nom, base in cibles]
        total += 1
        if len({vu for _, vu in reps}) <= 1:
            print(f"  {libelle:44} {abrege(reps[0][1])}")
            continue
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
