#!/usr/bin/env python3
"""Brancher une **vraie application** open source sur ferrite, et rejouer sa
propre suite d'integration.

    python3 tests/compat/appli_reelle.py --liste
    python3 tests/compat/appli_reelle.py gitea
    python3 tests/compat/appli_reelle.py wagtail --json docs/application.json
    python3 tests/compat/appli_reelle.py gitea --sans-etalonnage   # ferrite seul

# Pourquoi

Toutes les autres mesures du depot exercent une **surface d'API** : la suite
d'Elastic, le corpus d'usage, le fuzzer. Aucune ne repond a la question qui
decide du produit — *un logiciel ecrit par quelqu'un d'autre, qui n'a jamais
entendu parler de ferrite, demarre-t-il ?* Un test qu'on ecrit soi-meme porte
la meme idee fausse que le code qu'il teste ; le test d'un autre, non.

La reponse ne vaut qu'a trois conditions, et ce fichier les tient toutes les
trois :

1. **l'application n'est pas modifiee.** Le depot est cloné a une revision
   figee, et l'outil refuse de mesurer si un fichier suivi a bouge. « Ca passe
   apres deux retouches » ne prouve rien ;
2. **l'instrument est etalonne.** La meme suite est d'abord lancee contre un
   vrai Elasticsearch 8.15. Une suite rouge contre les deux serveurs ne dit
   rien de ferrite — c'est l'environnement. Le rapport publie les deux
   colonnes, jamais celle de ferrite seule ;
3. **le trafic est releve.** Un mouchard s'interpose entre l'application et le
   serveur et journalise chaque requete. C'est ce qui transforme « le test
   `Keyword` echoue » en « `POST /{index}/_search`, clause `multi_match`,
   refusee par la capacite `dsl.multi_match` » — et c'est aussi la seule facon
   de savoir ce qu'une application envoie **vraiment**, plutot que ce que son
   code source donne a lire.

# Ce que l'outil rend

Pour chaque cas de la suite : son verdict contre ES, son verdict contre
ferrite, et quand ils different, la requete qui les separe. Chaque refus de
ferrite releve dans le journal est rattache a une capacite de `compat.yaml`
(via [`perimetre.py`](perimetre.py)), ce qui le classe **cout de perimetre**
(la capacite est declaree refusee) ou **regression** (elle est declaree tenue).

# Ce qu'il exige

Docker — l'application tourne dans l'image de son propre ecosysteme, pas dans
le worker. C'est un outil de developpement, pas de CI.
"""
import argparse
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import threading
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import perimetre as mod_perimetre  # noqa: E402
import ponderation  # noqa: E402

RACINE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CLONES = os.path.join(RACINE, ".appli-reelle")

FERRITE = "http://127.0.0.1:9200"
ES = "http://127.0.0.1:9201"


# ===========================================================================
# 1. Les applications
# ===========================================================================
#
# Une entree par application, avec la revision **figee** : une mesure sur
# « la derniere version » n'est pas rejouable. `pourquoi` et `ecarte` sont
# publies dans le rapport — le choix d'une cible se defend, il ne se constate
# pas.

