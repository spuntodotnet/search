# Que se passe-t-il si le client officiel **7.x** parle a ferrite ?
#
#   python3 tests/compat/probe_es7.py [URL]        # defaut : http://localhost:9200
#
# ferrite annonce l'API 8.15.0 et la suite de compat est ecrite avec le client
# 8.x. Ce script pose l'autre question, celle d'un projet reste en 7.10.2 :
# quelles lignes de code client, ecrites pour un ES 7, marchent telles quelles
# et lesquelles cassent.
#
# Il n'affirme rien : chaque scenario est un vrai appel du client 7.x, et le
# script imprime ce qui a ete constate. Le pendant `diff_es7.py` fait tourner
# le meme fichier contre un vrai Elasticsearch 7.10.2 pour distinguer « ferrite
# diverge » de « ES 7 fait pareil ».
#
# Le client 7.10.2 n'est pas sur PyPI ; le 7.10.1 est le meme code de transport
# (pip install "elasticsearch==7.10.1").
import json
import sys

from elasticsearch import Elasticsearch

URL = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
INDEX = "probe7"

RESULTS = []
SCENARIOS = []


def scenario(titre, *, ecrit_pour="7.x"):
    """Enregistre un scenario. `ecrit_pour` documente d'ou vient ce code."""

    def deco(fn):
        SCENARIOS.append((titre, ecrit_pour, fn))
        return fn

    return deco


def resume(exc):
    """Une exception du client 7.x, reduite a ce qui aide a diagnostiquer."""
    status = getattr(exc, "status_code", None)
    info = getattr(exc, "info", None)
    detail = None
    if isinstance(info, dict):
        err = info.get("error")
        if isinstance(err, dict):
            detail = err.get("reason") or err.get("type")
        elif isinstance(err, str):
            detail = err
    return f"{type(exc).__name__}({status}) {detail or exc}"[:300]


# ---------------------------------------------------------------------------
# Poignee de main
# ---------------------------------------------------------------------------


@scenario("ping() / info()")
def poignee_de_main(es):
    assert es.ping(), "ping() renvoie False"
    info = es.info()
    return {
        "version": info["version"]["number"],
        "tagline": info["tagline"],
    }


@scenario("le client 7.x accepte un serveur qui annonce 8.x")
def negociation_de_version(es):
    # Le garde-fou produit (`X-elastic-product`) n'existe qu'a partir du client
    # 7.14 ; en 7.10 aucune verification n'a lieu. On note ce que le serveur
    # renvoie quand meme, puisque c'est ce qui rendra le passage en 7.14+
    # indolore.
    import urllib.request

    with urllib.request.urlopen(URL + "/") as resp:
        entete = resp.headers.get("X-elastic-product")
    return {"version annoncee": es.info()["version"]["number"], "x-elastic-product": entete}


# ---------------------------------------------------------------------------
# Index et mapping, ecrits comme en 7.x (tout passe par `body=`)
# ---------------------------------------------------------------------------

MAPPING = {
    "properties": {
        "titre": {"type": "text"},
        "auteur": {"type": "keyword"},
        "annee": {"type": "integer"},
    }
}


@scenario("indices.create(body={'mappings': ...})")
def creation_body(es):
    es.indices.delete(index=INDEX, ignore=[404])
    es.indices.create(index=INDEX, body={"mappings": MAPPING})
    assert es.indices.exists(index=INDEX)
    return "index cree"


@scenario("indices.create(..., include_type_name=True)")
def creation_include_type_name(es):
    # Le style 6.x encore tres present dans du code 7.x : le mapping est niche
    # sous un nom de type.
    es.indices.delete(index="probe7_itn", ignore=[404])
    es.indices.create(
        index="probe7_itn",
        include_type_name=True,
        body={"mappings": {"_doc": MAPPING}},
    )
    return "accepte"


@scenario("indices.put_mapping(body=..., include_type_name=False)")
def put_mapping(es):
    es.indices.put_mapping(
        index=INDEX,
        body={"properties": {"resume": {"type": "text"}}},
        include_type_name=False,
    )
    return "accepte"


@scenario("indices.get_mapping() — forme de la reponse")
def get_mapping(es):
    m = es.indices.get_mapping(index=INDEX)
    return {"cles": sorted(m[INDEX]["mappings"].keys())}


# ---------------------------------------------------------------------------
# Ingestion
# ---------------------------------------------------------------------------


@scenario("index(body=...) sans doc_type")
def index_sans_type(es):
    r = es.index(
        index=INDEX,
        id="1",
        body={"titre": "Le Horla", "auteur": "Maupassant", "annee": 1887},
        refresh=True,
    )
    return {"result": r.get("result"), "_type": r.get("_type", "<absent>")}


