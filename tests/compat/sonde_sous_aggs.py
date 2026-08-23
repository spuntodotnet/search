#!/usr/bin/env python3
"""Une sous-agregation voit-elle tous les documents de son bucket ?

    python3 tests/compat/sonde_sous_aggs.py [ferrite] [es]
    python3 tests/compat/sonde_sous_aggs.py --docs 50000

# Pourquoi cette sonde existe

`diff_aggs.py` compare 53 agregations sur un corpus de 600 documents, et les
trouve identiques. Le banc a l'echelle
([`bench_echelle.py`](bench_echelle.py)) a pose les memes agregations sur
500 000 documents et en a trouve deux fausses : les `doc_count` de chaque bucket
etaient exacts, mais la **sous-agregation** de ce bucket ne comptait pas tous
ses documents. Un `sum` rendait 9 672 881 la ou Elasticsearch rendait
12 008 586, en 200, sans un mot.

Ce que la sonde a montre en reduisant le cas :

- le defaut n'apparait qu'au-dela de **2 048 documents dans un meme segment** :
  aucun corpus ecrit a la main ne l'atteint, d'ou les annees d'invisibilite ;
- il frappe un `terms` de **premier niveau** sur un champ a moins de 100
  valeurs distinctes, et **tout** `range` — mais seulement leurs buckets
  **rares** : le cache ne recopie que les buckets au-dessus d'un seuil, et un
  bucket qui recoit sa part de chaque tranche le depasse toujours. C'est donc
  la minorite qui disparait, ce qui est exactement le contraire de rassurant
  (un pays minoritaire, une commande au-dessus d'un seuil, un statut d'erreur) ;
- un `histogram`, et un `terms` imbrique sous un autre bucket, sont **corrects**.
  Ce n'est donc pas « les sous-agregations sont approximatives » : c'est un
  chemin precis ;
- la cause est dans tantivy 0.26.1, `aggregation/cached_sub_aggs.rs` :
  `LowCardSubAggCache::flush_local` ne vide que les buckets au-dessus d'un seuil
  puis **efface le cache entier**, donc perd les documents des buckets qu'il n'a
  pas vides. 0.26.1 est la derniere version publiee.

La sonde ne demontre rien de tout ca : elle **mesure**. Chaque cas pose la meme
agregation aux deux serveurs et compare la somme des sous-agregations aux
`doc_count` des memes buckets — un invariant qu'Elasticsearch tient toujours.

Elle refuse de tourner si elle ne trouve pas ses deux cibles : une sonde
differentielle qui ne compare rien ne doit pas rendre de verdict.
"""
import argparse
import json
import sys
import urllib.error
import urllib.request

INDEX = "sonde_sous_aggs"