APPLICATIONS = {
    "gitea": {
        "nom": "Gitea",
        "version": "v1.27.2",
        "commit": "1dac1bb2f8593d4319125fa6bca9283000a2ddc2",
        "depot": "https://github.com/go-gitea/gitea",
        "licence": "MIT",
        "quoi": "forge Git ; Elasticsearch sert la recherche d'issues et de PR",
        "suite": "go test ./modules/indexer/issues/elasticsearch/...",
        "image": "golang:1.26",
        "commande": [
            "go",
            "test",
            "./modules/indexer/issues/elasticsearch/...",
            "-run",
            "TestElasticsearchIndexer",
            "-v",
            "-count=1",
        ],
        "variable": "TEST_ELASTICSEARCH_URL",
        "caches": {"/root/.cache/go-build": "go-build", "/go/pkg/mod": "go-mod"},
        "parseur": "go",
        "pourquoi": (
            "recherche (pas du log), suite d'integration maison pilotee par une "
            "variable d'environnement, aucune autre brique de la stack Elastic, "
            "et un profil de requetes identique au sous-corpus `github` de "
            "l'etude d'usage : bool, multi_match, term, terms, range, sort"
        ),
    },
    "wagtail": {
        "nom": "Wagtail",
        "version": "v7.1",
        "commit": "cf8c53ea06a1dea16fee8c2ca1cf76aa37e5e322",
        "depot": "https://github.com/wagtail/wagtail",
        "licence": "BSD-3-Clause",
        "quoi": "CMS Django ; Elasticsearch sert la recherche de pages, d'images et de documents",
        "suite": "python runtests.py --elasticsearch8 wagtail.search",
        "image": "python:3.12",
        # `Django<6` et `django-tasks<0.9` : Wagtail 7.1 est sorti avant
        # Django 6, et pip installerait la derniere version de tout. C'est une
        # derive de dependances, sans rapport avec ferrite — mais la taire
        # rendrait la recette injouable.
        "commande": [
            "bash",
            "-c",
            "pip install --quiet -e '.[testing]' 'Django>=5.2,<6' 'elasticsearch>=8,<9' && "
            "python runtests.py --elasticsearch8 wagtail.search -v 2",
        ],
        "variable": "ELASTICSEARCH_URL",
        "caches": {"/root/.cache/pip": "pip"},
        "parseur": "django",
        "pourquoi": (
            "l'autre bout du spectre : un CMS qui passe par le client officiel "
            "elasticsearch-py 8.x, pose ses propres analyzers et fait tourner "
            "sa suite de backend complete contre le serveur"
        ),
    },
}

# Les candidats ecartes, et la raison — mesurable, pas d'humeur. Un choix de
# cible arrange pour que ca passe ne prouverait rien ; ces lignes disent ce
# qu'on n'a pas mesure et pourquoi.
ECARTES = [
    {
        "nom": "ReadTheDocs (readthedocs.org)",
        "raison": (
            "sa recherche est batie sur `highlight` et `inner_hits` "
            "(readthedocs/search/faceted_search.py) : les deux sont declares "
            "refuses, et sans eux la suite ne mesurerait que l'absence d'extraits"
        ),
    },
    {
        "nom": "Open Food Facts search-a-licious",
        "raison": (
            "ses index reposent sur des filtres `synonym` (245 occurrences) que "
            "ferrite ne compose pas : l'index ne se creerait pas"
        ),
    },
    {
        "nom": "Zammad, Mastodon",
        "raison": (
            "exigent Postgres + Redis + une stack Ruby complete pour lancer un "
            "test ; le critere de la carte est « n'exige pas la moitie de la "
            "stack », et le temps passe a monter l'environnement ne mesure rien"
        ),
    },
    {
        "nom": "Graylog, Jaeger, elastalert2",
        "raison": (
            "du log a l'echelle : rotation d'index, ILM, `_msearch`. Ils "
            "mesureraient l'exploitation d'un cluster, pas la recherche"
        ),
    },
]


# ===========================================================================
# 2. Le mouchard
# ===========================================================================


