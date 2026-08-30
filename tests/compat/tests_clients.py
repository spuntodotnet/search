#!/usr/bin/env python3
"""Faire tourner la **suite de tests des clients officiels** contre ferrite.

    python3 tests/compat/tests_clients.py --liste
    python3 tests/compat/tests_clients.py python
    python3 tests/compat/tests_clients.py go --cycle          # la batterie seule
    python3 tests/compat/tests_clients.py javascript --json docs/clients.json

# Pourquoi celle-ci, alors qu'il y en a deja cinq

La page produit annonce « les clients officiels, sans modification ». Cinq
mesures du depot exercent deja ce que ferrite repond : le harnais maison
(`run.sh`), les deux suites REST rejouees par **notre** runner
(`conformance_es.py`), le corpus d'usage, le fuzzer. Aucune n'exerce le
**client** : elles passent toutes par un client, mais c'est nous qui ecrivons
ce qu'on lui demande.

Ici, ce sont les tests que l'equipe du client a ecrits pour son client, lances
par **son** lanceur, dans **son** langage. La difference est la meme qu'entre
nos cas et ceux d'Elastic — et elle porte sur une couche que rien d'autre ne
touche : la poignee de main, l'en-tete de produit, la negociation de
compression, le sniffing, la carte statut -> exception, les helpers.

# Ce que l'outil rend, et a quelles conditions

Trois garde-fous, les memes que pour `appli_reelle.py`, parce que ce sont les
memes pieges :

1. **la suite n'est pas modifiee.** Chaque client est cloné a une revision
   figee et l'outil refuse de conclure si un fichier suivi a bouge. Rien n'est
   recopie dans ce depot : le telechargement est a la demande, dans
   `.clients-tests/` (ignore par git), comme `.es-rest-spec/` ;
2. **la licence est verifiee avant qu'on en depende** — pas citee de memoire :
   le fichier `LICENSE` du clone doit porter la phrase attendue, sinon l'outil
   s'arrete ;
3. **l'instrument est etalonne.** La meme suite est d'abord lancee contre un
   vrai Elasticsearch 8.15. Une suite rouge des deux cotes ne dit rien de
   ferrite ; celle du client Python en compte six qui echouent contre un vrai
   ES, parce que le client a derive de la version du serveur.

Chaque echec que ferrite est seul a produire est rattache a une capacite de
`compat.yaml` par [`perimetre.py`](perimetre.py) — le meme verdict que le
rapport de conformance : `cout_perimetre`, `regression`, ou `indetermine` (qui
compte contre nous).

# Le mouchard ecoute sur un port impose

Le relais qui journalise le trafic se pose sur le port **9200** par defaut, pas
sur un port libre : quatre cas de la suite du client go ecrivent
`localhost:9200` en dur. Un port tire au sort les ferait viser autre chose que
la cible mesuree — donc ferrite doit ecouter ailleurs (`--ferrite`).

# Le nettoyage inter-cas : ce qui a du etre adapte, et pourquoi c'est dit

La suite serveur du client Python appelle `wipe_cluster` **entre chaque cas**.
Cette fonction n'est pas un test : c'est le menage, et il passe par seize
routes x-pack (rollup, SLM, ILM, CCR, ML, transform, snapshots, data streams,
`_cat/templates?h=name`, `_tasks`, `_cluster/pending_tasks`…). ferrite en
refuse seize sur dix-huit, donc la suite telle quelle rend **0 cas vert et 82
erreurs**, toutes dans la meme fixture, et ne mesure rien d'autre qu'elle.

Les deux chiffres sont donc publies :

  `origine`  la suite telle qu'elle est ecrite. C'est le chiffre honnete, et
             il vaut ce qu'il dit : une suite de client suppose un cluster
             complet ;
  `adapte`   la meme suite, avec un nettoyage de remplacement injecte par un
             **plugin pytest externe** (`-p`), qui n'utilise que des routes que
             les deux serveurs servent. Les fichiers de test, eux, ne bougent
             pas — et l'outil le verifie par `git diff` apres coup.

Le rapport nomme les routes ecartees, une par une. Une adaptation qu'on ne
compte pas est une adaptation qui grandit.

# Ce qu'il exige

Docker — chaque suite tourne dans l'image de son propre ecosysteme, pas dans le
worker. C'est un outil de developpement, pas de CI.
"""
import argparse
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import appli_reelle  # noqa: E402
import ponderation  # noqa: E402

