#!/usr/bin/env python3
"""Memes resultats, et a quel prix ? Le banc ferrite / Elasticsearch.

Le contrat du projet tient en une phrase : **les memes resultats, sans que les
performances s'effondrent**. Ce script mesure exactement ces deux choses, sur
les deux serveurs, avec le meme corpus et les memes requetes.

    python3 tests/compat/bench_vs_es.py [ferrite_url] [es_url] [--tours N]

Il ne depend d'aucun client Elasticsearch : tout passe par HTTP brut. C'est
volontaire — le client 8.x refuse de parler a un serveur 7.10, et surtout un
banc doit mesurer le serveur, pas la bibliotheque qui l'appelle. Il marche donc
indifferemment contre un ES 7.x ou 8.x.

Ce qui est mesure :

  - **indexation** : le meme `_bulk` des deux cotes, en secondes
  - **latence de recherche** : la batterie de `corpus.requetes()`, rejouee
    `--tours` fois, mediane et p95 par serveur
  - **debit** : la meme batterie, 8 requetes en vol
  - **resultats** : pour chaque requete, memes documents dans le meme ordre ?

Precautions prises pour que la comparaison veuille dire quelque chose : les
deux serveurs recoivent les memes documents, un `_refresh` explicite, puis des
tours de chauffe qui ne sont pas comptes (une JVM sans chauffe se mesure au
compilateur, pas au moteur).
"""
import json
import statistics
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

import corpus

ARGS = [a for a in sys.argv[1:] if not a.startswith("-")]
FERRITE = ARGS[0] if ARGS else "http://localhost:9200"
ES = ARGS[1] if len(ARGS) > 1 else "http://localhost:9201"
TOURS = int(sys.argv[sys.argv.index("--tours") + 1]) if "--tours" in sys.argv else 5
CHAUFFE = 2
EN_VOL = 8
INDEX = "bench_vs_es"
TAILLE = 25