class Mouchard:
    """Un relais HTTP qui journalise ce que l'application envoie.

    Sans lui, un echec de la suite ne dit que son nom. Avec lui, on a la
    requete exacte, le statut rendu et le message d'erreur — donc de quoi
    rattacher l'echec a une capacite declaree.
    """

    def __init__(self, cible):
        self.cible = cible.rstrip("/")
        self.journal = []
        self.verrou = threading.Lock()
        mouchard = self

        class Relais(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_):  # silence
                pass

            def handle_one_request(self):
                # Un client qui ferme sa connexion (fin de suite, timeout de
                # pool) leve ici une trace qui n'apprend rien : elle noierait
                # la sortie de la suite mesuree.
                try:
                    super().handle_one_request()
                except (ConnectionResetError, BrokenPipeError):
                    self.close_connection = True

            def _relaie(self):
                taille = int(self.headers.get("Content-Length") or 0)
                corps = self.rfile.read(taille) if taille else None
                entetes = {
                    k: v
                    for k, v in self.headers.items()
                    # `accept-encoding` est retire pour que le corps de la
                    # reponse reste lisible : un journal gzippe ne se
                    # rattache a rien.
                    if k.lower() not in ("host", "accept-encoding", "connection")
                }
                req = urllib.request.Request(
                    mouchard.cible + self.path,
                    data=corps,
                    headers=entetes,
                    method=self.command,
                )
                try:
                    with urllib.request.urlopen(req, timeout=120) as r:
                        statut, reponse, entetes_rep = r.status, r.read(), dict(r.headers)
                except urllib.error.HTTPError as e:
                    statut, reponse, entetes_rep = e.code, e.read(), dict(e.headers)
                except OSError as e:
                    statut, reponse, entetes_rep = 502, str(e).encode(), {}
                mouchard.note(self.command, self.path, corps, statut, reponse)
                self.send_response(statut)
                for k, v in entetes_rep.items():
                    if k.lower() in ("content-length", "transfer-encoding", "connection"):
                        continue
                    self.send_header(k, v)
                self.send_header("Content-Length", str(len(reponse)))
                self.end_headers()
                self.wfile.write(reponse)

            do_GET = do_PUT = do_POST = do_DELETE = do_HEAD = do_OPTIONS = _relaie

        self.serveur = ThreadingHTTPServer(("127.0.0.1", 0), Relais)
        self.port = self.serveur.server_address[1]
        self.fil = threading.Thread(target=self.serveur.serve_forever, daemon=True)

    @property
    def url(self):
        return f"http://127.0.0.1:{self.port}"

    def note(self, methode, chemin, corps, statut, reponse):
        entree = {
            "methode": methode,
            "chemin": chemin,
            "api": api_de(methode, chemin),
            "statut": statut,
        }
        if corps:
            entree["corps"] = decode(corps)
        if statut >= 400:
            entree["erreur"] = erreur_de(reponse)
        with self.verrou:
            self.journal.append(entree)

    def __enter__(self):
        self.fil.start()
        return self

    def __exit__(self, *_):
        self.serveur.shutdown()


def decode(brut):
    """Le corps d'une requete, en JSON si c'en est, en texte sinon.

    Un `_bulk` n'est pas un objet JSON mais une suite de lignes : le rendre en
    liste garde le journal exploitable sans mentir sur sa forme.
    """
    texte = brut.decode("utf-8", "replace")
    try:
        return json.loads(texte)
    except json.JSONDecodeError:
        lignes = []
        for ligne in texte.splitlines():
            if not ligne.strip():
                continue
            try:
                lignes.append(json.loads(ligne))
            except json.JSONDecodeError:
                lignes.append(ligne)
        return lignes


def erreur_de(reponse):
    try:
        corps = json.loads(reponse.decode("utf-8", "replace"))
    except (json.JSONDecodeError, AttributeError):
        return reponse.decode("utf-8", "replace")[:300]
    err = corps.get("error")
    if isinstance(err, dict):
        return f"{err.get('type')}: {err.get('reason')}"
    return str(err)[:300]


