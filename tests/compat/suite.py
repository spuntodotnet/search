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
# Une recherche qui ne vise aucun index
# ---------------------------------------------------------------------------

@scenario
def recherche_sans_index_valide_son_corps(es):
    """Zero index vise ne veut pas dire zero validation.

    Ce scenario est place **avant** tout ce qui cree un index : le cluster est
    encore vide, et c'est exactement l'etat qui n'etait pas exerce. La
    traduction du Query DSL se faisant index par index, une recherche sans
    index ne lisait pas son corps du tout et rendait 200 sur une requete que le
    premier index venu refuse — le seul echec silencieux connu du projet.

    Deux facons de ne viser aucun index, le meme chemin de code : le cluster
    vide, et le motif qui ne correspond a rien (des index quotidiens pas encore
    crees, un premier demarrage). Les deux sont couverts."""
    # La suite est proprietaire de son serveur (elle le purge deja a la fin) :
    # on ramene le cluster a l'etat qu'on veut mesurer, index par index, sans
    # dependre du reglage `action.destructive_requires_name`.
    for nom in list(es.indices.get(index="*")):
        es.indices.delete(index=nom)
    assert es.indices.get(index="*") == {}

    for cible in ({}, {"index": "ferrite-aucun-*"}):
        # Une requete valide reste une reponse vide, a l'identique d'ES 8.15 :
        # zero shard, et `max_score` a 0.0 (et non `null`, qu'ES ne rend que
        # quand un shard a repondu).
        r = es.search(**cible, query={"match_all": {}})
        assert r["hits"]["total"]["value"] == 0, r
        assert r["_shards"]["total"] == 0, r
        assert r["hits"]["max_score"] == 0.0, r
        assert "aggregations" not in r, r
        assert es.count(**cible, query={"match_all": {}})["count"] == 0

        # Un champ que personne ne mappe reste 200 : c'est un verdict de shard,
        # et il n'y a pas de shard (mesure contre ES 8.15, qui rend 200 aussi).
        assert es.search(**cible, query={"term": {"absent": "x"}})[
            "hits"]["total"]["value"] == 0
        assert es.search(**cible, sort=["absent"])["hits"]["total"]["value"] == 0
        assert es.search(**cible, aggs={"a": {"terms": {"field": "absent"}}})[
            "hits"]["total"]["value"] == 0

        # Ce qui ne se lit pas, en revanche, se refuse — ES aussi, sans index.
        refused(lambda c=cible: es.search(**c, query={"pas_une_query": {"f": "x"}}),
                contains="pas_une_query")
        refused(lambda c=cible: es.search(
            **c, query={"nested": {"path": "p", "query": {"pas_une_query": {}}}}),
                contains="pas_une_query")
        refused(lambda c=cible: es.count(**c, query={"pas_une_query": {}}),
                contains="pas_une_query")
        refused(lambda c=cible: es.search(
            **c, aggs={"a": {"terms": {"field": "f"},
                             "aggs": {"b": {"pas_une_agg": {}}}}}),
                contains="pas_une_agg")
        refused(lambda c=cible: es.search(**c, sort=[{"f": {"order": "nawak"}}]),
                contains="nawak")

        # Et ce que ferrite ne sait pas faire se refuse aussi, meme si ES le
        # sait : sans ca, un client decouvrirait la limite le jour ou il a des
        # donnees. C'est la regle qui prime dans ce depot.
        refused(lambda c=cible: es.search(
            **c, aggs={"a": {"significant_terms": {"field": "f"}}}),
                contains="significant_terms")
        refused(lambda c=cible: es.search(
            **c, query={"intervals": {"f": {"match": {"query": "x"}}}}),
                contains="intervals")


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
    # Non mappe, donc non interrogeable : la clause ne correspond a rien, comme
    # chez ES (`index.query.parse.allow_unmapped_fields`).
    assert es.search(index="dynfalse", query={"term": {"note": 5}},
                     )["hits"]["total"]["value"] == 0
    assert ids(es.search(index="dynfalse", query={"match": {"titre": "bel"}})) == ["1"]
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
            "t": {"type": "text", "analyzer": "german"}}}),
        contains="analyzer")


@scenario
def noms_de_champ_reserves(es):
    """Seuls les champs de **métadonnées** d'ES sont réservés, pas le préfixe
    `_` entier : Wagtail nomme les siens `_all_text` et `_edgengrams`, et un
    vrai ES 8.15 les accepte (mesuré, mapping relu compris)."""
    es.options(ignore_status=404).indices.delete(index="souligne")
    es.indices.create(index="souligne", mappings={"properties": {
        "_all_text": {"type": "text"},
        "_edgengrams": {"type": "text"},
        "_score": {"type": "keyword"},
    }})
    props = es.indices.get_mapping(index="souligne")["souligne"]["mappings"]["properties"]
    assert set(props) == {"_all_text", "_edgengrams", "_score"}, props
    es.index(index="souligne", id="1", refresh=True,
             document={"_all_text": "le cheval du pre", "_score": "abc"})
    assert ids(es.search(index="souligne",
                         query={"match": {"_all_text": "cheval"}})) == ["1"]
    assert ids(es.search(index="souligne", query={"term": {"_score": "abc"}})) == ["1"]
    # Et ce que la réponse transporte les voit aussi : le préfixe `_` ne fait
    # pas la métadonnée, c'est l'absence de champ qui la fait. Tant que la
    # lecture s'arrêtait au préfixe, `_all_text` était invisible à `fields` —
    # en 200 et sans un mot, sur les noms exacts que Wagtail emploie.
    hit = es.search(index="souligne", source=False,
                    fields=["_all_text", "_score"])["hits"]["hits"][0]
    assert hit["fields"] == {"_all_text": ["le cheval du pre"],
                             "_score": ["abc"]}, hit
    hit = es.search(index="souligne", source=False, fields=["*"])["hits"]["hits"][0]
    assert set(hit["fields"]) == {"_all_text", "_score"}, hit
    hit = es.search(index="souligne", source=False,
                    docvalue_fields=["_score"])["hits"]["hits"][0]
    assert hit["fields"] == {"_score": ["abc"]}, hit

    # Un champ de métadonnées, lui, reste refusé — avec le message d'ES.
    for nom in ("_id", "_source", "_seq_no"):
        refused(lambda n=nom: es.indices.create(index="souligne_ko", mappings={
            "properties": {n: {"type": "text"}}}),
            contains=f"Field [{nom}] is defined more than once")
    # Et les colonnes internes de ferrite, avec sa raison à lui : ES les
    # accepte, ferrite ne peut pas — un champ nommé `_elem` écraserait l'indice
    # d'élément d'un `nested`.
    refused(lambda: es.indices.create(index="souligne_ko", mappings={
        "properties": {"_elem": {"properties": {"x": {"type": "text"}}}}}),
        contains="colonne interne de ferrite")
    es.indices.delete(index="souligne")


@scenario
def index_vrai_est_le_defaut(es):
    """Le mapping que pose Gitea : `"index": true` sur chaque champ.

    C'est le defaut d'Elasticsearch — la valeur ne demande rien de plus que ce
    que ferrite fait deja, et ES lui-meme ne la conserve pas dans le mapping
    qu'il rend. La refuser bloquait une application entiere au demarrage, sans
    qu'aucune de ses requetes ne soit hors perimetre. `index: false`, lui,
    demande un champ absent de l'index : il reste refuse explicitement.
    """
    es.options(ignore_status=404).indices.delete(index="param_index")
    es.indices.create(index="param_index", mappings={"properties": {
        "id": {"type": "integer", "index": True},
        "titre": {"type": "text", "index": "true"},
        "tag": {"type": "keyword", "index": True,
                "fields": {"texte": {"type": "text", "index": True}}},
    }})
    # Comme ES : le parametre ne ressort pas du mapping relu.
    props = es.indices.get_mapping(index="param_index")["param_index"]["mappings"]["properties"]
    assert props["id"] == {"type": "integer"}, props["id"]
    assert "index" not in props["titre"], props["titre"]
    assert "index" not in props["tag"]["fields"]["texte"], props["tag"]

    # Et le champ est bien cherchable : accepter la valeur par defaut, c'est
    # faire ce qu'elle demande.
    es.index(index="param_index", id="1", document={"id": 7, "titre": "Bel-Ami",
                                                    "tag": "roman"}, refresh=True)
    assert ids(es.search(index="param_index", query={"match": {"titre": "bel"}})) == ["1"]
    assert ids(es.search(index="param_index", query={"term": {"id": 7}})) == ["1"]

    refused(lambda: es.indices.create(
        index="param_index_faux", mappings={"properties": {
            "t": {"type": "keyword", "index": False}}}),
        contains="index: false")
    refused(lambda: es.indices.create(
        index="param_index_bizarre", mappings={"properties": {
            "t": {"type": "keyword", "index": "no"}}}))
    es.indices.delete(index="param_index")


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
    """`french` et `english` sont mesures identiques a ES (`diff_analyzers.py`,
    210 textes) : leurs stemmers sont ceux de Lucene, portes dans
    `src/stemmer.rs`. Ce qui reste refuse, ce sont les langues dont le stemmer
    n'a pas ete porte."""
    assert [t["token"] for t in es.indices.analyze(
        analyzer="english", text="The running dogs run quickly")["tokens"]] == [
        "run", "dog", "run", "quickli"]
    assert [t["token"] for t in es.indices.analyze(
        analyzer="french", text="l'ascension des chevaux")["tokens"]] == [
        "ascension", "cheval"]
    for nom in ("german", "snowball"):
        refused(lambda n=nom: es.indices.create(
            index="an", mappings={"properties": {"t": {"type": "text", "analyzer": n}}}),
            contains=nom)
    refused(lambda: es.indices.create(
        index="an", mappings={"properties": {"t": {"type": "text",
                                                   "analyzer": "inexistant"}}}),
        contains="inexistant")
    # Un analyzer sur mesure, lui, est supporte (voir `analyzers_sur_mesure`) :
    # ce qui reste refuse, c'est ce qui repose sur un stemmer.
    refused(lambda: es.indices.create(index="an", settings={"analysis": {
        "analyzer": {"mien": {"type": "custom", "tokenizer": "standard",
                              "filter": ["french_stem"]}}}}),
        contains="french_stem")
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
        # Supprimer dans un index absent reste un 404 : seule l'ecriture cree
        # l'index a la volee, comme chez ES.
        {"delete": {"_index": "index_absent", "_id": "z"}},
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
def bulk_gros_lot(es):
    """Un lot plus gros que le defaut de la bibliotheque HTTP.

    `helpers.bulk` du client officiel decoupe a `max_chunk_bytes`, dont le
    defaut est 100 Mo — la valeur qu'ES annonce dans `GET /_nodes` sous
    `http.max_content_length_in_bytes`. Tant que la couche HTTP de ferrite
    gardait le defaut d'axum (2 Mo), il annoncait donc cinquante fois ce qu'il
    acceptait, et un `_bulk` de 5 000 documents — la taille de lot par defaut
    des tracks Rally — repartait en `413 text/plain`, hors format d'erreur d'ES.

    Le scenario mesure les deux moities : ce qu'annonce `_nodes`, et ce qui
    passe vraiment. Un chiffre annonce que rien n'exerce n'est qu'une phrase.
    """
    annonce = next(iter(es.nodes.info()["nodes"].values()))["http"][
        "max_content_length_in_bytes"]
    assert annonce == 104_857_600, annonce

    # ~6 Mo de corps utile : trois fois le defaut d'axum, en un seul appel.
    remplissage = "vergogne " * 600
    operations = []
    for i in range(1000):
        operations.append({"index": {"_index": "gros_lot", "_id": str(i)}})
        operations.append({"titre": f"document {i}", "corps": remplissage})
    resp = es.bulk(operations=operations, refresh=True)
    assert resp["errors"] is False, resp["items"][0]
    assert es.count(index="gros_lot")["count"] == 1000
    es.options(ignore_status=404).indices.delete(index="gros_lot")


@scenario
def index_cree_a_l_ecriture(es):
    """Indexer dans un index absent le cree, comme chez ES — mais lire ou
    supprimer dans un index absent reste un 404."""
    for nom in ("auto1", "auto2", "auto3"):
        es.options(ignore_status=404).indices.delete(index=nom)
    es.index(index="auto1", id="1", document={"titre": "cree a la volee"}, refresh=True)
    assert es.indices.exists(index="auto1")
    assert es.search(index="auto1", query={"match": {"titre": "volee"}}
                     )["hits"]["total"]["value"] == 1
    # `_update` avec upsert cree aussi l'index ; sans upsert, le document manque.
    es.update(index="auto2", id="1", doc={"a": 1}, doc_as_upsert=True, refresh=True)
    assert es.get(index="auto2", id="1")["_source"] == {"a": 1}
    refused(lambda: es.update(index="auto3", id="1", doc={"a": 1}), status=404,
            contains="document")
    # Le bulk aussi, sauf pour une suppression.
    r = es.bulk(operations=[{"index": {"_index": "auto4", "_id": "1"}}, {"a": 1}],
                refresh=True)
    assert r["items"][0]["index"]["status"] == 201
    refused(lambda: es.search(index="jamais_creee", query={"match_all": {}}), status=404)
    refused(lambda: es.delete(index="jamais_creee", id="1"), status=404)
    for nom in ("auto1", "auto2", "auto3", "auto4"):
        es.options(ignore_status=404).indices.delete(index=nom)


@scenario
def analyzers_sur_mesure(es):
    """`settings.analysis` : un mapping venu d'une instance réelle déclare
    presque toujours son propre analyzer, le plus souvent `standard` +
    `lowercase` + `asciifolding`."""
    es.options(ignore_status=404).indices.delete(index="ana")
    es.indices.create(index="ana", settings={"analysis": {
        "analyzer": {
            "fr_produit": {"type": "custom", "tokenizer": "standard",
                           "filter": ["lowercase", "asciifolding"]},
            "brut": {"type": "custom", "tokenizer": "keyword"},
            "sans_vides": {"type": "custom", "tokenizer": "standard",
                           "filter": ["lowercase", "mes_vides"]},
        },
        "filter": {"mes_vides": {"type": "stop", "stopwords": ["le", "la", "des"]}},
    }}, mappings={"properties": {
        "titre": {"type": "text", "analyzer": "fr_produit"},
        "code": {"type": "text", "analyzer": "brut"},
        "corps": {"type": "text", "analyzer": "sans_vides"},
    }})

    def tokens(**kw):
        return [t["token"] for t in es.indices.analyze(index="ana", **kw)["tokens"]]

    # Les accents sont repliés : « ÉDITION » et « edition » se retrouvent.
    assert tokens(analyzer="fr_produit", text="ÉDITION originale") == ["edition", "originale"]
    assert tokens(analyzer="brut", text="AB-12 xy") == ["AB-12 xy"]
    assert tokens(analyzer="sans_vides", text="le cheval des pres") == ["cheval", "pres"]
    # Et par champ, pas seulement par nom.
    assert tokens(field="titre", text="COÛTE") == ["coute"]

    es.index(index="ana", id="1", refresh=True,
             document={"titre": "ÉDITION originale", "code": "AB-12", "corps": "le cheval"})
    assert es.search(index="ana", query={"match": {"titre": "edition"}}
                     )["hits"]["total"]["value"] == 1
    # Le mapping rend le nom déclaré, pas un identifiant interne.
    props = es.indices.get_mapping(index="ana")["ana"]["mappings"]["properties"]
    assert props["titre"]["analyzer"] == "fr_produit"

    # Ce qui n'est pas reproductible à l'identique reste refusé.
    refused(lambda: es.indices.create(index="ana_ko", settings={"analysis": {"analyzer": {
        "x": {"type": "custom", "tokenizer": "standard", "filter": ["porter_stem"]}}}}),
        contains="ne supporte pas le filtre [porter_stem]")
    refused(lambda: es.indices.create(index="ana_ko", settings={"analysis": {"analyzer": {
        "x": {"type": "custom", "tokenizer": "pattern"}}}}),
        contains="ne supporte pas le tokenizer [pattern]")
    refused(lambda: es.indices.create(index="ana_ko", settings={"analysis": {"analyzer": {
        "x": {"type": "french"}}}}), contains="type [french]")
    es.indices.delete(index="ana")


