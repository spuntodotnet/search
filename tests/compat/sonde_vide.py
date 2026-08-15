#!/usr/bin/env python3
"""Sonde : que valide un serveur qui n'a **aucun** index ?

C'est l'etat que le harnais n'exercait pas, et c'est pour ca que le seul echec
silencieux du projet y a vecu si longtemps : la traduction du Query DSL se fait
index par index, donc zero index voulait dire zero traduction, donc zero
validation. `POST /_search` avec `{"aggs": {"a": {"significant_terms": ...}}}`
rendait 200 et des agregations vides.

Ce fichier pose les memes questions aux deux serveurs, sur un cluster vide, et
separe deux familles :

* **les identiques** : ce que les deux doivent rendre a l'octet pres — les
  requetes valides (200, meme corps) et les erreurs de lecture du corps
  (`unknown query`, `Unknown aggregation type`, cle inconnue), qu'ES refuse
  aussi sans le moindre index ;
* **les refus assumes** : ce qu'ES **sait faire** et pas ferrite
  (`significant_terms`, `intervals`, `flat_settings`). ES rend alors 200 avec
  un vrai resultat ; ferrite doit rendre 400. La question posee ici n'est donc
  pas « rend-il la meme chose qu'ES ? » mais « refuse-t-il, plutot que de
  repondre 200 en silence ? » — c'est la regle qui prime dans ce depot.

    python3 tests/compat/sonde_vide.py [ferrite] [es]

Les deux serveurs doivent etre **vides** : la sonde refuse de tourner sinon,
puisque c'est exactement l'etat qu'elle mesure.
"""
import json
import sys
import urllib.error
import urllib.request

INDEX_REGLAGES = "sonde-vide-reglages"


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


# --------------------------------------------------------------------------
# Ce que les deux serveurs doivent rendre pareil, sans aucun index
# --------------------------------------------------------------------------

# Requetes valides : 200 des deux cotes, et le meme corps — `_shards` a zero,
# `max_score` a 0.0 (et non `null`, qu'ES ne rend que quand un shard a
# repondu), aucune section `aggregations`.
VALIDES = [
    ("corps vide", "POST", "/_search", {}),
    ("match_all", "POST", "/_search", {"query": {"match_all": {}}}),
    # Un champ que personne ne mappe : sans shard, il n'y a pas de verdict de
    # mapping a rendre. ES repond 200, ferrite aussi.
    ("term sur champ absent", "POST", "/_search",
     {"query": {"term": {"absent": "x"}}}),
    ("exists sur champ absent", "POST", "/_search",
     {"query": {"exists": {"field": "absent"}}}),
    ("sort sur champ absent", "POST", "/_search", {"sort": ["absent"]}),
    ("agg terms sur champ absent", "POST", "/_search",
     {"aggs": {"a": {"terms": {"field": "absent"}}}}),
    ("nested sur chemin absent", "POST", "/_search",
     {"query": {"nested": {"path": "p", "query": {"term": {"p.x": "1"}}}}}),
    ("has_child sans champ join", "POST", "/_search",
     {"query": {"has_child": {"type": "t", "query": {"match_all": {}}}}}),
    ("parent_id sans champ join", "POST", "/_search",
     {"query": {"parent_id": {"type": "t", "id": "1"}}}),
    ("bool complet", "POST", "/_search",
     {"query": {"bool": {"must": [{"match": {"t": "x"}}],
                         "filter": [{"range": {"d": {"gte": "now-1d/d"}}}],
                         "should": [{"term": {"k": "a"}}],
                         "minimum_should_match": "50%"}}}),
    ("from/size", "POST", "/_search", {"from": 5, "size": 3}),
    ("_source", "POST", "/_search", {"_source": ["a", "b"]}),
    ("_count", "POST", "/_count", {"query": {"match_all": {}}}),
    ("_count champ absent", "POST", "/_count",
     {"query": {"term": {"absent": "x"}}}),
    # Un motif qui ne correspond a rien vise zero index, exactement comme un
    # cluster vide : c'est le cas qu'un client rencontre pour de vrai (index
    # quotidiens, premier demarrage).
    ("motif sans correspondance", "POST", "/pas-la-*/_search",
     {"query": {"match_all": {}}}),
    ("motif sans correspondance, query invalide vs mapping", "POST",
     "/pas-la-*/_search", {"query": {"term": {"absent": "x"}}}),
]