# Les routes qu'une application envoie, ramenees au nom d'API que `compat.yaml`
# et la suite d'Elastic emploient. Une route inconnue est rendue telle quelle
# (`?/chemin`) plutot que rangee de force : un rattachement invente vaudrait
# moins que rien.
def api_de(methode, chemin):
    voie = chemin.split("?")[0].strip("/")
    parts = [p for p in voie.split("/") if p]
    if not parts:
        return "info"
    dernier = parts[-1]
    fixes = {
        "_bulk": "bulk",
        "_search": "search",
        "_count": "count",
        "_refresh": "indices.refresh",
        "_flush": "indices.flush",
        "_mapping": "indices.get_mapping" if methode == "GET" else "indices.put_mapping",
        "_settings": "indices.get_settings" if methode == "GET" else "indices.put_settings",
        "_alias": "indices.get_alias" if methode == "GET" else "indices.put_alias",
        "_aliases": "indices.update_aliases",
        "_stats": "indices.stats",
        "_delete_by_query": "delete_by_query",
        "_update_by_query": "update_by_query",
        "_msearch": "msearch",
        "_mget": "mget",
        "_field_caps": "field_caps",
        "_analyze": "indices.analyze",
    }
    if dernier in fixes:
        return fixes[dernier]
    if parts[0] == "_cluster":
        return "cluster." + (parts[1] if len(parts) > 1 else "?")
    if parts[0] == "_cat":
        return "cat." + (parts[1] if len(parts) > 1 else "?")
    if parts[0] == "_search" and len(parts) > 1 and parts[1] == "scroll":
        return "scroll"
    if "_doc" in parts or "_create" in parts:
        if methode == "GET":
            return "get"
        if methode == "DELETE":
            return "delete"
        return "index"
    if "_update" in parts:
        return "update"
    if len(parts) == 1 and not parts[0].startswith("_"):
        return {
            "PUT": "indices.create",
            "DELETE": "indices.delete",
            "GET": "indices.get",
            "HEAD": "indices.exists",
        }.get(methode, "?" + chemin)
    if len(parts) == 2 and parts[1] == "_doc" and methode == "POST":
        return "index"
    return "?" + voie


# ===========================================================================
# 3. Le clone, fige et non modifie
# ===========================================================================


def git(*args, cwd=None):
    return subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )


def prepare(app, cle):
    """Clone l'application a sa revision figee, et **verifie** qu'elle n'a pas
    bouge. C'est la preuve centrale de la mesure : « sans modifier son code »
    se constate, il ne se promet pas."""
    chemin = os.path.join(CLONES, cle)
    if not os.path.isdir(os.path.join(chemin, ".git")):
        os.makedirs(CLONES, exist_ok=True)
        print(f"== clonage de {app['nom']} {app['version']} (une fois)")
        r = git("clone", "--filter=blob:none", "--no-checkout", app["depot"], chemin)
        if r.returncode:
            sortir(f"clonage impossible : {r.stderr.strip()}")
    r = git("fetch", "--depth", "1", "origin", app["commit"], cwd=chemin)
    if r.returncode:
        git("fetch", "--tags", "origin", cwd=chemin)
    r = git("checkout", "--force", app["commit"], cwd=chemin)
    if r.returncode:
        sortir(f"revision {app['commit']} introuvable : {r.stderr.strip()}")
    tete = git("rev-parse", "HEAD", cwd=chemin).stdout.strip()
    if tete != app["commit"]:
        sortir(f"revision inattendue : {tete} au lieu de {app['commit']}")
    return chemin


def intact(chemin):
    """Aucun fichier **suivi** modifie. Les fichiers non suivis (`.egg-info`,
    binaires de build) sont tolares : ils sont le residu de l'installation, pas
    une retouche du code."""
    return git("diff", "--stat", cwd=chemin).stdout.strip() == ""


# ===========================================================================
# 4. Lancer la suite
# ===========================================================================


def index_de(url):
    """Les index presents sur un serveur, ou `None` s'il ne repond pas."""
    try:
        with urllib.request.urlopen(url.rstrip("/") + "/_cat/indices?format=json", timeout=20) as r:
            return {ligne["index"] for ligne in json.load(r)}
    except (urllib.error.URLError, OSError, ValueError):
        return None


def nettoie(url, avant):
    """Supprime les index que la suite a laisses derriere elle.

    La suite d'une application ne range pas forcement : celle de Gitea cree un
    index horodate et ne le supprime jamais. Laisser cet etat sur le serveur de
    reference est exactement le defaut d'outillage qui a fait dérailler une
    campagne de fuzzing entiere — un passage suivant le retrouve et n'en sait
    rien. Seuls les index **apparus pendant** la mesure sont touches.
    """
    if avant is None:
        return []
    apres = index_de(url)
    if apres is None:
        return []
    nouveaux = sorted(apres - avant)
    for nom in nouveaux:
        req = urllib.request.Request(url.rstrip("/") + "/" + nom, method="DELETE")
        try:
            urllib.request.urlopen(req, timeout=30).close()
        except (urllib.error.URLError, OSError):
            pass
    return nouveaux


