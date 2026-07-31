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
def mapping_explicite_obligatoire(es):
    es.options(ignore_status=404).indices.delete(index="sans_mapping")
    refused(lambda: es.indices.create(index="sans_mapping"))


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
def multi_fields_refuses(es):
    refused(lambda: es.indices.create(
        index="multi", mappings={"properties": {
            "t": {"type": "text", "fields": {"keyword": {"type": "keyword"}}}}}),
        contains="fields")


@scenario
def modification_de_mapping_refusee(es):
    refused(lambda: es.indices.put_mapping(
        index=INDEX, properties={"nouveau": {"type": "text"}}))


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
def bulk_action_inconnue(es):
    refused(lambda: es.bulk(operations=[
        {"update": {"_index": INDEX, "_id": "1"}},
        {"doc": {"annee": 1900}},
    ]), contains="update")


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
    refused(lambda: es.search(index=INDEX, query={"wildcard": {"auteur": "Mau*"}}),
            contains="wildcard")
    refused(lambda: es.search(index=INDEX, query={"prefix": {"auteur": "Mau"}}),
            contains="prefix")


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
                              aggs={"par_auteur": {"terms": {"field": "auteur"}}}),
            contains="aggs")
    refused(lambda: es.search(index=INDEX, query={"match_all": {}},
                              highlight={"fields": {"resume": {}}}),
            contains="highlight")
    refused(lambda: es.search(index=INDEX, query={"match_all": {}},
                              search_after=[1885], sort=[{"annee": "asc"}]),
            contains="search_after")
    refused(lambda: es.update(index=INDEX, id="1", doc={"annee": 1888}))
    refused(lambda: es.mget(index=INDEX, ids=["1", "2"]))
    refused(lambda: es.search(index=INDEX, q="titre:bel"), contains="q")
    refused(lambda: es.search(query={"match_all": {}}))


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
