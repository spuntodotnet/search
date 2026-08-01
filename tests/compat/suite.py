#!/usr/bin/env python3
"""Harnais de compatibilite de ferrite.

La regle du projet : la seule preuve valable qu'une fonctionnalite marche, c'est
un vrai client Elasticsearch officiel qui l'exerce. Tout ce fichier passe par
`elasticsearch-py`, jamais par du HTTP brut.

Chaque fonctionnalite livree ajoute ici un scenario. Les scenarios de refus
comptent autant que les autres : une clause non supportee doit produire une
erreur explicite, jamais un resultat partiel presente comme complet.

    python3 tests/compat/suite.py [url]      # defaut : http://localhost:9200
"""
import sys
import traceback

from elasticsearch import ApiError, Elasticsearch

URL = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
INDEX = "compat_suite"

MAPPINGS = {
    # `strict` : la suite verifie surtout des refus explicites ; le mapping
    # dynamique a ses propres scenarios.
    "dynamic": "strict",
    "properties": {
        "titre": {"type": "text"},
        "auteur": {"type": "keyword"},
        "annee": {"type": "integer"},
        "tirage": {"type": "long"},
        "note": {"type": "double"},
        "dispo": {"type": "boolean"},
        "paru": {"type": "date"},
        "tags": {"type": "keyword"},
        "resume": {"type": "text"},
    }
}

DOCS = {
    "1": {"titre": "Le Horla", "auteur": "Maupassant", "annee": 1887,
          "tirage": 120000, "note": 4.5, "dispo": True, "paru": "1887-05-17",
          "tags": ["fantastique", "nouvelle"],
          "resume": "un homme se croit habite par une presence invisible"},
    "2": {"titre": "Bel-Ami", "auteur": "Maupassant", "annee": 1885,
          "tirage": 310000, "note": 4.1, "dispo": False, "paru": "1885-04-01",
          "tags": ["roman"],
          "resume": "l ascension sociale d un arriviste dans la presse parisienne"},
    "3": {"titre": "Germinal", "auteur": "Zola", "annee": 1885,
          "tirage": 880000, "note": 4.8, "dispo": True, "paru": "1885-03-01",
          "tags": ["roman", "social"],
          "resume": "la greve des mineurs du nord de la France"},
}

RESULTS = []


def scenario(fn):
    """Enregistre un scenario ; le nom affiche est celui de la fonction."""
    RESULTS.append(fn)
    return fn


def ids(response):
    return [hit["_id"] for hit in response["hits"]["hits"]]


def refused(fn, *, status=400, contains=None):
    """Verifie qu'un appel est refuse explicitement, au format d'erreur d'ES."""
    try:
        fn()
    except ApiError as exc:
        assert exc.meta.status == status, f"status {exc.meta.status} != {status}"
        error = exc.body["error"]
        assert isinstance(error, dict), f"error doit etre un objet : {error!r}"
        assert error["type"], "error.type manquant"
        assert error["reason"], "error.reason manquant"
        assert exc.body["status"] == status, "status manquant dans le corps"
        if contains:
            assert contains in error["reason"], \
                f"reason [{error['reason']}] ne contient pas [{contains}]"
        return error
    raise AssertionError("l'appel aurait du etre refuse")


# ---------------------------------------------------------------------------
# Poignee de main
# ---------------------------------------------------------------------------

@scenario
def poignee_de_main(es):
    assert es.ping()
    info = es.info()
    assert info["version"]["number"].startswith("8."), info["version"]["number"]
    assert info["tagline"] == "You Know, for Search"
    assert info["cluster_name"]
    assert info["cluster_uuid"]
    for key in ("build_flavor", "build_type", "lucene_version",
                "minimum_wire_compatibility_version"):
        assert key in info["version"], key


@scenario
def header_x_elastic_product(es):
    # Le client 8.x refuse de parler a un serveur qui ne le pose pas : s'il a
    # repondu a `info()`, le header est la. On le verifie tout de meme
    # explicitement sur une reponse arbitraire.
    resp = es.perform_request("GET", "/")
    assert resp.meta.headers.get("x-elastic-product") == "Elasticsearch"


# ---------------------------------------------------------------------------
# Index et mapping
# ---------------------------------------------------------------------------

@scenario
def cycle_de_vie_de_l_index(es):
    es.options(ignore_status=404).indices.delete(index=INDEX)
    assert not es.indices.exists(index=INDEX)
    es.indices.create(index=INDEX, mappings=MAPPINGS,
                      settings={"number_of_shards": 1, "number_of_replicas": 0})
    assert es.indices.exists(index=INDEX)
    mapping = es.indices.get_mapping(index=INDEX)
    assert mapping[INDEX]["mappings"]["properties"]["auteur"]["type"] == "keyword"
    assert mapping[INDEX]["mappings"]["properties"]["annee"]["type"] == "integer"


@scenario
def index_deja_existant(es):
    refused(lambda: es.indices.create(index=INDEX, mappings=MAPPINGS))


@scenario
def index_inconnu(es):
    refused(lambda: es.search(index="index_qui_n_existe_pas",
                              query={"match_all": {}}), status=404)


@scenario
def mapping_dynamique(es):
    """Sans mapping declare, les champs viennent des documents — et leur type
    est devine avec les memes regles qu'Elasticsearch."""
    es.options(ignore_status=404).indices.delete(index="dyn")
    es.indices.create(index="dyn")
    es.index(index="dyn", id="1", refresh=True, document={
        "titre": "Bel-Ami", "annee": 1885, "note": 4.1,
        "dispo": True, "paru": "1885-04-01", "chiffre_en_chaine": "42",
        "tags": ["roman", "social"],
    })
    props = es.indices.get_mapping(index="dyn")["dyn"]["mappings"]["properties"]
    assert props["annee"]["type"] == "long"
    assert props["note"]["type"] == "float"
    assert props["dispo"]["type"] == "boolean"
    assert props["paru"]["type"] == "date"
    # Une chaine devient `text` + un sous-champ `.keyword`, comme chez ES.
    assert props["titre"]["type"] == "text"
    assert props["titre"]["fields"]["keyword"]["type"] == "keyword"
    assert props["titre"]["fields"]["keyword"]["ignore_above"] == 256
    # `numeric_detection` est desactive chez ES : « 42 » reste du texte.
    assert props["chiffre_en_chaine"]["type"] == "text"

    def hits(**kw):
        return sorted(h["_id"] for h in es.search(index="dyn", size=10, **kw)["hits"]["hits"])

    assert hits(query={"match": {"titre": "bel"}}) == ["1"]
    assert hits(query={"term": {"titre.keyword": "Bel-Ami"}}) == ["1"]
    assert hits(query={"range": {"annee": {"gte": 1880}}}) == ["1"]
    assert hits(query={"term": {"tags.keyword": "social"}}) == ["1"]
    assert hits(query={"range": {"paru": {"lt": "1900-01-01"}}}) == ["1"]
    es.indices.delete(index="dyn")


@scenario
def sous_objets(es):
    """Un sous-objet s'indexe par ses chemins, declare ou devine.

    Un objet n'est pas un champ : `client` n'existe pas, `client.ville` si —
    c'est le modele d'Elasticsearch, et le mapping rendu re-niche les chemins.
    """
    es.options(ignore_status=404).indices.delete(index="imbrique")
    es.indices.create(index="imbrique", mappings={"properties": {
        "titre": {"type": "text"},
        "client": {"properties": {
            "ville": {"type": "keyword"},
            "adr": {"properties": {"cp": {"type": "integer"}}},
        }},
    }})
    es.index(index="imbrique", id="1", refresh=True, document={
        "titre": "commande", "client": {"ville": "Lyon", "adr": {"cp": 69001}}})

    def hits(**kw):
        return sorted(h["_id"] for h in es.search(index="imbrique", **kw)["hits"]["hits"])

    assert hits(query={"term": {"client.ville": "Lyon"}}) == ["1"]
    assert hits(query={"range": {"client.adr.cp": {"gte": 69000}}}) == ["1"]
    assert hits(query={"match_all": {}}, sort=[{"client.ville": "asc"}]) == ["1"]
    # `_source` n'est pas touche, et son filtrage suit les chemins.
    assert es.get(index="imbrique", id="1")["_source"]["client"]["adr"]["cp"] == 69001
    assert es.search(index="imbrique", query={"match_all": {}},
                     source_includes=["client.ville"]
                     )["hits"]["hits"][0]["_source"] == {"client": {"ville": "Lyon"}}
    # Le mapping rendu est niche, pas pointe.
    props = es.indices.get_mapping(index="imbrique")["imbrique"]["mappings"]["properties"]
    assert props["client"]["properties"]["adr"]["properties"]["cp"]["type"] == "integer"
    # Un objet declare comme champ, ou l'inverse : refus explicite.
    refused(lambda: es.indices.create(index="conflit", mappings={"properties": {
        "a": {"type": "keyword"}, "a.b": {"type": "keyword"}}}),
        contains="a la fois comme champ et comme objet")
    es.indices.delete(index="imbrique")