# Erreurs de lecture du corps : ES les rend **aussi** sans index, parce
# qu'elles sont prononcees avant qu'un shard ne soit consulte. C'est la
# frontiere exacte que ferrite doit reproduire.
REFUSES_PAR_LES_DEUX = [
    ("query inconnue", "POST", "/_search",
     {"query": {"pas_une_query": {"f": "x"}}}),
    ("query inconnue sous un bool", "POST", "/_search",
     {"query": {"bool": {"must": [{"pas_une_query": {}}]}}}),
    ("query inconnue sous un nested", "POST", "/_search",
     {"query": {"nested": {"path": "p", "query": {"pas_une_query": {}}}}}),
    ("query inconnue sous un has_child", "POST", "/_search",
     {"query": {"has_child": {"type": "t", "query": {"pas_une_query": {}}}}}),
    ("agregation inconnue", "POST", "/_search",
     {"aggs": {"a": {"pas_une_agg": {"field": "f"}}}}),
    ("sous-agregation inconnue", "POST", "/_search",
     {"aggs": {"a": {"terms": {"field": "f"},
                     "aggs": {"b": {"pas_une_agg": {}}}}}}),
    ("cle inconnue dans le corps", "POST", "/_search", {"pas_une_cle": 1}),
    ("size negatif", "POST", "/_search", {"size": -1}),
    ("ordre de tri invalide", "POST", "/_search",
     {"sort": [{"f": {"order": "nawak"}}]}),
    ("query inconnue sur _count", "POST", "/_count",
     {"query": {"pas_une_query": {}}}),
    ("query inconnue sur un motif sans correspondance", "POST",
     "/pas-la-*/_search", {"query": {"pas_une_query": {}}}),
]

# Ce qu'ES sait faire et pas ferrite. ES rend 200 (c'est une vraie reponse, pas
# un silence) ; ferrite doit rendre 400 plutot que de laisser croire que la
# requete a marche. Divergence assumee, listee dans docs/compat.md.
REFUSES_PAR_FERRITE_SEUL = [
    ("agregation significant_terms", "POST", "/_search",
     {"aggs": {"a": {"significant_terms": {"field": "f"}}}}),
    ("agregation filters", "POST", "/_search",
     {"aggs": {"a": {"filters": {"filters": {"x": {"match_all": {}}}}}}}),
    ("query intervals", "POST", "/_search",
     {"query": {"intervals": {"f": {"match": {"query": "x"}}}}}),
    ("query script_score", "POST", "/_search",
     {"query": {"script_score": {"query": {"match_all": {}},
                                 "script": {"source": "1"}}}}),
    ("rest_total_hits_as_int", "POST", "/_search?rest_total_hits_as_int=true",
     {"query": {"match_all": {}}}),
]

# Les deux petits freres de la meme famille, trouves en meme temps : ils
# changent la **forme** de la reponse chez ES, et ferrite la rendait inchangee
# sans rien dire. Ils ont besoin d'un index, donc ils passent apres.
#
# `flat_settings` sur les reglages d'un **index** en est sorti depuis : ce
# n'etait qu'une reecriture de cles, elle est ecrite, et il est donc servi. Il
# est verifie plus bas sur la propriete qui compte — que les cles soient
# vraiment aplaties — plutot que compte comme un refus manquant.
REGLAGES = [
    ("_settings include_defaults", "GET",
     f"/{INDEX_REGLAGES}/_settings?include_defaults=true", None),
    ("_cluster/settings flat_settings", "GET",
     "/_cluster/settings?flat_settings=true", None),
    ("_cluster/settings include_defaults", "GET",
     "/_cluster/settings?include_defaults=true", None),
]


def type_erreur(body):
    """Le type d'erreur, en depliant le `root_cause` d'ES."""
    err = body.get("error")
    if not isinstance(err, dict):
        return "?"
    racines = err.get("root_cause") or []
    if racines and isinstance(racines[0], dict) and racines[0].get("type"):
        return racines[0]["type"]
    return err.get("type", "?")


