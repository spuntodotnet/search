#!/usr/bin/env python3
"""Une sous-agregation voit-elle tous les documents de son bucket ?

    python3 tests/compat/sonde_sous_aggs.py [ferrite] [es]
    python3 tests/compat/sonde_sous_aggs.py --docs 50000
    python3 tests/compat/sonde_sous_aggs.py --seuil

# Pourquoi cette sonde existe

`diff_aggs.py` compare 53 agregations sur un corpus de 600 documents, et les
trouve identiques. Le banc a l'echelle
([`bench_echelle.py`](bench_echelle.py)) a pose les memes agregations sur
500 000 documents et en a trouve deux fausses : les `doc_count` de chaque bucket
etaient exacts, mais la **sous-agregation** de ce bucket ne comptait pas tous
ses documents. Un `sum` rendait 9 672 881 la ou Elasticsearch rendait
12 008 586, en 200, sans un mot. Sur deux millions de documents, un bucket de
28 518 documents rendait un `value_count` de 1 692 — 94 % de perdus.

La cause etait dans tantivy 0.26.1, `aggregation/cached_sub_aggs.rs` :
`LowCardSubAggCache::flush_local` ne vidait que les buckets au-dessus d'un
seuil puis **effacait le cache entier**, donc perdait les documents des buckets
qu'il n'avait pas vides.

Le defaut est **corrige** : ferrite epingle le correctif d'amont (tantivy issue
#2992), voir [`docs/tantivy-patch.md`](../../docs/tantivy-patch.md). Cette
sonde reste, et pour deux raisons qui ne sont pas la meme :

- **le mode par defaut** pose 46 combinaisons parent × sous-agregation aux deux
  serveurs sur un corpus assez gros et assez desequilibre pour reveiller le
  defaut, et compare les reponses champ par champ. Une sous-agregation qui
  reperdrait des documents y ressort tout de suite ;
- **`--seuil`** rejoue la mesure qui a fonde la decision : a partir de combien
  de documents par segment le defaut apparaissait, et a partir de combien de
  documents par bucket. Les bornes publiees dans `docs/compat.md` sortent de
  la, pas d'une lecture du code de tantivy.

Ce que la mesure a montre, et qui n'etait devinable ni de l'un ni de l'autre :

- le seuil est **exact, pas approximatif** : 2 047 documents dans un segment
  sont justes, 2 048 ne le sont plus. Aucun corpus ecrit a la main ne
  l'atteint, d'ou les annees d'invisibilite ;
- dans une fenetre qui se vide, un bucket est perdu s'il a **au plus
  `2048 / (2 * nombre de buckets)`** documents — 204 perdus, 205 gardes sur
  5 buckets. C'est donc la **minorite** qui disparait, ce qui est exactement le
  contraire de rassurant (un pays minoritaire, une commande au-dessus d'un
  seuil, un statut d'erreur) ;
- **toutes** les metriques etaient touchees, pas seulement `value_count` :
  `sum`, `avg`, `min`, `max`, `stats`. Un `avg` rendait 21,5 la ou ES rend
  21,428 — un nombre faux **plausible**, le pire des deux. Et les
  sous-agregations de **buckets** aussi (`terms`, `range`, `histogram`) ;
- cote parents, seuls un `terms` de premier niveau sous 100 valeurs distinctes
  et **tout** `range` empruntaient ce cache. Un `terms` a 200 valeurs, un
  `histogram`, et le `filter` que ferrite execute lui-meme etaient corrects.
  Ce n'etait donc pas « les sous-agregations sont approximatives » : c'etait un
  chemin precis.

Elle refuse de tourner si elle ne trouve pas ses deux cibles : une sonde
differentielle qui ne compare rien ne doit pas rendre de verdict.
"""
import argparse
import json
import sys
import urllib.error
import urllib.request

INDEX = "sonde_sous_aggs"

# Le seuil de vidage du cache de tantivy. Il n'est pas configurable et il n'est
# pas lu ici : c'est `--seuil` qui le mesure, cette constante ne sert qu'a
# choisir les tailles a poser de part et d'autre.
FLUSH = 2048


