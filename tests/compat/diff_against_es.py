#!/usr/bin/env python3
"""Compare champ par champ les reponses de ferrite et d'un vrai Elasticsearch.

Outil de developpement, pas de CI : il exige un Elasticsearch 8.x qui tourne a
cote (voir docs/dev-workflow.md). Les deux serveurs recoivent exactement la meme
suite d'appels, via le meme client officiel, et chaque reponse est comparee
apres neutralisation des champs volatils (durees, uuid, scores).

    python3 tests/compat/diff_against_es.py [ferrite_url] [es_url]
"""
import sys
import json

from elasticsearch import Elasticsearch

FERRITE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
ES = sys.argv[2] if len(sys.argv) > 2 else "http://localhost:9201"

INDEX = "compat_diff"
MAPPINGS = {
    "properties": {
        "titre": {"type": "text"},
        "auteur": {"type": "keyword"},
        "annee": {"type": "integer"},
        "note": {"type": "double"},
        "dispo": {"type": "boolean"},
        "paru": {"type": "date"},
        "resume": {"type": "text"},
    }
}

DOCS = [
    {"_id": "1", "titre": "Le Horla", "auteur": "Maupassant", "annee": 1887,
     "note": 4.5, "dispo": True, "paru": "1887-05-17",
     "resume": "un homme se croit habite par une presence invisible"},
    {"_id": "2", "titre": "Bel-Ami", "auteur": "Maupassant", "annee": 1885,
     "note": 4.1, "dispo": False, "paru": "1885-04-01",
     "resume": "l ascension sociale d un arriviste dans la presse parisienne"},
    {"_id": "3", "titre": "Germinal", "auteur": "Zola", "annee": 1885,
     "note": 4.8, "dispo": True, "paru": "1885-03-01",
     "resume": "la greve des mineurs du nord de la France"},
]

# Champs dont la valeur ne peut pas coincider (temps, identifiants, scores BM25
# calcules sur des statistiques d'index differentes) : on compare leur presence
# et leur type, pas leur valeur.
VOLATILE = {
    "took", "cluster_uuid", "uuid", "index_uuid", "_score", "max_score",
    "build_hash", "build_date", "cluster_name", "name", "creation_date",
    "store.size", "pri.store.size", "dataset.size", "docs.deleted",
    "_seq_no", "_primary_term", "_shards", "forced_refresh", "version",
    "settings", "epoch", "timestamp", "reason", "root_cause", "type",
    "error", "line", "col", "caused_by", "_nodes", "nodes", "rep",
}


def normalise(value):
    if isinstance(value, dict):
        return {k: ("<volatile>" if k in VOLATILE else normalise(v))
                for k, v in sorted(value.items())}
    if isinstance(value, list):
        return [normalise(v) for v in value]
    if isinstance(value, float):
        return "<float>"
    return value


def diff(path, a, b, out):
    if isinstance(a, dict) and isinstance(b, dict):
        for key in sorted(set(a) | set(b)):
            if key not in a:
                out.append(f"{path}.{key}: absent de ferrite (ES: {json.dumps(b[key])[:80]})")
            elif key not in b:
                out.append(f"{path}.{key}: en trop dans ferrite ({json.dumps(a[key])[:80]})")
            else:
                diff(f"{path}.{key}", a[key], b[key], out)
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            out.append(f"{path}: {len(a)} elements chez ferrite, {len(b)} chez ES")
        for i, (x, y) in enumerate(zip(a, b)):
            diff(f"{path}[{i}]", x, y, out)
    elif a != b:
        out.append(f"{path}: ferrite={json.dumps(a)[:80]} / ES={json.dumps(b)[:80]}")