def execute(app, chemin, url_cible, silencieux=False):
    """Lance la suite de l'application contre `url_cible`, a travers le
    mouchard. Rend (sortie, cas, journal)."""
    avant = index_de(url_cible)
    with Mouchard(url_cible) as mouchard:
        env = dict(app.get("env") or {})
        env[app["variable"]] = mouchard.url
        cmd = [
            "docker",
            "run",
            "--rm",
            "--network",
            "host",
            "-v",
            f"{chemin}:/src",
            "-w",
            "/src",
        ]
        for interne, nom in (app.get("caches") or {}).items():
            cache = os.path.join(CLONES, ".cache-" + nom)
            os.makedirs(cache, exist_ok=True)
            cmd += ["-v", f"{cache}:{interne}"]
        for k, v in env.items():
            cmd += ["-e", f"{k}={v}"]
        cmd += [app["image"], *app["commande"]]
        proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    sortie = proc.stdout + proc.stderr
    if not silencieux:
        print(sortie[-4000:] if len(sortie) > 4000 else sortie)
    laisses = nettoie(url_cible, avant)
    if laisses:
        print(f"   ({len(laisses)} index laisses par la suite, supprimes)")
    return sortie, PARSEURS[app["parseur"]](sortie), mouchard.journal


def parse_go(sortie):
    """`go test -v` : un sous-test par ligne `--- PASS: Suite/cas`."""
    cas = {}
    for m in re.finditer(r"^\s*--- (PASS|FAIL|SKIP): (\S+)", sortie, re.M):
        verdict, nom = m.group(1), m.group(2)
        if "/" not in nom:  # le test parent, pas un cas
            continue
        cas[nom.split("/", 1)[1]] = verdict
    if not cas:
        # La suite n'a pas demarre du tout (echec d'`Init`, serveur muet…) :
        # un dictionnaire vide serait lu comme « 0 cas, 0 echec ».
        m = re.search(r"^\s*--- FAIL: (\S+)", sortie, re.M)
        if m:
            cas[m.group(1)] = "FAIL"
    return cas


def parse_django(sortie):
    """`manage.py test -v 2` : `nom (chemin) ... ok | FAIL | ERROR | skipped`."""
    cas = {}
    for m in re.finditer(
        r"^(\w+) \(([\w.]+)\)(?:[^\n]*?)? \.\.\. (ok|FAIL|ERROR|skipped.*)$", sortie, re.M
    ):
        nom = f"{m.group(2)}.{m.group(1)}"
        verdict = {"ok": "PASS", "FAIL": "FAIL", "ERROR": "FAIL"}.get(m.group(3), "SKIP")
        cas[nom] = verdict
    if not cas:
        for m in re.finditer(r"^(?:FAIL|ERROR): (\w+) \(([\w.]+)\)", sortie, re.M):
            cas[f"{m.group(2)}.{m.group(1)}"] = "FAIL"
    return cas


PARSEURS = {"go": parse_go, "django": parse_django}


# ===========================================================================
# 5. Classer
# ===========================================================================


# Les erreurs qu'Elasticsearch rend lui-meme sur un etat donne : elles ne
# disent rien du perimetre de ferrite. Wagtail supprime son index avant de le
# recreer ; quand la creation a echoue, les 87 suppressions suivantes rendent
# `index_not_found_exception` — un ES a qui on demanderait la meme chose dans
# le meme etat rendrait exactement ca. Les compter comme des refus ferait de
# **une** cause 174 lignes de rapport, et gonflerait le decompte des manques.
ERREURS_D_ETAT = {
    "index_not_found_exception",
    "resource_already_exists_exception",
    "version_conflict_engine_exception",
    "document_missing_exception",
}