RACINE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CLONES = os.path.join(RACINE, ".clients-tests")
OUTILS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "clients")

FERRITE = "http://127.0.0.1:9210"
ES = "http://127.0.0.1:9201"
PORT_MOUCHARD = 9200


# ===========================================================================
# 1. Les clients, leurs suites, et pourquoi cette revision-la
# ===========================================================================
#
# `commit` est la revision **figee** : « la derniere version » n'est pas une
# mesure rejouable. `licence` et `licence_phrase` ne sont pas decoratifs — le
# clone est verifie contre eux avant qu'on lance quoi que ce soit.

CLIENTS = {
    "python": {
        "nom": "elasticsearch-py",
        "langage": "Python",
        "version": "v8.15.0",
        "commit": "4a0224927776568a579dcaed199f59aef9ba798e",
        "depot": "https://github.com/elastic/elasticsearch-py",
        "licence": "Apache-2.0",
        "licence_fichier": "LICENSE",
        "licence_phrase": "Apache License",
        "suite": "pytest test_elasticsearch/test_server",
        "quoi": (
            "la suite serveur du client : helpers (bulk, streaming_bulk, scan, "
            "reindex), formes de reponse, corps de _bulk, magasin vectoriel"
        ),
        "image": "python:3.12",
        "caches": {"/root/.cache/pip": "pip"},
        "variable": "ELASTICSEARCH_URL",
        "parseur": "pytest",
        "commande": [
            "bash",
            "-c",
            "pip install --quiet -e '.[dev]' >/dev/null 2>&1 && "
            "python -m pytest test_elasticsearch/test_server -v -p no:cacheprovider "
            "--no-header --tb=line -p no:randomly $EXTRA",
        ],
        # Le plugin de remplacement du nettoyage : monte a cote de la suite,
        # jamais dedans, et charge par `-p`. Voir l'entete du fichier.
        "adaptation": {
            "nom": "nettoyage_compatible",
            "extra": "-p nettoyage_compatible",
            "quoi": (
                "`wipe_cluster` de la suite est remplace par un nettoyage qui "
                "n'utilise que des routes servies par les deux serveurs : "
                "suppression des index, des templates des deux familles, remise "
                "a zero des reglages de cluster"
            ),
            "routes_ecartees": [
                "GET /_rollup/job/_all",
                "GET /_slm/policy",
                "GET /_cluster/state/metadata",
                "GET /_snapshot/_all",
                "DELETE /_data_stream/*",
                "GET /_cat/templates?h=name",
                "GET /_component_template",
                "GET /_ilm/policy",
                "GET /_ccr/auto_follow",
                "GET /_tasks",
                "GET /_nodes/shutdown",
                "POST /_ml/calendars/*",
                "GET /_ml/filters/*",
                "GET /_ml/datafeeds/*",
                "GET /_transform/*",
                "GET /_cluster/pending_tasks",
                "GET /_cat/tasks",
            ],
        },
        "pourquoi": (
            "le client de reference du projet — c'est lui que le harnais maison "
            "utilise, et c'est sa suite serveur qui exerce le plus de helpers"
        ),
        "cycle": {
            "image": "python:3.12",
            "caches": {"/root/.cache/pip": "pip"},
            "fichiers": ["cycle.py"],
            "commande": [
                "bash",
                "-c",
                "pip install --quiet 'elasticsearch==8.15.0' >/dev/null 2>&1 && "
                "python /travail/cycle.py $URL",
            ],
        },
    },
    "javascript": {
        "nom": "elasticsearch-js",
        "langage": "JavaScript (Node.js)",
        "version": "v8.15.0",
        "commit": "599d7e6e07023b0ce7fabec60f41a1497c8965ec",
        "depot": "https://github.com/elastic/elasticsearch-js",
        "licence": "Apache-2.0",
        "licence_fichier": "LICENSE",
        "licence_phrase": "Apache License",
        # Pas de suite : ce client n'en a aucune, a cette revision, qu'on
        # puisse pointer sur un serveur. Mesure, pas supposition — voir
        # `sans_suite` ci-dessous, et l'entree correspondante d'`ECARTES`.
        "suite": None,
        "quoi": "le cycle de vie du client, exerce par le paquet npm publie",
        "variable": "TEST_ES_SERVER",
        "sans_suite": (
            "les quatre tests d'integration des helpers "
            "(`test/integration/helpers/*.test.js`) importent `waitCluster` de "
            "`test/utils`, qui ne l'exporte pas — verifie sur v8.0.0, v8.4.0, "
            "v8.8.0, v8.11.0 et v8.15.0 : ils sont casses dans le depot du "
            "client depuis la reecriture de la 8.0, et ne tournent donc dans "
            "aucune CI. Mesure : **aucun cas vert** contre un vrai ES 8.15 "
            "comme contre ferrite (22 lignes `not ok` des deux cotes), donc "
            "rien a mesurer. Le reste de `test/integration/` est le "
            "runner YAML d'Elastic, deja joue par `conformance_es.py`"
        ),
        "pourquoi": (
            "l'autre bout du spectre : un client dont les helpers sont ecrits "
            "en flux (`datasource` + `onDocument`), et qui active la "
            "compression par defaut vers Elastic Cloud"
        ),
        "cycle": {
            "image": "node:20",
            "caches": {"/root/.npm": "npm"},
            "fichiers": ["cycle.js"],
            "commande": [
                "bash",
                "-c",
                "cd /travail && npm install --silent --no-fund --no-audit "
                "@elastic/elasticsearch@8.15.0 >/dev/null 2>&1 && node cycle.js $URL",
            ],
        },
    },
    "go": {
        "nom": "go-elasticsearch",
        "langage": "Go",
        "version": "v8.13.0",
        "commit": "0ce9bb8fd976247d78da70ec56044f698812a18a",
        "depot": "https://github.com/elastic/go-elasticsearch",
        "licence": "Apache-2.0",
        "licence_fichier": "LICENSE",
        "licence_phrase": "Apache License",
        "suite": "go test -tags integration . ./esapi ./esutil",
        "quoi": (
            "les tests d'integration du client : transport (persistance, "
            "concurrence, transport remplace), client type, `esapi`, et le "
            "`BulkIndexer` d'`esutil` avec et sans compression du corps"
        ),
        "image": "golang:1.23",
        "caches": {"/root/.cache/go-build": "go-build", "/go/pkg/mod": "go-mod"},
        "variable": "ELASTICSEARCH_URL",
        "parseur": "go",
        "commande": ["go", "test", "-tags", "integration", "-v", "-count=1", ".", "./esapi", "./esutil"],
        # La 8.14 a bascule ses tests d'integration sur `testcontainers` : ils
        # demarrent **leur propre** Elasticsearch et ne peuvent plus viser un
        # autre serveur sans modifier leur code. La 8.13.0 est donc la derniere
        # revision de ce client dont la suite se pointe sur une URL — et c'est
        # une mesure, pas une preference.
        "pourquoi": (
            "derniere revision dont la suite d'integration se pointe sur une "
            "URL : depuis la 8.14, elle demarre son propre conteneur "
            "Elasticsearch via testcontainers et ne peut plus viser ferrite"
        ),
        "cycle": {
            "image": "golang:1.23",
            "caches": {"/root/.cache/go-build": "go-build", "/go/pkg/mod": "go-mod"},
            "fichiers": ["cycle.go", "go.mod"],
            "commande": [
                "bash",
                "-c",
                "cd /travail && go mod tidy >/dev/null 2>&1 && go run . $URL",
            ],
        },
    },
}