@scenario
def n_grammes(es):
    """`ngram` et `edge_ngram` — la brique de l'autocomplétion « au fil de la
    frappe ». Elle travaille à l'**indexation**, là où `match_phrase_prefix`
    travaille à la requête, et c'est ce qui manquait à Wagtail v7.1.

    Les réglages posés ici sont ceux de Wagtail, mot pour mot."""
    es.options(ignore_status=404).indices.delete(index="auto")
    # Sans `max_ngram_diff`, l'écart par défaut est 1 : un `ngram` 3-15 est
    # refusé, avec le message d'ES.
    refused(lambda: es.indices.create(index="auto", settings={"analysis": {
        "tokenizer": {"t": {"type": "ngram", "min_gram": 3, "max_gram": 15}},
        "analyzer": {"a": {"type": "custom", "tokenizer": "t"}}}}),
        contains="The difference between max_gram and min_gram in NGram Tokenizer")
    # `edge_ngram`, lui, n'est pas borné par ce réglage — mesuré contre ES.
    es.indices.create(index="auto", settings={
        "index": {"max_ngram_diff": 12},
        "analysis": {
            "tokenizer": {
                "ngram_tokenizer": {"type": "ngram", "min_gram": 3, "max_gram": 15},
                # Wagtail écrit un `side` sur son tokenizer ; ES ne le lit pas
                # là (il rend les grammes de tête), donc ferrite non plus.
                "edgengram_tokenizer": {"type": "edge_ngram", "min_gram": 2,
                                        "max_gram": 15, "side": "front"},
                "mots": {"type": "edge_ngram", "min_gram": 1, "max_gram": 10,
                         "token_chars": ["letter", "digit"]},
            },
            "filter": {
                "ngram": {"type": "ngram", "min_gram": 3, "max_gram": 15},
                "edgengram": {"type": "edge_ngram", "min_gram": 1, "max_gram": 15},
            },
            "analyzer": {
                "ngram_analyzer": {"type": "custom", "tokenizer": "standard",
                                   "filter": ["asciifolding", "lowercase", "ngram"]},
                "edgengram_analyzer": {"type": "custom", "tokenizer": "standard",
                                       "filter": ["asciifolding", "lowercase", "edgengram"]},
                "par_mots": {"type": "custom", "tokenizer": "mots",
                             "filter": ["lowercase"]},
            },
        },
    }, mappings={"properties": {
        "titre": {"type": "text", "analyzer": "edgengram_analyzer"},
        "corps": {"type": "text", "analyzer": "ngram_analyzer"},
    }})

    def analyse(**kw):
        return [(t["token"], t["position"]) for t in
                es.indices.analyze(index="auto", **kw)["tokens"]]

    # Le filtre pose tous les grammes d'un mot **a sa position** : c'est ce qui
    # laisse `match_phrase` fonctionner par-dessus.
    assert analyse(analyzer="edgengram_analyzer", text="Élan bleu") == [
        ("e", 0), ("el", 0), ("ela", 0), ("elan", 0), ("b", 1), ("bl", 1),
        ("ble", 1), ("bleu", 1)]
    # L'ordre du tokenizer : par position de départ, longueurs croissantes.
    assert [t for t, _ in analyse(analyzer="par_mots", text="abc de")] == [
        "a", "ab", "abc", "d", "de"]
    # Un mot plus court que `min_gram` est jeté, il ne ressort pas tel quel.
    assert [t for t, _ in analyse(analyzer="ngram_analyzer", text="ab abcd")] == [
        "abc", "abcd", "bcd"]

    # Et la seule chose qui compte pour un client : chercher un début de mot.
    es.bulk(operations=[
        {"index": {"_index": "auto", "_id": "1"}},
        {"titre": "Élan bleu", "corps": "la grande traversée"},
        {"index": {"_index": "auto", "_id": "2"}},
        {"titre": "Éléphant", "corps": "une autre histoire"},
    ], refresh=True)
    assert sorted(ids(es.search(index="auto", query={"match": {"titre": "ele"}}))) == ["1", "2"]
    # Et le revers, mesuré identique chez ES : faute de `search_analyzer`, la
    # requête est découpée en grammes elle aussi, donc `elan` rend les deux —
    # `e` et `el` suffisent. C'est exactement ce que `search_analyzer` corrige
    # (scénario `search_analyzer`).
    assert sorted(ids(es.search(index="auto", query={"match": {"titre": "elan"}}))) == ["1", "2"]
    assert ids(es.search(index="auto", query={"match": {"titre": "bleu"}})) == ["1"]
    # Le n-gramme cherche aussi **au milieu** d'un mot, ce qu'un préfixe ne fait pas.
    assert ids(es.search(index="auto", query={"match": {"corps": "vers"}})) == ["1"]

    # Une phrase sur un champ à n-grammes : les grammes d'un mot occupent tous
    # la **même** position, donc ce sont des alternatives et non une suite.
    # Les enchaîner rendrait moins de documents — en silence.
    assert sorted(ids(es.search(index="auto",
                                query={"match_phrase": {"corps": "vers"}}))) == ["1"]
    assert sorted(ids(es.search(index="auto",
                                query={"match_phrase_prefix": {"titre": "ele"}}))) == ["1", "2"]
    # À plusieurs mots, il faudrait la `MultiPhraseQuery` de Lucene : refusé
    # explicitement plutôt que rendu faux.
    refused(lambda: es.search(index="auto",
                              query={"match_phrase": {"corps": "grande traversee"}}),
            contains="plusieurs termes a la meme position")

    # Le réglage et les déclarations ressortent des settings, et l'index se
    # relit : un tokenizer rendu en ligne là où le parseur attend un nom
    # casserait le redémarrage.
    reglages = es.indices.get_settings(index="auto")["auto"]["settings"]["index"]
    assert reglages["max_ngram_diff"] == "12", reglages

    # Les bornes impossibles sont refusées, avec les messages d'ES.
    for bornes, message in (
        ({"min_gram": 0, "max_gram": 1}, "minGram must be greater than zero"),
        ({"min_gram": 3, "max_gram": 2}, "minGram must not be greater than maxGram"),
    ):
        refused(lambda b=bornes: es.indices.create(index="auto_ko", settings={"analysis": {
            "tokenizer": {"t": {"type": "edge_ngram", **b}},
            "analyzer": {"a": {"type": "custom", "tokenizer": "t"}}}}), contains=message)
    refused(lambda: es.indices.create(index="auto_ko", settings={"analysis": {
        "tokenizer": {"t": {"type": "ngram", "token_chars": ["custom"]}},
        "analyzer": {"a": {"type": "custom", "tokenizer": "t"}}}}),
        contains="requires setting `custom_token_chars`")
    es.indices.delete(index="auto")


@scenario
def search_analyzer(es):
    """Indexer en grammes, chercher le mot entier.

    C'est le compagnon obligé des n-grammes, et c'est la déclaration que Wagtail
    pose sur ses deux champs d'autocomplétion. Sans lui, `elan` rend tout ce qui
    commence par `e` — mesuré identique chez ES, donc pas un défaut : ce que
    `search_analyzer` corrige."""
    es.options(ignore_status=404).indices.delete(index="sa")
    es.indices.create(index="sa", settings={
        "index": {"max_ngram_diff": 12},
        "analysis": {
            "filter": {"edgengram": {"type": "edge_ngram", "min_gram": 1, "max_gram": 15}},
            "analyzer": {"edgengram_analyzer": {
                "type": "custom", "tokenizer": "standard",
                "filter": ["asciifolding", "lowercase", "edgengram"]}},
        },
    }, mappings={"properties": {
        # La déclaration de Wagtail, mot pour mot.
        "titre": {"type": "text", "analyzer": "edgengram_analyzer",
                  "search_analyzer": "standard"},
        "sans": {"type": "text", "analyzer": "edgengram_analyzer"},
    }})
    es.bulk(operations=[
        {"index": {"_index": "sa", "_id": "1"}}, {"titre": "Élan bleu", "sans": "Élan bleu"},
        {"index": {"_index": "sa", "_id": "2"}}, {"titre": "Éléphant", "sans": "Éléphant"},
    ], refresh=True)
    # Avec : la requête n'est pas découpée, `elan` ne trouve qu'Élan.
    assert ids(es.search(index="sa", query={"match": {"titre": "elan"}})) == ["1"]
    # Sans : elle l'est, et `e` suffit à tout ramener.
    assert sorted(ids(es.search(index="sa", query={"match": {"sans": "elan"}}))) == ["1", "2"]
    # L'indexation, elle, n'a pas changé : le préfixe se cherche toujours, et
    # il ratisse d'autant plus large qu'il est court.
    assert sorted(ids(es.search(index="sa", query={"match": {"titre": "ele"}}))) == ["2"]
    assert sorted(ids(es.search(index="sa", query={"match": {"titre": "el"}}))) == ["1", "2"]

    # `_analyze` sur un champ rejoue l'analyzer d'**indexation**, pas celui de
    # recherche — c'est ce que fait ES (mesuré contre 8.15).
    tokens = [t["token"] for t in
              es.indices.analyze(index="sa", field="titre", text="elan")["tokens"]]
    assert tokens == ["e", "el", "ela", "elan"], tokens

    # Le mapping relu porte les deux analyzers ; un champ qui ne déclare que le
    # second se voit nommer `default` comme index analyzer, comme chez ES.
    props = es.indices.get_mapping(index="sa")["sa"]["mappings"]["properties"]
    assert props["titre"] == {"type": "text", "analyzer": "edgengram_analyzer",
                              "search_analyzer": "standard"}, props
    es.indices.put_mapping(index="sa", properties={
        "resume": {"type": "text", "search_analyzer": "keyword"}})
    props = es.indices.get_mapping(index="sa")["sa"]["mappings"]["properties"]
    assert props["resume"] == {"type": "text", "analyzer": "default",
                               "search_analyzer": "keyword"}, props

    # Ailleurs que sur un `text`, ES ne connaît pas le paramètre : sa phrase.
    refused(lambda: es.indices.create(index="sa_ko", mappings={"properties": {
        "k": {"type": "keyword", "search_analyzer": "standard"}}}),
        contains="unknown parameter [search_analyzer] on mapper [k] of type [keyword]")
    es.indices.delete(index="sa")


@scenario
def copy_to(es):
    """Se refaire un `_all` : recopier plusieurs champs dans un seul.

    C'est ainsi que Wagtail construit son `_all_text`. La copie se fait à
    l'indexation, sur la valeur **brute** — la cible la relit avec son propre
    type et son propre analyzer — et elle n'entre pas dans le `_source`."""
    es.options(ignore_status=404).indices.delete(index="cp")
    es.indices.create(index="cp", mappings={"properties": {
        "titre": {"type": "text", "copy_to": "_all_text"},
        "auteur": {"type": "keyword", "copy_to": ["_all_text", "gens"]},
        "annee": {"type": "integer", "copy_to": "_all_text"},
        "_all_text": {"type": "text"},
        "gens": {"type": "text"},
    }})
    # Rendu en tableau, même déclaré en chaîne — comme ES.
    props = es.indices.get_mapping(index="cp")["cp"]["mappings"]["properties"]
    assert props["titre"]["copy_to"] == ["_all_text"], props
    assert props["auteur"]["copy_to"] == ["_all_text", "gens"], props

    es.index(index="cp", id="1", refresh=True,
             document={"titre": "Le Horla", "auteur": "Maupassant", "annee": 1887})
    # Un seul champ à interroger pour trois.
    for terme in ("horla", "Maupassant", "1887"):
        assert ids(es.search(index="cp", query={"match": {"_all_text": terme}})) == ["1"], terme
    assert ids(es.search(index="cp", query={"match": {"gens": "Maupassant"}})) == ["1"]
    # La copie n'est pas dans le `_source` : c'est un champ indexé, pas stocké.
    assert es.get(index="cp", id="1")["_source"] == {
        "titre": "Le Horla", "auteur": "Maupassant", "annee": 1887}
    # `fields`, lui, la rend : la valeur propre de la cible d'abord, puis les
    # sources par ordre de nom (mesuré contre ES 8.15).
    hit = es.search(index="cp", fields=["_all_text"], query={"match_all": {}})["hits"]["hits"][0]
    assert hit["fields"]["_all_text"] == ["1887", "Maupassant", "Le Horla"], hit["fields"]

    # Une cible absente du mapping se crée toute seule, au type de la **valeur
    # copiée** — un `integer` copié donne un `long`, pas un `text`.
    es.options(ignore_status=404).indices.delete(index="cp2")
    es.indices.create(index="cp2", mappings={"properties": {
        "n": {"type": "integer", "copy_to": "tout"}}})
    es.index(index="cp2", id="1", refresh=True, document={"n": 42})
    props = es.indices.get_mapping(index="cp2")["cp2"]["mappings"]["properties"]
    assert props["tout"] == {"type": "long"}, props
    assert ids(es.search(index="cp2", query={"term": {"tout": 42}})) == ["1"]

    # La copie ne se **chaîne** pas : la cible d'une cible ne reçoit rien.
    es.options(ignore_status=404).indices.delete(index="cp3")
    es.indices.create(index="cp3", mappings={"properties": {
        "a": {"type": "text", "copy_to": "b"},
        "b": {"type": "text", "copy_to": "c"},
        "c": {"type": "text"}}})
    es.index(index="cp3", id="1", refresh=True, document={"a": "zebre"})
    assert ids(es.search(index="cp3", query={"match": {"b": "zebre"}})) == ["1"]
    assert ids(es.search(index="cp3", query={"match": {"c": "zebre"}})) == []

    # Les trois refus d'ES, avec ses phrases.
    refused(lambda: es.indices.create(index="cp_ko", mappings={"properties": {
        "t": {"type": "text", "fields": {"k": {"type": "keyword", "copy_to": "tout"}}},
        "tout": {"type": "text"}}}),
        contains="[copy_to] may not be used to copy from a multi-field: [t.k]")
    refused(lambda: es.indices.create(index="cp_ko", mappings={"properties": {
        "t": {"type": "text", "copy_to": "x.k"},
        "x": {"type": "text", "fields": {"k": {"type": "keyword"}}}}}),
        contains="[copy_to] may not be used to copy to a multi-field: [x.k]")
    refused(lambda: es.indices.create(index="cp_ko", mappings={"properties": {
        "t": {"type": "text", "copy_to": "o"},
        "o": {"properties": {"a": {"type": "text"}}}}}),
        contains="Cannot copy to field [o] since it is mapped as an object")
    refused(lambda: es.indices.create(index="cp_ko", mappings={"properties": {
        "t": {"type": "text", "copy_to": "l.a"},
        "l": {"type": "nested", "properties": {"a": {"type": "text"}}}}}),
        contains="Illegal combination of [copy_to] and [nested] mappings")
    # Depuis un `nested` vers la racine, en revanche, ES l'autorise.
    es.options(ignore_status=404).indices.delete(index="cp4")
    es.indices.create(index="cp4", mappings={"properties": {
        "l": {"type": "nested", "properties": {"a": {"type": "text", "copy_to": "tout"}}},
        "tout": {"type": "text"}}})
    es.index(index="cp4", id="1", refresh=True, document={"l": [{"a": "x"}, {"a": "y"}]})
    assert ids(es.search(index="cp4", query={"match": {"tout": "y"}})) == ["1"]
    for i in ("cp", "cp2", "cp3", "cp4"):
        es.indices.delete(index=i)