@scenario("index(doc_type='_doc', body=...)")
def index_type_doc(es):
    r = es.index(
        index=INDEX,
        doc_type="_doc",
        id="2",
        body={"titre": "Bel-Ami", "auteur": "Maupassant", "annee": 1885},
        refresh=True,
    )
    return {"result": r.get("result"), "_type": r.get("_type", "<absent>")}


@scenario("index(doc_type='livre', ...) — type nomme, style 6.x")
def index_type_nomme(es):
    r = es.index(
        index="probe7_typed",
        doc_type="livre",
        id="1",
        body={"titre": "Germinal"},
        refresh=True,
    )
    return {"result": r.get("result")}


@scenario("bulk(body=[...]) avec _type dans l'action")
def bulk_avec_type(es):
    r = es.bulk(
        body=[
            {"index": {"_index": INDEX, "_type": "_doc", "_id": "3"}},
            {"titre": "Germinal", "auteur": "Zola", "annee": 1885},
        ],
        refresh=True,
    )
    return {"errors": r["errors"], "item": json.dumps(r["items"][0])[:200]}


@scenario("bulk(body=[...]) sans _type")
def bulk_sans_type(es):
    r = es.bulk(
        body=[
            {"index": {"_index": INDEX, "_id": "4"}},
            {"titre": "Nana", "auteur": "Zola", "annee": 1880},
        ],
        refresh=True,
    )
    return {"errors": r["errors"], "_type": r["items"][0]["index"].get("_type", "<absent>")}


@scenario("get(index, id) — presence de _type")
def get_doc(es):
    r = es.get(index=INDEX, id="1")
    return {"_type": r.get("_type", "<absent>"), "titre": r["_source"]["titre"]}


@scenario("get(index, doc_type='_doc', id)")
def get_doc_type(es):
    r = es.get(index=INDEX, doc_type="_doc", id="1")
    return {"_type": r.get("_type", "<absent>")}


@scenario("update(body={'doc': ...})")
def update_doc(es):
    r = es.update(index=INDEX, id="1", body={"doc": {"annee": 1888}}, refresh=True)
    return {"result": r.get("result")}


@scenario("delete(index, id)")
def delete_doc(es):
    es.index(index=INDEX, id="99", body={"titre": "a jeter"}, refresh=True)
    r = es.delete(index=INDEX, id="99", refresh=True)
    return {"result": r.get("result")}


@scenario("mget(body={'docs': [...]}) ")
def mget(es):
    r = es.mget(body={"docs": [{"_index": INDEX, "_id": "1"}, {"_index": INDEX, "_id": "2"}]})
    return {"trouves": [d["found"] for d in r["docs"]]}


# ---------------------------------------------------------------------------
# Recherche
# ---------------------------------------------------------------------------


@scenario("search(body={'query': ...})")
def search_body(es):
    es.index(
        index=INDEX,
        id="3",
        body={"titre": "Germinal", "auteur": "Zola", "annee": 1885},
        refresh=True,
    )
    r = es.search(index=INDEX, body={"query": {"match": {"titre": "germinal"}}})
    return {
        "total": r["hits"]["total"],
        "_type du hit": r["hits"]["hits"][0].get("_type", "<absent>"),
    }


@scenario("search(..., rest_total_hits_as_int=True) — le compat 6.x")
def search_total_int(es):
    r = es.search(
        index=INDEX,
        body={"query": {"match_all": {}}},
        rest_total_hits_as_int=True,
    )
    return {"total": r["hits"]["total"]}


@scenario("search(doc_type='_doc', ...) — /index/_doc/_search")
def search_typed(es):
    # Le piege : cette URL ressemble a s'y meprendre a l'indexation d'un
    # document dont l'`_id` serait `_search`. On verifie donc aussi qu'aucun
    # document fantome n'est apparu — un echec silencieux serait pire qu'une
    # erreur.
    r = es.search(index=INDEX, doc_type="_doc", body={"query": {"match_all": {}}})
    es.indices.refresh(index=INDEX)
    fantome = es.exists(index=INDEX, id="_search")
    if fantome:
        raise AssertionError(
            "la requete a ete INDEXEE comme document _id=_search "
            f"(reponse : {json.dumps(r)[:160]})"
        )
    return {"total": r["hits"]["total"]}


@scenario("count(doc_type='_doc') — /index/_doc/_count")
def count_typed(es):
    r = es.count(index=INDEX, doc_type="_doc", body={"query": {"match_all": {}}})
    es.indices.refresh(index=INDEX)
    if es.exists(index=INDEX, id="_count"):
        raise AssertionError("la requete a ete INDEXEE comme document _id=_count")
    return {"count": r["count"]}


