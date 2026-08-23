#!/usr/bin/env python3
"""Le banc **a l'echelle** : un corpus public, standard et cite, des deux cotes.

    python3 tests/compat/bench_echelle.py --docs 500000
    python3 tests/compat/bench_echelle.py --docs 2000000 --json docs/bench.json
    python3 tests/compat/bench_echelle.py --inventaire   # ce que la track demande

# Pourquoi celui-ci et pas `bench_vs_es.py`

`bench_vs_es.py` mesure 600 documents et 138 requetes ecrites ici. A cette
taille, l'index tient entier dans le cache du processeur : on mesure surtout le
cout d'un aller-retour HTTP, et le corpus comme les requetes sont les notres —
donc un denominateur qu'on a choisi soi-meme, ce que ce depot refuse ailleurs
(voir `conformance_es.py`, `ponderation.py`). Il reste utile comme garde-fou
rapide pendant le developpement ; il ne peut pas etayer un chiffre publie.

Ce fichier-ci prend le contre-pied sur les trois points :

1. **le corpus n'est pas le notre.** C'est celui de la track Rally `geonames`
   d'Elastic (Apache-2.0) : 11 396 503 points d'interet, telecharges depuis
   `rally-tracks.elastic.co`, dont on verifie la taille compressee **exacte**
   annoncee par la track. Le lecteur peut refaire la mesure ;
2. **les requetes ne sont pas les notres.** Elles sont lues dans
   `operations/default.json` de la track, a une revision figee, y compris les
   trois requetes a 45 586 termes que `track.py` fabrique depuis `terms.txt` ;
3. **le tri entre « jouable » et « refuse » est mesure, pas declare.** Chaque
   operation de la track est posee a ferrite ; s'il la refuse, le refus est
   releve tel quel et rattache a une capacite de `compat.yaml` (via
   `perimetre.py`). C'est la moitie du resultat : un banc qui ne publie que les
   requetes qu'il sait servir mesure son propre choix.

# Ce qui est mesure

Temps d'indexation et debit d'indexation, taille sur disque, RSS, latence
(mediane, p95, p99 quand il y a assez d'echantillons) et debit a 8 requetes en
vol — des deux cotes, sur les memes documents et les memes requetes.

Et, parce qu'un banc qui ne montre que des victoires n'est pas lu comme un
banc : le rapport publie aussi ce que ferrite **ne sait pas faire** (les
operations refusees), ce qu'il paie plus cher qu'Elasticsearch, et la taille
au-dela de laquelle il n'est plus le bon choix.

# Ce qu'il exige

Docker (pour l'Elasticsearch de reference), ~1,5 Go de disque pour le corpus
telecharge, et de la patience : un passage a 2 000 000 de documents indexe
4 000 000 de documents en tout.
"""
import argparse
import bz2
import json
import os
import re
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

RACINE = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CACHE = os.path.join(RACINE, ".bench-echelle")

# --------------------------------------------------------------------------
# 1. La track : un corpus et des requetes que nous n'avons pas ecrits
# --------------------------------------------------------------------------

TRACK = {
    "nom": "geonames",
    "depot": "https://github.com/elastic/rally-tracks",
    # La meme revision que celle citee par `tests/compat/usage/sources.json` :
    # le corpus d'usage et le banc parlent de la meme track.
    "commit": "b1cc31cd1afd68dbc0a0bebfef3a17ebd3747d79",
    "licence": "Apache-2.0",
    "base_url": "https://rally-tracks.elastic.co/geonames",
    "fichier": "documents-2.json.bz2",
    "documents": 11396503,
    # Annonces par `track.json`. On les verifie : un corpus qui a bouge sous
    # nos pieds ne se compare a rien.
    "octets_compresses": 265208777,
    "octets_decompresses": 3547613828,
}

INDEX = "geonames"