# Ce qu'on n'a pas mesure, et pourquoi — le negatif documente vaut autant que
# le positif, et il dit ou la mesure s'arrete.
ECARTES = [
    {
        "nom": "elastic/elasticsearch-clients-tests",
        "raison": (
            "Apache-2.0 (verifie), et c'est bien la source commune des clients "
            "recents — mais ce sont les **memes cas YAML** que `conformance_es.py` "
            "rejoue deja, sur deux sources. Les brancher ici mesurerait une "
            "troisieme fois la surface d'API, pas le client ; ce que cette carte "
            "ajoute est la couche que les cas YAML ne touchent pas"
        ),
    },
    {
        "nom": "elasticsearch-py : test_rest_api_spec.py",
        "raison": (
            "la partie de la suite qui rejoue les cas YAML d'Elastic. Elle ne se "
            "collecte pas ici (elle telecharge les tests de la 8.x, dont la "
            "licence n'est plus Apache-2.0), et le meme corpus est deja joue par "
            "`conformance_es.py` sur sa derniere version Apache — donc la perdre "
            "ne perd rien"
        ),
    },
    {
        "nom": "go-elasticsearch : `make test-api` (esapi/test)",
        "raison": (
            "des milliers de tests generes depuis les memes cas YAML que la "
            "suite de conformance, et generes a partir d'une version d'ES dont "
            "la licence n'est plus Apache-2.0"
        ),
    },
    {
        "nom": "elasticsearch-js : test/integration/",
        "raison": (
            "`index.js` est le runner YAML d'Elastic (meme raison que les deux "
            "lignes ci-dessus). Et les quatre fichiers `helpers/*.test.js`, qui "
            "eux appartiennent bien au client, sont **casses dans son propre "
            "depot** : ils importent `waitCluster` de `test/utils`, qui ne "
            "l'exporte pas — verifie sur v8.0.0, v8.4.0, v8.8.0, v8.11.0 et "
            "v8.15.0. Mesure : aucun cas vert contre un vrai ES 8.15 comme "
            "contre ferrite, 22 lignes `not ok` des deux cotes. C'est "
            "l'etalonnage qui l'a montre, et c'est exactement ce a quoi il sert"
        ),
    },
    {
        "nom": "go-elasticsearch >= 8.14",
        "raison": (
            "ses tests d'integration demarrent **leur propre** Elasticsearch "
            "par testcontainers (`internal/testing/e2e`) : ils ne peuvent plus "
            "viser un autre serveur sans modifier leur code, ce que la premiere "
            "condition interdit. La 8.13.0 est la derniere revision pointable "
            "sur une URL"
        ),
    },
]