def appel(base, chemin, corps=None, methode=None, brut=False):
    data = None
    if corps is not None:
        data = corps.encode() if brut else json.dumps(corps).encode()
    req = urllib.request.Request(
        base + chemin, data=data, method=methode or ("POST" if data else "GET"),
        headers={"Content-Type": "application/x-ndjson" if brut
                 else "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=600) as r:
            return json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return json.loads(e.read() or b"null")
    except (urllib.error.URLError, OSError):
        return None


def vivant(base):
    r = appel(base, "/")
    return isinstance(r, dict) and "version" in r


MAPPING = {"properties": {
    "categorie": {"type": "keyword"},
    "large": {"type": "keyword"},
    "note": {"type": "integer"},
    "prix": {"type": "double"},
}}


def cree(base, mapping=None):
    appel(base, f"/{INDEX}", methode="DELETE")
    r = appel(base, f"/{INDEX}", {
        "settings": {"index": {"number_of_shards": 1, "number_of_replicas": 0}},
        "mappings": mapping or MAPPING}, methode="PUT")
    if not (r or {}).get("acknowledged"):
        raise SystemExit(f"  !! {base} refuse l'index : {json.dumps(r)[:200]}")


def verse(base, docs):
    lignes = []
    for i, d in enumerate(docs):
        lignes.append(json.dumps({"index": {"_index": INDEX, "_id": str(i)}}))
        lignes.append(json.dumps(d))
    for d in range(0, len(lignes), 20000):
        appel(base, "/_bulk", "\n".join(lignes[d:d + 20000]) + "\n", brut=True)
    appel(base, f"/{INDEX}/_refresh", methode="POST")


def prepare(base, n):
    """Un corpus minimal, et deliberement **desequilibre**.

    C'est la moitie du cas : un corpus regulier ne montre rien. Le cache de
    tantivy se vidait par tranches de 2 048 documents et ne recopiait que les
    buckets au-dessus d'un seuil calcule sur leur nombre — un bucket qui recoit
    sa part d'une tranche le depasse toujours. Il faut donc un bucket **rare** :
    `categorie` vaut `rare` un document sur 997, et `prix` depasse 50 un
    document sur 200. Ce sont exactement les proportions d'une vraie facette
    (un pays minoritaire, une commande au-dessus d'un seuil), et c'est pour ca
    que le defaut n'etait pas une curiosite.

    `large` porte 199 valeurs, donc au-dessus des 100 a partir desquelles
    tantivy prend l'autre cache : c'est le temoin, celui qui etait deja juste.
    """
    cree(base)
    verse(base, [{
        "categorie": "rare" if i % 997 == 0 else f"c{i % 4}",
        "large": "rare" if i % 997 == 0 else f"L{i % 199}",
        "note": i % 4,
        "prix": 90.0 if i % 200 == 0 else float(i % 40),
    } for i in range(n)])


# --- la matrice ------------------------------------------------------------
# Chaque parent est un chemin de code different chez tantivy, chaque
# sous-agregation une facon differente de perdre les documents. La matrice
# entiere est le seul moyen de repondre a « quelles formes sont touchees ? »
# sans lire le code de la dependance.

SOUS = {
    "value_count": {"value_count": {"field": "note"}},
    "sum": {"sum": {"field": "note"}},
    "avg": {"avg": {"field": "prix"}},
    "min": {"min": {"field": "prix"}},
    "max": {"max": {"field": "prix"}},
    "stats": {"stats": {"field": "note"}},
    "terms": {"terms": {"field": "note", "size": 10}},
    "range": {"range": {"field": "prix", "ranges": [{"to": 20}, {"from": 20}]}},
    "histogram": {"histogram": {"field": "prix", "interval": 20}},
}

PARENTS = {
    # Le chemin fautif no 1 : un `terms` de premier niveau sous 100 valeurs.
    "terms(keyword, 5 valeurs dont une rare)":
        lambda s: {"terms": {"field": "categorie", "size": 20}, "aggs": {"n": s}},
    # Le temoin : 199 valeurs, donc l'autre cache, donc deja juste.
    "terms(keyword, 199 valeurs)":
        lambda s: {"terms": {"field": "large", "size": 500}, "aggs": {"n": s}},
    # Le chemin fautif no 2 : un `range`, quel que soit son nombre d'intervalles.
    "range(prix, un intervalle rare)":
        lambda s: {"range": {"field": "prix", "ranges": [{"to": 50}, {"from": 50}]},
                   "aggs": {"n": s}},
    # Temoin : `histogram` prend l'autre cache.
    "histogram(prix)":
        lambda s: {"histogram": {"field": "prix", "interval": 25}, "aggs": {"n": s}},
    # Temoin : `filter` n'est pas execute par tantivy mais par ferrite.
    "filter (execute par ferrite)":
        lambda s: {"filter": {"match_all": {}}, "aggs": {"n": s}},
}


def cas():
    out = {}
    for pnom, pf in PARENTS.items():
        for snom, s in SOUS.items():
            out[f"{pnom} > {snom}"] = {"size": 0, "aggs": {"b": pf(s)}}
    # Trois niveaux : le defaut se transmettait-il plus bas ? (non — c'est le
    # premier niveau qui perdait les documents, mais il faut le montrer.)
    out["histogram > terms(5 dont une rare) > value_count"] = {
        "size": 0, "aggs": {"h": {
            "histogram": {"field": "prix", "interval": 25},
            "aggs": {"b": {"terms": {"field": "categorie", "size": 20},
                           "aggs": {"n": {"value_count": {"field": "note"}}}}}}}}
    return out


def presque_egal(a, b, chemin, ecarts):
    """Meme comparaison que `diff_aggs.py` : les flottants a 1e-9 pres, tout le
    reste identique."""
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
            ecarts.append(f"{chemin} : {len(a)} elements chez ferrite, "
                          f"{len(b)} chez ES")
        for i, (x, y) in enumerate(zip(a, b)):
            presque_egal(x, y, f"{chemin}[{i}]", ecarts)
    elif isinstance(a, float) or isinstance(b, float):
        if a is None or b is None:
            if a is not b:
                ecarts.append(f"{chemin} : ferrite={a} / ES={b}")
        elif abs(float(a) - float(b)) > 1e-9 * max(1.0, abs(float(b))):
            ecarts.append(f"{chemin} : ferrite={a} / ES={b}")
    elif a != b:
        ecarts.append(f"{chemin} : ferrite={json.dumps(a)[:60]} / "
                      f"ES={json.dumps(b)[:60]}")


def matrice(ferrite, es, docs):
    batterie = cas()
    print(f"== {docs} documents, {len(batterie)} combinaisons parent x "
          f"sous-agregation posees aux deux serveurs\n")
    for base in (ferrite, es):
        prepare(base, docs)
    seg = ((appel(ferrite, f"/{INDEX}/_stats") or {}).get("_all", {})
           .get("primaries", {}).get("segments", {}).get("count"))
    print(f"   ferrite : {seg} segment(s), soit ~{docs // max(seg or 1, 1)} "
          f"documents par segment (il en faut plus de {FLUSH} pour que le "
          f"defaut puisse apparaitre)\n")

    ecarts_total = 0
    for nom, corps in sorted(batterie.items()):
        rf = appel(ferrite, f"/{INDEX}/_search", corps)
        re_ = appel(es, f"/{INDEX}/_search", corps)
        ecarts = []
        if ("error" in (rf or {})) != ("error" in (re_ or {})):
            qui = "ferrite" if "error" in (rf or {}) else "Elasticsearch"
            autre = (rf if qui == "ferrite" else re_)
            ecarts.append(f"{qui} refuse ce que l'autre sert : "
                          f"{json.dumps(autre.get('error'))[:140]}")
        elif "error" not in (rf or {}):
            presque_egal(rf.get("aggregations"), re_.get("aggregations"),
                         "", ecarts)
        print(f"  [{'ok   ' if not ecarts else 'ECART'}] {nom}")
        for x in ecarts[:4]:
            print(f"          {x}")
        if ecarts:
            ecarts_total += 1

    for base in (ferrite, es):
        appel(base, f"/{INDEX}", methode="DELETE")
    print(f"\n  {len(batterie) - ecarts_total}/{len(batterie)} identiques a "
          f"Elasticsearch")
    return ecarts_total


def value_count_du_bucket(base, cle):
    r = appel(base, f"/{INDEX}/_search", {"size": 0, "aggs": {
        "b": {"terms": {"field": "categorie", "size": 20},
              "aggs": {"n": {"value_count": {"field": "note"}}}}}})
    for b in (r or {}).get("aggregations", {}).get("b", {}).get("buckets", []):
        if b["key"] == cle:
            return b["doc_count"], b["n"]["value"]
    return None, None


def seuil(ferrite, es):
    """Les deux bornes du defaut, mesurees plutot que lues.

    Les tailles posees encadrent la borne : la campagne ne dit pas « ca marche
    a 50 000 documents », elle dit ou exactement ca basculait."""
    print("== Les deux bornes du defaut, posees aux deux serveurs\n")
    print(f"-- 1. combien de documents dans un segment ? (un seul document "
          f"'rare', en tete)")
    print(f"   {'N':>7}  {'ferrite':>22}  {'Elasticsearch':>22}")
    ecarts = 0
    for n in (1024, FLUSH - 1, FLUSH, FLUSH * 2):
        vus = []
        for base in (ferrite, es):
            cree(base)
            verse(base, [{"categorie": "rare" if i == 0 else f"c{i % 4}",
                          "note": 1} for i in range(n)])
            vus.append(value_count_du_bucket(base, "rare"))
        (dcf, vcf), (dce, vce) = vus
        ok = (dcf, vcf) == (dce, vce)
        ecarts += 0 if ok else 1
        print(f"   {n:>7}  doc_count={dcf} value_count={vcf:<6}  "
              f"doc_count={dce} value_count={vce:<6}  "
              f"{'ok' if ok else '<-- ECART'}")

    # Le seuil par bucket : `FLUSH / (nombre de buckets * 2)`, soit 204 sur les
    # 5 buckets de ce corpus. On pose 203, 204 et 205 documents 'rare' dans la
    # premiere fenetre — la borne est entre les deux derniers.
    attendu = FLUSH // (5 * 2)
    print(f"\n-- 2. combien de documents dans le bucket ? ({FLUSH * 2} documents, "
          f"5 buckets, donc {FLUSH}/(5*2) = {attendu} attendu)")
    print(f"   {'k':>7}  {'ferrite':>22}  {'Elasticsearch':>22}")
    for k in (attendu - 1, attendu, attendu + 1):
        vus = []
        for base in (ferrite, es):
            cree(base)
            verse(base, [{"categorie": "rare" if i < k else f"c{i % 4}",
                          "note": 1} for i in range(FLUSH * 2)])
            vus.append(value_count_du_bucket(base, "rare"))
        (dcf, vcf), (dce, vce) = vus
        ok = (dcf, vcf) == (dce, vce)
        ecarts += 0 if ok else 1
        print(f"   {k:>7}  doc_count={dcf} value_count={vcf:<6}  "
              f"doc_count={dce} value_count={vce:<6}  "
              f"{'ok' if ok else '<-- ECART'}")

    for base in (ferrite, es):
        appel(base, f"/{INDEX}", methode="DELETE")
    print(f"\n  {7 - ecarts}/7 bornes identiques a Elasticsearch")
    return ecarts


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("ferrite", nargs="?", default="http://localhost:9200")
    p.add_argument("es", nargs="?", default="http://localhost:9201")
    p.add_argument("--docs", type=int, default=50000,
                   help=f"documents indexes (defaut : 50000 — il en faut plus "
                        f"de {FLUSH} par segment pour que le defaut puisse "
                        f"apparaitre)")
    p.add_argument("--seuil", action="store_true",
                   help="rejouer les deux bornes du defaut plutot que la "
                        "matrice : documents par segment, puis par bucket")
    args = p.parse_args()

    for nom, base in (("ferrite", args.ferrite), ("Elasticsearch", args.es)):
        if not vivant(base):
            print(f"  !! {nom} ne repond pas sur {base}. Une sonde "
                  f"differentielle qui ne trouve qu'un serveur ne compare "
                  f"rien : elle ne doit pas rendre de verdict.", file=sys.stderr)
            return 2

    ecarts = (seuil(args.ferrite, args.es) if args.seuil
              else matrice(args.ferrite, args.es, args.docs))
    if ecarts:
        print("\n  Un ecart ici veut dire que la sous-agregation reperd des "
              "documents : l'epingle de tantivy a saute (voir "
              "docs/tantivy-patch.md et tests/spike_sous_aggs.rs), ou le "
              "defaut est revenu par un autre chemin.")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