def appel(base, chemin, corps=None, methode=None, brut=False):
    """Un appel HTTP, sans client : c'est le serveur qu'on mesure."""
    data = None
    if corps is not None:
        data = corps.encode() if brut else json.dumps(corps).encode()
    req = urllib.request.Request(
        base + chemin,
        data=data,
        method=methode or ("POST" if data else "GET"),
        headers={"Content-Type": "application/x-ndjson" if brut else "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            return json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return json.loads(e.read() or b"null")


def indexe(base, docs):
    appel(base, f"/{INDEX}", methode="DELETE")
    r = appel(base, f"/{INDEX}", {"settings": {"number_of_shards": 1,
                                               "number_of_replicas": 0},
                                  "mappings": corpus.MAPPINGS}, methode="PUT")
    if "error" in (r or {}):
        raise SystemExit(f"{base} refuse l'index : {r['error'].get('reason')}")

    lignes = []
    for doc_id, doc in docs:
        lignes.append(json.dumps({"index": {"_index": INDEX, "_id": doc_id}}))
        lignes.append(json.dumps(doc))
    debut = time.perf_counter()
    for d in range(0, len(lignes), 800):
        appel(base, "/_bulk", "\n".join(lignes[d:d + 800]) + "\n", brut=True)
    appel(base, f"/{INDEX}/_refresh", methode="POST")
    return time.perf_counter() - debut


def corps_de(query, sort):
    corps = {"query": query, "size": TAILLE}
    if sort:
        corps["sort"] = sort
    return corps


def latences(base, qs, tours):
    """Rend la liste des latences, une par requete et par tour."""
    for _ in range(CHAUFFE):
        for _, query, sort in qs:
            appel(base, f"/{INDEX}/_search", corps_de(query, sort))
    mesures = []
    for _ in range(tours):
        for _, query, sort in qs:
            t = time.perf_counter()
            appel(base, f"/{INDEX}/_search", corps_de(query, sort))
            mesures.append((time.perf_counter() - t) * 1000)
    return mesures


def debit(base, qs):
    """Requetes par seconde avec `EN_VOL` requetes simultanees."""
    corps = [corps_de(q, s) for _, q, s in qs]
    debut = time.perf_counter()
    with ThreadPoolExecutor(max_workers=EN_VOL) as ex:
        list(ex.map(lambda c: appel(base, f"/{INDEX}/_search", c), corps))
    return len(corps) / (time.perf_counter() - debut)


def resultats(base, qs):
    """Pour chaque requete : (total, les identifiants dans l'ordre)."""
    out = []
    for label, query, sort in qs:
        r = appel(base, f"/{INDEX}/_search", corps_de(query, sort))
        if "error" in (r or {}):
            out.append((label, None, r["error"].get("reason", "")[:120]))
            continue
        total = r["hits"]["total"]
        total = total["value"] if isinstance(total, dict) else total
        out.append((label, total, [h["_id"] for h in r["hits"]["hits"]]))
    return out


def version(base):
    r = appel(base, "/")
    return (r or {}).get("version", {}).get("number", "?")


def ligne(gauche, a, b, rapport=None):
    print(f"  {gauche:<26} {a:>14} {b:>14}   {rapport or ''}")


def main():
    docs = corpus.documents()
    qs = corpus.requetes(docs)
    vf, ve = version(FERRITE), version(ES)
    print(f"== ferrite {vf} ({FERRITE})  vs  Elasticsearch {ve} ({ES})")
    print(f"== {len(docs)} documents, {len(qs)} requetes, {TOURS} tours "
          f"(+{CHAUFFE} de chauffe)\n")

    tf = indexe(FERRITE, docs)
    te = indexe(ES, docs)

    lf = latences(FERRITE, qs, TOURS)
    le = latences(ES, qs, TOURS)
    df = debit(FERRITE, qs)
    de = debit(ES, qs)

    rf = resultats(FERRITE, qs)
    re_ = resultats(ES, qs)
    identiques = refuses = permutes = ecarts = 0
    details = []
    for (label, tot_f, ids_f), (_, tot_e, ids_e) in zip(rf, re_):
        if tot_f is None:
            refuses += 1
            details.append(f"  [refus ferrite] {label} : {ids_f}")
        elif (tot_f, ids_f) == (tot_e, ids_e):
            identiques += 1
        elif tot_f == tot_e and set(ids_f) == set(ids_e):
            # Memes documents, ordre different : le plus souvent des ex aequo.
            # Trancher demande les scores d'ES, c'est le travail de
            # `diff_relevance.py` ; ici on le signale sans conclure.
            permutes += 1
            details.append(f"  [ordre] {label} — memes documents, ordre different")
        else:
            ecarts += 1
            details.append(f"  [ecart] {label} : total {tot_f} vs {tot_e}, "
                           f"{len(set(ids_e) - set(ids_f))} document(s) manquant(s)")

    def stat(m, q):
        return statistics.quantiles(m, n=100)[q - 1] if len(m) > 100 else max(m)

    print(f"  {'':<26} {'ferrite':>14} {'Elasticsearch':>14}")
    ligne("indexation (s)", f"{tf:.2f}", f"{te:.2f}", f"x{te / tf:.1f}")
    ligne("latence mediane (ms)", f"{statistics.median(lf):.2f}",
          f"{statistics.median(le):.2f}",
          f"x{statistics.median(le) / statistics.median(lf):.1f}")
    ligne("latence p95 (ms)", f"{stat(lf, 95):.2f}", f"{stat(le, 95):.2f}",
          f"x{stat(le, 95) / stat(lf, 95):.1f}")
    ligne(f"debit ({EN_VOL} en vol, req/s)", f"{df:.0f}", f"{de:.0f}",
          f"x{df / de:.1f}")

    print(f"\n  resultats : {identiques}/{len(qs)} requetes identiques "
          f"(memes documents, meme ordre)")
    if permutes:
        print(f"              {permutes} memes documents, ordre different "
              f"(ex aequo ? `diff_relevance.py` tranche)")
    if refuses:
        print(f"              {refuses} refusees par ferrite")
    if ecarts:
        print(f"              {ecarts} ecarts reels")
    for d in details[:10]:
        print(d)

    for base in (FERRITE, ES):
        appel(base, f"/{INDEX}", methode="DELETE")
    print("\n  Le rapport `x` compare ferrite a Elasticsearch : au-dessus de 1, "
          "ferrite est devant.")
    return 1 if (ecarts or refuses) else 0


if __name__ == "__main__":
    sys.exit(main())