# ===========================================================================
# 2. Le clone, fige, verifie, et non modifie
# ===========================================================================


def git(*args, cwd=None):
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, check=False)


def prepare(client, cle):
    chemin = os.path.join(CLONES, cle)
    if not os.path.isdir(os.path.join(chemin, ".git")):
        os.makedirs(CLONES, exist_ok=True)
        print(f"== clonage de {client['nom']} {client['version']} (une fois)")
        r = git("clone", "--filter=blob:none", "--no-checkout", client["depot"], chemin)
        if r.returncode:
            sortir(f"clonage impossible : {r.stderr.strip()}")
    cible = client.get("commit") or client["version"]
    if git("fetch", "--depth", "1", "origin", cible, cwd=chemin).returncode:
        git("fetch", "--tags", "--depth", "1", "origin", client["version"], cwd=chemin)
    r = git("checkout", "--force", client["version"], cwd=chemin)
    if r.returncode:
        sortir(f"revision {client['version']} introuvable : {r.stderr.strip()}")
    return chemin, git("rev-parse", "HEAD", cwd=chemin).stdout.strip()


def licence_verifiee(chemin, client):
    """La licence se lit dans le clone, elle ne se cite pas de memoire.

    Une suite dont on depend sans avoir ouvert son fichier de licence est une
    dependance qu'on n'a pas verifiee — et c'est la premiere condition posee
    par la carte.
    """
    fichier = os.path.join(chemin, client["licence_fichier"])
    if not os.path.exists(fichier):
        return False, f"{client['licence_fichier']} absent du depot"
    with open(fichier, encoding="utf-8", errors="replace") as f:
        texte = f.read(4000)
    if client["licence_phrase"] not in texte:
        return False, f"« {client['licence_phrase']} » introuvable dans {client['licence_fichier']}"
    return True, f"{client['licence_fichier']} porte « {client['licence_phrase']} »"


def intact(chemin):
    """Aucun fichier **suivi** modifie. C'est ce qui donne son sens au chiffre :
    « la suite du client passe » ne vaut que si c'est bien la sienne."""
    return git("diff", "--stat", cwd=chemin).stdout.strip() == ""


# ===========================================================================
# 3. Lancer une suite
# ===========================================================================