@scenario
def sous_objets_devines(es):
    """Sans mapping, les chemins viennent des documents — et un tableau d'objets
    est aplati, exactement comme chez ES : la correspondance entre sous-champs
    d'un meme element est perdue (c'est ce que `nested` existe pour garder)."""
    es.options(ignore_status=404).indices.delete(index="imbdyn")
    es.indices.create(index="imbdyn")
    es.index(index="imbdyn", id="1", refresh=True,
             document={"l": [{"ref": "A", "qte": 5}, {"ref": "B", "qte": 20}]})
    props = es.indices.get_mapping(index="imbdyn")["imbdyn"]["mappings"]["properties"]
    assert props["l"]["properties"]["qte"]["type"] == "long"
    assert props["l"]["properties"]["ref"]["fields"]["keyword"]["type"] == "keyword"

    def total(query):
        return es.search(index="imbdyn", query=query)["hits"]["total"]["value"]

    assert total({"term": {"l.ref.keyword": "B"}}) == 1
    # A a une quantite de 5, mais le document correspond quand meme : c'est le
    # comportement d'ES pour un `object`, et il est verifie tel quel.
    assert total({"bool": {"must": [{"term": {"l.ref.keyword": "A"}},
                                    {"range": {"l.qte": {"gte": 20}}}]}}) == 1
    es.indices.delete(index="imbdyn")


@scenario
def nested(es):
    """`nested` garde la correspondance entre sous-champs d'un meme element.

    C'est toute la difference avec un `object` : « une ligne `vis` d'au moins
    20 » ne doit pas remonter un document qui a une ligne `vis` *et* une ligne
    de 20, mais jamais les deux ensemble.
    """
    es.options(ignore_status=404).indices.delete(index="cmd")
    es.indices.create(index="cmd", mappings={"properties": {
        "client": {"type": "keyword"},
        "lignes": {"type": "nested", "properties": {
            "ref": {"type": "keyword"},
            "qte": {"type": "integer"},
            "promo": {"type": "boolean"},
        }},
    }})
    es.bulk(operations=[
        {"index": {"_index": "cmd", "_id": "1"}},
        {"client": "A", "lignes": [{"ref": "vis", "qte": 5, "promo": False},
                                   {"ref": "ecrou", "qte": 20, "promo": True}]},
        {"index": {"_index": "cmd", "_id": "2"}},
        {"client": "B", "lignes": [{"ref": "vis", "qte": 30, "promo": True}]},
        {"index": {"_index": "cmd", "_id": "3"}},
        {"client": "C", "lignes": [{"ref": "ecrou", "qte": 1}]},
    ], refresh=True)

    def hits(query):
        return sorted(h["_id"] for h in es.search(index="cmd", query=query, size=10)["hits"]["hits"])

    def nest(inner):
        return {"nested": {"path": "lignes", "query": inner}}

    assert hits(nest({"term": {"lignes.ref": "vis"}})) == ["1", "2"]
    # Le coeur du sujet : seul le document 2 a une ligne `vis` d'au moins 20.
    assert hits(nest({"bool": {"must": [{"term": {"lignes.ref": "vis"}},
                                        {"range": {"lignes.qte": {"gte": 20}}}]}})) == ["2"]
    # Un element qui *n'est pas* `vis` : le document 1 en a un.
    assert hits(nest({"bool": {"must": [{"exists": {"field": "lignes.ref"}}],
                               "must_not": [{"term": {"lignes.ref": "vis"}}]}})) == ["1", "3"]
    assert hits(nest({"terms": {"lignes.ref": ["ecrou"]}})) == ["1", "3"]
    assert hits(nest({"term": {"lignes.promo": True}})) == ["1", "2"]
    # Combinable avec le reste de la requete, et negeable.
    assert hits({"bool": {"must": [{"term": {"client": "A"}},
                                   nest({"range": {"lignes.qte": {"gte": 20}}})]}}) == ["1"]
    assert hits({"bool": {"must_not": [nest({"range": {"lignes.qte": {"gte": 20}}})]}}) == ["3"]
    # Le mapping rendu porte bien le type.
    m = es.indices.get_mapping(index="cmd")["cmd"]["mappings"]["properties"]["lignes"]
    assert m["type"] == "nested" and m["properties"]["qte"]["type"] == "integer"
    es.indices.delete(index="cmd")


@scenario
def nested_refus_explicites(es):
    """Ce que `nested` ne sait pas faire doit se dire, pas s'approximer."""
    es.options(ignore_status=404).indices.delete(index="cmd2")
    es.indices.create(index="cmd2", mappings={"properties": {
        "lignes": {"type": "nested", "properties": {
            "ref": {"type": "keyword"},
            "note": {"type": "text"},
        }},
    }})
    es.index(index="cmd2", id="1", refresh=True,
             document={"lignes": [{"ref": "vis", "note": "a serrer"}]})

    def nest(inner):
        return {"nested": {"path": "lignes", "query": inner}}

    # Un sous-champ de `nested` interroge depuis la racine : ES rend 0 hit en
    # silence, ferrite le dit (voir docs/compat.md, divergences assumees).
    refused(lambda: es.search(index="cmd2", query={"term": {"lignes.ref": "vis"}}),
            contains="ne peut etre interroge que dans une clause [nested]")
    # Un `text` ne se verifie pas element par element.
    refused(lambda: es.search(index="cmd2", query=nest({"match": {"lignes.note": "serrer"}})),
            contains="ne verifie pas un champ [text]")
    # Un chemin qui n'est pas `nested`, ou un champ hors du chemin.
    refused(lambda: es.search(index="cmd2", query={"nested": {"path": "autre",
                                                             "query": {"match_all": {}}}}),
            contains="n'est pas un champ de type [nested]")
    refused(lambda: es.search(index="cmd2", query={"nested": {
        "path": "lignes", "query": {"term": {"lignes.ref": "vis"}}, "inner_hits": {}}}),
        contains="[inner_hits]")
    es.indices.delete(index="cmd2")