def classe_refus(journal, journal_es, perim):
    """Ce que ferrite a refuse **et qu'ES accepte**, rattache a une capacite.

    Le predicat compte : un statut >= 400 n'est pas un refus en soi. Le
    `HEAD /{index}` d'une application qui teste l'existence de son index rend
    404 chez les deux serveurs — c'est une reponse, pas un manque. N'est donc
    retenue que la forme (route, methode, statut) qu'ES **ne rend pas** sur la
    meme suite. Sans etalonnage (`journal_es` vide), tout echec est retenu et
    le rapport dit que le tri n'a pas eu lieu.

    Le verdict vient ensuite de `perimetre.py`, le meme que celui du rapport de
    conformance : `cout_perimetre` si la capacite est declaree refusee,
    `regression` si elle est declaree tenue, `indetermine` si aucune ne la
    reclame — et l'indetermine compte **contre** nous.
    """
    formes_es = {
        (e["api"], e["methode"], e["statut"]) for e in journal_es if e["statut"] >= 400
    }
    refus = {}
    for e in journal:
        if e["statut"] < 400:
            continue
        if (e["api"], e["methode"], e["statut"]) in formes_es:
            continue
        raison = e.get("erreur") or ""
        genre = "etat" if raison.split(":")[0] in ERREURS_D_ETAT else "refus"
        verdict, cap, _ = perim.verdict(e["api"], raison)
        if genre == "etat":
            verdict, cap = "consequence_d_etat", None
        clef = (e["api"], raison)
        entree = refus.setdefault(
            clef,
            {
                "api": e["api"],
                "methode": e["methode"],
                "erreur": raison,
                "statut": e["statut"],
                "genre": genre,
                "verdict": verdict,
                "capacite": cap,
                "combien": 0,
                "exemple": e.get("corps"),
            },
        )
        entree["combien"] += 1
    return sorted(refus.values(), key=lambda r: -r["combien"])


def trafic(journal, croisement):
    """Ce que l'application envoie vraiment, par route — et ce que le perimetre
    declare en dit. Le code source d'une application ne suffit pas a le savoir :
    un client construit ses requetes, il ne les recopie pas."""
    par_api = {}
    for e in journal:
        stat = par_api.setdefault(
            e["api"],
            {"api": e["api"], "requetes": 0, "traits": set(), "echecs": 0, "exemple": None},
        )
        stat["requetes"] += 1
        if stat["exemple"] is None and e.get("corps") is not None:
            stat["exemple"] = e["corps"]
        if e["statut"] >= 400:
            stat["echecs"] += 1
        corps = e.get("corps")
        requete = {"api": e["api"], "chemin": e["chemin"]}
        if isinstance(corps, dict):
            requete["corps"] = corps
        elif isinstance(corps, list):
            # Un `_bulk` : ses lignes ne sont pas un corps de recherche, mais
            # `traits()` sait lire celles qui en portent un.
            requete["corps_lignes"] = corps
        stat["traits"] |= ponderation.traits(requete)
    sortie = []
    for stat in sorted(par_api.values(), key=lambda s: -s["requetes"]):
        verdicts = {t: croisement.verdict(t) for t in stat["traits"]}
        sortie.append(
            {
                "api": stat["api"],
                "requetes": stat["requetes"],
                "echecs": stat["echecs"],
                "traits": sorted(stat["traits"]),
                "traits_refuses": sorted(
                    t for t, (v, _) in verdicts.items() if v == ponderation.REFUSE
                ),
                "traits_indetermines": sorted(
                    t for t, (v, _) in verdicts.items() if v == ponderation.INDETERMINE
                ),
                "exemple": stat["exemple"],
            }
        )
    return sortie


