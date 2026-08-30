#  Le cycle de vie d'un client officiel, exerce par `elasticsearch-py` 8.x.
#
#  Lance dans un conteneur par `tests/compat/tests_clients.py`, contre l'URL
#  passee en argument. N'importe **que** le client officiel installe depuis
#  PyPI : le code ci-dessous est ecrit ici, la bibliotheque qu'il exerce ne
#  l'est pas.
#
#  Chaque cas imprime une ligne `CAS <nom> <PASS|FAIL> <detail>` — un format
#  volontairement pauvre, pour que les trois langages rendent la meme chose.
import sys
import traceback

from elasticsearch import (
    ApiError,
    BadRequestError,
    ConflictError,
    Elasticsearch,
    NotFoundError,
    helpers,
)

URL = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
INDEX = "cycle-python"

CAS = []


def cas(nom):
    def decore(f):
        CAS.append((nom, f))
        return f

    return decore


# ---------------------------------------------------------------------------
# 1. Ce que le client fait avant qu'on lui demande quoi que ce soit
# ---------------------------------------------------------------------------


@cas("decouverte_version")
def decouverte_version(es):
    """Le premier appel de tout client : la version, le nom du cluster, la
    signature. Un numero que le client juge incompatible, et il refuse de
    parler au serveur — avant meme la premiere recherche."""
    info = es.info()
    numero = info["version"]["number"]
    majeur = int(numero.split(".")[0])
    assert majeur == 8, f"version majeure {majeur}, le client 8.x exige 8"
    assert info["tagline"] == "You Know, for Search", info["tagline"]
    assert info["cluster_name"], "cluster_name vide"
    assert info["version"]["build_flavor"], "build_flavor absent"
    return numero


@cas("entete_produit")
def entete_produit(es):
    """`X-elastic-product: Elasticsearch`. Sans lui, le client 8.x leve
    `UnsupportedProductError` et **aucun** appel ne passe : c'est la premiere
    chose qui separe « un serveur qui repond du JSON » d'« un Elasticsearch ».
    Il est verifie sur trois formes de reponse, pas seulement sur la racine."""
    vus = {}
    vus["/"] = es.info().meta.headers.get("x-elastic-product")
    vus["_search"] = es.search(index="*", size=0).meta.headers.get("x-elastic-product")
    vus["_bulk"] = es.bulk(
        operations=[{"index": {"_index": INDEX, "_id": "entete"}}, {"n": 1}],
        refresh=True,
    ).meta.headers.get("x-elastic-product")
    manquants = [route for route, v in vus.items() if v != "Elasticsearch"]
    assert not manquants, f"en-tete absent ou faux sur {manquants} ({vus})"
    return "Elasticsearch sur " + ", ".join(vus)


@cas("negociation_compression")
def negociation_compression(es_url):
    """`http_compress=True` : le client gzippe **le corps de sa requete** et
    pose `Content-Encoding: gzip`. Un serveur qui ne sait pas le lire rend un
    400 sur un JSON qu'il juge invalide — donc un echec qui ne nomme pas sa
    cause. Le lot est volontairement gros : c'est la ou un client active la
    compression."""
    with Elasticsearch(es_url, http_compress=True) as gz:
        docs = [{"_index": INDEX, "_id": f"gz{i}", "texte": f"document {i} " * 40} for i in range(200)]
        ok, echecs = helpers.bulk(gz, docs, refresh=True)
        assert ok == 200 and not echecs, f"{ok} indexes, {echecs}"
        trouves = gz.search(index=INDEX, query={"match": {"texte": "document"}}, size=0)
        assert trouves["hits"]["total"]["value"] >= 200, trouves["hits"]["total"]
    return "200 documents envoyes en gzip, relus"


