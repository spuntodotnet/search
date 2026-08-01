#!/usr/bin/env python3
"""Les agregations de ferrite rendent-elles exactement ce que rend Elasticsearch ?

Meme corpus des deux cotes, meme batterie d'agregations, et comparaison du JSON
**champ par champ** — cles comprises. Une agregation qui rendrait les bons
nombres sous un nom different, ou qui oublierait un compteur qu'ES expose
toujours, casserait un client type sans que rien ne le signale.

Les flottants sont compares a 1e-9 pres ; le reste doit etre identique.

    python3 tests/compat/diff_aggs.py [ferrite_url] [es_url]

Outil de developpement : exige un Elasticsearch 8.x lance a cote (Docker).
"""
import json
import sys

from elasticsearch import ApiError, Elasticsearch

import corpus

FERRITE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
ES = sys.argv[2] if len(sys.argv) > 2 else "http://localhost:9201"
INDEX = "compat_aggs"


def aggregations():
    """La batterie, du plus simple au plus imbrique."""
    return [
        # --- metriques
        ("min", {"m": {"min": {"field": "prix"}}}),
        ("max", {"m": {"max": {"field": "prix"}}}),
        ("sum", {"m": {"sum": {"field": "prix"}}}),
        ("avg", {"m": {"avg": {"field": "prix"}}}),
        ("value_count", {"m": {"value_count": {"field": "prix"}}}),
        ("stats", {"m": {"stats": {"field": "prix"}}}),
        ("avg sur un entier", {"m": {"avg": {"field": "stock"}}}),
        ("min sur une date", {"m": {"min": {"field": "cree_le"}}}),
        ("stats sur une date", {"m": {"stats": {"field": "cree_le"}}}),
        ("missing sur une metrique", {"m": {"avg": {"field": "note", "missing": 0}}}),
        ("metrique sur champ partiel", {"m": {"avg": {"field": "note"}}}),
        ("deux metriques", {"a": {"min": {"field": "prix"}},
                            "b": {"max": {"field": "prix"}}}),
        # --- terms : le coeur des facettes
        ("terms keyword", {"f": {"terms": {"field": "marque"}}}),
        ("terms size=3", {"f": {"terms": {"field": "marque", "size": 3}}}),
        ("terms size=100", {"f": {"terms": {"field": "marque", "size": 100}}}),
        ("terms sur categorie", {"f": {"terms": {"field": "categorie"}}}),
        ("terms multi-valeur", {"f": {"terms": {"field": "tags"}}}),
        ("terms sur booleen", {"f": {"terms": {"field": "actif"}}}),
        ("terms min_doc_count", {"f": {"terms": {"field": "marque",
                                                 "min_doc_count": 50}}}),
        ("terms order _key asc", {"f": {"terms": {"field": "marque",
                                                  "order": {"_key": "asc"}}}}),
        ("terms order _count asc", {"f": {"terms": {"field": "marque",
                                                    "order": {"_count": "asc"}}}}),
        # --- range
        ("range prix", {"r": {"range": {"field": "prix", "ranges": [
            {"to": 100}, {"from": 100, "to": 500}, {"from": 500}]}}}),
        ("range avec cles", {"r": {"range": {"field": "prix", "ranges": [
            {"key": "petit", "to": 100}, {"key": "grand", "from": 100}]}}}),
        # --- histogram
        ("histogram prix", {"h": {"histogram": {"field": "prix", "interval": 200}}}),
        ("histogram min_doc_count=0", {"h": {"histogram": {
            "field": "prix", "interval": 300, "min_doc_count": 0}}}),
        ("histogram stock", {"h": {"histogram": {"field": "stock", "interval": 50}}}),
        # --- date_histogram
        ("date_histogram 30j", {"d": {"date_histogram": {
            "field": "cree_le", "fixed_interval": "30d"}}}),
        # --- sous-agregations : la vraie valeur
        ("terms + avg", {"f": {"terms": {"field": "marque"},
                               "aggs": {"prix_moyen": {"avg": {"field": "prix"}}}}}),
        ("terms + stats", {"f": {"terms": {"field": "categorie"},
                                 "aggs": {"s": {"stats": {"field": "prix"}}}}}),
        ("terms + terms", {"f": {"terms": {"field": "categorie"},
                                 "aggs": {"g": {"terms": {"field": "marque",
                                                          "size": 3}}}}}),
        ("terms + range", {"f": {"terms": {"field": "marque", "size": 2},
                                 "aggs": {"r": {"range": {"field": "prix", "ranges": [
                                     {"to": 200}, {"from": 200}]}}}}}),
        ("trois niveaux", {"a": {"terms": {"field": "categorie", "size": 2},
                                 "aggs": {"b": {"terms": {"field": "marque", "size": 2},
                                                "aggs": {"c": {"avg": {
                                                    "field": "prix"}}}}}}}),
        # --- agregations sous une requete (pas seulement match_all)
        ("avec une requete", {"f": {"terms": {"field": "marque"}}},
         {"match": {"corps": "appareil"}}),
        ("avec un filtre", {"m": {"avg": {"field": "prix"}}},
         {"bool": {"filter": [{"term": {"categorie": "audio"}}]}}),
    ]