@scenario
def join_parent_enfant(es):
    """`join` : parent et enfant sont deux documents, reunis a la requete."""
    es.options(ignore_status=404).indices.delete(index="blog")
    es.indices.create(index="blog", mappings={"properties": {
        "titre": {"type": "text"},
        "auteur": {"type": "keyword"},
        "note": {"type": "integer"},
        "lien": {"type": "join", "relations": {"article": "commentaire"}},
    }})
    for doc_id, doc in [
        ("a1", {"titre": "le rust au quotidien", "lien": {"name": "article"}}),
        ("a2", {"titre": "article sans commentaire", "lien": {"name": "article"}}),
        ("c1", {"titre": "tres bon papier", "auteur": "zoe", "note": 5,
                "lien": {"name": "commentaire", "parent": "a1"}}),
        ("c2", {"titre": "bof", "auteur": "max", "note": 2,
                "lien": {"name": "commentaire", "parent": "a1"}}),
    ]:
        es.index(index="blog", id=doc_id, document=doc, refresh=True)

    def hits(query):
        return sorted(h["_id"] for h in es.search(index="blog", query=query, size=10)["hits"]["hits"])

    assert hits({"has_child": {"type": "commentaire",
                               "query": {"term": {"auteur": "zoe"}}}}) == ["a1"]
    assert hits({"has_child": {"type": "commentaire",
                               "query": {"match_all": {}}}}) == ["a1"]
    assert hits({"has_parent": {"parent_type": "article",
                                "query": {"match": {"titre": "rust"}}}}) == ["c1", "c2"]
    assert hits({"parent_id": {"type": "commentaire", "id": "a1"}}) == ["c1", "c2"]
    # Le champ `join` se filtre comme un `keyword`, sous son propre nom.
    assert hits({"term": {"lien": "article"}}) == ["a1", "a2"]
    assert hits({"bool": {"must": [{"term": {"lien": "article"}}],
                          "must_not": [{"has_child": {"type": "commentaire",
                                                      "query": {"match_all": {}}}}]}}) == ["a2"]
    m = es.indices.get_mapping(index="blog")["blog"]["mappings"]["properties"]["lien"]
    assert m["type"] == "join" and m["relations"] == {"article": "commentaire"}

    # Ce qui n'a pas de sens est refuse, pas devine.
    refused(lambda: es.index(index="blog", id="x",
                             document={"lien": {"name": "commentaire"}}),
            contains="[parent] est obligatoire")
    refused(lambda: es.index(index="blog", id="y",
                             document={"lien": {"name": "inconnu"}}),
            contains="relation [inconnu] inconnue")
    refused(lambda: es.search(index="blog", query={"has_child": {
        "type": "article", "query": {"match_all": {}}}}),
        contains="n'est pas un enfant")
    refused(lambda: es.search(index="blog", query={"has_child": {
        "type": "commentaire", "query": {"match_all": {}}, "score_mode": "max"}}),
        contains="score_mode")
    es.indices.delete(index="blog")


@scenario
def mapping_dynamique_preserve_l_existant(es):
    """Le point dur : tantivy fige le schema, donc ferrite change de generation
    quand un champ apparait. Les documents deja indexes doivent survivre."""
    es.options(ignore_status=404).indices.delete(index="dyn2")
    es.indices.create(index="dyn2", mappings={"properties": {"titre": {"type": "text"}}})
    es.index(index="dyn2", id="1", document={"titre": "premier"}, refresh=True)
    avant = es.get(index="dyn2", id="1")

    # Un champ inedit fait grandir le mapping.
    es.index(index="dyn2", id="2", document={"titre": "second", "annee": 1885},
             refresh=True)
    assert "annee" in es.indices.get_mapping(index="dyn2")["dyn2"]["mappings"]["properties"]

    # Le document anterieur est intact : contenu, version, et interrogeable.
    apres = es.get(index="dyn2", id="1")
    assert apres["_source"] == avant["_source"]
    assert apres["_version"] == avant["_version"]
    assert [h["_id"] for h in es.search(
        index="dyn2", query={"match": {"titre": "premier"}})["hits"]["hits"]] == ["1"]
    assert es.search(index="dyn2", query={"match_all": {}},
                     size=10)["hits"]["total"]["value"] == 2
    es.indices.delete(index="dyn2")


@scenario
def dynamic_false_et_strict(es):
    # `false` : le champ reste dans `_source` mais n'est pas interrogeable.
    es.options(ignore_status=404).indices.delete(index="dynfalse")
    es.indices.create(index="dynfalse", mappings={
        "dynamic": False, "properties": {"titre": {"type": "text"}}})
    es.index(index="dynfalse", id="1", refresh=True,
             document={"titre": "Bel-Ami", "note": 5})
    assert es.get(index="dynfalse", id="1")["_source"]["note"] == 5
    assert "note" not in es.indices.get_mapping(
        index="dynfalse")["dynfalse"]["mappings"]["properties"]
    refused(lambda: es.search(index="dynfalse", query={"term": {"note": 5}}),
            contains="note")
    es.indices.delete(index="dynfalse")

    # `strict` : le document est refuse.
    es.options(ignore_status=404).indices.delete(index="dynstrict")
    es.indices.create(index="dynstrict", mappings={
        "dynamic": "strict", "properties": {"titre": {"type": "text"}}})
    refused(lambda: es.index(index="dynstrict", id="1",
                             document={"titre": "x", "note": 5}),
            contains="note")
    es.indices.delete(index="dynstrict")


@scenario
def type_de_champ_non_supporte(es):
    refused(lambda: es.indices.create(
        index="geo", mappings={"properties": {"p": {"type": "geo_point"}}}),
        contains="geo_point")


@scenario
def parametre_de_champ_non_supporte(es):
    refused(lambda: es.indices.create(
        index="analyse", mappings={"properties": {
            "t": {"type": "text", "analyzer": "french"}}}),
        contains="analyzer")


@scenario
def multi_fields(es):
    """Le mapping que genere Elasticsearch tout seul pour une chaine :
    `text` pour chercher, `.keyword` pour trier et filtrer exactement."""
    es.options(ignore_status=404).indices.delete(index="multi")
    es.indices.create(index="multi", mappings={"properties": {
        "titre": {"type": "text",
                  "fields": {"keyword": {"type": "keyword", "ignore_above": 8}}},
        "tag": {"type": "keyword", "fields": {"texte": {"type": "text"}}},
    }})
    mapping = es.indices.get_mapping(index="multi")["multi"]["mappings"]["properties"]
    assert mapping["titre"]["fields"]["keyword"]["type"] == "keyword"
    assert mapping["titre"]["fields"]["keyword"]["ignore_above"] == 8

    es.bulk(operations=[
        {"index": {"_index": "multi", "_id": "1"}},
        {"titre": "Bel-Ami", "tag": "roman social"},
        {"index": {"_index": "multi", "_id": "2"}},
        {"titre": "Nana", "tag": "roman"},
        {"index": {"_index": "multi", "_id": "3"}},
        {"titre": "un titre beaucoup trop long", "tag": "essai"},
    ], refresh=True)

    def hits(**kw):
        return sorted(h["_id"] for h in es.search(index="multi", size=10, **kw)["hits"]["hits"])

    # Le champ analyse cherche par mot...
    assert hits(query={"match": {"titre": "bel"}}) == ["1"]
    # ...le multi-field keyword exige la valeur entiere.
    assert hits(query={"term": {"titre.keyword": "Bel-Ami"}}) == ["1"]
    assert hits(query={"term": {"titre.keyword": "bel"}}) == []
    # On peut trier sur le keyword, pas sur le text.
    assert [h["_id"] for h in es.search(index="multi", query={"match_all": {}},
                                        sort=[{"titre.keyword": "asc"}],
                                        size=10)["hits"]["hits"]][:2] == ["1", "2"]
    refused(lambda: es.search(index="multi", query={"match_all": {}},
                              sort=[{"titre": "asc"}]))
    # L'inverse marche aussi : un keyword avec un sous-champ analyse.
    assert hits(query={"match": {"tag.texte": "social"}}) == ["1"]
    assert hits(query={"term": {"tag": "roman social"}}) == ["1"]
    # `ignore_above` : la valeur trop longue n'entre pas dans le keyword.
    assert hits(query={"exists": {"field": "titre.keyword"}}) == ["1", "2"]
    assert hits(query={"exists": {"field": "titre"}}) == ["1", "2", "3"]
    es.indices.delete(index="multi")


@scenario
def multi_fields_deux_niveaux_refuses(es):
    refused(lambda: es.indices.create(index="multi2", mappings={"properties": {
        "t": {"type": "text", "fields": {
            "k": {"type": "keyword", "fields": {"encore": {"type": "text"}}}}}}}),
        contains="niveau")