def execute(client, chemin, url_cible, extra="", silencieux=True, commande=None, image=None,
            caches=None, travail=None, port=PORT_MOUCHARD):
    """Lance la suite (ou la batterie) contre `url_cible`, a travers le
    mouchard. Rend (sortie, cas, journal)."""
    avant = appli_reelle.index_de(url_cible)
    with appli_reelle.Mouchard(url_cible, port=port) as mouchard:
        cmd = ["docker", "run", "--rm", "--network", "host"]
        if chemin:
            cmd += ["-v", f"{chemin}:/src", "-w", "/src"]
        if travail:
            # `PYTHONPATH` rend le plugin de nettoyage importable **sans** le
            # poser dans l'arbre du client. Sans cette ligne, `-p
            # nettoyage_compatible` echouait au demarrage de pytest et la
            # colonne « adapte » rendait 0/0 — un resultat qui n'est ni vert ni
            # rouge, donc celui qu'on ne regarde pas. D'ou le garde-fou de
            # `mesure_suite` : une campagne sans aucun cas est une erreur.
            cmd += ["-v", f"{travail}:/travail", "-e", "PYTHONPATH=/travail"]
        for interne, nom in (caches or {}).items():
            cache = os.path.join(CLONES, ".cache-" + nom)
            os.makedirs(cache, exist_ok=True)
            cmd += ["-v", f"{cache}:{interne}"]
        cmd += ["-e", f"{client['variable']}={mouchard.url}"]
        cmd += ["-e", f"URL={mouchard.url}", "-e", f"EXTRA={extra}"]
        cmd += [image or client["image"], *(commande or client["commande"])]
        proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    sortie = proc.stdout + proc.stderr
    if not silencieux:
        print(sortie[-6000:] if len(sortie) > 6000 else sortie)
    laisses = appli_reelle.nettoie(url_cible, avant)
    if laisses:
        print(f"   ({len(laisses)} index laisses par la suite, supprimes)")
    return sortie, mouchard.journal


# --- les parseurs de verdicts ---------------------------------------------


ANSI = re.compile(r"\x1b\[[0-9;]*m")


def parse_pytest(sortie):
    """`pytest -v` : une ligne `chemin::cas VERDICT [ 12%]` par cas."""
    cas = {}
    for m in re.finditer(
        r"^(\S+::\S+) (PASSED|FAILED|ERROR|SKIPPED|XFAIL|XPASS)", ANSI.sub("", sortie), re.M
    ):
        cas[m.group(1)] = {
            "PASSED": "PASS",
            "XPASS": "PASS",
            "FAILED": "FAIL",
            "ERROR": "FAIL",
            "SKIPPED": "SKIP",
            "XFAIL": "SKIP",
        }[m.group(2)]
    return cas


def parse_tap(sortie):
    """TAP : chaque fichier est annonce par `##### <fichier>`, et chaque cas y
    est une ligne `ok N - nom` / `not ok N - nom`. Un fichier qui plante avant
    d'emettre le moindre `ok` compte comme **un** cas rouge, sinon une suite
    qui ne demarre pas se lirait « 0 cas, 0 echec »."""
    cas = {}
    fichier = "?"
    emis = set()
    for ligne in ANSI.sub("", sortie).splitlines():
        if ligne.startswith("##### "):
            fichier = ligne[6:].strip()
            continue
        m = re.match(r"^(not ok|ok) (\d+)\s*-?\s*(.*)$", ligne.strip())
        if not m:
            continue
        nom = (m.group(3) or f"cas {m.group(2)}").strip()
        # Les sous-tests d'un `t.test()` imbrique portent le meme numero de
        # plan a un autre niveau : le nom du fichier les separe.
        cas[f"{fichier}::{nom}"] = "PASS" if m.group(1) == "ok" else "FAIL"
        emis.add(fichier)
    for m in re.finditer(r"^##### (\S+)", sortie, re.M):
        if m.group(1) not in emis:
            cas[f"{m.group(1)}::<la suite n'a rien emis>"] = "FAIL"
    return cas


PARSEURS = {"pytest": parse_pytest, "tap": parse_tap, "go": appli_reelle.parse_go}


def resume(cas, sortie):
    return {
        "cas": cas,
        "total": len(cas),
        "passes": sum(1 for v in cas.values() if v == "PASS"),
        "echecs": sum(1 for v in cas.values() if v == "FAIL"),
        "ignores": sum(1 for v in cas.values() if v == "SKIP"),
        "queue": sortie[-1500:],
    }


# ===========================================================================
# 4. La batterie « cycle de vie »
# ===========================================================================