@scenario
def store_et_stored_fields(es):
    """`store: true`, et ce que `stored_fields` en fait.

    C'est l'autre moitié du sujet de `stored_fields` : sans `store`, il n'y
    avait rien à rendre. Wagtail relit son `pk` exactement comme ça, avec
    `_source: false`."""
    es.options(ignore_status=404).indices.delete(index="st")
    es.indices.create(index="st", mappings={"properties": {
        "pk": {"type": "keyword", "store": True},
        "t": {"type": "text", "store": True},
        "n": {"type": "long", "store": True},
        "d": {"type": "date", "format": "yyyy/MM/dd", "store": True},
        # Le défaut d'ES : accepté, et **non rendu** dans le mapping.
        "sf": {"type": "text", "store": False},
        "ns": {"type": "text"},
    }})
    props = es.indices.get_mapping(index="st")["st"]["mappings"]["properties"]
    assert props["pk"] == {"type": "keyword", "store": True}, props
    assert props["sf"] == {"type": "text"}, props

    es.index(index="st", id="1", refresh=True, document={
        "pk": "42", "t": ["bonjour monde", "chat"], "n": [3, 1, 1],
        "d": "2026/03/15", "sf": "invisible", "ns": "invisible"})
    hit = es.search(index="st", stored_fields=["pk", "t", "n", "d", "sf", "ns"],
                    query={"match_all": {}})["hits"]["hits"][0]
    # L'ordre du document, doublons compris — là où `docvalue_fields` trie et
    # dédoublonne. Et un champ non stocké n'a **pas de clé**.
    assert hit["fields"] == {"pk": ["42"], "t": ["bonjour monde", "chat"],
                             "n": [3, 1, 1], "d": ["2026/03/15"]}, hit["fields"]
    assert "_source" not in hit, hit
    hit = es.search(index="st", docvalue_fields=["n"], query={"match_all": {}})["hits"]["hits"][0]
    assert hit["fields"]["n"] == [1, 1, 3], hit["fields"]

    # Le geste de Wagtail : relire une seule clé, sans le `_source`.
    hit = es.search(index="st", stored_fields=["pk"], source=False,
                    query={"match_all": {}})["hits"]["hits"][0]
    assert hit["fields"]["pk"][0] == "42", hit

    # `_doc` et `_mget` lisent les mêmes champs stockés, au même endroit :
    # livrer `store` pour la seule route `_search` en aurait fait un paramètre
    # qui marche « sauf là ».
    doc = es.get(index="st", id="1", stored_fields=["pk", "n"])
    assert doc["fields"] == {"pk": ["42"], "n": [3, 1, 1]}, doc
    assert "_source" not in doc, doc
    # `_source` cité dans la liste le ramène, ici comme sur une recherche.
    doc = es.get(index="st", id="1", stored_fields=["pk", "_source"])
    assert doc["_source"]["pk"] == "42", doc
    lot = es.mget(index="st", body={"docs": [
        {"_id": "1"},
        {"_id": "1", "stored_fields": ["pk"]},
    ]})["docs"]
    assert "fields" not in lot[0] and lot[0]["_source"]["pk"] == "42", lot[0]
    assert lot[1]["fields"] == {"pk": ["42"]} and "_source" not in lot[1], lot[1]

    # Redéclarer le même champ à l'identique est licite — c'est ce que fait une
    # application qui déclare le même champ pour deux de ses modèles. Le
    # **changer** ne l'est pas, chez ES non plus.
    es.indices.put_mapping(index="st", properties={"pk": {"type": "keyword", "store": True}})
    refused(lambda: es.indices.put_mapping(index="st", properties={
        "pk": {"type": "keyword", "store": False}}),
        contains="Cannot update parameter [store] from [true] to [false]")

    # Sur un objet, ES ne connaît pas le paramètre — refusé des deux côtés.
    refused(lambda: es.indices.create(index="st_ko", mappings={"properties": {
        "o": {"type": "object", "store": True, "properties": {"a": {"type": "text"}}}}}),
        contains="store")
    refused(lambda: es.indices.create(index="st_ko", mappings={"properties": {
        "t": {"type": "text", "store": "oui"}}}),
        contains="only [true] or [false] are allowed")
    es.indices.delete(index="st")


@scenario
def format_de_date(es):
    """Un mapping venu d'une instance reelle declare presque toujours un
    `format` sur ses dates. Il sert a lire (indexation, bornes d'un `range`) et
    a rendre (`*_as_string`)."""
    es.options(ignore_status=404).indices.delete(index="dates")
    es.indices.create(index="dates", mappings={"properties": {
        "cree_le": {"type": "date", "format": "yyyy-MM-dd HH:mm:ss"},
        "jour": {"type": "date", "format": "yyyy-MM-dd"},
        "multi": {"type": "date", "format": "yyyy-MM-dd HH:mm:ss||yyyy-MM-dd"},
        "basique": {"type": "date", "format": "basic_date"},
        "defaut": {"type": "date"},
    }})
    es.bulk(operations=[
        {"index": {"_index": "dates", "_id": "1"}},
        {"cree_le": "2021-03-04 10:00:00", "jour": "2021-03-04",
         "multi": "2021-03-04", "basique": "20210304", "defaut": "2021-03-04T10:00:00Z"},
        {"index": {"_index": "dates", "_id": "2"}},
        {"cree_le": "2022-06-11 09:30:00", "jour": "2022-06-11",
         "multi": "2022-06-11 09:30:00", "basique": "20220611",
         "defaut": "2022-06-11T09:30:00Z"},
    ], refresh=True)

    def hits(query):
        return sorted(h["_id"] for h in es.search(index="dates", query=query)["hits"]["hits"])

    # Les bornes d'un `range` se lisent au format du champ, pas en ISO.
    assert hits({"range": {"cree_le": {"gte": "2022-01-01 00:00:00"}}}) == ["2"]
    assert hits({"range": {"jour": {"lt": "2022-01-01"}}}) == ["1"]
    assert hits({"term": {"cree_le": "2021-03-04 10:00:00"}}) == ["1"]
    assert hits({"range": {"basique": {"gte": "20220101"}}}) == ["2"]
    # Une alternative `||` : les deux ecritures entrent.
    assert hits({"range": {"multi": {"gte": "2022-01-01"}}}) == ["2"]
    # Le format par defaut n'a pas bouge.
    assert hits({"range": {"defaut": {"gte": "2022-01-01T00:00:00Z"}}}) == ["2"]
    # Le mapping rend le format tel qu'il a ete declare.
    props = es.indices.get_mapping(index="dates")["dates"]["mappings"]["properties"]
    assert props["cree_le"]["format"] == "yyyy-MM-dd HH:mm:ss"
    assert "format" not in props["defaut"]
    # La forme lisible d'une agregation suit le format du champ.
    agg = es.search(index="dates", size=0,
                    aggs={"m": {"max": {"field": "cree_le"}}})["aggregations"]["m"]
    assert agg["value_as_string"] == "2022-06-11 09:30:00", agg
    # Ce qui n'est pas au format est refuse — y compris un epoch, comme chez ES.
    refused(lambda: es.index(index="dates", id="9", document={"cree_le": "04/03/2021"}),
            contains="failed to parse date field")
    refused(lambda: es.index(index="dates", id="9", document={"cree_le": 1614852000000}),
            contains="failed to parse date field")
    # Un motif qu'on ne sait pas traduire se dit, il ne s'approxime pas.
    refused(lambda: es.indices.create(index="dz", mappings={"properties": {
        "x": {"type": "date", "format": "GGGG yyyy"}}}),
        contains="ne sait pas traduire")
    es.indices.delete(index="dates")


@scenario
def date_math(es):
    """Une borne de date est une **expression** resolue par le serveur (`now`,
    `now-1d/d`, `2026-03-15||+1M`), et son arrondi depend du cote de la borne.

    C'est ce que fait le filtre « en retard » de n'importe quel tableau de bord
    (`{"range": {"fin": {"lt": "now"}}}`), et c'est aussi ce qui distingue
    `lte: "2026-03-15"` (toute la journee) de `lt: "2026-03-15"` (jusqu'a
    minuit)."""
    import datetime

    def iso(delta):
        t = datetime.datetime.now(datetime.timezone.utc) + delta
        return t.strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"

    es.options(ignore_status=404).indices.delete(index="dm")
    es.indices.create(index="dm", mappings={"properties": {
        "fin": {"type": "date"},
        "ref": {"type": "keyword"},
    }})
    es.bulk(operations=[
        {"index": {"_index": "dm", "_id": "hier"}},
        {"fin": iso(datetime.timedelta(days=-1)), "ref": "hier"},
        {"index": {"_index": "dm", "_id": "demain"}},
        {"fin": iso(datetime.timedelta(days=1)), "ref": "demain"},
        {"index": {"_index": "dm", "_id": "minuit"}},
        {"fin": "2026-03-15T00:00:00.000Z", "ref": "minuit"},
        {"index": {"_index": "dm", "_id": "midi"}},
        {"fin": "2026-03-15T12:00:00.000Z", "ref": "midi"},
        {"index": {"_index": "dm", "_id": "veille"}},
        {"fin": "2026-03-14T23:59:59.999Z", "ref": "veille"},
    ], refresh=True)

    def hits(query):
        return sorted(h["_id"] for h in es.search(index="dm", query=query)["hits"]["hits"])

    # Le filtre KPI : ce qui est deja passe.
    assert hits({"range": {"fin": {"lt": "now"}}}) == ["hier", "midi", "minuit", "veille"]
    assert hits({"range": {"fin": {"gt": "now"}}}) == ["demain"]
    assert hits({"range": {"fin": {"gte": "now-2d", "lt": "now"}}}) == ["hier"]
    # Les operations et l'arrondi.
    assert hits({"range": {"fin": {"gte": "now/d", "lt": "now/d+1d"}}}) == []
    assert hits({"range": {"fin": {"gte": "now-2d", "lte": "now+2d"}}}) == ["demain", "hier"]
    # Une ancre explicite suivie d'operations.
    assert hits({"range": {"fin": {"gte": "2026-03-15||/d",
                                   "lte": "2026-03-15||/d"}}}) == ["midi", "minuit"]
    assert hits({"range": {"fin": {"lt": "2026-03-14||+1d"}}}) == ["veille"]
    # L'arrondi selon la borne, sans date math : `lte` couvre la journee.
    assert hits({"range": {"fin": {"lte": "2026-03-15"}}}) == ["midi", "minuit", "veille"]
    assert hits({"range": {"fin": {"lt": "2026-03-15"}}}) == ["veille"]
    # Hors d'un `range`, une date designe la periode qu'elle couvre.
    assert hits({"term": {"fin": "2026-03-15"}}) == ["midi", "minuit"]
    assert hits({"term": {"fin": "2026-03-15T12:00:00.000Z"}}) == ["midi"]
    # Le `format` de la requete remplace celui du champ pour lire les bornes.
    assert hits({"range": {"fin": {"lte": "15/03/2026",
                                   "format": "dd/MM/yyyy"}}}) == ["midi", "minuit", "veille"]
    # Une expression malformee est refusee avec le message d'ES, jamais prise
    # pour une date litterale. (ES rend ce meme texte dans `root_cause[0]`,
    # sous un `search_phase_execution_exception` que ferrite n'empile pas.)
    refused(lambda: es.search(index="dm", query={"range": {"fin": {"lt": "now-1q"}}}),
            contains="unit [q] not supported for date math")
    refused(lambda: es.search(index="dm", query={"range": {"fin": {"lt": "now/"}}}),
            contains="truncated date math")
    refused(lambda: es.search(index="dm", query={"range": {"fin": {"lt": "NOW"}}}),
            contains="failed to parse date field [NOW]")
    # `time_zone` n'est pas supporte : il change l'arrondi, donc les resultats.
    refused(lambda: es.search(index="dm", query={"range": {"fin": {"lt": "now/d",
                                                                   "time_zone": "+02:00"}}}),
            contains="time_zone")
    # A l'indexation, `now` reste une date invalide — comme chez ES.
    refused(lambda: es.index(index="dm", id="x", document={"fin": "now", "ref": "x"}),
            contains="failed to parse date field")
    es.indices.delete(index="dm")


@scenario
def routes_sans_index(es):
    """`_refresh`, `_mapping`, `_search` et `_count` sans index portent sur
    tous, comme chez ES."""
    for nom in ("multi1", "multi2"):
        es.options(ignore_status=404).indices.delete(index=nom)
        es.index(index=nom, id="1", refresh=True, document={"titre": "x"})
    # Un index = un shard : `_shards` compte les index rafraichis.
    tous = set(es.indices.get_mapping())
    assert es.indices.refresh()["_shards"]["successful"] == len(tous)
    m = es.indices.get_mapping()
    assert {"multi1", "multi2"} <= set(m), sorted(m)
    assert set(es.indices.get_mapping(index="_all")) == set(m)
    # `_search` sans index cherche partout.
    partout = es.search(query={"match_all": {}}, size=0)
    assert partout["_shards"]["total"] == len(tous)
    assert partout["hits"]["total"]["value"] == es.count()["count"]
    for nom in ("multi1", "multi2"):
        es.indices.delete(index=nom)


@scenario
def recherche_multi_index(es):
    """Le tableau d'index d'un client officiel, les motifs, les exclusions.

    `es.search(index=["a", "b"])` est ce qu'ecrit un service qui cherche dans
    plusieurs catalogues d'un coup : le client recolle le tableau en une liste
    separee par des virgules, et le serveur doit fusionner les resultats des
    deux index — total, ordre et agregations comprises.
    """
    for nom in ("mi_a", "mi_b", "mi_c"):
        es.options(ignore_status=404).indices.delete(index=nom)
    es.indices.create(index="mi_a", mappings={"properties": {
        "titre": {"type": "text"}, "marque": {"type": "keyword"},
        "prix": {"type": "double"}}})
    es.indices.create(index="mi_b", mappings={"properties": {
        "titre": {"type": "text"}, "marque": {"type": "keyword"},
        "prix": {"type": "double"}}})
    es.indices.create(index="mi_c", mappings={"properties": {
        "titre": {"type": "text"}, "marque": {"type": "keyword"},
        "prix": {"type": "double"}}})
    es.bulk(refresh=True, operations=[
        {"index": {"_index": "mi_a", "_id": "1"}},
        {"titre": "casque bluetooth", "marque": "Sony", "prix": 100.0},
        {"index": {"_index": "mi_a", "_id": "2"}},
        {"titre": "casque filaire", "marque": "Sony", "prix": 40.0},
        {"index": {"_index": "mi_b", "_id": "3"}},
        {"titre": "casque de chantier", "marque": "Bose", "prix": 25.0},
        {"index": {"_index": "mi_c", "_id": "4"}},
        {"titre": "clavier", "marque": "Logitech", "prix": 80.0},
    ])

    # Le tableau : la forme exacte qu'ecrit le code client.
    r = es.search(index=["mi_a", "mi_b"], query={"match": {"titre": "casque"}})
    assert r["hits"]["total"]["value"] == 3, r["hits"]["total"]
    assert r["_shards"]["total"] == 2, r["_shards"]
    # Chaque hit dit de quel index il vient.
    assert {h["_index"] for h in r["hits"]["hits"]} == {"mi_a", "mi_b"}

    # Un motif, et un motif moins une exclusion.
    assert es.search(index="mi_*", query={"match_all": {}})["hits"]["total"]["value"] == 4
    r = es.search(index="mi_*,-mi_c", query={"match_all": {}})
    assert r["hits"]["total"]["value"] == 3 and r["_shards"]["total"] == 2

    # Un motif qui ne correspond a rien n'est pas une erreur : zero shard.
    r = es.search(index="fantome_*", query={"match_all": {}})
    assert r["hits"]["total"]["value"] == 0 and r["_shards"]["total"] == 0

    # Un index absent d'une liste reste une erreur, sauf `ignore_unavailable`.
    refused(lambda: es.search(index=["mi_a", "fantome"], query={"match_all": {}}),
            status=404)
    r = es.search(index=["mi_a", "fantome"], query={"match_all": {}},
                  ignore_unavailable=True)
    assert r["hits"]["total"]["value"] == 2

    # Le tri fusionne entre index, page par page.
    r = es.search(index="mi_*", query={"match_all": {}},
                  sort=[{"prix": "asc"}], size=10)
    assert [h["_id"] for h in r["hits"]["hits"]] == ["3", "2", "4", "1"]
    r = es.search(index="mi_*", query={"match_all": {}},
                  sort=[{"prix": "asc"}], from_=1, size=2)
    assert [h["_id"] for h in r["hits"]["hits"]] == ["2", "4"]

    # Les agregations aussi : un `avg` fusionne se repondere, il ne fait pas la
    # moyenne des moyennes.
    a = es.search(index="mi_*", size=0, aggs={
        "m": {"avg": {"field": "prix"}},
        "f": {"terms": {"field": "marque"}},
    })["aggregations"]
    assert abs(a["m"]["value"] - (100.0 + 40.0 + 25.0 + 80.0) / 4) < 1e-9
    assert {b["key"]: b["doc_count"] for b in a["f"]["buckets"]} == {
        "Sony": 2, "Bose": 1, "Logitech": 1}

    # `_count` suit la meme resolution.
    assert es.count(index=["mi_a", "mi_b"])["count"] == 3

    for nom in ("mi_a", "mi_b", "mi_c"):
        es.indices.delete(index=nom)