@scenario
def analyzers(es):
    """Un champ peut declarer son analyzer, et `_analyze` montre le decoupage."""
    r = es.indices.analyze(analyzer="standard",
                           text="l'ascension sociale d'un arriviste")
    # La difference qui compte en francais : ES garde l'elision dans le terme.
    assert [t["token"] for t in r["tokens"]] == \
        ["l'ascension", "sociale", "d'un", "arriviste"]

    assert [t["token"] for t in es.indices.analyze(
        analyzer="whitespace", text="Bel-Ami Zola")["tokens"]] == ["Bel-Ami", "Zola"]
    assert [t["token"] for t in es.indices.analyze(
        analyzer="keyword", text="Bel-Ami Zola")["tokens"]] == ["Bel-Ami Zola"]
    # `simple` coupe sur les chiffres, `standard` non.
    assert [t["token"] for t in es.indices.analyze(
        analyzer="simple", text="version 2 du logiciel")["tokens"]] == \
        ["version", "du", "logiciel"]

    # Un champ declare son analyzer, et la recherche s'y conforme.
    es.options(ignore_status=404).indices.delete(index="analyse")
    es.indices.create(index="analyse", mappings={"properties": {
        "brut": {"type": "text", "analyzer": "whitespace"},
        "normal": {"type": "text"},
    }})
    assert es.indices.get_mapping(index="analyse")["analyse"]["mappings"] \
        ["properties"]["brut"]["analyzer"] == "whitespace"
    es.index(index="analyse", id="1", refresh=True,
             document={"brut": "Bel-Ami", "normal": "Bel-Ami"})

    def hits(**kw):
        return [h["_id"] for h in es.search(index="analyse", **kw)["hits"]["hits"]]

    # `whitespace` ne minuscule pas : « bel-ami » ne matche pas, « Bel-Ami » oui.
    assert hits(query={"match": {"brut": "Bel-Ami"}}) == ["1"]
    assert hits(query={"match": {"brut": "bel-ami"}}) == []
    # Le champ `standard` decoupe et minuscule.
    assert hits(query={"match": {"normal": "bel"}}) == ["1"]

    # `_analyze` peut aussi partir d'un champ.
    r = es.indices.analyze(index="analyse", field="brut", text="Bel-Ami Zola")
    assert [t["token"] for t in r["tokens"]] == ["Bel-Ami", "Zola"]
    es.indices.delete(index="analyse")


@scenario
def analyzers_refuses(es):
    """Les analyzers de langue portent le nom d'ES mais pas son stemmer :
    les accepter changerait silencieusement les termes indexes."""
    for nom in ("french", "english", "snowball"):
        refused(lambda n=nom: es.indices.create(
            index="an", mappings={"properties": {"t": {"type": "text", "analyzer": n}}}),
            contains=nom)
    refused(lambda: es.indices.create(
        index="an", mappings={"properties": {"t": {"type": "text",
                                                   "analyzer": "inexistant"}}}),
        contains="inexistant")
    # Un analyzer sur mesure passe par [settings.analysis], non supporte.
    refused(lambda: es.indices.create(index="an", settings={"analysis": {
        "analyzer": {"mien": {"type": "custom", "tokenizer": "standard"}}}}),
        contains="analysis")
    # `analyzer` n'a de sens que sur un champ `text`.
    refused(lambda: es.indices.create(index="an", mappings={"properties": {
        "k": {"type": "keyword", "analyzer": "standard"}}}),
        contains="analyzer")


# ---------------------------------------------------------------------------
# Ingestion
# ---------------------------------------------------------------------------

@scenario
def bulk_indexation(es):
    operations = []
    for doc_id, doc in DOCS.items():
        operations.append({"index": {"_index": INDEX, "_id": doc_id}})
        operations.append(doc)
    resp = es.bulk(operations=operations, refresh=True)
    assert resp["errors"] is False, resp
    assert len(resp["items"]) == 3
    for item in resp["items"]:
        body = item["index"]
        assert body["status"] == 201, body
        assert body["result"] == "created"
        assert body["_version"] == 1
        assert body["_shards"]["successful"] == 1
        assert "_seq_no" in body and "_primary_term" in body


@scenario
def bulk_statut_par_item(es):
    """Un item en erreur ne doit pas faire echouer les autres, et son status
    doit etre le sien."""
    resp = es.bulk(operations=[
        {"index": {"_index": INDEX, "_id": "ok1"}},
        {"titre": "Une vie", "auteur": "Maupassant", "annee": 1883},
        {"index": {"_index": INDEX, "_id": "ko1"}},
        {"titre": "Sans mapping", "champ_absent_du_mapping": 1},
        {"create": {"_index": INDEX, "_id": "1"}},
        {"titre": "conflit", "auteur": "x"},
        {"delete": {"_index": INDEX, "_id": "ok1"}},
        {"index": {"_index": "index_absent", "_id": "z"}},
        {"titre": "x"},
    ], refresh=True)
    assert resp["errors"] is True
    statuses = [list(item.values())[0]["status"] for item in resp["items"]]
    assert statuses == [201, 400, 409, 200, 404], statuses
    bodies = [list(item.values())[0] for item in resp["items"]]
    assert bodies[1]["error"]["type"], bodies[1]
    assert bodies[2]["error"]["type"] == "version_conflict_engine_exception"
    assert bodies[4]["error"]["type"] == "index_not_found_exception"
    # Le document valide a bien ete indexe puis supprime.
    assert not es.exists(index=INDEX, id="ok1")


@scenario
def bulk_metadonnee_inconnue(es):
    refused(lambda: es.bulk(operations=[
        {"delete": {"_index": INDEX, "_id": "1", "_routing": "x"}},
    ]), contains="_routing")


@scenario
def doc_unitaire(es):
    resp = es.index(index=INDEX, id="10", refresh=True,
                    document={"titre": "Nana", "auteur": "Zola", "annee": 1880})
    assert resp["result"] == "created" and resp["_version"] == 1
    resp = es.index(index=INDEX, id="10", refresh=True,
                    document={"titre": "Nana", "auteur": "Zola", "annee": 1881})
    assert resp["result"] == "updated" and resp["_version"] == 2

    doc = es.get(index=INDEX, id="10")
    assert doc["found"] is True
    assert doc["_source"]["annee"] == 1881
    assert doc["_version"] == 2

    resp = es.delete(index=INDEX, id="10", refresh=True)
    assert resp["result"] == "deleted"
    assert not es.exists(index=INDEX, id="10")

    manquant = es.options(ignore_status=404).get(index=INDEX, id="10")
    assert manquant["found"] is False


@scenario
def compte_et_mget(es):
    """`_count` et `_mget` : les deux appels que fait toute application."""
    assert es.count(index=INDEX)["count"] == 3
    assert es.count(index=INDEX, query={"term": {"auteur": "Zola"}})["count"] == 1

    r = es.mget(index=INDEX, ids=["1", "3", "inexistant"])
    assert [d["_id"] for d in r["docs"]] == ["1", "3", "inexistant"]
    assert r["docs"][0]["found"] is True
    assert r["docs"][0]["_source"]["auteur"] == "Maupassant"
    assert r["docs"][2]["found"] is False

    r = es.mget(docs=[{"_index": INDEX, "_id": "2"}])
    assert r["docs"][0]["_source"]["titre"] == "Bel-Ami"
    r = es.mget(index=INDEX, ids=["1"], source_includes=["titre"])
    assert set(r["docs"][0]["_source"]) == {"titre"}


@scenario
def mise_a_jour_partielle(es):
    """`_update` : fusionner un fragment dans le document existant.

    Sur son propre index : ces scenarios ecrivent, et les scenarios de recherche
    comptent sur un contenu stable."""
    idx = "maj"
    es.options(ignore_status=404).indices.delete(index=idx)
    es.indices.create(index=idx, mappings={"properties": {
        "titre": {"type": "text"}, "auteur": {"type": "keyword"},
        "annee": {"type": "integer"}, "note": {"type": "double"}}})
    es.index(index=idx, id="1", refresh=True,
             document={"titre": "Une vie", "auteur": "Maupassant", "annee": 1883})

    r = es.update(index=idx, id="1", doc={"annee": 1884}, refresh=True)
    assert r["result"] == "updated" and r["_version"] == 2, r
    doc = es.get(index=idx, id="1")["_source"]
    # Le champ modifie change, les autres sont conserves.
    assert doc["annee"] == 1884 and doc["titre"] == "Une vie"

    # Une mise a jour sans effet est un `noop`, sans nouvelle version.
    r = es.update(index=idx, id="1", doc={"annee": 1884}, refresh=True)
    assert r["result"] == "noop" and r["_version"] == 2, r

    # Un champ absent du document est ajoute.
    es.update(index=idx, id="1", doc={"note": 3.5}, refresh=True)
    assert es.get(index=idx, id="1")["_source"]["note"] == 3.5

    # Document absent : erreur, sauf upsert.
    refused(lambda: es.update(index=idx, id="2", doc={"annee": 1}), status=404)
    r = es.update(index=idx, id="2", doc={"annee": 1900},
                  upsert={"titre": "Neuf", "auteur": "X", "annee": 1900}, refresh=True)
    assert r["result"] == "created"
    r = es.update(index=idx, id="3", doc={"titre": "Auto", "auteur": "Y"},
                  doc_as_upsert=True, refresh=True)
    assert r["result"] == "created"
    assert es.get(index=idx, id="3")["_source"]["titre"] == "Auto"

    # Les scripts ne sont pas supportes.
    refused(lambda: es.update(index=idx, id="1",
                              script={"source": "ctx._source.annee++"}))
    es.indices.delete(index=idx)