# Ce que la track demande et que ferrite ne sait pas tenir. Chaque ecart est
# applique **des deux cotes** (sauf mention) et imprime : un banc dont on ne
# peut pas relire les reglages ne prouve rien.
ECARTS_TRACK = [
    {
        "quoi": "champ `location` (`geo_point`) retire du mapping",
        "cote": "les deux",
        "pourquoi": "`geo_point` est hors du perimetre declare de ferrite "
                    "(capacite `type.autres`). La valeur reste dans le "
                    "`_source` des deux cotes : les documents envoyes sont "
                    "identiques a l'octet pres, seul l'indexation du champ "
                    "disparait.",
    },
    {
        "quoi": "`dynamic: strict` remplace par `dynamic: false`",
        "cote": "les deux",
        "pourquoi": "consequence de la ligne precedente : en `strict`, un "
                    "document portant `location` serait rejete. En `false`, il "
                    "entre dans le `_source` sans etre indexe — ce que fait "
                    "ferrite comme Elasticsearch.",
    },
    {
        "quoi": "`fielddata: true` retire de `country_code`",
        "cote": "les deux",
        "pourquoi": "ferrite refuse explicitement ce parametre de champ "
                    "(capacite `mapping.autres_params`). Il ne sert dans la "
                    "track qu'a rendre agregeable un champ `text` ; les "
                    "requetes du banc passent toutes par `country_code.raw`.",
    },
    {
        "quoi": "`index.number_of_shards` : 5 -> 1",
        "cote": "les deux",
        "pourquoi": "ferrite est mono-shard par construction. Comparer 1 shard "
                    "a 5 comparerait des nombres de shards, pas des moteurs.",
    },
    {
        "quoi": "`index.requests.cache.enable: false`",
        "cote": "Elasticsearch seul",
        "pourquoi": "c'est la track qui coupe le cache de requetes, pour "
                    "mesurer le moteur et pas son cache. ferrite refuse le "
                    "reglage — il n'a pas de cache de requetes du tout, donc "
                    "l'appliquer d'un seul cote rapproche les deux serveurs au "
                    "lieu de les eloigner.",
    },
    {
        "quoi": "pas de `_forcemerge` apres l'indexation",
        "cote": "les deux",
        "pourquoi": "la track force-merge avant de mesurer ; ferrite n'a pas la "
                    "route (cycle de vie d'index, hors perimetre). Les deux "
                    "serveurs sont donc mesures **tels qu'indexes**. La taille "
                    "sur disque d'Elasticsearch est en plus relevee apres un "
                    "`_forcemerge`, parce que c'est un gain que ferrite ne "
                    "sait pas aller chercher.",
    },
]


def sortir(msg):
    print(f"\n  !! {msg}", file=sys.stderr)
    raise SystemExit(2)


def git(*args, cwd=None):
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True,
                          check=False)


def prepare_track():
    """Clone la track a sa revision figee et **verifie** qu'elle n'a pas bouge.

    Meme geste que `appli_reelle.py` : les requetes ne valent que si elles sont
    celles d'Elastic, pas celles d'Elastic retouchees ici."""
    chemin = os.path.join(CACHE, "rally-tracks")
    if not os.path.isdir(os.path.join(chemin, ".git")):
        os.makedirs(CACHE, exist_ok=True)
        print("== clonage de rally-tracks (une fois)")
        r = git("clone", "--filter=blob:none", "--no-checkout", TRACK["depot"], chemin)
        if r.returncode:
            sortir(f"clonage impossible : {r.stderr.strip()}")
    if git("cat-file", "-e", TRACK["commit"], cwd=chemin).returncode:
        git("fetch", "origin", cwd=chemin)
    if git("checkout", "--force", TRACK["commit"], cwd=chemin).returncode:
        sortir(f"revision {TRACK['commit']} introuvable")
    tete = git("rev-parse", "HEAD", cwd=chemin).stdout.strip()
    if tete != TRACK["commit"]:
        sortir(f"revision inattendue : {tete}")
    if git("diff", "--stat", cwd=chemin).stdout.strip():
        sortir("la track a ete modifiee localement — la mesure ne vaudrait rien")
    return os.path.join(chemin, TRACK["nom"])


def sans_jinja(s):
    """Les fichiers d'une track sont des gabarits Jinja. On ne les rend pas :
    on remplace chaque parametre par le **defaut** que la track declare, ce qui
    est exactement ce qu'obtient quelqu'un qui lance `esrally --track=geonames`
    sans option."""
    s = re.sub(r"\{#.*?#\}", "", s, flags=re.S)
    s = re.sub(r"\{%.*?%\}", "", s, flags=re.S)
    s = re.sub(r"\{\{\s*[\w.]+\s*\|\s*default\(([^()]*)\)\s*(\|\s*tojson\s*)?\}\}",
               r"\1", s)
    reste = re.findall(r"\{\{.*?\}\}", s, flags=re.S)
    if reste:
        sortir(f"gabarit non resolu dans la track : {reste[0][:80]}")
    return s