@scenario
def alias(es):
    """Un alias sur des index quotidiens : le nom stable que le code client
    connait, alors que les index qu'il designe changent tous les jours."""
    jours = ["al_2026.08.01", "al_2026.08.02", "al_2026.08.03"]
    for nom in jours + ["al_vieux"]:
        es.options(ignore_status=404).indices.delete(index=nom)
    for i, nom in enumerate(jours):
        es.indices.create(index=nom, mappings={"properties": {
            "message": {"type": "text"}, "niveau": {"type": "keyword"}}})
        es.index(index=nom, id=str(i), refresh=True,
                 document={"message": f"evenement {i}", "niveau": "info"})
    # Un alias pose a la creation.
    es.indices.create(index="al_vieux", aliases={"audits": {}})

    # ... et le meme alias pose sur les trois autres, en un lot atomique.
    es.indices.put_alias(index=",".join(jours), name="audits")
    assert set(es.indices.get_alias(name="audits")) == set(jours) | {"al_vieux"}

    # L'alias se cherche comme un index.
    r = es.search(index="audits", query={"match_all": {}})
    assert r["hits"]["total"]["value"] == 3 and r["_shards"]["total"] == 4

    # Ecrire a travers un alias qui couvre plusieurs index est refuse tant
    # qu'aucun n'est designe comme index d'ecriture.
    refused(lambda: es.index(index="audits", id="x", document={"message": "y"}),
            contains="write index")
    es.indices.put_alias(index=jours[-1], name="audits", is_write_index=True)
    resp = es.index(index="audits", id="x", refresh=True,
                    document={"message": "ecrit via alias"})
    # La reponse porte le nom **concret**, pas celui de l'alias.
    assert resp["_index"] == jours[-1], resp
    assert es.get(index=jours[-1], id="x")["found"] is True

    # Une bascule sans interruption : retrait et pose dans le meme appel.
    es.indices.update_aliases(actions=[
        {"remove": {"index": jours[0], "alias": "audits"}},
        {"add": {"index": jours[0], "alias": "audits_froid"}},
    ])
    assert set(es.indices.get_alias(name="audits_froid")) == {jours[0]}
    assert jours[0] not in es.indices.get_alias(name="audits")

    # Supprimer un index le retire de ses alias.
    es.indices.delete(index="al_vieux")
    assert "al_vieux" not in es.indices.get_alias(name="audits")

    # Un index et un alias ne peuvent pas porter le meme nom.
    refused(lambda: es.indices.create(index="audits"), contains="already exists as alias")
    # Et `DELETE /{alias}` ne supprime pas les index qu'il designe.
    refused(lambda: es.indices.delete(index="audits"), contains="matches an alias")

    for nom in jours:
        es.indices.delete(index=nom)


@scenario
def expression_de_noms_d_alias(es):
    """Le nom d'alias est une expression : liste, joker, exclusion, `_all`.

    Un client qui garde deux familles d'alias sous un meme prefixe les demande
    par motif, et retire la ou les exceptions du lot. C'est aussi ce qui
    decide du 404 — et ES y a deux regles qui se contredisent en apparence,
    voir `tests/compat/sonde_alias.py`.
    """
    idx = "alx_index"
    es.options(ignore_status=404).indices.delete(index=idx)
    es.indices.create(index=idx, aliases={
        "alx_lecture_1": {}, "alx_lecture_2": {},
        "alx_ecriture_1": {}, "alx_ecriture_2": {}, "alx": {}})

    def vus(nom):
        # Un index sans alias retenu ne figure pas dans la reponse : `_alias`
        # filtre est un « ce qui correspond », pas un « un objet par index ».
        corps = es.indices.get_alias(name=nom).body
        return set(corps.get(idx, {}).get("aliases", {}))

    # `_all` et `*` designent tous les alias, sans 404.
    assert vus("_all") == vus("*") == {
        "alx_lecture_1", "alx_lecture_2", "alx_ecriture_1", "alx_ecriture_2", "alx"}

    # Un motif, moins une exception : l'exclusion retire de ce qui precede.
    assert vus("alx_lecture*,-alx_lecture_1") == {"alx_lecture_2"}
    assert vus("alx_ecriture_2,alx_lecture*,-alx_lecture_1") == {
        "alx_ecriture_2", "alx_lecture_2"}

    # Un nom demande qui manque rend 404 — mais le corps porte quand meme les
    # alias trouves : « il en manque », pas « il n'y a rien ». Sur cette route
    # seulement, `error` est une **chaine** et non l'objet habituel : ce
    # scenario ne peut donc pas passer par `refused`.
    try:
        es.indices.get_alias(name="alx_lecture_1,alx_absent")
        raise AssertionError("l'appel aurait du rendre 404")
    except ApiError as exc:
        assert exc.meta.status == 404, exc.meta.status
        assert exc.body["error"] == "alias [alx_absent] missing", exc.body
        assert exc.body["status"] == 404, exc.body
        assert set(exc.body[idx]["aliases"]) == {"alx_lecture_1"}, exc.body

    # Un joker qui ne correspond a rien, lui, ne declenche pas de 404.
    assert vus("alx_neant*") == set()

    es.indices.delete(index=idx)


@scenario
def suppression_par_motif(es):
    """La purge d'une retention par index quotidien.

    ES 8 la refuse par defaut (`action.destructive_requires_name`, passe a
    `true` en 8.0) : ferrite refuse au meme endroit, et n'obeit qu'une fois le
    reglage bascule — sinon la premiere difference de comportement entre les
    deux serveurs serait une suppression de donnees.
    """
    for nom in ("purge_2026.07.01", "purge_2026.07.02", "purge_2026.08.01"):
        es.options(ignore_status=404).indices.delete(index=nom)
        es.indices.create(index=nom)

    refused(lambda: es.indices.delete(index="purge_2026.07.*"),
            contains="Wildcard expressions or all indices are not allowed")

    es.cluster.put_settings(persistent={"action.destructive_requires_name": False})
    try:
        es.indices.delete(index="purge_2026.07.*")
        restants = set(es.indices.get(index="purge_*"))
        assert restants == {"purge_2026.08.01"}, restants
        # Un motif sans correspondance n'est pas une erreur.
        es.indices.delete(index="fantome_*")
    finally:
        es.cluster.put_settings(persistent={"action.destructive_requires_name": None})
    es.indices.delete(index="purge_2026.08.01")


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
def recherche_libre_lenient(es):
    """La barre « chercher par nom / reference / annee » : la meme chaine posee
    sur des champs de types differents.

    Sans `lenient`, taper un nom fait echouer la recherche entiere en 400 des
    qu'un des champs vises est numerique — c'est ce que remonte le premier
    client de ferrite.
    """
    champs = ["titre", "resume", "auteur", "annee", "tirage", "paru"]
    refused(lambda: es.search(index=INDEX, query={
        "multi_match": {"query": "presse", "fields": champs}}))
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "presse", "fields": champs, "lenient": True}})) == ["2"]
    # Le champ numerique reste cherche quand la valeur, elle, est lisible.
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "1887", "fields": ["titre", "annee"],
                        "lenient": True}})) == ["1"]
    # Aucun champ ne sait lire la valeur : 0 document, sans erreur...
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "presse", "fields": ["annee", "tirage"],
                        "lenient": True}})) == []
    # ...et la clause vide n'exclut rien sous un `must_not`.
    assert sorted(ids(es.search(index=INDEX, query={"bool": {
        "must": [{"match_all": {}}],
        "must_not": [{"multi_match": {"query": "presse", "fields": ["annee"],
                                      "lenient": True}}]}}))) == ["1", "2", "3"]
    # Un champ que le mapping ne connait pas est ecarte de la liste, il ne rend
    # pas la clause entiere vide : sinon une barre de recherche qui cite un
    # champ jamais mappe rendrait 0 document en silence.
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "presse", "fields": ["resume", "jamais_mappe"]}})) == ["2"]
    # `lenient` existe aussi sur `match` — et seulement la, comme chez ES.
    assert ids(es.search(index=INDEX, query={
        "match": {"annee": {"query": "presse", "lenient": True}}})) == []
    assert ids(es.search(index=INDEX, query={
        "match": {"annee": {"query": "1887", "lenient": True}}})) == ["1"]
    refused(lambda: es.search(index=INDEX, query={
        "match_phrase": {"annee": {"query": "presse", "lenient": True}}}),
        contains="lenient")


@scenario
def recherche_libre_phrase(es):
    """`type: phrase` : la meme phrase cherchee dans plusieurs champs."""
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "la presse parisienne", "fields": ["titre", "resume"],
                        "type": "phrase"}})) == ["2"]
    # Les memes mots dans le desordre ne matchent pas, la ou `best_fields` si.
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "parisienne presse", "fields": ["titre", "resume"],
                        "type": "phrase"}})) == []
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "parisienne presse", "fields": ["titre", "resume"],
                        "operator": "and"}})) == ["2"]
    # Sur un champ non analyse, la phrase est la valeur entiere (comme ES).
    assert sorted(ids(es.search(index=INDEX, query={
        "multi_match": {"query": "Maupassant", "fields": ["auteur", "titre"],
                        "type": "phrase"}}))) == ["1", "2"]
    # `tie_breaker` s'applique : `phrase` se combine en dis_max, comme
    # `best_fields`.
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "la presse parisienne", "fields": ["titre^2", "resume"],
                        "type": "phrase", "tie_breaker": 0.3}})) == ["2"]
    # phrase + lenient : le champ qui ne sait pas lire la valeur est ecarte.
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "la presse parisienne", "fields": ["resume", "annee"],
                        "type": "phrase", "lenient": True}})) == ["2"]


@scenario
def recherche_libre_phrase_prefix(es):
    """`type: phrase_prefix` : la meme barre, pendant la frappe."""
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "la presse pari", "fields": ["titre", "resume"],
                        "type": "phrase_prefix"}})) == ["2"]
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "germ", "fields": ["titre", "resume"],
                        "type": "phrase_prefix", "max_expansions": 10}})) == ["3"]
    # Un `keyword` ne peut pas porter de phrase a prefixe : ES refuse avec ce
    # message, ferrite le reprend — sauf sous `lenient`, ou le champ est
    # simplement ecarte (mesure contre ES 8.15).
    refused(lambda: es.search(index=INDEX, query={
        "multi_match": {"query": "germ", "fields": ["titre", "auteur"],
                        "type": "phrase_prefix"}}), contains="phrase prefix")
    assert ids(es.search(index=INDEX, query={
        "multi_match": {"query": "germ", "fields": ["titre", "auteur"],
                        "type": "phrase_prefix", "lenient": True}})) == ["3"]


@scenario
def multi_match_refus(es):
    refused(lambda: es.search(index=INDEX, query={
        "multi_match": {"query": "x", "fields": ["titre"], "type": "cross_fields"}}),
        contains="cross_fields")
    refused(lambda: es.search(index=INDEX, query={
        "multi_match": {"query": "x", "fields": ["titre"], "type": "bool_prefix"}}),
        contains="bool_prefix")
    # Un type qui n'existe pas chez ES non plus : son message, mot pour mot.
    refused(lambda: es.search(index=INDEX, query={
        "multi_match": {"query": "x", "fields": ["titre"], "type": "PHRASE"}}),
        contains="unknown type")
    refused(lambda: es.search(index=INDEX, query={
        "multi_match": {"query": "x", "fields": ["tit*"]}}), contains="motifs")
    refused(lambda: es.search(index=INDEX, query={"multi_match": {"query": "x"}}),
            contains="fields")
    # `slop` reste refuse partout (voir docs/compat.md), y compris en phrase.
    refused(lambda: es.search(index=INDEX, query={
        "multi_match": {"query": "la presse", "fields": ["resume"],
                        "type": "phrase", "slop": 2}}), contains="slop")
    # `tie_breaker` n'a pas de sens quand les scores s'additionnent.
    refused(lambda: es.search(index=INDEX, query={
        "multi_match": {"query": "x", "fields": ["titre"], "type": "most_fields",
                        "tie_breaker": 0.3}}), contains="tie_breaker")
    # `lenient` n'accepte que true/false, comme ES.
    refused(lambda: es.search(index=INDEX, query={
        "multi_match": {"query": "x", "fields": ["titre"], "lenient": "TRUE"}}),
        contains="only [true] or [false]")


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
def recherche_match_phrase_prefix(es):
    """La clause d'une barre de recherche qui complete pendant la frappe."""
    # Le dernier mot n'est qu'un debut : la phrase doit quand meme matcher.
    assert ids(es.search(index=INDEX, query={
        "match_phrase_prefix": {"resume": "la presse paris"}})) == ["2"]
    assert ids(es.search(index=INDEX, query={
        "match_phrase_prefix": {"resume": "greve des min"}})) == ["3"]
    # Un seul terme : c'est un prefixe sur le terme analyse.
    assert ids(es.search(index=INDEX, query={
        "match_phrase_prefix": {"resume": "parisien"}})) == ["2"]
    # L'ordre compte, comme dans `match_phrase`.
    assert ids(es.search(index=INDEX, query={
        "match_phrase_prefix": {"resume": "parisienne pre"}})) == []
    # Le prefixe vide ramene la phrase entiere.
    assert ids(es.search(index=INDEX, query={
        "match_phrase_prefix": {"resume": {"query": "la presse", "max_expansions": 5}}})) == ["2"]
    # Sur un `keyword`, ES refuse — il n'y a pas de positions.
    refused(lambda: es.search(index=INDEX, query={
        "match_phrase_prefix": {"auteur": "Maup"}}),
        contains="Can only use phrase prefix queries on text fields")
    # Meme raison que pour `match_phrase` : `slop` est refuse.
    refused(lambda: es.search(index=INDEX, query={
        "match_phrase_prefix": {"resume": {"query": "la presse", "slop": 2}}}),
        contains="slop")


@scenario
def recherche_regexp(es):
    """`regexp` : la clause des filtres « contient / commence par / finit par ».

    La syntaxe est celle de **Lucene**, pas celle du crate `regex` : ce
    scenario exerce les endroits ou les deux divergent, chacun mesure contre un
    vrai ES 8.15 (voir `tests/compat/diff_motifs.py`).
    """
    def refs(motif, **kw):
        v = {"value": motif, **kw}
        return sorted(ids(es.search(index=INDEX, size=50,
                                    query={"regexp": {"auteur": v}})))

    assert refs("Maupassant") == ["1", "2"]
    # Le motif est ancre des deux cotes : « contient » s'ecrit `.*x.*`.
    assert refs("passant") == []
    assert refs(".*passant.*") == ["1", "2"]
    assert refs("Z.*") == ["3"]
    assert refs(".*ola") == ["3"]
    assert refs("(Zola|Maupassant)") == ["1", "2", "3"]
    assert refs("[MZ].*") == ["1", "2", "3"]
    assert refs("Maup[a-z]{4}nt") == ["1", "2"]
    # `case_insensitive` replie l'ASCII, comme chez ES.
    assert refs("zola") == []
    assert refs("zola", case_insensitive=True) == ["3"]
    assert refs(".*OLA", case_insensitive=True) == ["3"]
    # Les classes predefinies existent, sur l'alphabet ASCII.
    assert refs("\\w+") == ["1", "2", "3"]
    assert refs("\\d+") == []
    # `^` et `$` ne sont pas des ancres chez Lucene : ce sont des caracteres.
    assert refs("^Zola$") == []

    # Ce que ferrite refuse, il le dit — plutot que de prendre l'operateur pour
    # un caractere litteral et de rendre d'autres documents qu'ES.
    for motif, mot in (("~Zola", "~"), ("Zola&Zola", "&"), ("<1-100>", "<n-m>")):
        refused(lambda m=motif: es.search(index=INDEX,
                                          query={"regexp": {"auteur": m}}), contains=mot)
    # Desactives par `flags`, ils redeviennent des caracteres litteraux.
    assert ids(es.search(index=INDEX, query={"regexp": {
        "auteur": {"value": "Zola~", "flags": "NONE"}}})) == []
    # Une lettre echappee qui n'est pas une classe predefinie est refusee,
    # comme chez Lucene.
    refused(lambda: es.search(index=INDEX, query={
        "regexp": {"auteur": "Zol\\a"}}), contains="invalid character class")
    refused(lambda: es.search(index=INDEX, query={
        "regexp": {"auteur": {"value": "Zola", "rewrite": "constant_score"}}}),
        contains="rewrite")
    # Un motif malforme est refuse, pas silencieusement vide.
    refused(lambda: es.search(index=INDEX, query={"regexp": {"auteur": "Zol("}}))


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
    # Sur un champ que le mapping ne connait pas : 0 document, comme chez ES.
    assert es.search(index=INDEX, query={"exists": {"field": "inconnu"}},
                     )["hits"]["total"]["value"] == 0
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
def minimum_should_match_en_notations(es):
    """Les quatre notations de `minimum_should_match`.

    Le cas signale par un vrai trafic applicatif : un `bool` a plusieurs
    `should` et un « 75% », qui echouait en 400. Chaque attendu ci-dessous a
    ete constate sur un ES 8.15 (`tests/compat/sonde_msm.py`).

    Le decompte du corpus, sur ces quatre clauses : le document 1 les satisfait
    toutes, le 3 en satisfait deux, le 2 une seule.
    """
    quatre = [{"term": {"auteur": "Maupassant"}},
              {"term": {"dispo": True}},
              {"range": {"annee": {"gte": 1886}}},
              {"range": {"note": {"gte": 4.5}}}]

    def avec(spec, clauses=None):
        return sorted(ids(es.search(index=INDEX, query={"bool": {
            "should": clauses or quatre, "minimum_should_match": spec}})))

    assert avec("75%") == ["1"]
    assert avec("50%") == ["1", "3"]
    assert avec("25%") == ["1", "2", "3"]
    assert avec("100%") == ["1"]
    # L'arrondi est une troncature : 70% de 4 fait 2,8, donc 2.
    assert avec("70%") == ["1", "3"]
    # Un minimum plus grand que le nombre de clauses ne rend rien (il n'est
    # pas ramene a ce nombre).
    assert avec("150%") == []
    # Un pourcentage negatif se compte a partir du total : « tout sauf un ».
    assert avec("-25%") == ["1"]
    assert avec(-1) == ["1"]
    # Forme combinee : au-dela de 3 clauses, 90% ; ici 90% de 4 fait 3.
    assert avec("3<90%") == ["1"]
    # ... et en dessous de la borne, tout est exige.
    assert avec("3<90%", quatre[:3]) == ["1"]
    assert avec("67%", quatre[:3]) == ["1"]
    assert avec("66%", quatre[:3]) == ["1", "2", "3"]

    # Ce qui n'est pas une notation connue est refuse, jamais devine : ignorer
    # le parametre rendrait plus de documents que demande, en silence.
    for spec in ["abc", "75.5%", "2<25%,9<3", "75%x", 1.5]:
        refused(lambda s=spec: es.search(index=INDEX, query={"bool": {
            "should": quatre, "minimum_should_match": s}}),
            contains="minimum_should_match")