@scenario
def bulk_update(es):
    idx = "majbulk"
    es.options(ignore_status=404).indices.delete(index=idx)
    es.indices.create(index=idx, mappings={"properties": {
        "titre": {"type": "text"}, "annee": {"type": "integer"}}})
    es.index(index=idx, id="1", document={"titre": "avant", "annee": 1880}, refresh=True)

    resp = es.bulk(operations=[
        {"update": {"_index": idx, "_id": "1"}}, {"doc": {"annee": 1888}},
        {"update": {"_index": idx, "_id": "2"}},
        {"doc": {"titre": "Cree par upsert"}, "doc_as_upsert": True},
    ], refresh=True)
    assert resp["errors"] is False, resp
    assert [i["update"]["result"] for i in resp["items"]] == ["updated", "created"]
    assert es.get(index=idx, id="1")["_source"]["annee"] == 1888
    assert es.get(index=idx, id="1")["_source"]["titre"] == "avant"
    es.indices.delete(index=idx)


@scenario
def concurrence_optimiste(es):
    """`if_seq_no` / `if_primary_term` : ecrire seulement si rien n'a bouge."""
    idx = "conc"
    es.options(ignore_status=404).indices.delete(index=idx)
    es.indices.create(index=idx, mappings={"properties": {"titre": {"type": "text"}}})
    r = es.index(index=idx, id="1", document={"titre": "A"}, refresh=True)
    seq, term = r["_seq_no"], r["_primary_term"]

    # Avec les bonnes valeurs, l'ecriture passe.
    es.index(index=idx, id="1", document={"titre": "B"},
             if_seq_no=seq, if_primary_term=term, refresh=True)
    # Avec les anciennes, elle est refusee.
    refused(lambda: es.index(index=idx, id="1", document={"titre": "C"},
                             if_seq_no=seq, if_primary_term=term),
            status=409)
    assert es.get(index=idx, id="1")["_source"]["titre"] == "B"
    es.indices.delete(index=idx)


@scenario
def ajout_de_champs_au_mapping(es):
    """`PUT /{index}/_mapping` : ajouter un champ, pas en changer le type."""
    es.options(ignore_status=404).indices.delete(index="evolutif")
    es.indices.create(index="evolutif", mappings={
        "dynamic": "strict", "properties": {"titre": {"type": "text"}}})
    es.index(index="evolutif", id="1", document={"titre": "avant"}, refresh=True)

    es.indices.put_mapping(index="evolutif", properties={"couleur": {"type": "keyword"}})
    assert "couleur" in es.indices.get_mapping(
        index="evolutif")["evolutif"]["mappings"]["properties"]

    # Le document anterieur survit, et le champ neuf est utilisable.
    assert es.get(index="evolutif", id="1")["_source"]["titre"] == "avant"
    es.index(index="evolutif", id="2", document={"titre": "apres", "couleur": "rouge"},
             refresh=True)
    assert [h["_id"] for h in es.search(index="evolutif", query={
        "term": {"couleur": "rouge"}})["hits"]["hits"]] == ["2"]

    # Changer le type d'un champ existant reste refuse.
    refused(lambda: es.indices.put_mapping(
        index="evolutif", properties={"titre": {"type": "keyword"}}))
    es.indices.delete(index="evolutif")


@scenario
def create_en_conflit(es):
    es.options(ignore_status=404).delete(index=INDEX, id="11", refresh=True)
    es.create(index=INDEX, id="11", refresh=True,
              document={"titre": "Pot-Bouille", "auteur": "Zola"})
    refused(lambda: es.create(index=INDEX, id="11",
                              document={"titre": "x", "auteur": "y"}),
            status=409)
    es.delete(index=INDEX, id="11", refresh=True)


@scenario
def id_genere_par_le_serveur(es):
    resp = es.index(index=INDEX, document={"titre": "Anonyme", "auteur": "?"},
                    refresh=True)
    assert resp["_id"]
    assert es.get(index=INDEX, id=resp["_id"])["found"]
    es.delete(index=INDEX, id=resp["_id"], refresh=True)


@scenario
def champ_absent_du_mapping_refuse(es):
    """L'index de la suite est en `dynamic: strict`."""
    error = refused(lambda: es.index(index=INDEX, id="x",
                                     document={"inconnu": "valeur"}))
    assert "inconnu" in error["reason"]


@scenario
def valeur_incoherente_refusee(es):
    refused(lambda: es.index(index=INDEX, id="x",
                             document={"titre": "t", "annee": "pas un entier"}))
    refused(lambda: es.index(index=INDEX, id="x",
                             document={"titre": "t", "paru": "hier"}))
    # `integer` a des bornes : 2^40 n'y rentre pas.
    refused(lambda: es.index(index=INDEX, id="x",
                             document={"titre": "t", "annee": 1099511627776}))


@scenario
def refresh_rend_visible(es):
    """Sans `refresh`, l'ecriture n'est pas encore visible ; `_refresh` la rend
    visible."""
    es.index(index=INDEX, id="20", document={"titre": "Differe", "auteur": "Zola"})
    es.indices.refresh(index=INDEX)
    assert "20" in ids(es.search(index=INDEX, query={"term": {"titre": "differe"}}))
    es.delete(index=INDEX, id="20", refresh=True)


@scenario
def get_est_temps_reel(es):
    """Un `get` voit une ecriture non rafraichie, comme chez ES."""
    es.index(index=INDEX, id="21", document={"titre": "Temps reel", "auteur": "Zola"})
    assert es.get(index=INDEX, id="21")["_source"]["titre"] == "Temps reel"
    es.delete(index=INDEX, id="21", refresh=True)


# ---------------------------------------------------------------------------
# Recherche
# ---------------------------------------------------------------------------

@scenario
def recherche_match(es):
    r = es.search(index=INDEX, query={"match": {"resume": "presse"}})
    assert r["hits"]["total"] == {"value": 1, "relation": "eq"}
    assert ids(r) == ["2"]
    assert r["hits"]["hits"][0]["_score"] > 0
    assert r["hits"]["max_score"] == r["hits"]["hits"][0]["_score"]
    assert r["timed_out"] is False
    assert r["_shards"] == {"total": 1, "successful": 1, "skipped": 0, "failed": 0}
    assert isinstance(r["took"], int)


@scenario
def recherche_match_multi_termes(es):
    # Par defaut `match` est un OU sur les termes analyses.
    r = es.search(index=INDEX, query={"match": {"titre": "bel ami"}})
    assert ids(r) == ["2"]
    r = es.search(index=INDEX, query={
        "match": {"resume": {"query": "presse mineurs", "operator": "or"}}})
    assert sorted(ids(r)) == ["2", "3"]
    r = es.search(index=INDEX, query={
        "match": {"resume": {"query": "presse parisienne", "operator": "and"}}})
    assert ids(r) == ["2"]
    r = es.search(index=INDEX, query={
        "match": {"resume": {"query": "presse mineurs", "operator": "and"}}})
    assert ids(r) == []