def mapping_de_la_track(track):
    """Le mapping de la track, lu dans son `index.json` — pas retape ici."""
    brut = json.loads(sans_jinja(open(os.path.join(track, "index.json")).read()))
    props = brut["mappings"]["properties"]
    if "location" not in props or props["location"]["type"] != "geo_point":
        sortir("la track ne declare plus `location` en `geo_point` : "
               "relire les ecarts declares avant de mesurer")
    props.pop("location")
    if not props["country_code"].pop("fielddata", False):
        sortir("la track ne declare plus `fielddata` sur `country_code`")
    return {
        "settings": {"index": {"number_of_shards": 1, "number_of_replicas": 0}},
        "mappings": {"dynamic": False, "properties": props},
    }


def operations_de_la_track(track):
    """Les requetes de la track, lues dans `operations/default.json`, plus les
    trois que `track.py` fabrique depuis `terms.txt`.

    Rendues dans l'ordre du fichier : c'est la track qui choisit le melange,
    pas nous."""
    brut = json.loads("[" + sans_jinja(
        open(os.path.join(track, "operations", "default.json")).read()) + "]")
    termes = [l.strip() for l in open(os.path.join(track, "terms.txt"))]
    # Reproduit `geonames/track.py` (les param-sources de la track). Le terme
    # aleatoire qu'elle ajoute pour deborder les caches est remplace par un
    # terme fixe : ici les deux serveurs sont sans cache de requetes, et une
    # requete qui change a chaque tour ne se compare plus d'un serveur a
    # l'autre.
    fabriques = {
        "large_terms": {"query": {"terms": {"name.raw": termes + ["7"]}}},
        "large_filtered_terms": {"query": {"bool": {
            "must": [{"match": {"feature_class.raw": "T"}}],
            "filter": [{"terms": {"name.raw": termes + ["7"]}}]}}},
        "large_prohibited_terms": {"query": {"bool": {
            "must": [{"match": {"feature_class.raw": "A"}}],
            "must_not": [{"terms": {"name.raw": termes + ["7"]}}]}}},
    }
    ops = []
    for op in brut:
        if op.get("operation-type") != "search":
            continue
        nom = op["name"]
        corps = op.get("body")
        if corps is None:
            corps = fabriques.get(nom)
            if corps is None:
                sortir(f"operation `{nom}` sans corps et sans equivalent connu")
        ops.append({
            "nom": nom,
            "corps": corps,
            "params": op.get("request-params") or {},
            "pages": op.get("pages"),
            "par_page": op.get("results-per-page"),
        })
    return ops


# --------------------------------------------------------------------------
# 2. Le corpus
# --------------------------------------------------------------------------


def corpus_local():
    """Telecharge le corpus de la track une fois, et verifie sa taille exacte."""
    dossier = os.path.join(CACHE, TRACK["nom"])
    os.makedirs(dossier, exist_ok=True)
    chemin = os.path.join(dossier, TRACK["fichier"])
    if not os.path.exists(chemin) or os.path.getsize(chemin) != TRACK["octets_compresses"]:
        url = f"{TRACK['base_url']}/{TRACK['fichier']}"
        print(f"== telechargement du corpus ({TRACK['octets_compresses'] / 1e6:.0f} Mo, une fois)")
        print(f"   {url}")
        tmp = chemin + ".part"
        urllib.request.urlretrieve(url, tmp)
        os.replace(tmp, chemin)
    taille = os.path.getsize(chemin)
    if taille != TRACK["octets_compresses"]:
        sortir(f"corpus inattendu : {taille} octets au lieu de "
               f"{TRACK['octets_compresses']} — la track et le fichier ne "
               f"parlent plus du meme corpus")
    return chemin


def lots(chemin, n, taille_lot):
    """Les `n` premiers documents du corpus, en lots NDJSON prets pour `_bulk`.

    Rendus **en une liste, avant tout chronometre**, et pas en generateur : la
    decompression bz2 et la mise en forme JSON coutent des dizaines de secondes
    sur deux millions de documents, et un generateur consomme pendant
    l'indexation les ferait entrer dans le temps mesure — des deux cotes, donc
    invisibles au rapport, mais en ecrasant l'ecart entre les deux moteurs. La
    meme liste sert ensuite aux deux serveurs : ils recoivent les memes octets,
    sous les memes `_id` (le rang dans le fichier)."""
    tout, lot, i = [], [], 0
    with bz2.open(chemin, "rt") as f:
        for ligne in f:
            if i >= n:
                break
            lot.append(json.dumps({"index": {"_index": INDEX, "_id": str(i)}}))
            lot.append(ligne.strip())
            i += 1
            if len(lot) >= taille_lot * 2:
                tout.append("\n".join(lot) + "\n")
                lot = []
    if lot:
        tout.append("\n".join(lot) + "\n")
    if i < n:
        sortir(f"corpus epuise a {i} documents, {n} demandes")
    return tout