def reponse_comparable(st, body):
    """Ce qui doit coincider : le corps entier sur un 200, le statut sinon.

    `took` est une duree, elle ne peut pas coincider. Sur une erreur, seul le
    **statut** se compare : ferrite nomme ses refus avec son propre type
    (`not_implemented_in_ferrite_exception`), exprès — un client qui le voit
    sait que ce n'est pas sa requete qui est fautive."""
    if st != 200:
        return f"{st}", f"{st} {type_erreur(body)}"
    vu = {k: v for k, v in body.items() if k != "took"}
    return json.dumps(vu, sort_keys=True), json.dumps(vu, sort_keys=True)


def indices(base):
    st, body = http(base, "GET", "/_cat/indices?format=json")
    return [i.get("index") for i in body] if st == 200 else []


def ligne(marque, libelle, reps):
    print(f"{marque} {libelle:44} " +
          "  |  ".join(f"{nom}={vu[:90]}" for nom, _, vu in reps))


def main():
    ferrite = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
    es = sys.argv[2] if len(sys.argv) > 2 else "http://localhost:9201"
    dispo = []
    for nom, base in [("ferrite", ferrite), ("es", es)]:
        try:
            http(base, "GET", "/")
        except Exception as exc:  # noqa: BLE001
            print(f"# {nom} indisponible ({base}) : {exc}")
            continue
        restants = indices(base)
        if restants:
            print(f"# {nom} n'est pas vide ({restants}) : c'est l'etat que "
                  f"cette sonde mesure, elle ne peut pas s'en passer")
            return 2
        dispo.append((nom, base))
    if len(dispo) < 2:
        print("# il faut les deux serveurs pour comparer")
        return 2

    ecarts = total = 0
    print("# serveur vide — les deux doivent repondre pareil")
    for famille in (VALIDES, REFUSES_PAR_LES_DEUX):
        for libelle, methode, chemin, corps in famille:
            reps = [(nom, *reponse_comparable(*http(base, methode, chemin, corps)))
                    for nom, base in dispo]
            distinctes = {cle for _, cle, _ in reps}
            total += 1
            if len(distinctes) > 1:
                ecarts += 1
            ligne(" " if len(distinctes) <= 1 else "*", libelle, reps)

    silences = 0
    print("\n# refus assumes — ES sait le faire, ferrite doit le dire")
    for libelle, methode, chemin, corps in REFUSES_PAR_FERRITE_SEUL:
        reps = [(nom, *reponse_comparable(*http(base, methode, chemin, corps)))
                for nom, base in dispo]
        st_ferrite = int(reps[0][1].split()[0]) if reps[0][1][:1].isdigit() else 200
        muet = st_ferrite == 200
        silences += muet
        ligne("!" if muet else " ", libelle, reps)

    print("\n# les memes questions, mais sur les reglages d'un index")
    for nom, base in dispo:
        http(base, "PUT", f"/{INDEX_REGLAGES}", {})
    try:
        for libelle, methode, chemin, corps in REGLAGES:
            reps = [(nom, *reponse_comparable(*http(base, methode, chemin, corps)))
                    for nom, base in dispo]
            st_ferrite = int(reps[0][1].split()[0]) if reps[0][1][:1].isdigit() else 200
            muet = st_ferrite == 200
            silences += muet
            ligne("!" if muet else " ", libelle, reps)

        # `flat_settings`, lui, est servi : les deux serveurs doivent rendre
        # 200, et **toutes** les cles de ferrite doivent porter un point. Les
        # valeurs, elles, ne peuvent pas coincider (uuid, date de creation) et
        # ne sont pas comparees.
        total += 1
        aplaties = []
        for nom, base in dispo:
            st, corps = http(base, "GET",
                             f"/{INDEX_REGLAGES}/_settings?flat_settings=true")
            cles = list(((corps.get(INDEX_REGLAGES) or {}).get("settings") or {}))
            aplaties.append(st == 200 and bool(cles)
                            and all("." in c for c in cles))
        ok = all(aplaties)
        if not ok:
            ecarts += 1
        ligne(" " if ok else "*", "_settings flat_settings (servi)",
              [(nom, "", "cles aplaties" if a else "cles NON aplaties")
               for (nom, _), a in zip(dispo, aplaties)])
    finally:
        for nom, base in dispo:
            http(base, "DELETE", f"/{INDEX_REGLAGES}")

    print(f"\n{total - ecarts}/{total} identiques, "
          f"{silences} refus rendus en silence")
    return 1 if (ecarts or silences) else 0


if __name__ == "__main__":
    sys.exit(main())