@scenario
def minimum_should_match_sous_un_nested(es):
    """Sous un `nested`, le minimum se compte **par element**.

    C'est ce qui distingue `nested` d'un `object` : le document 2 satisfait les
    deux clauses, mais jamais sur la meme ligne.
    """
    es.options(ignore_status=404).indices.delete(index="cmd3")
    es.indices.create(index="cmd3", mappings={"properties": {
        "lignes": {"type": "nested", "properties": {
            "produit": {"type": "keyword"},
            "quantite": {"type": "integer"},
        }},
    }})
    es.bulk(operations=[
        {"index": {"_index": "cmd3", "_id": "1"}},
        {"lignes": [{"produit": "clou", "quantite": 12}]},
        {"index": {"_index": "cmd3", "_id": "2"}},
        {"lignes": [{"produit": "clou", "quantite": 3},
                    {"produit": "vis", "quantite": 40}]},
        {"index": {"_index": "cmd3", "_id": "3"}},
        {"lignes": [{"produit": "vis", "quantite": 2}]},
    ], refresh=True)

    def hits(inner):
        return sorted(h["_id"] for h in es.search(
            index="cmd3", query={"nested": {"path": "lignes", "query": inner}},
            size=10)["hits"]["hits"])

    deux = [{"term": {"lignes.produit": "clou"}},
            {"range": {"lignes.quantite": {"gte": 10}}}]
    assert hits({"bool": {"should": deux,
                          "minimum_should_match": "100%"}}) == ["1"]
    assert hits({"bool": {"should": deux,
                          "minimum_should_match": "50%"}}) == ["1", "2"]
    assert hits({"bool": {"should": deux, "minimum_should_match": "-50%"}}) \
        == ["1", "2"]

    # Un `must_not` ne rend pas le `should` facultatif : il faut toujours un
    # element qui satisfait les deux. Le document 3 n'a aucun `clou`, et la
    # seule ligne `clou` du document 1 est trop grosse.
    assert hits({"bool": {"should": [{"term": {"lignes.produit": "clou"}}],
                          "must_not": [{"range": {"lignes.quantite": {"gte": 10}}}]}}) \
        == ["2"]

    refused(lambda: es.search(index="cmd3", query={"nested": {
        "path": "lignes", "query": {"bool": {
            "should": deux, "minimum_should_match": "abc"}}}}),
        contains="minimum_should_match")
    es.indices.delete(index="cmd3")


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


# ---------------------------------------------------------------------------
# scroll — la pagination par contexte fige, celle des exports
# ---------------------------------------------------------------------------

SCROLL_INDEX = "compat_scroll"


def _index_de_scroll(es, n=250):
    """Un index dedie, assez gros pour que l'export tienne en plusieurs pages."""
    es.options(ignore_status=404).indices.delete(index=SCROLL_INDEX)
    es.indices.create(index=SCROLL_INDEX, mappings={"properties": {
        "rang": {"type": "integer"}, "groupe": {"type": "keyword"}}})
    operations = []
    for i in range(n):
        operations.append({"index": {"_index": SCROLL_INDEX, "_id": str(i)}})
        operations.append({"rang": i, "groupe": f"g{i % 5}"})
    es.bulk(operations=operations, refresh=True)
    return n


@scenario
def scroll_page_par_page(es):
    """Le cycle complet : ouvrir, derouler, fermer."""
    total = _index_de_scroll(es)
    r = es.search(index=SCROLL_INDEX, scroll="1m", size=100,
                  query={"match_all": {}}, sort=["_doc"])
    assert r["_scroll_id"], "pas de _scroll_id sur la premiere reponse"
    assert r["hits"]["total"]["value"] == total
    vus = ids(r)
    assert len(vus) == 100

    sid = r["_scroll_id"]
    while True:
        r = es.scroll(scroll_id=sid, scroll="1m")
        sid = r["_scroll_id"]
        # Le total ne bouge pas d'une page a l'autre, comme chez ES.
        assert r["hits"]["total"]["value"] == total
        if not r["hits"]["hits"]:
            break
        vus += ids(r)

    # Chaque document une fois, et une seule.
    assert len(vus) == total, f"{len(vus)} documents rendus pour {total}"
    assert len(set(vus)) == total, "un document a ete rendu deux fois"
    liberes = es.clear_scroll(scroll_id=sid)
    assert liberes["succeeded"] and liberes["num_freed"] == 1
    # Ferme, le contexte n'existe plus : 404, et la cause nommee — c'est a ca
    # qu'un client reconnait « ton scroll a expire », pas a « requete invalide ».
    err = refused(lambda: es.scroll(scroll_id=sid, scroll="1m"), status=404,
                  contains="all shards failed")
    assert err["root_cause"][0]["type"] == "search_context_missing_exception"
    assert "No search context found" in err["root_cause"][0]["reason"]


@scenario
def export_complet_par_helpers_scan(es):
    """Le juge de paix de la carte : `helpers.scan`, le code que tout export
    ecrit (dont `devbox timemachine export`), sans une ligne de changement.

    Il ouvre un scroll avec `sort=_doc`, deroule jusqu'a la page vide, verifie
    `_shards` a chaque page, puis appelle `clear_scroll`."""
    from elasticsearch import helpers

    total = _index_de_scroll(es)
    vus = [d["_id"] for d in helpers.scan(
        es, index=SCROLL_INDEX, query={"query": {"match_all": {}}},
        scroll="1m", size=64)]
    assert len(vus) == total and len(set(vus)) == total, len(vus)

    # Avec une requete qui ne prend qu'une partie des documents.
    partiel = [d["_source"]["rang"] for d in helpers.scan(
        es, index=SCROLL_INDEX, query={"query": {"term": {"groupe": "g3"}}},
        scroll="1m", size=10)]
    assert sorted(partiel) == [i for i in range(total) if i % 5 == 3]


@scenario
def scroll_fige_l_index(es):
    """La promesse de `scroll` : ce qui est ecrit pendant l'export ne s'y
    invite pas, et rien de ce qui existait ne se perd.

    C'est le seul point vraiment delicat de la fonctionnalite : sans un
    instantane retenu, un commit pendant l'export renumerote les segments et les
    documents deja reperes ne sont plus les memes."""
    total = _index_de_scroll(es, 40)
    r = es.search(index=SCROLL_INDEX, scroll="1m", size=10,
                  query={"match_all": {}}, sort=["_doc"])
    vus, sid = ids(r), r["_scroll_id"]

    # 20 documents de plus, visibles pour toute recherche neuve.
    operations = []
    for i in range(1000, 1020):
        operations.append({"index": {"_index": SCROLL_INDEX, "_id": str(i)}})
        operations.append({"rang": i, "groupe": "tardif"})
    es.bulk(operations=operations, refresh=True)
    assert es.search(index=SCROLL_INDEX, size=0,
                     query={"match_all": {}})["hits"]["total"]["value"] == total + 20

    while True:
        r = es.scroll(scroll_id=sid, scroll="1m")
        sid = r["_scroll_id"]
        if not r["hits"]["hits"]:
            break
        vus += ids(r)
    es.clear_scroll(scroll_id=sid)

    assert len(vus) == total and len(set(vus)) == total, len(vus)
    assert not any(int(i) >= 1000 for i in vus), "un document ecrit pendant le scroll est apparu"


@scenario
def scroll_agregations_sur_la_premiere_page(es):
    """Les agregations portent sur tout le resultat, et ne sont rendues qu'une
    fois — comme chez ES, qui ne les recalcule pas a chaque page."""
    _index_de_scroll(es, 50)
    r = es.search(index=SCROLL_INDEX, scroll="1m", size=10,
                  query={"match_all": {}}, sort=["_doc"],
                  aggs={"par_groupe": {"terms": {"field": "groupe"}}})
    buckets = {b["key"]: b["doc_count"] for b in r["aggregations"]["par_groupe"]["buckets"]}
    assert buckets == {f"g{i}": 10 for i in range(5)}, buckets
    suite = es.scroll(scroll_id=r["_scroll_id"], scroll="1m")
    assert "aggregations" not in suite
    es.clear_scroll(scroll_id=r["_scroll_id"])


@scenario
def scroll_refus_explicites(es):
    """Ce que scroll ne peut pas faire doit se dire."""
    # `from` dans un contexte de scroll : ES le refuse, ferrite aussi.
    refused(lambda: es.search(index=INDEX, scroll="1m", from_=2, size=1,
                              query={"match_all": {}}),
            contains="from")
    # Une duree sans unite est le piege classique (`scroll=1`).
    refused(lambda: es.search(index=INDEX, scroll="1", query={"match_all": {}}),
            contains="unit is missing")
    refused(lambda: es.search(index=INDEX, scroll="2d", query={"match_all": {}}),
            contains="too large")
    # Un identifiant qui n'a jamais existe : 404, pas 500.
    err = refused(lambda: es.scroll(scroll_id="jamais_ouvert", scroll="1m"),
                  status=404)
    assert err["root_cause"][0]["type"] == "search_context_missing_exception"
    # Fermer deux fois n'est pas une erreur : c'est le cas normal d'un client
    # qui nettoie apres une interruption.
    r = es.search(index=INDEX, scroll="1m", size=1, query={"match_all": {}})
    assert es.clear_scroll(scroll_id=r["_scroll_id"])["num_freed"] == 1
    assert es.clear_scroll(scroll_id=r["_scroll_id"])["num_freed"] == 0


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
    refused(lambda: es.search(index=INDEX, query={"match": {
        "titre": {"query": "bel", "zero_terms_query": "all"}}}),
        contains="zero_terms_query")


@scenario
def champ_non_mappe_ne_correspond_a_rien(es):
    """Un champ absent du mapping ne correspond a rien, comme chez ES.

    Le cas reel qui l'a impose : un filtre pose sur **chaque** recherche
    (`archiveAt`, jamais renseigne tant qu'aucune commande n'est archivee).
    Faire echouer la recherche entiere rendait l'application inutilisable
    jusqu'a ce qu'un premier document porte le champ ; ES, lui, repond 0.

    C'est le reglage `index.query.parse.allow_unmapped_fields` d'ES, avec son
    defaut (`true`)."""
    r = es.search(index=INDEX, query={"term": {"inconnu": "x"}})
    assert r["hits"]["total"]["value"] == 0
    r = es.search(index=INDEX, query={"exists": {"field": "archiveAt"}})
    assert r["hits"]["total"]["value"] == 0
    # La negation d'une clause qui ne correspond a rien correspond a tout.
    r = es.search(index=INDEX, query={
        "bool": {"must_not": [{"exists": {"field": "archiveAt"}}]}})
    assert r["hits"]["total"]["value"] == 3
    # Et les clauses qui l'entourent continuent de compter.
    r = es.search(index=INDEX, query={"bool": {"should": [
        {"term": {"inconnu": "x"}}, {"term": {"auteur": "Zola"}}]}})
    assert ids(r) == ["3"]


@scenario
def refus_de_clause_survit_a_un_champ_non_mappe(es):
    """Un champ non mappe ne doit pas **avaler** le refus de la clause.

    Le meme piege que la recherche sans index, un cran plus bas : `range`,
    `term`, `terms` et `regexp` resolvaient leur champ avant de lire leurs
    parametres, donc un champ jamais mappe (tolere par
    `allow_unmapped_fields`) court-circuitait la clause **avant** son refus.
    `time_zone`, `relation`, un *terms lookup*, `case_insensitive` et les
    operateurs Lucene que ferrite ne construit pas passaient alors en silence
    — exactement ce que ce projet refuse.

    Trouve par le rejeu du corpus d'usage (`tests/compat/ponderation.py`), qui
    posait les memes requetes a ferrite et a un ES 8.15."""
    for clause, contient in (
            ({"range": {"jamais_mappe": {"gte": "2020-01-01", "time_zone": "+01:00"}}},
             "time_zone"),
            ({"range": {"jamais_mappe": {"gte": "2020-01-01", "relation": "within"}}},
             "relation"),
            ({"terms": {"jamais_mappe": {"index": "autre", "id": "1", "path": "p"}}},
             "lookup"),
            ({"term": {"jamais_mappe": {"value": "x", "case_insensitive": True}}},
             "case_insensitive"),
            ({"regexp": {"jamais_mappe": "bel~ami"}}, "~"),
    ):
        refused(lambda c=clause: es.search(index=INDEX, query=c), contains=contient)
    # Et ce qui est supporte continue de ne correspondre a rien, sans erreur.
    r = es.search(index=INDEX, query={"range": {"jamais_mappe": {"gte": "2020-01-01"}}})
    assert r["hits"]["total"]["value"] == 0
    r = es.search(index=INDEX, query={"regexp": {"jamais_mappe": "bel.*"}})
    assert r["hits"]["total"]["value"] == 0


@scenario
def champ_non_mappe_refuse_en_strict(es):
    """Le mode strict de ferrite reste disponible, sous le nom d'ES.

    `allow_unmapped_fields: false` : interroger un champ inconnu redevient une
    erreur, ce qui attrape les fautes de frappe. C'est ce que ferrite faisait
    pour tous les index avant d'apprendre le reglage."""
    strict = "compat_strict"
    es.options(ignore_status=404).indices.delete(index=strict)
    es.indices.create(index=strict,
                      settings={"index.query.parse.allow_unmapped_fields": False},
                      mappings={"properties": {"auteur": {"type": "keyword"}}})
    reglages = es.indices.get_settings(index=strict)
    assert reglages[strict]["settings"]["index"]["query"]["parse"][
        "allow_unmapped_fields"] == "false"
    refused(lambda: es.search(index=strict, query={"term": {"inconnu": "x"}}),
            contains="inconnu")
    # Le reglage ne se change pas a chaud, et ferrite le dit plutot que de
    # laisser croire que la demande a ete prise en compte.
    refused(lambda: es.indices.put_settings(
        index=strict, settings={"index.query.parse.allow_unmapped_fields": True}),
        contains="apres la creation de l'index")
    es.indices.delete(index=strict)


@scenario
def parametres_de_reglages_non_appliques_refuses(es):
    """`flat_settings` et `include_defaults` changent la **forme** de la
    reponse chez ES : le premier aplatit les cles
    (`settings["index.number_of_shards"]`), le second ajoute une section
    `defaults`. ferrite les acceptait et rendait la reponse inchangee — un
    client qui lit la cle aplatie n'y trouvait rien, sans la moindre erreur.

    Depuis, `flat_settings` est **applique** la ou ferrite rend des reglages
    d'index : ce n'est qu'une reecriture des cles, et il n'y avait aucune raison
    de le refuser une fois ecrit. `include_defaults`, lui, reste refuse — ferrite
    n'a pas les dizaines de reglages qu'il ajouterait."""
    plat = es.indices.get_settings(index=INDEX, flat_settings=True)[INDEX]["settings"]
    assert plat["index.number_of_shards"] == "1", plat
    for appel in (
        lambda: es.indices.get_settings(index=INDEX, include_defaults=True),
        lambda: es.cluster.get_settings(flat_settings=True),
        lambda: es.cluster.get_settings(include_defaults=True),
    ):
        refused(appel)
    # Sans lui, la reponse est celle d'avant.
    assert es.indices.get_settings(index=INDEX)[INDEX]["settings"]["index"][
        "number_of_shards"] == "1"