def compare(cas_es, cas_ferrite):
    """Les deux colonnes, cas par cas. Un cas rouge des deux cotes ne dit rien
    de ferrite : il sort du denominateur, et le rapport le dit."""
    noms = sorted(set(cas_es) | set(cas_ferrite))
    lignes = []
    for nom in noms:
        e, f = cas_es.get(nom, "ABSENT"), cas_ferrite.get(nom, "ABSENT")
        if e == f:
            etat = "identique"
        elif e == "PASS" and f == "FAIL":
            etat = "ecart"
        elif e == "FAIL" and f == "PASS":
            etat = "inverse"
        else:
            etat = "different"
        lignes.append({"cas": nom, "es": e, "ferrite": f, "etat": etat})
    return lignes


# ===========================================================================
# 6. Rapport
# ===========================================================================


def imprime(rapport):
    a = rapport["application"]
    print()
    print(f"== {a['nom']} {a['version']} ({a['commit'][:12]}) — {a['quoi']}")
    print(f"   suite   : {a['suite']}")
    print(f"   arbre non modifie : {'oui' if rapport['intact'] else 'NON'}")
    print()
    for cible in ("es", "ferrite"):
        r = rapport["executions"].get(cible)
        if not r:
            print(f"   {cible:8} : non lance")
            continue
        print(
            f"   {cible:8} : {r['passes']}/{r['total']} cas verts"
            + (f"  ({r['echecs']} rouges)" if r["echecs"] else "")
        )
    if rapport["comparaison"]:
        ecarts = [c for c in rapport["comparaison"] if c["etat"] != "identique"]
        print()
        if not ecarts:
            print("   aucun ecart : ferrite rend le meme verdict qu'ES sur chaque cas")
        else:
            print(f"   {len(ecarts)} cas ou les deux serveurs different :")
            for c in ecarts:
                print(f"     {c['cas']:50} ES={c['es']:6} ferrite={c['ferrite']}")
    print()
    print("   ce que l'application envoie (releve du mouchard) :")
    for t in rapport["trafic"]:
        refus = (
            "  traits refuses : " + ", ".join(t["traits_refuses"]) if t["traits_refuses"] else ""
        )
        echecs = f"  dont {t['echecs']} en erreur" if t["echecs"] else ""
        print(f"     {t['api']:24} {t['requetes']:5} requetes{echecs}{refus}")
    print()
    if not rapport["etalonne"]:
        print("   (sans etalonnage : les erreurs qu'ES rend aussi n'ont pas ete ecartees)")
    vrais = [r for r in rapport["refus"] if r["genre"] == "refus"]
    etats = [r for r in rapport["refus"] if r["genre"] == "etat"]
    if vrais:
        print("   ce que ferrite refuse et qu'ES sert :")
        for r in vrais:
            print(f"     [{r['verdict']}] {r['api']} ×{r['combien']} — {r['erreur'][:140]}")
            if r["capacite"]:
                print(f"        capacite : {r['capacite']}")
    else:
        print("   ce que ferrite refuse et qu'ES sert : rien")
    for r in etats:
        print(
            f"   (consequence : {r['api']} ×{r['combien']} — {r['erreur'][:90]} ; "
            "un ES dans le meme etat rendrait la meme chose)"
        )
    print()


def sortir(message):
    print(f"ERREUR : {message}", file=sys.stderr)
    sys.exit(2)