def presque_egal(a, b, chemin, ecarts):
    if isinstance(a, dict) and isinstance(b, dict):
        for cle in sorted(set(a) | set(b)):
            if cle not in a:
                ecarts.append(f"{chemin}.{cle} : absent de ferrite "
                              f"(ES : {json.dumps(b[cle])[:60]})")
            elif cle not in b:
                ecarts.append(f"{chemin}.{cle} : en trop chez ferrite "
                              f"({json.dumps(a[cle])[:60]})")
            else:
                presque_egal(a[cle], b[cle], f"{chemin}.{cle}", ecarts)
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            ecarts.append(f"{chemin} : {len(a)} elements chez ferrite, {len(b)} chez ES")
        for i, (x, y) in enumerate(zip(a, b)):
            presque_egal(x, y, f"{chemin}[{i}]", ecarts)
    elif isinstance(a, float) or isinstance(b, float):
        if a is None or b is None:
            if a is not b:
                ecarts.append(f"{chemin} : ferrite={a} / ES={b}")
        elif abs(float(a) - float(b)) > 1e-9 * max(1.0, abs(float(b))):
            ecarts.append(f"{chemin} : ferrite={a} / ES={b}")
    elif a != b:
        ecarts.append(f"{chemin} : ferrite={json.dumps(a)[:60]} / ES={json.dumps(b)[:60]}")


def main():
    f = Elasticsearch(FERRITE, request_timeout=120)
    e = Elasticsearch(ES, request_timeout=120)

    docs = corpus.documents()
    print(f"== corpus : {len(docs)} documents\n== indexation")
    for client, nom in ((f, "ferrite"), (e, "ES")):
        client.options(ignore_status=404).indices.delete(index=INDEX)
        client.indices.create(index=INDEX, mappings=corpus.MAPPINGS,
                              settings={"number_of_shards": 1, "number_of_replicas": 0})
        ops = []
        for doc_id, doc in docs:
            ops.append({"index": {"_index": INDEX, "_id": doc_id}})
            ops.append(doc)
        for debut in range(0, len(ops), 400):
            client.bulk(operations=ops[debut:debut + 400])
        client.indices.refresh(index=INDEX)
        print(f"   {nom} pret")

    batterie = aggregations()
    print(f"\n== {len(batterie)} agregations, comparees champ par champ\n")
    total = ok = 0
    for entree in batterie:
        label, aggs = entree[0], entree[1]
        query = entree[2] if len(entree) > 2 else {"match_all": {}}
        total += 1
        resultats = []
        for client in (f, e):
            try:
                r = client.search(index=INDEX, query=query, aggs=aggs, size=0)
                resultats.append(("ok", r.get("aggregations")))
            except ApiError as ex:
                resultats.append(("err", {"status": ex.meta.status,
                                          "type": ex.body.get("error", {}).get("type")}))
        (kf, vf), (ke, ve) = resultats
        ecarts = []
        if kf != ke:
            ecarts.append(f"ferrite {kf} / ES {ke} :: ferrite={json.dumps(vf)[:180]} "
                          f":: ES={json.dumps(ve)[:180]}")
        elif kf == "err":
            if vf["status"] != ve["status"]:
                ecarts.append(f"status ferrite={vf['status']} / ES={ve['status']}")
        else:
            presque_egal(vf, ve, "", ecarts)

        if ecarts:
            print(f"  [ecart] {label}")
            for x in ecarts[:6]:
                print(f"          {x}")
        else:
            ok += 1
            print(f"  [  ok  ] {label}")

    for client in (f, e):
        client.options(ignore_status=404).indices.delete(index=INDEX)
    print(f"\n  {ok}/{total} agregations identiques a Elasticsearch")
    return 0 if ok == total else 1


if __name__ == "__main__":
    sys.exit(main())