@scenario("search bool + filter + range + sort")
def search_bool(es):
    r = es.search(
        index=INDEX,
        body={
            "query": {
                "bool": {
                    "must": [{"match": {"titre": "bel ami"}}],
                    "filter": [
                        {"term": {"auteur": "Maupassant"}},
                        {"range": {"annee": {"gte": 1880, "lt": 1886}}},
                    ],
                }
            },
            "sort": [{"annee": "desc"}],
            "_source": ["titre"],
        },
    )
    return {"ids": [h["_id"] for h in r["hits"]["hits"]]}


@scenario("search(..., track_total_hits=True)")
def search_track_total(es):
    r = es.search(
        index=INDEX, body={"query": {"match_all": {}}, "track_total_hits": True}
    )
    return {"total": r["hits"]["total"]}


@scenario("aggregations (body 'aggs')")
def aggs(es):
    r = es.search(
        index=INDEX,
        body={"size": 0, "aggs": {"par_auteur": {"terms": {"field": "auteur"}}}},
    )
    return {"buckets": r["aggregations"]["par_auteur"]["buckets"]}


@scenario("count(body={'query': ...})")
def count(es):
    r = es.count(index=INDEX, body={"query": {"match_all": {}}})
    return {"count": r["count"]}


@scenario("search(scroll='1m') puis scroll() — la pagination 7.x classique")
def scroll(es):
    r = es.search(index=INDEX, body={"query": {"match_all": {}}, "size": 2}, scroll="1m")
    sid = r.get("_scroll_id")
    suite = es.scroll(scroll_id=sid, scroll="1m")
    es.clear_scroll(scroll_id=sid)
    return {"scroll_id": bool(sid), "page2": len(suite["hits"]["hits"])}


@scenario("helpers.scan() — le raccourci le plus utilise en 7.x")
def helpers_scan(es):
    from elasticsearch import helpers

    docs = list(helpers.scan(es, index=INDEX, query={"query": {"match_all": {}}}))
    return {"docs": len(docs)}


@scenario("helpers.bulk() — l'autre helper universel")
def helpers_bulk(es):
    from elasticsearch import helpers

    ok, erreurs = helpers.bulk(
        es,
        [
            {"_index": INDEX, "_id": "10", "_source": {"titre": "Pot-Bouille", "auteur": "Zola"}},
            {"_index": INDEX, "_id": "11", "_source": {"titre": "L Assommoir", "auteur": "Zola"}},
        ],
        refresh=True,
    )
    return {"indexes": ok, "erreurs": erreurs}


@scenario("msearch(body=[...])")
def msearch(es):
    r = es.msearch(body=[{"index": INDEX}, {"query": {"match_all": {}}}])
    return {"reponses": len(r["responses"])}


# ---------------------------------------------------------------------------
# Cluster / admin, tel qu'un code 7.x l'appelle
# ---------------------------------------------------------------------------


@scenario("cluster.health()")
def cluster_health(es):
    r = es.cluster.health()
    return {"status": r["status"], "cles": sorted(r.keys())[:6]}


@scenario("cat.indices(format='json')")
def cat_indices(es):
    r = es.cat.indices(format="json")
    return {"indices": [i["index"] for i in r]}


@scenario("indices.refresh() / indices.stats()")
def refresh_stats(es):
    es.indices.refresh(index=INDEX)
    r = es.indices.stats(index=INDEX)
    return {"docs": r["_all"]["primaries"]["docs"]["count"]}


@scenario("indices.delete(index)")
def delete_index(es):
    for i in (INDEX, "probe7_itn", "probe7_typed"):
        es.indices.delete(index=i, ignore=[404])
    return "nettoye"


def main():
    es = Elasticsearch(URL)
    largeur = max(len(t) for t, _, _ in SCENARIOS)
    ok = ko = 0
    for titre, _, fn in SCENARIOS:
        try:
            detail = fn(es)
            ok += 1
            etat = "OK  "
        except Exception as exc:  # noqa: BLE001 — c'est le sujet du script
            detail = resume(exc)
            ko += 1
            etat = "KO  "
        RESULTS.append({"scenario": titre, "etat": etat.strip(), "detail": detail})
        print(f"{etat}{titre.ljust(largeur)}  {detail}")
    print(f"\n{ok} OK, {ko} KO sur {len(SCENARIOS)} scenarios — cible {URL}")
    if "--json" in sys.argv:
        with open(sys.argv[sys.argv.index("--json") + 1], "w") as f:
            json.dump(RESULTS, f, indent=2, default=str, ensure_ascii=False)


if __name__ == "__main__":
    main()