def appel(base, chemin, corps=None, methode=None, brut=False):
    data = None
    if corps is not None:
        data = corps.encode() if brut else json.dumps(corps).encode()
    req = urllib.request.Request(
        base + chemin, data=data, method=methode or ("POST" if data else "GET"),
        headers={"Content-Type": "application/x-ndjson" if brut
                 else "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=300) as r:
            return json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return json.loads(e.read() or b"null")
    except (urllib.error.URLError, OSError):
        return None


def vivant(base):
    r = appel(base, "/")
    return isinstance(r, dict) and "version" in r


def prepare(base, n):
    """Un corpus minimal, et deliberement **desequilibre**.

    C'est la moitie du cas : un corpus regulier ne montre rien. Le cache de
    tantivy se vide par tranches de 2 048 documents, et ne recopie que les
    buckets qui depassent un seuil calcule sur le nombre de buckets — un bucket
    qui recoit sa part d'une tranche la depasse toujours. Il faut donc un
    bucket **rare** : `categorie` vaut `rare` un document sur 997, et `prix`
    depasse 50 un document sur 200. Ce sont exactement les proportions d'une
    vraie facette (un pays minoritaire, une commande au-dessus d'un seuil), et
    c'est pour ca que le defaut n'est pas une curiosite."""
    appel(base, f"/{INDEX}", methode="DELETE")
    r = appel(base, f"/{INDEX}", {
        "settings": {"index": {"number_of_shards": 1, "number_of_replicas": 0}},
        "mappings": {"properties": {
            "categorie": {"type": "keyword"},
            "note": {"type": "integer"},
            "prix": {"type": "double"},
        }}}, methode="PUT")
    if not (r or {}).get("acknowledged"):
        raise SystemExit(f"  !! {base} refuse l'index : {json.dumps(r)[:200]}")
    lignes = []
    for i in range(n):
        lignes.append(json.dumps({"index": {"_index": INDEX, "_id": str(i)}}))
        lignes.append(json.dumps({
            "categorie": "rare" if i % 997 == 0 else f"c{i % 4}",
            "note": i % 4,
            "prix": 90.0 if i % 200 == 0 else float(i % 40)}))
    for d in range(0, len(lignes), 10000):
        appel(base, "/_bulk", "\n".join(lignes[d:d + 10000]) + "\n", brut=True)
    appel(base, f"/{INDEX}/_refresh", methode="POST")


CAS = {
    "terms(keyword, 5 valeurs dont une rare) > value_count": {
        "size": 0, "aggs": {"b": {"terms": {"field": "categorie", "size": 20},
                                  "aggs": {"n": {"value_count": {"field": "note"}}}}}},
    "terms(keyword, 5 valeurs dont une rare) > sum": {
        "size": 0, "aggs": {"b": {"terms": {"field": "categorie", "size": 20},
                                  "aggs": {"n": {"sum": {"field": "note"}}}}}},
    "terms(entier, 4 valeurs) > value_count": {
        "size": 0, "aggs": {"b": {"terms": {"field": "note", "size": 20},
                                  "aggs": {"n": {"value_count": {"field": "note"}}}}}},
    "range > value_count": {
        "size": 0, "aggs": {"b": {"range": {"field": "prix",
                                            "ranges": [{"to": 50}, {"from": 50}]},
                                  "aggs": {"n": {"value_count": {"field": "note"}}}}}},
    "histogram > value_count": {
        "size": 0, "aggs": {"b": {"histogram": {"field": "prix", "interval": 25},
                                  "aggs": {"n": {"value_count": {"field": "note"}}}}}},
    "terms imbrique sous un histogram > value_count": {
        "size": 0, "aggs": {"h": {"histogram": {"field": "prix", "interval": 25},
                                  "aggs": {"b": {"terms": {"field": "categorie", "size": 20},
                                                 "aggs": {"n": {"value_count": {"field": "note"}}}}}}}},
}


def buckets(noeud):
    """Toutes les paires (doc_count, value_count de la sous-agregation `n`)
    trouvees dans la reponse, quel que soit le niveau d'imbrication."""
    out = []
    if isinstance(noeud, dict):
        if "buckets" in noeud:
            for b in (noeud["buckets"].values()
                      if isinstance(noeud["buckets"], dict) else noeud["buckets"]):
                if "n" in b:
                    out.append((b["doc_count"], b["n"]["value"]))
                out.extend(buckets(b))
        for cle, v in noeud.items():
            if cle != "buckets":
                out.extend(buckets(v))
    return out


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("ferrite", nargs="?", default="http://localhost:9200")
    p.add_argument("es", nargs="?", default="http://localhost:9201")
    p.add_argument("--docs", type=int, default=50000,
                   help="documents indexes (defaut : 50000 — il en faut plus "
                        "de 2048 par segment pour que le defaut apparaisse)")
    args = p.parse_args()

    for nom, base in (("ferrite", args.ferrite), ("Elasticsearch", args.es)):
        if not vivant(base):
            print(f"  !! {nom} ne repond pas sur {base}. Une sonde "
                  f"differentielle qui ne trouve qu'un serveur ne compare "
                  f"rien : elle ne doit pas rendre de verdict.", file=sys.stderr)
            return 2

    print(f"== {args.docs} documents, {len(CAS)} agregations posees aux deux "
          f"serveurs\n")
    for base in (args.ferrite, args.es):
        prepare(base, args.docs)

    ecarts = 0
    for nom, corps in CAS.items():
        lignes = []
        for base in (args.ferrite, args.es):
            r = appel(base, f"/{INDEX}/_search", corps)
            if "error" in (r or {}):
                lignes.append(None)
                continue
            lignes.append(buckets(r.get("aggregations") or {}))
        f, e = lignes
        if f is None or e is None:
            print(f"  [refus] {nom}")
            ecarts += 1
            continue
        # L'invariant : sur ces agregations, chaque document du bucket porte une
        # valeur, donc la sous-agregation doit compter exactement `doc_count`.
        manquants_f = sum(dc - n for dc, n in f if n is not None)
        manquants_e = sum(dc - n for dc, n in e if n is not None)
        # `sum` ne se compare pas a `doc_count` : on compare alors les valeurs.
        if "sum" in nom:
            vf = [n for _, n in f]
            ve = [n for _, n in e]
            ok = vf == ve
            ecart = next(((i, a, b) for i, (a, b) in enumerate(zip(vf, ve))
                          if a != b), None)
            detail = (f"bucket {ecart[0]} : ferrite {ecart[1]}, "
                      f"Elasticsearch {ecart[2]}" if ecart
                      else f"{len(vf)} vs {len(ve)} buckets")
        else:
            ok = manquants_f == 0 and manquants_e == 0
            detail = (f"{manquants_f} document(s) perdu(s) cote ferrite, "
                      f"{manquants_e} cote Elasticsearch")
        print(f"  [{'ok   ' if ok else 'ECART'}] {nom}")
        if not ok:
            print(f"          {detail}")
            ecarts += 1

    for base in (args.ferrite, args.es):
        appel(base, f"/{INDEX}", methode="DELETE")

    print(f"\n  {len(CAS) - ecarts}/{len(CAS)} agregations d'accord")
    if ecarts:
        print("\n  Les ecarts ci-dessus sont le defaut decrit en tete de ce "
              "fichier : tantivy 0.26.1 perd des documents de sous-agregation "
              "au-dela de 2 048 en cache. Voir docs/bench.md et la section "
              "« Limites connues » de docs/compat.md.")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