def joignable(url):
    try:
        with urllib.request.urlopen(url, timeout=3) as r:
            return r.status == 200
    except (urllib.error.URLError, OSError, socket.timeout):
        return False


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("application", nargs="?", choices=sorted(APPLICATIONS))
    p.add_argument("--ferrite", default=FERRITE)
    p.add_argument("--es", default=ES)
    p.add_argument(
        "--sans-etalonnage",
        action="store_true",
        help="ne lance pas la suite contre le vrai Elasticsearch (le rapport le dit)",
    )
    p.add_argument("--json", metavar="FICHIER", help="ecrit le rapport machine")
    p.add_argument("--liste", action="store_true", help="les applications et les ecartees")
    args = p.parse_args()

    if args.liste or not args.application:
        print("Applications mesurees :")
        for cle, app in APPLICATIONS.items():
            print(f"  {cle:10} {app['nom']} {app['version']} — {app['quoi']}")
            print(f"             {app['pourquoi']}")
        print("\nCandidats ecartes :")
        for e in ECARTES:
            print(f"  {e['nom']} — {e['raison']}")
        return 0

    if not shutil.which("docker"):
        sortir("docker est necessaire : l'application tourne dans son propre ecosysteme")
    if not joignable(args.ferrite):
        sortir(f"ferrite ne repond pas sur {args.ferrite}")
    if not args.sans_etalonnage and not joignable(args.es):
        sortir(
            f"le vrai Elasticsearch ne repond pas sur {args.es}. Une suite d'application "
            "rouge contre les deux serveurs ne dit rien de ferrite : l'etalonnage n'est "
            "pas optionnel (--sans-etalonnage pour l'assumer explicitement)"
        )

    app = APPLICATIONS[args.application]
    chemin = prepare(app, args.application)

    executions, journaux = {}, {}
    if not args.sans_etalonnage:
        print(f"== {app['nom']} contre un vrai Elasticsearch ({args.es})")
        sortie, cas, journal = execute(app, chemin, args.es, silencieux=True)
        executions["es"] = resume(cas, sortie)
        journaux["es"] = journal
        print(f"   {executions['es']['passes']}/{executions['es']['total']}")
    print(f"== {app['nom']} contre ferrite ({args.ferrite})")
    sortie, cas, journal = execute(app, chemin, args.ferrite, silencieux=True)
    executions["ferrite"] = resume(cas, sortie)
    journaux["ferrite"] = journal
    print(f"   {executions['ferrite']['passes']}/{executions['ferrite']['total']}")

    croisement = ponderation.Croisement()
    perim = croisement.perimetre
    rapport = {
        "application": {k: app[k] for k in ("nom", "version", "commit", "depot", "licence", "quoi", "suite", "pourquoi")},
        "intact": intact(chemin),
        "etalonne": not args.sans_etalonnage,
        "executions": executions,
        "comparaison": compare(executions.get("es", {}).get("cas", {}), executions["ferrite"]["cas"])
        if "es" in executions
        else [],
        "trafic": trafic(journaux["ferrite"], croisement),
        "refus": classe_refus(journaux["ferrite"], journaux.get("es") or [], perim),
        "ecartes": ECARTES,
    }
    imprime(rapport)
    if args.json:
        # `comparaison` porte deja le verdict des deux cotes, cas par cas :
        # republier les deux tables de verdicts triplerait le fichier sans
        # ajouter une information.
        if rapport["comparaison"]:
            for r in rapport["executions"].values():
                r.pop("cas", None)
        # Le rapport est **par application**, dans un seul fichier : mesurer
        # une cible n'efface pas ce qu'on sait de l'autre — le resultat negatif
        # documente vaut autant que le positif.
        doc = {}
        if os.path.exists(args.json):
            with open(args.json, encoding="utf-8") as f:
                doc = json.load(f)
        doc[args.application] = rapport
        with open(args.json, "w", encoding="utf-8") as f:
            json.dump(doc, f, ensure_ascii=False, indent=1, sort_keys=True)
            f.write("\n")
        print(f"   rapport ecrit dans {args.json}")

    if not rapport["intact"]:
        sortir("l'arbre de l'application a ete modifie : la mesure ne vaut rien")
    ecarts = [c for c in rapport["comparaison"] if c["etat"] == "ecart"]
    return 1 if ecarts or executions["ferrite"]["echecs"] else 0


def resume(cas, sortie):
    return {
        "cas": cas,
        "total": len(cas),
        "passes": sum(1 for v in cas.values() if v == "PASS"),
        "echecs": sum(1 for v in cas.values() if v == "FAIL"),
        "ignores": sum(1 for v in cas.values() if v == "SKIP"),
        "queue": sortie[-1500:],
    }


if __name__ == "__main__":
    sys.exit(main())
