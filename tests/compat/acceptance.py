# Le critere d'acceptation de la carte ferrite #1, tel quel, sans aucune
# modification, avec le client officiel (pip install "elasticsearch>=8,<9").
from elasticsearch import Elasticsearch

es = Elasticsearch("http://localhost:9200")
assert es.ping()
print(es.info())

es.indices.create(
    index="livres",
    mappings={
        "properties": {
            "titre":  {"type": "text"},
            "auteur": {"type": "keyword"},
            "annee":  {"type": "integer"},
            "resume": {"type": "text"},
        }
    },
)

es.bulk(operations=[
    {"index": {"_index": "livres", "_id": "1"}},
    {"titre": "Le Horla", "auteur": "Maupassant", "annee": 1887,
     "resume": "un homme se croit habite par une presence invisible"},
    {"index": {"_index": "livres", "_id": "2"}},
    {"titre": "Bel-Ami", "auteur": "Maupassant", "annee": 1885,
     "resume": "l ascension sociale d un arriviste dans la presse parisienne"},
    {"index": {"_index": "livres", "_id": "3"}},
    {"titre": "Germinal", "auteur": "Zola", "annee": 1885,
     "resume": "la greve des mineurs du nord de la France"},
], refresh=True)

r = es.search(index="livres", query={"match": {"resume": "presse"}})
assert r["hits"]["total"]["value"] == 1
assert r["hits"]["hits"][0]["_id"] == "2"
assert r["hits"]["hits"][0]["_source"]["titre"] == "Bel-Ami"
assert r["hits"]["hits"][0]["_score"] > 0

r = es.search(index="livres", query={
    "bool": {
        "must":   [{"match": {"titre": "bel ami"}}],
        "filter": [{"term": {"auteur": "Maupassant"}},
                   {"range": {"annee": {"gte": 1880, "lt": 1886}}}],
    }
})
assert r["hits"]["total"]["value"] == 1

r = es.search(index="livres", query={"match_all": {}},
              sort=[{"annee": "desc"}], size=2, source_includes=["titre"])
assert [h["_source"]["titre"] for h in r["hits"]["hits"]] == ["Le Horla", "Bel-Ami"]

assert es.get(index="livres", id="3")["_source"]["auteur"] == "Zola"
print("OK — ferrite parle elasticsearch")