class Runner:
    def __init__(self):
        self.ferrite = Elasticsearch(FERRITE, request_timeout=30)
        self.es = Elasticsearch(ES, request_timeout=30)
        self.failures = 0
        self.checks = 0

    def both(self, label, fn):
        """Appelle `fn(client)` sur les deux serveurs et compare."""
        results = []
        for client in (self.ferrite, self.es):
            try:
                results.append(("ok", dict(fn(client))))
            except Exception as exc:  # noqa: BLE001 - on compare aussi les erreurs
                status = getattr(getattr(exc, "meta", None), "status", None)
                results.append(("err", {"status": status,
                                        "body": getattr(exc, "body", str(exc))}))
        (kind_f, body_f), (kind_e, body_e) = results
        out = []
        if kind_f != kind_e:
            out.append(f"ferrite {kind_f} / ES {kind_e} :: "
                       f"ferrite={json.dumps(body_f, default=str)[:200]} :: "
                       f"ES={json.dumps(body_e, default=str)[:200]}")
        elif kind_f == "err":
            if body_f["status"] != body_e["status"]:
                out.append(f"status ferrite={body_f['status']} / ES={body_e['status']}")
        else:
            diff("", normalise(body_f), normalise(body_e), out)

        self.checks += 1
        if out:
            self.failures += 1
            print(f"[ecart] {label}")
            for line in out:
                print(f"        {line}")
        else:
            print(f"[  ok  ] {label}")

    def run(self):
        for client in (self.ferrite, self.es):
            client.options(ignore_status=404).indices.delete(index=INDEX)

        self.both("indices.create", lambda c: c.indices.create(
            index=INDEX, mappings=MAPPINGS,
            settings={"number_of_shards": 1, "number_of_replicas": 0}))
        self.both("indices.get_mapping", lambda c: c.indices.get_mapping(index=INDEX))
        self.both("indices.exists", lambda c: {"exists": bool(c.indices.exists(index=INDEX))})

        ops = []
        for doc in DOCS:
            body = {k: v for k, v in doc.items() if k != "_id"}
            ops.append({"index": {"_index": INDEX, "_id": doc["_id"]}})
            ops.append(body)
        self.both("bulk (index x3, refresh)", lambda c: c.bulk(operations=ops, refresh=True))

        self.both("search match", lambda c: c.search(
            index=INDEX, query={"match": {"resume": "presse"}}))
        self.both("search match_all", lambda c: c.search(
            index=INDEX, query={"match_all": {}}))
        self.both("search term keyword", lambda c: c.search(
            index=INDEX, query={"term": {"auteur": "Maupassant"}}))
        self.both("search terms", lambda c: c.search(
            index=INDEX, query={"terms": {"auteur": ["Zola", "Hugo"]}}))
        self.both("search range int", lambda c: c.search(
            index=INDEX, query={"range": {"annee": {"gte": 1880, "lt": 1886}}}))
        self.both("search range date", lambda c: c.search(
            index=INDEX, query={"range": {"paru": {"gte": "1885-04-01"}}}))
        self.both("search range double", lambda c: c.search(
            index=INDEX, query={"range": {"note": {"gt": 4.2}}}))
        self.both("search term bool", lambda c: c.search(
            index=INDEX, query={"term": {"dispo": True}}))
        self.both("search bool must+filter", lambda c: c.search(
            index=INDEX, query={"bool": {
                "must": [{"match": {"titre": "bel ami"}}],
                "filter": [{"term": {"auteur": "Maupassant"}},
                           {"range": {"annee": {"gte": 1880, "lt": 1886}}}]}}))
        self.both("search bool should", lambda c: c.search(
            index=INDEX, query={"bool": {"should": [
                {"term": {"auteur": "Zola"}}, {"term": {"auteur": "Hugo"}}]}}))
        self.both("search bool must_not", lambda c: c.search(
            index=INDEX, query={"bool": {"must_not": [{"term": {"auteur": "Zola"}}]}}))
        self.both("search bool minimum_should_match", lambda c: c.search(
            index=INDEX, query={"bool": {
                "should": [{"term": {"auteur": "Zola"}},
                           {"range": {"annee": {"gte": 1885}}}],
                "minimum_should_match": 2}}))
        self.both("search match operator=and", lambda c: c.search(
            index=INDEX, query={"match": {"resume": {"query": "presse parisienne",
                                                     "operator": "and"}}}))
        self.both("search sort desc + size + includes", lambda c: c.search(
            index=INDEX, query={"match_all": {}}, sort=[{"annee": "desc"}],
            size=2, source_includes=["titre"]))
        self.both("search sort keyword asc", lambda c: c.search(
            index=INDEX, query={"match_all": {}}, sort=[{"auteur": "asc"}]))
        self.both("search sort multi-cles", lambda c: c.search(
            index=INDEX, query={"match_all": {}},
            sort=[{"annee": "asc"}, {"note": "desc"}]))
        self.both("search sort date desc", lambda c: c.search(
            index=INDEX, query={"match_all": {}}, sort=[{"paru": "desc"}]))
        self.both("search from/size", lambda c: c.search(
            index=INDEX, query={"match_all": {}}, sort=[{"annee": "asc"}],
            from_=1, size=1))
        self.both("search _source false", lambda c: c.search(
            index=INDEX, query={"match_all": {}}, source=False, size=1,
            sort=[{"annee": "asc"}]))
        self.both("search _source excludes", lambda c: c.search(
            index=INDEX, query={"match_all": {}}, sort=[{"annee": "asc"}],
            source={"excludes": ["resume", "note"]}))
        self.both("search size=0", lambda c: c.search(
            index=INDEX, query={"match_all": {}}, size=0))

        self.both("get doc", lambda c: c.get(index=INDEX, id="3"))
        self.both("get doc absent", lambda c: c.options(ignore_status=404).get(
            index=INDEX, id="404"))
        self.both("exists doc", lambda c: {"exists": bool(c.exists(index=INDEX, id="1"))})
        self.both("index doc (create)", lambda c: c.index(
            index=INDEX, id="9", document={"titre": "Nana", "auteur": "Zola",
                                           "annee": 1880}, refresh=True))
        self.both("index doc (update)", lambda c: c.index(
            index=INDEX, id="9", document={"titre": "Nana", "auteur": "Zola",
                                           "annee": 1881}, refresh=True))
        self.both("create doc en conflit", lambda c: c.options(
            ignore_status=409).create(
            index=INDEX, id="9", document={"titre": "x", "auteur": "y"}))
        self.both("delete doc", lambda c: c.delete(index=INDEX, id="9", refresh=True))
        self.both("delete doc absent", lambda c: c.options(ignore_status=404).delete(
            index=INDEX, id="9"))
        self.both("refresh", lambda c: c.indices.refresh(index=INDEX))

        self.both("cluster.health", lambda c: c.cluster.health())
        self.both("cat.indices json", lambda c: {"rows": list(
            c.cat.indices(index=INDEX, format="json"))})

        self.both("search index inconnu", lambda c: c.search(
            index="absent_du_cluster", query={"match_all": {}}))
        self.both("create index deja existant", lambda c: c.indices.create(
            index=INDEX, mappings=MAPPINGS))
        self.both("search clause inconnue", lambda c: c.search(
            index=INDEX, query={"clause_inexistante": {"titre": "x"}}))
        self.both("search champ non mappe", lambda c: c.search(
            index=INDEX, query={"term": {"champ_inconnu": "x"}}))

        for client in (self.ferrite, self.es):
            client.options(ignore_status=404).indices.delete(index=INDEX)

        print()
        print(f"{self.checks - self.failures}/{self.checks} appels identiques "
              f"(hors champs volatils)")
        return 1 if self.failures else 0


if __name__ == "__main__":
    sys.exit(Runner().run())
