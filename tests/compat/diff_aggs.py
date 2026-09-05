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
        # --- extended_stats : purement calculatoire, donc rien a tolerer.
        #
        # La variance et l'ecart-type sont **recalcules** par ferrite avec les
        # expressions d'ES a partir des trois sommes compensees que tantivy
        # accumule (celui de tantivy passe par Welford, et les deux ne rendent
        # pas le meme `double`). Les bords sont mesures un par un par
        # `sonde_metriques.py` ; ce qui est verrouille ici, ce sont les cles de
        # la reponse — ES en pose dix-huit, dont un objet `std_deviation_bounds`
        # a six valeurs qu'il ne retire jamais.
        ("extended_stats", {"m": {"extended_stats": {"field": "prix"}}}),
        ("extended_stats sur un entier", {"m": {"extended_stats": {"field": "stock"}}}),
        ("extended_stats sigma 3", {"m": {"extended_stats": {"field": "prix",
                                                             "sigma": 3}}}),
        ("extended_stats sur champ partiel", {"m": {"extended_stats": {"field": "note"}}}),
        ("extended_stats missing", {"m": {"extended_stats": {"field": "note",
                                                             "missing": 0}}}),
        # Un seul document : la variance de population vaut `0.0`, celle
        # d'echantillon divise par zero et ES rend la **chaine** `"NaN"`.
        ("extended_stats sur un document", {"m": {"extended_stats": {"field": "prix"}}},
         {"term": {"_id": "1"}}),
        ("extended_stats sur zero document", {"m": {"extended_stats": {"field": "prix"}}},
         {"term": {"categorie": "categorie_inexistante"}}),
        ("terms + extended_stats", {"f": {"terms": {"field": "marque", "size": 3},
                                          "aggs": {"s": {"extended_stats": {
                                              "field": "prix"}}}}}),
        ("terms order extended_stats.variance", {"f": {"terms": {
            "field": "marque", "size": 3, "order": {"s.variance": "desc"}},
            "aggs": {"s": {"extended_stats": {"field": "prix"}}}}}),
        # --- percentiles : exact chez ferrite, et exact chez ES tant qu'un
        # seau porte moins de 2 000 valeurs. Le corpus en compte 600, donc les
        # deux doivent coincider **au bit pres** ; au-dela, la divergence est
        # declaree et chiffree par `sonde_metriques.py --frontiere`.
        ("percentiles", {"m": {"percentiles": {"field": "prix"}}}),
        ("percentiles percents", {"m": {"percentiles": {"field": "prix",
                                                        "percents": [10, 50, 90]}}}),
        ("percentiles keyed false", {"m": {"percentiles": {"field": "prix",
                                                           "keyed": False}}}),
        ("percentiles sur un entier", {"m": {"percentiles": {"field": "stock"}}}),
        ("percentiles sur une date", {"m": {"percentiles": {"field": "cree_le"}}}),
        ("percentiles missing", {"m": {"percentiles": {"field": "note",
                                                       "missing": 0}}}),
        ("percentiles sur zero document", {"m": {"percentiles": {"field": "prix"}}},
         {"term": {"categorie": "categorie_inexistante"}}),
        ("terms + percentiles", {"f": {"terms": {"field": "marque", "size": 3},
                                       "aggs": {"p": {"percentiles": {
                                           "field": "prix"}}}}}),
        ("range + percentiles", {"r": {"range": {"field": "prix", "ranges": [
            {"to": 200}, {"from": 200}]},
            "aggs": {"p": {"percentiles": {"field": "prix"}}}}}),
        ("date_histogram + percentiles", {"d": {"date_histogram": {
            "field": "cree_le", "fixed_interval": "30d"},
            "aggs": {"p": {"percentiles": {"field": "prix"}}}}}),
        ("filter + percentiles", {"f": {"filter": {"term": {"categorie": "audio"}},
                                        "aggs": {"p": {"percentiles": {
                                            "field": "prix"}}}}}),
        # --- top_hits : une recherche complete a l'interieur d'un seau. Le
        # bloc `hits` entier est compare — `total`, `max_score`, l'ordre, et le
        # tableau `sort` de chaque hit.
        ("top_hits", {"t": {"top_hits": {"size": 2, "sort": [{"prix": "desc"}]}}}),
        ("top_hits from", {"t": {"top_hits": {"size": 2, "from": 3,
                                              "sort": [{"prix": "asc"}]}}}),
        ("top_hits _source liste", {"t": {"top_hits": {
            "size": 1, "sort": [{"prix": "asc"}], "_source": ["marque", "prix"]}}}),
        ("top_hits _source false", {"t": {"top_hits": {
            "size": 1, "sort": [{"prix": "asc"}], "_source": False}}}),
        ("top_hits docvalue_fields", {"t": {"top_hits": {
            "size": 1, "sort": [{"prix": "asc"}], "docvalue_fields": ["prix"]}}}),
        ("terms + top_hits", {"f": {"terms": {"field": "marque", "size": 3},
                                    "aggs": {"t": {"top_hits": {
                                        "size": 1, "sort": [{"prix": "desc"}]}}}}}),
        ("terms + top_hits + avg", {"f": {"terms": {"field": "categorie", "size": 2},
                                          "aggs": {"t": {"top_hits": {
                                              "size": 2, "sort": [{"prix": "asc"}]}},
                                              "m": {"avg": {"field": "prix"}}}}}),
        ("range + top_hits", {"r": {"range": {"field": "prix", "ranges": [
            {"to": 200}, {"from": 200}]},
            "aggs": {"t": {"top_hits": {"size": 1, "sort": [{"prix": "asc"}]}}}}}),
        ("histogram + top_hits, seaux vides", {"h": {
            "histogram": {"field": "prix", "interval": 200},
            "aggs": {"t": {"top_hits": {"size": 1, "sort": [{"prix": "asc"}]}}}}},
         {"term": {"categorie": "audio"}}),
        ("date_histogram + top_hits", {"d": {"date_histogram": {
            "field": "cree_le", "fixed_interval": "30d"},
            "aggs": {"t": {"top_hits": {"size": 1, "sort": [{"cree_le": "asc"}]}}}}}),
        ("filter + top_hits", {"f": {"filter": {"term": {"categorie": "audio"}},
                                     "aggs": {"t": {"top_hits": {
                                         "size": 2, "sort": [{"prix": "asc"}]}}}}}),
        ("terms + top_hits sans tri", {"f": {"terms": {"field": "marque", "size": 2},
                                             "aggs": {"t": {"top_hits": {"size": 1}}}}}),
        # --- terms : le coeur des facettes
        ("terms keyword", {"f": {"terms": {"field": "marque"}}}),
        ("terms size=3", {"f": {"terms": {"field": "marque", "size": 3}}}),
        ("terms size=100", {"f": {"terms": {"field": "marque", "size": 100}}}),
        ("terms sur categorie", {"f": {"terms": {"field": "categorie"}}}),
        ("terms multi-valeur", {"f": {"terms": {"field": "tags"}}}),
        ("terms sur booleen", {"f": {"terms": {"field": "actif"}}}),
        # `min_doc_count` autre que sa valeur par defaut est **refuse** depuis
        # que le fuzzing differentiel a montre que `sum_other_doc_count` n'en
        # suivait pas la regle d'ES (voir docs/fuzz.md). Le cas reste ici sous
        # sa forme par defaut : c'est celle qui doit coincider.
        ("terms min_doc_count=1", {"f": {"terms": {"field": "marque",
                                                   "min_doc_count": 1}}}),
        ("terms order _key asc", {"f": {"terms": {"field": "marque",
                                                  "order": {"_key": "asc"}}}}),
        ("terms order _count asc", {"f": {"terms": {"field": "marque",
                                                    "order": {"_count": "asc"}}}}),
        # --- ce qui separe un `terms` d'une facette : filtrer les termes, et
        # les classer sur une sous-agregation.
        #
        # Les deux compteurs sont **dans** la comparaison, et c'est le point :
        # ES les calcule apres filtrage, donc un `sum_other_doc_count` qui
        # ignorerait l'`exclude` resterait plausible. Les bords sont mesures
        # cas par cas par `sonde_facettes.py` ; ce qui est verrouille ici, ce
        # sont les cles de la reponse.
        ("terms include regexp", {"f": {"terms": {"field": "marque",
                                                  "include": "S.*"}}}),
        ("terms exclude regexp", {"f": {"terms": {"field": "marque",
                                                  "exclude": "S.*"}}}),
        ("terms include liste", {"f": {"terms": {"field": "categorie",
                                                 "include": ["audio", "ecran"]}}}),
        ("terms exclude liste", {"f": {"terms": {"field": "categorie",
                                                 "exclude": ["audio"]}}}),
        ("terms include + exclude", {"f": {"terms": {
            "field": "marque", "include": ".*e.*", "exclude": ["Dell"]}}}),
        ("terms include + size", {"f": {"terms": {
            "field": "marque", "size": 2, "include": ".*e.*"}}}),
        ("terms exclude + size", {"f": {"terms": {
            "field": "marque", "size": 2, "exclude": ["Sony"]}}}),
        ("terms exclude + count asc", {"f": {"terms": {
            "field": "marque", "size": 2, "shard_size": 4,
            "exclude": ["Sony"], "order": {"_count": "asc"}}}}),
        ("terms order avg desc", {"f": {"terms": {
            "field": "marque", "size": 3, "order": {"pm": "desc"}},
            "aggs": {"pm": {"avg": {"field": "prix"}}}}}),
        ("terms order avg asc", {"f": {"terms": {
            "field": "marque", "size": 3, "order": {"pm": "asc"}},
            "aggs": {"pm": {"avg": {"field": "prix"}}}}}),
        ("terms order sum desc", {"f": {"terms": {
            "field": "categorie", "order": {"pm": "desc"}},
            "aggs": {"pm": {"sum": {"field": "prix"}}}}}),
        ("terms order value_count asc", {"f": {"terms": {
            "field": "categorie", "order": {"vc": "asc"}},
            "aggs": {"vc": {"value_count": {"field": "prix"}}}}}),
        ("terms order stats.avg desc", {"f": {"terms": {
            "field": "marque", "size": 3, "order": {"s.avg": "desc"}},
            "aggs": {"s": {"stats": {"field": "prix"}}}}}),
        ("terms order stats.count asc", {"f": {"terms": {
            "field": "marque", "size": 3, "order": {"s.count": "asc"}},
            "aggs": {"s": {"stats": {"field": "prix"}}}}}),
        # `note` n'est pas renseigne partout : ces trois-la portent donc des
        # seaux dont la metrique n'a **aucune** valeur, et c'est le seul
        # endroit ou `min`, `max` et `avg` cessent de se classer pareil.
        ("terms order avg desc, metrique vide", {"f": {"terms": {
            "field": "marque", "size": 4, "order": {"pm": "desc"}},
            "aggs": {"pm": {"avg": {"field": "note"}}}}}),
        ("terms order min asc, metrique vide", {"f": {"terms": {
            "field": "marque", "size": 4, "order": {"pm": "asc"}},
            "aggs": {"pm": {"min": {"field": "note"}}}}}),
        ("terms order max desc, metrique vide", {"f": {"terms": {
            "field": "marque", "size": 4, "order": {"pm": "desc"}},
            "aggs": {"pm": {"max": {"field": "note"}}}}}),
        ("terms order stats.max desc, metrique vide", {"f": {"terms": {
            "field": "marque", "size": 4, "order": {"s.max": "desc"}},
            "aggs": {"s": {"stats": {"field": "note"}}}}}),
        ("terms include + order avg desc", {"f": {"terms": {
            "field": "marque", "size": 2, "include": ".*e.*",
            "order": {"pm": "desc"}},
            "aggs": {"pm": {"avg": {"field": "prix"}}}}}),
        ("terms order avg desc + sous-agg de seaux", {"f": {"terms": {
            "field": "categorie", "size": 2, "order": {"pm": "desc"}},
            "aggs": {"pm": {"avg": {"field": "prix"}},
                     "g": {"terms": {"field": "marque", "size": 2}}}}}),
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
        # --- ce qu'un bucket **vide** doit porter
        #
        # Un `histogram` comble ses trous des deux cotes, mais tantivy ne fait
        # pas tourner les sous-agregations dans les buckets qu'il fabrique : un
        # `range` y rendait `buckets: []` la ou ES rend ses intervalles a
        # `doc_count: 0`. Trouve par le fuzzer, corrige dans `src/aggs.rs`,
        # verrouille ici — c'est un resultat faux rendu en 200, et un graphe qui
        # empile deux niveaux y perdait ses categories sur les periodes creuses.
        #
        # La requete restreint le corpus a une categorie : c'est ce qui creuse
        # les trous, donc ce qui fait exister les buckets vides.
        ("histogram + range, buckets vides", {"h": {
            "histogram": {"field": "prix", "interval": 20},
            "aggs": {"r": {"range": {"field": "prix", "ranges": [
                {"to": 100}, {"from": 100, "to": 500}, {"from": 500}]}}}}},
         {"term": {"categorie": "audio"}}),
        ("histogram + range keyed, buckets vides", {"h": {
            "histogram": {"field": "prix", "interval": 20},
            "aggs": {"r": {"range": {"field": "prix", "keyed": True, "ranges": [
                {"key": "petit", "to": 100}, {"key": "grand", "from": 100}]}}}}},
         {"term": {"categorie": "audio"}}),
        ("histogram keyed + range, buckets vides", {"h": {
            "histogram": {"field": "prix", "interval": 20, "keyed": True},
            "aggs": {"r": {"range": {"field": "prix", "ranges": [
                {"to": 100}, {"from": 100}]}}}}},
         {"term": {"categorie": "audio"}}),
        ("histogram + range + terms, buckets vides", {"h": {
            "histogram": {"field": "prix", "interval": 10},
            "aggs": {"r": {"range": {"field": "prix", "ranges": [
                {"to": 100}, {"from": 100}]},
                "aggs": {"g": {"terms": {"field": "marque", "size": 3}},
                         "s": {"stats": {"field": "stock"}}}}}}},
         {"term": {"categorie": "audio"}}),
        ("histogram extended_bounds + range", {"h": {
            "histogram": {"field": "prix", "interval": 200,
                          "extended_bounds": {"min": -400, "max": 1800}},
            "aggs": {"r": {"range": {"field": "prix", "ranges": [
                {"to": 100}, {"from": 100}]}}}}}),
        ("date_histogram + range, buckets vides", {"d": {
            "date_histogram": {"field": "cree_le", "fixed_interval": "5d"},
            "aggs": {"r": {"range": {"field": "prix", "ranges": [
                {"to": 100}, {"from": 100}]}}}}},
         {"term": {"categorie": "audio"}}),
        ("histogram + range sur une date, buckets vides", {"h": {
            "histogram": {"field": "prix", "interval": 20},
            "aggs": {"r": {"range": {"field": "cree_le", "ranges": [
                {"to": "2026-01-01"}, {"from": "2026-01-01"}]}}}}},
         {"term": {"categorie": "audio"}}),
        ("recherche sans resultat, histogram + range", {"h": {
            "histogram": {"field": "prix", "interval": 200,
                          "extended_bounds": {"min": 0, "max": 600}},
            "aggs": {"r": {"range": {"field": "prix", "ranges": [
                {"to": 100}, {"from": 100}]},
                "aggs": {"g": {"terms": {"field": "marque"}}}}}}},
         {"term": {"categorie": "categorie_inexistante"}}),
        # --- filter : celle que ferrite execute lui-meme, en croisant la
        # requete de la recherche avec celle du filtre
        ("filter simple", {"f": {"filter": {"term": {"categorie": "audio"}}}}),
        ("filter qui ne prend rien", {"f": {"filter": {
            "term": {"categorie": "categorie_inexistante"}}}}),
        ("filter sur un range", {"f": {"filter": {"range": {"prix": {"gte": 500}}}}}),
        ("filter sur un bool", {"f": {"filter": {"bool": {
            "must": [{"range": {"prix": {"lt": 300}}}],
            "must_not": [{"term": {"categorie": "audio"}}]}}}}),
        ("plusieurs filter", {"a": {"filter": {"term": {"actif": True}}},
                              "b": {"filter": {"term": {"actif": False}}},
                              "c": {"filter": {"match_all": {}}}}),
        ("filter + metrique", {"f": {"filter": {"term": {"categorie": "audio"}},
                                     "aggs": {"m": {"avg": {"field": "prix"}}}}}),
        ("filter + terms", {"f": {"filter": {"range": {"prix": {"gte": 200}}},
                                  "aggs": {"g": {"terms": {"field": "marque",
                                                           "size": 5}}}}}),
        ("filter + terms + avg", {"f": {
            "filter": {"term": {"actif": True}},
            "aggs": {"g": {"terms": {"field": "categorie"},
                           "aggs": {"m": {"avg": {"field": "prix"}}}}}}}),
        ("filter dans filter", {"a": {
            "filter": {"range": {"prix": {"gte": 100}}},
            "aggs": {"b": {"filter": {"term": {"categorie": "audio"}}}}}}),
        ("filter a cote d'un terms", {"f": {"filter": {"term": {"actif": True}}},
                                      "g": {"terms": {"field": "categorie"}}}),
        ("filter sous une requete", {"f": {"filter": {"term": {"actif": True}}}},
         {"match": {"corps": "appareil"}}),
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