@scenario
def fonctionnalites_hors_perimetre_refusees(es):
    refused(lambda: es.search(index=INDEX, query={"match_all": {}},
                              search_after=[1885], sort=[{"annee": "asc"}]),
            contains="search_after")
    refused(lambda: es.search(index=INDEX, q="titre:bel"), contains="q")


@scenario
def surlignage(es):
    """Les fragments surlignes d'une barre de recherche.

    Ce qui compte pour le client n'est pas qu'il y ait des `<em>` : c'est **ou**
    le fragment commence et finit. ES ne rend ni « la phrase » ni
    « `fragment_size` caracteres » — il fusionne les phrases vers l'avant tant
    que ca tient sous la borne, et re-coupe au mot sinon. Les valeurs attendues
    ici sont celles qu'un vrai ES 8.15 rend sur le meme texte
    (`tests/compat/diff_highlight.py` le rejoue sur un corpus entier)."""
    idx = "compat_highlight"
    es.options(ignore_status=404).indices.delete(index=idx)
    es.indices.create(index=idx, mappings={"properties": {
        "titre": {"type": "text"},
        "corps": {"type": "text"},
        "tag": {"type": "keyword"},
        "n": {"type": "integer"},
    }})
    prose = ("Le chat dort sur le tapis. Le chien aboie dans le jardin voisin "
             "depuis ce matin. Un oiseau chante sur la branche du grand chene. "
             "Le chat se reveille et regarde l'oiseau avec attention.")
    es.index(index=idx, id="1", document={
        "titre": "Le chat noir", "corps": prose, "tag": "animaux", "n": 1})
    es.index(index=idx, id="2", document={
        "titre": "Multi", "n": 2, "tag": "multi",
        "corps": ["Premier chat ici.", "Rien la.", "Troisieme chat enfin."]})
    es.indices.refresh(index=idx)

    def frags(**kw):
        r = es.search(index=idx, size=10, sort=["n"], **kw)
        return [h.get("highlight") for h in r["hits"]["hits"]]

    # Le defaut : `<em>`, cinq fragments au plus, cent caracteres visés — donc
    # deux phrases fusionnees ici, la seconde etant courte.
    assert frags(query={"match": {"corps": "chat"}},
                 highlight={"fields": {"corps": {}}}) == [
        {"corps": [
            "Le <em>chat</em> dort sur le tapis. Le chien aboie dans le jardin "
            "voisin depuis ce matin.",
            "Le <em>chat</em> se reveille et regarde l'oiseau avec attention."]},
        {"corps": ["Premier <em>chat</em> ici.", "Troisieme <em>chat</em> enfin."]},
    ]
    # `fragment_size` re-coupe **au mot** quand une phrase deborde a elle seule.
    assert frags(query={"match": {"corps": "chat"}},
                 highlight={"fragment_size": 30, "fields": {"corps": {}}}) == [
        {"corps": ["Le <em>chat</em> dort sur le tapis.",
                   "Le <em>chat</em> se reveille et regarde"]},
        {"corps": ["Premier <em>chat</em> ici.", "Troisieme <em>chat</em> enfin."]},
    ]
    # Une phrase rend **une seule** marque, du premier terme au dernier.
    assert frags(query={"match_phrase": {"corps": "le chat"}},
                 highlight={"fragment_size": 30, "fields": {"corps": {}}})[0] == {
        "corps": ["<em>Le chat</em> dort sur le tapis.",
                  "<em>Le chat</em> se reveille et regarde"]}
    # Les balises se choisissent, et `number_of_fragments` borne le nombre.
    # Celui qui reste n'est pas le premier : c'est le mieux note (le
    # `PassageScorer` de Lucene prefere ici le fragment le plus court).
    assert frags(query={"match": {"corps": "chat"}},
                 highlight={"pre_tags": ["<b>"], "post_tags": ["</b>"],
                            "number_of_fragments": 1,
                            "fields": {"corps": {}}})[0] == {
        "corps": ["Le <b>chat</b> se reveille et regarde l'oiseau avec attention."]}
    # `number_of_fragments: 0` rend la valeur entiere, valeur par valeur.
    assert frags(query={"match": {"corps": "chat"}},
                 highlight={"number_of_fragments": 0,
                            "fields": {"corps": {}}})[1] == {
        "corps": ["Premier <em>chat</em> ici.", "Troisieme <em>chat</em> enfin."]}
    # Un champ sans correspondance est **absent** — ce n'est pas une chaine
    # vide — et `no_match_size` est ce qui le ramene.
    assert frags(query={"match": {"corps": "chat"}},
                 highlight={"fields": {"corps": {}, "titre": {}}})[0].keys() \
        == {"corps"}
    assert frags(query={"match": {"corps": "chat"}},
                 highlight={"no_match_size": 20,
                            "fields": {"titre": {}}})[0] == {
        "titre": ["Le chat noir"]}
    # Un motif designe les champs, et seuls les `text` / `keyword` repondent.
    assert frags(query={"term": {"tag": "animaux"}},
                 highlight={"fields": {"*": {}}})[0] == {"tag": ["<em>animaux</em>"]}
    # `require_field_match` (vrai par defaut) : le champ n'est surligne que par
    # ce que la requete y pose. `false` est refuse — ES y cherche les termes de
    # toutes les clauses dans tous les champs, par une extraction dont il dit
    # lui-meme qu'elle est approximative.
    assert frags(query={"match": {"titre": "chat"}},
                 highlight={"fields": {"corps": {}}})[0] is None
    refused(lambda: es.search(index=idx, query={"match": {"titre": "chat"}},
                              highlight={"require_field_match": False,
                                         "fields": {"corps": {}}}),
            contains="[require_field_match: false]")

    # Ce qui n'est pas reproduit est refuse **par son nom** : accepte en
    # silence, un `type: fvh` rendrait des fragments coupes autrement.
    for cle, valeur in (("type", "fvh"), ("boundary_scanner", "word"),
                        ("encoder", "html"), ("fragmenter", "simple"),
                        ("order", "score")):
        refused(lambda c=cle, v=valeur: es.search(
            index=idx, query={"match": {"corps": "chat"}},
            highlight={c: v, "fields": {"corps": {}}}), contains=f"[{cle}]")
    refused(lambda: es.search(index=idx, query={"match_all": {}},
                              highlight={"highlight_query": {"match": {"corps": "chat"}},
                                         "fields": {"corps": {}}}),
            contains="[highlight_query]")
    refused(lambda: es.search(index=idx, query={"match": {"corps": "chat"}},
                              highlight={"fields": {"corps": {
                                  "matched_fields": ["corps"]}}}),
            contains="[matched_fields]")
    # Deux fautes de forme qu'ES refuse aussi, avec ses phrases.
    refused(lambda: es.search(index=idx, query={"match": {"corps": "chat"}},
                              highlight={"pre_tags": ["<b>"], "fields": {"corps": {}}}),
            contains="pre_tags are set but post_tags are not set")
    refused(lambda: es.search(index=idx, query={"match": {"corps": "chat"}},
                              highlight={"nawak": 1, "fields": {"corps": {}}}),
            contains="unknown field [nawak]")
    es.indices.delete(index=idx)


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
def agregation_filter(es):
    """Compter un sous-ensemble sans faire une requete de plus : c'est ce dont
    se servent les compteurs de filtres rapides d'une interface."""
    r = es.search(index=INDEX, size=0, aggs={
        "recents": {"filter": {"range": {"annee": {"gte": 1886}}}},
        "de_zola": {"filter": {"term": {"auteur": "Zola"}}},
        "aucun": {"filter": {"term": {"auteur": "Personne"}}},
    })
    a = r["aggregations"]
    assert a["recents"] == {"doc_count": 1}, a["recents"]
    assert a["de_zola"] == {"doc_count": 1}
    assert a["aucun"] == {"doc_count": 0}

    # Avec des sous-agregations : elles portent sur le croisement.
    r = es.search(index=INDEX, size=0, aggs={"anciens": {
        "filter": {"range": {"annee": {"lt": 1887}}},
        "aggs": {"par_auteur": {"terms": {"field": "auteur"}},
                 "note_moyenne": {"avg": {"field": "note"}}}}})
    a = r["aggregations"]["anciens"]
    assert a["doc_count"] == 2
    assert {b["key"]: b["doc_count"] for b in a["par_auteur"]["buckets"]} == \
        {"Maupassant": 1, "Zola": 1}
    assert round(a["note_moyenne"]["value"], 2) == 4.45

    # Le filtre se croise avec la requete de la recherche, pas a la place.
    r = es.search(index=INDEX, size=0, query={"term": {"auteur": "Maupassant"}},
                  aggs={"recents": {"filter": {"range": {"annee": {"gte": 1886}}}}})
    assert r["aggregations"]["recents"]["doc_count"] == 1

    # Un `filter` dans un `filter` : le croisement se poursuit.
    r = es.search(index=INDEX, size=0, aggs={"a": {
        "filter": {"match_all": {}},
        "aggs": {"b": {"filter": {"term": {"auteur": "Zola"}}}}}})
    assert r["aggregations"]["a"] == {"doc_count": 3, "b": {"doc_count": 1}}