# --------------------------------------------------------------------------
# 3. Parler aux deux serveurs
# --------------------------------------------------------------------------


def appel(base, chemin, corps=None, methode=None, brut=False, timeout=1800):
    """Un appel HTTP, sans client : c'est le serveur qu'on mesure, pas la
    bibliotheque qui l'appelle."""
    data = None
    if corps is not None:
        data = corps.encode() if brut else json.dumps(corps).encode()
    req = urllib.request.Request(
        base + chemin, data=data, method=methode or ("POST" if data else "GET"),
        headers={"Content-Type": "application/x-ndjson" if brut
                 else "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            octets = r.read()
    except urllib.error.HTTPError as e:
        octets = e.read()
    except (urllib.error.URLError, OSError) as e:
        sortir(f"{base}{chemin} injoignable : {e}")
    try:
        return json.loads(octets or b"null")
    except ValueError:
        sortir(f"{base}{chemin} rend une reponse illisible : {octets[:200]!r}")


def version(base):
    r = appel(base, "/", timeout=10)
    if not isinstance(r, dict) or "version" not in r:
        sortir(f"{base} ne repond pas comme un Elasticsearch")
    return r["version"]["number"]


def indexe(base, ndjson, n, mapping, es, clients):
    """Cree l'index et y verse les lots deja prepares, avec `clients`
    connexions en parallele — ce que fait la track
    (`bulk_indexing_clients: 8`). Le chronometre ne couvre que les appels HTTP
    et le `_refresh` : la lecture du corpus est faite avant, une fois."""
    appel(base, f"/{INDEX}", methode="DELETE")
    m = json.loads(json.dumps(mapping))
    if es:
        m["settings"]["index"]["requests"] = {"cache": {"enable": False}}
    r = appel(base, f"/{INDEX}", m, methode="PUT")
    if not (r or {}).get("acknowledged"):
        # Sans ce garde-fou, un `_bulk` creerait l'index tout seul avec un
        # mapping dynamique : on mesurerait deux index differents sans le voir.
        sortir(f"{base} refuse le mapping de la track : {json.dumps(r)[:400]}")

    echecs = []

    def envoie(ndjson):
        r = appel(base, "/_bulk", ndjson, brut=True)
        if (r or {}).get("errors"):
            for item in r["items"]:
                erreur = list(item.values())[0].get("error")
                if erreur and len(echecs) < 5:
                    echecs.append(json.dumps(erreur)[:200])

    debut = time.perf_counter()
    with ThreadPoolExecutor(max_workers=clients) as ex:
        list(ex.map(envoie, ndjson))
    appel(base, f"/{INDEX}/_refresh", methode="POST")
    duree = time.perf_counter() - debut
    if echecs:
        sortir(f"{base} a refuse des documents : {echecs[0]}")
    compte = appel(base, f"/{INDEX}/_count").get("count")
    if compte != n:
        sortir(f"{base} porte {compte} documents au lieu de {n}")
    return duree


def attend_merges(base, limite_s=600):
    """Attend que les fusions de segments en cours soient finies.

    C'est ce que fait la track avant de mesurer (`wait-until-merges-finish` sur
    `_all.total.merges.current`). ferrite ne rend pas ce compteur — il refuse le
    groupe `merges` de `_stats` plutot que d'en rendre un zero, ce qui ferait
    passer « non mesure » pour « aucune fusion ». Sur un serveur qui ne le rend
    pas, l'attente est donc simplement sans objet."""
    debut = time.perf_counter()
    while time.perf_counter() - debut < limite_s:
        r = appel(base, "/_stats/merge")
        try:
            en_cours = r["_all"]["total"]["merges"]["current"]
        except (KeyError, TypeError):
            return None
        if en_cours == 0:
            return time.perf_counter() - debut
        time.sleep(1)
    sortir(f"{base} fusionne encore apres {limite_s} s")


def taille_index(base):
    r = appel(base, f"/{INDEX}/_stats/store")
    try:
        return r["_all"]["primaries"]["store"]["size_in_bytes"]
    except (KeyError, TypeError):
        return None


def rss_conteneur(nom):
    """RSS reel du serveur : la somme des `VmRSS` des processus du conteneur,
    lue depuis l'hote. `docker stats` compterait le cache de pages, ce qui
    ferait passer pour de la memoire du moteur ce qui est de la memoire du
    noyau."""
    if not nom:
        return None
    r = subprocess.run(["docker", "top", nom, "-eo", "pid"],
                       capture_output=True, text=True, check=False)
    if r.returncode:
        return None
    total = 0
    for ligne in r.stdout.splitlines()[1:]:
        pid = ligne.strip()
        if not pid.isdigit():
            continue
        try:
            with open(f"/proc/{pid}/status") as f:
                for l in f:
                    if l.startswith("VmRSS:"):
                        total += int(l.split()[1]) * 1024
        except OSError:
            pass
    return total or None


# --------------------------------------------------------------------------
# 4. Les mesures
# --------------------------------------------------------------------------


def chemin_de(op):
    params = "&".join(f"{k}={v}" for k, v in op["params"].items())
    return f"/{INDEX}/_search" + (f"?{params}" if params else "")


def joue(base, op):
    """Joue une operation une fois. Un `scroll` deroule ses pages, comme la
    track le demande — sinon on mesurerait l'ouverture, pas l'export."""
    if op["pages"]:
        corps = dict(op["corps"], size=op["par_page"])
        r = appel(base, f"/{INDEX}/_search?scroll=2m", corps)
        if "error" in (r or {}):
            return r
        sid, pages = r.get("_scroll_id"), 1
        while sid and pages < op["pages"]:
            r = appel(base, "/_search/scroll", {"scroll": "2m", "scroll_id": sid})
            if "error" in (r or {}) or not r["hits"]["hits"]:
                break
            sid, pages = r.get("_scroll_id"), pages + 1
        if sid:
            appel(base, "/_search/scroll", {"scroll_id": sid}, methode="DELETE")
        return r
    return appel(base, chemin_de(op), op["corps"])


def refus(r):
    """La phrase du refus, ou None si le serveur a repondu."""
    if isinstance(r, dict) and "error" in r:
        e = r["error"]
        if isinstance(e, dict):
            return f"{e.get('type', '?')}: {str(e.get('reason', ''))[:200]}"
        return str(e)[:200]
    return None


def total_de(r):
    t = (r.get("hits") or {}).get("total")
    if isinstance(t, dict):
        return (t.get("value"), t.get("relation"))
    return (t, "eq") if t is not None else (None, None)


def stats(mesures):
    m = sorted(mesures)
    out = {
        "n": len(m),
        "mediane_ms": statistics.median(m),
        "p95_ms": m[max(0, int(len(m) * 0.95) - 1)] if len(m) >= 20 else None,
        # Un p99 sur moins de 100 echantillons est le maximum deguise en
        # centile. On rend `null` plutot qu'un chiffre qui ne mesure rien.
        "p99_ms": m[max(0, int(len(m) * 0.99) - 1)] if len(m) >= 100 else None,
        "max_ms": m[-1],
    }
    return out


def latence(base, op, tours, chauffe, budget_s):
    for _ in range(chauffe):
        joue(base, op)
    mesures, debut = [], time.perf_counter()
    for _ in range(tours):
        t = time.perf_counter()
        joue(base, op)
        mesures.append((time.perf_counter() - t) * 1000)
        if time.perf_counter() - debut > budget_s:
            break
    return stats(mesures)


def debit(base, ops, secondes, en_vol):
    """Requetes par seconde avec `en_vol` requetes simultanees, en tournant sur
    le melange d'operations de la track."""
    fin = time.perf_counter() + secondes
    compte = [0]

    def boucle(i):
        k = i
        while time.perf_counter() < fin:
            joue(base, ops[k % len(ops)])
            k += en_vol
            compte[0] += 1

    debut = time.perf_counter()
    with ThreadPoolExecutor(max_workers=en_vol) as ex:
        list(ex.map(boucle, range(en_vol)))
    return compte[0] / (time.perf_counter() - debut)


_PERIMETRE = []


def perimetre_de(message):
    """Rattache un refus a une capacite declaree de `compat.yaml`, et dit s'il
    est un **cout de perimetre** (capacite declaree refusee) ou une
    **regression** (capacite declaree tenue). Un refus qu'aucune capacite ne
    reclame compte contre nous : c'est le troisieme verdict."""
    if not _PERIMETRE:
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        import perimetre as _p
        _PERIMETRE.append(_p.Perimetre())
    verdict, cid, _ = _PERIMETRE[0].verdict("search", message)
    return {"verdict": verdict, "capacite": cid}


# --------------------------------------------------------------------------
# 5. Le rapport
# --------------------------------------------------------------------------


def octets(n):
    if n is None:
        return "non mesure"
    for unite in ("o", "Ko", "Mo", "Go"):
        if n < 1024 or unite == "Go":
            return f"{n:.1f} {unite}" if unite != "o" else f"{n} o"
        n /= 1024
    return ""


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("ferrite", nargs="?", default="http://localhost:9200")
    p.add_argument("es", nargs="?", default="http://localhost:9201")
    p.add_argument("--docs", type=int, default=500000,
                   help="nombre de documents indexes (defaut : 500000)")
    p.add_argument("--tours", type=int, default=100,
                   help="iterations de latence par operation (defaut : 100)")
    p.add_argument("--chauffe", type=int, default=5)
    p.add_argument("--budget", type=float, default=60.0,
                   help="secondes au plus par operation et par serveur")
    p.add_argument("--debit-secondes", type=float, default=20.0)
    p.add_argument("--en-vol", type=int, default=8)
    p.add_argument("--clients", type=int, default=8,
                   help="connexions d'indexation en parallele (track : 8)")
    p.add_argument("--taille-lot", type=int, default=5000,
                   help="documents par `_bulk` (track : 5000)")
    p.add_argument("--ferrite-conteneur", default=None)
    p.add_argument("--es-conteneur", default=None)
    p.add_argument("--json", default=None, help="ecrit le rapport machine")
    p.add_argument("--inventaire", action="store_true",
                   help="imprime ce que la track demande, sans rien mesurer")
    p.add_argument("--sans-indexation", action="store_true",
                   help="reutilise l'index deja en place des deux cotes")
    args = p.parse_args()

    track = prepare_track()
    mapping = mapping_de_la_track(track)
    ops = operations_de_la_track(track)

    if args.inventaire:
        print(f"== track {TRACK['nom']} @ {TRACK['commit'][:12]} ({TRACK['licence']})")
        print(f"   corpus  : {TRACK['fichier']}, {TRACK['documents']} documents")
        print(f"   mapping : {len(mapping['mappings']['properties'])} champs")
        print(f"   requetes: {len(ops)}")
        for op in ops:
            print(f"     - {op['nom']}")
        print("\n== ecarts appliques a la track")
        for e in ECARTS_TRACK:
            print(f"   - {e['quoi']} ({e['cote']})\n     {e['pourquoi']}")
        return 0

    vf, ve = version(args.ferrite), version(args.es)
    if args.ferrite == args.es:
        sortir("les deux cibles sont le meme serveur")
    print(f"== ferrite {vf} ({args.ferrite})  vs  Elasticsearch {ve} ({args.es})")
    print(f"== track Rally `{TRACK['nom']}` @ {TRACK['commit'][:12]} "
          f"({TRACK['licence']}), {args.docs} documents\n")

    chemin_corpus = corpus_local()

    # -- indexation ---------------------------------------------------------
    if args.sans_indexation:
        tf = te = None
        for base in (args.ferrite, args.es):
            if appel(base, f"/{INDEX}/_count").get("count") != args.docs:
                sortir(f"{base} ne porte pas {args.docs} documents")
    else:
        print(f"-- lecture du corpus ({args.docs} documents, lots de "
              f"{args.taille_lot})")
        ndjson = lots(chemin_corpus, args.docs, args.taille_lot)
        print(f"   {len(ndjson)} lots prets\n")
        print(f"-- indexation ({args.clients} clients)")
        tf = indexe(args.ferrite, ndjson, args.docs, mapping, False, args.clients)
        print(f"   ferrite       : {tf:.1f} s ({args.docs / tf:,.0f} doc/s)")
        te = indexe(args.es, ndjson, args.docs, mapping, True, args.clients)
        print(f"   Elasticsearch : {te:.1f} s ({args.docs / te:,.0f} doc/s)")
        del ndjson

    # La track attend explicitement que les fusions de segments d'ES soient
    # finies avant de mesurer quoi que ce soit (`wait-until-merges-finish`).
    # Sans ca, les fusions d'apres-indexation d'Elasticsearch tourneraient
    # pendant qu'on chronometre ferrite — on mesurerait la machine, pas les
    # deux moteurs.
    attend_merges(args.es)

    disque_f, disque_e = taille_index(args.ferrite), taille_index(args.es)
    rss_f = rss_conteneur(args.ferrite_conteneur)
    rss_e = rss_conteneur(args.es_conteneur)

    # -- tri des operations : mesure, pas declaration -----------------------
    print(f"\n-- {len(ops)} operations de la track, posees a ferrite")
    jouables, refusees = [], []
    for op in ops:
        rf, re_ = joue(args.ferrite, op), joue(args.es, op)
        mf, me = refus(rf), refus(re_)
        if mf:
            refusees.append({"operation": op["nom"], "refus": mf,
                             "es_refuse_aussi": me,
                             "capacite": perimetre_de(mf)})
        elif me:
            # ES refuse ce que ferrite sert : la ligne existe, elle ne se
            # mesure pas (rien a comparer).
            refusees.append({"operation": op["nom"], "refus": None,
                             "es_refuse_aussi": me, "capacite": None})
        else:
            jouables.append(op)
    print(f"   {len(jouables)} jouables des deux cotes, {len(refusees)} non "
          f"(detail plus bas)")
    if not jouables:
        sortir("aucune operation jouable : il n'y a rien a mesurer")

    # -- memes resultats ? --------------------------------------------------
    resultats = []
    for op in jouables:
        rf, re_ = joue(args.ferrite, op), joue(args.es, op)
        tf_, _ = total_de(rf)
        te_, rele = total_de(re_)
        if rele == "gte":
            # ES s'arrete de compter a 10 000 (`track_total_hits` par defaut) ;
            # ferrite compte toujours tout. Le total n'est alors comparable que
            # par l'inegalite qu'ES annonce lui-meme.
            accord = tf_ is not None and tf_ >= te_
            note = f"ES s'arrete a {te_} (relation gte), ferrite compte {tf_}"
        else:
            accord = tf_ == te_
            note = None if accord else f"total {tf_} vs {te_}"
        aggs_f = json.dumps(rf.get("aggregations"), sort_keys=True) \
            if "aggregations" in rf else None
        aggs_e = json.dumps(re_.get("aggregations"), sort_keys=True) \
            if "aggregations" in re_ else None
        if aggs_f != aggs_e:
            accord = False
            note = "agregations differentes"
        resultats.append({"operation": op["nom"], "accord": accord, "note": note})

    # -- latence ------------------------------------------------------------
    print(f"\n-- latence ({args.tours} tours au plus par operation, "
          f"{args.budget:.0f} s de budget)")
    lat = {}
    for op in jouables:
        lat[op["nom"]] = {
            "ferrite": latence(args.ferrite, op, args.tours, args.chauffe, args.budget),
            "es": latence(args.es, op, args.tours, args.chauffe, args.budget),
        }
        f, e = lat[op["nom"]]["ferrite"], lat[op["nom"]]["es"]
        print(f"   {op['nom']:<42} {f['mediane_ms']:>9.2f} ms  "
              f"{e['mediane_ms']:>9.2f} ms   "
              f"x{e['mediane_ms'] / f['mediane_ms']:.2f}")

    # -- debit --------------------------------------------------------------
    print(f"\n-- debit ({args.en_vol} requetes en vol, "
          f"{args.debit_secondes:.0f} s par serveur)")
    df = debit(args.ferrite, jouables, args.debit_secondes, args.en_vol)
    de = debit(args.es, jouables, args.debit_secondes, args.en_vol)
    print(f"   ferrite {df:,.1f} req/s   Elasticsearch {de:,.1f} req/s   "
          f"x{df / de:.2f}")

    # -- ce que ferrite ne sait pas aller chercher ---------------------------
    # `_forcemerge` : ferrite n'a pas la route (cycle de vie d'index, hors
    # perimetre). On mesure quand meme ce que la ligne « taille sur disque »
    # deviendrait cote ES si on la lui appliquait. **Apres** les chronometres,
    # jamais avant : un index fusionne en un seul segment cherche plus vite, et
    # ES partirait alors avec un tour d'avance que la mesure ne dirait pas.
    disque_e_merge = None
    if not args.sans_indexation:
        r = appel(args.es, f"/{INDEX}/_forcemerge?max_num_segments=1", methode="POST")
        if "error" not in (r or {}):
            attend_merges(args.es)
            appel(args.es, f"/{INDEX}/_refresh", methode="POST")
            disque_e_merge = taille_index(args.es)

    # -- synthese -----------------------------------------------------------
    pool_f = [v for s in lat.values() for v in [s["ferrite"]["mediane_ms"]]]
    pool_e = [v for s in lat.values() for v in [s["es"]["mediane_ms"]]]
    gagne = [n for n, s in lat.items()
             if s["ferrite"]["mediane_ms"] < s["es"]["mediane_ms"]]
    perd = [n for n, s in lat.items()
            if s["ferrite"]["mediane_ms"] >= s["es"]["mediane_ms"]]

    print("\n== synthese")
    print(f"  {'':<30} {'ferrite':>16} {'Elasticsearch':>16}")
    if tf and te:
        print(f"  {'indexation (s)':<30} {tf:>16.1f} {te:>16.1f}   x{te / tf:.2f}")
        print(f"  {'indexation (doc/s)':<30} {args.docs / tf:>16,.0f} "
              f"{args.docs / te:>16,.0f}   x{(args.docs / tf) / (args.docs / te):.2f}")
    print(f"  {'taille sur disque':<30} {octets(disque_f):>16} "
          f"{octets(disque_e):>16}"
          + (f"   x{disque_e / disque_f:.2f}" if disque_f and disque_e else ""))
    if disque_e_merge:
        print(f"  {'   (ES apres _forcemerge)':<30} {'—':>16} "
              f"{octets(disque_e_merge):>16}"
              + (f"   x{disque_e_merge / disque_f:.2f}" if disque_f else ""))
    print(f"  {'RSS':<30} {octets(rss_f):>16} {octets(rss_e):>16}"
          + (f"   x{rss_e / rss_f:.2f}" if rss_f and rss_e else ""))
    print(f"  {'latence mediane des medianes':<30} "
          f"{statistics.median(pool_f):>16.2f} {statistics.median(pool_e):>16.2f}"
          f"   x{statistics.median(pool_e) / statistics.median(pool_f):.2f}")
    print(f"  {'debit (req/s)':<30} {df:>16,.1f} {de:>16,.1f}   x{df / de:.2f}")

    print(f"\n  ferrite plus rapide sur {len(gagne)}/{len(lat)} operations, "
          f"plus lent sur {len(perd)}")
    for n in perd:
        s = lat[n]
        print(f"    [perd] {n} : {s['ferrite']['mediane_ms']:.2f} ms vs "
              f"{s['es']['mediane_ms']:.2f} ms")

    desaccords = [r for r in resultats if not r["accord"]]
    print(f"\n  resultats : {len(resultats) - len(desaccords)}/{len(resultats)} "
          f"operations d'accord sur le total et les agregations")
    for r in desaccords:
        print(f"    [ecart] {r['operation']} : {r['note']}")

    print(f"\n  {len(refusees)} operations de la track hors mesure :")
    for r in refusees:
        if r["refus"]:
            c = r["capacite"] or {}
            cap = f" [{c.get('verdict')} / {c.get('capacite') or 'aucune capacite'}]"
            print(f"    [refus ferrite]{cap} {r['operation']} : {r['refus'][:110]}")
        else:
            print(f"    [refus ES] {r['operation']} : {r['es_refuse_aussi'][:110]}")

    rapport = {
        "track": dict(TRACK, chemin=None),
        "ecarts_track": ECARTS_TRACK,
        "documents": args.docs,
        "serveurs": {"ferrite": vf, "elasticsearch": ve},
        "reglages": {"clients_indexation": args.clients,
                     "taille_lot": args.taille_lot,
                     "tours": args.tours, "chauffe": args.chauffe,
                     "budget_s": args.budget, "en_vol": args.en_vol,
                     "debit_secondes": args.debit_secondes},
        "indexation_s": {"ferrite": tf, "elasticsearch": te},
        "disque_octets": {"ferrite": disque_f, "elasticsearch": disque_e,
                          "elasticsearch_apres_forcemerge": disque_e_merge},
        "rss_octets": {"ferrite": rss_f, "elasticsearch": rss_e},
        "debit_req_s": {"ferrite": df, "elasticsearch": de},
        "latence": lat,
        "resultats": resultats,
        "hors_mesure": refusees,
    }
    if args.json:
        # Le fichier porte **une entree par echelle** : deux passages a deux
        # tailles se completent au lieu de s'ecraser. Une seule echelle
        # publiee ne dit pas comment les chiffres bougent avec la taille, et
        # c'est precisement ce que ce banc doit dire.
        doc = {}
        if os.path.exists(args.json):
            try:
                with open(args.json) as f:
                    doc = json.load(f)
            except ValueError:
                sortir(f"{args.json} existe mais n'est pas lisible")
        doc[f"{TRACK['nom']}-{args.docs}"] = rapport
        with open(args.json, "w") as f:
            json.dump(doc, f, indent=2, ensure_ascii=False, sort_keys=True)
            f.write("\n")
        print(f"\n  rapport ecrit dans {args.json} "
              f"(cle `{TRACK['nom']}-{args.docs}`)")

    return 1 if desaccords else 0


if __name__ == "__main__":
    sys.exit(main())