@scenario
def recherche_multi_match(es):
    """La clause d'une barre de recherche : un mot, plusieurs champs."""
    # « presse » n'est que dans un resume, « bel » que dans un titre.
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "presse", "fields": ["titre", "resume"]}})) == ["2"]
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "germinal", "fields": ["titre", "resume"]}})) == ["3"]
    assert sorted(ids(es.search(index=INDEX, query={
        "multi_match": {"query": "greve presse", "fields": ["titre", "resume"]}}))) == ["2", "3"]
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "greve presse", "fields": ["titre", "resume"],
                        "operator": "and"}})) == []
    # Ponderation d'un champ, et les deux strategies de score.
    for requete in (
        {"multi_match": {"query": "bel", "fields": ["titre^3", "resume"]}},
        {"multi_match": {"query": "bel", "fields": ["titre", "resume"],
                         "type": "most_fields"}},
        {"multi_match": {"query": "bel", "fields": ["titre", "resume"],
                         "tie_breaker": 0.3}},
    ):
        assert ids(es.search(index=INDEX, query=requete)) == ["2"], requete

    # `best_fields` prend le meilleur champ, pas la somme : un document qui
    # porte le terme dans les deux champs ne doit pas doubler son score.
    un_champ = es.search(index=INDEX, query={
        "multi_match": {"query": "maupassant", "fields": ["auteur"]}})
    deux_champs = es.search(index=INDEX, query={
        "multi_match": {"query": "maupassant", "fields": ["auteur", "titre"]}})
    assert deux_champs["hits"]["max_score"] == un_champ["hits"]["max_score"], \
        "best_fields doit garder le meilleur score, pas additionner"


@scenario
def multi_match_refus(es):
    refused(lambda: es.search(index=INDEX, query={
        "multi_match": {"query": "x", "fields": ["titre"], "type": "cross_fields"}}),
        contains="cross_fields")
    refused(lambda: es.search(index=INDEX, query={
        "multi_match": {"query": "x", "fields": ["tit*"]}}), contains="motifs")
    refused(lambda: es.search(index=INDEX, query={"multi_match": {"query": "x"}}),
            contains="fields")


@scenario
def recherche_match_phrase(es):
    """Les mots dans cet ordre, cote a cote — pas juste tous presents."""
    assert ids(es.search(index=INDEX, query={
        "match_phrase": {"resume": "la presse parisienne"}})) == ["2"]
    # Les memes mots dans le desordre ne matchent pas...
    assert ids(es.search(index=INDEX, query={
        "match_phrase": {"resume": "parisienne presse"}})) == []
    # ...alors qu'un `match` ordinaire, si.
    assert ids(es.search(index=INDEX, query={
        "match": {"resume": {"query": "parisienne presse", "operator": "and"}}})) == ["2"]
    assert ids(es.search(index=INDEX, query={
        "match_phrase": {"resume": "greve des mineurs"}})) == ["3"]
    # Une phrase d'un seul mot reste valide.
    assert ids(es.search(index=INDEX, query={
        "match_phrase": {"resume": "parisienne"}})) == ["2"]
    # Sur un keyword, la phrase est la valeur entiere.
    assert sorted(ids(es.search(index=INDEX, query={
        "match_phrase": {"auteur": "Maupassant"}}))) == ["1", "2"]


@scenario
def match_phrase_slop_refuse(es):
    """`slop` est refuse : tantivy et Lucene ne le comptent pas pareil au-dela
    de deux termes, et ferrite rendrait moins de documents qu'ES en silence."""
    refused(lambda: es.search(index=INDEX, query={
        "match_phrase": {"resume": {"query": "la presse", "slop": 2}}}),
        contains="slop")


@scenario
def recherche_exists(es):
    """`exists` sur chaque famille de champ, y compris `text` (qui n'a pas de
    fast field et passe donc par l'index inverse)."""
    tous = sorted(ids(es.search(index=INDEX, query={"match_all": {}}, size=100)))
    for champ in ("titre", "resume", "auteur", "annee", "note", "dispo", "paru", "tags"):
        assert sorted(ids(es.search(index=INDEX, query={"exists": {"field": champ}},
                                    size=100))) == tous, champ

    # Un document sans le champ ne doit pas ressortir.
    es.index(index=INDEX, id="40", refresh=True,
             document={"titre": "Sans auteur ni note", "annee": 1900})
    trouves = ids(es.search(index=INDEX, query={"exists": {"field": "note"}}, size=100))
    assert "40" not in trouves
    assert "40" in ids(es.search(index=INDEX, query={"exists": {"field": "annee"}},
                                 size=100))
    # Une valeur nulle explicite compte comme absente, comme chez ES.
    es.index(index=INDEX, id="41", refresh=True,
             document={"titre": "Note nulle", "note": None})
    assert "41" not in ids(es.search(index=INDEX, query={"exists": {"field": "note"}},
                                     size=100))
    refused(lambda: es.search(index=INDEX, query={"exists": {"field": "inconnu"}}),
            contains="inconnu")
    for doc_id in ("40", "41"):
        es.delete(index=INDEX, id=doc_id, refresh=True)


@scenario
def recherche_match_all(es):
    r = es.search(index=INDEX, query={"match_all": {}}, size=100)
    assert r["hits"]["total"]["value"] == 3


@scenario
def recherche_term_et_terms(es):
    assert sorted(ids(es.search(index=INDEX,
                                query={"term": {"auteur": "Maupassant"}}))) == ["1", "2"]
    assert ids(es.search(index=INDEX, query={"term": {"auteur": "maupassant"}})) == []
    r = es.search(index=INDEX, query={"terms": {"auteur": ["Zola", "Hugo"]}})
    assert ids(r) == ["3"]
    # Un keyword multi-valeur se cherche comme un scalaire.
    assert sorted(ids(es.search(index=INDEX,
                                query={"term": {"tags": "roman"}}))) == ["2", "3"]


@scenario
def recherche_range(es):
    assert sorted(ids(es.search(index=INDEX, query={
        "range": {"annee": {"gte": 1880, "lt": 1886}}}))) == ["2", "3"]
    assert ids(es.search(index=INDEX, query={
        "range": {"annee": {"gt": 1885}}})) == ["1"]
    assert sorted(ids(es.search(index=INDEX, query={
        "range": {"note": {"gte": 4.5}}}))) == ["1", "3"]
    assert sorted(ids(es.search(index=INDEX, query={
        "range": {"tirage": {"gt": 200000}}}))) == ["2", "3"]
    assert ids(es.search(index=INDEX, query={
        "range": {"paru": {"gte": "1887-01-01"}}})) == ["1"]


@scenario
def recherche_booleen(es):
    assert sorted(ids(es.search(index=INDEX,
                                query={"term": {"dispo": True}}))) == ["1", "3"]
    assert ids(es.search(index=INDEX, query={"term": {"dispo": False}})) == ["2"]


@scenario
def recherche_par_motif(es):
    """`ids`, `prefix`, `wildcard`, `fuzzy` — non analysees, comme chez ES."""
    assert sorted(ids(es.search(index=INDEX, query={"ids": {"values": ["1", "3"]}}))) == \
        ["1", "3"]
    assert sorted(ids(es.search(index=INDEX,
                                query={"prefix": {"auteur": "Maup"}}))) == ["1", "2"]
    # `prefix` ne passe pas par l'analyzer : la casse compte sur un keyword.
    assert ids(es.search(index=INDEX, query={"prefix": {"auteur": "maup"}})) == []
    assert ids(es.search(index=INDEX, query={"wildcard": {"auteur": "Z*la"}})) == ["3"]
    assert ids(es.search(index=INDEX, query={"wildcard": {"auteur": "Zol?"}})) == ["3"]
    # `fuzzy` : une faute de frappe est rattrapee.
    assert ids(es.search(index=INDEX, query={"fuzzy": {"auteur": "Zolx"}})) == ["3"]
    assert ids(es.search(index=INDEX,
                         query={"fuzzy": {"auteur": {"value": "Zoll", "fuzziness": 1}}})) == ["3"]


