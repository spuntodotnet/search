#!/usr/bin/env python3
"""Sonde : que fait *vraiment* un Elasticsearch avec `minimum_should_match` ?

La documentation decrit les notations (entier, pourcentage, combinaison) sans
dire ce qui arrive aux bords : arrondi, depassement du nombre de `should`,
formes negatives, chaine invalide. Ce fichier pose la question aux deux
serveurs et imprime leurs deux reponses cote a cote.

    python3 tests/compat/sonde_msm.py [ferrite] [es]
"""
import json
import sys
import urllib.error
import urllib.request

INDEX = "sonde-msm"
INDEX_NESTED = "sonde-msm-nested"


def http(base, method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        base + path, data=data, method=method,
        headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        corps = e.read()
        try:
            return e.code, json.loads(corps or b"{}")
        except json.JSONDecodeError:
            return e.code, {"raw": corps.decode(errors="replace")}


# Chaque document satisfait un nombre connu de clauses `should` : le document
# `dN` porte les N premieres lettres. Le compte de resultats dit donc
# directement quel minimum a ete applique.
DOCS = {
    "d0": {"a": "non", "b": "non", "c": "non", "d": "non"},
    "d1": {"a": "oui", "b": "non", "c": "non", "d": "non"},
    "d2": {"a": "oui", "b": "oui", "c": "non", "d": "non"},
    "d3": {"a": "oui", "b": "oui", "c": "oui", "d": "non"},
    "d4": {"a": "oui", "b": "oui", "c": "oui", "d": "oui"},
}
SHOULD4 = [{"term": {c: "oui"}} for c in "abcd"]
SHOULD3 = SHOULD4[:3]


# Sous un `nested`, c'est le meme calcul mais compte **par element** : le
# document `nN` a un element qui satisfait N clauses `should`, les autres
# elements n'en satisfaisant qu'une chacun.
DOCS_NESTED = {
    "n0": [{"a": "non", "b": "non", "c": "non"}],
    "n1": [{"a": "oui", "b": "non", "c": "non"}],
    "n2": [{"a": "oui", "b": "oui", "c": "non"}],
    "n3": [{"a": "oui", "b": "oui", "c": "oui"}],
    "nx": [{"a": "oui", "b": "non", "c": "non"},
           {"a": "non", "b": "oui", "c": "non"},
           {"a": "non", "b": "non", "c": "oui"}],
    # Le document qui separe « le `should` est facultatif quand il y a un
    # `must_not` » de « il ne l'est pas » : son premier element satisfait le
    # `should` mais tombe sous le `must_not`, le second ne satisfait aucun
    # `should`. Aucun element ne satisfait les deux, donc ES ne le rend pas.
    "ny": [{"a": "oui", "b": "oui", "c": "non"},
           {"a": "non", "b": "non", "c": "non"}],
}
SHOULD_NESTED = [{"term": {f"lignes.{c}": "oui"}} for c in "abc"]


def bulk(base, index, docs):
    lignes = []
    for id_, doc in docs.items():
        lignes.append(json.dumps({"index": {"_index": index, "_id": id_}}))
        lignes.append(json.dumps(doc))
    corps = "\n".join(lignes) + "\n"
    req = urllib.request.Request(
        base + "/_bulk?refresh=true", data=corps.encode(), method="POST",
        headers={"Content-Type": "application/x-ndjson"})
    urllib.request.urlopen(req).read()


def prepare(base):
    http(base, "DELETE", f"/{INDEX}")
    http(base, "PUT", f"/{INDEX}", {"mappings": {"properties": {
        c: {"type": "keyword"} for c in "abcd"}}})
    bulk(base, INDEX, DOCS)

    http(base, "DELETE", f"/{INDEX_NESTED}")
    http(base, "PUT", f"/{INDEX_NESTED}", {"mappings": {"properties": {
        "lignes": {"type": "nested", "properties": {
            c: {"type": "keyword"} for c in "abc"}}}}})
    bulk(base, INDEX_NESTED, {k: {"lignes": v} for k, v in DOCS_NESTED.items()})


def cas():
    """(libelle, corps de requete)."""
    out = []
    for spec in ["75%", "70%", "76%", "50%", "25%", "100%", "150%", "0%",
                 "-25%", "-50%", "-99%", "3<90%", "2<-25% 9<-3", "abc",
                 "75", "75.5%", "", "1<2", 2, 4, 5, 0, -1, -2, 1.5]:
        out.append((f"4 should, msm={spec!r}",
                    {"bool": {"should": SHOULD4, "minimum_should_match": spec}}))
    for spec in ["50%", "66%", "67%", "33%", "-33%"]:
        out.append((f"3 should, msm={spec!r}",
                    {"bool": {"should": SHOULD3, "minimum_should_match": spec}}))
    # Avec une clause obligatoire a cote.
    out.append(("must + 4 should, msm='75%'", {"bool": {
        "must": [{"term": {"a": "oui"}}], "should": SHOULD4,
        "minimum_should_match": "75%"}}))
    # Sans aucun `should`.
    out.append(("0 should, msm='75%'", {"bool": {
        "must": [{"term": {"a": "oui"}}], "minimum_should_match": "75%"}}))
    out.append(("must_not seul, msm='75%'", {"bool": {
        "must_not": [{"term": {"a": "oui"}}], "minimum_should_match": "75%"}}))
    # Une clause `should` sur un champ non mappe compte-t-elle dans le total ?
    for spec in ["75%", "100%", 3]:
        out.append((f"3 mappes + 1 inconnu, msm={spec!r}", {"bool": {
            "should": SHOULD3 + [{"term": {"inconnu": "oui"}}],
            "minimum_should_match": spec}}))
    return out


def cas_nested():
    out = []
    for spec in ["50%", "66%", "67%", "100%", "-33%", "2<50%", 2, "abc"]:
        out.append((f"nested, 3 should, msm={spec!r}", {"nested": {
            "path": "lignes", "query": {"bool": {
                "should": SHOULD_NESTED, "minimum_should_match": spec}}}}))
    # Le defaut : `should` est-il facultatif des qu'il y a un `must_not` ?
    out.append(("nested, should + must_not, sans msm", {"nested": {
        "path": "lignes", "query": {"bool": {
            "should": [{"term": {"lignes.a": "oui"}}],
            "must_not": [{"term": {"lignes.b": "oui"}}]}}}}))
    out.append(("nested, should + must_not, msm=1", {"nested": {
        "path": "lignes", "query": {"bool": {
            "should": [{"term": {"lignes.a": "oui"}}],
            "must_not": [{"term": {"lignes.b": "oui"}}],
            "minimum_should_match": 1}}}}))
    out.append(("nested, should + must + must_not", {"nested": {
        "path": "lignes", "query": {"bool": {
            "must": [{"term": {"lignes.a": "oui"}}],
            "should": [{"term": {"lignes.c": "oui"}}],
            "must_not": [{"term": {"lignes.b": "oui"}}]}}}}))
    return out


def interroge(base, requete, index=INDEX):
    """(ce qui se compare, ce qui s'affiche).

    Sur une erreur, seul le **statut** se compare : ES empile ses erreurs de
    recherche sous un `search_phase_execution_exception` dont la `root_cause`
    porte le vrai type, la ou ferrite rend l'erreur directement. C'est une
    divergence connue et documentee (`docs/compat.md`), pas un effet de ce
    parametre — le type reste affiche pour qu'on la voie."""
    st, body = http(base, "POST", f"/{index}/_search?size=10", {"query": requete})
    if st == 200:
        docs = sorted(h["_id"] for h in body["hits"]["hits"])
        return str(docs), str(docs)
    t = body.get("error", {})
    if isinstance(t, dict):
        t = t.get("root_cause", [{}])[0].get("type") or t.get("type", "?")
    return str(st), f"{st} {t}"


def main():
    ferrite = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
    es = sys.argv[2] if len(sys.argv) > 2 else "http://localhost:9201"
    cibles = [("ferrite", ferrite), ("es", es)]
    dispo = []
    for nom, base in cibles:
        try:
            http(base, "GET", "/")
            prepare(base)
            dispo.append((nom, base))
        except Exception as e:  # noqa: BLE001
            print(f"# {nom} indisponible ({base}) : {e}")
    ecarts = total = 0
    for index, batterie in [(INDEX, cas()), (INDEX_NESTED, cas_nested())]:
        for libelle, requete in batterie:
            reps = [(nom, *interroge(base, requete, index)) for nom, base in dispo]
            vals = {cle for _, cle, _ in reps}
            marque = " " if len(vals) <= 1 else "*"
            print(f"{marque} {libelle:36} " +
                  "  |  ".join(f"{nom}={vu}" for nom, _, vu in reps))
            total += 1
            if len(vals) > 1:
                ecarts += 1
    print(f"\n{total - ecarts}/{total} identiques")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