@cas("sniffing")
def sniffing(es_url):
    """Le sniffing demande la liste des noeuds (`GET /_nodes/_all/http`) et
    remplace le pool de connexions par ce qu'elle rend. ferrite est mono-noeud :
    le tenir ou le refuser sont deux reponses acceptables — **se taire** n'en
    est pas une. Le cas exige donc l'un des deux, nomme."""
    try:
        with Elasticsearch(es_url, sniff_on_start=True, min_delay_between_sniffing=0) as sniff:
            info = sniff.info()
            assert info["version"]["number"], info
        return "sniffing tenu, le client a garde la main"
    except Exception as e:  # noqa: BLE001 — c'est le refus qu'on mesure
        message = str(e)
        assert message.strip(), f"{type(e).__name__} sans message : un refus muet"
        assert "no handler" in message or "ferrite ne supporte pas" in message or "sniff" in message.lower(), (
            f"refus illisible : {type(e).__name__}: {message[:200]}"
        )
        return f"refuse proprement ({type(e).__name__}: {message[:120]})"


# ---------------------------------------------------------------------------
# 2. Ce que le client fait des erreurs
# ---------------------------------------------------------------------------


@cas("erreurs_typees")
def erreurs_typees(es):
    """Un client officiel ne rend pas un code de statut : il leve une classe.
    Le mapping statut -> classe se fait sur le **corps** de l'erreur, donc il
    exige le format d'ES — `error.type`, `error.reason`, `status`."""
    vus = []

    try:
        es.get(index=INDEX, id="jamais-ecrit")
        raise AssertionError("un document absent aurait du lever NotFoundError")
    except NotFoundError as e:
        assert e.status_code == 404, e.status_code
        # Un document absent n'est pas une erreur d'API : ES rend le meme corps
        # qu'un document trouve, `found: false`, sous un 404.
        assert e.body.get("found") is False, e.body
        assert e.body["_id"] == "jamais-ecrit", e.body
        vus.append("NotFoundError(404, found:false)")

    try:
        es.search(index=INDEX, query={"pas_une_clause": {}})
        raise AssertionError("une clause inconnue aurait du lever BadRequestError")
    except BadRequestError as e:
        assert e.status_code == 400, e.status_code
        assert e.body["error"]["type"], e.body
        assert e.body["error"]["reason"], e.body
        vus.append(f"BadRequestError(400, {e.body['error']['type']})")

    es.index(index=INDEX, id="conflit", document={"n": 1}, refresh=True)
    try:
        es.index(index=INDEX, id="conflit", document={"n": 2}, if_seq_no=99999, if_primary_term=1)
        raise AssertionError("un `if_seq_no` perime aurait du lever ConflictError")
    except ConflictError as e:
        assert e.status_code == 409, e.status_code
        assert e.body["error"]["type"] == "version_conflict_engine_exception", e.body
        vus.append("ConflictError(409)")

    try:
        es.indices.create(index=INDEX)
        raise AssertionError("un index deja la aurait du lever une ApiError")
    except ApiError as e:
        assert e.status_code == 400, e.status_code
        assert e.body["error"]["type"] == "resource_already_exists_exception", e.body
        vus.append("ApiError(400, resource_already_exists_exception)")

    return ", ".join(vus)


# ---------------------------------------------------------------------------
# 3. Les helpers — le code que personne ne reecrit
# ---------------------------------------------------------------------------


@cas("helpers_bulk")
def helpers_bulk(es):
    """`helpers.bulk` decoupe, envoie, et **leve** sur le premier document
    refuse. Les deux moities comptent : un helper qui avale un rejet rendrait
    un index incomplet en silence."""
    docs = [{"_index": INDEX, "_id": f"b{i}", "rang": i} for i in range(1500)]
    ok, echecs = helpers.bulk(es, docs, chunk_size=250, refresh=True)
    assert ok == 1500, ok
    assert not echecs, echecs
    total = es.count(index=INDEX, query={"range": {"rang": {"gte": 0}}})["count"]
    assert total == 1500, total

    mauvais = [{"_index": INDEX, "_id": "b0", "_op_type": "create", "rang": 0}]
    try:
        helpers.bulk(es, mauvais)
        raise AssertionError("un `create` sur un id existant aurait du lever")
    except helpers.BulkIndexError as e:
        assert e.errors, e
    return "1500 documents en 6 lots, et le rejet leve"