@scenario
def requetes_composees(es):
    """`constant_score` et `dis_max`."""
    r = es.search(index=INDEX, query={
        "constant_score": {"filter": {"term": {"auteur": "Zola"}}, "boost": 2.0}})
    assert ids(r) == ["3"]
    assert r["hits"]["hits"][0]["_score"] == 2.0

    # `dis_max` garde le meilleur score, il ne les additionne pas.
    seul = es.search(index=INDEX, query={"match": {"resume": "presse"}})
    combine = es.search(index=INDEX, query={"dis_max": {"queries": [
        {"match": {"resume": "presse"}}, {"term": {"auteur": "Maupassant"}}]}})
    assert combine["hits"]["max_score"] >= seul["hits"]["max_score"]
    assert sorted(ids(combine)) == ["1", "2"]


@scenario
def recherche_bool(es):
    r = es.search(index=INDEX, query={"bool": {
        "must": [{"match": {"titre": "bel ami"}}],
        "filter": [{"term": {"auteur": "Maupassant"}},
                   {"range": {"annee": {"gte": 1880, "lt": 1886}}}]}})
    assert ids(r) == ["2"]

    assert sorted(ids(es.search(index=INDEX, query={"bool": {
        "must_not": [{"term": {"auteur": "Zola"}}]}}))) == ["1", "2"]

    assert sorted(ids(es.search(index=INDEX, query={"bool": {"should": [
        {"term": {"auteur": "Zola"}},
        {"term": {"auteur": "Maupassant"}}]}}))) == ["1", "2", "3"]

    assert ids(es.search(index=INDEX, query={"bool": {
        "should": [{"term": {"auteur": "Maupassant"}},
                   {"range": {"annee": {"gte": 1887}}}],
        "minimum_should_match": 2}})) == ["1"]

    # Le contexte `filter` ne doit pas contribuer au score.
    avec = es.search(index=INDEX, query={"bool": {
        "must": [{"match": {"resume": "presse"}}],
        "filter": [{"term": {"auteur": "Maupassant"}}]}})
    sans = es.search(index=INDEX, query={"match": {"resume": "presse"}})
    assert avec["hits"]["hits"][0]["_score"] == sans["hits"]["hits"][0]["_score"]


@scenario
def tri(es):
    r = es.search(index=INDEX, query={"match_all": {}}, sort=[{"annee": "desc"}])
    assert ids(r) == ["1", "2", "3"]
    # Avec un tri, ES annule les scores et expose les cles de tri.
    assert r["hits"]["max_score"] is None
    assert all(hit["_score"] is None for hit in r["hits"]["hits"])
    assert r["hits"]["hits"][0]["sort"] == [1887]

    assert ids(es.search(index=INDEX, query={"match_all": {}},
                         sort=[{"auteur": "asc"}])) == ["1", "2", "3"]
    assert ids(es.search(index=INDEX, query={"match_all": {}},
                         sort=[{"note": "desc"}])) == ["3", "1", "2"]
    assert ids(es.search(index=INDEX, query={"match_all": {}},
                         sort=[{"paru": "asc"}])) == ["3", "2", "1"]
    # Tri multi-cles : annee croissante, puis note decroissante.
    assert ids(es.search(index=INDEX, query={"match_all": {}},
                         sort=[{"annee": "asc"}, {"note": "desc"}])) == ["3", "2", "1"]


@scenario
def tri_sur_champ_text_refuse(es):
    refused(lambda: es.search(index=INDEX, query={"match_all": {}},
                              sort=[{"titre": "asc"}]))


@scenario
def pagination(es):
    tous = ids(es.search(index=INDEX, query={"match_all": {}},
                         sort=[{"annee": "asc"}], size=10))
    page = ids(es.search(index=INDEX, query={"match_all": {}},
                         sort=[{"annee": "asc"}], from_=1, size=1))
    assert page == tous[1:2]
    r = es.search(index=INDEX, query={"match_all": {}}, size=0)
    assert r["hits"]["total"]["value"] == 3 and r["hits"]["hits"] == []
    # ES ne calcule pas de score quand aucun document n'est demande.
    assert r["hits"]["max_score"] is None
    # Au-dela du dernier hit : page vide, mais le meilleur score reste rendu.
    r = es.search(index=INDEX, query={"match_all": {}}, from_=50, size=2)
    assert r["hits"]["hits"] == [] and r["hits"]["max_score"] > 0
    # Aucun resultat : pas de score.
    r = es.search(index=INDEX, query={"term": {"auteur": "Personne"}})
    assert r["hits"]["total"]["value"] == 0 and r["hits"]["max_score"] is None


@scenario
def pagination_profonde_refusee(es):
    refused(lambda: es.search(index=INDEX, query={"match_all": {}},
                              from_=10000, size=10))


@scenario
def taille_invalide_refusee(es):
    """`size: -1` doit se voir, pas retomber en silence sur le defaut."""
    refused(lambda: es.perform_request(
        "POST", f"/{INDEX}/_search",
        headers={"content-type": "application/json"},
        body={"query": {"match_all": {}}, "size": -1}))


@scenario
def filtrage_de_source(es):
    r = es.search(index=INDEX, query={"match_all": {}}, sort=[{"annee": "desc"}],
                  size=2, source_includes=["titre"])
    assert [h["_source"]["titre"] for h in r["hits"]["hits"]] == ["Le Horla", "Bel-Ami"]
    assert all(set(h["_source"]) == {"titre"} for h in r["hits"]["hits"])

    r = es.search(index=INDEX, query={"match_all": {}}, size=1,
                  source={"excludes": ["resume", "tags"]})
    assert "resume" not in r["hits"]["hits"][0]["_source"]
    assert "titre" in r["hits"]["hits"][0]["_source"]

    r = es.search(index=INDEX, query={"match_all": {}}, size=1, source=False)
    assert "_source" not in r["hits"]["hits"][0]

    doc = es.get(index=INDEX, id="1", source_includes=["auteur"])
    assert doc["_source"] == {"auteur": "Maupassant"}


@scenario
def clause_de_dsl_inconnue_refusee(es):
    refused(lambda: es.search(index=INDEX,
                              query={"clause_inexistante": {"titre": "x"}}),
            contains="clause_inexistante")
    refused(lambda: es.search(index=INDEX, query={"regexp": {"auteur": "Mau.*"}}),
            contains="regexp")
    refused(lambda: es.search(index=INDEX,
                              query={"query_string": {"query": "titre:bel"}}),
            contains="query_string")
    refused(lambda: es.search(index=INDEX, query={"boosting": {
        "positive": {"match_all": {}}, "negative": {"term": {"auteur": "Zola"}},
        "negative_boost": 0.5}}), contains="boosting")


@scenario
def parametre_de_clause_non_supporte_refuse(es):
    refused(lambda: es.search(index=INDEX, query={
        "match": {"titre": {"query": "bel", "fuzziness": "AUTO"}}}),
        contains="fuzziness")
    refused(lambda: es.search(index=INDEX, query={"bool": {
        "should": [{"term": {"auteur": "Zola"}}],
        "minimum_should_match": "75%"}}),
        contains="minimum_should_match")


@scenario
def champ_non_mappe_refuse(es):
    """Divergence assumee : ES renvoie 0 hit, ferrite refuse.

    Sans mapping dynamique, interroger un champ inconnu est une erreur du
    client ; repondre « 0 resultat » serait un resultat faux presente comme
    complet."""
    refused(lambda: es.search(index=INDEX, query={"term": {"inconnu": "x"}}),
            contains="inconnu")


@scenario
def fonctionnalites_hors_perimetre_refusees(es):
    refused(lambda: es.search(index=INDEX, query={"match_all": {}},
                              highlight={"fields": {"resume": {}}}),
            contains="highlight")
    refused(lambda: es.search(index=INDEX, query={"match_all": {}},
                              search_after=[1885], sort=[{"annee": "asc"}]),
            contains="search_after")
    refused(lambda: es.search(index=INDEX, q="titre:bel"), contains="q")
    refused(lambda: es.search(query={"match_all": {}}))


