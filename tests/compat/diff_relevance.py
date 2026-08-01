#!/usr/bin/env python3
"""ferrite et Elasticsearch rendent-ils les memes documents, dans le meme ordre ?

`diff_against_es.py` compare la *forme* des reponses sur quelques documents.
Celui-ci compare la *pertinence* : meme corpus de plusieurs centaines de
documents des deux cotes, une centaine de requetes generees, et pour chacune on
verifie que le total, l'ensemble des documents et **leur ordre** coincident.

L'ordre est la seule chose qui compte vraiment pour un moteur de recherche, et
c'est la seule qu'un test ecrit a la main ne sait pas verifier : on ecrirait le
test avec la meme idee fausse que le code.

Les scores ne sont pas compares en valeur — les statistiques d'index different —
mais un ecart d'ordre est signale sauf s'il porte sur des documents qu'ES
lui-meme classe ex aequo.

    python3 tests/compat/diff_relevance.py [ferrite_url] [es_url]

Outil de developpement : exige un Elasticsearch 8.x lance a cote (Docker).
"""
import sys

from elasticsearch import ApiError, Elasticsearch

import corpus
from corpus import requetes

FERRITE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
ES = sys.argv[2] if len(sys.argv) > 2 else "http://localhost:9201"
INDEX = "compat_relevance"
TAILLE = 25



class Comparateur:
    def __init__(self):
        self.ferrite = Elasticsearch(FERRITE, request_timeout=60)
        self.es = Elasticsearch(ES, request_timeout=60)
        self.total = 0
        self.identiques = 0
        self.ex_aequo = 0
        self.ecarts = []

    def prepare(self, docs):
        for client, nom in ((self.ferrite, "ferrite"), (self.es, "ES")):
            client.options(ignore_status=404).indices.delete(index=INDEX)
            client.indices.create(index=INDEX, mappings=corpus.MAPPINGS,
                                  settings={"number_of_shards": 1,
                                            "number_of_replicas": 0})
            ops = []
            for doc_id, doc in docs:
                ops.append({"index": {"_index": INDEX, "_id": doc_id}})
                ops.append(doc)
            for debut in range(0, len(ops), 400):
                client.bulk(operations=ops[debut:debut + 400])
            client.indices.refresh(index=INDEX)
            n = client.count(index=INDEX)["count"] if nom == "ES" else \
                client.search(index=INDEX, query={"match_all": {}},
                              size=0)["hits"]["total"]["value"]
            print(f"   {nom:<8} {n} documents indexes")

    def compare(self, label, query, sort):
        kw = {"index": INDEX, "query": query, "size": TAILLE}
        if sort:
            kw["sort"] = sort
        try:
            rf = self.ferrite.search(**kw)
        except ApiError as e:
            self.ecarts.append((label, f"ferrite refuse : {e.body['error']['type']}"))
            self.total += 1
            return
        re_ = self.es.search(**kw)
        self.total += 1

        tf, te = rf["hits"]["total"]["value"], re_["hits"]["total"]["value"]
        if tf != te:
            self.ecarts.append((label, f"total ferrite={tf} / ES={te}"))
            return

        idf = [h["_id"] for h in rf["hits"]["hits"]]
        ide = [h["_id"] for h in re_["hits"]["hits"]]
        if idf == ide:
            self.identiques += 1
            return

        if set(idf) != set(ide):
            manque = set(ide) - set(idf)
            trop = set(idf) - set(ide)
            self.ecarts.append((
                label,
                f"documents differents (total {tf}) — manquants chez ferrite: "
                f"{sorted(manque)[:5]}, en trop: {sorted(trop)[:5]}"))
            return

        # Memes documents, ordre different : est-ce un simple ex aequo chez ES ?
        scores = {h["_id"]: h["_score"] for h in re_["hits"]["hits"]}
        divergent = [
            (a, b) for a, b in zip(idf, ide)
            if a != b and scores.get(a) != scores.get(b)
        ]
        if divergent:
            a, b = divergent[0]
            self.ecarts.append((
                label,
                f"ordre different — ferrite place [{a}] (score ES {scores.get(a)}) la ou "
                f"ES place [{b}] ({scores.get(b)})"))
        else:
            self.ex_aequo += 1

    def run(self):
        docs = corpus.documents()
        print(f"== corpus : {len(docs)} documents, vocabulaire de "
              f"{len(corpus.VOCAB)} mots\n== indexation")
        self.prepare(docs)

        qs = requetes(docs)
        print(f"\n== {len(qs)} requetes, comparees document par document "
              f"(top {TAILLE})\n")
        for label, query, sort in qs:
            self.compare(label, query, sort)

        for label, detail in self.ecarts:
            print(f"  [ecart] {label}\n          {detail}")
        if self.ecarts:
            print()
        print(f"  {self.identiques}/{self.total} requetes : memes documents, meme ordre")
        if self.ex_aequo:
            print(f"  {self.ex_aequo}/{self.total} : memes documents, ordre permute "
                  f"uniquement entre ex aequo d'ES")
        print(f"  {len(self.ecarts)}/{self.total} ecarts reels")

        for client in (self.ferrite, self.es):
            client.options(ignore_status=404).indices.delete(index=INDEX)
        return 1 if self.ecarts else 0


if __name__ == "__main__":
    sys.exit(Comparateur().run())