@cas("helpers_streaming_bulk")
def helpers_streaming_bulk(es):
    """`streaming_bulk` rend un resultat **par document**, dans l'ordre — c'est
    ce qui permet a un appelant de rattacher un echec a sa ligne."""
    def source():
        for i in range(600):
            yield {"_index": INDEX, "_id": f"s{i}", "rang": i, "flux": True}

    rendus = []
    for succes, detail in helpers.streaming_bulk(es, source(), chunk_size=100, refresh=True):
        assert succes, detail
        rendus.append(next(iter(detail.values()))["_id"])
    assert len(rendus) == 600, len(rendus)
    assert rendus == [f"s{i}" for i in range(600)], "l'ordre des resultats n'est pas celui des documents"
    return "600 resultats, un par document, dans l'ordre"


@cas("helpers_parallel_bulk")
def helpers_parallel_bulk(es):
    """Le meme, sur quatre fils : ce que fait un import un peu presse."""
    docs = ({"_index": INDEX, "_id": f"p{i}", "rang": i} for i in range(800))
    combien = 0
    for succes, detail in helpers.parallel_bulk(es, docs, thread_count=4, chunk_size=100):
        assert succes, detail
        combien += 1
    assert combien == 800, combien
    return "800 documents par 4 fils"


@cas("helpers_scan")
def helpers_scan(es):
    """`helpers.scan` est le code de tout export : il ouvre un `scroll`, deroule
    toutes les pages et ferme le contexte. Chaque document doit sortir **une
    fois et une seule**."""
    es.indices.refresh(index=INDEX)
    vus = [d["_id"] for d in helpers.scan(es, index=INDEX, query={"query": {"term": {"flux": True}}}, size=97)]
    assert len(vus) == 600, len(vus)
    assert len(set(vus)) == 600, "des documents sortent deux fois"

    tries = [
        d["_source"]["rang"]
        for d in helpers.scan(
            es,
            index=INDEX,
            query={"query": {"match_all": {}}, "sort": ["rang"]},
            preserve_order=True,
            size=50,
        )
        if "rang" in d["_source"]
    ]
    assert tries == sorted(tries), "preserve_order ne preserve pas l'ordre"
    return f"600 documents deroules sans doublon, {len(tries)} en ordre conserve"


# ---------------------------------------------------------------------------


def main():
    with Elasticsearch(URL) as es:
        if es.indices.exists(index=INDEX):
            es.indices.delete(index=INDEX)
        es.indices.create(
            index=INDEX,
            mappings={
                "properties": {
                    "rang": {"type": "integer"},
                    "flux": {"type": "boolean"},
                    "texte": {"type": "text"},
                    "n": {"type": "integer"},
                }
            },
        )
        rate = 0
        for nom, f in CAS:
            try:
                # Les cas qui construisent leur propre client prennent l'URL,
                # les autres celui qui est deja ouvert.
                detail = f(URL if f.__code__.co_varnames[0] == "es_url" else es)
                print(f"CAS {nom} PASS {detail}")
            except Exception:  # noqa: BLE001 — un cas rouge n'arrete pas la batterie
                trace = traceback.format_exc().strip().splitlines()
                print(f"CAS {nom} FAIL {trace[-1]}")
                for ligne in trace:
                    print(f"    | {ligne}")
                rate += 1
        es.options(ignore_status=404).indices.delete(index=INDEX)
    print(f"CYCLE python {len(CAS) - rate}/{len(CAS)}")
    return 1 if rate else 0


if __name__ == "__main__":
    sys.exit(main())
