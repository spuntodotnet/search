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
import random
import sys

try:
    from elasticsearch import ApiError, Elasticsearch
except ImportError:  # client 7.x : l'exception ne porte pas le meme nom.
    # `diff_es7.py` importe la batterie de requetes de ce module en pilotant un
    # client 7.x ; seul `requetes()` l'interesse, mais l'import doit passer.
    from elasticsearch import Elasticsearch, TransportError as ApiError

import corpus

FERRITE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
ES = sys.argv[2] if len(sys.argv) > 2 else "http://localhost:9201"
INDEX = "compat_relevance"
TAILLE = 25


def requetes(docs):
    """La batterie de requetes, deterministe elle aussi."""
    rng = random.Random(corpus.SEED + 1)
    mots = corpus.VOCAB
    phrases = corpus.bigrammes(docs, 14, rng)
    q = []

    # --- match : un, deux, trois termes, et l'operateur `and`
    for mot in mots[:14]:
        q.append((f"match corps [{mot}]", {"match": {"corps": mot}}, None))
    for _ in range(10):
        deux = f"{rng.choice(mots)} {rng.choice(mots)}"
        q.append((f"match corps [{deux}]", {"match": {"corps": deux}}, None))
        q.append((f"match and corps [{deux}]",
                  {"match": {"corps": {"query": deux, "operator": "and"}}}, None))
    for _ in range(6):
        trois = " ".join(rng.choice(mots) for _ in range(3))
        q.append((f"match corps [{trois}]", {"match": {"corps": trois}}, None))
    q.append(("match titre + corps identiques",
              {"bool": {"should": [{"match": {"titre": "ecran"}},
                                   {"match": {"corps": "ecran"}}]}}, None))

    # --- multi_match : la clause d'une barre de recherche
    for _ in range(8):
        mot = rng.choice(mots)
        q.append((f"multi_match best_fields [{mot}]",
                  {"multi_match": {"query": mot, "fields": ["titre", "corps"]}}, None))
        q.append((f"multi_match most_fields [{mot}]",
                  {"multi_match": {"query": mot, "fields": ["titre", "corps"],
                                   "type": "most_fields"}}, None))
        q.append((f"multi_match titre^3 [{mot}]",
                  {"multi_match": {"query": mot, "fields": ["titre^3", "corps"]}}, None))
    for _ in range(4):
        deux = f"{rng.choice(mots)} {rng.choice(mots)}"
        q.append((f"multi_match and [{deux}]",
                  {"multi_match": {"query": deux, "fields": ["titre", "corps"],
                                   "operator": "and"}}, None))
        q.append((f"multi_match tie_breaker [{deux}]",
                  {"multi_match": {"query": deux, "fields": ["titre", "corps"],
                                   "tie_breaker": 0.3}}, None))

    # --- match_phrase : sur des suites qui existent vraiment
    for phrase in phrases:
        q.append((f"match_phrase [{phrase}]", {"match_phrase": {"corps": phrase}}, None))
    # `slop` est refuse par ferrite (voir docs/compat.md) : le comparer n'aurait
    # pas de sens, la suite de compat verifie le refus.
    q.append(("match_phrase mot unique", {"match_phrase": {"corps": "ecran"}}, None))

    # --- motifs et identifiants
    for mot in mots[:6]:
        q.append((f"prefix corps [{mot[:4]}]", {"prefix": {"marque": mot[:2]}},
                  [{"prix": "asc"}]))
    for marque in corpus.MARQUES[:4]:
        q.append((f"wildcard [{marque}]", {"wildcard": {"marque": f"{marque[:2]}*"}},
                  [{"prix": "asc"}]))
        q.append((f"fuzzy [{marque}]", {"fuzzy": {"marque": marque[:-1] + "x"}},
                  [{"prix": "asc"}]))
    q.append(("ids", {"ids": {"values": ["1", "5", "9", "42"]}}, [{"prix": "asc"}]))
    q.append(("constant_score", {"constant_score": {
        "filter": {"term": {"categorie": "audio"}}, "boost": 3.0}}, None))
    for _ in range(4):
        mot = rng.choice(mots)
        q.append((f"dis_max [{mot}]", {"dis_max": {"queries": [
            {"match": {"titre": mot}}, {"match": {"corps": mot}}]}}, None))
        q.append((f"dis_max tie [{mot}]", {"dis_max": {"queries": [
            {"match": {"titre": mot}}, {"match": {"corps": mot}}],
            "tie_breaker": 0.4}}, None))

    # --- exists
    for champ in ("note", "tags", "corps", "marque"):
        q.append((f"exists [{champ}]", {"exists": {"field": champ}},
                  [{"prix": "asc"}]))

    # --- term / terms / range, en contexte filtre (ordre par tri)
    for marque in corpus.MARQUES[:5]:
        q.append((f"term marque [{marque}]", {"term": {"marque": marque}},
                  [{"prix": "asc"}]))
    q.append(("terms marque x3",
              {"terms": {"marque": corpus.MARQUES[:3]}}, [{"prix": "desc"}]))
    q.append(("term actif=true", {"term": {"actif": True}}, [{"stock": "desc"}]))
    for lo, hi in ((0, 50), (50, 200), (200, 900)):
        q.append((f"range prix [{lo},{hi}[", {"range": {"prix": {"gte": lo, "lt": hi}}},
                  [{"prix": "asc"}]))
    q.append(("range stock > 150", {"range": {"stock": {"gt": 150}}}, [{"stock": "asc"}]))
    q.append(("range date >= 2025", {"range": {"cree_le": {"gte": "2025-01-01"}}},
              [{"cree_le": "asc"}]))

    # --- bool : les combinaisons qui font une vraie recherche a facettes
    for cat in corpus.CATEGORIES[:4]:
        q.append((f"bool must+filter [{cat}]", {"bool": {
            "must": [{"match": {"corps": rng.choice(mots)}}],
            "filter": [{"term": {"categorie": cat}},
                       {"range": {"prix": {"lt": 500}}}]}}, None))
    q.append(("bool must_not", {"bool": {
        "must_not": [{"term": {"marque": "Sony"}}]}}, [{"prix": "asc"}]))
    q.append(("bool should min=2", {"bool": {
        "should": [{"term": {"categorie": "audio"}},
                   {"term": {"actif": True}},
                   {"range": {"prix": {"lt": 100}}}],
        "minimum_should_match": 2}}, [{"prix": "asc"}]))
    q.append(("bool multi_match + filtres", {"bool": {
        "must": [{"multi_match": {"query": "bluetooth reduction",
                                  "fields": ["titre^2", "corps"]}}],
        "filter": [{"term": {"actif": True}}, {"exists": {"field": "note"}}]}}, None))

    # --- tris : plusieurs cles, plusieurs types
    q.append(("tri multi-cles categorie+prix", {"match_all": {}},
              [{"categorie": "asc"}, {"prix": "desc"}]))
    q.append(("tri note desc (valeurs manquantes)", {"match_all": {}},
              [{"note": "desc"}]))
    q.append(("tri marque asc", {"match_all": {}}, [{"marque": "asc"}]))
    return q


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