@scenario
def agregations(es):
    """Les facettes : compter, regrouper, calculer — sur les documents qui
    correspondent, pas sur la page rendue."""
    r = es.search(index=INDEX, size=0, query={"match_all": {}}, aggs={
        "par_auteur": {"terms": {"field": "auteur"}},
        "annee_min": {"min": {"field": "annee"}},
        "annee_max": {"max": {"field": "annee"}},
        "note": {"stats": {"field": "note"}},
    })
    a = r["aggregations"]
    # L'agregation porte sur tous les documents, meme avec size=0.
    assert r["hits"]["total"]["value"] == 3
    assert r["hits"]["hits"] == []
    buckets = {b["key"]: b["doc_count"] for b in a["par_auteur"]["buckets"]}
    assert buckets == {"Maupassant": 2, "Zola": 1}
    # ES expose toujours ces deux compteurs sur un `terms`.
    assert a["par_auteur"]["doc_count_error_upper_bound"] == 0
    assert a["par_auteur"]["sum_other_doc_count"] == 0
    assert a["annee_min"]["value"] == 1885
    assert a["annee_max"]["value"] == 1887
    assert a["note"]["count"] == 3
    assert a["note"]["min"] == 4.1

    # `size` tronque, et les documents ecartes sont comptes.
    r = es.search(index=INDEX, size=0, aggs={
        "f": {"terms": {"field": "auteur", "size": 1}}})
    assert len(r["aggregations"]["f"]["buckets"]) == 1
    assert r["aggregations"]["f"]["sum_other_doc_count"] == 1

    # L'agregation suit la requete.
    r = es.search(index=INDEX, size=0, query={"term": {"auteur": "Zola"}},
                  aggs={"f": {"terms": {"field": "auteur"}}})
    assert [b["key"] for b in r["aggregations"]["f"]["buckets"]] == ["Zola"]

    # Sous-agregations.
    r = es.search(index=INDEX, size=0, aggs={
        "par_auteur": {"terms": {"field": "auteur"},
                       "aggs": {"annee_moyenne": {"avg": {"field": "annee"}}}}})
    b = {x["key"]: x["annee_moyenne"]["value"]
         for x in r["aggregations"]["par_auteur"]["buckets"]}
    assert b["Zola"] == 1885 and b["Maupassant"] == 1886

    # Buckets de plages et d'histogramme.
    r = es.search(index=INDEX, size=0, aggs={
        "plages": {"range": {"field": "annee", "ranges": [
            {"to": 1886}, {"from": 1886}]}}})
    plages = {b["key"]: b["doc_count"] for b in r["aggregations"]["plages"]["buckets"]}
    assert plages == {"*-1886.0": 2, "1886.0-*": 1}

    # Une metrique de date rend des millisecondes et sa forme lisible.
    r = es.search(index=INDEX, size=0, aggs={"d": {"min": {"field": "paru"}}})
    assert r["aggregations"]["d"]["value_as_string"].startswith("1885-03-01T")


@scenario
def agregations_refusees(es):
    # Agreger sur un `text` n'a pas de sens sans fielddata — ES refuse aussi.
    refused(lambda: es.search(index=INDEX, size=0,
                              aggs={"f": {"terms": {"field": "titre"}}}),
            contains="titre")
    # Champ inconnu.
    refused(lambda: es.search(index=INDEX, size=0,
                              aggs={"f": {"terms": {"field": "inconnu"}}}),
            contains="inconnu")
    # Agregation hors perimetre.
    refused(lambda: es.search(index=INDEX, size=0,
                              aggs={"f": {"percentiles": {"field": "annee"}}}),
            contains="percentiles")
    # Refus assumes, avec la raison.
    refused(lambda: es.search(index=INDEX, size=0,
                              aggs={"f": {"cardinality": {"field": "auteur"}}}),
            contains="cardinality")
    refused(lambda: es.search(index=INDEX, size=0,
                              aggs={"f": {"filter": {"term": {"auteur": "Zola"}}}}),
            contains="filter")
    # Parametre non supporte : jamais avale en silence.
    refused(lambda: es.search(index=INDEX, size=0, aggs={
        "f": {"terms": {"field": "auteur", "include": "Z.*"}}}),
        contains="include")
    # Ordonner par une sous-agregation n'est pas supporte.
    refused(lambda: es.search(index=INDEX, size=0, aggs={
        "f": {"terms": {"field": "auteur", "order": {"m": "desc"}},
              "aggs": {"m": {"avg": {"field": "annee"}}}}}))
    # Une metrique ne porte pas de sous-agregations.
    refused(lambda: es.search(index=INDEX, size=0, aggs={
        "m": {"avg": {"field": "annee"}, "aggs": {"x": {"max": {"field": "annee"}}}}}))


@scenario
def parametre_inconnu_refuse(es):
    """Un parametre ignore en silence, c'est une demande du client perdue."""
    refused(lambda: es.search(index=INDEX, query={"match_all": {}},
                              routing="abc"),
            contains="routing")
    # Ces parametres n'ont de sens qu'avec des motifs multi-index, que ferrite
    # ne supporte pas : les accepter laisserait croire qu'ils font quelque chose.
    refused(lambda: es.search(index=INDEX, query={"match_all": {}},
                              ignore_unavailable=True),
            contains="ignore_unavailable")
    refused(lambda: es.cat.indices(index=INDEX, format="json", h="index"),
            contains="h")


@scenario
def parametres_honores(es):
    """Les parametres acceptes doivent vraiment agir."""
    # op_type=create : meme semantique que _create.
    es.options(ignore_status=404).delete(index=INDEX, id="30", refresh=True)
    es.index(index=INDEX, id="30", op_type="create", refresh=True,
             document={"titre": "L Assommoir", "auteur": "Zola"})
    refused(lambda: es.index(index=INDEX, id="30", op_type="create",
                             document={"titre": "x", "auteur": "y"}),
            status=409)
    es.delete(index=INDEX, id="30", refresh=True)

    # ignore_unavailable sur une suppression d'index absent.
    es.indices.delete(index="index_jamais_cree", ignore_unavailable=True)
    refused(lambda: es.indices.delete(index="index_jamais_cree"), status=404)


# ---------------------------------------------------------------------------
# Cluster
# ---------------------------------------------------------------------------

@scenario
def routes_de_cluster(es):
    health = es.cluster.health()
    assert health["status"] == "green"
    assert health["number_of_nodes"] == 1
    assert health["timed_out"] is False
    assert health["unassigned_shards"] == 0

    rows = list(es.cat.indices(index=INDEX, format="json"))
    assert rows and rows[0]["index"] == INDEX
    assert rows[0]["health"] == "green" and rows[0]["status"] == "open"
    assert rows[0]["pri"] == "1" and rows[0]["rep"] == "0"
    assert int(rows[0]["docs.count"]) == 3

    nodes = es.nodes.info()
    assert nodes["_nodes"]["total"] == 1
    node = list(nodes["nodes"].values())[0]
    assert node["version"] == es.info()["version"]["number"]

    # Une sous-ressource de `_nodes` demande une autre reponse : la confondre
    # avec `/_nodes` serait rendre le mauvais document en silence.
    refused(lambda: es.nodes.stats())


@scenario
def route_inconnue_refusee(es):
    try:
        es.perform_request("GET", "/une/route/inconnue")
    except ApiError as exc:
        assert exc.meta.status == 400, exc.meta.status
        assert "no handler found" in str(exc.body["error"])
    else:
        raise AssertionError("une route inconnue doit etre refusee")

    # Un nom d'index reserve n'est pas un index absent, comme chez ES.
    refused(lambda: es.perform_request("GET", "/_route_reservee"),
            contains="must not start with")
    # Un motif multi-index dit pourquoi il est refuse.
    refused(lambda: es.search(index="compat_*", query={"match_all": {}}),
            contains="motifs")


# ---------------------------------------------------------------------------

def main():
    es = Elasticsearch(URL, request_timeout=30)
    failures = []
    for fn in RESULTS:
        name = fn.__name__.replace("_", " ")
        try:
            fn(es)
            print(f"[  ok  ] {name}")
        except Exception:  # noqa: BLE001 - on veut la suite complete
            failures.append(fn.__name__)
            print(f"[ echec] {name}")
            print("".join("        " + l for l in
                          traceback.format_exc().splitlines(keepends=True)))
    es.options(ignore_status=404).indices.delete(index=INDEX)

    print()
    total = len(RESULTS)
    print(f"{total - len(failures)}/{total} scenarios de compatibilite passes")
    if failures:
        print("echecs : " + ", ".join(failures))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