def parse_cycle(sortie):
    """`CAS <nom> <PASS|FAIL> <detail>` — le format que les trois batteries
    partagent, pour que le rapport ne depende pas du langage."""
    cas, details = {}, {}
    for m in re.finditer(r"^CAS (\S+) (PASS|FAIL) ?(.*)$", sortie, re.M):
        cas[m.group(1)] = m.group(2)
        details[m.group(1)] = m.group(3).strip()
    return cas, details


def cycle_de_vie(client, url, port):
    """La batterie du cycle de vie, exercee par le client **publie** (PyPI, npm,
    le proxy de modules Go) — pas par le clone : ce qu'un utilisateur installe.

    C'est le plancher que la carte pose : decouverte de version, en-tete
    `X-elastic-product`, negociation de compression, sniffing (ou son refus
    propre), erreurs typees, helpers.
    """
    spec = client["cycle"]
    travail = os.path.join(CLONES, ".cycle-" + spec["image"].split(":")[0])
    shutil.rmtree(travail, ignore_errors=True)
    os.makedirs(travail, exist_ok=True)
    for f in spec["fichiers"]:
        shutil.copy(os.path.join(OUTILS, f), travail)
    sortie, journal = execute(
        client,
        None,
        url,
        commande=spec["commande"],
        image=spec["image"],
        caches=spec.get("caches"),
        travail=travail,
        port=port,
    )
    cas, details = parse_cycle(sortie)
    return cas, details, sortie, journal


# ===========================================================================
# 5. Rapport
# ===========================================================================


def imprime(rapport):
    c = rapport["client"]
    print()
    print(f"== {c['nom']} {c['version']} ({c['commit'][:12]}) — {c['langage']}")
    print(f"   depot   : {c['depot']}")
    print(f"   licence : {c['licence']} — {rapport['licence']['constat']}")
    print(f"   suite   : {c['suite'] or 'aucune jouable — ' + rapport['sans_suite']}")
    print(f"   arbre non modifie : {'oui' if rapport['intact'] else 'NON'}")
    for mode, mesure in rapport["suites"].items():
        print()
        titre = "telle qu'elle est ecrite" if mode == "origine" else "avec le nettoyage de remplacement"
        print(f"   -- suite {titre}")
        for cible in ("es", "ferrite"):
            r = mesure["executions"].get(cible)
            if not r:
                print(f"      {cible:8} : non lance")
                continue
            print(
                f"      {cible:8} : {r['passes']}/{r['total']} verts"
                + (f", {r['echecs']} rouges" if r["echecs"] else "")
                + (f", {r['ignores']} sautes" if r["ignores"] else "")
            )
        ecarts = [x for x in mesure["comparaison"] if x["etat"] == "ecart"]
        inverses = [x for x in mesure["comparaison"] if x["etat"] == "inverse"]
        if mesure["comparaison"]:
            print(f"      {len(ecarts)} cas verts chez ES et rouges chez ferrite"
                  + (f", {len(inverses)} l'inverse" if inverses else ""))
            for x in ecarts[:40]:
                print(f"        {x['cas']}")
            if len(ecarts) > 40:
                print(f"        … et {len(ecarts) - 40} autres")
    if rapport.get("cycle"):
        cy = rapport["cycle"]
        print()
        print(f"   -- cycle de vie du client ({cy['total']} cas, batterie ecrite ici,"
              " jouee par le client publie)")
        for nom in cy["ordre"]:
            f, e = cy["ferrite"].get(nom, "ABSENT"), cy["es"].get(nom, "ABSENT")
            marque = "  " if f == "PASS" else "!!"
            print(f"    {marque} {nom:26} ferrite={f:5} es={e:6}  {cy['details_ferrite'].get(nom, '')[:90]}")
    print()
    vrais = [r for r in rapport["refus"] if r["genre"] == "refus"]
    if vrais:
        print("   ce que ferrite refuse et qu'ES sert :")
        for r in vrais:
            print(f"     [{r['verdict']}] {r['api']} ×{r['combien']} — {r['erreur'][:130]}")
            if r["capacite"]:
                print(f"        capacite : {r['capacite']}")
    else:
        print("   ce que ferrite refuse et qu'ES sert : rien")
    print()


def ecourte(valeur, lignes=3, taille=400):
    """Un exemple de requete, ramene a ce qui le rend reconnaissable."""
    if isinstance(valeur, list):
        coupe = [ecourte(v, lignes, taille) for v in valeur[:lignes]]
        if len(valeur) > lignes:
            coupe.append(f"… et {len(valeur) - lignes} lignes")
        return coupe
    if isinstance(valeur, str) and len(valeur) > taille:
        return valeur[:taille] + "…"
    if isinstance(valeur, dict):
        rendu = json.dumps(valeur, ensure_ascii=False)
        return json.loads(rendu) if len(rendu) <= 2000 else rendu[:2000] + "…"
    return valeur