@scenario
def compteurs_de_filtres_rapides(es):
    """Le cas reel qui a motive tout ca : une recherche de commandes qui pose,
    a **chaque** appel, un filtre sur un champ jamais renseigne et cinq
    compteurs en agregations `filter`.

    Rien ici n'est artificiel : c'est la forme exacte de la requete d'un
    service de livraison branche sur ferrite, sur un jeu ou aucune commande
    n'est archivee (donc `archiveAt` n'est jamais mappe)."""
    commandes = "compat_commandes"
    es.options(ignore_status=404).indices.delete(index=commandes)
    es.indices.create(index=commandes, mappings={"properties": {
        "reference": {"type": "keyword"},
        "statut": {"type": "keyword"},
        "enlevementPrevu": {"type": "date"},
        "livraisonPrevue": {"type": "date"},
    }})
    etats = ["pending", "pending", "shipped", "failed", "delivered"]
    operations = []
    for i, statut in enumerate(etats):
        operations.append({"index": {"_index": commandes, "_id": str(i)}})
        operations.append({"reference": f"CMD-{i}", "statut": statut,
                           "enlevementPrevu": "2026-07-01T08:00:00Z",
                           "livraisonPrevue": "2026-07-02T08:00:00Z"})
    es.bulk(operations=operations, refresh=True)

    limite = "2026-07-01T12:00:00Z"
    r = es.search(index=commandes, size=10, query={"bool": {
        # Le filtre pose sur chaque recherche : « pas encore archivee ».
        "must_not": [{"exists": {"field": "archiveAt"}}],
        "must": [{"match_all": {}}],
    }}, aggs={
        "pending": {"filter": {"term": {"statut": "pending"}}},
        "late": {"filter": {"bool": {
            "must": [{"range": {"livraisonPrevue": {"lt": limite}}}],
            "must_not": [{"term": {"statut": "delivered"}}]}}},
        "latePickup": {"filter": {"bool": {
            "must": [{"range": {"enlevementPrevu": {"lt": limite}}}],
            "must_not": [{"terms": {"statut": ["shipped", "delivered"]}}]}}},
        "failed": {"filter": {"term": {"statut": "failed"}}},
        "active": {"filter": {"bool": {
            "must_not": [{"terms": {"statut": ["delivered", "failed"]}}]}}},
    })
    assert r["hits"]["total"]["value"] == 5
    compteurs = {k: v["doc_count"] for k, v in r["aggregations"].items()}
    assert compteurs == {"pending": 2, "late": 0, "latePickup": 3,
                         "failed": 1, "active": 3}, compteurs
    es.indices.delete(index=commandes)


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
    # `filter` est supportee au premier niveau ; sous une agregation de
    # buckets, elle exigerait de rejouer sa requete bucket par bucket.
    refused(lambda: es.search(index=INDEX, size=0, aggs={
        "pa": {"terms": {"field": "auteur"},
               "aggs": {"f": {"filter": {"term": {"auteur": "Zola"}}}}}}),
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
    refused(lambda: es.cat.indices(index=INDEX, format="json", h="index"),
            contains="h")
    # `expand_wildcards=none` demanderait de chercher un motif comme un nom
    # litteral : refuse plutot que d'inventer une erreur sur un nom que
    # personne n'a ecrit.
    refused(lambda: es.search(index="compat_*", query={"match_all": {}},
                              expand_wildcards="none"),
            contains="expand_wildcards")


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
    # Un motif multi-index, lui, est resolu : il ne trouve rien, sans erreur.
    r = es.search(index="fantome_absolu_*", query={"match_all": {}})
    assert r["hits"]["total"]["value"] == 0


@scenario
def purge_totale_du_script_d_init(es):
    """`DELETE /*` : ce que fait un script d'initialisation avant de repartir.

    **Ce scenario doit rester le dernier** : il supprime tous les index du
    serveur, y compris celui de la suite. Il est ici parce que c'est
    exactement ce qu'un projet ecrit en tete de son script d'init, et que
    l'interdiction par defaut d'ES 8 le casse tel quel : la seule preuve utile
    est de le faire aboutir en basculant le reglage, pas de le contourner.
    """
    for nom in ("init_a", "init_b"):
        es.options(ignore_status=404).indices.delete(index=nom)
        es.indices.create(index=nom)

    # Par defaut, ES 8 refuse — ferrite aussi, avec le meme message.
    refused(lambda: es.indices.delete(index="*"),
            contains="Wildcard expressions or all indices are not allowed")
    refused(lambda: es.indices.delete(index="_all"),
            contains="Wildcard expressions or all indices are not allowed")

    # Le reglage se pose a plat ou en arborescence : ES accepte les deux.
    resp = es.cluster.put_settings(persistent={"action": {"destructive_requires_name": False}})
    assert resp["acknowledged"] is True
    lu = es.cluster.get_settings()["persistent"]["action"]["destructive_requires_name"]
    assert lu in ("false", False), lu

    # Et le script d'init passe.
    es.indices.delete(index="*")
    assert es.indices.get(index="*") == {}, "il ne devait plus rester d'index"

    # `transient` l'emporte sur `persistent`, comme chez ES.
    es.cluster.put_settings(transient={"action.destructive_requires_name": True})
    es.indices.create(index="init_c")
    refused(lambda: es.indices.delete(index="init_*"),
            contains="Wildcard expressions or all indices are not allowed")
    es.cluster.put_settings(transient={"action.destructive_requires_name": None})
    es.indices.delete(index="init_*")

    # Remis dans l'etat par defaut, et un reglage inconnu reste refuse.
    es.cluster.put_settings(persistent={"action.destructive_requires_name": None})
    assert es.cluster.get_settings()["persistent"] == {}
    refused(lambda: es.cluster.put_settings(persistent={"indices.recovery.max_bytes_per_sec": "50mb"}),
            contains="not recognized")
    refused(lambda: es.indices.delete(index="*"),
            contains="Wildcard expressions or all indices are not allowed")


# ---------------------------------------------------------------------------
# Les petites routes qui bloquent un outil entier
# ---------------------------------------------------------------------------

FC = "compat_fc"
FC2 = "compat_fc2"
JOURNAUX = ["compat_log-2026.01.01", "compat_log-2026.01.02"]


@scenario
def field_caps(es):
    """`_field_caps` : le type de chaque champ, et son type **par index**."""
    for nom in (FC, FC2):
        es.options(ignore_status=404).indices.delete(index=nom)
    es.indices.create(index=FC, mappings={"properties": {
        "titre": {"type": "text"},
        "tag": {"type": "keyword"},
        "prix": {"type": "long"},
        "client": {"type": "object", "properties": {"ville": {"type": "keyword"}}},
        "lignes": {"type": "nested", "properties": {"ref": {"type": "keyword"}}},
    }})
    es.indices.create(index=FC2, mappings={"properties": {
        "tag": {"type": "text"},
        "prix": {"type": "long"},
    }})

    caps = es.field_caps(index=FC, fields="*")
    assert caps["indices"] == [FC], caps["indices"]
    f = caps["fields"]
    assert f["titre"]["text"]["searchable"] is True
    # Un `text` est analyse : ES ne l'agrege pas sans `fielddata`.
    assert f["titre"]["text"]["aggregatable"] is False
    assert f["tag"]["keyword"]["aggregatable"] is True
    # Les conteneurs sont rendus, et ne sont ni cherchables ni agregeables.
    assert f["client"]["object"]["searchable"] is False
    assert f["lignes"]["nested"]["searchable"] is False
    assert f["client.ville"]["keyword"]["aggregatable"] is True
    assert f["lignes.ref"]["keyword"]["aggregatable"] is True

    # Deux index, deux types pour le meme nom : c'est **la** question que pose
    # un outil de decouverte avant de proposer un filtre.
    caps = es.field_caps(index=[FC, FC2], fields=["tag", "prix"])
    tag = caps["fields"]["tag"]
    assert set(tag) == {"keyword", "text"}, tag
    assert tag["keyword"]["indices"] == [FC]
    assert tag["text"]["indices"] == [FC2]
    # Un seul type : pas de `indices`, meme quand plusieurs index sont vises.
    assert "indices" not in caps["fields"]["prix"]["long"]

    # `include_unmapped` fait apparaitre les index qui ne connaissent pas le
    # champ — et fait donc apparaitre `indices` sur l'entree qui le connait.
    caps = es.field_caps(index=[FC, FC2], fields="titre", include_unmapped=True)
    titre = caps["fields"]["titre"]
    assert titre["text"]["indices"] == [FC]
    assert titre["unmapped"]["indices"] == [FC2]

    # `index_filter` : ne decrire que les index qui ont quelque chose a dire.
    es.index(index=FC, id="1", document={"prix": 10}, refresh=True)
    caps = es.field_caps(index=[FC, FC2], fields="prix",
                         index_filter={"range": {"prix": {"gte": 5}}})
    assert caps["indices"] == [FC], caps["indices"]

    # Sans `fields`, il n'y a rien a decrire : ES refuse, ferrite aussi.
    refused(lambda: es.field_caps(index=FC),
            contains="no fields specified")
    es.indices.delete(index=[FC, FC2])


@scenario
def validate_query(es):
    """`_validate/query` : le parseur du DSL expose, sans executer."""
    INDEX = "compat_validate"
    es.options(ignore_status=404).indices.delete(index=INDEX)
    es.indices.create(index=INDEX, mappings={"properties": {
        "titre": {"type": "text"}, "annee": {"type": "integer"}}})

    r = es.indices.validate_query(index=INDEX, query={"match": {"titre": "horla"}})
    assert r["valid"] is True, r

    r = es.indices.validate_query(index=INDEX, explain=True,
                                  query={"match": {"titre": "horla"}})
    assert r["_shards"]["failed"] == 0
    assert r["explanations"][0]["index"] == INDEX
    assert r["explanations"][0]["valid"] is True
    assert r["explanations"][0]["explanation"], "une explication est attendue"

    # Sans corps : la requete est `*:*`, et elle est valide.
    r = es.indices.validate_query(index=INDEX, explain=True)
    assert r["valid"] is True
    assert r["explanations"][0]["explanation"] == "*:*"

    # Une clause inconnue est une erreur **de forme** : ES ne rend alors ni
    # `_shards` ni `explanations`, seulement `valid` (et `error` avec explain).
    r = es.indices.validate_query(index=INDEX, query={"match_zzz": {"titre": "x"}})
    assert r["valid"] is False, r
    assert "error" not in r, r
    r = es.indices.validate_query(index=INDEX, explain=True,
                                  query={"match_zzz": {"titre": "x"}})
    assert r["valid"] is False
    assert "match_zzz" in r["error"], r["error"]

    # Une valeur qui n'a pas le type du champ, elle, ne se voit qu'avec le
    # mapping : la reponse porte alors `_shards` et une explication par index.
    r = es.indices.validate_query(index=INDEX, explain=True,
                                  query={"range": {"annee": {"gte": "pas un nombre"}}})
    assert r["valid"] is False, r
    assert r["explanations"][0]["valid"] is False
    assert "annee" in r["explanations"][0]["error"], r["explanations"][0]

    # `q` (query_string) n'est pas implemente : refus explicite, pas un silence.
    refused(lambda: es.indices.validate_query(index=INDEX, q="titre:horla"),
            contains="query_string")
    es.indices.delete(index=INDEX)


@scenario
def stats(es):
    """`_stats` : les compteurs que ferrite mesure, et le refus des autres."""
    INDEX = "compat_stats"
    es.options(ignore_status=404).indices.delete(index=INDEX)
    es.indices.create(index=INDEX, mappings={"properties": {"a": {"type": "keyword"}}})
    for i in range(3):
        es.index(index=INDEX, id=str(i), document={"a": "x"}, refresh=True)

    st = es.indices.stats(index=INDEX)
    assert st["_shards"]["failed"] == 0
    assert st["_all"]["primaries"]["docs"]["count"] == 3
    assert st["_all"]["total"]["docs"]["count"] == 3
    idx = st["indices"][INDEX]
    assert idx["uuid"], "l'uuid de l'index est attendu"
    assert idx["status"] == "open"
    assert idx["primaries"]["store"]["size_in_bytes"] > 0
    assert idx["primaries"]["docs"]["count"] == 3

    # Un sous-ensemble de metriques.
    st = es.indices.stats(index=INDEX, metric="docs")
    assert set(st["_all"]["primaries"]) == {"docs"}, st["_all"]["primaries"]

    # Un motif sans correspondance : zero shard, pas d'erreur.
    st = es.indices.stats(index="pas_un_index-*")
    assert st["_shards"]["total"] == 0
    assert st["indices"] == {}

    # ES nomme la metrique la plus proche ; ferrite reprend son message.
    err = refused(lambda: es.indices.stats(metric="fieldata"))
    assert "did you mean [fielddata]?" in err["reason"], err["reason"]

    # Un compteur que ferrite ne tient pas est **refuse**, pas rendu a zero :
    # un `index_total: 0` sur un index qu'on vient de remplir ferait passer
    # « non mesure » pour « aucune activite ».
    refused(lambda: es.indices.stats(index=INDEX, metric="indexing"),
            contains="il ne tient pas ce compteur")
    es.indices.delete(index=INDEX)


@scenario
def put_settings(es):
    """`PUT /{index}/_settings` : les reglages inertes acceptes, les autres non."""
    nom = "compat_settings"
    es.options(ignore_status=404).indices.delete(index=nom)
    es.indices.create(index=nom)

    # Le geste d'un script d'init : poser un nombre de repliques. Sans effet
    # ici, mais faire echouer le script entier pour autant serait pire.
    assert es.indices.put_settings(index=nom, settings={"number_of_replicas": 1})["acknowledged"]
    lu = es.indices.get_settings(index=nom)[nom]["settings"]["index"]
    assert lu["number_of_replicas"] == "1", lu

    # `flat_settings` aplatit les cles, comme chez ES.
    plat = es.indices.get_settings(index=nom, flat_settings=True)[nom]["settings"]
    assert plat["index.number_of_replicas"] == "1", plat

    # `preserve_existing` ne pose que ce qui manque.
    es.indices.put_settings(index=nom, preserve_existing=True,
                            settings={"number_of_replicas": 3})
    lu = es.indices.get_settings(index=nom)[nom]["settings"]["index"]
    assert lu["number_of_replicas"] == "1", lu

    # `null` efface, comme chez ES.
    es.indices.put_settings(index=nom, settings={"index.refresh_interval": "30s"})
    es.indices.put_settings(index=nom, settings={"index.refresh_interval": None})
    lu = es.indices.get_settings(index=nom)[nom]["settings"]["index"]
    assert "refresh_interval" not in lu, lu

    # Un reglage fige a la creation ne se modifie pas, et ES le dit ainsi.
    refused(lambda: es.indices.put_settings(index=nom, settings={"index.number_of_shards": 2}),
            contains="Can't update non dynamic settings")
    # Un reglage que ferrite n'applique pas est refuse, jamais avale.
    refused(lambda: es.indices.put_settings(index=nom, settings={"index.blocks.read_only": True}),
            contains="ne supporte pas le reglage d'index")
    es.indices.delete(index=nom)


@scenario
def index_template(es):
    """`_index_template` : un mapping applique a un index qui n'existe pas."""
    es.options(ignore_status=404).indices.delete_index_template(name="compat_tpl")
    # Jamais de motif ici : `action.destructive_requires_name` le refuse, comme
    # sur un vrai ES 8.
    for nom in JOURNAUX:
        es.options(ignore_status=404).indices.delete(index=nom)

    assert es.indices.put_index_template(
        name="compat_tpl",
        index_patterns=["compat_log-*"],
        priority=100,
        version=3,
        template={
            "settings": {"number_of_replicas": 0},
            "mappings": {"properties": {"ts": {"type": "date"},
                                        "niveau": {"type": "keyword"}}},
            "aliases": {"compat_log": {}},
        },
    )["acknowledged"]

    lu = es.indices.get_index_template(name="compat_tpl")["index_templates"][0]
    assert lu["name"] == "compat_tpl"
    assert lu["index_template"]["index_patterns"] == ["compat_log-*"]
    assert lu["index_template"]["priority"] == 100
    assert lu["index_template"]["version"] == 3
    assert es.indices.exists_index_template(name="compat_tpl")
    assert not es.indices.exists_index_template(name="compat_tpl_absent")

    # L'ecriture cree l'index — et le template lui donne son mapping, ses
    # reglages et son alias. C'est tout l'objet d'un template.
    es.index(index="compat_log-2026.01.01", document={"ts": "2026-01-01", "niveau": "warn"},
             refresh=True)
    vu = es.indices.get(index="compat_log-2026.01.01")["compat_log-2026.01.01"]
    assert vu["mappings"]["properties"]["ts"] == {"type": "date"}
    assert vu["mappings"]["properties"]["niveau"] == {"type": "keyword"}
    assert "compat_log" in vu["aliases"], vu["aliases"]
    # Et l'alias sert : c'est par lui qu'une application lit ses index du jour.
    assert es.search(index="compat_log")["hits"]["total"]["value"] == 1

    # Une creation **explicite** applique aussi le template ; ce que le corps
    # dit l'emporte.
    es.indices.create(index="compat_log-2026.01.02",
                      mappings={"properties": {"extra": {"type": "keyword"}}})
    vu = es.indices.get(index="compat_log-2026.01.02")["compat_log-2026.01.02"]
    assert vu["mappings"]["properties"]["ts"] == {"type": "date"}
    assert vu["mappings"]["properties"]["extra"] == {"type": "keyword"}

    # Deux templates de meme priorite dont les motifs se recouvrent rendraient
    # la creation ambigue : ES refuse, ferrite aussi.
    refused(lambda: es.indices.put_index_template(
        name="compat_tpl_bis", index_patterns=["compat_log-*-*"], priority=100),
        contains="same priority")

    # Un template dont le contenu ne s'appliquerait pas est refuse **a la
    # pose**, la ou le client regarde.
    refused(lambda: es.indices.put_index_template(
        name="compat_tpl_ko", index_patterns=["compat_ko-*"],
        template={"settings": {"index.blocks.read_only": True}}),
        contains="ne supporte pas le reglage d'index")
    refused(lambda: es.indices.put_index_template(
        name="compat_tpl_ko", index_patterns=["compat_ko-*"],
        template={"aliases": {"a": {"filter": {"term": {"x": "y"}}}}}),
        contains="[filter] sur un alias")

    # Un nom litteral absent est un 404 qui le nomme.
    refused(lambda: es.indices.get_index_template(name="compat_tpl_absent"),
            status=404, contains="not found")

    assert es.indices.delete_index_template(name="compat_tpl")["acknowledged"]
    refused(lambda: es.indices.delete_index_template(name="compat_tpl"),
            status=404, contains="missing")
    es.indices.delete(index=JOURNAUX)


@scenario
def template_ancien(es):
    """`_template` : la forme depreciee, celle des scripts d'init venus de 7.x."""
    es.options(ignore_status=404).indices.delete_template(name="compat_leg")
    es.options(ignore_status=404).indices.delete(index="compat_leg-1")

    assert es.indices.put_template(
        name="compat_leg",
        index_patterns=["compat_leg-*"],
        order=5,
        version=7,
        settings={"number_of_replicas": 0},
        mappings={"properties": {"a": {"type": "keyword"}}},
        aliases={"compat_leg_alias": {}},
    )["acknowledged"]

    lu = es.indices.get_template(name="compat_leg")["compat_leg"]
    assert lu["order"] == 5
    assert lu["version"] == 7
    assert lu["index_patterns"] == ["compat_leg-*"]
    assert lu["settings"] == {"index": {"number_of_replicas": "0"}}, lu["settings"]
    plat = es.indices.get_template(name="compat_leg", flat_settings=True)["compat_leg"]
    assert plat["settings"] == {"index.number_of_replicas": "0"}, plat["settings"]
    assert es.indices.exists_template(name="compat_leg")
    assert not es.indices.exists_template(name="compat_leg_absent")

    es.index(index="compat_leg-1", document={"a": "x"}, refresh=True)
    vu = es.indices.get(index="compat_leg-1")["compat_leg-1"]
    assert vu["mappings"]["properties"]["a"] == {"type": "keyword"}
    assert "compat_leg_alias" in vu["aliases"]

    # `create=true` refuse d'ecraser.
    refused(lambda: es.indices.put_template(name="compat_leg", create=True,
                                            index_patterns=["compat_leg-*"]),
            contains="already exists")

    assert es.indices.delete_template(name="compat_leg")["acknowledged"]
    refused(lambda: es.indices.delete_template(name="compat_leg"),
            status=404, contains="missing")
    es.indices.delete(index="compat_leg-1")


@scenario
def fields_et_docvalue_fields(es):
    """`fields` / `docvalue_fields` : ce que la reponse transporte, via le client.

    Ce qu'un client officiel doit voir, et qui n'est pas negociable : dans le
    bloc `fields`, **chaque valeur est un tableau**, meme pour un champ
    mono-value, et un champ absent n'a **pas de cle**. Un code qui connait
    cette forme lit `hit["fields"]["titre"][0]` — un scalaire lui casserait le
    typage sans rien dire.
    """
    INDEX = "compat_fields"
    es.options(ignore_status=404).indices.delete(index=INDEX)
    es.indices.create(index=INDEX, mappings={"properties": {
        "titre": {"type": "text", "fields": {"keyword": {"type": "keyword"}}},
        "tag": {"type": "keyword"},
        "n": {"type": "integer"},
        "d": {"type": "date"},
        "court": {"type": "keyword", "ignore_above": 5},
        "lignes": {"type": "nested", "properties": {
            "ref": {"type": "keyword"}, "q": {"type": "integer"}}},
        "jamais": {"type": "keyword"},
    }})
    es.index(index=INDEX, id="1", refresh=True, document={
        "titre": "le grand bleu", "tag": ["zoulou", "alpha", "alpha"],
        "n": [3, 1, 1], "d": "2026-03-15",
        "court": ["ok", "beaucoup trop long"],
        "lignes": [{"ref": "X1", "q": 2}, {"q": 5}]})
    es.index(index=INDEX, id="2", refresh=True, document={"titre": "petit"})

    hits = es.search(index=INDEX, source=False, sort=["tag"],
                     fields=["titre", "tag", "n", "jamais"])["hits"]["hits"]
    par_id = {h["_id"]: h for h in hits}
    # Chaque valeur est un tableau, meme mono-value.
    assert par_id["1"]["fields"]["titre"] == ["le grand bleu"]
    # L'ordre du document et ses doublons sont conserves : `fields` lit le
    # `_source`, pas la colonne.
    assert par_id["1"]["fields"]["tag"] == ["zoulou", "alpha", "alpha"]
    assert par_id["1"]["fields"]["n"] == [3, 1, 1]
    # Un champ jamais rempli n'a pas de cle — ce n'est pas une valeur nulle.
    assert "jamais" not in par_id["1"]["fields"], par_id["1"]["fields"]
    assert set(par_id["2"]["fields"]) == {"titre"}, par_id["2"]["fields"]
    # `_source` a bien ete retire par `_source: false`.
    assert "_source" not in par_id["1"]

    # Un multi-field est adressable, et lit la valeur de son parent.
    h = es.search(index=INDEX, source=False, q=None,
                  query={"ids": {"values": ["1"]}},
                  fields=["titre.keyword"])["hits"]["hits"][0]
    assert h["fields"] == {"titre.keyword": ["le grand bleu"]}

    # Un sous-champ de `nested` se rend **groupe par element**, cle relative a
    # la racine ; un element sans valeur demandee est omis.
    h = es.search(index=INDEX, source=False, query={"ids": {"values": ["1"]}},
                  fields=["lignes.ref", "lignes.q"])["hits"]["hits"][0]
    assert h["fields"] == {"lignes": [{"ref": ["X1"], "q": [2]}, {"q": [5]}]}, \
        h["fields"]

    # Le `format` d'une date remplace celui du mapping.
    h = es.search(index=INDEX, source=False, query={"ids": {"values": ["1"]}},
                  fields=[{"field": "d", "format": "yyyy-MM-dd"}]
                  )["hits"]["hits"][0]
    assert h["fields"] == {"d": ["2026-03-15"]}
    h = es.search(index=INDEX, source=False, query={"ids": {"values": ["1"]}},
                  fields=["d"])["hits"]["hits"][0]
    assert h["fields"] == {"d": ["2026-03-15T00:00:00.000Z"]}

    # Une valeur qu'`ignore_above` a ecartee n'est pas indexee : elle ne sort
    # pas dans `fields`, elle sort dans `ignored_field_values`.
    h = es.search(index=INDEX, source=False, query={"ids": {"values": ["1"]}},
                  fields=["court"])["hits"]["hits"][0]
    assert h["fields"] == {"court": ["ok"]}, h["fields"]
    assert h["ignored_field_values"] == {"court": ["beaucoup trop long"]}, h

    # `docvalue_fields` lit la colonne : triee, et dedoublonnee sur un keyword.
    h = es.search(index=INDEX, source=False, query={"ids": {"values": ["1"]}},
                  docvalue_fields=["tag", "n"])["hits"]["hits"][0]
    assert h["fields"] == {"tag": ["alpha", "zoulou"], "n": [1, 1, 3]}, h["fields"]

    # Le meme champ des deux cotes : c'est `fields` qui rend la valeur.
    h = es.search(index=INDEX, source=False, query={"ids": {"values": ["1"]}},
                  fields=["tag"], docvalue_fields=["tag"])["hits"]["hits"][0]
    assert h["fields"] == {"tag": ["zoulou", "alpha", "alpha"]}, h["fields"]

    # Un `text` n'a pas de colonne — mais le refus est celui de la phase de
    # fetch : sans document ramene, les deux serveurs rendent 200.
    assert es.search(index=INDEX, source=False, size=0,
                     docvalue_fields=["titre"])["hits"]["total"]["value"] == 2
    # Le refus arrive au **format d'ES** : un `search_phase_execution_exception`
    # « all shards failed » dont la `root_cause` porte la vraie phrase.
    err = refused(lambda: es.search(index=INDEX, source=False,
                                    docvalue_fields=["titre"]))
    assert err["type"] == "search_phase_execution_exception", err
    assert "Fielddata is disabled on [titre]" in \
        err["root_cause"][0]["reason"], err
    es.indices.delete(index=INDEX)


@scenario
def stored_fields_change_la_forme(es):
    """`stored_fields` : aucun champ stocke, mais une reponse differente.

    ferrite refuse `store` au mapping, donc aucun champ n'est stocke — et un ES
    dont le mapping ne porte pas `store: true` ne rend rien non plus. Ce qui se
    verifie ici, c'est ce que `stored_fields` change vraiment : il retire
    `_source`, et `_none_` retire aussi `_id`.
    """
    INDEX = "compat_stored"
    es.options(ignore_status=404).indices.delete(index=INDEX)
    es.indices.create(index=INDEX, mappings={
        "properties": {"titre": {"type": "keyword"}}})
    es.index(index=INDEX, id="1", document={"titre": "x"}, refresh=True)

    h = es.search(index=INDEX, stored_fields=["titre"])["hits"]["hits"][0]
    assert "fields" not in h, h
    assert "_source" not in h, h
    assert h["_id"] == "1"

    # `_source` explicite le ramene.
    h = es.search(index=INDEX, stored_fields=["titre"],
                  source=True)["hits"]["hits"][0]
    assert h["_source"] == {"titre": "x"}

    # `_none_` retire aussi `_id`.
    h = es.search(index=INDEX, stored_fields="_none_")["hits"]["hits"][0]
    assert "_id" not in h, h
    assert "_source" not in h, h

    # `_none_` avec `fields` est contradictoire : ES le refuse, ferrite aussi,
    # avec son type d'erreur.
    err = refused(lambda: es.perform_request(
        "POST", f"/{INDEX}/_search",
        headers={"content-type": "application/json"},
        body={"stored_fields": "_none_", "fields": ["titre"]}))
    assert err["type"] == "action_request_validation_exception", err

    # Un champ que le mapping ne stocke pas ne rend rien — c'est ce qui rend
    # l'absence de valeur exacte plutôt qu'approximative : `stored_fields` ne
    # reconstitue pas depuis le `_source`. Ce que `store: true` change est
    # mesuré par le scénario `store_et_stored_fields`.
    h = es.search(index=INDEX, stored_fields=["titre", "*"])["hits"]["hits"][0]
    assert "fields" not in h, h

    # Painless est hors perimetre : `script_fields` non vide est refuse, mais
    # l'objet **vide** ne definit aucun champ et passe, comme chez ES.
    es.perform_request("POST", f"/{INDEX}/_search",
                       headers={"content-type": "application/json"},
                       body={"script_fields": {}, "runtime_mappings": {}})
    refused(lambda: es.perform_request(
        "POST", f"/{INDEX}/_search",
        headers={"content-type": "application/json"},
        body={"script_fields": {"x": {"script": "1"}}}),
        contains="script Painless")
    es.indices.delete(index=INDEX)


# ---------------------------------------------------------------------------
# Modifier ou purger par requete
# ---------------------------------------------------------------------------

PQ = "compat_par_requete"
PQ2 = "compat_par_requete_2"


def remplir_pq(es, index=PQ, base=0, n=6):
    """Six documents d'un index, deux locataires : de quoi purger la moitie."""
    es.options(ignore_status=404).indices.delete(index=index)
    es.indices.create(index=index, mappings={"properties": {
        "tenant": {"type": "keyword"},
        "n": {"type": "integer"},
        "txt": {"type": "text"},
    }})
    es.bulk(operations=[op for i in range(n) for op in (
        {"index": {"_index": index, "_id": str(base + i)}},
        {"tenant": "a" if i % 2 == 0 else "b", "n": base + i,
         "txt": f"document numero {base + i}"})], refresh=True)


@scenario
def purger_par_requete(es):
    """`client.delete_by_query()` : purger les documents d'un locataire.

    Le geste que le client officiel fait sans une ligne de plus, et pour lequel
    il n'existe pas d'alternative cote client : sans lui, purger un lot par
    filtre demande de tout lire puis de tout reecrire.
    """
    remplir_pq(es)
    r = es.delete_by_query(index=PQ, query={"term": {"tenant": "a"}}, refresh=True)
    assert r["total"] == 3 and r["deleted"] == 3, r
    assert r["batches"] == 1 and r["version_conflicts"] == 0, r
    assert r["failures"] == [] and r["timed_out"] is False, r
    assert r["noops"] == 0 and r["retries"] == {"bulk": 0, "search": 0}, r
    # `_delete_by_query` ne rend pas de cle `updated` du tout : la rendre a zero
    # serait deja une divergence de forme.
    assert "updated" not in r, r
    assert sorted(ids(es.search(index=PQ, query={"match_all": {}}))) == ["1", "3", "5"]

    # Zero correspondance : `batches` vaut 0, pas 1 — aucun lot n'a tourne.
    r = es.delete_by_query(index=PQ, query={"term": {"tenant": "zzz"}}, refresh=True)
    assert (r["total"], r["deleted"], r["batches"]) == (0, 0, 0), r

    # `max_docs` : ES prend les premiers **dans l'ordre du document**.
    remplir_pq(es)
    r = es.delete_by_query(index=PQ, query={"match_all": {}}, max_docs=2, refresh=True)
    assert (r["total"], r["deleted"], r["batches"]) == (2, 2, 1), r
    assert sorted(ids(es.search(index=PQ, query={"match_all": {}}, size=10))) == \
        ["2", "3", "4", "5"]

    # `scroll_size` ne change pas le resultat, il change le **decoupage** — et
    # `batches` est le seul endroit ou ca se voit.
    remplir_pq(es)
    r = es.delete_by_query(index=PQ, query={"match_all": {}},
                           scroll_size=2, refresh=True)
    assert (r["total"], r["deleted"], r["batches"]) == (6, 6, 3), r
    assert es.count(index=PQ)["count"] == 0

    # Une expression d'index, comme partout ailleurs.
    remplir_pq(es)
    remplir_pq(es, PQ2, base=100, n=4)
    r = es.delete_by_query(index=f"{PQ}*", query={"term": {"tenant": "a"}},
                           refresh=True)
    assert (r["total"], r["deleted"]) == (5, 5), r
    assert es.count(index=f"{PQ}*")["count"] == 5
    es.indices.delete(index=PQ2)


@scenario
def reindexer_par_requete(es):
    """`client.update_by_query()` : le geste d'apres un changement de mapping.

    Sans script, la route reindexe chaque document depuis son `_source`. Ce que
    le client observe, c'est que la `_version` avance d'un cran par document —
    y compris quand le `_source` ne change pas, puisque ES ne compte un `noop`
    que sur ordre d'un script.
    """
    remplir_pq(es)
    avant = es.get(index=PQ, id="0")
    r = es.update_by_query(index=PQ, query={"term": {"tenant": "a"}}, refresh=True)
    assert (r["total"], r["updated"], r["deleted"]) == (3, 3, 0), r
    assert r["noops"] == 0 and r["version_conflicts"] == 0, r
    apres = es.get(index=PQ, id="0")
    assert apres["_version"] == avant["_version"] + 1, (avant, apres)
    assert apres["_source"] == avant["_source"], apres
    # Un document que la requete ne vise pas ne bouge pas.
    assert es.get(index=PQ, id="1")["_version"] == 1

    # Sans corps du tout, ES reindexe tout l'index — c'est `match_all`.
    r = es.update_by_query(index=PQ, refresh=True)
    assert (r["total"], r["updated"]) == (6, 6), r

    # Le cas qui motive la route : un champ ajoute au mapping apres coup.
    es.indices.put_mapping(index=PQ, properties={"tenant_bis": {"type": "keyword"}})
    es.index(index=PQ, id="7", document={"tenant": "c", "n": 7, "txt": "sept",
                                         "tenant_bis": "c"}, refresh=True)
    r = es.update_by_query(index=PQ, query={"match_all": {}}, refresh=True)
    assert r["updated"] == 7, r
    assert ids(es.search(index=PQ, query={"term": {"tenant_bis": "c"}})) == ["7"]


@scenario
def conflits_par_requete(es):
    """Un document qui bouge entre la recherche et l'ecriture.

    C'est ce que `version_conflicts` compte, et c'est la raison d'etre de
    `conflicts`. Une ecriture **non rafraichie** le provoque a coup sur : la
    recherche voit encore l'ancien document et son `_seq_no`, l'ecriture trouve
    le nouveau.
    """
    remplir_pq(es)
    es.index(index=PQ, id="0", document={"tenant": "a", "n": 99, "txt": "reecrit"})

    # `abort` (le defaut) : 409, et le detail du conflit dans `failures[]`.
    try:
        es.delete_by_query(index=PQ, query={"match_all": {}}, refresh=True)
        raise AssertionError("un conflit non traite aurait du rendre 409")
    except ApiError as exc:
        assert exc.meta.status == 409, exc.meta.status
        corps = exc.body
        assert corps["version_conflicts"] == 1, corps
        assert corps["total"] == 6 and corps["deleted"] == 5, corps
        echec = corps["failures"][0]
        assert echec["index"] == PQ and echec["id"] == "0", echec
        assert echec["status"] == 409, echec
        assert echec["cause"]["type"] == "version_conflict_engine_exception", echec
        # Le message dit **ce qui** a change : le document a bouge, il n'a pas
        # disparu. ES a deux phrases pour ces deux cas, et la difference est
        # exactement ce qu'un exploitant cherche a savoir.
        assert "current document has seqNo" in echec["cause"]["reason"], echec

    # `proceed` : 200, `failures[]` vide, seul le compteur bouge.
    remplir_pq(es)
    es.index(index=PQ, id="0", document={"tenant": "a", "n": 99, "txt": "reecrit"})
    r = es.delete_by_query(index=PQ, query={"match_all": {}},
                           conflicts="proceed", refresh=True)
    assert r["version_conflicts"] == 1 and r["failures"] == [], r
    assert (r["total"], r["deleted"]) == (6, 5), r
    assert ids(es.search(index=PQ, query={"match_all": {}})) == ["0"]


@scenario
def refus_par_requete(es):
    """Ce que ferrite refuse sur ces deux routes, et le dit.

    Chaque refus est une chose qu'ES sait faire : c'est un cout de perimetre,
    donc il se nomme. Le pire serait de les accepter en silence — un `slices=5`
    ignore rendrait la meme reponse en ayant travaille autrement, et un
    `conflicts: "proceed"` avale ferait echouer une purge que le client voulait
    voir continuer.
    """
    remplir_pq(es)
    # Une purge par distraction n'arrive pas : sans `query`, c'est 400. ES
    # refuse aussi, avec ce type d'erreur.
    err = refused(lambda: es.perform_request(
        "POST", f"/{PQ}/_delete_by_query",
        headers={"content-type": "application/json"}, body={}),
        contains="query is missing")
    assert err["type"] == "action_request_validation_exception", err

    refused(lambda: es.update_by_query(
        index=PQ, query={"match_all": {}},
        script={"source": "ctx._source.n++"}), contains="[script]")
    refused(lambda: es.delete_by_query(
        index=PQ, query={"match_all": {}}, slices=5), contains="[slices=5]")
    refused(lambda: es.delete_by_query(
        index=PQ, query={"match_all": {}}, requests_per_second=10),
        contains="[requests_per_second=10]")
    refused(lambda: es.delete_by_query(
        index=PQ, query={"match_all": {}}, wait_for_completion=False),
        contains="[wait_for_completion=false]")
    refused(lambda: es.delete_by_query(
        index=PQ, query={"match_all": {}}, terminate_after=2),
        contains="[terminate_after]")
    refused(lambda: es.update_by_query(
        index=PQ, query={"match_all": {}}, pipeline="p"), contains="[pipeline]")
    # `slice` dans le corps est refuse par son nom — c'est la ou le client
    # officiel le met, donc c'est la qu'il faut le reconnaitre.
    refused(lambda: es.delete_by_query(
        index=PQ, query={"match_all": {}}, slice={"id": 0, "max": 2}),
        contains="[slice]")

    # Les valeurs par defaut d'ES ecrites explicitement ne demandent rien : les
    # refuser ferait echouer un client qui se contente de les poser.
    r = es.delete_by_query(index=PQ, query={"term": {"tenant": "zzz"}},
                           slices=1, requests_per_second=-1,
                           wait_for_completion=True)
    assert r["total"] == 0, r

    # Les bornes des parametres, avec les messages d'ES.
    refused(lambda: es.delete_by_query(index=PQ, query={"match_all": {}},
                                       conflicts="zzz"),
            contains='conflicts may only be "proceed" or "abort"')
    refused(lambda: es.delete_by_query(index=PQ, query={"match_all": {}},
                                       max_docs=0),
            contains="[max_docs] should be >= [slices]")
    refused(lambda: es.delete_by_query(index=PQ, query={"match_all": {}},
                                       scroll_size=0),
            contains="cannot be [0] in a scroll context")
    refused(lambda: es.perform_request(
        "POST", f"/{PQ}/_delete_by_query?refresh=wait_for",
        headers={"content-type": "application/json"},
        body={"query": {"match_all": {}}}),
        contains="as only [true] or [false] are allowed")

    # Une clause inconnue reste une clause inconnue, meme quand la commande ne
    # vise aucun index : ES la refuse aussi sur un motif sans correspondance.
    refused(lambda: es.delete_by_query(index="compat_rien_du_tout-*",
                                       query={"pas_une_clause": {}}),
            contains="unknown query")
    refused(lambda: es.delete_by_query(index="compat_rien_du_tout",
                                       query={"match_all": {}}),
            status=404, contains="no such index")

    # `_reindex` reste hors perimetre, et le dit par son nom.
    refused(lambda: es.perform_request(
        "POST", "/_reindex", headers={"content-type": "application/json"},
        body={"source": {"index": PQ}, "dest": {"index": PQ2}}),
        contains="/_reindex")
    es.indices.delete(index=PQ)


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
    for index in (INDEX, SCROLL_INDEX, PQ, PQ2):
        es.options(ignore_status=404).indices.delete(index=index)

    print()
    total = len(RESULTS)
    print(f"{total - len(failures)}/{total} scenarios de compatibilite passes")
    if failures:
        print("echecs : " + ", ".join(failures))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