def sortir(message):
    print(f"ERREUR : {message}", file=sys.stderr)
    sys.exit(2)


def joignable(url):
    try:
        with urllib.request.urlopen(url, timeout=3) as r:
            return r.status == 200
    except (urllib.error.URLError, OSError, socket.timeout):
        return False


def port_libre(port):
    with socket.socket() as s:
        # `SO_REUSEADDR` comme le fait `HTTPServer` : sans lui, la sonde serait
        # **plus stricte** que le mouchard qu'elle protege et refuserait de
        # partir sur un port encore en `TIME_WAIT` — un port que le mouchard,
        # lui, prendrait sans broncher.
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            s.bind(("127.0.0.1", port))
            return True
        except OSError:
            return False


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("client", nargs="?", choices=sorted(CLIENTS))
    p.add_argument("--ferrite", default=FERRITE)
    p.add_argument("--es", default=ES)
    p.add_argument("--port", type=int, default=PORT_MOUCHARD,
                   help="le port du mouchard (impose : des suites l'ecrivent en dur)")
    p.add_argument("--cycle", action="store_true", help="la batterie du cycle de vie seule")
    p.add_argument("--sans-etalonnage", action="store_true",
                   help="ne lance pas contre le vrai Elasticsearch (le rapport le dit)")
    p.add_argument("--json", metavar="FICHIER", help="ecrit le rapport machine")
    p.add_argument("--liste", action="store_true", help="les clients mesures et les ecartes")
    p.add_argument("--verbeux", action="store_true", help="imprime la sortie brute des suites")
    args = p.parse_args()

    if args.liste or not args.client:
        print("Clients mesures :")
        for cle, c in CLIENTS.items():
            print(f"  {cle:11} {c['nom']} {c['version']} ({c['langage']}, {c['licence']})")
            print(f"              suite : {c['suite'] or 'aucune jouable (voir les ecartes)'}")
            print(f"              {c['pourquoi']}")
        print("\nEcartes :")
        for e in ECARTES:
            print(f"  {e['nom']}\n      {e['raison']}")
        return 0

    if not shutil.which("docker"):
        sortir("docker est necessaire : chaque suite tourne dans l'image de son ecosysteme")
    if not joignable(args.ferrite):
        sortir(f"ferrite ne repond pas sur {args.ferrite}")
    if not args.sans_etalonnage and not joignable(args.es):
        sortir(
            f"le vrai Elasticsearch ne repond pas sur {args.es}. Une suite de client rouge "
            "contre les deux serveurs ne dit rien de ferrite : l'etalonnage n'est pas "
            "optionnel (--sans-etalonnage pour l'assumer explicitement)"
        )
    if not port_libre(args.port):
        sortir(
            f"le port {args.port} est occupe : c'est celui du mouchard, et des suites "
            "l'ecrivent en dur. Lancer ferrite ailleurs (--ferrite), ou choisir --port"
        )

    client = CLIENTS[args.client]
    chemin, tete = prepare(client, args.client)
    ok, constat = licence_verifiee(chemin, client)
    if not ok:
        sortir(f"licence non verifiee : {constat}")
    print(f"== {client['nom']} {client['version']} — licence {client['licence']} : {constat}")

    croisement = ponderation.Croisement()
    perim = croisement.perimetre
    cibles = [("ferrite", args.ferrite)]
    if not args.sans_etalonnage:
        cibles.insert(0, ("es", args.es))

    suites, journaux = {}, {}
    if not args.cycle and client.get("suite"):
        modes = [("origine", "")]
        if client.get("adaptation"):
            modes.append(("adapte", client["adaptation"]["extra"]))
            plugin = os.path.join(CLONES, ".plugin")
            os.makedirs(plugin, exist_ok=True)
            shutil.copy(os.path.join(OUTILS, "nettoyage_compatible.py"), plugin)
        for mode, extra in modes:
            executions = {}
            for nom, url in cibles:
                print(f"== suite {mode} contre {nom} ({url})")
                sortie, journal = execute(
                    client, chemin, url, extra=extra, silencieux=not args.verbeux,
                    port=args.port,
                    # Le plugin vit a cote de la suite, pas dedans : monte en
                    # `/travail` et trouve par `PYTHONPATH`, il ne touche pas un
                    # fichier du clone.
                    travail=os.path.join(CLONES, ".plugin") if extra else None,
                )
                cas = PARSEURS[client["parseur"]](sortie)
                if not cas:
                    # Zero cas n'est ni un succes ni un echec : c'est une suite
                    # qui n'a pas demarre. La laisser passer, c'est publier
                    # « 0/0 » — le seul resultat qui ne se lit pas.
                    print(sortie[-3000:])
                    sortir(
                        f"la suite {mode} n'a rendu aucun cas contre {nom} : "
                        "elle n'a pas demarre, et 0/0 ne mesure rien"
                    )
                executions[nom] = resume(cas, sortie)
                print(f"   {executions[nom]['passes']}/{executions[nom]['total']}")
                if mode == modes[-1][0]:
                    journaux[nom] = journal
            suites[mode] = {
                "executions": executions,
                "comparaison": appli_reelle.compare(
                    executions.get("es", {}).get("cas", {}), executions["ferrite"]["cas"]
                )
                if "es" in executions
                else [],
            }

    cycle = None
    if client.get("cycle"):
        colonnes, details = {}, {}
        for nom, url in cibles:
            print(f"== cycle de vie contre {nom} ({url})")
            cas, det, sortie, journal = cycle_de_vie(client, url, args.port)
            if not cas:
                print(sortie[-3000:])
            colonnes[nom] = cas
            details[nom] = det
            journaux.setdefault(nom, [])
            journaux[nom] += journal
            print(f"   {sum(1 for v in cas.values() if v == 'PASS')}/{len(cas)}")
        cycle = {
            "ordre": list(colonnes.get("ferrite", {})),
            "ferrite": colonnes.get("ferrite", {}),
            "es": colonnes.get("es", {}),
            "details_ferrite": details.get("ferrite", {}),
            "details_es": details.get("es", {}),
            "total": len(colonnes.get("ferrite", {})),
            "verts": sum(1 for v in colonnes.get("ferrite", {}).values() if v == "PASS"),
        }

    rapport = {
        "client": {
            k: client[k]
            for k in ("nom", "langage", "version", "depot", "licence", "quoi", "suite", "pourquoi")
        }
        | {"commit": tete},
        "licence": {"declaree": client["licence"], "constat": constat, "verifiee": ok},
        "intact": intact(chemin),
        "etalonne": not args.sans_etalonnage,
        "adaptation": client.get("adaptation"),
        "sans_suite": client.get("sans_suite"),
        "suites": suites,
        "cycle": cycle,
        "trafic": appli_reelle.trafic(journaux.get("ferrite", []), croisement),
        "refus": appli_reelle.classe_refus(
            journaux.get("ferrite", []), journaux.get("es") or [], perim
        ),
        "ecartes": ECARTES,
    }
    imprime(rapport)

    if args.json:
        # Un exemple de requete sert a reconnaitre la forme, pas a rejouer le
        # lot : le `_bulk` de la suite go porte dix documents de Lorem ipsum et
        # pesait a lui seul 347 Ko du rapport. Un fichier commite qu'on
        # n'ouvre plus parce qu'il est trop gros ne prouve rien.
        rapport["trafic"] = [dict(t, exemple=ecourte(t.get("exemple"))) for t in rapport["trafic"]]
        rapport["refus"] = [dict(r, exemple=ecourte(r.get("exemple"))) for r in rapport["refus"]]
        for mesure in rapport["suites"].values():
            if mesure["comparaison"]:
                for r in mesure["executions"].values():
                    r.pop("cas", None)
        doc = {}
        if os.path.exists(args.json):
            with open(args.json, encoding="utf-8") as f:
                doc = json.load(f)
        doc[args.client] = rapport
        with open(args.json, "w", encoding="utf-8") as f:
            json.dump(doc, f, ensure_ascii=False, indent=1, sort_keys=True)
            f.write("\n")
        print(f"   rapport ecrit dans {args.json}")

    if not rapport["intact"]:
        sortir("l'arbre du client a ete modifie : la mesure ne vaut rien")
    if cycle and cycle["verts"] != cycle["total"]:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
